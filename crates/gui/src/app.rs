//! FeigeCraft 桌面端：以「会话」为中心的局域网通信界面。
//!
//! 布局对齐主流聊天软件：
//! 左侧会话列表（头像 / 备注 / 最后一条消息 / 未读）→ 右侧消息流（气泡 + 文件卡片）
//! → 底部输入区（表情 / 附件 / 回车发送）。

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, pos2, Align, Align2, Color32, FontId, Layout, Rect, RichText, ScrollArea, Sense, Stroke,
    ThemePreference, Vec2, ViewportCommand,
};
use feigecraft_core::chat::{peer_key, ChatMessage, MessageKind, MessageStatus};
use feigecraft_core::client::{collect_files, send_chat, send_files};
use feigecraft_core::config::Config;
use feigecraft_core::events::AppEvent;
use feigecraft_core::model::{Direction, DiscoveredDevice, DeviceInfo, DeviceType, FileMetadata};
use feigecraft_core::node::Node;
use feigecraft_core::util::{human_size, reveal_path};
use tokio::sync::{broadcast, watch};

use crate::emoji::{self, EmojiSet};
use crate::i18n::{self, tf, tr};
use crate::theme;
use crate::widgets;

/// 聊天区标题栏高度（显式分区用，必须与实际绘制一致）。
const HEADER_HEIGHT: f32 = 58.0;

/// 自绘窗口标题栏高度。
const TITLEBAR_HEIGHT: f32 = 40.0;

/// 系统通知气泡只容得下两三行，长消息在这里截断。
fn clip(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        out.push('…');
    }
    out
}

/// 气泡旁头像边长。
const AVATAR_SIZE: f32 = 34.0;

/// 同一个人连续发言时，超过这个间隔就重新显示头像。
const GROUP_GAP_MS: u64 = 180_000;

/// 消息里只有一个（或几个）表情时，按这个边长画成大表情。
const BIG_EMOJI: f32 = 34.0;

/// 表情和文字混排时的表情边长：略高于 14pt 正文行高，和文字基本齐平。
const EMOJI_INLINE: f32 = 17.0;

/// 设置弹窗整体尺寸。
const SETTINGS_SIZE: Vec2 = Vec2::new(672.0, 486.0);
/// 设置弹窗左侧导航宽度。
const SETTINGS_RAIL: f32 = 158.0;

enum TaskResult {
    Scanned(usize),
    Probed(Result<DiscoveredDevice, String>),
    FilesSent(Result<(), String>),
    TextSent(Result<(), String>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RowStatus {
    Running,
    Done,
    Failed,
}

/// 一次文件传输的实时状态（按 sessionId 索引）。
#[derive(Clone)]
struct TransferState {
    peer: String,
    direction: Direction,
    file_name: String,
    file_done: u64,
    file_total: u64,
    total_bytes: u64,
    done_bytes: u64,
    files_done: usize,
    files_total: usize,
    speed: f64,
    last_sample: Instant,
    last_bytes: u64,
    status: RowStatus,
    dir: Option<PathBuf>,
}

impl TransferState {
    fn fraction(&self) -> f32 {
        if self.total_bytes == 0 {
            0.0
        } else {
            (self.done_bytes as f32 / self.total_bytes as f32).clamp(0.0, 1.0)
        }
    }
}

/// 对方发来的接收请求。
struct IncomingRequest {
    peer: String,
    sender: String,
    files: Vec<FileMetadata>,
    total_bytes: u64,
}

/// 本地发出的一批文件消息，等 `SessionStarted` 到来后绑定 sessionId。
struct PendingBatch {
    peer: String,
    ids: Vec<String>,
}

struct Toast {
    text: String,
    at: Instant,
    error: bool,
}

/// 设置弹窗的左侧导航。
#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    Appearance,
    Identity,
    Transfer,
    About,
}

impl SettingsTab {
    const ALL: [SettingsTab; 4] = [
        SettingsTab::Appearance,
        SettingsTab::Identity,
        SettingsTab::Transfer,
        SettingsTab::About,
    ];

    fn label(self) -> &'static str {
        match self {
            SettingsTab::Appearance => tr("外观"),
            SettingsTab::Identity => tr("身份"),
            SettingsTab::Transfer => tr("传输"),
            SettingsTab::About => tr("关于"),
        }
    }

    fn hint(self) -> &'static str {
        match self {
            SettingsTab::Appearance => tr("主题与配色"),
            SettingsTab::Identity => tr("别人看到的名字"),
            SettingsTab::Transfer => tr("保存位置与确认"),
            SettingsTab::About => tr("版本与身份指纹"),
        }
    }
}

/// 消息流里需要回调到 app 的动作（避免渲染时借用冲突）。
enum MsgAction {
    Reveal(PathBuf),
    Retry(String, String),
    Cancel(String),
    /// 右击菜单「复制」：把一条文本消息复制到剪贴板。
    Copy(String),
}

pub struct FeigeCraftApp {
    runtime: tokio::runtime::Runtime,
    node: Option<Arc<Node>>,
    events: Option<broadcast::Receiver<AppEvent>>,
    task_tx: Sender<TaskResult>,
    tasks: Receiver<TaskResult>,

    config: Config,
    draft: Config,

    devices: Vec<DiscoveredDevice>,
    active: Option<String>,
    threads: HashMap<String, Vec<ChatMessage>>,
    unread: HashMap<String, usize>,
    search: String,

    transfers: HashMap<String, TransferState>,
    pending: VecDeque<PendingBatch>,
    batches: HashMap<String, PendingBatch>,
    cancel_tx: Option<watch::Sender<bool>>,

    incoming: HashMap<String, IncomingRequest>,

    composer: String,
    emoji_open: bool,
    /// 表情按钮在本帧的屏幕矩形：浮层按它定位，不再挤占输入区高度。
    emoji_anchor: Rect,
    emojis: EmojiSet,

    settings_open: bool,
    settings_tab: SettingsTab,
    remark_target: Option<String>,
    remark_draft: String,
    /// 弹窗刚打开时把焦点交给输入框（只请求一次，否则按钮点不动）
    remark_focus: bool,
    connect_open: bool,
    /// 截图走查用：让「更多」菜单保持展开（`FEIGECRAFT_UI_DEMO=menu`）。
    menu_demo: bool,
    manual_ip: String,
    connect_focus: bool,

    status: String,
    scanning: bool,
    /// 多开：本进程的实例编号（0 = 主实例）。
    instance: usize,
    /// 实例编号的占用凭证，同时负责接收「把窗口叫出来」的请求。
    instance_guard: Option<crate::instance::Instance>,
    /// 已经点了关闭、正在倒计时收起窗口（留一点时间让提示被看见）。
    hide_at: Option<Instant>,
    /// 截图模式收尾 / 明确要求退出时为真，此时不再拦截关窗。
    quitting: bool,
    /// 还剩几帧就收进后台（`--background` 启动时用）。
    /// 不能在第一帧就发隐藏：eframe 会在首帧渲染后强制 `set_visible(true)` 防白闪，
    /// 先发的命令会被它盖掉。
    hide_frames: u8,
    /// 输入框是否处于聚焦态（上一帧实测，用于画聚焦描边）
    composer_focused: bool,
    /// 消息不足一屏时，把它顶到贴近输入框的位置（下一帧生效）
    bottom_pad: f32,
    /// 输入区高度（上一帧实测，用于分区）
    composer_h: f32,
    /// 输入区整块的屏幕矩形（表情浮层贴着它的上沿弹出）
    composer_rect: Rect,
    toasts: Vec<Toast>,
    shot: Option<ShotState>,
}

struct ShotState {
    path: PathBuf,
    frames: u32,
    requested: bool,
}

impl FeigeCraftApp {
    pub fn new(
        ctx: &egui::Context,
        instance: usize,
        guard: Option<crate::instance::Instance>,
        background: bool,
    ) -> Self {
        // 控制端口的监听放到独立线程：窗口隐藏后这个线程还要替我们把窗口叫醒。
        if let Some(guard) = &guard {
            guard.watch(ctx.clone());
        }

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("创建 tokio 运行时失败");
        let config = Config::load();
        // 语言要在拼第一句状态文案之前就定下来，否则状态栏会先是中文再跳成英文。
        // `FEIGECRAFT_LANG` 可临时覆盖（截图 / 验收用）。
        let lang_key =
            std::env::var("FEIGECRAFT_LANG").unwrap_or_else(|_| config.language.clone());
        i18n::set(i18n::resolve(&lang_key));
        // 主题偏好要在第一帧之前装好，否则会先闪一帧默认深色。
        // `FEIGECRAFT_THEME` 可临时覆盖（截图 / 验收用）。
        let theme_key = std::env::var("FEIGECRAFT_THEME").unwrap_or_else(|_| config.theme.clone());
        ctx.set_theme(theme::preference(&theme_key));
        theme::sync(ctx.theme());
        let (task_tx, tasks) = channel();

        let node = match runtime.block_on(Node::start(config.clone())) {
            Ok(node) => Some(node),
            Err(err) => {
                tracing::error!("启动失败: {err}");
                None
            }
        };
        let events = node.as_ref().map(|node| node.subscribe());
        let status = match &node {
            Some(node) => {
                let port = node.device_info().port;
                if instance == 0 {
                    tf(tr("已就绪 · 监听 {}"), &[&port])
                } else {
                    tf(tr("实例 {} · 监听 {}"), &[&(instance + 1), &port])
                }
            }
            None => tr("服务启动失败，请查看日志").to_string(),
        };

        let mut app = Self {
            runtime,
            node,
            events,
            task_tx,
            tasks,
            draft: config.clone(),
            config,
            devices: Vec::new(),
            active: None,
            threads: HashMap::new(),
            unread: HashMap::new(),
            search: String::new(),
            transfers: HashMap::new(),
            pending: VecDeque::new(),
            batches: HashMap::new(),
            cancel_tx: None,
            incoming: HashMap::new(),
            composer: String::new(),
            emoji_open: false,
            emoji_anchor: Rect::NOTHING,
            emojis: EmojiSet::load(ctx),
            settings_open: false,
            settings_tab: SettingsTab::Appearance,
            remark_target: None,
            remark_draft: String::new(),
            remark_focus: false,
            connect_open: false,
            menu_demo: false,
            manual_ip: String::new(),
            connect_focus: false,
            status,
            scanning: false,
            instance,
            instance_guard: guard,
            hide_at: None,
            quitting: false,
            hide_frames: if background { 2 } else { 0 },
            composer_focused: false,
            bottom_pad: 0.0,
            composer_h: 142.0,
            composer_rect: Rect::NOTHING,
            toasts: Vec::new(),
            shot: std::env::var("FEIGECRAFT_SCREENSHOT")
                .ok()
                .map(|path| ShotState { path: PathBuf::from(path), frames: 0, requested: false }),
        };

        if let Ok(mode) = std::env::var("FEIGECRAFT_UI_DEMO") {
            app.seed_demo(&mode);
        }
        app.spawn_tray_notifier();
        app
    }

    /// 托盘提醒不能挂在 `update()` 上：窗口收进后台后 winit 不再产生重绘帧，
    /// `update()` 根本不会被调用，消息提醒就永远不会弹。
    /// 这里单独订阅一路事件广播（`broadcast` 本来就支持多订阅者），
    /// 用独立线程处理，和界面渲染彻底解耦。
    fn spawn_tray_notifier(&mut self) {
        let Some(node) = &self.node else { return };
        let mut events = node.subscribe();
        tracing::info!("托盘通知线程已启动");
        std::thread::spawn(move || {
            while let Ok(event) = events.blocking_recv() {
                tracing::debug!("托盘收到事件 {event:?}");
                // 窗口在前台时用户自己就看见了，不用再提醒。
                if crate::tray::is_foreground() {
                    tracing::debug!("窗口在前台，跳过提醒");
                    continue;
                }
                match event {
                    AppEvent::ChatReceived { alias, text, .. } => {
                        crate::tray::bump_unread();
                        crate::tray::notify(&alias, &clip(&text, 60));
                    }
                    AppEvent::IncomingRequest { sender, files, .. } => {
                        crate::tray::bump_unread();
                        let body = tf(tr("{} 个文件，点开确认"), &[&files.len()]);
                        crate::tray::notify(&sender.alias, &body);
                    }
                    _ => {}
                }
            }
        });
    }

    // ------------------------------------------------------------ 数据辅助

    fn device_by_peer(&self, peer: &str) -> Option<DiscoveredDevice> {
        self.devices
            .iter()
            .find(|device| peer_key(&device.info) == peer)
            .cloned()
    }

    fn is_online(&self, peer: &str) -> bool {
        self.devices.iter().any(|device| peer_key(&device.info) == peer)
    }

    /// 显示名：备注 > 对方广播的名字 > 占位。
    fn display_name(&self, peer: &str) -> String {
        // 设了备注时保留原名，展示成「原昵称（备注名）」——只显示备注的话，
        // 对方改名后会对不上号。
        if let Some(remark) = self.config.remark_for(peer) {
            let alias = self
                .devices
                .iter()
                .find(|item| peer_key(&item.info) == peer)
                .map(|item| item.info.alias.clone())
                .or_else(|| {
                    self.node
                        .as_ref()
                        .and_then(|node| node.state.chats.alias(peer))
                });
            return match alias {
                Some(alias) if alias != remark => format!("{alias}（{remark}）"),
                _ => remark.to_string(),
            };
        }
        if let Some(device) = self.devices.iter().find(|item| peer_key(&item.info) == peer) {
            return device.info.alias.clone();
        }
        if let Some(node) = &self.node {
            if let Some(alias) = node.state.chats.alias(peer) {
                return alias;
            }
        }
        tr("未知设备").to_string()
    }

    fn reload_thread(&mut self, peer: &str) {
        let Some(node) = &self.node else { return };
        let messages = node.state.chats.thread(peer);
        self.threads.insert(peer.to_string(), messages);
    }

    fn select(&mut self, peer: &str) {
        self.active = Some(peer.to_string());
        self.unread.remove(peer);
        self.reload_thread(peer);
        self.emoji_open = false;
    }

    fn toast(&mut self, text: impl Into<String>, error: bool) {
        self.toasts.push(Toast { text: text.into(), at: Instant::now(), error });
        if self.toasts.len() > 3 {
            self.toasts.remove(0);
        }
    }

    // ------------------------------------------------------------ 事件

    fn pump_events(&mut self) {
        let mut closed = false;
        loop {
            let outcome = match self.events.as_mut() {
                None => break,
                Some(receiver) => receiver.try_recv(),
            };
            match outcome {
                Ok(event) => self.on_event(event),
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
                Err(broadcast::error::TryRecvError::Closed) => {
                    closed = true;
                    break;
                }
            }
        }
        if closed {
            self.events = None;
        }
    }

    fn pump_tasks(&mut self) {
        while let Ok(outcome) = self.tasks.try_recv() {
            match outcome {
                TaskResult::Scanned(count) => {
                    self.scanning = false;
                    self.status = tf(tr("扫描完成 · 当前在线 {} 台"), &[&self.devices.len()]);
                    if count == 0 && self.devices.is_empty() {
                        self.toast(tr("没有扫描到设备，请检查防火墙或手动输入 IP"), true);
                    } else if count > 0 {
                        self.toast(tf(tr("扫描完成，新发现 {} 台设备"), &[&count]), false);
                    }
                }
                TaskResult::Probed(Ok(device)) => {
                    self.upsert_device(device.clone());
                    self.select(&peer_key(&device.info));
                    self.status = tf(tr("已连接到 {}"), &[&device.info.alias]);
                }
                TaskResult::Probed(Err(err)) => {
                    self.toast(tf(tr("连接失败：{}"), &[&err]), true);
                }
                TaskResult::FilesSent(Err(err)) => {
                    if let Some(batch) = self.pending.pop_front() {
                        self.fail_batch(&batch, &err);
                    }
                    self.toast(tf(tr("发送失败：{}"), &[&err]), true);
                }
                TaskResult::FilesSent(Ok(())) => {
                    self.status = tr("发送完成").to_string();
                }
                TaskResult::TextSent(Err(err)) => {
                    self.toast(err, true);
                }
                TaskResult::TextSent(Ok(())) => {}
            }
        }
    }

