//! 界面多语言。
//!
//! key 直接取中文原文：`tr("设置")`。英文表里没有的条目自动回退到中文，
//! 这样新增一处文案不用先去注册 key，也不会出现「忘了翻译就冒出一串 key」的尴尬。
//! 需要插值的句子把 `{}` 留在原文里，用 `tf(tr("在线 · {}"), &[&ip])` 拼，
//! 参数按占位符出现顺序排列。

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU8, Ordering};

use feigecraft_core::model::DeviceType;

/// 目前支持的界面语言。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    ZhCn,
    En,
}

impl Lang {
    /// 写进 `config.json` 的值。
    pub fn key(self) -> &'static str {
        match self {
            Lang::ZhCn => "zh-CN",
            Lang::En => "en",
        }
    }

    /// 语言选择器里的写法：一律用母语名，不随界面语言变。
    pub fn native_name(self) -> &'static str {
        match self {
            Lang::ZhCn => "简体中文",
            Lang::En => "English",
        }
    }
}

const ZH: u8 = 0;
const EN: u8 = 1;

static ACTIVE: AtomicU8 = AtomicU8::new(ZH);

pub fn set(lang: Lang) {
    ACTIVE.store(if lang == Lang::En { EN } else { ZH }, Ordering::Relaxed);
}

pub fn current() -> Lang {
    if ACTIVE.load(Ordering::Relaxed) == EN {
        Lang::En
    } else {
        Lang::ZhCn
    }
}

/// 配置里的 `language` → 实际语言：`system` 按系统界面语言猜，其余按值取。
pub fn resolve(pref: &str) -> Lang {
    match pref {
        "zh-CN" | "zh-cn" | "zh" => Lang::ZhCn,
        "en" | "en-US" | "en-us" => Lang::En,
        _ => system_lang(),
    }
}

/// 中文原文 → 当前语言。找不到译文时原样返回中文。
pub fn tr(zh: &'static str) -> &'static str {
    if current() == Lang::En {
        translate(zh)
    } else {
        zh
    }
}

/// 带占位符的翻译：按顺序把 `{}` 换成实参。
pub fn tf(template: &str, args: &[&dyn std::fmt::Display]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    let mut index = 0;
    while let Some(brace) = rest.find("{}") {
        out.push_str(&rest[..brace]);
        if let Some(arg) = args.get(index) {
            let _ = write!(out, "{arg}");
        }
        index += 1;
        rest = &rest[brace + 2..];
    }
    out.push_str(rest);
    out
}

/// 设备类型名：协议里的枚举不走界面语言，只有显示时才翻译。
pub fn device_label(kind: DeviceType) -> &'static str {
    match (kind, current()) {
        (DeviceType::Mobile, Lang::ZhCn) => "手机",
        (DeviceType::Mobile, Lang::En) => "Phone",
        (DeviceType::Desktop, Lang::ZhCn) => "电脑",
        (DeviceType::Desktop, Lang::En) => "Computer",
        (DeviceType::Web, Lang::ZhCn) => "网页",
        (DeviceType::Web, Lang::En) => "Web",
        (DeviceType::Headless, Lang::ZhCn) => "服务端",
        (DeviceType::Headless, Lang::En) => "Headless",
        (DeviceType::Server, Lang::ZhCn) => "服务器",
        (DeviceType::Server, Lang::En) => "Server",
    }
}

// ---- 系统语言探测 --------------------------------------------------------

#[cfg(target_os = "windows")]
fn system_lang() -> Lang {
    // 不为这一个调用引入 windows 依赖：GetUserDefaultUILanguage 是 kernel32 的稳定导出。
    #[link(name = "kernel32")]
    extern "system" {
        fn GetUserDefaultUILanguage() -> u16;
    }
    // LANGID 低 10 位是主语言 ID，0x04 = 中文。
    let id = unsafe { GetUserDefaultUILanguage() } & 0x3ff;
    if id == 0x04 {
        Lang::ZhCn
    } else {
        Lang::En
    }
}

#[cfg(not(target_os = "windows"))]
fn system_lang() -> Lang {
    for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(value) = std::env::var(key) {
            let value = value.to_lowercase();
            if value.starts_with("zh") {
                return Lang::ZhCn;
            }
            if value.starts_with("en") {
                return Lang::En;
            }
        }
    }
    Lang::En
}

// ---- 英文表 --------------------------------------------------------------
//
// 只放真正的界面文案：演示模式里的假聊天内容、示例文件名保持原样，
// 那些是「用户的聊天记录」，不该被翻译。

