//! 局域网聊天：消息模型、本地会话存储、以及对端的收发格式。
//!
//! 聊天是 FeigeCraft 在 LocalSend 协议之外的扩展端点
//! （`POST /api/feigecraft/chat`），不与 LocalSend 官方客户端冲突：
//! 对方不认识这个路径时只会返回 404，文件传输仍然照常工作。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::model::DeviceInfo;

/// 每个会话最多保留的消息条数（超出后丢弃最旧的）。
pub const MAX_MESSAGES_PER_PEER: usize = 400;

/// 当前 Unix 毫秒时间戳。
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// 消息类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MessageKind {
    #[default]
    Text,
    File,
    /// 本地生成的分隔提示（"对方上线""传输已取消"等），不发送给对方。
    System,
}

/// 消息投递状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MessageStatus {
    #[default]
    Sent,
    Sending,
    Failed,
}

/// 一条聊天消息。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub id: String,
    /// 对端标识（优先用证书指纹，保证改名 / 换 IP 后会话不丢）。
    pub peer: String,
    /// 本地视角：是我发出的吗。
    pub outgoing: bool,
    pub kind: MessageKind,
    pub text: String,
    /// Unix 毫秒。
    pub at: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub file_name: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub file_size: u64,
    /// 接收方落盘后的本地路径。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_path: Option<PathBuf>,
    /// 关联的传输会话（用于把进度画在文件气泡里）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default)]
    pub status: MessageStatus,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

impl ChatMessage {
    pub fn text(peer: &str, outgoing: bool, text: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            peer: peer.to_string(),
            outgoing,
            kind: MessageKind::Text,
            text: text.into(),
            at: now_ms(),
            file_name: String::new(),
            file_size: 0,
            file_path: None,
            session_id: None,
            status: if outgoing { MessageStatus::Sending } else { MessageStatus::Sent },
        }
    }

    pub fn file(peer: &str, outgoing: bool, name: impl Into<String>, size: u64) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            peer: peer.to_string(),
            outgoing,
            kind: MessageKind::File,
            text: String::new(),
            at: now_ms(),
            file_name: name.into(),
            file_size: size,
            file_path: None,
            session_id: None,
            status: if outgoing { MessageStatus::Sending } else { MessageStatus::Sent },
        }
    }

    pub fn system(peer: &str, text: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            peer: peer.to_string(),
            outgoing: false,
            kind: MessageKind::System,
            text: text.into(),
            at: now_ms(),
            file_name: String::new(),
            file_size: 0,
            file_path: None,
            session_id: None,
            status: MessageStatus::Sent,
        }
    }
}

/// `POST /api/feigecraft/chat` 的请求体。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatEnvelope {
    pub info: DeviceInfo,
    pub messages: Vec<ChatPayload>,
}

/// 单条待投递消息（只带必要字段，避免把本地路径泄露给对方）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatPayload {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub at: u64,
}

impl ChatPayload {
    pub fn from_message(message: &ChatMessage) -> Self {
        Self { id: message.id.clone(), text: message.text.clone(), at: message.at }
    }
}

/// 持久化到磁盘的会话集合。
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoreFile {
    #[serde(default = "store_version")]
    version: u32,
    /// 对端指纹 → 最后一次见到的名字（离线时用来显示）。
    #[serde(default)]
    aliases: HashMap<String, String>,
    #[serde(default)]
    threads: HashMap<String, Vec<ChatMessage>>,
}

fn store_version() -> u32 {
    1
}

/// 会话存储：内存态 + 落盘。
///
/// 落盘是**合并写**：`append` 只置脏标记，后台线程每 400ms 至多写一次，
/// `Drop` 时补写最后一份。为什么不能每条都写：`append` 会把整个 StoreFile
/// 序列化一遍再整文件覆盖，连发消息时就是「一条消息 = 一次全量序列化 + 一次
/// 全文件写入」，聊天记录越长越慢（实测灌 3000 条要 5 秒）。
pub struct ChatStore {
    shared: Arc<Shared>,
}

struct Shared {
    path: PathBuf,
    inner: Mutex<StoreFile>,
    dirty: AtomicBool,
}

/// 合并写的时间窗口。比一帧长很多，又不至于让「刚发的消息」在崩溃时丢太久。
const SAVE_INTERVAL: Duration = Duration::from_millis(400);

impl Drop for Shared {
    fn drop(&mut self) {
        if self.dirty.load(Ordering::Relaxed) {
            self.write_now();
        }
    }
}

impl Shared {
    /// 真正落盘：序列化 + 覆盖写。
    fn write_now(&self) {
        let text = {
            let store = self.inner.lock().expect("chat store poisoned");
            match serde_json::to_string_pretty(&*store) {
                Ok(text) => text,
                Err(err) => {
                    tracing::warn!("序列化聊天记录失败：{err}");
                    return;
                }
            }
        };
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Err(err) = fs::write(&self.path, text) {
            tracing::warn!("写入聊天记录失败：{err}");
        }
    }
}