    fn fail_batch(&mut self, batch: &PendingBatch, reason: &str) {
        let Some(node) = &self.node else { return };
        for id in &batch.ids {
            node.state.chats.set_status(&batch.peer, id, MessageStatus::Failed);
        }
        node.state.chats.append(ChatMessage::system(
            &batch.peer,
            tf(tr("发送失败：{}"), &[&reason]),
        ));
        node.state.emit(AppEvent::ChatUpdated { peer: batch.peer.clone() });
        self.reload_thread(&batch.peer);
    }

    fn on_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::DeviceFound(device) => {
                if let Some(node) = &self.node {
                    node.state.chats.touch_alias(&peer_key(&device.info), &device.info.alias);
                }
                self.upsert_device(device);
            }
            AppEvent::DeviceLost { fingerprint } => {
                self.devices.retain(|device| device.info.fingerprint != fingerprint);
            }
            AppEvent::ChatUpdated { peer } => {
                self.reload_thread(&peer);
            }
            AppEvent::ChatReceived { peer, alias, text } => {
                self.reload_thread(&peer);
                if self.active.as_deref() != Some(peer.as_str()) {
                    *self.unread.entry(peer.clone()).or_insert(0) += 1;
                    self.status = tf(tr("{}：{}"), &[&alias, &text]);
                }
            }
            AppEvent::IncomingRequest { session_id, sender, files, total_bytes } => {
                let peer = peer_key(&sender);
                let name = sender.alias.clone();
                let count = files.len();
                self.incoming.insert(
                    session_id,
                    IncomingRequest { peer: peer.clone(), sender: name.clone(), files, total_bytes },
                );
                if self.active.as_deref() != Some(peer.as_str()) {
                    self.toast(tf(tr("{} 想发送 {} 个文件"), &[&name, &count]), false);
                    *self.unread.entry(peer).or_insert(0) += 1;
                }
                self.status = tf(tr("{} 想发送文件，等待确认"), &[&name]);
            }
            AppEvent::RequestResolved { session_id, accepted } => {
                if let Some(request) = self.incoming.remove(&session_id) {
                    if accepted {
                        self.create_incoming_bubbles(&request, &session_id);
                    } else {
                        if let Some(node) = &self.node {
                            node.state.chats.append(ChatMessage::system(
                                &request.peer,
                                tf(
                                    tr("已拒绝来自 {} 的 {} 个文件"),
                                    &[&request.sender, &request.files.len()],
                                ),
                            ));
                        }
                        self.reload_thread(&request.peer);
                    }
                }
            }
            AppEvent::SessionStarted { session_id, direction, peer, files, total_bytes } => {
                let peer_key_value = match direction {
                    Direction::Send => {
                        // 把本地已经画出来的文件气泡与这次会话绑定
                        match self.pending.pop_front() {
                            Some(batch) => {
                                if let Some(node) = &self.node {
                                    node.state
                                        .chats
                                        .bind_session(&batch.peer, &batch.ids, &session_id);
                                }
                                self.reload_thread(&batch.peer);
                                let key = batch.peer.clone();
                                self.batches.insert(session_id.clone(), batch);
                                key
                            }
                            None => peer.clone(),
                        }
                    }
                    Direction::Receive => self
                        .devices
                        .iter()
                        .find(|device| device.info.alias == peer)
                        .map(|device| peer_key(&device.info))
                        .unwrap_or_else(|| format!("alias:{peer}")),
                };
                self.transfers.insert(
                    session_id,
                    TransferState {
                        peer: peer_key_value,
                        direction,
                        file_name: String::new(),
                        file_done: 0,
                        file_total: 0,
                        total_bytes,
                        done_bytes: 0,
                        files_done: 0,
                        files_total: files,
                        speed: 0.0,
                        last_sample: Instant::now(),
                        last_bytes: 0,
                        status: RowStatus::Running,
                        dir: None,
                    },
                );
            }
            AppEvent::FileProgress { session_id, file_name, transferred, total, .. } => {
                let Some(state) = self.transfers.get_mut(&session_id) else {
                    return;
                };
                if state.file_name != file_name {
                    state.done_bytes = state.done_bytes.saturating_add(state.file_done);
                    if !state.file_name.is_empty() {
                        state.files_done = state.files_done.saturating_add(1);
                    }
                    state.file_name = file_name;
                    state.file_total = total;
                    state.file_done = 0;
                    state.last_bytes = state.done_bytes;
                    state.last_sample = Instant::now();
                }
                state.file_done = transferred;
                let now = Instant::now();
                if now.duration_since(state.last_sample) >= Duration::from_millis(400) {
                    let elapsed = now.duration_since(state.last_sample).as_secs_f64().max(0.001);
                    let bytes = state.done_bytes.saturating_add(state.file_done);
                    state.speed = bytes.saturating_sub(state.last_bytes) as f64 / elapsed;
                    state.last_sample = now;
                    state.last_bytes = bytes;
                }
            }
            AppEvent::FileFinished { session_id, file_name, path, .. } => {
                let peer = self.transfers.get(&session_id).map(|state| state.peer.clone());
                if let (Some(peer), Some(path)) = (peer, path.clone()) {
                    if let Some(node) = &self.node {
                        node.state.chats.set_file_path(&peer, &file_name, path);
                    }
                    self.reload_thread(&peer);
                }
                if let Some(state) = self.transfers.get_mut(&session_id) {
                    state.done_bytes = state.done_bytes.saturating_add(state.file_done);
                    state.file_done = 0;
                    state.files_done = (state.files_done + 1).min(state.files_total.max(1));
                }
            }
            AppEvent::SessionFinished { session_id, direction, files, bytes, dir } => {
                let mut peer = None;
                if let Some(state) = self.transfers.get_mut(&session_id) {
                    state.status = RowStatus::Done;
                    state.done_bytes = state.total_bytes;
                    state.speed = 0.0;
                    state.dir = dir.clone();
                    peer = Some(state.peer.clone());
                }
                if let Some(batch) = self.batches.remove(&session_id) {
                    peer = Some(batch.peer.clone());
                }
                if let Some(peer) = peer {
                    if let Some(node) = &self.node {
                        let text = match direction {
                            Direction::Send => {
                                tf(tr("已发送 {} 个文件 · {}"), &[&files, &human_size(bytes)])
                            }
                            Direction::Receive => {
                                tf(tr("已接收 {} 个文件 · {}"), &[&files, &human_size(bytes)])
                            }
                        };
                        node.state.chats.append(ChatMessage::system(&peer, text));
                    }
                    self.reload_thread(&peer);
                }
                self.status = tf(tr("传输完成：{} 个文件 · {}"), &[&files, &human_size(bytes)]);
            }
            AppEvent::SessionFailed { session_id, direction, error } => {
                let mut peer = None;
                if let Some(state) = self.transfers.get_mut(&session_id) {
                    state.status = RowStatus::Failed;
                    state.speed = 0.0;
                    peer = Some(state.peer.clone());
                }
                if let Some(batch) = self.batches.remove(&session_id) {
                    peer = Some(batch.peer.clone());
                    self.fail_batch(&batch, &error);
                }
                if let Some(peer) = peer {
                    if let Some(node) = &self.node {
                        let prefix = match direction {
                            Direction::Send => tr("发送中断"),
                            Direction::Receive => tr("接收中断"),
                        };
                        node.state
                            .chats
                            .append(ChatMessage::system(&peer, tf(tr("{}：{}"), &[&prefix, &error])));
                    }
                    self.reload_thread(&peer);
                }
                self.toast(error, true);
            }
            AppEvent::Log { level, message } => {
                self.status = message.clone();
                if matches!(level, feigecraft_core::LogLevel::Error) {
                    self.toast(message, true);
                }
            }
        }
    }

    /// 自动接受时也要把文件气泡补上，保证会话里能看到收件记录。
    fn create_incoming_bubbles(&mut self, request: &IncomingRequest, session_id: &str) {
        let Some(node) = &self.node else { return };
        let mut ids = Vec::new();
        for file in &request.files {
            let message = ChatMessage::file(&request.peer, false, file.file_name.clone(), file.size);
            ids.push(message.id.clone());
            node.state.chats.append(message);
        }
        node.state.chats.bind_session(&request.peer, &ids, session_id);
        self.reload_thread(&request.peer);
    }

    fn upsert_device(&mut self, device: DiscoveredDevice) {
        let key = peer_key(&device.info);
        if let Some(existing) = self.devices.iter_mut().find(|item| peer_key(&item.info) == key) {
            *existing = device;
        } else {
            self.devices.push(device);
        }
        self.devices.sort_by_key(|item| item.info.alias.to_lowercase());
        if self.active.is_none() {
            // 首次发现自动进入会话，减少一次点击
            self.select(&key);
        }
    }

    // ------------------------------------------------------------ 动作

    fn send_current_text(&mut self) {
        let text = self.composer.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.composer.clear();
        self.send_text_value(text);
    }

    fn send_text_value(&mut self, text: String) {
        let Some(peer) = self.active.clone() else {
            self.toast(tr("先选择一个设备"), true);
            return;
        };
        let Some(device) = self.device_by_peer(&peer) else {
            self.toast(tr("对方已离线，暂时发不出去"), true);
            self.composer = text;
            return;
        };
        let Some(node) = &self.node else { return };

        let state = node.state.clone();
        let tx = self.task_tx.clone();
        self.runtime.spawn(async move {
            let result = send_chat(state, &device, &text).await;
            let _ = tx.send(TaskResult::TextSent(result.map(|_| ()).map_err(|err| err.to_string())));
        });
        self.reload_thread(&peer);
    }

    fn send_paths(&mut self, paths: Vec<PathBuf>) {
        let Some(peer) = self.active.clone() else {
            self.toast(tr("先选择一个设备，再发送文件"), true);
            return;
        };
        let Some(device) = self.device_by_peer(&peer) else {
            self.toast(tr("对方已离线，无法发送文件"), true);
            return;
        };
        let files = collect_files(&paths);
        if files.is_empty() {
            return;
        }
        let Some(node) = self.node.clone() else { return };

        let mut ids = Vec::new();
        for file in &files {
            let size = std::fs::metadata(&file.path).map(|meta| meta.len()).unwrap_or(0);
            let message = ChatMessage::file(&peer, true, file.name.clone(), size);
            ids.push(message.id.clone());
            node.state.chats.append(message);
        }
        node.state.chats.touch_alias(&peer, &device.info.alias);
        self.reload_thread(&peer);
        self.pending.push_back(PendingBatch { peer, ids });

        let state = node.state.clone();
        let tx = self.task_tx.clone();
        let (cancel_tx, cancel_rx) = watch::channel(false);
        self.cancel_tx = Some(cancel_tx);
        self.runtime.spawn(async move {
            let result = send_files(state, device, files, cancel_rx).await;
            let _ = tx.send(TaskResult::FilesSent(result.map(|_| ()).map_err(|err| err.to_string())));
        });
    }

    fn pick_files(&mut self) {
        if let Some(paths) = rfd::FileDialog::new().pick_files() {
            self.send_paths(paths);
        }
    }

    fn respond_incoming(&mut self, session_id: &str, accept: bool) {
        if let Some(node) = &self.node {
            node.receiver.resolve(session_id, accept);
        }
    }

    fn start_scan(&mut self) {
        let Some(node) = &self.node else { return };
        let node = node.clone();
        let tx = self.task_tx.clone();
        self.scanning = true;
        self.status = tr("正在扫描局域网…").to_string();
        self.runtime.spawn(async move {
            let count = node.scan().await;
            let _ = tx.send(TaskResult::Scanned(count));
        });
    }

    fn connect_manual(&mut self) {
        let ip = self.manual_ip.trim().to_string();
        if ip.is_empty() {
            self.toast(tr("请输入对方 IP 或网段"), true);
            return;
        }

        // 输入的是网段（`192.168.2.0/24` / `192.168.2.`）时扫描整段：
        // 跨网段时路由器不转发组播，这是唯一能自动找到对方的方式。
        if ip.contains('/') || ip.ends_with('.') {
            let Some((network, mask)) = feigecraft_core::util::parse_subnet_spec(&ip) else {
                self.toast(tr("网段格式不对，例如 192.168.2.0/24"), true);
                return;
            };
            let Some(discovery) = self.node.as_ref().and_then(|node| node.discovery.clone())
            else {
                self.toast(tr("发现服务未启动，无法扫描网段"), true);
                return;
            };
            let tx = self.task_tx.clone();
            self.status = tf(tr("正在扫描网段 {}…"), &[&ip]);
            self.runtime.spawn(async move {
                match discovery.scan_subnet(network, mask).await {
                    Ok(count) => {
                        let _ = tx.send(TaskResult::Scanned(count));
                    }
                    Err(err) => {
                        tracing::warn!("扫描网段失败: {err}");
                        let _ = tx.send(TaskResult::Scanned(0));
                    }
                }
            });
            self.connect_open = false;
            self.manual_ip.clear();
            return;
        }

        let Some(node) = &self.node else { return };
        let tx = self.task_tx.clone();
        let base = if ip.starts_with("http") {
            ip.clone()
        } else if ip.contains(':') {
            format!("https://{ip}")
        } else {
            format!("https://{ip}:53317")
        };
        self.runtime.spawn(async move {
            let result = feigecraft_core::client::probe(&base, None).await;
            let mapped = match result {
                Ok(info) => {
                    let host = base
                        .trim_start_matches("https://")
                        .trim_start_matches("http://")
                        .split(':')
                        .next()
                        .unwrap_or_default()
                        .to_string();
                    match host.parse() {
                        Ok(ip) => Ok(DiscoveredDevice {
                            info,
                            ip,
                            last_seen: Instant::now(),
                            via_multicast: false,
                        }),
                        Err(err) => Err(tf(tr("地址解析失败：{}"), &[&err])),
                    }
                }
                Err(err) => Err(err.to_string()),
            };
            let _ = tx.send(TaskResult::Probed(mapped));
        });
        let _ = node;
        self.connect_open = false;
        self.manual_ip.clear();
    }

    fn save_settings(&mut self, ctx: &egui::Context) {
        let Some(node) = &self.node else { return };
        // 用户主动保存过，名字就算「自己定的」了，以后升级不再自动改成随机名。
        self.draft.alias_custom = true;
        if let Err(err) = node.apply_config(&self.draft) {
            self.toast(tf(tr("保存失败：{}"), &[&err]), true);
            return;
        }
        self.config = self.draft.clone();
        i18n::set(i18n::resolve(&self.config.language));
        ctx.set_theme(theme::preference(&self.config.theme));
        theme::sync(ctx.theme());
        self.settings_open = false;
        self.toast(tr("设置已保存"), false);
    }

    fn save_remark(&mut self) {
        let Some(peer) = self.remark_target.clone() else { return };
        let Some(node) = self.node.clone() else { return };
        if let Err(err) = node.state.set_remark(&peer, &self.remark_draft) {
            self.toast(tf(tr("备注保存失败：{}"), &[&err]), true);
        }
        self.config = node.state.config();
        self.remark_target = None;
        self.remark_draft.clear();
    }

    // ------------------------------------------------------------ 界面：标题栏

    /// 自绘标题栏：品牌 + 运行状态 + 窗口按钮。
    ///
    /// 之所以不用系统标题栏：Windows 默认那条是亮白的，和深色界面完全割裂，
    /// 而且没法跟着主题变色。自绘后深浅两套主题都是一体的。
    fn title_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("titlebar")
            .exact_height(TITLEBAR_HEIGHT)
            .frame(egui::Frame::none().fill(theme::titlebar()))
            .show_separator_line(false)
            .show(ctx, |ui| {
                let rect = ui.max_rect();
                let painter = ui.painter().clone();
                painter.rect_filled(rect, 0.0, theme::titlebar());
                painter.rect_filled(
                    Rect::from_min_size(
                        pos2(rect.min.x, rect.max.y - 1.0),
                        Vec2::new(rect.width(), 1.0),
                    ),
                    0.0,
                    theme::border_soft(),
                );

                // ---- 窗口按钮：右起 关闭 / 最大化 / 最小化 ----
                let maximized = ctx.input(|input| input.viewport().maximized).unwrap_or(false);
                let bw = 46.0;
                let bh = TITLEBAR_HEIGHT - 1.0;
                let close_rect = Rect::from_min_size(
                    pos2(rect.max.x - bw, rect.min.y),
                    Vec2::new(bw, bh),
                );
                let max_rect = close_rect.translate(Vec2::new(-bw, 0.0));
                let min_rect = max_rect.translate(Vec2::new(-bw, 0.0));

                let button = |id: &str,
                                  area: Rect,
                                  glyph: widgets::WindowGlyph,
                                  danger: bool|
                 -> bool {
                    let response = ui.interact(area, ui.id().with(id), Sense::click());
                    if response.hovered() {
                        let fill =
                            if danger { theme::err() } else { theme::titlebar_hover() };
                        painter.rect_filled(area, 0.0, fill);
                    }
                    let color = if danger && response.hovered() {
                        Color32::WHITE
                    } else {
                        theme::muted()
                    };
                    let icon = Rect::from_center_size(area.center(), Vec2::splat(11.0));
                    widgets::paint_window_glyph(&painter, icon, glyph, color);
                    response.clicked()
                };

                if button("win-close", close_rect, widgets::WindowGlyph::Close, true) {
                    self.request_close(ctx);
                }
                let max_glyph = if maximized {
                    widgets::WindowGlyph::Restore
                } else {
                    widgets::WindowGlyph::Maximize
                };
                if button("win-max", max_rect, max_glyph, false) {
                    ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
                }
                if button("win-min", min_rect, widgets::WindowGlyph::Minimize, false) {
                    ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
                }

                // ---- 左侧：品牌 + 运行状态 ----
                let center_y = rect.center().y;
                let mark = Rect::from_min_size(
                    pos2(rect.min.x + 15.0, center_y - 8.0),
                    Vec2::splat(16.0),
                );
                widgets::paint_brand(&painter, mark, theme::accent());

                let title = painter.layout_no_wrap(
                    "FeigeCraft".to_owned(),
                    FontId::proportional(12.5),
                    theme::text(),
                );
                let title_pos = pos2(mark.max.x + 8.0, center_y - title.size().y * 0.5);
                painter.galley(title_pos, title.clone(), theme::text());

                let dot_x = title_pos.x + title.size().x + 11.0;
                painter.circle_filled(pos2(dot_x, center_y), 1.6, theme::faint());
                let status = painter.layout_no_wrap(
                    self.status.clone(),
                    FontId::proportional(11.0),
                    theme::faint(),
                );
                // 状态文字必须在窗口按钮之前收住，窄窗口下自动省略。
                let limit = min_rect.min.x - 12.0;
                if dot_x + 9.0 + status.size().x < limit {
                    painter.galley(
                        pos2(dot_x + 9.0, center_y - status.size().y * 0.5),
                        status,
                        theme::faint(),
                    );
                }

                // ---- 拖动区域：按钮之外的整条 ----
                let drag_rect = Rect::from_min_max(rect.min, pos2(min_rect.min.x, rect.max.y));
                let drag = ui.interact(
                    drag_rect,
                    ui.id().with("titlebar-drag"),
                    Sense::click_and_drag(),
                );
                if drag.is_pointer_button_down_on() && ctx.input(|input| input.pointer.any_pressed())
                {
                    // 交给系统做 caption 拖动，这样贴边分屏、双击最大化都还是原生行为。
                    ctx.send_viewport_cmd(ViewportCommand::StartDrag);
                }
            });
    }

    // ------------------------------------------------------------ 界面：侧栏

    fn sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("conversations")
            .exact_width(292.0)
            .resizable(false)
            .frame(
                egui::Frame::none()
                    .fill(theme::panel())
                    .inner_margin(egui::Margin::symmetric(10.0, 10.0)),
            )
            .show(ctx, |ui| {
                self.sidebar_header(ui);
                ui.add_space(8.0);
                self.search_box(ui);
                ui.add_space(6.0);
                ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    self.conversation_list(ui);
                });
            });
    }

    fn sidebar_header(&mut self, ui: &mut egui::Ui) {
        let info = self.node.as_ref().map(|node| node.device_info());
        let alias = info
            .as_ref()
            .map(|item| item.alias.clone())
            .unwrap_or_else(|| "FeigeCraft".into());
        let port = info.as_ref().map(|item| item.port).unwrap_or(0);
        ui.horizontal(|ui| {
            widgets::avatar(ui, 36.0, &alias, &format!("me:{alias}"), true);
            ui.add_space(2.0);
            ui.vertical(|ui| {
                ui.label(RichText::new(&alias).size(14.5).strong().color(theme::text()));
                ui.label(
                    RichText::new(tf(tr("在线 · 端口 {}"), &[&port]))
                        .size(11.0)
                        .color(theme::muted()),
                );
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if widgets::icon_button(ui, widgets::Icon::Sliders, tr("设置")).clicked() {
                    self.draft = self.config.clone();
                    self.settings_tab = SettingsTab::Appearance;
                    self.settings_open = true;
                }
                if widgets::icon_button(ui, widgets::Icon::Plus, tr("手动输入 IP 连接")).clicked() {
                    self.connect_open = true;
                    self.connect_focus = true;
                }
                if widgets::icon_button(ui, widgets::Icon::Refresh, tr("扫描局域网")).clicked() {
                    self.start_scan();
                }
            });
        });
    }

    fn search_box(&mut self, ui: &mut egui::Ui) {
        egui::Frame::none()
            .fill(theme::card())
            .rounding(egui::Rounding::same(theme::RADIUS_MD))
            .inner_margin(egui::Margin::symmetric(9.0, 5.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    widgets::icon(ui, widgets::Icon::Search, 13.0, theme::faint());
                    ui.add(
                        egui::TextEdit::singleline(&mut self.search)
                            .hint_text(
                                RichText::new(tr("搜索设备或聊天记录")).size(12.5).color(theme::faint()),
                            )
                            .frame(false)
                            .desired_width(f32::INFINITY)
                            .font(FontId::proportional(12.5)),
                    );
                });
            });
    }

    fn conversation_list(&mut self, ui: &mut egui::Ui) {
        let keyword = self.search.trim().to_lowercase();
        let mut rows: Vec<(String, String, String, u64)> = Vec::new();

        // 1) 有聊天记录的会话（按最后一条消息时间倒序）
        if let Some(node) = &self.node {
            for (peer, last) in node.state.chats.peers() {
                let name = self.display_name(&peer);
                let preview = feigecraft_core::client::preview_of(&last);
                if !keyword.is_empty()
                    && !name.to_lowercase().contains(&keyword)
                    && !preview.to_lowercase().contains(&keyword)
                {
                    continue;
                }
                rows.push((peer, name, preview, last.at));
            }
        }

        // 2) 在线但还没聊过的设备
        let mut fresh: Vec<(String, String)> = Vec::new();
        for device in &self.devices {
            let peer = peer_key(&device.info);
            if rows.iter().any(|row| row.0 == peer) {
                continue;
            }
            let name = self.display_name(&peer);
            if !keyword.is_empty() && !name.to_lowercase().contains(&keyword) {
                continue;
            }
            fresh.push((peer, name));
        }
        fresh.sort_by_key(|item| item.1.to_lowercase());

        if rows.is_empty() && fresh.is_empty() {
            ui.add_space(28.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new(tr("还没有发现设备")).size(13.0).color(theme::muted()));
                ui.add_space(4.0);
                ui.label(
                    RichText::new(tr("让对方的设备打开 FeigeCraft，\n或点右上角 ＋ 输入 IP"))
                        .size(11.5)
                        .color(theme::faint()),
                );
            });
            return;
        }

        let mut clicked: Option<String> = None;
        for (peer, name, preview, at) in &rows {
            if self.conversation_row(ui, peer, name, preview, *at, false) {
                clicked = Some(peer.clone());
            }
        }

        if !fresh.is_empty() {
            ui.add_space(14.0);
            let (line, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
            ui.painter().rect_filled(line, 0.0, theme::border_soft());
            ui.add_space(10.0);
            ui.label(
                RichText::new(tf(tr("附近设备 · {}"), &[&fresh.len()]))
                    .size(10.5)
                    .color(theme::faint()),
            );
            ui.add_space(3.0);
            for (peer, name) in &fresh {
                if self.conversation_row(ui, peer, name, tr("在线 · 点击开始聊天"), 0, true) {
                    clicked = Some(peer.clone());
                }
            }
        }

        if let Some(peer) = clicked {
            self.select(&peer);
        }
    }

    /// 会话行：头像 + 名字 + 时间 + 最新消息预览 + 未读徽标。
    fn conversation_row(
        &mut self,
        ui: &mut egui::Ui,
        peer: &str,
        name: &str,
        preview: &str,
        at: u64,
        nearby: bool,
    ) -> bool {
        let selected = self.active.as_deref() == Some(peer);
        let unread = self.unread.get(peer).copied().unwrap_or(0);
        let online = self.is_online(peer);
        let pending_files = self
            .incoming
            .values()
            .find(|request| request.peer == peer)
            .map(|request| request.files.len());

        let width = ui.available_width();
        let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 60.0), Sense::click());
        let bg = if selected {
            theme::row_active()
        } else if response.hovered() {
            theme::card()
        } else {
            Color32::TRANSPARENT
        };
        if bg != Color32::TRANSPARENT {
            ui.painter().rect_filled(rect, egui::Rounding::same(theme::RADIUS_LG), bg);
        }
        if selected {
            let bar = egui::Rect::from_min_size(
                rect.left_top() + Vec2::new(0.0, 14.0),
                Vec2::new(3.0, rect.height() - 28.0),
            );
            ui.painter().rect_filled(bar, egui::Rounding::same(2.0), theme::accent());
        }

        ui.allocate_new_ui(
            egui::UiBuilder::new()
                .max_rect(rect.shrink2(Vec2::new(10.0, 8.0)))
                .layout(Layout::left_to_right(Align::Center)),
            |ui| {
                widgets::avatar(ui, 38.0, name, peer, online);
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        let color =
                            if selected { theme::row_active_text() } else { theme::text() };
                        ui.add(
                            egui::Label::new(RichText::new(name).size(13.5).color(color))
                                .truncate(),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if unread > 0 {
                                let text =
                                    if unread > 99 { "99+".to_string() } else { unread.to_string() };
                                let galley = ui.painter().layout_no_wrap(
                                    text,
                                    FontId::proportional(10.0),
                                    Color32::WHITE,
                                );
                                let size = Vec2::new(galley.size().x.max(8.0) + 10.0, 15.0);
                                let (badge, _) = ui.allocate_exact_size(size, Sense::hover());
                                ui.painter().rect_filled(
                                    badge,
                                    egui::Rounding::same(theme::RADIUS_SM),
                                    theme::err(),
                                );
                                ui.painter().galley(
                                    badge.center() - galley.size() * 0.5,
                                    galley,
                                    Color32::WHITE,
                                );
                            } else if at > 0 {
                                ui.label(
                                    RichText::new(widgets::list_time(at))
                                        .size(10.5)
                                        .color(theme::faint()),
                                );
                            }
                        });
                    });
                    // 第二行：优先显示待收文件提示，其次是最新聊天消息预览，
                    // 附近设备（还没聊过）则显示引导。
                    let preview_text = match pending_files {
                        Some(count) => Some(tf(tr("[文件] 对方想发送 {} 个文件"), &[&count])),
                        None if nearby => Some(tr("在线 · 点击开始聊天").to_string()),
                        None => Some(preview.to_string()),
                    };
                    if let Some(text) = preview_text {
                        let color =
                            if pending_files.is_some() { theme::warn() } else { theme::muted() };
                        ui.add(
                            egui::Label::new(RichText::new(text).size(11.5).color(color))
                                .truncate(),
                        );
                    }
                });
            },
        );

        response.clicked()
    }

    // ------------------------------------------------------------ 界面：聊天区

    fn chat_area(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(theme::bg()))
            .show(ctx, |ui| {
                let Some(peer) = self.active.clone() else {
                    self.empty_state(ui);
                    return;
                };

                // 三块显式分区：标题栏 / 消息流 / 输入区。
                // 不用 TopBottomPanel::show_inside —— 它不会真正收窄父 ui 的 max_rect，
                // 消息区会一路延伸到窗口底部、盖住输入框。
                let full = ui.max_rect();
                let composer_h = self.composer_h.clamp(120.0, (full.height() * 0.45).max(120.0));
                let header_rect = egui::Rect::from_min_size(
                    full.min,
                    Vec2::new(full.width(), HEADER_HEIGHT),
                );
                let list_rect = egui::Rect::from_min_max(
                    egui::pos2(full.min.x, full.min.y + HEADER_HEIGHT),
                    egui::pos2(full.max.x, full.max.y - composer_h),
                );
                let composer_rect = egui::Rect::from_min_max(
                    egui::pos2(full.min.x, full.max.y - composer_h),
                    full.max,
                );
                let viewport = list_rect.height();

                ui.allocate_new_ui(
                    egui::UiBuilder::new().max_rect(header_rect),
                    |ui| self.chat_header(ui, &peer),
                );
                ui.allocate_new_ui(
                    egui::UiBuilder::new().max_rect(list_rect),
                    |ui| self.message_stream(ui, &peer, viewport),
                );
                let measured = ui.allocate_new_ui(
                    egui::UiBuilder::new().max_rect(composer_rect),
                    |ui| {
                        let frame = egui::Frame::none()
                            .fill(theme::bg())
                            .inner_margin(egui::Margin::symmetric(16.0, 12.0))
                            .show(ui, |ui| {
                                self.composer_area(ui, &peer);
                            });
                        // 取 Frame 自己的 rect：`allocate_new_ui` 的返回 rect 会被撑成
                        // 整个 max_rect，拿它做高度会让输入区每帧长高一点、上方空一条。
                        frame.response.rect
                    },
                );
                // 输入区高度随多行文本变化，用上一帧的实测值做下一帧分区。
                // 表情面板是浮层，不参与这里的高度计算。
                if measured.inner.height() > 0.0 {
                    self.composer_h = measured.inner.height();
                    self.composer_rect = measured.inner;
                }
            });
    }

    fn chat_header(&mut self, ui: &mut egui::Ui, peer: &str) {
        let name = self.display_name(peer);
        let online = self.is_online(peer);
        let device = self.device_by_peer(peer);
        let subtitle = match &device {
            Some(device) => {
                let kind = device.info.device_type.unwrap_or(DeviceType::Desktop);
                let kind = i18n::device_label(kind);
                tf(tr("在线 · {} · {}"), &[&device.ip, &kind])
            }
            None => tr("离线 · 消息会保留在本地").to_string(),
        };

        let mut open_remark = false;
        let mut copy_ip = false;
        let mut clear_chat = false;
        let mut pick = false;

        egui::Frame::none()
            .fill(theme::panel())
            .inner_margin(egui::Margin::symmetric(16.0, 10.0))
            .show(ui, |ui| {
                ui.set_min_height(HEADER_HEIGHT - 20.0);
                ui.horizontal(|ui| {
                    widgets::avatar(ui, 34.0, &name, peer, online);
                    ui.add_space(2.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&name).size(15.0).strong().color(theme::text()));
                        ui.label(RichText::new(&subtitle).size(11.0).color(theme::muted()));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let popup_id = ui.make_persistent_id("chat-more-menu");
                        let more = widgets::icon_button(ui, widgets::Icon::More, tr("更多"));
                        // 这一帧的点击就是「打开菜单」的那一下，不能再当成「点在外面」，
                        // 否则菜单刚展开就被同一帧关掉（表现为点了没反应）。
                        let toggled = more.clicked();
                        if toggled {
                            ui.memory_mut(|memory| memory.toggle_popup(popup_id));
                        }
                        if self.menu_demo {
                            ui.memory_mut(|memory| memory.open_popup(popup_id));
                        }
                        if ui.memory(|memory| memory.is_popup_open(popup_id)) {
                            // 不用 `popup_below_widget`：它按左边缘对齐，菜单挂在窗口右端的
                            // 「更多」下面时会伸出窗口被裁掉。这里右对齐，并且自己处理
                            // 「点外面 / ESC 关闭」。
                            let labels = [tr("修改备注"), tr("复制对方 IP"), tr("清空聊天记录")];
                            let widest = labels
                                .iter()
                                .map(|label| {
                                    ui.painter()
                                        .layout_no_wrap(
                                            (*label).to_string(),
                                            FontId::proportional(13.0),
                                            theme::text(),
                                        )
                                        .size()
                                        .x
                                })
                                .fold(0.0_f32, f32::max);
                            let inner_w = widest + 22.0;
                            let margin = 6.0;
                            let position = pos2(
                                (more.rect.right() - inner_w - margin * 2.0).max(8.0),
                                more.rect.bottom() + 6.0,
                            );
                            let ctx = ui.ctx().clone();
                            let light = theme::palette().is_light;
                            let area = egui::Area::new(popup_id.with("area"))
                                .order(egui::Order::Foreground)
                                .fixed_pos(position)
                                .show(&ctx, |ui| {
                                    egui::Frame::none()
                                        .fill(theme::panel())
                                        .rounding(egui::Rounding::same(theme::RADIUS_LG))
                                        .stroke(Stroke::new(1.0_f32, theme::border()))
                                        .shadow(egui::epaint::Shadow {
                                            offset: Vec2::new(0.0, 10.0),
                                            blur: 28.0,
                                            spread: 0.0,
                                            color: Color32::from_black_alpha(if light {
                                                32
                                            } else {
                                                120
                                            }),
                                        })
                                        .inner_margin(egui::Margin::same(margin))
                                        .show(ui, |ui| {
                                            ui.set_min_width(inner_w);
                                            ui.spacing_mut().item_spacing.y = 1.0;
                                            // 菜单项默认透明，只画 hover 色块：默认 button
                                            // 在这里会变成一块块带描边的灰卡片。
                                            if menu_item(ui, labels[0], false, inner_w) {
                                                open_remark = true;
                                            }
                                            if menu_item(ui, labels[1], false, inner_w) {
                                                copy_ip = true;
                                            }
                                            menu_separator(ui, inner_w);
                                            if menu_item(ui, labels[2], true, inner_w) {
                                                clear_chat = true;
                                            }
                                        });
                                });
                            let pointer = ctx.input(|input| input.pointer.interact_pos());
                            let inside = pointer
                                .map(|pos| area.response.rect.contains(pos))
                                .unwrap_or(false);
                            let clicked_outside =
                                !toggled && ctx.input(|input| input.pointer.any_click()) && !inside;
                            let escaped = ctx.input(|input| input.key_pressed(egui::Key::Escape));
                            if clicked_outside || escaped || open_remark || copy_ip || clear_chat
                            {
                                ui.memory_mut(|memory| memory.close_popup());
                            }
                        }
                        if widgets::icon_button(ui, widgets::Icon::Plus, tr("发送文件")).clicked() {
                            pick = true;
                        }
                    });
                });
            });
        // 分隔线贴着头部区域的底边画。不要用 allocate_exact_size：
        // 前面 Frame 和它之间会插入默认 8px 的 item_spacing，线就悬到
        // 消息区里去了（看起来像多出来一条莫名的线）。
        let header_rect = ui.max_rect();
        let line = egui::Rect::from_min_max(
            pos2(header_rect.left(), header_rect.bottom() - 1.0),
            pos2(header_rect.right(), header_rect.bottom()),
        );
        ui.painter().rect_filled(line, 0.0, theme::border());

        if open_remark {
            self.remark_draft =
                self.config.remark_for(peer).map(str::to_string).unwrap_or_default();
            self.remark_target = Some(peer.to_string());
            self.remark_focus = true;
        }
        if copy_ip {
            if let Some(device) = &device {
                ui.output_mut(|out| out.copied_text = device.ip.to_string());
            }
        }
        if clear_chat {
            if let Some(node) = &self.node {
                node.state.chats.clear(peer);
            }
            self.reload_thread(peer);
        }
        if pick {
            self.pick_files();
        }
    }

    fn message_stream(&mut self, ui: &mut egui::Ui, peer: &str, viewport: f32) {
        let messages = self.threads.get(peer).cloned().unwrap_or_default();
        let transfers = self.transfers.clone();
        let peer_name = self.display_name(peer);
        let request = self
            .incoming
            .iter()
            .find(|(_, request)| request.peer == peer)
            .map(|(id, request)| {
                (id.clone(), request.sender.clone(), request.files.clone(), request.total_bytes)
            });

        let mut action: Option<MsgAction> = None;
        let mut decision: Option<bool> = None;

        let scroll = ScrollArea::vertical()
            .auto_shrink([false, false])
            .max_height(viewport)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if self.bottom_pad > 0.0 {
                    ui.add_space(self.bottom_pad);
                }
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.label(
                        RichText::new(tr("局域网直连 · 消息不经过服务器"))
                            .size(10.5)
                            .color(theme::faint()),
                    );
                });
                ui.add_space(8.0);

                let mut last_day = String::new();
                let mut previous: Option<&ChatMessage> = None;
                for message in &messages {
                    let day = widgets::day_label(message.at);
                    if day != last_day {
                        day_separator(ui, &day);
                        last_day = day;
                        previous = None;
                    }
                    if message.kind == MessageKind::System {
                        system_line(ui, &message.text);
                        previous = Some(message);
                        continue;
                    }
                    // 同一侧连续发言（且间隔不长）时，合并成一组：只画一次头像、间距收紧。
                    let grouped = match previous {
                        Some(prev) => {
                            prev.outgoing == message.outgoing
                                && message.at.saturating_sub(prev.at) < GROUP_GAP_MS
                        }
                        None => false,
                    };
                    ui.add_space(if grouped { 5.0 } else { 13.0 });
                    let transfer =
                        message.session_id.as_ref().and_then(|id| transfers.get(id)).cloned();
                    if let Some(found) = message_bubble(
                        ui,
                        message,
                        transfer.as_ref(),
                        &peer_name,
                        &self.emojis,
                        !grouped,
                    ) {
                        action = Some(found);
                    }
                    previous = Some(message);
                }

                if let Some((_, sender, files, total)) = &request {
                    ui.add_space(6.0);
                    if let Some(value) = incoming_card(ui, sender, files, *total) {
                        decision = Some(value);
                    }
                    ui.add_space(6.0);
                }
                ui.add_space(8.0);
            });

        // 内容不足一屏时贴底显示，和聊天软件一致。
        // content_size 含已应用的 padding，减掉后才是真实内容高度。
        let content_true = scroll.content_size.y - self.bottom_pad;
        self.bottom_pad = (scroll.inner_rect.height() - content_true - 2.0).max(0.0);

        if let (Some(value), Some((session_id, _, _, _))) = (decision, request) {
            self.respond_incoming(&session_id, value);
        }
        match action {
            Some(MsgAction::Reveal(path)) => reveal_path(&path),
            Some(MsgAction::Retry(peer, text)) => {
                self.active = Some(peer);
                self.send_text_value(text);
            }
            Some(MsgAction::Cancel(session_id)) => {
                if let Some(node) = &self.node {
                    node.receiver.cancel_session(&session_id);
                }
                if let Some(sender) = &self.cancel_tx {
                    let _ = sender.send(true);
                }
            }
            Some(MsgAction::Copy(text)) => {
                ui.output_mut(|out| out.copied_text = text);
                self.toast(tr("已复制"), false);
            }
            None => {}
        }
    }

    fn composer_area(&mut self, ui: &mut egui::Ui, peer: &str) {
        let online = self.is_online(peer);
        let mut pick = false;
        // 聚焦时描边变强调色：这是聊天软件里最基本的「我在输入」反馈。
        let stroke = if self.composer_focused {
            Stroke::new(1.4_f32, theme::accent())
        } else {
            Stroke::new(1.0_f32, theme::border())
        };
        egui::Frame::none()
            .fill(theme::field())
            .rounding(egui::Rounding::same(theme::RADIUS_LG))
            .stroke(stroke)
            .inner_margin(egui::Margin::symmetric(14.0, 12.0))
            .show(ui, |ui| {
                // 三行起步：一行太窄，写两句话就得上下滚，是之前最别扭的一点。
                let text_edit = egui::TextEdit::multiline(&mut self.composer)
                    .hint_text(
                        RichText::new(if online {
                            tr("发消息…（Enter 发送，Shift+Enter 换行）")
                        } else {
                            tr("对方已离线，消息会立即尝试投递")
                        })
                        .size(13.0)
                        .color(theme::faint()),
                    )
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .frame(false)
                    .margin(egui::Margin::symmetric(0.0, 2.0))
                    .font(FontId::proportional(14.5));
                let response = ui.add(text_edit);
                self.composer_focused = response.has_focus();
                let enter =
                    ui.input(|input| input.key_pressed(egui::Key::Enter) && !input.modifiers.shift);
                if enter && response.has_focus() {
                    self.composer = self.composer.trim_end_matches('\n').to_string();
                    self.send_current_text();
                    response.request_focus();
                }

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let emoji = widgets::icon_button(ui, widgets::Icon::Smile, tr("表情"));
                    // 本帧按钮的屏幕位置，浮层按它定位（下一帧也用它判断「点在外面」）。
                    self.emoji_anchor = emoji.rect;
                    if emoji.clicked() {
                        self.emoji_open = !self.emoji_open;
                    }
                    if widgets::icon_button(ui, widgets::Icon::Plus, tr("发送文件")).clicked() {
                        pick = true;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let ready = !self.composer.trim().is_empty();
                        if ui.add(widgets::primary_button(tr("发送"), ready)).clicked() && ready {
                            self.send_current_text();
                        }
                        if !online {
                            ui.label(RichText::new(tr("离线")).size(10.5).color(theme::warn()));
                        } else {
                            ui.label(
                                RichText::new(tf(tr("{} 字"), &[&self.composer.chars().count()]))
                                    .size(10.5)
                                    .color(theme::faint()),
                            );
                        }
                    });
                });
            });
        if pick {
            self.pick_files();
        }
    }

    /// 表情浮层：从表情按钮向上浮出，盖在消息区之上。
    ///
    /// 之前是把面板塞进输入框，展开时消息流会被压掉半屏、关掉又要重新量高度，
    /// 整体像界面在跳。改成浮层后它完全不影响布局，点外面 / ESC / 再点笑脸即可收起。
    fn emoji_popup(&mut self, ctx: &egui::Context) {
        if !self.emoji_open {
            return;
        }
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.emoji_open = false;
            return;
        }

        const COLS: usize = 9;
        const CELL: f32 = 38.0;
        const GAP: f32 = 4.0;
        const PAD: f32 = 14.0;
        const HEADER: f32 = 32.0;

        let rows = self.emojis.len().div_ceil(COLS);
        let grid_w = COLS as f32 * CELL + (COLS as f32 - 1.0) * GAP;
        let grid_h = rows as f32 * CELL + rows.saturating_sub(1) as f32 * GAP;
        let width = grid_w + PAD * 2.0;
        let height = PAD * 2.0 + HEADER + grid_h;

        let screen = ctx.screen_rect();
        let anchor = if self.emoji_anchor.width() > 0.0 {
            self.emoji_anchor
        } else {
            Rect::from_min_size(pos2(screen.center().x, screen.bottom() - 120.0), Vec2::ZERO)
        };
        // 贴着输入卡片的上沿弹出、左缘与卡片对齐：既不压住输入框，
        // 也不会像之前那样往左溢到会话列表上。
        let base = if self.composer_rect.width() > 0.0 {
            self.composer_rect
        } else {
            anchor
        };
        let mut pos = pos2(base.left(), base.top() - 10.0 - height);
        if pos.y < screen.top() + 8.0 {
            pos.y = anchor.bottom() + 8.0;
        }
        pos.y = pos.y.min((screen.bottom() - height - 8.0).max(screen.top() + 8.0));
        pos.x = pos
            .x
            .min((screen.right() - width - 8.0).max(screen.left() + 8.0))
            .max(screen.left() + 8.0);

        let mut picked: Option<&'static str> = None;
        let mut close = false;
        let area = egui::Area::new(egui::Id::new("emoji-popup"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                egui::Frame::none()
                    .fill(theme::panel())
                    .rounding(egui::Rounding::same(theme::RADIUS_LG))
                    .stroke(Stroke::new(1.0_f32, theme::border()))
                    .shadow(egui::epaint::Shadow {
                        offset: Vec2::new(0.0, 14.0),
                        blur: 36.0,
                        spread: 0.0,
                        color: Color32::from_black_alpha(if theme::palette().is_light {
                            40
                        } else {
                            120
                        }),
                    })
                    .inner_margin(egui::Margin::same(PAD))
                    .show(ui, |ui| {
                        ui.set_width(grid_w);
                        ui.allocate_ui_with_layout(
                            Vec2::new(grid_w, HEADER),
                            Layout::left_to_right(Align::Center),
                            |ui| {
                                ui.label(
                                    RichText::new(tr("常用表情")).size(12.0).color(theme::muted()),
                                );
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                RichText::new(tr("收起"))
                                                    .size(11.0)
                                                    .color(theme::muted()),
                                            )
                                            .fill(Color32::TRANSPARENT)
                                            .stroke(Stroke::NONE)
                                            .rounding(egui::Rounding::same(theme::RADIUS_SM))
                                            .min_size(Vec2::new(36.0, 20.0)),
                                        )
                                        .clicked()
                                    {
                                        close = true;
                                    }
                                });
                            },
                        );
                        egui::Grid::new("emoji-popup-grid")
                            .spacing(Vec2::new(GAP, GAP))
                            .show(ui, |ui| {
                                for index in 0..self.emojis.len() {
                                    let Some(item) = self.emojis.get(index) else { continue };
                                    let (rect, response) =
                                        ui.allocate_exact_size(Vec2::splat(CELL), Sense::click());
                                    if response.hovered() {
                                        ui.painter().rect_filled(
                                            rect,
                                            egui::Rounding::same(theme::RADIUS_MD),
                                            theme::card_hover(),
                                        );
                                    }
                                    emoji::paint(
                                        ui.painter(),
                                        rect,
                                        &item.texture,
                                        CELL - 12.0,
                                    );
                                    if response.clicked() {
                                        picked = Some(item.ch);
                                    }
                                    response.on_hover_text(item.ch);
                                    if (index + 1) % COLS == 0 {
                                        ui.end_row();
                                    }
                                }
                            });
                    });
            });

        if let Some(ch) = picked {
            self.composer.push_str(ch);
        }
        if close {
            self.emoji_open = false;
            return;
        }
        // 点浮层和表情按钮之外的地方就收起（聊天区、发送按钮、别处都算）。
        let rect = area.response.rect;
        let outside = ctx.input(|input| {
            input.pointer.any_click()
                && input
                    .pointer
                    .interact_pos()
                    .is_some_and(|pos| !rect.contains(pos) && !anchor.contains(pos))
        });
        if outside {
            self.emoji_open = false;
        }
    }

    fn empty_state(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        // 内容实测约 72(标记) + 18 + 22 + 8 + 17 ≈ 137px，按视口高度做光学居中：
        // 比几何中心略高一点更符合视觉重心。
        let content_h = 137.0;
        let top = ((rect.height() - content_h) * 0.46).max(24.0);
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect), |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(top);
                let (badge, _) = ui.allocate_exact_size(Vec2::splat(72.0), Sense::hover());
                ui.painter().circle_filled(badge.center(), 36.0, theme::accent_dim());
                widgets::paint_brand(
                    ui.painter(),
                    Rect::from_center_size(badge.center(), Vec2::splat(34.0)),
                    theme::accent(),
                );
                ui.add_space(18.0);
                ui.label(
                    RichText::new(tr("选择左侧设备，开始聊天或发送文件"))
                        .size(16.0)
                        .color(theme::text()),
                );
                ui.add_space(8.0);
                ui.label(
                    RichText::new(tr("同一 Wi-Fi 下会自动发现，文件点对点直传，不经过服务器"))
                        .size(12.0)
                        .color(theme::muted()),
                );
            });
        });
    }

    // ------------------------------------------------------------ 弹窗

    fn windows(&mut self, ctx: &egui::Context) {
        // 表情浮层先画：弹窗打开时应该盖住它。
        self.emoji_popup(ctx);
        self.settings_modal(ctx);
        self.remark_window(ctx);
        self.connect_window(ctx);
        self.toast_overlay(ctx);
        self.window_frame(ctx);
    }

    /// 窗口外框。
    ///
    /// 无边框窗口在浅色主题下和旁边的白色窗口糊成一片，找不到边界；
    /// 这里沿窗口最外沿补一圈 1px 描边（浅色用中性灰，深色用半透明白），
    /// 最大化时贴边不需要，直接跳过。
    fn window_frame(&self, ctx: &egui::Context) {
        if ctx.input(|input| input.viewport().maximized).unwrap_or(false) {
            return;
        }
        let rect = ctx.screen_rect();
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("window-frame"),
        ));
        let palette = theme::palette();
        // 1px 描边：浅色用中性灰而不是深灰，够看见边界又不至于像贴了一圈黑框；
        // 深色用半透明白。圆角跟弹窗同一档，Windows 11 会按同样的圆角把窗口裁掉。
        let outer = if palette.is_light {
            Color32::from_rgb(203, 209, 219)
        } else {
            Color32::from_rgba_unmultiplied(255, 255, 255, 34)
        };
        let rounding = egui::Rounding::same(theme::RADIUS_LG);
        painter.rect_stroke(rect.shrink(0.5), rounding, Stroke::new(1.0_f32, outer));
        // 内侧再压一道极淡的暗线，让边看起来有厚度而不是一根生硬的线。
        painter.rect_stroke(
            rect.shrink(1.5),
            egui::Rounding::same(theme::RADIUS_LG - 1.0),
            Stroke::new(1.0_f32, Color32::from_black_alpha(if palette.is_light { 6 } else { 28 })),
        );
    }

    // ------------------------------------------------------------ 设置弹窗

    /// 设置弹窗：左侧导航 + 右侧内容 + 底部按钮的自绘模态框。
    ///
    /// 不用 `egui::Window`：它的标题栏、内边距、按钮样式都跟着默认 Style 走，
    /// 和自绘的聊天界面放在一起会像两个程序。这里整块自己画，才能做到
    /// 「遮罩 + 卡片 + 分组」的观感，也让主题切换卡能直接预览配色。
    fn settings_modal(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.cancel_settings(ctx);
            return;
        }

        let screen = ctx.screen_rect();
        let light = theme::palette().is_light;

        // 遮罩：吃掉点击，别让用户在弹窗上点穿到后面的会话列表。
        egui::Area::new(egui::Id::new("settings-scrim"))
            .order(egui::Order::Middle)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                let (rect, _) = ui.allocate_exact_size(screen.size(), Sense::click_and_drag());
                ui.painter().rect_filled(
                    rect,
                    0.0,
                    Color32::from_black_alpha(if light { 88 } else { 152 }),
                );
            });

        let size = Vec2::new(
            SETTINGS_SIZE.x.min(screen.width() - 48.0),
            SETTINGS_SIZE.y.min(screen.height() - 48.0),
        );
        let rect = Rect::from_center_size(screen.center(), size);
        let origin = rect.min;

        let header_h = 56.0;
        let footer_h = 66.0;
        let body_top = origin.y + header_h;
        let body_bottom = origin.y + size.y - footer_h;

        let mut save = false;
        let mut close = false;
        let mut quit = false;

        egui::Area::new(egui::Id::new("settings-modal"))
            .order(egui::Order::Foreground)
            .fixed_pos(origin)
            .show(ctx, |ui| {
                egui::Frame::none()
                    .fill(theme::panel())
                    .rounding(egui::Rounding::same(theme::RADIUS_LG))
                    .stroke(Stroke::new(1.0_f32, theme::border()))
                    .shadow(egui::epaint::Shadow {
                        offset: Vec2::new(0.0, 22.0),
                        blur: 60.0,
                        spread: 0.0,
                        color: Color32::from_black_alpha(if light { 44 } else { 132 }),
                    })
                    .show(ui, |ui| {
                        ui.set_min_size(size);
                        ui.set_max_width(size.x);

                        // ---- 标题栏 ----
                        ui.painter().text(
                            pos2(origin.x + 22.0, origin.y + header_h * 0.5),
                            Align2::LEFT_CENTER,
                            tr("设置"),
                            FontId::proportional(16.0),
                            theme::text(),
                        );
                        let close_rect = Rect::from_center_size(
                            pos2(origin.x + size.x - 36.0, origin.y + header_h * 0.5),
                            Vec2::splat(28.0),
                        );
                        let close_resp =
                            ui.interact(close_rect, ui.id().with("settings-close"), Sense::click());
                        if close_resp.hovered() {
                            ui.painter().rect_filled(
                                close_rect,
                                egui::Rounding::same(theme::RADIUS_MD),
                                theme::card_hover(),
                            );
                        }
                        widgets::paint_window_glyph(
                            ui.painter(),
                            Rect::from_center_size(close_rect.center(), Vec2::splat(10.0)),
                            widgets::WindowGlyph::Close,
                            theme::muted(),
                        );
                        if close_resp.clicked() {
                            close = true;
                        }
                        for y in [origin.y + header_h, origin.y + size.y - footer_h] {
                            ui.painter().rect_filled(
                                Rect::from_min_size(
                                    pos2(origin.x + 1.0, y),
                                    Vec2::new(size.x - 2.0, 1.0),
                                ),
                                0.0,
                                theme::border_soft(),
                            );
                        }

                        // ---- 左侧导航 ----
                        let rail = Rect::from_min_max(
                            pos2(origin.x + 14.0, body_top + 14.0),
                            pos2(origin.x + SETTINGS_RAIL, body_bottom - 10.0),
                        );
                        ui.painter().rect_filled(
                            Rect::from_min_size(
                                pos2(origin.x + SETTINGS_RAIL, body_top + 12.0),
                                Vec2::new(1.0, body_bottom - body_top - 24.0),
                            ),
                            0.0,
                            theme::border_soft(),
                        );
                        let mut tab = self.settings_tab;
                        for (index, item) in SettingsTab::ALL.iter().enumerate() {
                            let item_rect = Rect::from_min_size(
                                pos2(rail.min.x, rail.min.y + index as f32 * 46.0),
                                Vec2::new(rail.width(), 40.0),
                            );
                            let response =
                                ui.interact(item_rect, ui.id().with(("tab", index)), Sense::click());
                            let active = *item == self.settings_tab;
                            if active {
                                ui.painter().rect_filled(
                                    item_rect,
                                    egui::Rounding::same(theme::RADIUS_LG),
                                    theme::accent_dim(),
                                );
                            } else if response.hovered() {
                                ui.painter().rect_filled(
                                    item_rect,
                                    egui::Rounding::same(theme::RADIUS_LG),
                                    theme::card_hover(),
                                );
                            }
                            ui.painter().text(
                                pos2(item_rect.min.x + 14.0, item_rect.center().y),
                                Align2::LEFT_CENTER,
                                item.label(),
                                FontId::proportional(13.5),
                                if active { theme::accent() } else { theme::muted() },
                            );
                            if response.clicked() {
                                tab = *item;
                            }
                            response.on_hover_text(item.hint());
                        }
                        self.settings_tab = tab;

                        // ---- 右侧内容 ----
                        let pane = Rect::from_min_max(
                            pos2(origin.x + SETTINGS_RAIL + 24.0, body_top + 20.0),
                            pos2(origin.x + size.x - 24.0, body_bottom - 8.0),
                        );
                        ui.painter().text(
                            pos2(pane.min.x, pane.min.y),
                            Align2::LEFT_TOP,
                            self.settings_tab.label(),
                            FontId::proportional(15.0),
                            theme::text(),
                        );
                        ui.painter().text(
                            pos2(pane.min.x, pane.min.y + 24.0),
                            Align2::LEFT_TOP,
                            self.settings_tab.hint(),
                            FontId::proportional(11.5),
                            theme::muted(),
                        );
                        let content = Rect::from_min_max(
                            pos2(pane.min.x, pane.min.y + 54.0),
                            pane.max,
                        );
                        let tab = self.settings_tab;
                        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(content), |ui| {
                            self.settings_pane(ui, tab, ctx);
                        });

                        // ---- 底部按钮 ----
                        let footer = Rect::from_min_max(
                            pos2(origin.x, body_bottom),
                            pos2(origin.x + size.x, origin.y + size.y),
                        );
                        // 左侧放「退出」：关窗默认只是收进后台，总得给个真正退出的出口，
                        // 否则用户只能去任务管理器结束进程。
                        ui.allocate_new_ui(
                            egui::UiBuilder::new().max_rect(Rect::from_min_max(
                                pos2(origin.x + 22.0, footer.min.y),
                                pos2(origin.x + 240.0, footer.max.y),
                            )),
                            |ui| {
                                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                    if ui
                                        .add_sized([48.0, 26.0], widgets::ghost_button(tr("退出")))
                                        .clicked()
                                    {
                                        quit = true;
                                    }
                                    ui.add_space(10.0);
                                    ui.label(
                                        RichText::new(tf(
                                            tr("版本 {}"),
                                            &[&env!("CARGO_PKG_VERSION")],
                                        ))
                                        .size(10.5)
                                        .color(theme::faint()),
                                    );
                                });
                            },
                        );
                        ui.allocate_new_ui(
                            egui::UiBuilder::new()
                                .max_rect(footer.shrink2(Vec2::new(22.0, 17.0))),
                            |ui| {
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if ui.add(widgets::primary_button(tr("保存"), true)).clicked() {
                                        save = true;
                                    }
                                    ui.add_space(6.0);
                                    if ui.add(widgets::ghost_button(tr("取消"))).clicked() {
                                        close = true;
                                    }
                                });
                            },
                        );
                    });
            });

        if quit {
            // 置位之后再关，避免被「关闭窗口转后台」拦下来。
            self.quitting = true;
            ctx.send_viewport_cmd(ViewportCommand::Close);
            return;
        }
        if save {
            self.save_settings(ctx);
            return;
        }
        if close {
            self.cancel_settings(ctx);
        }
    }

    fn settings_pane(&mut self, ui: &mut egui::Ui, tab: SettingsTab, ctx: &egui::Context) {
        match tab {
            SettingsTab::Appearance => {
                const GAP: f32 = 14.0;
                let card_w = ((ui.available_width() - GAP * 2.0) / 3.0).floor().max(80.0);
                let mut chosen = None;
                ui.spacing_mut().item_spacing.x = GAP;
                ui.horizontal(|ui| {
                    for pref in
                        [ThemePreference::System, ThemePreference::Light, ThemePreference::Dark]
                    {
                        let selected = theme::preference(&self.draft.theme) == pref;
                        if settings_theme_card(ui, pref, card_w, selected) {
                            chosen = Some(pref);
                        }
                    }
                });
                if let Some(pref) = chosen {
                    self.draft.theme = theme::preference_key(pref).to_string();
                    // 立即预览，不用先点保存
                    ctx.set_theme(pref);
                    theme::sync(ctx.theme());
                    ctx.request_repaint();
                }
                ui.add_space(18.0);
                settings_hint(
                    ui,
                    tr("卡片就是主题的实际配色。选「跟随系统」时，切换 Windows 深浅色会立刻跟着变。"),
                );

                ui.add_space(22.0);
                settings_label(ui, tr("界面语言"));
                ui.add_space(5.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    // 「跟随系统」排第一位：大多数人不该为这个设置费脑子。
                    let options: [(&str, &str); 3] = [
                        ("system", tr("跟随系统")),
                        (i18n::Lang::ZhCn.key(), i18n::Lang::ZhCn.native_name()),
                        (i18n::Lang::En.key(), i18n::Lang::En.native_name()),
                    ];
                    for (value, label) in options {
                        let selected = self.draft.language == value;
                        if settings_choice(ui, label, selected) && !selected {
                            self.draft.language = value.to_string();
                            // 语言只改显示，不涉及后台，直接预览，不必等保存。
                            i18n::set(i18n::resolve(&self.draft.language));
                            ctx.request_repaint();
                        }
                    }
                });
                ui.add_space(6.0);
                settings_hint(ui, tr("选择界面显示的语言，切换后立即生效。"));
            }
            SettingsTab::Identity => {
                settings_label(ui, tr("我的用户名"));
                ui.add_space(5.0);
                ui.horizontal(|ui| {
                    let button_w = 68.0;
                    let field_w = (ui.available_width() - button_w * 2.0 - 16.0).max(140.0);
                    ui.add_sized(
                        [field_w, 33.0],
                        egui::TextEdit::singleline(&mut self.draft.alias)
                            .margin(Vec2::new(9.0, 7.0))
                            .font(FontId::proportional(13.0))
                            .hint_text(tr("别人在会话列表里看到的名字")),
                    );
                    if ui
                        .add_sized([button_w, 33.0], widgets::ghost_button(tr("随机一个")))
                        .clicked()
                    {
                        self.draft.alias =
                            feigecraft_core::util::random_alias_lang(crate::i18n::current().key());
                    }
                    if ui
                        .add_sized([button_w, 33.0], widgets::ghost_button(tr("电脑名")))
                        .clicked()
                    {
                        self.draft.alias = feigecraft_core::util::host_alias();
                    }
                });
                ui.add_space(6.0);
                settings_hint(
                    ui,
                    tr("默认名是启动时随机生成的，避免和别人撞名；保存后立刻广播，不用重启。"),
                );
                ui.add_space(20.0);
                settings_label(ui, tr("设备类型"));
                ui.add_space(5.0);
                egui::ComboBox::from_id_salt("device-type")
                    .selected_text(i18n::device_label(self.draft.device_type))
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        for kind in DeviceType::ALL {
                            ui.selectable_value(&mut self.draft.device_type, kind, i18n::device_label(kind));
                        }
                    });
                ui.add_space(8.0);
                settings_hint(ui, tr("对方看到的图标会跟着这个类型变化。"));
            }
            SettingsTab::Transfer => {
                settings_label(ui, tr("收到的文件保存到"));
                ui.add_space(5.0);
                let mut dir = self.draft.download_dir.display().to_string();
                let mut picked_dir = None;
                ui.horizontal(|ui| {
                    let button_w = 82.0;
                    let field_w = (ui.available_width() - button_w - 8.0).max(120.0);
                    if ui
                        .add_sized(
                            [field_w, 32.0],
                            egui::TextEdit::singleline(&mut dir)
                                .margin(Vec2::new(9.0, 7.0))
                                .font(FontId::proportional(13.0)),
                        )
                        .changed()
                    {
                        self.draft.download_dir = PathBuf::from(dir.trim());
                    }
                    if ui
                        .add_sized([button_w, 32.0], widgets::ghost_button(tr("选择…")))
                        .clicked()
                    {
                        picked_dir = rfd::FileDialog::new().pick_folder();
                    }
                });
                if let Some(folder) = picked_dir {
                    self.draft.download_dir = folder;
                }
                ui.add_space(20.0);
                settings_label(ui, tr("接收方式"));
                ui.add_space(4.0);
                settings_toggle(
                    ui,
                    &mut self.draft.auto_accept,
                    tr("自动接受文件"),
                    tr("关闭时，每次传输都要先点一次「接受」"),
                );
                ui.add_space(20.0);
                settings_label(ui, tr("关闭窗口时"));
                ui.add_space(4.0);
                settings_toggle(
                    ui,
                    &mut self.draft.background_on_close,
                    tr("留在后台继续接收"),
                    tr("关闭窗口不退出程序，仍可收发文件；再双击程序即可唤回窗口"),
                );
            }
            SettingsTab::About => {
                let info = self.node.as_ref().map(|node| node.device_info());
                settings_row(ui, tr("版本"), env!("CARGO_PKG_VERSION"));
                settings_row(
                    ui,
                    tr("实例"),
                    &if self.instance == 0 {
                        tr("主实例").to_string()
                    } else {
                        tf(tr("实例 {}"), &[&(self.instance + 1)])
                    },
                );
                settings_row(
                    ui,
                    tr("监听端口"),
                    &info.as_ref().map(|i| i.port.to_string()).unwrap_or_default(),
                );
                ui.horizontal(|ui| {
                    ui.label(RichText::new(tr("配置目录")).size(12.0).color(theme::muted()));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(Config::config_dir().display().to_string())
                                    .size(11.0)
                                    .color(theme::text()),
                            )
                            .truncate(),
                        );
                    });
                });
                ui.add_space(9.0);
                ui.add_space(4.0);
                widgets::divider(ui, 6.0);
                ui.horizontal(|ui| {
                    settings_label(ui, tr("本机指纹"));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new(tr("复制")).size(11.0).color(theme::muted()),
                                )
                                .fill(theme::card())
                                .stroke(Stroke::new(1.0_f32, theme::border()))
                                .rounding(egui::Rounding::same(theme::RADIUS_SM))
                                .min_size(Vec2::new(44.0, 22.0)),
                            )
                            .clicked()
                        {
                            if let Some(info) = &info {
                                ui.output_mut(|out| out.copied_text = info.fingerprint.clone());
                            }
                        }
                    });
                });
                ui.add_space(2.0);
                // 指纹固定 64 个十六进制字符，按 32 字符折两行才不会顶开按钮。
                if let Some(info) = &info {
                    let fingerprint = info.fingerprint.as_str();
                    let split = fingerprint.len().min(32);
                    for chunk in [&fingerprint[..split], &fingerprint[split..]] {
                        if chunk.is_empty() {
                            continue;
                        }
                        ui.label(
                            RichText::new(chunk).size(10.5).monospace().color(theme::faint()),
                        );
                    }
                }
                ui.add_space(8.0);
                settings_hint(ui, tr("对方首次连接时用指纹校验身份，指纹不变即可放心。"));
            }
        }
    }

    /// 放弃改动：主题回到已保存的偏好。
    fn cancel_settings(&mut self, ctx: &egui::Context) {
        self.settings_open = false;
        // 关掉弹窗时把预览过的主题 / 语言退回已保存的设置。
        self.draft = self.config.clone();
        i18n::set(i18n::resolve(&self.config.language));
        ctx.set_theme(theme::preference(&self.config.theme));
        theme::sync(ctx.theme());
    }

    /// 修改备注：和设置弹窗同一套自绘模态，不再用默认的 `egui::Window`。
    fn remark_window(&mut self, ctx: &egui::Context) {
        let Some(peer) = self.remark_target.clone() else { return };
        let name = self.display_name(&peer);
        let alias = self
            .device_by_peer(&peer)
            .map(|device| device.info.alias)
            .or_else(|| self.node.as_ref().and_then(|node| node.state.chats.alias(&peer)))
            .unwrap_or_default();
        let has_remark = self
            .config
            .remarks
            .get(&peer)
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false);
        let online = self.is_online(&peer);

        enum Act {
            Save,
            Clear,
            Cancel,
        }

        let (action, dismissed) = modal_card(
            ctx,
            "remark-modal",
            Vec2::new(430.0, 286.0),
            tr("修改备注"),
            |ui| {
                let mut return_enter = false;
                // ---- 对方身份：头像 + 现用名 + 原名 ----
                ui.horizontal(|ui| {
                    widgets::avatar(ui, 38.0, &name, &peer, online);
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new(if alias.is_empty() { &name } else { &alias })
                                .size(13.5)
                                .strong()
                                .color(theme::text()),
                        );
                        ui.label(
                            RichText::new(if online { tr("局域网在线") } else { tr("当前离线") })
                                .size(11.0)
                                .color(if online { theme::ok() } else { theme::faint() }),
                        );
                    });
                });
                ui.add_space(16.0);

                settings_label(ui, tr("备注名"));
                ui.add_space(6.0);
                // 自绘输入框：默认 TextEdit 的边框在深色卡片上偏灰，看起来像没画完。
                let focused = ui.memory(|memory| memory.has_focus(egui::Id::new("remark-input")));
                egui::Frame::none()
                    .fill(theme::field())
                    .rounding(egui::Rounding::same(theme::RADIUS_MD))
                    .stroke(Stroke::new(
                        if focused { 1.4_f32 } else { 1.0_f32 },
                        if focused { theme::accent() } else { theme::border() },
                    ))
                    .inner_margin(egui::Margin::symmetric(11.0, 9.0))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let edit = ui.add(
                            egui::TextEdit::singleline(&mut self.remark_draft)
                                .id(egui::Id::new("remark-input"))
                                .desired_width(f32::INFINITY)
                                .frame(false)
                                .font(FontId::proportional(13.5))
                                .hint_text(
                                    RichText::new(tf(tr("给 {} 起个名字"), &[&name]))
                                        .size(13.0)
                                        .color(theme::faint()),
                                ),
                        );
                        if self.remark_focus {
                            edit.request_focus();
                            self.remark_focus = false;
                        }
                        if edit.lost_focus()
                            && ui.input(|input| input.key_pressed(egui::Key::Enter))
                        {
                            // 回车保存，和聊天框里的习惯一致。
                            return_enter = true;
                        }
                    });
                ui.add_space(8.0);
                settings_hint(ui, tr("只在本机生效，对方看到的仍然是他自己的名字。"));

                ui.add_space(18.0);
                let mut act = None;
                ui.horizontal(|ui| {
                    if has_remark && ui.add(widgets::ghost_button(tr("清除备注"))).clicked() {
                        act = Some(Act::Clear);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add(widgets::primary_button(tr("保存"), true)).clicked() {
                            act = Some(Act::Save);
                        }
                        ui.add_space(6.0);
                        if ui.add(widgets::ghost_button(tr("取消"))).clicked() {
                            act = Some(Act::Cancel);
                        }
                    });
                });
                if return_enter {
                    act = Some(Act::Save);
                }
                act
            },
        );

        if dismissed || matches!(action, Some(Some(Act::Cancel))) {
            self.remark_target = None;
            return;
        }
        match action.flatten() {
            Some(Act::Save) => {
                self.save_remark();
                self.remark_target = None;
            }
            Some(Act::Clear) => {
                self.remark_draft.clear();
                self.save_remark();
                self.remark_target = None;
            }
            _ => {}
        }
    }

    /// 手动连接：同样换成自绘模态，风格和设置 / 备注统一。
    fn connect_window(&mut self, ctx: &egui::Context) {
        if !self.connect_open {
            return;
        }
        let (action, dismissed) =
            modal_card(ctx, "connect-modal", Vec2::new(430.0, 250.0), tr("手动连接"), |ui| {
                settings_label(ui, tr("对方 IP"));
                ui.add_space(6.0);
                let focused = ui.memory(|memory| memory.has_focus(egui::Id::new("connect-input")));
                egui::Frame::none()
                    .fill(theme::field())
                    .rounding(egui::Rounding::same(theme::RADIUS_MD))
                    .stroke(Stroke::new(
                        if focused { 1.4_f32 } else { 1.0_f32 },
                        if focused { theme::accent() } else { theme::border() },
                    ))
                    .inner_margin(egui::Margin::symmetric(11.0, 9.0))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let edit = ui.add(
                            egui::TextEdit::singleline(&mut self.manual_ip)
                                .id(egui::Id::new("connect-input"))
                                .desired_width(f32::INFINITY)
                                .frame(false)
                                .font(FontId::proportional(13.5))
                                .hint_text(
                                    RichText::new(tr("192.168.1.23 或 192.168.2.0/24"))
                                        .size(13.0)
                                        .color(theme::faint()),
                                ),
                        );
                        if self.connect_focus {
                            edit.request_focus();
                            self.connect_focus = false;
                        }
                    });
                ui.add_space(8.0);
                settings_hint(
                    ui,
                    tr("跨网段或交换机拦截组播时用这里：填 IP 直接连，填 192.168.2.0/24 扫描整段。"),
                );
                ui.add_space(18.0);
                // 用 Option 区分「没点」和「点了取消」，否则空结果会被误判成取消。
                let mut act = None;
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add(widgets::primary_button(tr("连接"), true)).clicked() {
                            act = Some(true);
                        }
                        ui.add_space(6.0);
                        if ui.add(widgets::ghost_button(tr("取消"))).clicked() {
                            act = Some(false);
                        }
                    });
                });
                act
            });
        if dismissed || matches!(action, Some(Some(false))) {
            self.connect_open = false;
            return;
        }
        if matches!(action, Some(Some(true))) {
            self.connect_open = false;
            self.connect_manual();
        }
    }

    fn toast_overlay(&mut self, ctx: &egui::Context) {
        self.toasts.retain(|toast| toast.at.elapsed() < Duration::from_secs(4));
        if self.toasts.is_empty() {
            return;
        }
        let screen = ctx.screen_rect();
        let mut offset = 16.0;
        let items: Vec<(String, bool)> =
            self.toasts.iter().rev().map(|toast| (toast.text.clone(), toast.error)).collect();
        for (index, (text, error)) in items.into_iter().enumerate() {
            let galley = ctx.fonts(|fonts| {
                fonts.layout_no_wrap(text.clone(), FontId::proportional(12.5), Color32::WHITE)
            });
            let size = galley.size() + Vec2::new(26.0, 16.0);
            let rect = egui::Rect::from_min_size(
                egui::pos2(screen.center().x - size.x * 0.5, screen.top() + offset),
                size,
            );
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Tooltip,
                egui::Id::new(("toast", index)),
            ));
            painter.rect_filled(
                rect,
                egui::Rounding::same(theme::RADIUS_MD),
                if error {
                    Color32::from_rgb(140, 58, 58)
                } else {
                    Color32::from_rgb(40, 48, 62)
                },
            );
            painter.galley(rect.min + Vec2::new(13.0, 8.0), galley, Color32::WHITE);
            offset += size.y + 8.0;
        }
        ctx.request_repaint_after(Duration::from_millis(200));
    }

    // ------------------------------------------------------------ 截图 / 演示

    fn handle_screenshot(&mut self, ctx: &egui::Context) {
        let Some(shot) = self.shot.as_mut() else { return };
        let captured = ctx.input(|input| {
            input.events.iter().find_map(|event| match event {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = captured {
            match crate::capture::save_bmp(&shot.path, &image) {
                Ok(()) => eprintln!("截图已保存：{}", shot.path.display()),
                Err(err) => eprintln!("截图保存失败：{err}"),
            }
            self.shot = None;
            self.quitting = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        shot.frames += 1;
        if shot.frames > 40 && !shot.requested {
            shot.requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
        }
        ctx.request_repaint();
    }

    /// 设计走查用假数据（`FEIGECRAFT_UI_DEMO=1`），只写入独立的演示配置目录。
    fn seed_demo(&mut self, mode: &str) {
        use feigecraft_core::model::ProtocolType;
        let make = |alias: &str, kind: DeviceType, ip: &str, fp: &str| {
            let mut info = DeviceInfo::new(alias, kind, fp.to_string(), 53317);
            info.protocol = ProtocolType::Https;
            DiscoveredDevice {
                info,
                ip: ip.parse().expect("demo ip"),
                last_seen: Instant::now(),
                via_multicast: true,
            }
        };
        let devices = vec![
            make(tr("小米 14 Pro"), DeviceType::Mobile, "192.168.1.23", "a1b2c3d4e5f60718"),
            make("MacBook Pro", DeviceType::Desktop, "192.168.1.31", "11aa22bb33cc44dd"),
            make(tr("家庭 NAS"), DeviceType::Server, "192.168.1.8", "99ff88ee77dd66cc"),
        ];
        self.devices = devices;
        // 截图走查时把「语言」也写进草稿，否则设置页会显示「跟随系统」被选中、
        // 界面却是英文，看着像 bug。
        if let Ok(lang) = std::env::var("FEIGECRAFT_LANG") {
            self.draft.language = lang.clone();
            self.config.language = lang;
        }
        let xiaomi = peer_key(&self.devices[0].info);
        let mac = peer_key(&self.devices[1].info);
        // 演示「原昵称（备注名）」的展示格式。
        self.config.remarks.insert(mac.clone(), tr("设计组的 Mac").to_string());

        if let Some(node) = &self.node {
            for device in &self.devices {
                node.state.chats.touch_alias(&peer_key(&device.info), &device.info.alias);
            }
            let now = feigecraft_core::chat::now_ms();
            let script = [
                (xiaomi.as_str(), false, tr("到家了？路由器好像又抽风"), 42),
                (xiaomi.as_str(), true, tr("刚到，Wi-Fi 连上了，我看看"), 40),
                (xiaomi.as_str(), false, tr("先把设计稿发我一份，客户下午要看"), 38),
                (xiaomi.as_str(), true, tr("好，这就传 👍"), 37),
                (xiaomi.as_str(), false, "🎉🎉", 36),
            ];
            for (peer, outgoing, text, minutes_ago) in script {
                let mut message = ChatMessage::text(peer, outgoing, text);
                message.at = now - minutes_ago * 60 * 1000;
                message.status = MessageStatus::Sent;
                node.state.chats.append(message);
            }
            let mut file = ChatMessage::file(&xiaomi, true, tr("设计稿_v3_final.zip"), 182_000_000);
            file.at = now - 36 * 60 * 1000;
            file.status = MessageStatus::Sent;
            node.state.chats.append(file);
            node.state.chats.append(ChatMessage::system(&xiaomi, tr("已发送 1 个文件 · 173.6 MB")));

            let mut hello = ChatMessage::text(&mac, false, tr("服务器那台机器我重启完了"));
            hello.at = now - 90 * 60 * 1000;
            hello.status = MessageStatus::Sent;
            node.state.chats.append(hello);
        }

        self.config.remarks.insert(xiaomi.clone(), tr("小王").to_string());
        if !mode.contains("incoming") && !mode.contains("settings") {
            // 让输入区看起来是"正在输入"的状态，文档截图更有说服力
            self.composer = tr("收到，我下午三点前把改好的发你").to_string();
        }
        self.select(&xiaomi);
        *self.unread.entry(mac).or_insert(0) = 2;

        if mode.contains("emoji") {
            self.emoji_open = true;
        }
        if mode.contains("menu") {
            self.menu_demo = true;
        }
        if mode.contains("settings") {
            self.settings_open = true;
            // 走查用：`settings-identity` / `settings-transfer` / `settings-about`
            if mode.contains("identity") {
                self.settings_tab = SettingsTab::Identity;
            } else if mode.contains("transfer") {
                self.settings_tab = SettingsTab::Transfer;
            } else if mode.contains("about") {
                self.settings_tab = SettingsTab::About;
            }
        }
        if mode.contains("remark") {
            self.remark_target = Some(xiaomi.clone());
            self.remark_draft = tr("小王").to_string();
        }
        if mode.contains("connect") {
            self.connect_open = true;
        }
        if mode.contains("incoming") {
            let files = vec![
                FileMetadata {
                    id: "f1".to_string(),
                    file_name: "IMG_20261104_103355.jpg".to_string(),
                    size: 4_800_000,
                    file_type: "image/jpeg".to_string(),
                    sha256: None,
                    preview: None,
                },
                FileMetadata {
                    id: "f2".to_string(),
                    file_name: tr("会议纪要.pdf").to_string(),
                    size: 820_000,
                    file_type: "application/pdf".to_string(),
                    sha256: None,
                    preview: None,
                },
                FileMetadata {
                    id: "f3".to_string(),
                    file_name: tr("录屏 2026-11-04.mp4").to_string(),
                    size: 128_000_000,
                    file_type: "video/mp4".to_string(),
                    sha256: None,
                    preview: None,
                },
            ];
            let total = files.iter().map(|file| file.size).sum();
            self.incoming.insert(
                "demo-session".to_string(),
                IncomingRequest {
                    peer: xiaomi.clone(),
                    sender: tr("小米 14 Pro").to_string(),
                    files,
                    total_bytes: total,
                },
            );
        }
        self.status = tr("已就绪 · 3 台设备在线").to_string();
    }
}

// ---------------------------------------------------------------- 渲染辅助

/// 下拉菜单里的一行：平时透明，hover 才出色块；危险操作用红字。
///
/// 不用 `ui.button`：默认 button 在菜单里会变成带描边的灰卡片（`card` 填充 +
/// `border` 描边），和自绘界面放一起像另一个程序的控件。
fn menu_item(ui: &mut egui::Ui, label: &str, danger: bool, width: f32) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 32.0), Sense::click());
    let palette = theme::palette();
    if response.hovered() {
        ui.painter().rect_filled(
            rect,
            egui::Rounding::same(theme::RADIUS_SM),
            palette.card_hover,
        );
    }
    ui.painter().text(
        rect.left_center() + Vec2::new(10.0, 0.0),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(13.0),
        if danger { palette.err } else { palette.text },
    );
    response.clicked()
}

/// 菜单里的分隔线：占 9px 高，线画在中间。
fn menu_separator(ui: &mut egui::Ui, width: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 9.0), Sense::hover());
    ui.painter().rect_filled(
        Rect::from_center_size(rect.center(), Vec2::new(width, 1.0)),
        0.0,
        theme::border_soft(),
    );
}

