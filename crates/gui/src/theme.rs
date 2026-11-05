//! 设计令牌、中文字体、深浅双主题。
//!
//! 配色策略：
//! - 三级明度：`bg`（聊天区）→ `panel`（侧栏 / 标题栏）→ `field`（浮起的输入框），
//!   靠明度而不是靠描边来分隔区域，界面更安静。
//! - 强调色只用在「当前会话、主按钮、自己发的气泡」三处，其余一律中性灰，
//!   避免整屏发蓝。
//! - 深色不是把浅色反相：深色用低饱和冷灰打底 + 略降饱和的蓝，长时间看不刺眼；
//!   浅色用白 + 冷灰底，气泡用纯白，靠 `border_soft` 收边。
//!
//! 令牌通过函数访问（`theme::bg()`），当前取值放在线程局部的 `ACTIVE` 里，
//! 每帧开头由 `sync()` 按 egui 解析出的主题刷新。

use eframe::egui::{
    self, Color32, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle, Theme,
    ThemePreference,
};

/// 一套完整配色。字段全是 `Copy`，可放进 `Cell` 做线程局部状态。
#[derive(Clone, Copy)]
pub struct Palette {
    /// 聊天区底色（最深 / 浅色模式下是唯一的灰底）
    pub bg: Color32,
    /// 侧栏、标题栏、聊天头部
    pub panel: Color32,
    /// 浮在 `panel` 上的块（搜索框、hover 行）
    pub card: Color32,
    pub card_hover: Color32,
    /// 浮在 `bg` 上的块（输入框）
    pub field: Color32,
    /// 会话列表选中行
    pub row_active: Color32,
    /// 选中行里的标题色（深色模式下才用纯白）
    pub row_active_text: Color32,
    pub border: Color32,
    pub border_soft: Color32,
    pub accent: Color32,
    /// 强调色的低饱和底（选中、标签背景）
    pub accent_dim: Color32,
    pub on_accent: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub faint: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub err: Color32,
    pub bubble_in: Color32,
    pub bubble_out: Color32,
    pub bubble_in_text: Color32,
    pub bubble_out_text: Color32,
    pub bubble_in_meta: Color32,
    pub bubble_out_meta: Color32,
    pub titlebar: Color32,
    /// 窗口按钮 hover 底色
    pub titlebar_hover: Color32,
    pub is_light: bool,
}

pub const DARK: Palette = Palette {
    bg: Color32::from_rgb(16, 18, 23),
    panel: Color32::from_rgb(22, 25, 32),
    card: Color32::from_rgb(30, 34, 43),
    card_hover: Color32::from_rgb(38, 43, 54),
    field: Color32::from_rgb(26, 30, 38),
    row_active: Color32::from_rgb(34, 47, 65),
    row_active_text: Color32::from_rgb(255, 255, 255),
    border: Color32::from_rgb(38, 43, 54),
    border_soft: Color32::from_rgb(31, 35, 44),
    accent: Color32::from_rgb(59, 130, 246),
    accent_dim: Color32::from_rgb(30, 58, 95),
    on_accent: Color32::from_rgb(255, 255, 255),
    text: Color32::from_rgb(231, 236, 245),
    muted: Color32::from_rgb(150, 160, 176),
    faint: Color32::from_rgb(106, 115, 132),
    ok: Color32::from_rgb(74, 199, 143),
    warn: Color32::from_rgb(240, 180, 78),
    err: Color32::from_rgb(240, 110, 110),
    bubble_in: Color32::from_rgb(34, 39, 49),
    bubble_out: Color32::from_rgb(47, 111, 208),
    bubble_in_text: Color32::from_rgb(232, 236, 245),
    bubble_out_text: Color32::from_rgb(255, 255, 255),
    bubble_in_meta: Color32::from_rgb(136, 147, 166),
    bubble_out_meta: Color32::from_rgb(206, 223, 246),
    titlebar: Color32::from_rgb(22, 25, 32),
    titlebar_hover: Color32::from_rgb(40, 45, 56),
    is_light: false,
};

