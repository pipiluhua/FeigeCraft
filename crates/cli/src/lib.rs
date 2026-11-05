//! FeigeCraft 的命令行能力。
//!
//! 这里不再是一个独立的可执行文件，而是作为库挂在 `feigecraft.exe` 上：
//! 主程序发现命令行带了子命令，就把控制权交给 [`run`]，否则照常开窗口。
//! 这样一个 exe 既能双击用，也能写进脚本、开机启动、跑在无桌面的机器上。
//!
//! ```text
//! feigecraft                        # 打开窗口（默认）
//! feigecraft serve                  # 后台常驻收文件，不开窗口
//! feigecraft send a.jpg b.zip --to 客厅电脑
//! feigecraft chat "文件放桌上了" --to 192.168.1.23
//! feigecraft discover               # 扫一遍局域网
//! feigecraft info                   # 本机身份 / 端口 / 指纹
//! ```

use std::io::Write;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use clap::{Args, Parser, Subcommand};
use feigecraft_core::client::{self, collect_files, send_files};
use feigecraft_core::config::Config;
use feigecraft_core::events::AppEvent;
use feigecraft_core::model::{Direction, DiscoveredDevice, DeviceType, DEFAULT_PORT};
use feigecraft_core::node::Node;
use feigecraft_core::util::{human_size, human_speed};
use tokio::sync::watch;

/// 子命令名。主程序靠它区分「开窗口」和「跑命令行」。
const SUBCOMMANDS: [&str; 6] = ["serve", "send", "chat", "discover", "info", "help"];

/// 只带这些选项时仍然开窗口，而不是进命令行。
/// `--config-dir` / `--port` / `--alias` / `--instance` 是窗口模式自己的参数。
fn is_gui_option(arg: &str) -> bool {
    matches!(
        arg,
        "--config-dir" | "--port" | "--alias" | "--instance" | "--new"
    ) || arg.starts_with("--config-dir=")
        || arg.starts_with("--port=")
        || arg.starts_with("--alias=")
        || arg.starts_with("--instance=")
}

/// 这次启动该走命令行吗？
///
/// 判断依据是「第一个不是选项的词是不是子命令」。这样
/// `feigecraft --config-dir X send a.jpg` 和 `feigecraft send a.jpg` 都能识别，
/// 而 `feigecraft --port 53400` 依旧开窗口。
pub fn wants_cli() -> bool {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        match arg {
            "--help" | "-h" | "--version" | "-V" => return true,
            _ if is_gui_option(arg) => {
                // 带值的选项要连值一起跳过，否则 `--alias serve` 会被当成子命令。
                index += if arg.contains('=') { 1 } else { 2 };
            }
            _ if arg.starts_with('-') => index += 1,
            _ => return SUBCOMMANDS.contains(&arg),
        }
    }
    false
}

#[derive(Parser, Debug)]
#[command(
    name = "feigecraft",
    bin_name = "feigecraft",
    version,
    about = "FeigeCraft · 局域网极速传文件（Rust 实现，兼容 LocalSend 协议）",
    long_about = "不带子命令时打开图形窗口。\n\
                  子命令用于脚本、服务器和没有桌面的环境。",
    disable_help_subcommand = true
)]
struct Cli {
    /// 配置 / 证书 / 聊天记录所在目录（默认 %APPDATA%\FeigeCraft）
    #[arg(long, global = true, value_name = "目录")]
    config_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// 后台常驻接收文件（不开窗口，默认全部自动接受）
    Serve(ServeArgs),
    /// 发送文件或文件夹
    Send(SendArgs),
    /// 发送一条局域网聊天消息（FeigeCraft 扩展协议）
    Chat(ChatArgs),
    /// 扫描并列出局域网内的设备
    Discover(DiscoverArgs),
    /// 显示本机信息（别名、IP、端口、证书指纹）
    Info,
}

#[derive(Args, Debug)]
struct ServeArgs {
    /// 设备显示名（默认取配置）
    #[arg(long)]
    alias: Option<String>,
    /// 文件保存目录
    #[arg(long)]
    dir: Option<PathBuf>,
    /// 覆盖监听端口
    #[arg(long)]
    port: Option<u16>,
    /// 设备类型: desktop/mobile/web/headless/server
    #[arg(long, default_value = "server")]
    device_type: String,
    /// 每个请求都先在终端问一句（默认全自动）
    #[arg(long)]
    ask: bool,
}

#[derive(Args, Debug)]
struct SendArgs {
    /// 要发送的文件或目录
    #[arg(required = true)]
    paths: Vec<PathBuf>,
    /// 目标设备：别名、IP 或指纹
    #[arg(long, short)]
    to: String,
    /// 目标端口（仅当 --to 是 IP 时生效）
    #[arg(long)]
    port: Option<u16>,
    /// 等待设备出现的最长秒数
    #[arg(long, default_value_t = 8)]
    wait: u64,
}