/// 设置弹窗里的小标题。
/// 自绘模态框的骨架：遮罩 + 居中卡片 + 标题 + 右上角关闭。
///
/// 返回 `(body 的返回值, 是否取消)`；点遮罩、按 ESC、点 × 都记为取消。
/// 卡片尺寸是固定的，`body` 负责内容，底部按钮也放在 `body` 里，
/// 这样每个弹窗只需要关心自己的字段，不用重复画外框。
fn modal_card<R>(
    ctx: &egui::Context,
    id: &'static str,
    size: Vec2,
    title: &str,
    body: impl FnOnce(&mut egui::Ui) -> R,
) -> (Option<R>, bool) {
    let screen = ctx.screen_rect();
    let light = theme::palette().is_light;
    let mut cancel = ctx.input(|input| input.key_pressed(egui::Key::Escape));

    egui::Area::new(egui::Id::new((id, "scrim")))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let (rect, response) =
                ui.allocate_exact_size(screen.size(), Sense::click_and_drag());
            ui.painter().rect_filled(
                rect,
                0.0,
                Color32::from_black_alpha(if light { 72 } else { 142 }),
            );
            if response.clicked() {
                cancel = true;
            }
        });

    let size = Vec2::new(
        size.x.min(screen.width() - 40.0),
        size.y.min(screen.height() - 40.0),
    );
    let rect = Rect::from_center_size(screen.center(), size);

    let mut result = None;
    egui::Area::new(egui::Id::new((id, "card")))
        .order(egui::Order::Foreground)
        .fixed_pos(rect.min)
        .show(ctx, |ui| {
            egui::Frame::none()
                .fill(theme::panel())
                .rounding(egui::Rounding::same(theme::RADIUS_LG))
                .stroke(Stroke::new(1.0_f32, theme::border()))
                .shadow(egui::epaint::Shadow {
                    offset: Vec2::new(0.0, 18.0),
                    blur: 48.0,
                    spread: 0.0,
                    color: Color32::from_black_alpha(if light { 38 } else { 128 }),
                })
                .inner_margin(egui::Margin::same(22.0))
                .show(ui, |ui| {
                    ui.set_min_size(Vec2::new(size.x - 44.0, size.y - 44.0));
                    ui.set_width(size.x - 44.0);

                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(title).size(16.0).strong().color(theme::text()),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let (close_rect, close) =
                                ui.allocate_exact_size(Vec2::splat(26.0), Sense::click());
                            if close.hovered() {
                                ui.painter().rect_filled(
                                    close_rect,
                                    egui::Rounding::same(theme::RADIUS_SM),
                                    theme::card_hover(),
                                );
                            }
                            widgets::paint_window_glyph(
                                ui.painter(),
                                Rect::from_center_size(close_rect.center(), Vec2::splat(10.0)),
                                widgets::WindowGlyph::Close,
                                theme::muted(),
                            );
                            if close.clicked() {
                                cancel = true;
                            }
                        });
                    });
                    ui.add_space(16.0);
                    result = Some(body(ui));
                });
        });

    (result, cancel)
}

