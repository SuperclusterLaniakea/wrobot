//! 统一主题与界面组件。
//!
//! 所有颜色、间距、字号集中在这里，页面里不再各写各的 RGB 常量；
//! 深色 / 浅色两套调色板共用同一组语义字段，切换主题只需重设一次 egui Style。

use eframe::egui;
use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Margin, Pos2, Rect, Response, RichText,
    Sense, Stroke, Ui, Vec2, WidgetInfo, WidgetType,
};

/// 语义化调色板：页面代码只引用角色（背景 / 卡片 / 强调色…），不关心具体色值。
#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    /// 窗口底色（最外层）。
    pub bg: Color32,
    /// 侧栏 / 参数栏等面板底色。
    pub panel: Color32,
    /// 卡片底色。
    pub card: Color32,
    /// 卡片悬停 / 次级色块。
    pub card_hover: Color32,
    /// 输入框、代码块底色。
    pub input_bg: Color32,
    /// 卡片描边。
    pub stroke: Color32,
    /// 强描边（分隔线、输入框边框）。
    pub stroke_strong: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub accent: Color32,
    /// 强调色的浅底（选中项、图标底板）。
    pub accent_soft: Color32,
    pub success: Color32,
    pub warning: Color32,
    pub danger: Color32,
    /// 画布外围底色。
    pub canvas_bg: Color32,
    /// 纸张颜色。
    pub paper: Color32,
    /// 预览墨迹颜色。
    pub ink: Color32,
    pub shadow: egui::epaint::Shadow,
}

/// 取调色板。
pub fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            dark: true,
            bg: Color32::from_rgb(17, 20, 25),
            panel: Color32::from_rgb(22, 26, 33),
            card: Color32::from_rgb(29, 34, 42),
            card_hover: Color32::from_rgb(37, 43, 53),
            input_bg: Color32::from_rgb(20, 23, 29),
            stroke: Color32::from_rgb(50, 57, 69),
            stroke_strong: Color32::from_rgb(72, 81, 96),
            text: Color32::from_rgb(232, 236, 243),
            muted: Color32::from_rgb(146, 155, 170),
            accent: Color32::from_rgb(104, 156, 255),
            accent_soft: Color32::from_rgb(38, 54, 84),
            success: Color32::from_rgb(86, 196, 126),
            warning: Color32::from_rgb(232, 176, 84),
            danger: Color32::from_rgb(232, 108, 98),
            canvas_bg: Color32::from_rgb(12, 15, 19),
            paper: Color32::from_rgb(247, 246, 242),
            ink: Color32::from_rgb(35, 40, 50),
            shadow: egui::epaint::Shadow {
                offset: [0, 10],
                blur: 28,
                spread: 0,
                color: Color32::from_black_alpha(90),
            },
        }
    } else {
        Palette {
            dark: false,
            bg: Color32::from_rgb(242, 245, 249),
            panel: Color32::from_rgb(250, 251, 253),
            card: Color32::from_rgb(255, 255, 255),
            card_hover: Color32::from_rgb(245, 248, 252),
            input_bg: Color32::from_rgb(247, 249, 252),
            stroke: Color32::from_rgb(225, 230, 238),
            stroke_strong: Color32::from_rgb(203, 211, 222),
            text: Color32::from_rgb(30, 37, 48),
            muted: Color32::from_rgb(108, 119, 136),
            accent: Color32::from_rgb(48, 105, 214),
            accent_soft: Color32::from_rgb(232, 240, 255),
            success: Color32::from_rgb(32, 150, 88),
            warning: Color32::from_rgb(198, 128, 28),
            danger: Color32::from_rgb(206, 70, 60),
            canvas_bg: Color32::from_rgb(230, 234, 240),
            paper: Color32::from_rgb(255, 255, 255),
            ink: Color32::from_rgb(28, 33, 42),
            shadow: egui::epaint::Shadow {
                offset: [0, 6],
                blur: 20,
                spread: 0,
                color: Color32::from_black_alpha(28),
            },
        }
    }
}

