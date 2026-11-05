//! 接收端与发现模块共享的运行时状态。

use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

use crate::chat::ChatStore;
use crate::config::Config;
use crate::events::AppEvent;
use crate::model::{DeviceInfo, DeviceType};
use crate::tls::Identity;

pub struct SharedState {
    pub identity: Identity,
    pub config: Mutex<Config>,
    pub device: Mutex<DeviceInfo>,
    pub events: broadcast::Sender<AppEvent>,
    /// 聊天记录（收发两个方向共用一份）。
    pub chats: ChatStore,
}

impl SharedState {
    pub fn new(
        identity: Identity,
        config: Config,
        events: broadcast::Sender<AppEvent>,
        chats: ChatStore,
    ) -> Arc<Self> {
        let device = DeviceInfo::new(
            config.alias.clone(),
            config.device_type,
            identity.fingerprint.clone(),
            config.port,
        );
        Arc::new(Self {
            identity,
            config: Mutex::new(config),
            device: Mutex::new(device),
            events,
            chats,
        })
    }

    pub fn device_info(&self) -> DeviceInfo {
        self.device.lock().expect("device mutex poisoned").clone()
    }

    pub fn fingerprint(&self) -> String {
        self.identity.fingerprint.clone()
    }

    pub fn config(&self) -> Config {
        self.config.lock().expect("config mutex poisoned").clone()
    }

    pub fn download_dir(&self) -> std::path::PathBuf {
        self.config.lock().expect("config mutex poisoned").download_dir.clone()
    }

    pub fn auto_accept(&self) -> bool {
        self.config.lock().expect("config mutex poisoned").auto_accept
    }

    /// 对端的显示名：备注优先，其次对方广播的名字。
    pub fn display_name(&self, peer: &str) -> Option<String> {
        if let Some(remark) = self.config().remark_for(peer).map(str::to_string) {
            return Some(remark);
        }
        self.chats.alias(peer)
    }

    /// 设置 / 清除备注（空字符串表示清除），并落盘。
    pub fn set_remark(&self, peer: &str, remark: &str) -> anyhow::Result<()> {
        {
            let mut config = self.config.lock().expect("config mutex poisoned");
            let trimmed = remark.trim();
            if trimmed.is_empty() {
                config.remarks.remove(peer);
            } else {
                config.remarks.insert(peer.to_string(), trimmed.to_string());
            }
        }
        self.save_config()
    }

    pub fn remark(&self, peer: &str) -> Option<String> {
        self.config().remark_for(peer).map(str::to_string)
    }

    /// 统一在这里落盘，避免各调用点重复写配置。
    pub fn save_config(&self) -> anyhow::Result<()> {
        self.config().save()
    }

    pub fn set_alias(&self, alias: &str) {
        {
            let mut config = self.config.lock().expect("config mutex poisoned");
            config.alias = alias.to_string();
        }
        let mut device = self.device.lock().expect("device mutex poisoned");
        device.alias = alias.to_string();
    }

    pub fn set_device_type(&self, device_type: DeviceType) {
        {
            let mut config = self.config.lock().expect("config mutex poisoned");
            config.device_type = device_type;
        }
        let mut device = self.device.lock().expect("device mutex poisoned");
        device.device_type = Some(device_type);
    }

    /// 界面主题偏好。核心层不用它，但要跟着配置一起保存，
    /// 否则 GUI 里读回的 `state.config()` 会把它覆盖成旧值。
    pub fn set_theme(&self, theme: &str) {
        let mut config = self.config.lock().expect("config mutex poisoned");
        config.theme = theme.to_string();
    }

    pub fn set_auto_accept(&self, value: bool) {
        let mut config = self.config.lock().expect("config mutex poisoned");
        config.auto_accept = value;
    }

    pub fn set_download_dir(&self, dir: std::path::PathBuf) {
        if std::fs::create_dir_all(&dir).is_ok() {
            let mut config = self.config.lock().expect("config mutex poisoned");
            config.download_dir = dir;
        }
    }

    pub fn set_port(&self, port: u16) {
        {
            let mut config = self.config.lock().expect("config mutex poisoned");
            config.port = port;
        }
        let mut device = self.device.lock().expect("device mutex poisoned");
        device.port = port;
    }

    /// 广播一个事件；没有订阅者时静默丢弃。
    pub fn emit(&self, event: AppEvent) {
        let _ = self.events.send(event);
    }
}