fn settings_label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(11.5).color(theme::muted()));
}

/// 设置弹窗里的说明文字（比标签再淡一档）。
fn settings_hint(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(10.5).color(theme::faint()));
}

/// 设置弹窗里的「键 — 值」行。
fn settings_row(ui: &mut egui::Ui, key: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(key).size(12.0).color(theme::muted()));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(value).size(12.0).color(theme::text()));
        });
    });
    ui.add_space(9.0);
}

/// 主题预览卡：一张缩小的聊天窗口，用真实配色画，点一下即时换主题。
fn settings_theme_card(
    ui: &mut egui::Ui,
    pref: ThemePreference,
    width: f32,
    selected: bool,
) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 126.0), Sense::click());
    // 「跟随系统」用当前生效的配色做预览，否则会跟旁边那张长得一模一样。
    let p = match pref {
        ThemePreference::Light => theme::LIGHT,
        ThemePreference::Dark => theme::DARK,
        ThemePreference::System => theme::palette(),
    };
    let card = Rect::from_min_size(rect.min, Vec2::new(width, 94.0));
    let painter = ui.painter().clone();

    painter.rect_filled(card, egui::Rounding::same(theme::RADIUS_LG), p.panel);
    let inner = card.shrink(4.0);
    painter.rect_filled(inner, egui::Rounding::same(theme::RADIUS_SM), p.bg);

    // 标题条
    let strip = Rect::from_min_size(inner.min, Vec2::new(inner.width(), 14.0));
    painter.rect_filled(
        strip,
        egui::Rounding { nw: 7.0, ne: 7.0, sw: 0.0, se: 0.0 },
        p.titlebar,
    );
    // 左侧会话栏
    let side_w = (inner.width() * 0.26).max(16.0);
    let side = Rect::from_min_max(
        pos2(inner.min.x, strip.max.y),
        pos2(inner.min.x + side_w, inner.max.y),
    );
    painter.rect_filled(
        side,
        egui::Rounding { nw: 0.0, ne: 0.0, sw: 7.0, se: 0.0 },
        p.panel,
    );

    // 聊天区：对方一条、自己一条、底部输入框
    let chat_x = side.max.x + 8.0;
    let chat_w = (inner.max.x - 8.0 - chat_x).max(20.0);
    let incoming = Rect::from_min_size(
        pos2(chat_x, strip.max.y + 11.0),
        Vec2::new(chat_w * 0.62, 14.0),
    );
    painter.rect_filled(incoming, egui::Rounding::same(theme::RADIUS_SM), p.bubble_in);
    if p.is_light {
        painter.rect_stroke(
            incoming,
            egui::Rounding::same(theme::RADIUS_SM),
            Stroke::new(1.0_f32, p.border),
        );
    }
    let outgoing = Rect::from_min_size(
        pos2(inner.max.x - 8.0 - chat_w * 0.7, strip.max.y + 32.0),
        Vec2::new(chat_w * 0.7, 14.0),
    );
    painter.rect_filled(outgoing, egui::Rounding::same(theme::RADIUS_SM), p.bubble_out);
    let field = Rect::from_min_size(
        pos2(chat_x, inner.max.y - 14.0),
        Vec2::new(chat_w, 9.0),
    );
    painter.rect_filled(field, egui::Rounding::same(theme::RADIUS_SM), p.field);
    if p.is_light {
        painter.rect_stroke(field, egui::Rounding::same(theme::RADIUS_SM), Stroke::new(1.0_f32, p.border));
    }
    painter.rect_stroke(inner, egui::Rounding::same(theme::RADIUS_SM), Stroke::new(1.0_f32, p.border));

    let ring = if selected {
        Stroke::new(1.8_f32, theme::accent())
    } else if response.hovered() {
        Stroke::new(1.2_f32, theme::accent_dim())
    } else {
        Stroke::new(1.0_f32, theme::border())
    };
    painter.rect_stroke(card, egui::Rounding::same(theme::RADIUS_LG), ring);

    if selected {
        let center = pos2(card.max.x - 13.0, card.min.y + 13.0);
        painter.circle_filled(center, 8.5, theme::accent());
        widgets::paint_status(
            &painter,
            Rect::from_center_size(center, Vec2::splat(11.0)),
            MessageStatus::Sent,
            theme::on_accent(),
        );
    }
    painter.text(
        pos2(card.center().x, card.max.y + 16.0),
        Align2::CENTER_CENTER,
        theme::preference_label(pref),
        FontId::proportional(12.0),
        if selected { theme::accent() } else { theme::muted() },
    );
    response.clicked()
}