/// 应用主题（深色 / 浅色）。只在切换时调用，不要每帧重设。
pub fn apply(ctx: &egui::Context, dark: bool) {
    let pal = palette(dark);
    let theme = if dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    };
    ctx.set_theme(theme);
    let mut style = (*ctx.style_of(theme)).clone();
    style.animation_time = 0.14;
    style.visuals = visuals(&pal);

    let mut text = std::collections::BTreeMap::new();
    text.insert(egui::TextStyle::Heading, FontId::proportional(20.0));
    text.insert(egui::TextStyle::Body, FontId::proportional(14.0));
    text.insert(egui::TextStyle::Button, FontId::proportional(13.5));
    text.insert(egui::TextStyle::Small, FontId::proportional(11.0));
    text.insert(
        egui::TextStyle::Monospace,
        FontId::monospace(12.0),
    );
    style.text_styles = text;

    style.spacing.item_spacing = Vec2::new(8.0, 8.0);
    style.spacing.button_padding = Vec2::new(11.0, 5.0);
    style.spacing.window_margin = Margin::symmetric(14, 12);
    style.spacing.menu_margin = Margin::symmetric(7, 6);
    style.spacing.indent = 18.0;
    style.spacing.interact_size = Vec2::new(38.0, 28.0);
    style.spacing.slider_width = 150.0;
    style.spacing.combo_width = 150.0;
    style.spacing.icon_width = 16.0;
    style.spacing.icon_width_inner = 9.0;
    style.spacing.icon_spacing = 6.0;
    style.spacing.scroll.bar_width = 9.0;
    style.spacing.scroll.floating = true;
    style.spacing.scroll.content_margin = Margin::symmetric(0, 4);

    ctx.set_style_of(theme, style);
}

fn visuals(pal: &Palette) -> egui::Visuals {
    let mut v = if pal.dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    let corner = CornerRadius::same(8);
    v.dark_mode = pal.dark;
    v.override_text_color = Some(pal.text);
    v.weak_text_color = Some(pal.muted);
    v.panel_fill = pal.bg;
    v.window_fill = pal.panel;
    v.window_stroke = Stroke::new(1.0, pal.stroke);
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(9);
    v.window_shadow = pal.shadow;
    v.popup_shadow = pal.shadow;
    v.extreme_bg_color = pal.input_bg;
    v.faint_bg_color = pal.card_hover;
    v.code_bg_color = pal.input_bg;
    v.text_edit_bg_color = Some(pal.input_bg);
    v.hyperlink_color = pal.accent;
    v.warn_fg_color = pal.warning;
    v.error_fg_color = pal.danger;
    v.selection = egui::style::Selection {
        bg_fill: pal.accent.gamma_multiply(0.28),
        stroke: Stroke::new(1.0, pal.accent),
    };
    v.button_frame = true;
    v.collapsing_header_frame = true;
    v.striped = false;
    v.slider_trailing_fill = true;
    v.handle_shape = egui::style::HandleShape::Circle;
    v.interact_cursor = Some(CursorIcon::PointingHand);
    v.indent_has_left_vline = true;

    let w = egui::style::Widgets {
        noninteractive: egui::style::WidgetVisuals {
            bg_fill: pal.panel,
            weak_bg_fill: pal.panel,
            bg_stroke: Stroke::new(1.0, pal.stroke),
            corner_radius: corner,
            fg_stroke: Stroke::new(1.0, pal.text),
            expansion: 0.0,
        },
        inactive: egui::style::WidgetVisuals {
            bg_fill: pal.input_bg,
            weak_bg_fill: pal.card,
            bg_stroke: Stroke::new(1.0, pal.stroke),
            corner_radius: corner,
            fg_stroke: Stroke::new(1.0, pal.text),
            expansion: 0.0,
        },
        hovered: egui::style::WidgetVisuals {
            bg_fill: pal.card_hover,
            weak_bg_fill: pal.card_hover,
            bg_stroke: Stroke::new(1.0, pal.stroke_strong),
            corner_radius: corner,
            fg_stroke: Stroke::new(1.2, pal.text),
            expansion: 0.0,
        },
        active: egui::style::WidgetVisuals {
            bg_fill: pal.accent,
            weak_bg_fill: pal.accent_soft,
            bg_stroke: Stroke::new(1.0, pal.accent),
            corner_radius: corner,
            fg_stroke: Stroke::new(1.4, pal.text),
            expansion: 0.0,
        },
        open: egui::style::WidgetVisuals {
            bg_fill: pal.accent_soft,
            weak_bg_fill: pal.card_hover,
            bg_stroke: Stroke::new(1.0, pal.stroke_strong),
            corner_radius: corner,
            fg_stroke: Stroke::new(1.0, pal.text),
            expansion: 0.0,
        },
    };
    v.widgets = w;
    v
}

