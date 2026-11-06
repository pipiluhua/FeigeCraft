//! FeigeCraft 入口：一个 exe 同时承担窗口和命令行。
//!
//! - `feigecraft` → 打开窗口（默认）
//! - `feigecraft serve / send / chat / discover / info` → 命令行模式，不开窗口
//!
//! 合并不是为了少一个文件，而是让「后台收文件」变得顺手：装一次就是这一个程序，
//! 想开机自启就写 `feigecraft serve`，想交互就双击。以前那种两个 exe 的做法，
//! 用户得先搞清楚该点哪一个。

mod app;
mod capture;
mod emoji;
mod emoji_data;
mod i18n;
mod instance;
mod theme;
mod widgets;

/// 系统托盘只在 Windows 上实现（见 `tray.rs`）。其它平台留一套空实现，
/// 免得调用点到处写 `cfg`。
#[cfg(windows)]
mod tray;
#[cfg(not(windows))]
mod tray {
    pub fn install(_hwnd: isize) {}
    pub fn uninstall() {}
    pub fn set_foreground(_on: bool) {}
    pub fn is_foreground() -> bool {
        true
    }
    pub fn quit_requested() -> bool {
        false
    }
    pub fn set_unread(_count: usize) {}
    pub fn bump_unread() {}
    pub fn notify(_title: &str, _body: &str) {}
}

use std::path::PathBuf;

use app::FeigeCraftApp;
use feigecraft_core::config::Config;
use feigecraft_core::model::DEFAULT_PORT;

/// 启动参数。全部可选——不带参数时就是「正常启动」。
struct Launch {
    instance: Option<usize>,
    port: Option<u16>,
    alias: Option<String>,
    config_dir: Option<PathBuf>,
    /// 明确要求再开一个窗口（多开测试用），跳过敏捷唤醒。
    new_window: bool,
    /// 启动后不进前台：只在后台接收，双击程序才露脸。开机自启用这个。
    background: bool,
}

impl Launch {
    fn parse() -> Self {
        let mut launch = Self {
            instance: None,
            port: None,
            alias: None,
            config_dir: None,
            new_window: false,
            background: false,
        };
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--new" => {
                    launch.new_window = true;
                    continue;
                }
                "--background" | "--hidden" => {
                    launch.background = true;
                    continue;
                }
                _ => {}
            }
            let mut take = |name: &str| -> Option<String> {
                // 同时支持 `--port 53400` 与 `--port=53400`
                if let Some(value) = arg.strip_prefix(&format!("{name}=")) {
                    return Some(value.to_string());
                }
                if arg == name {
                    return args.next();
                }
                None
            };
            if let Some(value) = take("--instance") {
                launch.instance = value.trim().parse().ok();
            } else if let Some(value) = take("--port") {
                launch.port = value.trim().parse().ok();
            } else if let Some(value) = take("--alias") {
                launch.alias = Some(value);
            } else if let Some(value) = take("--config-dir") {
                launch.config_dir = Some(PathBuf::from(value));
            }
        }
        launch
    }
}

