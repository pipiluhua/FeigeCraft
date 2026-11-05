//! 单实例与「叫醒」。
//!
//! 没有用锁文件：跨进程文件锁在 Windows 上要额外依赖，而「占一个本地端口」
//! 本身就是内核保证的原子操作，进程退出时自动释放，最省事也最可靠。
//!
//! 端口顺带承担第二个职责——当个小小的控制通道。后启动的进程连上去说一句
//! `show`，已经在跑的那个就把窗口亮出来，然后新进程自己退出。这样
//! 「关窗口后台继续收文件」才有回头路：再双击一次程序就回来了。
//!
//! 接收请求必须放在独立线程里：窗口隐藏后 winit 不再产生重绘，`update()`
//! 也就不再被调用，靠主线程轮询会永远收不到敲门声。线程拿到请求后还会直接用
//! Win32 `ShowWindow` 把窗口亮出来，不依赖渲染循环——否则就成了「叫了没人应」。
//!
//! ```text
//! 53300  0 号实例     53301  1 号实例     …
//! ```

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// 实例互斥端口起点。选 53300 是因为它离业务端口（53317）足够远，不会撞上。
const LOCK_BASE: u16 = 53300;
/// 最多同时开几个实例。
const MAX_INSTANCES: usize = 8;

/// 敲门：把窗口显示出来。
const CMD_SHOW: &[u8] = b"show\n";
/// 应答：确认对方确实是 FeigeCraft，而不是随便什么占着端口的程序。
const ACK: &[u8] = b"ok\n";

#[cfg(target_os = "windows")]
const SW_SHOW: i32 = 5;

#[cfg(target_os = "windows")]
#[link(name = "user32")]
extern "system" {
    fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
    fn SetForegroundWindow(hwnd: isize) -> i32;
}

/// 一个已占用的实例编号。
pub struct Instance {
    pub index: usize,
    /// 占住端口的监听器。抢不到时为 `None`，此时程序照常可用，只是唤不回窗口。
    listener: Option<TcpListener>,
    /// 后台线程收到「显示」请求后置位，主线程每帧取一次。
    wake: Arc<AtomicBool>,
    /// 窗口句柄，给后台线程直接调 `ShowWindow` 用。
    hwnd: Arc<AtomicIsize>,
}

impl Instance {
    /// 占用一个编号：`preferred` 给定时只用它，否则从小到大找空位。
    pub fn claim(preferred: Option<usize>) -> Self {
        let order: Vec<usize> = match preferred {
            Some(index) => vec![index],
            None => (0..MAX_INSTANCES).collect(),
        };
        let mut picked = None;
        for index in order {
            if let Ok(listener) = TcpListener::bind(("127.0.0.1", LOCK_BASE + index as u16)) {
                picked = Some((index, listener));
                break;
            }
        }
        let (index, listener) = match picked {
            Some((index, listener)) => (index, Some(listener)),
            None => (0, None),
        };
        Self {
            index,
            listener,
            wake: Arc::new(AtomicBool::new(false)),
            hwnd: Arc::new(AtomicIsize::new(0)),
        }
    }

    /// 记下真实窗口句柄（Windows 以外的平台忽略）。
    pub fn remember_window(&self, cc: &eframe::CreationContext<'_>) {
        #[cfg(target_os = "windows")]
        {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(handle) = cc.window_handle() {
                if let RawWindowHandle::Win32(win32) = handle.as_raw() {
                    self.hwnd.store(win32.hwnd.get(), Ordering::Relaxed);
                }
            }
        }
        #[cfg(not(target_os = "windows"))]
        let _ = cc;
    }

    /// 起一个线程守着控制端口。
    pub fn watch(&self, ctx: eframe::egui::Context) {
        let Some(listener) = self.listener.as_ref().and_then(|listener| listener.try_clone().ok())
        else {
            return;
        };
        let wake = self.wake.clone();
        let hwnd = self.hwnd.clone();
        std::thread::spawn(move || loop {
            let Ok((mut stream, _)) = listener.accept() else {
                // 进程退出、监听器关闭时结束线程。
                break;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
            let mut buf = [0u8; 16];
            let read = stream.read(&mut buf).unwrap_or(0);
            if !buf[..read].starts_with(CMD_SHOW) {
                continue;
            }
            let _ = stream.write_all(ACK);
            wake.store(true, Ordering::Relaxed);

            // 关键：窗口隐藏时 egui 不重绘，光让主线程「下一帧处理」是不够的，
            // 这里直接把窗口显示出来，渲染循环随之恢复。
            #[cfg(target_os = "windows")]
            {
                let handle = hwnd.load(Ordering::Relaxed);
                if handle != 0 {
                    unsafe {
                        ShowWindow(handle, SW_SHOW);
                        SetForegroundWindow(handle);
                    }
                }
            }
            ctx.request_repaint();
        });
    }

    /// 主线程每帧取一次：这一帧之前有没有人来敲过门。
    pub fn take_wakeup(&self) -> bool {
        self.wake.swap(false, Ordering::Relaxed)
    }

    /// 当前窗口句柄（还没建好窗口时为 0）。
    pub fn hwnd(&self) -> isize {
        self.hwnd.load(Ordering::Relaxed)
    }
}

/// 敲一下指定编号的门：成功说明那个实例正在运行，并且已经把窗口亮出来了。
pub fn wake(index: usize) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], LOCK_BASE + index as u16));
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(250)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(600)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(250)));
    if stream.write_all(CMD_SHOW).is_err() {
        return false;
    }
    let mut buf = [0u8; 8];
    matches!(stream.read(&mut buf), Ok(n) if n > 0 && buf[..n].starts_with(ACK))
}

/// 找出第一个正在运行的实例。
pub fn running() -> Option<usize> {
    (0..MAX_INSTANCES).find(|index| wake(*index))
}
