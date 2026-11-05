//! LocalSend v2 协议数据模型。
//!
//! 全部结构体字段名遵循 LocalSend 的 camelCase 约定，以便与官方客户端互通。

use std::collections::HashMap;
use std::fmt;
use std::net::IpAddr;
use std::time::Instant;

use serde::{Deserialize, Serialize};

/// 我们实现的协议版本，与 LocalSend 2.1 对齐。
pub const PROTOCOL_VERSION: &str = "2.1";
/// 默认端口（TCP + UDP 共用）。
pub const DEFAULT_PORT: u16 = 53317;
/// 组播地址 224.0.0.167:53317
pub const MULTICAST_ADDR: [u8; 4] = [224, 0, 0, 167];

fn default_port() -> u16 {
    DEFAULT_PORT
}
fn default_protocol() -> ProtocolType {
    ProtocolType::Https
}
fn default_version() -> String {
    PROTOCOL_VERSION.to_string()
}
fn default_mime() -> String {
    "application/octet-stream".to_string()
}

/// 设备类型，对应 LocalSend 的 `deviceType`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DeviceType {
    Mobile,
    #[default]
    Desktop,
    Web,
    Headless,
    Server,
}

impl DeviceType {
    pub const ALL: [DeviceType; 5] = [
        DeviceType::Desktop,
        DeviceType::Mobile,
        DeviceType::Web,
        DeviceType::Headless,
        DeviceType::Server,
    ];

    pub fn label(self) -> &'static str {
        match self {
            DeviceType::Mobile => "手机",
            DeviceType::Desktop => "电脑",
            DeviceType::Web => "网页",
            DeviceType::Headless => "无界面",
            DeviceType::Server => "服务器",
        }
    }
}

impl fmt::Display for DeviceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// 传输层协议，`http` 或 `https`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProtocolType {
    Http,
    #[default]
    Https,
}

impl ProtocolType {
    pub fn as_str(self) -> &'static str {
        match self {
            ProtocolType::Http => "http",
            ProtocolType::Https => "https",
        }
    }
}

impl fmt::Display for ProtocolType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 设备信息，用于组播广播、`/register` 与 `/info` 响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub alias: String,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_type: Option<DeviceType>,
    #[serde(default)]
    pub fingerprint: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_protocol")]
    pub protocol: ProtocolType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download: Option<bool>,
}

impl DeviceInfo {
    pub fn new(alias: impl Into<String>, device_type: DeviceType, fingerprint: String, port: u16) -> Self {
        Self {
            alias: alias.into(),
            version: PROTOCOL_VERSION.to_string(),
            device_model: Some(format!("{} ({})", std::env::consts::OS, std::env::consts::ARCH)),
            device_type: Some(device_type),
            fingerprint,
            port,
            protocol: ProtocolType::Https,
            download: Some(false),
        }
    }

    /// 生成 `https://ip:port` 形式的基础地址。
    pub fn base_url(&self, ip: IpAddr) -> String {
        match ip {
            IpAddr::V4(v4) => format!("{}://{}:{}", self.protocol, v4, self.port),
            IpAddr::V6(v6) => format!("{}://[{}]:{}", self.protocol, v6, self.port),
        }
    }
}

/// 组播发现的报文，等价于 `DeviceInfo` + `announce` 标记。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryMessage {
    #[serde(flatten)]
    pub info: DeviceInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub announce: Option<bool>,
}

impl DiscoveryMessage {
    pub fn announce(info: &DeviceInfo) -> Self {
        Self { info: info.clone(), announce: Some(true) }
    }
    pub fn answer(info: &DeviceInfo) -> Self {
        Self { info: info.clone(), announce: Some(false) }
    }
    pub fn is_announce(&self) -> bool {
        self.announce.unwrap_or(false)
    }
}

/// 局域网内发现到的设备。
#[derive(Debug, Clone)]
pub struct DiscoveredDevice {
    pub info: DeviceInfo,
    pub ip: IpAddr,
    pub last_seen: Instant,
    /// 是否由组播发现（否则来自 HTTP 扫描）。
    pub via_multicast: bool,
}

impl DiscoveredDevice {
    pub fn base_url(&self) -> String {
        self.info.base_url(self.ip)
    }
}

/// 单个文件的元数据。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileMetadata {
    pub id: String,
    pub file_name: String,
    pub size: u64,
    #[serde(default = "default_mime")]
    pub file_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

impl FileMetadata {
    pub fn from_path(path: &std::path::Path, size: u64) -> Self {
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unnamed".to_string());
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            file_name,
            size,
            file_type: crate::util::guess_mime(path),
            sha256: None,
            preview: None,
        }
    }
}

/// `POST /api/localsend/v2/prepare-upload` 请求体。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareUploadRequest {
    pub info: DeviceInfo,
    pub files: HashMap<String, FileMetadata>,
}

/// `POST /api/localsend/v2/prepare-upload` 响应体。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareUploadResponse {
    pub session_id: String,
    pub files: HashMap<String, String>,
}

/// `POST /api/localsend/v2/register` 的响应体。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterResponse {
    pub alias: String,
    pub fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_type: Option<DeviceType>,
}

/// 传输方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Send,
    Receive,
}