// ── 组件 ────────────────────────────────────────────────

/// 卡片容器：白底 / 深灰底 + 细描边 + 圆角，页面里的参数分组都放这里面。
pub fn card<R>(ui: &mut Ui, pal: &Palette, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(pal.card)
        .stroke(Stroke::new(1.0, pal.stroke))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// 可折叠卡片标题（用于收纳次要参数，保持右栏清爽）。
pub fn collapsible_card<R>(
    ui: &mut Ui,
    pal: &Palette,
    title: &str,
    default_open: bool,
    add: impl FnOnce(&mut Ui) -> R,
) -> Option<R> {
    let mut inner = None;
    egui::Frame::new()
        .fill(pal.card)
        .stroke(Stroke::new(1.0, pal.stroke))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::CollapsingHeader::new(RichText::new(title).size(13.0).strong().color(pal.text))
                .id_salt(title)
                .default_open(default_open)
                .show(ui, |ui| {
                    inner = Some(add(ui));
                });
        });
    inner
}

/// 分区标题：左侧短竖条 + 标题 + 可选说明。
pub fn section(ui: &mut Ui, pal: &Palette, title: &str, subtitle: Option<&str>) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let h = if subtitle.is_some() { 30.0 } else { 18.0 };
        let (rect, _) = ui.allocate_exact_size(Vec2::new(3.0, h), Sense::hover());
        ui.painter()
            .rect_filled(rect, CornerRadius::same(2), pal.accent);
        ui.add_space(2.0);
        ui.vertical(|ui| {
            ui.label(RichText::new(title).size(14.0).strong().color(pal.text));
            if let Some(s) = subtitle {
                ui.label(RichText::new(s).size(10.5).color(pal.muted));
            }
        });
    });
    ui.add_space(2.0);
}

/// 侧栏导航项：中文图标块 + 标题 + 说明，选中时整行浅色底 + 左侧强调条。
pub fn nav_item(
    ui: &mut Ui,
    pal: &Palette,
    icon: &str,
    title: &str,
    hint: &str,
    selected: bool,
) -> Response {
    let height = if hint.is_empty() { 36.0 } else { 42.0 };
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), height),
        Sense::click(),
    );
    // 自定义绘制的导航项要手动登记无障碍信息，屏幕阅读器/UI 自动化才能识别。
    resp.widget_info(|| {
        WidgetInfo::selected(WidgetType::Button, true, selected, format!("{title} {hint}"))
    });
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        if selected {
            p.rect_filled(
                rect.shrink2(Vec2::new(0.0, 1.0)),
                CornerRadius::same(9),
                pal.accent_soft,
            );
            p.rect_filled(
                Rect::from_min_size(
                    rect.left_top() + Vec2::new(0.0, 9.0),
                    Vec2::new(3.0, rect.height() - 18.0),
                ),
                CornerRadius::same(2),
                pal.accent,
            );
        } else if resp.hovered() {
            p.rect_filled(
                rect.shrink2(Vec2::new(0.0, 1.0)),
                CornerRadius::same(9),
                pal.card_hover,
            );
        }
        let tile = Rect::from_min_size(
            rect.left_center() + Vec2::new(10.0, -14.0),
            Vec2::splat(28.0),
        );
        p.rect_filled(
            tile,
            CornerRadius::same(8),
            if selected { pal.accent } else { pal.card_hover },
        );
        p.text(
            tile.center(),
            Align2::CENTER_CENTER,
            icon,
            FontId::proportional(14.0),
            if selected {
                Color32::WHITE
            } else {
                pal.muted
            },
        );
        let tx = tile.right() + 9.0;
        if hint.is_empty() {
            p.text(
                Pos2::new(tx, rect.center().y),
                Align2::LEFT_CENTER,
                title,
                FontId::proportional(13.5),
                if selected { pal.accent } else { pal.text },
            );
        } else {
            p.text(
                Pos2::new(tx, rect.center().y - 7.5),
                Align2::LEFT_CENTER,
                title,
                FontId::proportional(13.5),
                if selected { pal.accent } else { pal.text },
            );
            p.text(
                Pos2::new(tx, rect.center().y + 8.0),
                Align2::LEFT_CENTER,
                hint,
                FontId::proportional(10.0),
                pal.muted,
            );
        }
    }
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// 小圆点状态 + 文本。
pub fn status_dot(ui: &mut Ui, color: Color32, text: impl Into<String>) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
        ui.painter().circle_filled(rect.center(), 3.5, color);
        ui.label(RichText::new(text.into()).size(11.0).color(color));
    });
}

