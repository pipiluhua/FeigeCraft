//! 把接收服务与设备发现组装成一个可运行的节点。

use std::fs;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::broadcast;

use crate::chat::ChatStore;
use crate::config::Config;
use crate::discovery::Discovery;
use crate::events::AppEvent;
use crate::model::DeviceInfo;
use crate::server::Receiver;
use crate::state::SharedState;
use crate::tls::Identity;

/// 安装 rustls 的加密后端（多次调用安全）。
pub fn init_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

pub struct Node {
    pub state: Arc<SharedState>,
    pub receiver: Arc<Receiver>,
    /// 组播端口被占用时为 `None`，此时仍可通过手动输入 IP 传输。
    pub discovery: Option<Arc<Discovery>>,
}

impl Node {
    /// 启动接收服务 + 设备发现（使用默认配置目录）。
    pub async fn start(config: Config) -> Result<Arc<Self>> {
        Self::start_in(config, Config::config_dir()).await
    }

    /// 使用指定的配置 / 证书目录启动，便于多实例与测试。
    pub async fn start_in(config: Config, config_dir: std::path::PathBuf) -> Result<Arc<Self>> {
        init_crypto();
        fs::create_dir_all(&config_dir).context("创建配置目录失败")?;
        let identity = Identity::load_or_create(&config_dir)?;
        if let Err(err) = config.ensure_download_dir() {
            tracing::warn!("下载目录不可用: {err}");
        }

        let (events, _) = broadcast::channel(4096);
        let chats = ChatStore::load(config_dir.join("chats.json"));
        let state = SharedState::new(identity, config, events, chats);
        let receiver = Receiver::bind(state.clone()).await?;

        let discovery = match Discovery::start(state.clone()).await {
            Ok(discovery) => Some(discovery),
            Err(err) => {
                state.emit(AppEvent::log_warn(format!(
                    "组播发现不可用（{err}），可手动输入对方 IP"
                )));
                None
            }
        };

        // 组播被屏蔽 / 端口被占用时，启动后自动做一次子网扫描兜底
        if let Some(found) = &discovery {
            let discovery = found.clone();
            let state_for_scan = state.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(4)).await;
                if discovery.devices().is_empty() {
                    state_for_scan.emit(AppEvent::log_info("组播没有回应，正在扫描本机子网…"));
                    let _ = discovery.scan().await;
                }
            });
        }

        let node = Arc::new(Self { state: state.clone(), receiver, discovery });
        let info = state.device_info();
        state.emit(AppEvent::log_info(format!(
            "{} 已就绪，监听 {}:{}",
            info.alias, "0.0.0.0", info.port
        )));
        Ok(node)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AppEvent> {
        self.state.events.subscribe()
    }

    pub fn device_info(&self) -> DeviceInfo {
        self.state.device_info()
    }

    /// 应用新的配置（改名、改类型、改下载目录、自动接受）。
    pub fn apply_config(&self, config: &Config) -> Result<()> {
        self.state.set_alias(&config.alias);
        self.state.set_device_type(config.device_type);
        self.state.set_auto_accept(config.auto_accept);
        self.state.set_download_dir(config.download_dir.clone());
        self.state.set_theme(&config.theme);
        config.save()?;
        if let Some(discovery) = &self.discovery {
            let discovery = discovery.clone();
            tokio::spawn(async move { discovery.announce().await });
        }
        Ok(())
    }

    /// 触发一次子网扫描，返回新发现的数量。
    pub async fn scan(&self) -> usize {
        match &self.discovery {
            Some(discovery) => match discovery.scan().await {
                Ok(count) => count,
                Err(err) => {
                    self.state.emit(AppEvent::log_warn(format!("扫描失败: {err}")));
                    0
                }
            },
            None => 0,
        }
    }

    pub fn devices(&self) -> Vec<crate::model::DiscoveredDevice> {
        self.discovery
            .as_ref()
            .map(|discovery| discovery.devices())
            .unwrap_or_default()
    }
}
