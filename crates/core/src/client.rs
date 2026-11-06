//! 发送端：走 LocalSend v2 的三段式（register → prepare-upload → upload）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use anyhow::{anyhow, bail, Context as _, Result};
use reqwest::header::CONTENT_LENGTH;
use tokio::io::{AsyncRead, ReadBuf};
use tokio::sync::watch;
use tokio_util::io::ReaderStream;

/// 上传流的读缓冲大小。局域网直传，4MB 的块能显著减少 syscall 与 TLS 记录切分，
/// 把千兆网络吞吐跑满（64KB 时单连接 HTTP/1.1 会被小缓冲拖慢）。
const UPLOAD_BUF_SIZE: usize = 4 * 1024 * 1024;

use crate::chat::{peer_key, ChatEnvelope, ChatMessage, ChatPayload, MessageStatus};
use crate::events::AppEvent;
use crate::model::{
    DeviceInfo, Direction, DiscoveredDevice, FileMetadata, PrepareUploadRequest,
    PrepareUploadResponse, RegisterResponse,
};
use crate::state::SharedState;
use crate::tls;

/// 等待对方用户点击「接受」的最长时间。
const DECISION_TIMEOUT: Duration = Duration::from_secs(600);

/// 待发送的文件（`name` 可能与磁盘上的名字不同，目录内容会被扁平化）。
#[derive(Debug, Clone)]
pub struct OutgoingFile {
    pub path: PathBuf,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct SendReport {
    pub session_id: String,
    pub files: usize,
    pub bytes: u64,
}

/// 把拖入的文件 / 目录展开成待发送列表（目录递归展开，文件名扁平化为 `目录_文件`）。
pub fn collect_files(inputs: &[PathBuf]) -> Vec<OutgoingFile> {
    let mut out: Vec<OutgoingFile> = Vec::new();
    for input in inputs {
        if input.is_dir() {
            walk_dir(input, input, &mut out);
        } else if input.is_file() {
            let name = input
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "unnamed".to_string());
            out.push(OutgoingFile { path: input.clone(), name });
        }
    }
    out.dedup_by(|a, b| a.path == b.path);
    out
}

fn walk_dir(root: &Path, dir: &Path, out: &mut Vec<OutgoingFile>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            walk_dir(root, &path, out);
        } else if path.is_file() {
            let relative = path.strip_prefix(root).unwrap_or(&path);
            let name = relative
                .components()
                .map(|component| component.as_os_str().to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join("_");
            out.push(OutgoingFile { path, name });
        }
    }
}

/// 构造带指纹校验的 HTTP 客户端。
pub fn build_client(fingerprint: Option<&str>) -> Result<reqwest::Client> {
    let tls_config = tls::client_config(fingerprint)?;
    reqwest::Client::builder()
        // 局域网直连：绝不能走系统代理，否则开了 VPN / 代理软件的用户会全部失败
        .no_proxy()
        .use_preconfigured_tls(tls_config)
        .http1_only()
        .connect_timeout(Duration::from_secs(5))
        .pool_idle_timeout(Duration::from_secs(20))
        .build()
        .context("构建 HTTP 客户端失败")
}

/// 探测某台设备的信息（用于手动输入 IP 添加设备）。
pub async fn probe(base_url: &str, fingerprint: Option<&str>) -> Result<DeviceInfo> {
    let client = build_client(fingerprint)?;
    let response = client
        .get(format!("{base_url}/api/localsend/v2/info"))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .with_context(|| format!("连接 {base_url} 失败"))?;
    if !response.status().is_success() {
        bail!("对方返回状态 {}", response.status());
    }
    response.json::<DeviceInfo>().await.context("解析设备信息失败")
}