pub const LIGHT: Palette = Palette {
    bg: Color32::from_rgb(244, 246, 250),
    panel: Color32::from_rgb(255, 255, 255),
    card: Color32::from_rgb(240, 242, 247),
    card_hover: Color32::from_rgb(232, 235, 241),
    field: Color32::from_rgb(255, 255, 255),
    row_active: Color32::from_rgb(226, 237, 252),
    row_active_text: Color32::from_rgb(29, 78, 216),
    border: Color32::from_rgb(226, 230, 237),
    border_soft: Color32::from_rgb(236, 239, 244),
    accent: Color32::from_rgb(37, 99, 235),
    accent_dim: Color32::from_rgb(219, 232, 253),
    on_accent: Color32::from_rgb(255, 255, 255),
    text: Color32::from_rgb(25, 29, 37),
    muted: Color32::from_rgb(95, 103, 117),
    faint: Color32::from_rgb(139, 148, 164),
    ok: Color32::from_rgb(22, 163, 106),
    warn: Color32::from_rgb(183, 121, 15),
    err: Color32::from_rgb(216, 66, 66),
    bubble_in: Color32::from_rgb(255, 255, 255),
    bubble_out: Color32::from_rgb(43, 105, 203),
    bubble_in_text: Color32::from_rgb(26, 30, 38),
    bubble_out_text: Color32::from_rgb(255, 255, 255),
    bubble_in_meta: Color32::from_rgb(140, 149, 165),
    bubble_out_meta: Color32::from_rgb(214, 230, 250),
    titlebar: Color32::from_rgb(255, 255, 255),
    titlebar_hover: Color32::from_rgb(233, 236, 242),
    is_light: true,
};

thread_local! {
    static ACTIVE: std::cell::Cell<Palette> = const { std::cell::Cell::new(DARK) };
}

/// 按 egui 解析出来的主题刷新当前调色板。每帧开头调用一次。
pub fn sync(theme: Theme) {
    let palette = match theme {
        Theme::Dark => DARK,
        Theme::Light => LIGHT,
    };
    ACTIVE.with(|cell| cell.set(palette));
}

pub fn palette() -> Palette {
    ACTIVE.with(|cell| cell.get())
}

// ---- 令牌访问器 ----------------------------------------------------------
// 写成函数而不是常量，才能随主题切换；调用点形如 `theme::bg()`。

pub fn bg() -> Color32 {
    palette().bg
}
pub fn panel() -> Color32 {
    palette().panel
}
pub fn card() -> Color32 {
    palette().card
}
pub fn card_hover() -> Color32 {
    palette().card_hover
}
pub fn field() -> Color32 {
    palette().field
}
pub fn row_active() -> Color32 {
    palette().row_active
}
pub fn row_active_text() -> Color32 {
    palette().row_active_text
}
pub fn border() -> Color32 {
    palette().border
}
pub fn border_soft() -> Color32 {
    palette().border_soft
}
pub fn accent() -> Color32 {
    palette().accent
}
pub fn accent_dim() -> Color32 {
    palette().accent_dim
}
pub fn on_accent() -> Color32 {
    palette().on_accent
}
pub fn text() -> Color32 {
    palette().text
}
pub fn muted() -> Color32 {
    palette().muted
}
pub fn faint() -> Color32 {
    palette().faint
}
pub fn ok() -> Color32 {
    palette().ok
}
pub fn warn() -> Color32 {
    palette().warn
}
pub fn err() -> Color32 {
    palette().err
}
pub fn titlebar() -> Color32 {
    palette().titlebar
}
pub fn titlebar_hover() -> Color32 {
    palette().titlebar_hover
}

// ---- 圆角 ---------------------------------------------------------------
// 只留三级，界面里所有圆角都从这三个值取，避免出现 5/7/9/12/16 混着用的碎片感。

/// 小控件：菜单项、图标按钮、输入框内块。
pub const RADIUS_SM: f32 = 6.0;
/// 常规块：按钮、列表行、气泡。
pub const RADIUS_MD: f32 = 8.0;
/// 大块：弹窗、下拉菜单、窗口本身。
pub const RADIUS_LG: f32 = 10.0;

