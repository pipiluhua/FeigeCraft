//! 复用的小部件：头像、时间格式、图标按钮。

use eframe::egui::{self, Align2, Color32, FontId, Response, RichText, Sense, Stroke, Vec2};

use crate::i18n::{tf, tr};
use crate::theme;

/// 圆形头像：首字符 + 稳定底色，右下角可带在线小绿点。
pub fn avatar(ui: &mut egui::Ui, size: f32, name: &str, key: &str, online: bool) {
    avatar_on(ui, size, name, key, online, theme::panel());
}

/// 同上，但显式指定在线小圆点的描边色（贴在不同底色上时用它压住缝隙）。
pub fn avatar_on(
    ui: &mut egui::Ui,
    size: f32,
    name: &str,
    key: &str,
    online: bool,
    ring: Color32,
) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    paint_avatar(ui.painter(), rect, name, key, online, ring);
}

/// 纯绘制版头像：参数给矩形，不占用布局，供手绘气泡复用。
pub fn paint_avatar(
    painter: &egui::Painter,
    rect: egui::Rect,
    name: &str,
    key: &str,
    online: bool,
    ring: Color32,
) {
    let size = rect.width();
    let center = rect.center();
    let radius = size * 0.5;
    painter.circle_filled(center, radius, theme::avatar_color(key));

    let initial: String = name
        .chars()
        .next()
        .map(|ch| ch.to_uppercase().to_string())
        .unwrap_or_else(|| "?".to_string());
    painter.text(
        center,
        Align2::CENTER_CENTER,
        initial,
        FontId::proportional(size * 0.42),
        Color32::from_rgb(255, 255, 255),
    );

    if online {
        let dot = center + Vec2::new(radius * 0.68, radius * 0.68);
        painter.circle_filled(dot, size * 0.15, ring);
        painter.circle_filled(dot, size * 0.105, theme::ok());
    }
}

/// 界面图标。全部矢量绘制：字体里的 🔍 📎 😊 在不同系统上会被替换成
/// 粗细不一的实心剪影（NotoEmoji 的彩色字重尤其明显），统一画出来才够干净。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Search,
    Plus,
    Refresh,
    Sliders,
    Smile,
    More,
    Clipboard,
}

impl Icon {
    fn paint(self, painter: &egui::Painter, rect: egui::Rect, color: Color32) {
        let map = |x: f32, y: f32| {
            egui::pos2(rect.min.x + rect.width() * x, rect.min.y + rect.height() * y)
        };
        let stroke = Stroke::new(1.5_f32, color);
        match self {
            Icon::Search => {
                let center = map(0.42, 0.42);
                let radius = rect.width() * 0.26;
                painter.circle_stroke(center, radius, stroke);
                painter.line_segment([map(0.62, 0.62), map(0.84, 0.84)], stroke);
            }
            Icon::Plus => {
                painter.line_segment([map(0.5, 0.18), map(0.5, 0.82)], stroke);
                painter.line_segment([map(0.18, 0.5), map(0.82, 0.5)], stroke);
            }
            Icon::Refresh => {
                // 3/4 圆弧 + 箭头，比整圆更像「刷新」
                let center = map(0.5, 0.52);
                let radius = rect.width() * 0.3;
                let mut points = Vec::new();
                for step in 0..=24 {
                    let t = step as f32 / 24.0;
                    let angle = -0.35 + t * std::f32::consts::TAU * 0.78;
                    points.push(egui::pos2(
                        center.x + radius * angle.cos(),
                        center.y + radius * angle.sin(),
                    ));
                }
                painter.add(egui::Shape::line(points, stroke));
                let tip = map(0.74, 0.22);
                painter.line_segment([tip, tip + Vec2::new(-0.14, 0.02) * rect.width()], stroke);
                painter.line_segment([tip, tip + Vec2::new(0.02, 0.14) * rect.width()], stroke);
            }
            Icon::Sliders => {
                for (y, knob) in [(0.3_f32, 0.62_f32), (0.5, 0.36), (0.7, 0.54)] {
                    painter.line_segment([map(0.16, y), map(0.84, y)], stroke);
                    painter.circle_filled(map(knob, y), rect.width() * 0.075, color);
                }
            }
            Icon::More => {
                for x in [0.24_f32, 0.5, 0.76] {
                    painter.circle_filled(map(x, 0.5), rect.width() * 0.075, color);
                }
            }
            Icon::Smile => {
                let center = map(0.5, 0.5);
                let radius = rect.width() * 0.33;
                painter.circle_stroke(center, radius, stroke);
                painter.circle_filled(map(0.39, 0.41), rect.width() * 0.045, color);
                painter.circle_filled(map(0.61, 0.41), rect.width() * 0.045, color);
                let mut mouth = Vec::new();
                for step in 0..=16 {
                    let t = step as f32 / 16.0;
                    let angle = std::f32::consts::PI * (0.18 + t * 0.64);
                    mouth.push(egui::pos2(
                        center.x + radius * 0.72 * angle.cos(),
                        center.y + radius * 0.72 * angle.sin(),
                    ));
                }
                painter.add(egui::Shape::line(mouth, stroke));
            }
            Icon::Clipboard => {
                // 剪贴板：主体圆角矩形 + 顶部小夹子
                let body = egui::Rect::from_min_max(map(0.24, 0.32), map(0.76, 0.86));
                painter.rect_stroke(
                    body,
                    egui::Rounding::same(rect.width() * 0.08),
                    stroke,
                );
                painter.line_segment([map(0.4, 0.24), map(0.4, 0.32)], stroke);
                painter.line_segment([map(0.6, 0.24), map(0.6, 0.32)], stroke);
                painter.line_segment([map(0.38, 0.26), map(0.62, 0.26)], stroke);
            }
        }
    }
}

