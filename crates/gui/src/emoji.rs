//! 彩色表情贴图。
//!
//! 为什么不用字体：egui 走 ab_glyph，只能画单色轮廓。系统里的 `seguiemj.ttf`
//! 是 COLR/CPAL 彩色字体，落到 ab_glyph 里就只剩轮廓层——所以表情面板和聊天气泡
//! 里的表情一律是黑白的剪影，放在深色背景上非常难看。
//!
//! 这里改用 Twemoji 的 72×72 PNG（CC-BY 4.0，署名见 `assets/emoji/ATTRIBUTION.md`），
//! 启动时解码成贴图，面板和大表情消息都直接贴图，颜色就是设计稿里的颜色。

use std::collections::HashMap;

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};

use crate::emoji_data::EMOJI;

/// 一条表情：字符（用于写入输入框 / 匹配消息文本）+ 贴图。
pub struct Emoji {
    pub ch: &'static str,
    pub texture: TextureHandle,
}

pub struct EmojiSet {
    items: Vec<Emoji>,
    index: HashMap<char, usize>,
}

impl EmojiSet {
    /// 解码全部素材并建贴图。60 张 72×72 的 PNG 大约 10ms，放启动阶段可以接受。
    pub fn load(ctx: &egui::Context) -> Self {
        let options = TextureOptions {
            magnification: egui::TextureFilter::Linear,
            minification: egui::TextureFilter::Linear,
            wrap_mode: egui::TextureWrapMode::ClampToEdge,
            mipmap_mode: Some(egui::TextureFilter::Linear),
        };
        let mut items = Vec::with_capacity(EMOJI.len());
        let mut index = HashMap::with_capacity(EMOJI.len());
        for (ch, bytes) in EMOJI.iter() {
            let Some(texture) = decode(ctx, ch, bytes, options) else { continue };
            let single = ch.chars().next().unwrap_or('?');
            index.insert(single, items.len());
            items.push(Emoji { ch, texture });
        }
        Self { items, index }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn get(&self, index: usize) -> Option<&Emoji> {
        self.items.get(index)
    }

    pub fn index_of(&self, ch: char) -> Option<usize> {
        self.index.get(&ch).copied()
    }

    /// 文本里是否至少有一个表情集内的表情。用来决定要不要走混排贴图那条路。
    pub fn contains_any(&self, text: &str) -> bool {
        text.chars().any(|ch| self.index.contains_key(&ch))
    }

    /// 整段文本是否「全是表情」。是的话返回每个表情的下标，供气泡画成大表情；
    /// 只要掺了一个普通字符就返回 `None`，回到常规文字排版。
    ///
    /// 变体选择符（U+FE0F）与空白忽略：用户从输入法/网页里粘过来的表情
    /// 常常带这个不可见字符，`❤️` 和 `❤` 应当视为同一个表情。
    pub fn only_emoji(&self, text: &str) -> Option<Vec<usize>> {
        let mut out = Vec::new();
        for ch in text.chars() {
            if matches!(ch, '\u{fe0f}' | '\u{200d}' | ' ' | '\t') {
                continue;
            }
            out.push(self.index_of(ch)?);
        }
        (!out.is_empty()).then_some(out)
    }
}

fn decode(
    ctx: &egui::Context,
    name: &str,
    bytes: &[u8],
    options: TextureOptions,
) -> Option<TextureHandle> {
    let image = image::load_from_memory(bytes).ok()?.to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    let color = ColorImage::from_rgba_unmultiplied(size, image.as_raw());
    Some(ctx.load_texture(format!("emoji-{name}"), color, options))
}

/// 贴一张表情到 `rect`（等比缩放、居中）。
pub fn paint(painter: &egui::Painter, rect: egui::Rect, texture: &TextureHandle, size: f32) {
    let target = egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(size));
    painter.image(
        texture.id(),
        target,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
}