/// 给颜色叠加透明度（用于分隔线、内层玻璃块）。
pub fn alpha(color: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), a)
}

// ---- 主题偏好 ------------------------------------------------------------

/// 配置里的 `theme` 字符串 → egui 的偏好枚举。
pub fn preference(value: &str) -> ThemePreference {
    match value {
        "light" => ThemePreference::Light,
        "dark" => ThemePreference::Dark,
        _ => ThemePreference::System,
    }
}

/// 偏好枚举 → 配置字符串（写回 `config.json`）。
pub fn preference_key(value: ThemePreference) -> &'static str {
    match value {
        ThemePreference::Light => "light",
        ThemePreference::Dark => "dark",
        ThemePreference::System => "system",
    }
}

pub fn preference_label(value: ThemePreference) -> &'static str {
    match value {
        ThemePreference::Light => crate::i18n::tr("浅色"),
        ThemePreference::Dark => crate::i18n::tr("深色"),
        ThemePreference::System => crate::i18n::tr("跟随系统"),
    }
}

// ---- 字体 ----------------------------------------------------------------

/// 候选中文字体（只挑单体 ttf/otf，避免 ttc 字体集解析失败）。
const FONT_CANDIDATES: &[&str] = &[
    "C:/Windows/Fonts/Deng.ttf",
    "C:/Windows/Fonts/simhei.ttf",
    "C:/Windows/Fonts/Noto Sans SC (TrueType).otf",
    "C:/Windows/Fonts/simsunb.ttf",
    "/System/Library/Fonts/Supplemental/Songti.ttf",
    "/System/Library/Fonts/STHeiti Light.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJKsc-Regular.otf",
    "/usr/share/fonts/truetype/noto/NotoSansSC-Regular.ttf",
    "/usr/share/fonts/truetype/wqy/wqy-microhei.ttf",
];

fn load_cjk_font() -> Option<(String, Vec<u8>)> {
    if let Ok(path) = std::env::var("FEIGECRAFT_FONT") {
        if let Ok(bytes) = std::fs::read(&path) {
            return Some(("cjk-user".to_string(), bytes));
        }
    }
    for candidate in FONT_CANDIDATES {
        if let Ok(bytes) = std::fs::read(candidate) {
            return Some(("cjk".to_string(), bytes));
        }
    }
    None
}

/// 系统 emoji 字体。egui 自带的是很小的子集，❤ ☀ ⭐ 这类常见符号会掉成方框，
/// 用系统字体兜底才能覆盖完整的表情面板。
const EMOJI_CANDIDATES: &[&str] = &[
    "C:/Windows/Fonts/seguiemj.ttf",
    "/System/Library/Fonts/Apple Color Emoji.ttc",
    "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf",
    "/usr/share/fonts/noto/NotoColorEmoji.ttf",
];

fn load_emoji_font() -> Option<Vec<u8>> {
    for candidate in EMOJI_CANDIDATES {
        if let Ok(bytes) = std::fs::read(candidate) {
            return Some(bytes);
        }
    }
    None
}

pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    if let Some((name, bytes)) = load_cjk_font() {
        fonts.font_data.insert(name.clone(), FontData::from_owned(bytes));
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, name.clone());
        fonts.families.entry(FontFamily::Monospace).or_default().push(name);
    } else {
        tracing::warn!("未找到中文字体，可在环境变量 FEIGECRAFT_FONT 指定 ttf/otf 路径");
    }
    if let Some(bytes) = load_emoji_font() {
        fonts.font_data.insert("emoji-system".to_string(), FontData::from_owned(bytes));
        // 放在末尾：只是补齐缺失字形，不改变中文/拉丁的字体选择。
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .push("emoji-system".to_string());
    }
    ctx.set_fonts(fonts);
}

// ---- 样式 ----------------------------------------------------------------

