//! 杂项工具：文件名清洗、大小格式化、本机地址枚举等。

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

/// 人类可读的字节数。
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{:.1} {}", size, UNITS[unit])
    }
}

/// 传输速率（字节/秒）。
pub fn human_speed(bytes_per_sec: f64) -> String {
    format!("{}/s", human_size(bytes_per_sec.max(0.0) as u64))
}

/// 默认下载目录：`<系统下载目录>/FeigeCraft`，退化到主目录。
pub fn default_download_dir() -> PathBuf {
    let base = dirs::download_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("FeigeCraft")
}

/// 清洗对方传来的文件名，避免路径穿越与非法字符。
pub fn sanitize_file_name(name: &str) -> String {
    let name = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = name
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').to_string();
    if cleaned.is_empty() {
        return "unnamed".to_string();
    }
    if cleaned.chars().count() > 180 {
        cleaned.chars().take(180).collect()
    } else {
        cleaned
    }
}

/// 若目标文件已存在，生成 `name (1).ext` 形式的唯一路径。
pub fn unique_path(dir: &Path, file_name: &str) -> PathBuf {
    let file_name = sanitize_file_name(file_name);
    let candidate = dir.join(&file_name);
    if !candidate.exists() {
        return candidate;
    }
    let path = Path::new(&file_name);
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| file_name.clone());
    let ext = path.extension().map(|s| s.to_string_lossy().to_string());
    for index in 1..10_000 {
        let name = match &ext {
            Some(ext) => format!("{stem} ({index}).{ext}"),
            None => format!("{stem} ({index})"),
        };
        let candidate = dir.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("{stem}-{}", uuid::Uuid::new_v4()))
}

/// 根据扩展名猜测 MIME 类型，覆盖常见的文件种类。
pub fn guess_mime(path: &Path) -> String {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "heic" => "image/heic",
        "mp4" => "video/mp4",
        "mkv" => "video/x-matroska",
        "mov" => "video/quicktime",
        "avi" => "video/x-msvideo",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "m4a" => "audio/mp4",
        "ogg" => "audio/ogg",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "7z" => "application/x-7z-compressed",
        "rar" => "application/vnd.rar",
        "tar" => "application/x-tar",
        "gz" => "application/gzip",
        "txt" => "text/plain",
        "md" => "text/markdown",
        "csv" => "text/csv",
        "json" => "application/json",
        "xml" => "application/xml",
        "html" | "htm" => "text/html",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "exe" => "application/vnd.microsoft.portable-executable",
        "apk" => "application/vnd.android.package-archive",
        "dmg" => "application/x-apple-diskimage",
        "iso" => "application/x-iso9660-image",
        _ => "application/octet-stream",
    };
    mime.to_string()
}

/// 枚举本机所有 IPv4 地址（排除回环），用于子网扫描兜底。
pub fn local_ipv4() -> Vec<(Ipv4Addr, Ipv4Addr)> {
    let mut result = Vec::new();
    if let Ok(ifaces) = if_addrs::get_if_addrs() {
        for iface in ifaces {
            if iface.is_loopback() {
                continue;
            }
            if let if_addrs::IfAddr::V4(v4) = iface.addr {
                result.push((v4.ip, v4.netmask));
            }
        }
    }
    result
}

/// 解析用户输入的网段写法，用于跨网段手动扫描。
///
/// 支持 `192.168.2.0/24`、`192.168.2.15/24`（按网络地址处理）和
/// `192.168.2.` 这种省略掩码的前缀写法（等价 /24）。
pub fn parse_subnet_spec(spec: &str) -> Option<(Ipv4Addr, Ipv4Addr)> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }
    if let Some(prefix) = spec.strip_suffix('.') {
        let base: Ipv4Addr = format!("{prefix}.0").parse().ok()?;
        return Some((base, Ipv4Addr::new(255, 255, 255, 0)));
    }
    let (addr, len) = match spec.split_once('/') {
        Some((addr, len)) => (
            addr.trim().parse::<Ipv4Addr>().ok()?,
            len.trim().parse::<u8>().ok()?,
        ),
        None => (spec.parse::<Ipv4Addr>().ok()?, 24),
    };
    // /8 太小会扫出上千万个地址，直接拒绝；/31、/32 没有可用主机。
    if !(16..=30).contains(&len) {
        return None;
    }
    let mask = u32::MAX << (32 - len);
    Some((Ipv4Addr::from(u32::from(addr) & mask), Ipv4Addr::from(mask)))
}