/// 发送一批文件。
pub async fn send_files(
    state: Arc<SharedState>,
    target: DiscoveredDevice,
    files: Vec<OutgoingFile>,
    cancel: watch::Receiver<bool>,
) -> Result<SendReport> {
    if files.is_empty() {
        bail!("没有需要发送的文件");
    }

    let client = build_client(Some(&target.info.fingerprint))?;
    let base_url = target.base_url();

    // 1) register：交换设备信息，顺便探测对方是否可达 / 是否拒绝
    match client
        .post(format!("{base_url}/api/localsend/v2/register"))
        .json(&state.device_info())
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => {
            if let Ok(info) = response.json::<RegisterResponse>().await {
                state.emit(AppEvent::log_info(format!("已连接到 {}", info.alias)));
            }
        }
        Ok(response) if response.status() == reqwest::StatusCode::FORBIDDEN => {
            bail!("对方拒绝接收来自本机的文件");
        }
        Ok(response) => {
            state.emit(AppEvent::log_warn(format!(
                "register 返回 {}，继续尝试发送",
                response.status()
            )));
        }
        Err(err) => {
            state.emit(AppEvent::log_warn(format!("register 失败：{err}，继续尝试发送")));
        }
    }

    // 2) 组装元数据
    let mut entries: Vec<(FileMetadata, PathBuf)> = Vec::new();
    let mut request_files: HashMap<String, FileMetadata> = HashMap::new();
    let mut total_bytes = 0u64;
    for file in &files {
        let metadata = tokio::fs::metadata(&file.path)
            .await
            .with_context(|| format!("读取 {} 失败", file.path.display()))?;
        if !metadata.is_file() {
            continue;
        }
        let mut meta = FileMetadata::from_path(&file.path, metadata.len());
        meta.file_name = file.name.clone();
        total_bytes = total_bytes.saturating_add(meta.size);
        request_files.insert(meta.id.clone(), meta.clone());
        entries.push((meta, file.path.clone()));
    }
    if entries.is_empty() {
        bail!("没有可发送的文件");
    }

    // 3) prepare-upload：等对方确认
    state.emit(AppEvent::log_info(format!(
        "等待 {} 确认接收 {} 个文件…",
        target.info.alias,
        entries.len()
    )));
    let request = PrepareUploadRequest { info: state.device_info(), files: request_files };
    let response = tokio::select! {
        result = client
            .post(format!("{base_url}/api/localsend/v2/prepare-upload"))
            .json(&request)
            .timeout(DECISION_TIMEOUT)
            .send() => result.context("发送接收请求失败")?,
        _ = wait_cancel(cancel.clone()) => bail!("已取消"),
    };

    let status = response.status();
    match status.as_u16() {
        200 => {}
        403 => bail!("对方拒绝了本次传输"),
        409 => bail!("本机已被对方屏蔽"),
        429 => bail!("对方正忙，请稍后再试"),
        other => bail!("对方返回异常状态 {other}"),
    }
    let prepared: PrepareUploadResponse = response.json().await.context("解析会话信息失败")?;

    state.emit(AppEvent::SessionStarted {
        session_id: prepared.session_id.clone(),
        direction: Direction::Send,
        peer: target.info.alias.clone(),
        files: entries.len(),
        total_bytes,
    });

    // 4) 逐个上传
    let mut sent_bytes = 0u64;
    for (meta, path) in &entries {
        let token = prepared
            .files
            .get(&meta.id)
            .ok_or_else(|| anyhow!("对方未返回文件 {} 的 token", meta.file_name))?
            .clone();
        match upload_one(
            &client,
            &base_url,
            &prepared.session_id,
            meta,
            path,
            token,
            state.clone(),
            cancel.clone(),
        )
        .await
        {
            Ok(bytes) => {
                sent_bytes += bytes;
                state.emit(AppEvent::FileFinished {
                    session_id: prepared.session_id.clone(),
                    file_name: meta.file_name.clone(),
                    path: None,
                    size: bytes,
                    direction: Direction::Send,
                });
            }
            Err(err) => {
                // 通知对方取消整个会话
                let _ = client
                    .post(format!("{base_url}/api/localsend/v2/cancel"))
                    .query(&[("sessionId", prepared.session_id.as_str())])
                    .timeout(Duration::from_secs(5))
                    .send()
                    .await;
                state.emit(AppEvent::SessionFailed {
                    session_id: prepared.session_id.clone(),
                    direction: Direction::Send,
                    error: err.to_string(),
                });
                return Err(err);
            }
        }
    }

    state.emit(AppEvent::SessionFinished {
        session_id: prepared.session_id.clone(),
        direction: Direction::Send,
        files: entries.len(),
        bytes: sent_bytes,
        dir: None,
    });

    Ok(SendReport { session_id: prepared.session_id, files: entries.len(), bytes: sent_bytes })
}

