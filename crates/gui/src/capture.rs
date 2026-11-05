//! 应用内截图：`FEIGECRAFT_SCREENSHOT=out.bmp` 启动后自动截一张图并退出。
//! 用于文档配图与回归验收，不依赖窗口是否在前台。

use std::io::Write;
use std::path::Path;

use eframe::egui::ColorImage;

/// 把 `ColorImage` 存成 24 位 BMP（零依赖）。
pub fn save_bmp(path: &Path, image: &ColorImage) -> std::io::Result<()> {
    let width = image.size[0];
    let height = image.size[1];
    let row_size = (width * 3 + 3) & !3;
    let data_size = row_size * height;

    let mut out = Vec::with_capacity(54 + data_size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&((54 + data_size) as u32).to_le_bytes());
    out.extend_from_slice(&[0u8; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(width as i32).to_le_bytes());
    out.extend_from_slice(&(height as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(data_size as u32).to_le_bytes());
    out.extend_from_slice(&2835i32.to_le_bytes());
    out.extend_from_slice(&2835i32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());

    let padding = row_size - width * 3;
    for y in (0..height).rev() {
        for x in 0..width {
            let pixel = image.pixels[y * width + x];
            out.extend_from_slice(&[pixel.b(), pixel.g(), pixel.r()]);
        }
        out.extend(std::iter::repeat(0u8).take(padding));
    }

    let mut file = std::fs::File::create(path)?;
    file.write_all(&out)
}
