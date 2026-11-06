//! 截图走查用的小工具：裁一块区域并放大，方便目视检查细节。
//!
//! `cargo run -p feigecraft-gui --example crop -- <in> <out> <x> <y> <w> <h> <scale>`

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 8 {
        eprintln!("用法: crop <in> <out> <x> <y> <w> <h> <scale>");
        return;
    }
    let input = image::open(&args[1]).expect("读图失败");
    let x: u32 = args[3].parse().expect("x");
    let y: u32 = args[4].parse().expect("y");
    let w: u32 = args[5].parse().expect("w");
    let h: u32 = args[6].parse().expect("h");
    let scale: u32 = args[7].parse().expect("scale");
    let cropped = input.crop_imm(x, y, w, h);
    let scaled = cropped.resize(w * scale, h * scale, image::imageops::FilterType::Nearest);
    scaled.save(&args[2]).expect("保存失败");
    println!("已保存 {}", args[2]);
}
