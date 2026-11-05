//! FeigeCraft Core —— 一个用 Rust 实现的、与 LocalSend 协议互通的局域网文件传输引擎。
//!
//! 模块划分：
//! - [`chat`]      局域网聊天（消息模型 + 本地会话存储）
//! - [`model`]     协议数据模型（camelCase，与 LocalSend 2.x 对齐）
//! - [`tls`]       自签名证书 + 指纹固定校验
//! - [`discovery`] 组播发现 + 子网扫描
//! - [`server`]    接收端 HTTPS 服务
//! - [`client`]    发送端
//! - [`node`]      组装成可运行节点
//!
//! ```no_run
//! # async fn demo() -> anyhow::Result<()> {
//! use feigecraft_core::{config::Config, node::Node};
//! let node = Node::start(Config::default()).await?;
//! println!("监听端口 {}", node.device_info().port);
//! # Ok(())
//! # }
//! ```

pub mod chat;
pub mod client;
pub mod config;
pub mod discovery;
pub mod events;
pub mod model;
pub mod node;
pub mod server;
pub mod state;
pub mod tls;
pub mod util;

pub use chat::{ChatMessage, ChatStore, MessageKind, MessageStatus};
pub use config::Config;
pub use events::{AppEvent, LogLevel};
pub use model::{
    DeviceInfo, DeviceType, Direction, DiscoveredDevice, FileMetadata, ProtocolType,
    DEFAULT_PORT, PROTOCOL_VERSION,
};
pub use node::Node;

/// 初始化日志（`FEIGECRAFT_LOG` 或 `RUST_LOG` 控制级别）。
pub fn init_logging() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_env("FEIGECRAFT_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("info,feigecraft_core=debug"));
    let _ = fmt().with_env_filter(filter).with_target(false).try_init();
}