/// 一排单选按钮（语言这类少量互斥选项）。选中：强调色底 + 描边 + 强调色字；
/// 未选中：卡片底 + 描边。宽度按文字实测，中英文都贴合。
fn settings_choice(ui: &mut egui::Ui, label: &str, selected: bool) -> bool {
    let galley = ui.painter().layout_no_wrap(
        label.to_string(),
        FontId::proportional(12.5),
        theme::text(),
    );
    let width = galley.size().x + 26.0;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 30.0), Sense::click());

    let palette = theme::palette();
    let (fill, stroke, color) = if selected {
        (palette.accent_dim, Stroke::new(1.0_f32, palette.accent), palette.accent)
    } else if response.hovered() {
        (palette.card_hover, Stroke::new(1.0_f32, palette.border), palette.text)
    } else {
        (palette.card, Stroke::new(1.0_f32, palette.border), palette.text)
    };
    ui.painter().rect_filled(rect, egui::Rounding::same(theme::RADIUS_SM), fill);
    ui.painter().rect_stroke(rect, egui::Rounding::same(theme::RADIUS_SM), stroke);
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, label, FontId::proportional(12.5), color);
    response.clicked()
}

/// 开关（自绘）：默认的 checkbox 在深色卡片上又小又不明显。
fn settings_toggle(ui: &mut egui::Ui, value: &mut bool, title: &str, hint: &str) -> bool {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 44.0), Sense::click());
    let painter = ui.painter().clone();
    let track = Rect::from_center_size(
        egui::pos2(rect.max.x - 24.0, rect.center().y),
        Vec2::new(42.0, 24.0),
    );
    // 药丸形：圆角取高度的一半，跟"大块用 RADIUS_LG"是两码事。
    let pill = egui::Rounding::same(track.height() * 0.5);
    if *value {
        painter.rect_filled(track, pill, theme::accent());
    } else {
        painter.rect_filled(track, pill, theme::border());
    }
    let knob_x = if *value { track.max.x - 12.0 } else { track.min.x + 12.0 };
    painter.circle_filled(egui::pos2(knob_x, track.center().y), 8.5, Color32::WHITE);

    painter.text(
        egui::pos2(rect.min.x, rect.center().y - 8.0),
        Align2::LEFT_CENTER,
        title,
        FontId::proportional(13.0),
        theme::text(),
    );
    painter.text(
        egui::pos2(rect.min.x, rect.center().y + 10.0),
        Align2::LEFT_CENTER,
        hint,
        FontId::proportional(10.5),
        theme::faint(),
    );

    if response.clicked() {
        *value = !*value;
        return true;
    }
    false
}