/// 给实例准备独立的配置目录与初始配置。
fn prepare_instance(launch: &Launch, index: usize) {
    let base = match &launch.config_dir {
        Some(dir) => dir.clone(),
        None => Config::config_dir(),
    };
    let dir = if index == 0 { base } else { base.join(format!("instance-{index}")) };
    feigecraft_core::config::set_config_dir(dir);

    // 第一次启动这个实例：写入能和 0 号实例区分开的别名、监听端口与下载目录，
    // 否则两个窗口在会话列表里同名同色，根本分不清谁是谁。
    if !Config::path().exists() {
        let mut config = Config::default();
        // 默认别名先用中文词库占位，这里按最终界面语言重新随机一次。
        let lang = i18n::resolve(&config.language);
        config.alias = feigecraft_core::util::random_alias_lang(lang.key());
        if index > 0 {
            config.alias = format!("{}-{index}", config.alias);
            config.port = instance_port(index);
            config.download_dir =
                feigecraft_core::util::default_download_dir().join(format!("FeigeCraft-{index}"));
        }
        if let Some(port) = launch.port {
            config.port = port;
        }
        if let Some(alias) = &launch.alias {
            config.alias = alias.clone();
        }
        // 这个名字是刚随机出来的，记下来，下次启动别再改。
        config.alias_custom = true;
        let _ = config.save();
        return;
    }

    // 配置已存在时，命令行显式给出的端口 / 别名仍然生效；
    // 另外把沿用默认端口的 2 号及以后的实例挪到独立端口上（老配置升上来也适用）。
    let mut config = Config::load();
    let mut dirty = false;
    // 老配置的默认名是电脑名（比如 DESKTOP-8K2L），升级上来看着像「随机名没生效」。
    // 用户没自己改过名，就在升级后补一次随机名；改过名的（`alias_custom` 为真）一律不动。
    // 昵称跟随界面语言生成（中文词库 / 英文词库）。
    if !config.alias_custom {
        let lang = i18n::resolve(&config.language);
        let mut fresh = feigecraft_core::util::random_alias_lang(lang.key());
        if index > 0 {
            fresh = format!("{fresh}-{index}");
        }
        config.alias = fresh;
        config.alias_custom = true;
        dirty = true;
    }
    if let Some(port) = launch.port {
        config.port = port;
        dirty = true;
    }
    if let Some(alias) = &launch.alias {
        config.alias = alias.clone();
        config.alias_custom = true;
        dirty = true;
    }
    if index > 0 && launch.port.is_none() && config.port == DEFAULT_PORT {
        config.port = instance_port(index);
        dirty = true;
    }
    if dirty {
        let _ = config.save();
    }
}

/// 多开时每个实例必须有自己的 HTTP 监听端口：都用 53317 的话，
/// 第二个实例会靠 SO_REUSEADDR 抢同一个端口，别人发给它的文件
/// 可能被 0 号实例收走。1 号实例保持 53317 以兼容 LocalSend 互操作。
fn instance_port(index: usize) -> u16 {
    DEFAULT_PORT.saturating_add(index as u16)
}

/// 窗口图标。任务栏 / Alt-Tab 用的是窗口图标（`WM_SETICON`），
/// 和 exe 里嵌入的资源图标是两套东西，两个都要设，否则会出现
/// 「窗口是默认图标、文件是纸飞机」的割裂感。
fn window_icon() -> Option<std::sync::Arc<eframe::egui::IconData>> {
    let bytes = include_bytes!("../assets/icon-256.png");
    let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Png).ok()?;
    let image = image.to_rgba8();
    let (width, height) = image.dimensions();
    Some(std::sync::Arc::new(eframe::egui::IconData {
        rgba: image.into_raw(),
        width,
        height,
    }))
}

/// 窗口模式下把控制台窗口摘掉。
///
/// Rust 默认是 console 子系统，双击 exe 会先弹一个黑框，对普通用户来说
/// 既莫名其妙又难看。命令行模式不走这里，所以 `feigecraft send …` 照常能看到输出。
///
/// 想在窗口模式里看日志就设 `FEIGECRAFT_KEEP_CONSOLE=1`。
#[cfg(target_os = "windows")]
fn detach_console() {
    if std::env::var_os("FEIGECRAFT_KEEP_CONSOLE").is_some() {
        return;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn FreeConsole() -> i32;
    }
    unsafe {
        FreeConsole();
    }
}

#[cfg(not(target_os = "windows"))]
fn detach_console() {}

