//! 系统托盘：任务栏右下角的图标、右键菜单和消息提醒。
//!
//! 没有引第三方托盘库：这里要的东西很少（一个图标、一个两项的菜单、一个气泡），
//! 而 `windows-sys` 那套绑定会把依赖树撑大不少。工程里已经有直接调 Win32 的先例
//! （`instance.rs` 的 `ShowWindow`、`main.rs` 的 DWM 圆角），这里沿用同样的写法。
//!
//! 窗口是**消息窗口**（`HWND_MESSAGE` 父窗口）：不可见、不进 Alt-Tab、不进任务栏，
//! 只用来接托盘回调。它建在主线程上——winit 的消息泵会把线程内所有窗口的消息都
//! `DispatchMessageW` 出去，所以不需要自己再跑一个消息循环。
//!
//! 未读提醒分三层，从轻到重：
//! 1. 悬停提示带上「N 条未读」；
//! 2. 图标右下角加红点（换一个带点的 HICON）；
//! 3. 不在前台时弹一条系统通知气泡。

#![cfg(windows)]

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicUsize, Ordering};
use std::sync::Mutex;

const WM_APP: u32 = 0x8000;
const WM_TRAY: u32 = WM_APP + 1;
const WM_NULL: u32 = 0x0000;
const WM_LBUTTONUP: u32 = 0x0202;
const WM_LBUTTONDBLCLK: u32 = 0x0203;
const WM_RBUTTONUP: u32 = 0x0205;

const SW_SHOW: i32 = 5;
const SW_HIDE: i32 = 0;
const SW_RESTORE: i32 = 9;

const NIM_ADD: u32 = 0;
const NIM_MODIFY: u32 = 1;
const NIM_DELETE: u32 = 2;
const NIF_MESSAGE: u32 = 0x01;
const NIF_ICON: u32 = 0x02;
const NIF_TIP: u32 = 0x04;
const NIF_INFO: u32 = 0x10;
const NIIF_INFO: u32 = 0x01;

const IMAGE_ICON: u32 = 1;
const LR_DEFAULTCOLOR: u32 = 0;
const SM_CXSMICON: i32 = 49;

const MF_STRING: u32 = 0x0000;
const MF_SEPARATOR: u32 = 0x0800;
const TPM_RETURNCMD: u32 = 0x0100;
const TPM_RIGHTBUTTON: u32 = 0x0002;

const ID_SHOW: usize = 1;
const ID_QUIT: usize = 2;

const HWND_MESSAGE: isize = -3;

/// 图标资源 id（`build.rs` 里 rc 文件写的是 `1 ICON ...`）。
const ICON_RESOURCE: usize = 1;

#[repr(C)]
struct NotifyIconDataW {
    cb_size: u32,
    hwnd: isize,
    uid: u32,
    flags: u32,
    callback_message: u32,
    hicon: isize,
    tip: [u16; 128],
    state: u32,
    state_mask: u32,
    info: [u16; 256],
    version: u32,
    info_title: [u16; 64],
    info_flags: u32,
    guid: [u8; 16],
    balloon_icon: isize,
}

impl NotifyIconDataW {
    fn new(hwnd: isize) -> Self {
        Self {
            cb_size: std::mem::size_of::<Self>() as u32,
            hwnd,
            uid: 1,
            flags: 0,
            callback_message: WM_TRAY,
            hicon: 0,
            tip: [0; 128],
            state: 0,
            state_mask: 0,
            info: [0; 256],
            version: 0,
            info_title: [0; 64],
            info_flags: 0,
            guid: [0; 16],
            balloon_icon: 0,
        }
    }
}

#[repr(C)]
struct WndClassW {
    style: u32,
    wnd_proc: Option<unsafe extern "system" fn(isize, u32, usize, isize) -> isize>,
    cls_extra: i32,
    wnd_extra: i32,
    instance: isize,
    icon: isize,
    cursor: isize,
    background: isize,
    menu_name: *const u16,
    class_name: *const u16,
}

#[repr(C)]
struct Point {
    x: i32,
    y: i32,
}

#[repr(C)]
struct BitmapInfoHeader {
    size: u32,
    width: i32,
    height: i32,
    planes: u16,
    bit_count: u16,
    compression: u32,
    size_image: u32,
    x_pels: i32,
    y_pels: i32,
    clr_used: u32,
    clr_important: u32,
}

#[repr(C)]
struct IconInfo {
    is_icon: i32,
    hot_spot_x: u32,
    hot_spot_y: u32,
    mask: isize,
    color: isize,
}