/// 无边框图标按钮（用于工具栏上的表情 / 附件 / 更多）。
pub fn icon_button(ui: &mut egui::Ui, icon: Icon, tip: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, egui::Rounding::same(theme::RADIUS_SM), theme::card_hover());
    }
    let glyph = egui::Rect::from_center_size(rect.center(), Vec2::splat(16.0));
    Icon::paint(icon, ui.painter(), glyph, theme::muted());
    response.on_hover_text(tip)
}

/// 纯图标（不可点），用于装饰性位置。
pub fn icon(ui: &mut egui::Ui, icon: Icon, size: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    Icon::paint(icon, ui.painter(), rect, color);
}

/// `14:32`
pub fn time_hm(ms: u64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_millis_opt(ms as i64).single() {
        Some(dt) => dt.format("%H:%M").to_string(),
        None => String::new(),
    }
}

thread_local! {
    /// 本帧的「今天」。`Local::now()` 要走一遍时区表，逐条消息调用在长会话里
    /// 是实打实的开销（400 条 × 60fps = 每秒两万多次），改成每帧只取一次。
    static TODAY: std::cell::Cell<Option<chrono::NaiveDate>> = const { std::cell::Cell::new(None) };
}

/// 每帧开头调一次，刷新「今天」。
pub fn begin_frame() {
    TODAY.with(|cell| cell.set(Some(chrono::Local::now().date_naive())));
}

fn today() -> chrono::NaiveDate {
    TODAY.with(|cell| cell.get()).unwrap_or_else(|| chrono::Local::now().date_naive())
}

/// 消息流里的日期分隔：「今天」「昨天」「11月3日」「2025年11月3日」
pub fn day_label(ms: u64) -> String {
    use chrono::{Datelike, Local, TimeZone};
    let Some(dt) = Local.timestamp_millis_opt(ms as i64).single() else {
        return String::new();
    };
    let today = today();
    let date = dt.date_naive();
    if date == today {
        tr("今天").to_string()
    } else if Some(date) == today.pred_opt() {
        tr("昨天").to_string()
    } else if date.year() == today.year() {
        tf(tr("{}月{}日"), &[&dt.month(), &dt.day()])
    } else {
        tf(tr("{}年{}月{}日"), &[&dt.year(), &dt.month(), &dt.day()])
    }
}

/// 会话列表右侧的时间：今天显示时分，昨天显示「昨天」，更早显示日期。
pub fn list_time(ms: u64) -> String {
    use chrono::{Datelike, Local, TimeZone};
    if ms == 0 {
        return String::new();
    }
    let Some(dt) = Local.timestamp_millis_opt(ms as i64).single() else {
        return String::new();
    };
    let today = today();
    let date = dt.date_naive();
    if date == today {
        dt.format("%H:%M").to_string()
    } else if Some(date) == today.pred_opt() {
        tr("昨天").to_string()
    } else if date.year() == today.year() {
        format!("{}/{}", dt.month(), dt.day())
    } else {
        format!("{}/{}/{}", dt.year() % 100, dt.month(), dt.day())
    }
}

/// 分隔线。
pub fn divider(ui: &mut egui::Ui, pad: f32) {
    ui.add_space(pad);
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 1.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, 0.0, theme::border());
    ui.add_space(pad);
}

/// 主按钮（蓝色实心）。
pub fn primary_button(text: &str, enabled: bool) -> egui::Button<'static> {
    let label = RichText::new(text.to_string())
        .size(13.5)
        .color(if enabled { theme::on_accent() } else { theme::faint() });
    egui::Button::new(label)
        .fill(if enabled { theme::accent() } else { theme::card() })
        .stroke(Stroke::new(
            1.0_f32,
            if enabled { theme::accent() } else { theme::border() },
        ))
        .rounding(egui::Rounding::same(theme::RADIUS_MD))
        .min_size(Vec2::new(76.0, 32.0))
}

/// 次按钮（描边）。
pub fn ghost_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).size(13.5).color(theme::text()))
        .fill(theme::card())
        .stroke(Stroke::new(1.0_f32, theme::border()))
        .rounding(egui::Rounding::same(theme::RADIUS_MD))
        .min_size(Vec2::new(64.0, 32.0))
}