fn translate(zh: &'static str) -> &'static str {
    match zh {
        // 窗口 / 通用按钮
        "关闭" => "Close",
        "取消" => "Cancel",
        "保存" => "Save",
        "复制" => "Copy",
        "已复制" => "Copied",
        "设置" => "Settings",
        "更多" => "More",
        "收起" => "Close",
        "选择…" => "Browse…",
        "拒绝" => "Decline",
        "接受" => "Accept",
        "发送" => "Send",
        "连接" => "Connect",
        "文件" => "File",
        "表情" => "Emoji",
        "随机一个" => "Random",
        "电脑名" => "Computer name",
        // 会话列表
        "搜索设备或聊天记录" => "Search devices or chats",
        "附近设备 · {}" => "Nearby · {}",
        "局域网在线" => "On the LAN",
        "未知设备" => "Unknown device",
        "还没有发现设备" => "No devices found yet",
        "让对方的设备打开 FeigeCraft，\n或点右上角 ＋ 输入 IP" => {
            "Ask them to open FeigeCraft,\nor tap + at the top right to enter an IP"
        }
        "扫描局域网" => "Scan local network",
        "正在扫描局域网…" => "Scanning the local network…",
        "没有扫描到设备，请检查防火墙或手动输入 IP" => {
            "No devices found — check the firewall or enter an IP manually"
        }
        "手动连接" => "Connect manually",
        "手动输入 IP 连接" => "Connect by IP",
        // 聊天区
        "修改备注" => "Edit nickname",
        "复制对方 IP" => "Copy IP",
        "清空聊天记录" => "Clear chat history",
        "发送文件" => "Send files",
        "已粘贴图片，点发送即可发出" => "Image pasted — tap Send to deliver it",
        "读取剪贴板图片失败：{}" => "Failed to read clipboard image: {}",
        "选择左侧设备，开始聊天或发送文件" => "Pick a device on the left to chat or send files",
        "在线 · 点击开始聊天" => "Online · click to chat",
        "在线 · {} · {}" => "Online · {} · {}",
        "离线 · 消息会保留在本地" => "Offline · messages stay on this device",
        "离线" => "Offline",
        "当前离线" => "Offline now",
        "局域网直连 · 消息不经过服务器" => "Direct over LAN · no server involved",
        "发消息…（Enter 发送，Shift+Enter 换行）" => {
            "Message…  (Enter to send, Shift+Enter for a new line)"
        }
        "{} 字" => "{} chars",
        "今天" => "Today",
        "昨天" => "Yesterday",
        "{}月{}日" => "{}/{}",
        "{}年{}月{}日" => "{}/{}/{}",
        "常用表情" => "Frequently used",
        // 会话列表 / 消息摘要
        "{}：{}" => "{}: {}",
        "{} 想发送 {} 个文件" => "{} wants to send {} file(s)",
        "{} 想发送文件，等待确认" => "{} wants to send files",
        "[文件] 对方想发送 {} 个文件" => "[File] They want to send {} file(s)",
        "… 还有 {} 个文件" => "… and {} more files",
        "共 {} · 保存在接收目录" => "Total {} · saved to your receive folder",
        "打开文件所在目录" => "Open containing folder",
        "打开文件" => "Open file",
        "复制文件名" => "Copy file name",
        "取消传输" => "Cancel transfer",
        "双击打开文件 · 右键打开所在目录" => {
            "Double-click to open · right-click to reveal"
        }
        "双击打开文件 · 单击取消传输 · 右键打开所在目录" => {
            "Double-click to open · click to cancel · right-click to reveal"
        },
        // 传输状态
        "发送中" => "Sending",
        "发送完成" => "Sent",
        "发送中断" => "Interrupted",
        "接收中" => "Receiving",
        "接收中断" => "Interrupted",
        "已发送 {} 个文件 · {}" => "Sent {} files · {}",
        "已接收 {} 个文件 · {}" => "Received {} files · {}",
        "传输完成：{} 个文件 · {}" => "Transfer complete: {} files · {}",
        "已拒绝来自 {} 的 {} 个文件" => "Declined {} file(s) from {}",
        "对方已离线，暂时发不出去" => "They are offline — not sent",
        "对方已离线，消息会立即尝试投递" => "They are offline — will retry shortly",
        "对方已离线，无法发送文件" => "They are offline — can't send files",
        "先选择一个设备" => "Select a device first",
        "先选择一个设备，再发送文件" => "Select a device before sending files",
        // 状态 / 提示
        "已就绪 · 监听 {}" => "Ready · listening on {}",
        "实例 {} · 监听 {}" => "Instance {} · listening on {}",
        "服务启动失败，请查看日志" => "Service failed to start — check the log",
        "在线 · 端口 {}" => "Online · port {}",
        "已连接到 {}" => "Connected to {}",
        "连接失败：{}" => "Connection failed: {}",
        "发送失败：{}" => "Send failed: {}",
        "保存失败：{}" => "Save failed: {}",
        "地址解析失败：{}" => "Invalid address: {}",
        "备注保存失败：{}" => "Could not save nickname: {}",
        "扫描完成 · 当前在线 {} 台" => "Scan done · {} online",
        "扫描完成，新发现 {} 台设备" => "Scan done · {} new device(s)",
        "正在扫描网段 {}…" => "Scanning subnet {}…",
        "发现服务未启动，无法扫描网段" => "Discovery is not running — can't scan the subnet",
        "设置已保存" => "Settings saved",
        // 设置 - 导航
        "外观" => "Appearance",
        "身份" => "Identity",
        "传输" => "Transfer",
        "关于" => "About",
        // 设置 - 页签副标题
        "主题与配色" => "Theme & colors",
        "别人看到的名字" => "The name others see",
        "保存位置与确认" => "Destination & confirmation",
        "版本与身份指纹" => "Version & fingerprint",
        // 设置 - 外观
        "界面语言" => "Language",
        "跟随系统" => "System",
        "浅色" => "Light",
        "深色" => "Dark",
        "卡片就是主题的实际配色。选「跟随系统」时，切换 Windows 深浅色会立刻跟着变。" => {
            "Each card previews its real colors. With System, it follows Windows light/dark."
        }
        "选择界面显示的语言，切换后立即生效。" => {
            "Choose the display language. Changes apply immediately."
        }
        // 设置 - 身份
        "我的用户名" => "My name",
        "别人在会话列表里看到的名字" => "Name shown in others' chat list",
        "默认名是启动时随机生成的，避免和别人撞名；保存后立刻广播，不用重启。" => {
            "The default name is randomized to avoid clashes; saved changes are broadcast immediately."
        }
        "设备类型" => "Device type",
        "对方看到的图标会跟着这个类型变化。" => "The icon others see follows this type.",
        "备注名" => "Nickname",
        "给 {} 起个名字" => "Nickname for {}",
        "只在本机生效，对方看到的仍然是他自己的名字。" => {
            "Only applies to this device — they still see their own name."
        }
        "清除备注" => "Clear",
        // 设置 - 传输
        "收到的文件保存到" => "Save received files to",
        "接收方式" => "Receiving",
        "自动接受文件" => "Auto-accept files",
        "关闭时，每次传输都要先点一次「接受」" => "When off, you confirm every transfer first",
        "关闭窗口时" => "When closing the window",
        "留在后台继续接收" => "Keep receiving in the background",
        "关闭窗口不退出程序，仍可收发文件；再双击程序即可唤回窗口" => {
            "The app keeps running so files still arrive. Run it again to bring the window back."
        }
        "已转入后台继续接收，再双击程序即可重新打开" => {
            "Still running in the background — run the app again to reopen."
        }
        "退出" => "Quit",
        // 托盘
        "显示主窗口" => "Show window",
        "{} 条未读" => "{} unread",
        "{} 个文件，点开确认" => "{} files, click to confirm",
        // 设置 - 关于
        "版本" => "Version",
        "实例" => "Instance",
        "实例 {}" => "Instance {}",
        "主实例" => "Primary",
        "监听端口" => "Port",
        "配置目录" => "Config folder",
        "本机指纹" => "This device's fingerprint",
        "对方首次连接时用指纹校验身份，指纹不变即可放心。" => {
            "Identity is pinned by certificate fingerprint on first connect."
        }
        // 手动连接弹窗
        "手动输入 IP 或网段" => "Enter an IP or subnet",
        "请输入对方 IP 或网段" => "Enter an IP or subnet",
        "192.168.1.23 或 192.168.2.0/24" => "192.168.1.23 or 192.168.2.0/24",
        "跨网段或交换机拦截组播时用这里：填 IP 直接连，填 192.168.2.0/24 扫描整段。" => {
            "Use this across subnets or when multicast is blocked: an IP connects directly, 192.168.2.0/24 scans a range."
        }
        "网段格式不对，例如 192.168.2.0/24" => "Invalid subnet — try 192.168.2.0/24",
        "扫描网段" => "Scan subnet",
        "版本 {}" => "Version {}",
        // 演示模式（截图 / 走查）用的示例数据：真实使用时不会出现，
        // 但英文截图里如果混着中文会显得没翻完，所以一并给出。
        "已就绪 · 3 台设备在线" => "Ready · 3 devices online",
        "已发送 1 个文件 · 173.6 MB" => "Sent 1 file · 173.6 MB",
        "小米 14 Pro" => "Xiaomi 14 Pro",
        "家庭 NAS" => "Home NAS",
        "小王" => "Alex",
        "设计组的 Mac" => "Design Mac",
        "设计稿_v3_final.zip" => "design_v3_final.zip",
        "会议纪要.pdf" => "meeting-notes.pdf",
        "录屏 2026-11-04.mp4" => "recording 2026-11-04.mp4",
        "到家了？路由器好像又抽风" => "Home yet? The router is acting up again",
        "刚到，Wi-Fi 连上了，我看看" => "Just got in, Wi-Fi is up — let me check",
        "先把设计稿发我一份，客户下午要看" => "Send me the design first, client review is this afternoon",
        "好，这就传 👍" => "Sure, sending now 👍",
        "收到，我下午三点前把改好的发你" => "Got it — I'll send the revision before 3pm",
        "服务器那台机器我重启完了" => "I finished rebooting the server box",
        // 关于页的宣传语
        "同一 Wi-Fi 下会自动发现，文件点对点直传，不经过服务器" => {
            "Auto-discovered on the same network · peer-to-peer transfers, no server"
        }
        "对方 IP" => "Their IP",
        _ => zh,
    }
}