#[link(name = "user32")]
extern "system" {
    fn RegisterClassW(class: *const WndClassW) -> u16;
    fn CreateWindowExW(
        ex_style: u32,
        class_name: *const u16,
        window_name: *const u16,
        style: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        parent: isize,
        menu: isize,
        instance: isize,
        param: *mut std::ffi::c_void,
    ) -> isize;
    fn DefWindowProcW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize;
    fn DestroyWindow(hwnd: isize) -> i32;
    fn LoadImageW(instance: isize, name: *const u16, kind: u32, cx: i32, cy: i32, load: u32)
        -> isize;
    fn DestroyIcon(icon: isize) -> i32;
    fn CreatePopupMenu() -> isize;
    fn AppendMenuW(menu: isize, flags: u32, id: usize, item: *const u16) -> i32;
    fn DestroyMenu(menu: isize) -> i32;
    fn TrackPopupMenu(
        menu: isize,
        flags: u32,
        x: i32,
        y: i32,
        reserved: i32,
        hwnd: isize,
        rect: *const std::ffi::c_void,
    ) -> i32;
    fn GetCursorPos(point: *mut Point) -> i32;
    fn SetForegroundWindow(hwnd: isize) -> i32;
    fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
    fn PostMessageW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> i32;
    fn RegisterWindowMessageW(name: *const u16) -> u32;
    fn GetSystemMetrics(index: i32) -> i32;
    fn CreateIconIndirect(info: *const IconInfo) -> isize;
}

#[link(name = "shell32")]
extern "system" {
    fn Shell_NotifyIconW(message: u32, data: *const NotifyIconDataW) -> i32;
}

#[link(name = "gdi32")]
extern "system" {
    fn CreateDIBSection(
        hdc: isize,
        header: *const BitmapInfoHeader,
        usage: u32,
        bits: *mut *mut std::ffi::c_void,
        section: isize,
        offset: u32,
    ) -> isize;
    fn CreateBitmap(width: i32, height: i32, planes: u32, bits: u32, data: *const std::ffi::c_void)
        -> isize;
    fn DeleteObject(object: isize) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(name: *const u16) -> isize;
}

static TRAY_HWND: AtomicIsize = AtomicIsize::new(0);
static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);
static ICON_PLAIN: AtomicIsize = AtomicIsize::new(0);
static ICON_BADGED: AtomicIsize = AtomicIsize::new(0);
/// 窗口是否在前台。主线程每帧刷新，通知线程据此决定要不要弹气泡。
static FOREGROUND: AtomicBool = AtomicBool::new(true);
/// 托盘菜单里点了「退出」。
static QUIT_REQUESTED: AtomicBool = AtomicBool::new(false);
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
/// 托盘上显示的未读数。主线程和通知线程都会写，主线程的值是权威的。
static UNREAD: AtomicUsize = AtomicUsize::new(0);
static BADGE_SHOWN: AtomicBool = AtomicBool::new(false);
static TIP: Mutex<String> = Mutex::new(String::new());

/// 把 Rust 字符串抄进固定长度的 UTF-16 缓冲（带结尾 0）。
fn fill_wide<const N: usize>(slot: &mut [u16; N], text: &str) {
    slot.fill(0);
    for (index, unit) in text.encode_utf16().take(N - 1).enumerate() {
        slot[index] = unit;
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 装好托盘图标。`hwnd` 是主窗口，点图标时用来把它亮出来。
pub fn install(hwnd: isize) {
    MAIN_HWND.store(hwnd, Ordering::Relaxed);
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let class_name = wide("FeigeCraftTrayWindow");
        let class = WndClassW {
            style: 0,
            wnd_proc: Some(wnd_proc),
            cls_extra: 0,
            wnd_extra: 0,
            instance,
            icon: 0,
            cursor: 0,
            background: 0,
            menu_name: std::ptr::null(),
            class_name: class_name.as_ptr(),
        };
        RegisterClassW(&class);
        let tray = CreateWindowExW(
            0,
            class_name.as_ptr(),
            class_name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            0,
            instance,
            std::ptr::null_mut(),
        );
        if tray == 0 {
            tracing::warn!("创建托盘窗口失败，托盘图标不可用");
            return;
        }
        TRAY_HWND.store(tray, Ordering::Relaxed);
        TASKBAR_CREATED.store(RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()), Ordering::Relaxed);

        let size = GetSystemMetrics(SM_CXSMICON).max(16);
        let plain = LoadImageW(instance, ICON_RESOURCE as *const u16, IMAGE_ICON, size, size, LR_DEFAULTCOLOR);
        if plain == 0 {
            tracing::warn!("加载托盘图标失败");
        }
        ICON_PLAIN.store(plain, Ordering::Relaxed);
        let badged = make_badged(plain, size);
        if badged == 0 {
            tracing::warn!("生成未读红点图标失败，只保留悬停提示和气泡提醒");
        }
        ICON_BADGED.store(badged, Ordering::Relaxed);

        let mut data = NotifyIconDataW::new(tray);
        data.flags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        data.hicon = plain;
        fill_wide(&mut data.tip, "FeigeCraft");
        if Shell_NotifyIconW(NIM_ADD, &data) == 0 {
            tracing::warn!("托盘图标添加失败");
        }
        tracing::info!("托盘已安装 hwnd={tray} 普通图标={plain} 红点图标={badged}");
        *TIP.lock().unwrap_or_else(|err| err.into_inner()) = "FeigeCraft".to_string();
    }
}

