//! 应用事件：核心层通过 `broadcast` 通道把状态推给 GUI / CLI。

use std::path::PathBuf;

use crate::model::{DeviceInfo, Direction, DiscoveredDevice, FileMetadata};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

/// 核心层对外广播的所有事件。
#[derive(Debug, Clone)]
pub enum AppEvent {
    DeviceFound(DiscoveredDevice),
    DeviceLost {
        fingerprint: String,
    },
    /// 收到别人的发送请求，需要用户（或自动接受策略）裁决。
    IncomingRequest {
        session_id: String,
        sender: DeviceInfo,
        files: Vec<FileMetadata>,
        total_bytes: u64,
    },
    /// 请求已被裁决（自动接受 / 手动接受 / 拒绝 / 超时）。
    RequestResolved {
        session_id: String,
        accepted: bool,
    },
    /// 一个传输会话开始（发送或接收）。
    SessionStarted {
        session_id: String,
        direction: Direction,
        peer: String,
        files: usize,
        total_bytes: u64,
    },
    /// 单个文件进度。
    FileProgress {
        session_id: String,
        file_name: String,
        transferred: u64,
        total: u64,
        direction: Direction,
    },
    /// 单个文件完成。
    FileFinished {
        session_id: String,
        file_name: String,
        path: Option<PathBuf>,
        size: u64,
        direction: Direction,
    },
    /// 整个会话结束。
    SessionFinished {
        session_id: String,
        direction: Direction,
        files: usize,
        bytes: u64,
        dir: Option<PathBuf>,
    },
    /// 会话失败或取消。
    SessionFailed {
        session_id: String,
        direction: Direction,
        error: String,
    },
    /// 某个会话有新消息（自己发出、对方发来、或状态变化），前端重读该会话即可。
    ChatUpdated {
        peer: String,
    },
    /// 对方发来了一条消息。
    ChatReceived {
        peer: String,
        alias: String,
        text: String,
    },
    Log {
        level: LogLevel,
        message: String,
    },
}

impl AppEvent {
    pub fn log_info(message: impl Into<String>) -> Self {
        AppEvent::Log { level: LogLevel::Info, message: message.into() }
    }
    pub fn log_warn(message: impl Into<String>) -> Self {
        AppEvent::Log { level: LogLevel::Warn, message: message.into() }
    }
    pub fn log_error(message: impl Into<String>) -> Self {
        AppEvent::Log { level: LogLevel::Error, message: message.into() }
    }
}