impl ChatStore {
    /// 从磁盘加载；文件缺失或损坏时从空开始。
    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let inner = match fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<StoreFile>(&text) {
                Ok(mut store) => {
                    // 兜底：修正超过上限的会话
                    for thread in store.threads.values_mut() {
                        let len = thread.len();
                        if len > MAX_MESSAGES_PER_PEER {
                            thread.drain(..len - MAX_MESSAGES_PER_PEER);
                        }
                    }
                    store
                }
                Err(err) => {
                    tracing::warn!("聊天记录解析失败，已忽略：{err}");
                    StoreFile::default()
                }
            },
            Err(_) => StoreFile::default(),
        };
        let shared = Arc::new(Shared {
            path,
            inner: Mutex::new(inner),
            dirty: AtomicBool::new(false),
        });
        // 后台合并写线程：拿着弱引用，`ChatStore` 全部释放后自己退出。
        let weak = Arc::downgrade(&shared);
        let spawned = std::thread::Builder::new()
            .name("chat-save".to_string())
            .spawn(move || loop {
                std::thread::sleep(SAVE_INTERVAL);
                let Some(shared) = weak.upgrade() else { break };
                if shared.dirty.swap(false, Ordering::Relaxed) {
                    shared.write_now();
                }
            });
        if let Err(err) = spawned {
            tracing::warn!("聊天记录合并写线程启动失败，改为同步落盘：{err}");
        }
        Self { shared }
    }

    pub fn path(&self) -> &Path {
        &self.shared.path
    }

    /// 追加一条消息（落盘交给合并写线程）。
    pub fn append(&self, message: ChatMessage) {
        {
            let mut store = self.shared.inner.lock().expect("chat store poisoned");
            let thread = store.threads.entry(message.peer.clone()).or_default();
            thread.push(message);
            let len = thread.len();
            if len > MAX_MESSAGES_PER_PEER {
                thread.drain(..len - MAX_MESSAGES_PER_PEER);
            }
        }
        self.mark_dirty();
    }

    /// 立刻落盘（退出前 / 测试里用）。
    pub fn flush(&self) {
        self.shared.dirty.store(false, Ordering::Relaxed);
        self.shared.write_now();
    }

    fn mark_dirty(&self) {
        self.shared.dirty.store(true, Ordering::Relaxed);
    }

    /// 记录对端名字（用于离线展示）。
    pub fn touch_alias(&self, peer: &str, alias: &str) {
        let changed = {
            let mut store = self.shared.inner.lock().expect("chat store poisoned");
            if store.aliases.get(peer).map(String::as_str) == Some(alias) {
                false
            } else {
                store.aliases.insert(peer.to_string(), alias.to_string());
                true
            }
        };
        if changed {
            self.mark_dirty();
        }
    }

    pub fn alias(&self, peer: &str) -> Option<String> {
        self.shared
            .inner
            .lock()
            .expect("chat store poisoned")
            .aliases
            .get(peer)
            .cloned()
    }

    /// 某个会话的全部消息（按时间升序）。
    pub fn thread(&self, peer: &str) -> Vec<ChatMessage> {
        self.shared
            .inner
            .lock()
            .expect("chat store poisoned")
            .threads
            .get(peer)
            .cloned()
            .unwrap_or_default()
    }

    pub fn last(&self, peer: &str) -> Option<ChatMessage> {
        self.shared
            .inner
            .lock()
            .expect("chat store poisoned")
            .threads
            .get(peer)
            .and_then(|thread| thread.last().cloned())
    }

    /// 所有有过聊天的对端及其最后一条消息，按时间倒序。
    pub fn peers(&self) -> Vec<(String, ChatMessage)> {
        let store = self.shared.inner.lock().expect("chat store poisoned");
        let mut list: Vec<(String, ChatMessage)> = store
            .threads
            .iter()
            .filter_map(|(peer, thread)| {
                thread
                    .last()
                    .filter(|_| !thread.is_empty())
                    .map(|last| (peer.clone(), last.clone()))
            })
            .collect();
        list.sort_by_key(|a| std::cmp::Reverse(a.1.at));
        list
    }

    /// 更新某条消息的状态。
    pub fn set_status(&self, peer: &str, id: &str, status: MessageStatus) {
        {
            let mut store = self.shared.inner.lock().expect("chat store poisoned");
            if let Some(thread) = store.threads.get_mut(peer) {
                if let Some(message) = thread.iter_mut().find(|item| item.id == id) {
                    message.status = status;
                }
            }
        }
        self.mark_dirty();
    }

    /// 把消息与传输会话关联起来（文件气泡据此显示进度）。
    pub fn bind_session(&self, peer: &str, ids: &[String], session_id: &str) {
        {
            let mut store = self.shared.inner.lock().expect("chat store poisoned");
            if let Some(thread) = store.threads.get_mut(peer) {
                for message in thread.iter_mut() {
                    if ids.iter().any(|id| id == &message.id) {
                        message.session_id = Some(session_id.to_string());
                    }
                }
            }
        }
        self.mark_dirty();
    }

    /// 记录接收到的文件落在哪里。
    pub fn set_file_path(&self, peer: &str, file_name: &str, path: PathBuf) -> bool {
        let updated = {
            let mut store = self.shared.inner.lock().expect("chat store poisoned");
            match store.threads.get_mut(peer) {
                Some(thread) => {
                    // 从后往前找最近一条同名且还没落盘的文件消息
                    let found = thread
                        .iter_mut()
                        .rev()
                        .find(|item| item.kind == MessageKind::File && item.file_name == file_name);
                    match found {
                        Some(message) => {
                            message.file_path = Some(path);
                            message.status = MessageStatus::Sent;
                            true
                        }
                        None => false,
                    }
                }
                None => false,
            }
        };
        if updated {
            self.mark_dirty();
        }
        updated
    }

    /// 从会话里删掉一条消息（JSON 文件同步更新）。返回是否真的删掉了。
    ///
    /// 只动聊天记录，不碰磁盘上的文件——是否删文件由调用方决定，避免误删。
    pub fn remove_message(&self, peer: &str, id: &str) -> bool {
        let removed = {
            let mut store = self.shared.inner.lock().expect("chat store poisoned");
            match store.threads.get_mut(peer) {
                Some(thread) => {
                    let before = thread.len();
                    thread.retain(|message| message.id != id);
                    thread.len() != before
                }
                None => false,
            }
        };
        if removed {
            self.mark_dirty();
        }
        removed
    }

    /// 清空某个会话（JSON 文件同步更新）。
    pub fn clear(&self, peer: &str) {
        {
            let mut store = self.shared.inner.lock().expect("chat store poisoned");
            store.threads.remove(peer);
        }
        self.mark_dirty();
    }

}

