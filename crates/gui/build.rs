//! 把应用图标与文件属性嵌入 Windows 可执行文件。
//!
//! 只影响 Windows 产物：没有这一步，任务栏、Alt-Tab 和资源管理器里显示的都是
//! 系统默认的空白程序图标。资源编译器（windres / rc.exe）由本脚本自动探测，
//! 找不到时只打印一条警告并跳过——不影响程序本身构建和运行。

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let icon = manifest.join("assets").join("icon.ico");
    if !icon.exists() {
        println!("cargo:warning=assets/icon.ico 不存在，跳过图标嵌入（先运行 scripts/make-icon.py）");
        return;
    }

    let rc_path = out_dir.join("feigecraft.rc");
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.1.0".into());
    let version_nums = version
        .split('.')
        .map(|part| part.parse::<u32>().unwrap_or(0))
        .chain(std::iter::repeat(0))
        .take(4)
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let icon_path = slash(&icon);
    let rc = format!(
        r#"#include <winver.h>

1 ICON "{icon_path}"

VS_VERSION_INFO VERSIONINFO
FILEVERSION {version_nums}
PRODUCTVERSION {version_nums}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "080404B0"
        BEGIN
            VALUE "CompanyName", "FeigeCraft"
            VALUE "FileDescription", "FeigeCraft 局域网聊天与快传"
            VALUE "ProductName", "FeigeCraft"
            VALUE "OriginalFilename", "feigecraft.exe"
            VALUE "FileVersion", "{version}"
            VALUE "ProductVersion", "{version}"
            VALUE "LegalCopyright", "MIT License"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x804, 1200
    END
END
"#
    );
    if std::fs::write(&rc_path, rc).is_err() {
        println!("cargo:warning=写入资源脚本失败，跳过图标嵌入");
        return;
    }

    let obj = out_dir.join("feigecraft-res.o");
    let compiled = match find_windres() {
        Some(windres) => run_windres(&windres, &rc_path, &obj),
        None => false,
    };
    if !compiled {
        println!(
            "cargo:warning=未找到 windres（或编译失败），exe 将使用系统默认图标。\
             安装 mingw 后重试，或设置环境变量 WINDRES 指向 windres.exe"
        );
        return;
    }
    // 直接把资源对象交给链接器，不需要额外 crate。
    println!("cargo:rustc-link-arg-bins={}", obj.display());
}

/// RC 里用正斜杠，避免 Windows 路径的反斜杠被当成转义。
fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn find_windres() -> Option<PathBuf> {
    if let Ok(value) = std::env::var("WINDRES") {
        let path = PathBuf::from(value);
        if path.exists() {
            return Some(path);
        }
    }
    const GUESSES: &[&str] = &[
        r"C:\msys64\mingw64\bin\windres.exe",
        r"C:\msys64\usr\bin\windres.exe",
        r"C:\mingw64\bin\windres.exe",
        r"C:\mingw32\bin\windres.exe",
        r"C:\ProgramData\chocolatey\bin\windres.exe",
    ];
    for guess in GUESSES {
        let path = PathBuf::from(guess);
        if path.exists() {
            return Some(path);
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("windres.exe"))
        .find(|candidate| candidate.exists())
}

fn run_windres(windres: &Path, rc: &Path, obj: &Path) -> bool {
    let mut command = Command::new(windres);
    // windres 内部还要调 gcc 做预处理，得把它自己所在目录放进 PATH。
    if let Some(dir) = windres.parent() {
        let current = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![dir.to_path_buf()];
        paths.extend(std::env::split_paths(&current));
        if let Ok(joined) = std::env::join_paths(paths) {
            command.env("PATH", joined);
        }
    }
    let status = command
        .arg("-c")
        .arg("65001")
        .arg("-i")
        .arg(rc)
        .arg("-o")
        .arg(obj)
        .status();
    matches!(status, Ok(status) if status.success()) && obj.exists()
}