fn day_separator(ui: &mut egui::Ui, label: &str) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_space(16.0);
        ui.label(RichText::new(label).size(10.5).color(theme::faint()));
    });
    ui.add_space(4.0);
}

fn system_line(ui: &mut egui::Ui, text: &str) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(16.0);
        let width = (ui.available_width() - 32.0).max(80.0);
        let galley = ui.painter().layout(
            text.to_string(),
            FontId::proportional(11.0),
            theme::faint(),
            width,
        );
        let size = galley.size() + Vec2::new(16.0, 8.0);
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        ui.painter()
            .rect_filled(rect, egui::Rounding::same(theme::RADIUS_MD), theme::card());
        ui.painter().galley(rect.min + Vec2::new(8.0, 4.0), galley, theme::faint());
    });
    ui.add_space(4.0);
}

/// 渲染一条消息气泡；返回需要 app 处理的交互。
///
/// 气泡是**手工测量 + 手工绘制**的，不用 `Frame` + 子 UI，原因有三个：
/// 1. 气泡要贴着右边（外层是 RTL 布局），而 `with_layout` / `allocate_ui` 会把子 UI
///    撑满整行，气泡就变成通栏；
/// 2. 直接往 RTL 里塞「正文 + 时间」，两者会被排进同一行，时间跑到正文左边；
/// 3. 时间和送达状态需要贴气泡内右下角，只有手绘才能精确控制。
fn message_bubble(
    ui: &mut egui::Ui,
    message: &ChatMessage,
    transfer: Option<&TransferState>,
    peer_name: &str,
    emojis: &EmojiSet,
    show_avatar: bool,
) -> Option<MsgAction> {
    const PAD: Vec2 = Vec2::new(13.0, 9.0);
    const GAP: f32 = 5.0;
    const ICON: f32 = 36.0;
    const BADGE: f32 = 15.0;
    /// 头像与气泡之间的空隙
    const GUTTER: f32 = 10.0;

    let palette = theme::palette();
    let outgoing = message.outgoing;
    let painter = ui.painter().clone();
    let body_color = if outgoing { palette.bubble_out_text } else { palette.bubble_in_text };
    let meta_color = if outgoing { palette.bubble_out_meta } else { palette.bubble_in_meta };
    let fill = if outgoing { palette.bubble_out } else { palette.bubble_in };
    let rounding = if outgoing {
        egui::Rounding { nw: 14.0, ne: 4.0, sw: 14.0, se: 14.0 }
    } else {
        egui::Rounding { nw: 4.0, ne: 14.0, sw: 14.0, se: 14.0 }
    };

    let avail = ui.available_rect_before_wrap();
    let row = Rect::from_min_max(
        pos2(avail.min.x + 16.0, avail.min.y),
        pos2(avail.max.x - 16.0, avail.max.y),
    );
    // 头像那一列始终预留出来：即使这条消息不画头像，气泡也和同侧其它气泡对齐。
    let side = AVATAR_SIZE + GUTTER;
    let (space_min_x, space_max_x) = if outgoing {
        (row.min.x, row.max.x - side)
    } else {
        (row.min.x + side, row.max.x)
    };
    let cap = (space_max_x - space_min_x).min(540.0);
    let inner_cap = (cap - PAD.x * 2.0).max(80.0);

    // ---------- 元信息：时间 + 送达状态 ----------
    let time_galley = painter.layout_no_wrap(
        widgets::time_hm(message.at),
        FontId::proportional(10.0),
        meta_color,
    );
    let meta_w = time_galley.size().x + if outgoing { BADGE + 4.0 } else { 0.0 };
    let meta_h = 14.0_f32;

    // ---------- 正文 ----------
    let mut action = None;
    let mut text_galley = None;
    let mut file_galley = None;
    // 纯表情消息：整段只有表情符号时按贴图画，颜色和面板里一致。
    let mut emoji_run: Option<(Vec<usize>, usize)> = None;
    // 文字里夹着表情：字体渲染不出彩色，字形墨迹还会超出度量宽度，
    // 行尾的表情会被气泡裁掉。这类消息改成自己排版 + 贴图。
    let mut inline_run: Option<InlineBody> = None;
    let body_w: f32;
    let body_h: f32;
    let mut running: Option<f32> = None;

    match message.kind {
        MessageKind::File => {
            let name = if message.file_name.is_empty() {
                tr("文件").to_string()
            } else {
                message.file_name.clone()
            };
            let mut detail = human_size(message.file_size);
            if let Some(state) = transfer {
                if state.status == RowStatus::Running {
                    running = Some(state.fraction().clamp(0.0, 1.0));
                    let phase =
                        if state.direction == Direction::Send { tr("发送中") } else { tr("接收中") };
                    detail = format!(
                        "{phase} {:.0}% · {}/s",
                        state.fraction() * 100.0,
                        human_size(state.speed.max(0.0) as u64)
                    );
                }
            }
            let text_cap = (inner_cap - ICON - 10.0).max(60.0);
            let name_galley =
                painter.layout(name, FontId::proportional(13.0), body_color, text_cap);
            let detail_galley =
                painter.layout_no_wrap(detail, FontId::proportional(10.5), meta_color);
            let col_w = name_galley.size().x.max(detail_galley.size().x).min(text_cap);
            body_w = ICON + 10.0 + col_w;
            let col_h = name_galley.size().y + 2.0 + detail_galley.size().y;
            body_h = ICON.max(col_h) + if running.is_some() { 9.0 } else { 0.0 };
            file_galley = Some((name_galley, detail_galley, col_w));
        }
        _ => {
            if let Some(indices) = emojis.only_emoji(&message.text) {
                const GAP: f32 = 4.0;
                let per_row = (((inner_cap + GAP) / (BIG_EMOJI + GAP)).floor() as usize)
                    .clamp(1, 6)
                    .min(indices.len().max(1));
                let rows = indices.len().div_ceil(per_row);
                let cols = indices.len().min(per_row);
                body_w = cols as f32 * (BIG_EMOJI + GAP) - GAP;
                body_h = rows as f32 * (BIG_EMOJI + GAP) - GAP;
                emoji_run = Some((indices, per_row));
            } else if emojis.contains_any(&message.text) {
                let body = layout_inline(
                    &painter,
                    &message.text,
                    emojis,
                    inner_cap,
                    &FontId::proportional(14.0),
                    body_color,
                );
                body_w = body.width.min(inner_cap);
                body_h = body.height;
                inline_run = Some(body);
            } else {
                let galley = painter.layout(
                    message.text.clone(),
                    FontId::proportional(14.0),
                    body_color,
                    inner_cap,
                );
                body_w = galley.size().x;
                body_h = galley.size().y;
                text_galley = Some(galley);
            }
        }
    }

    let bubble_w = (body_w.max(meta_w) + PAD.x * 2.0).min(cap);
    let bubble_h = PAD.y * 2.0 + body_h + GAP + meta_h;
    let left = if outgoing { space_max_x - bubble_w } else { space_min_x };
    let rect = Rect::from_min_size(pos2(left, row.min.y), Vec2::new(bubble_w, bubble_h));

    painter.rect_filled(rect, rounding, fill);
    // 浅色模式下白色气泡贴在浅灰底上，需要一根发丝线才立得住。
    if !outgoing && palette.is_light {
        painter.rect_stroke(rect, rounding, Stroke::new(1.0_f32, theme::border()));
    }

    // ---------- 正文落位 ----------
    let origin = rect.min + PAD;
    if let Some(galley) = text_galley {
        // 纯文本消息用「可选中」的 Label 渲染：像普通文本一样框选、Ctrl+C 复制。
        // 气泡背景是绝对定位画的，这里也用一个绝对定位的子 Ui 放 Label，让它
        // 正好落在气泡正文区（origin）。布局要开 main_wrap，Label 才能在气泡
        // 宽度内自动换行（left_to_right 是单行布局，长文本会被挤成一条）。
        let text_rect = Rect::from_min_size(origin, galley.size());
        let mut text_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(text_rect)
                .layout(Layout::left_to_right(Align::Min).with_main_wrap(true)),
        );
        let resp = text_ui.add(
            egui::Label::new(RichText::new(&message.text).size(14.0).color(body_color))
                .selectable(true),
        );
        // 右键菜单直接挂在文字上：右键气泡里的文字即可复制整条消息。
        resp.context_menu(|ui| {
            ui.set_min_width(104.0);
            if ui.selectable_label(false, tr("复制")).clicked() {
                action = Some(MsgAction::Copy(message.text.clone()));
            }
        });
    } else if let Some((indices, per_row)) = emoji_run {
        const GAP: f32 = 4.0;
        for (offset, index) in indices.iter().enumerate() {
            let Some(item) = emojis.get(*index) else { continue };
            let cell = Rect::from_min_size(
                origin
                    + Vec2::new(
                        (offset % per_row) as f32 * (BIG_EMOJI + GAP),
                        (offset / per_row) as f32 * (BIG_EMOJI + GAP),
                    ),
                Vec2::splat(BIG_EMOJI),
            );
            emoji::paint(&painter, cell, &item.texture, BIG_EMOJI);
        }
    } else if let Some(body) = inline_run {
        let row_h = body.pitch - INLINE_GAP;
        for (line_y, segments) in &body.lines {
            for (seg_x, segment) in segments {
                let x = origin.x + seg_x;
                let y = origin.y + line_y;
                match segment {
                    InlineSegment::Text(run) => {
                        let galley = painter.layout_no_wrap(
                            run.clone(),
                            FontId::proportional(14.0),
                            body_color,
                        );
                        painter.galley(
                            pos2(x, y + (row_h - galley.size().y) * 0.5),
                            galley,
                            body_color,
                        );
                    }
                    InlineSegment::Emoji(index) => {
                        if let Some(item) = emojis.get(*index) {
                            let cell = Rect::from_min_size(
                                pos2(x, y + (row_h - EMOJI_INLINE) * 0.5),
                                Vec2::splat(EMOJI_INLINE),
                            );
                            emoji::paint(&painter, cell, &item.texture, EMOJI_INLINE);
                        }
                    }
                    InlineSegment::System(ch, width) => {
                        // 表情集之外的表情：拿系统字体兜底，但仍按预留宽度居中，
                        // 这样即便字形墨迹比度量宽，也不会压到后面的字。
                        painter.text(
                            pos2(x + width * 0.5, y + row_h * 0.5),
                            egui::Align2::CENTER_CENTER,
                            ch,
                            FontId::proportional(14.0),
                            body_color,
                        );
                    }
                }
            }
        }
    } else if let Some((name_galley, detail_galley, col_w)) = file_galley {
        let glass = if outgoing {
            theme::alpha(Color32::WHITE, 38)
        } else if palette.is_light {
            theme::alpha(palette.accent, 26)
        } else {
            theme::alpha(Color32::WHITE, 20)
        };
        widgets::paint_file_icon(
            &painter,
            Rect::from_min_size(origin, Vec2::splat(ICON)),
            glass,
            body_color,
        );
        let col_x = origin.x + ICON + 10.0;
        painter.galley(pos2(col_x, origin.y + 1.0), name_galley, body_color);
        painter.galley(
            pos2(col_x, origin.y + 2.0 + 17.0),
            detail_galley,
            meta_color,
        );
        if let Some(fraction) = running {
            let bar = Rect::from_min_size(
                pos2(col_x, origin.y + body_h - 4.0),
                Vec2::new(col_w.max(60.0), 3.5),
            );
            painter.rect_filled(bar, egui::Rounding::same(2.0), theme::alpha(Color32::WHITE, 56));
            painter.rect_filled(
                Rect::from_min_size(bar.min, Vec2::new(bar.width() * fraction, bar.height())),
                egui::Rounding::same(2.0),
                theme::alpha(Color32::WHITE, 235),
            );
        }
    }

    // ---------- 元信息落位：贴内右下角 ----------
    let meta_y = rect.max.y - PAD.y - meta_h;
    let time_x = rect.max.x - PAD.x - meta_w;
    painter.galley(
        pos2(time_x, meta_y + (meta_h - time_galley.size().y) * 0.5),
        time_galley,
        meta_color,
    );
    if outgoing {
        let badge_rect = Rect::from_min_size(
            pos2(rect.max.x - PAD.x - BADGE, meta_y + (meta_h - 13.0) * 0.5),
            Vec2::splat(13.0),
        );
        let color = if message.status == MessageStatus::Failed {
            theme::err()
        } else {
            palette.bubble_out_meta
        };
        widgets::paint_status(&painter, badge_rect, message.status, color);
    }

    // ---------- 头像：与气泡顶部对齐 ----------
    if show_avatar {
        let avatar_rect = Rect::from_min_size(
            pos2(
                if outgoing { row.max.x - AVATAR_SIZE } else { row.min.x },
                rect.min.y + 1.0,
            ),
            Vec2::splat(AVATAR_SIZE),
        );
        widgets::paint_avatar(
            &painter,
            avatar_rect,
            peer_name,
            &message.peer,
            false,
            theme::bg(),
        );
    }

    // ---------- 交互 ----------
    // 纯文本气泡的可选中 Label 已负责框选复制 + 右键复制；这里只需给文件气泡
    // 挂点击（打开文件夹 / 取消传输）。
    if message.kind == MessageKind::File {
        let response = ui.interact(
            rect,
            ui.make_persistent_id(("bubble", &message.id)),
            Sense::click(),
        );

        if response.clicked() {
            if let Some(session) = message.session_id.clone() {
                if running.is_some() {
                    action = Some(MsgAction::Cancel(session));
                }
            }
            if action.is_none() {
                if let Some(path) = &message.file_path {
                    action = Some(MsgAction::Reveal(path.clone()));
                }
            }
        }

        let hint = if running.is_some() {
            Some(tr("点击取消传输"))
        } else {
            Some(tr("点击打开所在文件夹"))
        };
        if let Some(text) = hint {
            let _ = response.on_hover_text(text);
        }
    }

    // 发送失败的文本消息：点击气泡可重发。正常文本不抢点击（让位给选中复制），
    // 只有失败态才叠加一个点击交互。
    if outgoing
        && message.status == MessageStatus::Failed
        && message.kind != MessageKind::File
    {
        let resp = ui.interact(
            rect,
            ui.make_persistent_id(("retry", &message.id)),
            Sense::click(),
        );
        if resp.clicked() {
            action = Some(MsgAction::Retry(message.peer.clone(), message.text.clone()));
        }
    }

    let row_h = bubble_h.max(if show_avatar { AVATAR_SIZE } else { 0.0 });
    ui.advance_cursor_after_rect(Rect::from_min_size(
        avail.min,
        Vec2::new(avail.width(), row_h),
    ));
    action
}