#[derive(Args, Debug)]
struct ChatArgs {
    /// 消息内容
    #[arg(required = true)]
    text: Vec<String>,
    /// 目标设备：别名、IP 或指纹
    #[arg(long, short)]
    to: String,
    /// 目标端口（仅当 --to 是 IP 时生效）
    #[arg(long)]
    port: Option<u16>,
    /// 等待设备出现的最长秒数
    #[arg(long, default_value_t = 8)]
    wait: u64,
}

#[derive(Args, Debug)]
struct DiscoverArgs {
    /// 等待组播回应的秒数
    #[arg(long, default_value_t = 4)]
    timeout: u64,
    /// 组播无响应时扫描本机子网（较慢）
    #[arg(long)]
    scan: bool,
}

/// 跑完一个子命令。返回 `Ok` 表示正常结束，调用方据此决定退出码。
pub fn run() -> Result<()> {
    // 命令行默认只报警告：`init_logging` 的兜底级别带 DEBUG，
    // 屏幕上会混进一堆组播日志，把真正的输出淹掉。
    // 想看细节就自己设 `FEIGECRAFT_LOG=debug`。
    if std::env::var_os("FEIGECRAFT_LOG").is_none() && std::env::var_os("RUST_LOG").is_none() {
        std::env::set_var("FEIGECRAFT_LOG", "warn");
    }
    feigecraft_core::init_logging();
    let cli = Cli::parse();

    // 必须在任何 `Config::load()` 之前设置，否则会先读到默认目录。
    if let Some(dir) = cli.config_dir {
        feigecraft_core::config::set_config_dir(dir);
    }

    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async move {
        match cli.command {
            Command::Serve(args) => run_serve(args).await,
            Command::Send(args) => run_send(args).await,
            Command::Chat(args) => run_chat(args).await,
            Command::Discover(args) => run_discover(args).await,
            Command::Info => run_info().await,
        }
    })
}

fn parse_device_type(value: &str) -> DeviceType {
    match value.to_ascii_lowercase().as_str() {
        "mobile" => DeviceType::Mobile,
        "web" => DeviceType::Web,
        "headless" => DeviceType::Headless,
        "server" => DeviceType::Server,
        _ => DeviceType::Desktop,
    }
}

fn banner(alias: &str, port: u16, fingerprint: &str) {
    println!();
    println!("  ╭──────────────────────────────────────────────╮");
    println!("  │   FeigeCraft  (Rust / LocalSend 协议)         │");
    println!("  ╰──────────────────────────────────────────────╯");
    println!("  设备名  : {alias}");
    println!("  端口    : {port}");
    println!("  指纹    : {}…", &fingerprint[..fingerprint.len().min(16)]);
    for (ip, _) in feigecraft_core::util::local_ipv4() {
        println!("  本机 IP : {ip}");
    }
    println!();
}