/// 摘掉托盘图标（程序退出前调用，否则会在任务栏留下一个「幽灵图标」）。
pub fn uninstall() {
    let tray = TRAY_HWND.swap(0, Ordering::Relaxed);
    if tray == 0 {
        return;
    }
    unsafe {
        let data = NotifyIconDataW::new(tray);
        Shell_NotifyIconW(NIM_DELETE, &data);
        for slot in [&ICON_PLAIN, &ICON_BADGED] {
            let icon = slot.swap(0, Ordering::Relaxed);
            if icon != 0 {
                DestroyIcon(icon);
            }
        }
        DestroyWindow(tray);
    }
}

/// 主线程每帧同步一次：窗口在不在前台。
pub fn set_foreground(on: bool) {
    FOREGROUND.store(on, Ordering::Relaxed);
}

pub fn is_foreground() -> bool {
    FOREGROUND.load(Ordering::Relaxed)
}

pub fn quit_requested() -> bool {
    QUIT_REQUESTED.load(Ordering::Relaxed)
}

/// 主线程把自己的未读总数推过来（比托盘线程自己数更准）。
pub fn set_unread(count: usize) {
    if UNREAD.swap(count, Ordering::Relaxed) != count {
        refresh_icon();
    }
}

/// 通知线程收到新消息时调用：累加未读并刷新图标。
pub fn bump_unread() {
    UNREAD.fetch_add(1, Ordering::Relaxed);
    refresh_icon();
}

fn refresh_icon() {
    let tray = TRAY_HWND.load(Ordering::Relaxed);
    if tray == 0 {
        tracing::warn!("托盘窗口不存在，无法刷新图标");
        return;
    }
    let unread = UNREAD.load(Ordering::Relaxed);
    let want_badge = unread > 0;
    let badged = want_badge != BADGE_SHOWN.swap(want_badge, Ordering::Relaxed);

    let mut tip = String::from("FeigeCraft");
    if unread > 0 {
        tip.push_str(" · ");
        tip.push_str(&crate::i18n::tf(crate::i18n::tr("{} 条未读"), &[&unread]));
    }
    let mut slot = TIP.lock().unwrap_or_else(|err| err.into_inner());
    let tip_changed = *slot != tip;
    if !tip_changed && !badged {
        return;
    }
    *slot = tip.clone();

    unsafe {
        let mut data = NotifyIconDataW::new(tray);
        data.flags = NIF_TIP;
        if badged {
            data.flags |= NIF_ICON;
            data.hicon = if want_badge {
                ICON_BADGED.load(Ordering::Relaxed)
            } else {
                ICON_PLAIN.load(Ordering::Relaxed)
            };
        }
        fill_wide(&mut data.tip, &tip);
        let ok = Shell_NotifyIconW(NIM_MODIFY, &data);
        tracing::info!("刷新托盘：未读={unread} 红点={want_badge} 结果={ok}");
    }
}

/// 弹一条系统通知气泡。
pub fn notify(title: &str, body: &str) {
    let tray = TRAY_HWND.load(Ordering::Relaxed);
    if tray == 0 {
        return;
    }
    unsafe {
        let mut data = NotifyIconDataW::new(tray);
        data.flags = NIF_INFO | NIF_ICON;
        data.hicon = ICON_PLAIN.load(Ordering::Relaxed);
        data.info_flags = NIIF_INFO;
        fill_wide(&mut data.info_title, title);
        fill_wide(&mut data.info, body);
        let ok = Shell_NotifyIconW(NIM_MODIFY, &data);
        tracing::info!("托盘气泡「{title}」：{body} 结果={ok}");
    }
}