/// 由 IP + 掩码推导同网段待扫描地址（最多 1022 个，避免 /16 卡死）。
pub fn subnet_hosts(ip: Ipv4Addr, netmask: Ipv4Addr) -> Vec<Ipv4Addr> {
    let ip_u = u32::from(ip);
    let mask = u32::from(netmask);
    if mask == 0 {
        return Vec::new();
    }
    let network = ip_u & mask;
    let broadcast = network | !mask;
    let count = broadcast.saturating_sub(network).saturating_sub(1);
    if count == 0 || count > 1022 {
        // 不是 /22 及更小的网段，只扫描所在 /24
        let base = ip_u & 0xFFFF_FF00;
        return (1..=254u32)
            .map(|i| Ipv4Addr::from(base | i))
            .filter(|candidate| *candidate != ip)
            .collect();
    }
    (network + 1..broadcast)
        .map(Ipv4Addr::from)
        .filter(|candidate| *candidate != ip)
        .collect()
}

/// 随机用户名的词表：形容词 + 名词，好念、不容易重样。
///
/// 之前默认直接用 `COMPUTERNAME`，一台机器一个名字看着像设备清单，
/// 而且同型号的机器会撞名（一堆 `DESKTOP-XXXX`）。参考 LocalSend 的做法，
/// 首次启动随机取一个，用户可以在设置里改。
const ALIAS_ADJECTIVES_ZH: &[&str] = &[
    "轻快的",
    "安静的",
    "蓝色的",
    "灵巧的",
    "热心的",
    "明亮的",
    "温和的",
    "敏捷的",
    "踏实的",
    "清亮的",
    "慢悠悠的",
    "不打盹的",
];

const ALIAS_NOUNS_ZH: &[&str] = &[
    "纸飞机", "海豚", "橘子", "风筝", "海鸥", "小熊", "柠檬", "帆船", "山雀", "橡果", "信鸽",
    "咖啡", "云朵", "灯塔", "萤火虫",
];

const ALIAS_ADJECTIVES_EN: &[&str] = &[
    "Swift", "Quiet", "Blue", "Clever", "Eager", "Bright", "Gentle", "Nimble", "Steady", "Lively",
    "Dreamy", "Busy",
];

const ALIAS_NOUNS_EN: &[&str] = &[
    "Paper Plane", "Dolphin", "Tangerine", "Kite", "Gull", "Bear", "Lemon", "Sailboat", "Titmouse",
    "Acorn", "Pigeon", "Coffee", "Cloud", "Lighthouse", "Firefly",
];

/// 随机生成一个显示名（首次启动时写进 config.json，之后固定不变）。
/// `lang` 传界面语言（`zh-CN` / `en`），昵称跟着语言走；其余取值回落中文。
pub fn random_alias_lang(lang: &str) -> String {
    let bytes = uuid::Uuid::new_v4();
    let bytes = bytes.as_bytes();
    let english = lang.eq_ignore_ascii_case("en") || lang.to_ascii_lowercase().starts_with("en-");
    let (adjectives, nouns, space) = if english {
        (ALIAS_ADJECTIVES_EN, ALIAS_NOUNS_EN, " ")
    } else {
        (ALIAS_ADJECTIVES_ZH, ALIAS_NOUNS_ZH, "")
    };
    let adjective =
        adjectives[(bytes[0] as usize * 7 + bytes[1] as usize) % adjectives.len()];
    let noun = nouns[(bytes[2] as usize * 11 + bytes[3] as usize) % nouns.len()];
    format!("{adjective}{space}{noun}")
}

/// 兼容旧调用：默认中文词库。
pub fn random_alias() -> String {
    random_alias_lang("zh-CN")
}

/// 本机主机名，设置页「使用电脑名」用。
pub fn host_alias() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|host| !host.trim().is_empty())
        .unwrap_or_else(|| "FeigeCraft".to_string())
}

/// 默认用户名：随机取一个，避免同名设备在列表里无法区分。
pub fn default_alias() -> String {
    random_alias()
}

/// 同一台设备可能在多个网段各广播一次（有线 + 无线 + 虚拟机网卡）。
/// 优先选与本机某个接口同网段的地址；两个都一样时保持原来的，
/// 免得地址在两条广播之间来回跳。
pub fn prefer_lan_ip(
    current: std::net::IpAddr,
    incoming: std::net::IpAddr,
    local: &[(Ipv4Addr, Ipv4Addr)],
) -> std::net::IpAddr {
    let on_lan = |ip: std::net::IpAddr| match ip {
        std::net::IpAddr::V4(v4) => {
            local.iter().any(|(host, mask)| same_subnet(v4, *host, *mask))
        }
        std::net::IpAddr::V6(_) => false,
    };
    match (on_lan(current), on_lan(incoming)) {
        (false, true) => incoming,
        _ => current,
    }
}

/// 两个地址是否在同一网段。
pub fn same_subnet(a: Ipv4Addr, b: Ipv4Addr, mask: Ipv4Addr) -> bool {
    let mask = u32::from(mask);
    (u32::from(a) & mask) == (u32::from(b) & mask)
}