/// 混排时行与行之间多留的空白。
const INLINE_GAP: f32 = 2.0;

/// 混排正文里的一段。文字成段保存是为了让同一段共用一次排版（保留字距），
/// 表情单独成段，好按固定边长贴图。
enum InlineSegment {
    Text(String),
    /// 表情集内的表情，`usize` 是下标。
    Emoji(usize),
    /// 表情集之外、需要系统字体兜底的字符，附带为它预留的宽度。
    System(char, f32),
}

/// `layout_inline` 的结果：每行是「纵坐标 + 若干 (横坐标, 段落)」。
struct InlineBody {
    lines: Vec<(f32, Vec<(f32, InlineSegment)>)>,
    width: f32,
    height: f32,
    /// 行距（行高 + `INLINE_GAP`）。
    pitch: f32,
}

fn is_emoji_like(ch: char) -> bool {
    matches!(ch as u32, 0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2B00..=0x2BFF)
}

/// 文字 + 表情混排：逐字贪心换行（中文没有词边界，按字断最自然），
/// 表情当成一个固定边长的字。
///
/// 为什么不用 `egui` 的 `layout`：它只认字形宽度，表情字形经常「墨迹比度量宽」，
/// 行尾的表情就会溢出气泡被裁掉；而且贴图也没法塞进 galley 里。
fn layout_inline(
    painter: &egui::Painter,
    text: &str,
    emojis: &EmojiSet,
    cap: f32,
    font: &FontId,
    color: Color32,
) -> InlineBody {
    let row_h =
        painter.layout_no_wrap("国".to_owned(), font.clone(), color).size().y.max(EMOJI_INLINE);
    let pitch = row_h + INLINE_GAP;

    let mut rows: Vec<Vec<(f32, InlineSegment)>> = vec![Vec::new()];
    let mut x = 0.0_f32;
    let mut max_x = 0.0_f32;

    for ch in text.chars() {
        // 变体选择符和零宽连接符不占宽度，直接跳过。
        if matches!(ch, '\u{fe0f}' | '\u{200d}') {
            continue;
        }
        if ch == '\n' {
            max_x = max_x.max(x);
            rows.push(Vec::new());
            x = 0.0;
            continue;
        }

        let known = emojis.index_of(ch);
        let system = known.is_none() && is_emoji_like(ch);
        let advance = if known.is_some() {
            EMOJI_INLINE
        } else {
            let measured =
                painter.layout_no_wrap(ch.to_string(), font.clone(), color).size().x;
            if system {
                measured.max(EMOJI_INLINE)
            } else {
                measured
            }
        };

        if x + advance > cap + 0.5 && x > 0.0 {
            max_x = max_x.max(x);
            rows.push(Vec::new());
            x = 0.0;
        }

        let row = rows.last_mut().expect("至少有一行");
        match known {
            Some(index) => row.push((x, InlineSegment::Emoji(index))),
            None if system => row.push((x, InlineSegment::System(ch, advance))),
            None => {
                // 连着的普通文字并进同一段，避免逐字画导致字距不自然。
                match row.last_mut() {
                    Some((_, InlineSegment::Text(run))) => run.push(ch),
                    _ => row.push((x, InlineSegment::Text(ch.to_string()))),
                }
            }
        }
        x += advance;
    }

    let width = max_x.max(x);
    let height = (rows.len() as f32 * pitch - INLINE_GAP).max(row_h);
    InlineBody {
        lines: rows
            .into_iter()
            .enumerate()
            .map(|(index, segments)| (index as f32 * pitch, segments))
            .collect(),
        width,
        height,
        pitch,
    }
}