fn style_for(p: Palette) -> egui::Style {
    let mut visuals = if p.is_light { egui::Visuals::light() } else { egui::Visuals::dark() };

    visuals.panel_fill = p.bg;
    visuals.window_fill = p.panel;
    visuals.faint_bg_color = p.card;
    visuals.extreme_bg_color = if p.is_light { Color32::WHITE } else { Color32::from_rgb(11, 13, 18) };
    visuals.override_text_color = Some(p.text);
    visuals.selection.bg_fill = p.accent_dim;
    visuals.selection.stroke = Stroke::new(1.0_f32, p.accent);
    visuals.hyperlink_color = p.accent;
    visuals.window_stroke = Stroke::new(1.0_f32, p.border);
    visuals.window_rounding = egui::Rounding::same(RADIUS_LG);
    visuals.menu_rounding = egui::Rounding::same(RADIUS_LG);
    // 弹窗阴影：深色用更实的黑，浅色用更淡的灰，否则浅色主题会显得脏。
    let shadow = if p.is_light {
        egui::epaint::Shadow {
            offset: egui::vec2(0.0, 10.0),
            blur: 32.0,
            spread: 0.0,
            color: Color32::from_rgba_unmultiplied(15, 23, 42, 34),
        }
    } else {
        egui::epaint::Shadow {
            offset: egui::vec2(0.0, 10.0),
            blur: 32.0,
            spread: 0.0,
            color: Color32::from_rgba_unmultiplied(0, 0, 0, 140),
        }
    };
    visuals.window_shadow = shadow;
    visuals.popup_shadow = shadow;

    visuals.widgets.noninteractive.bg_fill = p.panel;
    visuals.widgets.noninteractive.weak_bg_fill = p.panel;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, p.border_soft);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, p.text);

    visuals.widgets.inactive.bg_fill = p.card;
    visuals.widgets.inactive.weak_bg_fill = p.card;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, p.border);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, p.text);

    visuals.widgets.hovered.bg_fill = p.card_hover;
    visuals.widgets.hovered.weak_bg_fill = p.card_hover;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, p.accent);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, p.text);

    visuals.widgets.active.bg_fill = p.accent_dim;
    visuals.widgets.active.weak_bg_fill = p.accent_dim;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, p.accent);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0_f32, p.text);

    visuals.widgets.open.bg_fill = p.card_hover;
    visuals.widgets.open.weak_bg_fill = p.card_hover;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0_f32, p.border);

    let mut style = egui::Style { visuals, ..Default::default() };
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.spacing.menu_margin = egui::Margin::same(6.0);
    style.spacing.scroll.floating = true;
    style.spacing.scroll.bar_width = 8.0;
    style.spacing.scroll.floating_width = 3.0;
    style.spacing.scroll.bar_inner_margin = 4.0;
    style.spacing.scroll.handle_min_length = 28.0;
    style.text_styles.insert(TextStyle::Heading, FontId::new(20.0, FontFamily::Proportional));
    style.text_styles.insert(TextStyle::Body, FontId::new(14.0, FontFamily::Proportional));
    style.text_styles.insert(TextStyle::Button, FontId::new(14.0, FontFamily::Proportional));
    style.text_styles.insert(TextStyle::Small, FontId::new(11.5, FontFamily::Proportional));
    style
}

/// 一次装好深、浅两套 Style；egui 会按当前 `Theme` 自动取用。
pub fn install_style(ctx: &egui::Context) {
    ctx.set_style_of(Theme::Dark, std::sync::Arc::new(style_for(DARK)));
    ctx.set_style_of(Theme::Light, std::sync::Arc::new(style_for(LIGHT)));
}

/// 根据标识生成稳定的头像底色，保证同一台设备每次打开颜色一致。
pub fn avatar_color(key: &str) -> Color32 {
    const PALETTE: [(u8, u8, u8); 8] = [
        (86, 124, 214),
        (52, 158, 152),
        (198, 122, 66),
        (148, 106, 200),
        (199, 92, 118),
        (104, 152, 74),
        (196, 154, 60),
        (86, 128, 160),
    ];
    let mut hash: u32 = 2_166_136_261;
    for byte in key.as_bytes() {
        hash ^= *byte as u32;
        hash = hash.wrapping_mul(16_777_619);
    }
    let (r, g, b) = PALETTE[(hash as usize) % PALETTE.len()];
    Color32::from_rgb(r, g, b)
}