unsafe extern "system" fn wnd_proc(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize {
    let taskbar_created = TASKBAR_CREATED.load(Ordering::Relaxed);
    if taskbar_created != 0 && msg == taskbar_created {
        // 资源管理器重启后托盘会被清空，得把图标重新加回去。
        add_again(hwnd);
        return 0;
    }
    if msg == WM_TRAY {
        match lparam as u32 {
            WM_LBUTTONUP | WM_LBUTTONDBLCLK => show_main(),
            WM_RBUTTONUP => popup_menu(hwnd),
            _ => {}
        }
        return 0;
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

fn add_again(hwnd: isize) {
    unsafe {
        let mut data = NotifyIconDataW::new(hwnd);
        data.flags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        data.hicon = ICON_PLAIN.load(Ordering::Relaxed);
        fill_wide(&mut data.tip, "FeigeCraft");
        Shell_NotifyIconW(NIM_ADD, &data);
    }
}

fn show_main() {
    let hwnd = MAIN_HWND.load(Ordering::Relaxed);
    if hwnd == 0 {
        return;
    }
    unsafe {
        ShowWindow(hwnd, SW_RESTORE);
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
    }
}

/// 原生隐藏主窗口。不要用 `ViewportCommand::Visible(false)`：
/// 托盘/单实例唤醒是绕过 winit 直接 `ShowWindow` 的，winit 感知不到
/// `WM_SHOWWINDOW`，它的可见标志还停在「隐藏」，再发 Visible(false)
/// 会因为「无变化」被吞掉，窗口就收不回去了。两边都用原生调用才对称。
pub fn hide_main() {
    let hwnd = MAIN_HWND.load(Ordering::Relaxed);
    if hwnd == 0 {
        return;
    }
    unsafe {
        ShowWindow(hwnd, SW_HIDE);
    }
}

fn popup_menu(hwnd: isize) {
    unsafe {
        let menu = CreatePopupMenu();
        if menu == 0 {
            return;
        }
        AppendMenuW(menu, MF_STRING, ID_SHOW, wide(crate::i18n::tr("显示主窗口")).as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        AppendMenuW(menu, MF_STRING, ID_QUIT, wide(crate::i18n::tr("退出")).as_ptr());

        let mut point = Point { x: 0, y: 0 };
        GetCursorPos(&mut point);
        // 不先抢前台，菜单点外面就不会消失（Windows 的老规矩）。
        SetForegroundWindow(hwnd);
        let picked = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            point.x,
            point.y,
            0,
            hwnd,
            std::ptr::null(),
        );
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);

        match picked as usize {
            ID_SHOW => show_main(),
            ID_QUIT => quit(),
            _ => {}
        }
    }
}

/// 从托盘退出：窗口可能在后台藏着，先亮出来让它跑一帧，关窗请求才有人处理。
fn quit() {
    QUIT_REQUESTED.store(true, Ordering::Relaxed);
    let hwnd = MAIN_HWND.load(Ordering::Relaxed);
    if hwnd == 0 {
        return;
    }
    unsafe {
        ShowWindow(hwnd, SW_SHOW);
        PostMessageW(hwnd, 0x0010, 0, 0); // WM_CLOSE
    }
}

/// 在主图标右下角画一个红点，做成「有未读」的版本。
fn make_badged(base: isize, size: i32) -> isize {
    if base == 0 {
        return 0;
    }
    const BADGE_PNG: &[u8] = include_bytes!("../assets/icon-256.png");
    let Ok(image) = image::load_from_memory(BADGE_PNG) else { return 0 };
    let mut rgba = image
        .resize_exact(size as u32, size as u32, image::imageops::FilterType::Lanczos3)
        .to_rgba8();

    // 红点：右下的圆，外面留一圈浅色描边，深色/浅色任务栏上都能看清。
    let cx = size as f32 - 4.6;
    let cy = size as f32 - 4.6;
    let r = (size as f32 * 0.30).max(3.6);
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist <= r {
                rgba.put_pixel(x as u32, y as u32, image::Rgba([229, 57, 53, 255]));
            } else if dist <= r + 1.0 {
                rgba.put_pixel(x as u32, y as u32, image::Rgba([255, 255, 255, 235]));
            }
        }
    }

    unsafe {
        // 32 位 DIB：高度给负数是自上而下，省得再翻转一次。
        let header = BitmapInfoHeader {
            size: std::mem::size_of::<BitmapInfoHeader>() as u32,
            width: size,
            height: -size,
            planes: 1,
            bit_count: 32,
            compression: 0,
            size_image: 0,
            x_pels: 0,
            y_pels: 0,
            clr_used: 0,
            clr_important: 0,
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let color = CreateDIBSection(0, &header, 0, &mut bits, 0, 0);
        if color == 0 || bits.is_null() {
            return 0;
        }
        let pixels = std::slice::from_raw_parts_mut(bits as *mut u8, (size * size * 4) as usize);
        for (index, pixel) in rgba.pixels().enumerate() {
            let [r, g, b, a] = pixel.0;
            pixels[index * 4] = b;
            pixels[index * 4 + 1] = g;
            pixels[index * 4 + 2] = r;
            pixels[index * 4 + 3] = a;
        }
        // 掩码位图全 0：透明度完全交给颜色图层的 alpha。
        let mask = CreateBitmap(size, size, 1, 1, std::ptr::null());
        let info = IconInfo { is_icon: 1, hot_spot_x: 0, hot_spot_y: 0, mask, color };
        let icon = CreateIconIndirect(&info);
        DeleteObject(color);
        if mask != 0 {
            DeleteObject(mask);
        }
        icon
    }
}