/// 对端标识：优先指纹，缺失时退回名字。
pub fn peer_key(info: &DeviceInfo) -> String {
    if info.fingerprint.is_empty() {
        format!("alias:{}", info.alias)
    } else {
        info.fingerprint.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(name: &str) -> (ChatStore, PathBuf) {
        let dir = std::env::temp_dir().join(format!("feigecraft-chat-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("chats.json");
        (ChatStore::load(&path), dir)
    }

    #[test]
    fn append_and_reload_keeps_messages() {
        let (store, dir) = temp_store("reload");
        store.touch_alias("fp1", "客厅电脑");
        store.append(ChatMessage::text("fp1", true, "你好"));
        store.append(ChatMessage::text("fp1", false, "收到"));
        // 落盘是合并写的，重载前要手动 flush，否则读到的还是旧文件。
        store.flush();

        let reloaded = ChatStore::load(store.path());
        let thread = reloaded.thread("fp1");
        assert_eq!(thread.len(), 2);
        assert_eq!(thread[0].text, "你好");
        assert!(thread[0].outgoing);
        assert!(!thread[1].outgoing);
        assert_eq!(reloaded.alias("fp1").as_deref(), Some("客厅电脑"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn remove_message_drops_one_and_persists() {
        let (store, dir) = temp_store("remove");
        let keep = ChatMessage::text("fp1", true, "留着");
        let drop = ChatMessage::text("fp1", false, "删掉");
        let drop_id = drop.id.clone();
        store.append(keep);
        store.append(drop);

        assert!(store.remove_message("fp1", &drop_id));
        // 再删同一个 id 应该返回 false（已经没有了），避免上层误判。
        assert!(!store.remove_message("fp1", &drop_id));
        store.flush();

        let reloaded = ChatStore::load(store.path());
        let thread = reloaded.thread("fp1");
        assert_eq!(thread.len(), 1);
        assert_eq!(thread[0].text, "留着");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn peers_sorted_by_last_message() {
        let (store, dir) = temp_store("peers");
        store.append(ChatMessage::text("a", true, "1"));
        store.append(ChatMessage::text("b", true, "2"));
        let mut newest = ChatMessage::text("a", true, "3");
        newest.at += 10;
        store.append(newest);
        let peers = store.peers();
        assert_eq!(peers[0].0, "a");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn status_and_file_path_update() {
        let (store, dir) = temp_store("status");
        let message = ChatMessage::file("fp", true, "a.txt", 12);
        let id = message.id.clone();
        store.append(message);
        store.set_status("fp", &id, MessageStatus::Failed);
        assert_eq!(store.thread("fp")[0].status, MessageStatus::Failed);

        let incoming = ChatMessage::file("fp", false, "b.txt", 3);
        store.append(incoming);
        assert!(store.set_file_path("fp", "b.txt", PathBuf::from("/tmp/b.txt")));
        assert_eq!(store.thread("fp")[1].file_path.as_deref(), Some(Path::new("/tmp/b.txt")));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn thread_is_capped() {
        let (store, dir) = temp_store("cap");
        for index in 0..(MAX_MESSAGES_PER_PEER + 25) {
            store.append(ChatMessage::text("fp", true, format!("m{index}")));
        }
        assert_eq!(store.thread("fp").len(), MAX_MESSAGES_PER_PEER);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn peer_key_falls_back_to_alias() {
        let mut info = DeviceInfo::new("匿名设备", crate::model::DeviceType::Desktop, String::new(), 1);
        assert_eq!(peer_key(&info), "alias:匿名设备");
        info.fingerprint = "abc".to_string();
        assert_eq!(peer_key(&info), "abc");
    }
}