async fn run_serve(args: ServeArgs) -> Result<()> {
    let mut config = Config::load();
    if let Some(alias) = args.alias {
        config.alias = alias;
    }
    if let Some(dir) = args.dir {
        config.download_dir = dir;
    }
    if let Some(port) = args.port {
        config.port = port;
    }
    config.device_type = parse_device_type(&args.device_type);
    // 后台模式默认全自动：没人守着终端，问也没人答。
    let auto_accept = !args.ask;
    config.auto_accept = auto_accept;

    let node = Node::start(config.clone()).await?;
    let info = node.device_info();
    banner(&info.alias, info.port, &info.fingerprint);
    println!("  保存目录: {}", config.download_dir.display());
    println!("  接收方式: {}", if auto_accept { "自动接受" } else { "终端内确认" });
    println!("  按 Ctrl+C 退出。\n");

    let mut events = node.subscribe();
    let node_for_events = node.clone();
    let mut last_tick = Instant::now();

    loop {
        tokio::select! {
            event = events.recv() => {
                match event {
                    Ok(AppEvent::DeviceFound(device)) => {
                        println!("  [+] 发现设备 {} ({}:{})", device.info.alias, device.ip, device.info.port);
                    }
                    Ok(AppEvent::DeviceLost { fingerprint }) => {
                        println!("  [-] 设备离线 {fingerprint}");
                    }
                    Ok(AppEvent::IncomingRequest { session_id, sender, files, total_bytes }) => {
                        println!();
                        println!("  {} 想发送 {} 个文件（共 {}）", sender.alias, files.len(), human_size(total_bytes));
                        for file in files.iter().take(20) {
                            println!("      · {}  {}", file.file_name, human_size(file.size));
                        }
                        if files.len() > 20 {
                            println!("      … 以及另外 {} 个文件", files.len() - 20);
                        }
                        if auto_accept {
                            node_for_events.receiver.resolve(&session_id, true);
                            println!("  已自动接受");
                        } else {
                            print!("  是否接受？[y/N] ");
                            let _ = std::io::stdout().flush();
                            let answer = tokio::task::spawn_blocking(|| {
                                let mut line = String::new();
                                std::io::stdin().read_line(&mut line).ok();
                                line.trim().to_ascii_lowercase()
                            }).await.unwrap_or_default();
                            let accept = answer == "y" || answer == "yes" || answer == "是";
                            node_for_events.receiver.resolve(&session_id, accept);
                            println!("  {}", if accept { "已接受，开始接收…" } else { "已拒绝" });
                        }
                    }
                    Ok(AppEvent::FileProgress { direction: Direction::Receive, file_name, transferred: current, total, .. }) => {
                        if last_tick.elapsed() >= Duration::from_millis(300) {
                            last_tick = Instant::now();
                            print!("\r  ↓ {:<40} {:>6.1}%  {}", elide(&file_name, 40), percent(current, total), human_size(current));
                            let _ = std::io::stdout().flush();
                        }
                    }
                    Ok(AppEvent::FileFinished { file_name, path, size, direction: Direction::Receive, .. }) => {
                        println!("\r  ✓ {:<40} {}", elide(&file_name, 40), human_size(size));
                        if let Some(path) = path {
                            println!("     → {}", path.display());
                        }
                    }
                    Ok(AppEvent::SessionFinished { direction: Direction::Receive, files, bytes, .. }) => {
                        println!("  接收完成：{files} 个文件，{}", human_size(bytes));
                        println!();
                    }
                    Ok(AppEvent::SessionFailed { error, .. }) => {
                        println!("\r  ✗ {error}");
                    }
                    Ok(AppEvent::Log { message, .. }) => {
                        println!("  · {message}");
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => break,
                }
            }
            _ = tokio::signal::ctrl_c() => {
                println!("\n  已退出");
                break;
            }
        }
    }
    Ok(())
}

async fn run_discover(args: DiscoverArgs) -> Result<()> {
    let node = Node::start(Config::load()).await?;
    let info = node.device_info();
    println!("  本机：{}（端口 {}）", info.alias, info.port);
    println!("  正在搜索局域网设备…");

    let mut events = node.subscribe();
    let deadline = Instant::now() + Duration::from_secs(args.timeout);
    while Instant::now() < deadline {
        if let Ok(AppEvent::DeviceFound(device)) = events.try_recv() {
            println!(
                "  · {:<20} {:<24} {}",
                device.info.alias,
                format!("{}:{}", device.ip, device.info.port),
                device.info.device_type.map(|t| t.label()).unwrap_or("未知")
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let mut devices = node.devices();
    if devices.is_empty() && args.scan {
        println!("  组播无回应，开始扫描本机子网（约 10 秒）…");
        node.scan().await;
        devices = node.devices();
    }

    println!();
    if devices.is_empty() {
        println!("  没有发现设备。请确认对方已运行 FeigeCraft / LocalSend，且防火墙放行了 53317 端口。");
    } else {
        println!("  共发现 {} 台设备：", devices.len());
        for device in devices {
            println!(
                "    {:<20} {:<22} {}",
                device.info.alias,
                format!("{}:{}", device.ip, device.info.port),
                device.info.fingerprint.chars().take(12).collect::<String>()
            );
        }
    }
    Ok(())
}

async fn run_info() -> Result<()> {
    let config = Config::load();
    let dir = Config::config_dir();
    let identity = feigecraft_core::tls::Identity::load_or_create(&dir)?;
    println!();
    println!("  设备名    : {}", config.alias);
    println!("  设备类型  : {}", config.device_type.label());
    println!("  端口      : {}", config.port);
    println!("  保存目录  : {}", config.download_dir.display());
    println!("  自动接受  : {}", config.auto_accept);
    println!("  配置文件  : {}", Config::path().display());
    println!("  证书指纹  : {}", identity.fingerprint);
    println!("  本机 IPv4 :");
    for (ip, mask) in feigecraft_core::util::local_ipv4() {
        println!("      {ip} / {mask}");
    }
    println!();
    Ok(())
}

async fn run_send(args: SendArgs) -> Result<()> {
    let files = collect_files(&args.paths);
    if files.is_empty() {
        bail!("没有找到可发送的文件：{:?}", args.paths);
    }
    let total: u64 = files
        .iter()
        .filter_map(|f| std::fs::metadata(&f.path).ok())
        .map(|m| m.len())
        .sum();
    println!("  待发送 {} 个文件，共 {}", files.len(), human_size(total));

    let node = Node::start(Config::load()).await?;
    let target = resolve_target(&node, &args.to, args.port, args.wait).await?;
    println!("  目标设备：{} ({}:{})", target.info.alias, target.ip, target.info.port);

    let (cancel_tx, cancel_rx) = watch::channel(false);
    {
        let cancel_tx = cancel_tx.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                let _ = cancel_tx.send(true);
            }
        });
    }

    let mut events = node.subscribe();
    let printer = tokio::spawn(async move {
        let mut last_tick = Instant::now();
        let mut started = Instant::now();
        while let Ok(event) = events.recv().await {
            match event {
                AppEvent::SessionStarted { files, total_bytes, .. } => {
                    started = Instant::now();
                    println!("  开始传输 {files} 个文件（{}）…", human_size(total_bytes));
                }
                AppEvent::FileProgress { file_name, transferred, total, direction: Direction::Send, .. } => {
                    if last_tick.elapsed() >= Duration::from_millis(200) {
                        let speed = transferred as f64 / started.elapsed().as_secs_f64().max(0.001);
                        last_tick = Instant::now();
                        print!(
                            "\r  ↑ {:<30} {:>6.1}%  {}  {}",
                            elide(&file_name, 30),
                            percent(transferred, total),
                            human_size(transferred),
                            human_speed(speed)
                        );
                        let _ = std::io::stdout().flush();
                    }
                }
                AppEvent::FileFinished { file_name, size, direction: Direction::Send, .. } => {
                    println!("\r  ✓ {:<30} {}", elide(&file_name, 30), human_size(size));
                }
                AppEvent::SessionFinished { files, bytes, direction: Direction::Send, .. } => {
                    println!(
                        "  发送完成：{files} 个文件，{}，耗时 {:.1}s",
                        human_size(bytes),
                        started.elapsed().as_secs_f64()
                    );
                    break;
                }
                AppEvent::SessionFailed { error, .. } => {
                    println!("\r  ✗ {error}");
                    break;
                }
                AppEvent::Log { message, .. } => println!("  · {message}"),
                _ => {}
            }
        }
    });

    let result = send_files(node.state.clone(), target, files, cancel_rx).await;
    let _ = printer.await;
    result.map(|_| ()).map_err(|err| anyhow!(err.to_string()))
}

async fn resolve_target(
    node: &Arc<Node>,
    to: &str,
    port: Option<u16>,
    wait: u64,
) -> Result<DiscoveredDevice> {
    // 1) 直接给 IP：探测一次拿到指纹
    if let Ok(ip) = to.parse::<IpAddr>() {
        let port = port.unwrap_or(DEFAULT_PORT);
        let base = match ip {
            IpAddr::V4(v4) => format!("https://{v4}:{port}"),
            IpAddr::V6(v6) => format!("https://[{v6}]:{port}"),
        };
        let info = client::probe(&base, None).await?;
        return Ok(DiscoveredDevice {
            info,
            ip,
            last_seen: Instant::now(),
            via_multicast: false,
        });
    }

    // 2) 按别名 / 指纹，在超时内等待组播发现
    let deadline = Instant::now() + Duration::from_secs(wait);
    loop {
        if let Some(device) = node.devices().into_iter().find(|device| {
            device.info.alias.eq_ignore_ascii_case(to) || device.info.fingerprint == to
        }) {
            return Ok(device);
        }
        if Instant::now() >= deadline {
            let available: Vec<String> = node
                .devices()
                .into_iter()
                .map(|device| format!("{} ({})", device.info.alias, device.ip))
                .collect();
            bail!(
                "未找到设备「{}」。当前可见：{}。可用 --to <IP> 直接指定。",
                to,
                if available.is_empty() { "无".to_string() } else { available.join("、") }
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn run_chat(args: ChatArgs) -> Result<()> {
    let text = args.text.join(" ");
    if text.trim().is_empty() {
        bail!("消息内容不能为空");
    }

    let node = Node::start(Config::load()).await?;
    let me = node.device_info();
    let target = resolve_target(&node, &args.to, args.port, args.wait).await?;
    println!("  {} → {}", me.alias, target.info.alias);

    match client::send_chat(node.state.clone(), &target, &text).await {
        Ok(message) => {
            println!("  ✓ 已送达（{}）：{}", message.id, text.trim());
            Ok(())
        }
        Err(err) => {
            println!("  ✗ {err}");
            Err(err)
        }
    }
}

fn percent(current: u64, total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    (current as f64 / total as f64 * 100.0).min(100.0)
}

fn elide(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}