/// Windows 11 的原生窗口圆角。
///
/// egui 的裁剪只有矩形，自己画圆角只能「糊」四角、不能真的把角挖掉；
/// 交给 DWM 是最干净的做法：系统按圆角裁掉整个窗口，任务栏缩略图和
/// 阴影也一并对齐。Win10 及更早不认这个属性，返回错误码，忽略即可。
#[cfg(target_os = "windows")]
fn round_window_corners(cc: &eframe::CreationContext<'_>) {
    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    const DWMWCP_ROUND: i32 = 2;
    /// `DwmSetWindowAttribute(HWND, DWORD, LPCVOID, DWORD) -> HRESULT`
    type SetAttribute = unsafe extern "system" fn(isize, u32, *const i32, u32) -> i32;

    // 不直接 `#[link(name = "dwmapi")]`：mingw 的 sysroot 里没有 libdwmapi.a，
    // raw-dylib 又要 dlltool，换个工具链就编不过。运行时查一次符号最省事，
    // 也顺便把没有 DWM 的老系统挡在门外。
    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryA(name: *const u8) -> *mut std::ffi::c_void;
        fn GetProcAddress(module: *mut std::ffi::c_void, name: *const u8) -> *mut std::ffi::c_void;
    }

    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = cc.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(win32) = handle.as_raw() else {
        return;
    };

    unsafe {
        let module = LoadLibraryA(c"dwmapi.dll".as_ptr().cast());
        if module.is_null() {
            return;
        }
        let address = GetProcAddress(module, c"DwmSetWindowAttribute".as_ptr().cast());
        if address.is_null() {
            return;
        }
        let set_attribute: SetAttribute = std::mem::transmute(address);
        // Win11 才有 33 号属性；Win10 会返回 E_INVALIDARG（0x80070057），忽略即可。
        let hr = set_attribute(
            win32.hwnd.get(),
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &DWMWCP_ROUND,
            std::mem::size_of::<i32>() as u32,
        );
        if hr == 0 {
            tracing::debug!("已请求 DWM 圆角窗口（Win11）");
        } else {
            tracing::debug!("DWM 圆角不可用：HRESULT=0x{hr:08X}");
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn round_window_corners(_cc: &eframe::CreationContext<'_>) {}

fn main() -> eframe::Result<()> {
    // ---- 命令行模式 ----------------------------------------------------
    // 有子命令就完全走命令行：不开窗口、不占实例端口，跑完直接退出。
    if feigecraft_cli::wants_cli() {
        match feigecraft_cli::run() {
            Ok(()) => return Ok(()),
            Err(err) => {
                eprintln!("\n  ✗ {err}");
                std::process::exit(1);
            }
        }
    }

    // ---- 窗口模式 ------------------------------------------------------
    detach_console();
    feigecraft_core::init_logging();

    let launch = Launch::parse();

    // 已经在跑？把那个窗口叫出来就行，不再开第二个。
    // 这也是「关闭窗口后继续在后台收文件」的回头路：再双击一次程序就回来了。
    if !launch.new_window {
        let hit = match launch.instance {
            Some(index) => instance::wake(index).then_some(index),
            None => instance::running(),
        };
        if let Some(index) = hit {
            tracing::info!("实例 {index} 已在运行，已请求显示窗口");
            return Ok(());
        }
    }

    let guard = instance::Instance::claim(launch.instance);
    let instance = guard.index;
    let background = launch.background;
    prepare_instance(&launch, instance);
    tracing::info!("FeigeCraft 实例 {instance}，配置目录 {}", Config::config_dir().display());

    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_inner_size([1180.0, 780.0])
        .with_min_inner_size([960.0, 640.0])
        // 去掉系统标题栏：Windows 默认是亮白的一整条，和深色界面割裂；
        // 标题栏改由 app 自绘（可跟随主题、可放品牌与状态）。
        .with_decorations(false)
        .with_resizable(true)
        // 开机自启走 `--background`：窗口先不露面，但接收服务照常跑。
        .with_visible(!launch.background)
        .with_title(if instance == 0 {
            "FeigeCraft".to_string()
        } else {
            format!("FeigeCraft · 实例 {}", instance + 1)
        })
        .with_app_id("feigecraft");
    if let Some(icon) = window_icon() {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
        centered: true,
        ..Default::default()
    };

    eframe::run_native(
        "FeigeCraft · 局域网聊天与快传",
        options,
        Box::new(move |cc| {
            // 表情兜底字体（12 MB）先不装：正文里真出现表情集之外的表情时，
            // 由 FeigeCraftApp::ensure_emoji_font 按需补上。
            theme::install_fonts(&cc.egui_ctx, false);
            theme::install_style(&cc.egui_ctx);
            round_window_corners(cc);
            // 先把窗口句柄记下来，后台线程才能在窗口隐藏时把它重新亮出来。
            guard.remember_window(cc);
            // 托盘图标：窗口收进后台后，就靠它把人叫回来。
            tray::install(guard.hwnd());
            Ok(Box::new(FeigeCraftApp::new(
                &cc.egui_ctx,
                instance,
                Some(guard),
                background,
            )))
        }),
    )
}