#[allow(clippy::too_many_arguments)]
async fn upload_one(
    client: &reqwest::Client,
    base_url: &str,
    session_id: &str,
    meta: &FileMetadata,
    path: &Path,
    token: String,
    state: Arc<SharedState>,
    cancel: watch::Receiver<bool>,
) -> Result<u64> {
    let file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("打开 {} 失败", path.display()))?;
    let total = meta.size;
    let done = Arc::new(AtomicU64::new(0));

    let reader = ProgressReader {
        inner: tokio::io::BufReader::with_capacity(UPLOAD_BUF_SIZE, file),
        done: done.clone(),
    };
    let body = reqwest::Body::wrap_stream(ReaderStream::with_capacity(reader, UPLOAD_BUF_SIZE));

    // 进度上报：每读满一块立即上报，但对界面限频，避免刷屏。
    let ticker = {
        let state = state.clone();
        let file_name = meta.file_name.clone();
        let session_id = session_id.to_string();
        let done = done.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_millis(50));
            let mut last = 0u64;
            loop {
                ticker.tick().await;
                let transferred = done.load(Ordering::Relaxed);
                if transferred != last || transferred >= total {
                    state.emit(AppEvent::FileProgress {
                        session_id: session_id.clone(),
                        file_name: file_name.clone(),
                        transferred,
                        total: total.max(transferred),
                        direction: Direction::Send,
                    });
                    last = transferred;
                }
                if transferred >= total {
                    return;
                }
            }
        })
    };

    let result = tokio::select! {
        result = client
            .post(format!("{base_url}/api/localsend/v2/upload"))
            .query(&[("sessionId", session_id), ("fileId", meta.id.as_str()), ("token", token.as_str())])
            .header(CONTENT_LENGTH, total)
            .body(body)
            .send() => result,
        _ = wait_cancel(cancel) => {
            ticker.abort();
            bail!("已取消");
        }
    };

    ticker.abort();

    let response = result.with_context(|| format!("上传 {} 失败", meta.file_name))?;
    let status = response.status();
    if !status.is_success() {
        bail!("对方返回状态 {status}");
    }

    state.emit(AppEvent::FileProgress {
        session_id: session_id.to_string(),
        file_name: meta.file_name.clone(),
        transferred: total,
        total,
        direction: Direction::Send,
    });
    Ok(total)
}

async fn wait_cancel(mut receiver: watch::Receiver<bool>) {
    loop {
        if *receiver.borrow() {
            return;
        }
        if receiver.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/// 统计已读字节数的包装器，用于上传进度。
struct ProgressReader<R> {
    inner: R,
    done: Arc<AtomicU64>,
}

impl<R: AsyncRead + Unpin> AsyncRead for ProgressReader<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        match Pin::new(&mut this.inner).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                let read = buf.filled().len().saturating_sub(before);
                this.done.fetch_add(read as u64, Ordering::Relaxed);
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

/// 发送一条文本聊天消息。
///
/// 先本地落库（状态 `Sending`），再投递；失败时标记为 `Failed` 并把错误返回给界面。
/// 这样即使对方不在线，消息也不会凭空消失。
pub async fn send_chat(
    state: Arc<SharedState>,
    target: &DiscoveredDevice,
    text: &str,
) -> Result<ChatMessage> {
    let text = text.trim();
    if text.is_empty() {
        bail!("消息为空");
    }

    let peer = peer_key(&target.info);
    let message = ChatMessage::text(&peer, true, text);
    state.chats.touch_alias(&peer, &target.info.alias);
    state.chats.append(message.clone());
    state.emit(AppEvent::ChatUpdated { peer: peer.clone() });

    let client = build_client(Some(&target.info.fingerprint))?;
    let envelope = ChatEnvelope {
        info: state.device_info(),
        messages: vec![ChatPayload::from_message(&message)],
    };
    let result = client
        .post(format!("{}/api/feigecraft/chat", target.base_url()))
        .json(&envelope)
        .timeout(Duration::from_secs(8))
        .send()
        .await;

    let outcome = match result {
        Ok(response) if response.status().is_success() => Ok(()),
        Ok(response) if response.status() == reqwest::StatusCode::NOT_FOUND => {
            Err(anyhow!("对方版本不支持聊天（仅支持收发文件）"))
        }
        Ok(response) => Err(anyhow!("对方返回 {}", response.status())),
        Err(err) => Err(anyhow!("发送失败：{err}")),
    };

    match outcome {
        Ok(()) => {
            state.chats.set_status(&peer, &message.id, MessageStatus::Sent);
            state.emit(AppEvent::ChatUpdated { peer });
            Ok(message)
        }
        Err(err) => {
            state.chats.set_status(&peer, &message.id, MessageStatus::Failed);
            state.emit(AppEvent::ChatUpdated { peer });
            Err(err)
        }
    }
}

/// 会话列表里的一行预览文案。
pub fn preview_of(message: &ChatMessage) -> String {
    match message.kind {
        crate::chat::MessageKind::Text => message.text.clone(),
        crate::chat::MessageKind::File => format!("[文件] {}", message.file_name),
        crate::chat::MessageKind::System => message.text.clone(),
    }
}