/// 收到文件请求时的内联卡片（替代打断式弹窗）。
/// 返回 `Some(true)` 表示接受、`Some(false)` 表示拒绝。
fn incoming_card(
    ui: &mut egui::Ui,
    sender: &str,
    files: &[FileMetadata],
    total: u64,
) -> Option<bool> {
    let mut decision = None;
    ui.horizontal(|ui| {
        // 和对方气泡的左边缘对齐（让开头像那一列）
        ui.add_space(16.0 + AVATAR_SIZE + 10.0);
        egui::Frame::none()
            .fill(theme::card())
            .stroke(Stroke::new(1.0_f32, theme::border()))
            .rounding(egui::Rounding::same(theme::RADIUS_LG))
            .inner_margin(egui::Margin::symmetric(14.0, 12.0))
            .show(ui, |ui| {
                // 外层是 horizontal，Frame 内部默认会继承横向布局，
                // 结果所有文字和按钮挤成一行、互相压字。这里强制改成纵向。
                ui.vertical(|ui| {
                    ui.set_max_width(420.0);
                    ui.label(
                        RichText::new(tf(tr("{} 想发送 {} 个文件"), &[&sender, &files.len()]))
                            .size(13.5)
                            .strong()
                            .color(theme::text()),
                    );
                    ui.label(
                        RichText::new(tf(tr("共 {} · 保存在接收目录"), &[&human_size(total)]))
                            .size(11.0)
                            .color(theme::muted()),
                    );
                    ui.add_space(6.0);
                    for file in files.iter().take(4) {
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!(
                                    "· {}  {}",
                                    file.file_name,
                                    human_size(file.size)
                                ))
                                .size(11.0)
                                .color(theme::muted()),
                            )
                            .truncate(),
                        );
                    }
                    if files.len() > 4 {
                        ui.label(
                            RichText::new(tf(tr("… 还有 {} 个文件"), &[&(files.len() - 4)]))
                                .size(11.0)
                                .color(theme::faint()),
                        );
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.add(widgets::primary_button(tr("接受"), true)).clicked() {
                            decision = Some(true);
                        }
                        if ui.add(widgets::ghost_button(tr("拒绝"))).clicked() {
                            decision = Some(false);
                        }
                    });
                });
            });
    });
    decision
}

impl eframe::App for FeigeCraftApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 「跟随系统」时主题可能被操作系统改掉，每帧同步一次调色板。
        theme::sync(ctx.theme());

        self.sync_tray(ctx);
        self.pump_wakeup(ctx);
        self.pump_hide(ctx);
        self.pump_events();
        self.pump_tasks();
        self.handle_dropped(ctx);

        self.title_bar(ctx);
        self.sidebar(ctx);
        self.chat_area(ctx);
        self.windows(ctx);
        self.handle_screenshot(ctx);

        ctx.request_repaint_after(Duration::from_millis(120));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // 不摘掉的话，任务栏右下角会留一个点了没反应的「幽灵图标」。
        crate::tray::uninstall();
    }
}

impl FeigeCraftApp {
    /// 把「窗口在不在前台」和「未读总数」推给托盘。
    fn sync_tray(&self, ctx: &egui::Context) {
        let focused = ctx.input(|input| input.viewport().focused.unwrap_or(false));
        crate::tray::set_foreground(focused && !self.hide_pending());
        crate::tray::set_unread(self.unread.values().sum());
    }

    /// 已经点了关闭、正在收起窗口。
    fn hide_pending(&self) -> bool {
        self.hide_at.is_some() || self.hide_frames > 0
    }

    /// 有人双击程序图标 → 把窗口叫回来。
    fn pump_wakeup(&mut self, ctx: &egui::Context) {
        let woke = self
            .instance_guard
            .as_ref()
            .is_some_and(crate::instance::Instance::take_wakeup);
        if woke {
            self.hide_at = None;
            ctx.send_viewport_cmd(ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(ViewportCommand::Focus);
        }
    }

    /// 收起到后台：拦截系统关窗，并在提示露过面之后真正隐藏窗口。
    fn pump_hide(&mut self, ctx: &egui::Context) {
        // 托盘菜单里点了「退出」：老老实实退，别再拦成「收进后台」。
        if crate::tray::quit_requested() {
            self.quitting = true;
            ctx.send_viewport_cmd(ViewportCommand::Close);
            return;
        }
        if self.quitting {
            return;
        }
        // Alt+F4、任务栏右键关闭都会走到 close_requested，和标题栏的 × 等价。
        let close_requested = ctx.input(|input| input.viewport().close_requested());
        if close_requested {
            tracing::debug!("收到关窗请求 background_on_close={}", self.config.background_on_close);
        }
        if close_requested && self.config.background_on_close {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            if self.hide_at.is_none() {
                self.request_close(ctx);
            }
        }
        if self.hide_frames > 0 {
            self.hide_frames -= 1;
            if self.hide_frames == 0 {
                crate::tray::hide_main();
            } else {
                ctx.request_repaint();
            }
        }
        if let Some(at) = self.hide_at {
            if Instant::now() >= at {
                tracing::debug!("隐藏窗口（转后台）");
                self.hide_at = None;
                crate::tray::hide_main();
            } else {
                ctx.request_repaint_after(Duration::from_millis(60));
            }
        }
    }

    /// 点 × 之后的行为。
    fn request_close(&mut self, ctx: &egui::Context) {
        if !self.config.background_on_close {
            ctx.send_viewport_cmd(ViewportCommand::Close);
            return;
        }
        // 窗口直接消失容易让人以为「关掉了」，先让提示露个面再收。
        tracing::debug!("准备收进后台（1.5s 后隐藏）");
        self.toast(tr("已转入后台继续接收，再双击程序即可重新打开"), false);
        self.hide_at = Some(Instant::now() + Duration::from_millis(1500));
        ctx.request_repaint_after(Duration::from_millis(60));
    }

    fn handle_dropped(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .filter_map(|file| file.path.clone())
                .collect()
        });
        if !dropped.is_empty() {
            self.send_paths(dropped);
        }
    }
}