/// 圆角小标签（统计 / 状态）。
pub fn pill(ui: &mut Ui, text: &str, fg: Color32, bg: Color32) -> Response {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), FontId::proportional(10.5), fg);
    let size = Vec2::new(galley.size().x + 14.0, 19.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(9), bg);
    ui.painter().galley(
        Pos2::new(
            rect.center().x - galley.size().x / 2.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        fg,
    );
    resp
}

/// 统计小格：左标签右数值，放在卡片里横排。
pub fn stat(ui: &mut Ui, pal: &Palette, label: &str, value: &str, color: Color32) {
    egui::Frame::new()
        .fill(pal.card_hover)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(9, 6))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(label).size(10.0).color(pal.muted));
                ui.label(RichText::new(value).size(14.0).strong().color(color));
            });
        });
}

/// 主按钮（开始绘制）。
pub fn primary_button(pal: &Palette, text: &str) -> egui::Button<'static> {
    egui::Button::new(
        RichText::new(text.to_owned())
            .size(14.0)
            .strong()
            .color(Color32::WHITE),
    )
    .fill(pal.accent)
    .stroke(Stroke::NONE)
    .corner_radius(CornerRadius::same(8))
    .min_size(Vec2::new(104.0, 31.0))
}

/// 次按钮（浅底描边）。
pub fn soft_button(pal: &Palette, text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_owned()).size(12.5).color(pal.text))
        .fill(pal.card)
        .stroke(Stroke::new(1.0, pal.stroke_strong))
        .corner_radius(CornerRadius::same(8))
        .min_size(Vec2::new(0.0, 29.0))
}

/// 危险按钮（停止 / 清空）。
pub fn danger_button(pal: &Palette, text: &str, filled: bool) -> egui::Button<'static> {
    if filled {
        egui::Button::new(
            RichText::new(text.to_owned())
                .size(12.5)
                .strong()
                .color(Color32::WHITE),
        )
        .fill(pal.danger)
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(8))
    } else {
        egui::Button::new(RichText::new(text.to_owned()).size(12.5).color(pal.danger))
            .fill(pal.card)
            .stroke(Stroke::new(1.0, pal.danger.gamma_multiply(0.55)))
            .corner_radius(CornerRadius::same(8))
    }
}

/// 细分割线。
pub fn divider(ui: &mut Ui, pal: &Palette) {
    ui.add_space(3.0);
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0, pal.stroke);
    ui.add_space(3.0);
}

/// 带外框的提示条（未连接 / 警告 / 说明）。
pub fn notice(ui: &mut Ui, pal: &Palette, text: &str, tone: Color32) {
    egui::Frame::new()
        .fill(tone.gamma_multiply(if pal.dark { 0.16 } else { 0.08 }))
        .stroke(Stroke::new(1.0, tone.gamma_multiply(0.45)))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(10, 7))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(text).size(11.0).color(tone));
        });
}