/// 打开系统文件管理器定位到指定路径。
pub fn reveal_path(path: &Path) {
    let target = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| path.to_path_buf())
    };
    let _ = open::that(target);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_size_formats_units() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1024 * 1024 * 3 / 2), "1.5 MB");
        assert_eq!(human_size(5 * 1024 * 1024 * 1024), "5.0 GB");
    }

    #[test]
    fn sanitize_strips_paths_and_illegal_chars() {
        assert_eq!(sanitize_file_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_file_name("C://Windows//a.txt"), "a.txt");
        assert_eq!(sanitize_file_name("a<b>c:d\"e|f?g*h.txt"), "abcdefg h.txt".replace(' ', ""));
        assert_eq!(sanitize_file_name("   "), "unnamed");
        assert_eq!(sanitize_file_name(".."), "unnamed");
        assert_eq!(sanitize_file_name("中文 文件名.txt"), "中文 文件名.txt");
    }

    #[test]
    fn unique_path_adds_suffix() {
        let dir = std::env::temp_dir().join(format!("feige-util-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let first = unique_path(&dir, "a.txt");
        assert_eq!(first.file_name().unwrap(), "a.txt");
        std::fs::write(&first, b"x").unwrap();
        let second = unique_path(&dir, "a.txt");
        assert_eq!(second.file_name().unwrap(), "a (1).txt");
        std::fs::write(&second, b"x").unwrap();
        let third = unique_path(&dir, "a.txt");
        assert_eq!(third.file_name().unwrap(), "a (2).txt");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_subnet_spec_supports_cidr_and_prefix() {
        let (net, mask) = parse_subnet_spec("192.168.2.0/24").unwrap();
        assert_eq!(net, "192.168.2.0".parse::<Ipv4Addr>().unwrap());
        assert_eq!(mask, "255.255.255.0".parse::<Ipv4Addr>().unwrap());

        // 主机地址按网络地址处理
        let (net, _) = parse_subnet_spec("10.1.5.6/16").unwrap();
        assert_eq!(net, "10.1.0.0".parse::<Ipv4Addr>().unwrap());

        // 省略掩码的前缀写法
        let (net, mask) = parse_subnet_spec("172.16.8.").unwrap();
        assert_eq!(net, "172.16.8.0".parse::<Ipv4Addr>().unwrap());
        assert_eq!(mask, "255.255.255.0".parse::<Ipv4Addr>().unwrap());

        assert!(parse_subnet_spec("192.168.1.2").is_some());
        assert!(parse_subnet_spec("192.168.1.0/8").is_none());
        assert!(parse_subnet_spec("not-an-ip").is_none());
    }

    #[test]
    fn subnet_hosts_handles_slash24_and_large() {
        let hosts = subnet_hosts("192.168.1.10".parse().unwrap(), "255.255.255.0".parse().unwrap());
        assert_eq!(hosts.len(), 253); // .1 ~ .254 去掉自己
        assert!(!hosts.contains(&"192.168.1.10".parse().unwrap()));

        // /16 太大时退回所在 /24
        let hosts = subnet_hosts("10.1.5.6".parse().unwrap(), "255.255.0.0".parse().unwrap());
        assert_eq!(hosts.len(), 253);
        assert!(hosts.iter().all(|ip| ip.octets()[2] == 5));
    }

    #[test]
    fn random_alias_is_non_empty_and_varies() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..40 {
            let alias = random_alias();
            assert!(!alias.is_empty());
            assert!(alias.chars().count() >= 4);
            seen.insert(alias);
        }
        // 40 次里至少要有若干个不同结果，否则随机性等于没有
        assert!(seen.len() > 10, "随机别名重复过多: {}", seen.len());
    }

    #[test]
    fn prefer_lan_ip_keeps_reachable_address() {
        let local = vec![("192.168.1.10".parse().unwrap(), "255.255.255.0".parse().unwrap())];
        let lan: std::net::IpAddr = "192.168.1.20".parse().unwrap();
        let other: std::net::IpAddr = "10.8.0.3".parse().unwrap();
        // 只有新地址同网段时切换
        assert_eq!(prefer_lan_ip(other, lan, &local), lan);
        // 旧地址已经同网段，就不再改
        assert_eq!(prefer_lan_ip(lan, other, &local), lan);
        // 两个都不同网段时保持原样
        assert_eq!(prefer_lan_ip(other, "10.9.0.1".parse().unwrap(), &local), other);
    }

    #[test]
    fn mime_guessing_covers_common_types() {
        assert_eq!(guess_mime(Path::new("a.PNG")), "image/png");
        assert_eq!(guess_mime(Path::new("a.mkv")), "video/x-matroska");
        assert_eq!(guess_mime(Path::new("noext")), "application/octet-stream");
    }
}
