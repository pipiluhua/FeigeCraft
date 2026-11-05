//! 本地配置的持久化（`<配置目录>/FeigeCraft/config.json`）。

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::model::{DeviceType, DEFAULT_PORT};
use crate::util;

/// 应用配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// 显示给其他设备看的名字。
    pub alias: String,
    pub device_type: DeviceType,
    /// 优先使用的端口，被占用时会自动顺延。
    pub port: u16,
    /// 接收文件的保存目录。
    pub download_dir: PathBuf,
    /// 是否自动接受所有发送请求（免确认）。
    pub auto_accept: bool,
    /// 给别人的备注：对端标识（指纹）→ 自定义名字。
    #[serde(default)]
    pub remarks: HashMap<String, String>,
    /// 界面主题：`system`（跟随系统）/ `light` / `dark`。
    #[serde(default = "default_theme")]
    pub theme: String,
    /// 界面语言：`system`（跟随系统）/ `zh-CN` / `en`。
    #[serde(default = "default_language")]
    pub language: String,
    /// 关闭窗口后是否留在后台继续收发（双击程序可重新唤出窗口）。
    #[serde(default = "default_true")]
    pub background_on_close: bool,
    /// 设备名是不是用户自己定下的。
    ///
    /// 老版本的默认名直接取电脑名，升级上来会一直是「DESKTOP-XXXX」这种，
    /// 看起来像「随机昵称失效」。启动时发现这个标记还是 `false`，
    /// 就补一次随机名，之后不再动。
    #[serde(default)]
    pub alias_custom: bool,
}

fn default_theme() -> String {
    "system".to_string()
}

fn default_language() -> String {
    "system".to_string()
}

fn default_true() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            alias: util::default_alias(),
            device_type: DeviceType::Desktop,
            port: DEFAULT_PORT,
            download_dir: util::default_download_dir(),
            auto_accept: false,
            remarks: HashMap::new(),
            theme: default_theme(),
            language: default_language(),
            background_on_close: true,
            alias_custom: false,
        }
    }
}

/// 由启动参数指定的配置目录。多开实例时用它把每个实例的配置 / 证书 / 聊天记录分开。
static DIR_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

/// 在读取配置之前调用（重复调用只保留第一次）。
pub fn set_config_dir(dir: PathBuf) {
    let _ = DIR_OVERRIDE.set(dir);
}

impl Config {
    /// 配置与证书目录。优先级：启动参数 > `FEIGECRAFT_CONFIG_DIR` > 系统配置目录。
    pub fn config_dir() -> PathBuf {
        if let Some(dir) = DIR_OVERRIDE.get() {
            return dir.clone();
        }
        if let Ok(dir) = std::env::var("FEIGECRAFT_CONFIG_DIR") {
            if !dir.trim().is_empty() {
                return PathBuf::from(dir);
            }
        }
        dirs::config_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("FeigeCraft")
    }

    pub fn path() -> PathBuf {
        Self::config_dir().join("config.json")
    }

    /// 读取配置，缺失或损坏时返回默认值。
    pub fn load() -> Self {
        match fs::read_to_string(Self::path()) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|err| {
                tracing::warn!("配置文件解析失败，使用默认配置：{err}");
                Config::default()
            }),
            Err(_) => Config::default(),
        }
    }

    pub fn save(&self) -> Result<()> {
        let dir = Self::config_dir();
        fs::create_dir_all(&dir).context("创建配置目录失败")?;
        let text = serde_json::to_string_pretty(self)?;
        fs::write(Self::path(), text).context("写入配置文件失败")?;
        Ok(())
    }

    /// 保证下载目录存在。
    /// 显示名：有备注用备注，否则用对方广播的名字。
    pub fn remark_for(&self, peer: &str) -> Option<&str> {
        self.remarks.get(peer).map(String::as_str).filter(|name| !name.trim().is_empty())
    }

    pub fn ensure_download_dir(&self) -> Result<()> {
        fs::create_dir_all(&self.download_dir).context("创建下载目录失败")?;
        Ok(())
    }
}