/// 消息状态角标。**全部用画笔直接绘制**，不依赖字体里是否有 `✓` / `◌` 这类符号
/// （中文字体、系统 emoji 字体覆盖情况各不相同，用字符会掉成「豆腐块」）。
pub fn paint_status(painter: &egui::Painter, rect: egui::Rect, status: feigecraft_core::MessageStatus, color: Color32) {
    use feigecraft_core::MessageStatus;

    let center = rect.center();
    match status {
        MessageStatus::Sent => {
            // 两段折线的对勾
            let stroke = Stroke::new(1.5_f32, color);
            painter.line_segment(
                [center + Vec2::new(-3.6, 0.3), center + Vec2::new(-1.0, 2.9)],
                stroke,
            );
            painter.line_segment(
                [center + Vec2::new(-1.0, 2.9), center + Vec2::new(3.8, -2.6)],
                stroke,
            );
        }
        MessageStatus::Sending => {
            // 空心小圆：表示「在途」
            painter.circle_stroke(center, 3.6, Stroke::new(1.2_f32, color));
        }
        MessageStatus::Failed => {
            painter.text(
                center,
                Align2::CENTER_CENTER,
                "!",
                FontId::proportional(13.0),
                color,
            );
        }
    }
}

/// 文件图标：一张折角纸。同样是画出来的，不依赖 emoji 字体。
pub fn paint_file_icon(painter: &egui::Painter, rect: egui::Rect, bg: Color32, fg: Color32) {
    painter.rect_filled(rect, egui::Rounding::same(theme::RADIUS_MD), bg);
    let size = rect.width() * 0.46;
    let sheet = egui::Rect::from_center_size(rect.center(), Vec2::new(size, size * 1.24));
    let stroke = Stroke::new(1.3_f32, fg);
    painter.rect_stroke(sheet, egui::Rounding::same(2.0), stroke);
    // 三道横线代表正文
    for index in 0..3 {
        let y = sheet.min.y + sheet.height() * (0.36 + index as f32 * 0.2);
        painter.line_segment(
            [
                egui::pos2(sheet.min.x + sheet.width() * 0.2, y),
                egui::pos2(sheet.max.x - sheet.width() * 0.2, y),
            ],
            Stroke::new(1.0_f32, fg),
        );
    }
}

/// 品牌标记：一只折纸飞机。纯矢量绘制，任何字体环境下都不会变形。
pub fn paint_brand(painter: &egui::Painter, rect: egui::Rect, fg: Color32) {
    let map = |x: f32, y: f32| {
        egui::pos2(rect.min.x + rect.width() * x, rect.min.y + rect.height() * y)
    };
    let nose = map(0.96, 0.10);
    let tail = map(0.06, 0.48);
    let belly = map(0.58, 0.94);
    let crease = map(0.46, 0.55);

    painter.add(egui::Shape::convex_polygon(
        vec![tail, nose, belly],
        fg,
        Stroke::NONE,
    ));
    // 一道深浅差，让机身有折面感
    painter.add(egui::Shape::convex_polygon(
        vec![tail, nose, crease],
        theme::alpha(Color32::WHITE, 46),
        Stroke::NONE,
    ));
}

/// 窗口控制按钮的图形（最小化 / 最大化 / 关闭），坐标为 0..1 归一化。
pub fn paint_window_glyph(
    painter: &egui::Painter,
    rect: egui::Rect,
    kind: WindowGlyph,
    color: Color32,
) {
    let map = |x: f32, y: f32| {
        egui::pos2(rect.min.x + rect.width() * x, rect.min.y + rect.height() * y)
    };
    let stroke = Stroke::new(1.15_f32, color);
    match kind {
        WindowGlyph::Minimize => {
            painter.line_segment([map(0.20, 0.5), map(0.80, 0.5)], stroke);
        }
        WindowGlyph::Maximize => {
            let rect = egui::Rect::from_min_max(map(0.20, 0.20), map(0.80, 0.80));
            painter.rect_stroke(rect, egui::Rounding::same(1.5), stroke);
        }
        WindowGlyph::Restore => {
            painter.rect_stroke(
                egui::Rect::from_min_max(map(0.18, 0.32), map(0.68, 0.82)),
                egui::Rounding::same(1.5),
                stroke,
            );
            painter.line_segment([map(0.34, 0.20), map(0.82, 0.20)], stroke);
            painter.line_segment([map(0.82, 0.20), map(0.82, 0.68)], stroke);
        }
        WindowGlyph::Close => {
            painter.line_segment([map(0.24, 0.24), map(0.76, 0.76)], stroke);
            painter.line_segment([map(0.76, 0.24), map(0.24, 0.76)], stroke);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum WindowGlyph {
    Minimize,
    Maximize,
    Restore,
    Close,
}
