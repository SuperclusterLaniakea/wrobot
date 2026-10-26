//! 画布预览：把笔迹折线（`Vec<Polyline>`，页面毫米坐标）绘制到 egui。
//!
//! 提供两类绘制：
//! - `draw_canvas`：编辑态主画布，绘制纸张/网格/文字笔迹/背景参考图，并支持
//!   在「图片」页直接拖动画布移动选中图片；
//! - `draw_progress_page`：运行中进度页，整页显示，已完成笔迹黑色、未完成浅灰。

use egui::{
    Align2, Color32, CursorIcon, FontId, Key, Painter, PointerButton, Pos2, Rect, Stroke, Vec2,
};
use wrobot_core::render::Polyline;

use crate::state::{corner_mark_polylines, AppState};
use crate::theme::Palette;

/// 屏幕 ↔ 页面毫米 的映射（供手绘等交互把鼠标位置换算成毫米）。
pub struct PreviewMap {
    pub scale: f32,
    pub off_x: f32,
    pub off_y: f32,
}

impl PreviewMap {
    /// 屏幕坐标 → 页面毫米坐标。
    pub fn to_mm(&self, p: Pos2) -> (f64, f64) {
        let s = self.scale.max(0.0001) as f64;
        ((p.x - self.off_x) as f64 / s, (p.y - self.off_y) as f64 / s)
    }
    /// 页面毫米坐标 → 屏幕坐标。
    pub fn to_screen(&self, x: f64, y: f64) -> Pos2 {
        Pos2::new(self.off_x + x as f32 * self.scale, self.off_y + y as f32 * self.scale)
    }
}

/// 计算整页缩放映射。
///
/// - `top_left = false`（默认）：页面在可用区**居中**（标尺关闭、进度页使用）。
/// - `top_left = true`：页面**左上对齐**，仅留出 `margin` 边距贴着可用区左上角。
///   用于开启标尺时——标尺条就画在这条 `margin` 边距内，左侧只空出
///   「标尺条 + 小间隙」一小块，**不会在宽窗口下因居中而产生整片左侧空白**。
fn layout(avail: egui::Rect, page_w_mm: f64, page_h_mm: f64) -> (PreviewMap, Rect) {
    layout_opts(avail, page_w_mm, page_h_mm, false, 18.0)
}

fn layout_opts(
    avail: egui::Rect,
    page_w_mm: f64,
    page_h_mm: f64,
    top_left: bool,
    margin: f32,
) -> (PreviewMap, Rect) {
    let scale = ((avail.width() - 2.0 * margin) / page_w_mm as f32)
        .min((avail.height() - 2.0 * margin) / page_h_mm as f32)
        .max(0.01);
    let draw_w = page_w_mm as f32 * scale;
    let draw_h = page_h_mm as f32 * scale;
    let (off_x, off_y) = if top_left {
        (avail.min.x + margin, avail.min.y + margin)
    } else {
        (
            avail.min.x + (avail.width() - draw_w) / 2.0,
            avail.min.y + (avail.height() - draw_h) / 2.0,
        )
    };
    let map = PreviewMap {
        scale,
        off_x,
        off_y,
    };
    let page_rect = Rect::from_min_size(Pos2::new(off_x, off_y), Vec2::new(draw_w, draw_h));
    (map, page_rect)
}

/// 在「适应窗口」布局之上叠加缩放与平移。
///
/// `zoom = 1` 即适应窗口；缩放围绕页面中心，`pan` 是页面相对该中心位置的屏幕位移。
fn layout_view(
    avail: Rect,
    page_w_mm: f64,
    page_h_mm: f64,
    ruler_on: bool,
    zoom: f32,
    pan: (f32, f32),
) -> (PreviewMap, Rect) {
    let (fit, fit_rect) = if ruler_on {
        layout_opts(avail, page_w_mm, page_h_mm, false, RULER_PX + 6.0)
    } else {
        layout(avail, page_w_mm, page_h_mm)
    };
    let z = zoom.clamp(1.0, 6.0);
    if z <= 1.0005 && pan == (0.0, 0.0) {
        return (fit, fit_rect);
    }
    let scale = fit.scale * z;
    let c = fit_rect.center();
    let off_x = c.x + pan.0 - page_w_mm as f32 * 0.5 * scale;
    let off_y = c.y + pan.1 - page_h_mm as f32 * 0.5 * scale;
    let map = PreviewMap {
        scale,
        off_x,
        off_y,
    };
    let rect = Rect::from_min_size(
        Pos2::new(off_x, off_y),
        Vec2::new(page_w_mm as f32 * scale, page_h_mm as f32 * scale),
    );
    (map, rect)
}

/// 限制页面偏移：缩放/平移后页面不能整页滑出画布，至少留一块可见区域。
fn clamp_view_offset(
    avail: Rect,
    scale: f32,
    page_w_mm: f64,
    page_h_mm: f64,
    off: (f32, f32),
) -> (f32, f32) {
    let dw = page_w_mm as f32 * scale;
    let dh = page_h_mm as f32 * scale;
    let keep = 60.0_f32
        .min(avail.width() / 3.0)
        .min(avail.height() / 3.0)
        .max(8.0);
    let axis = |min: f32, len: f32, draw: f32, v: f32| {
        let fallback = min + (len - draw) / 2.0;
        let lo = min + keep - draw;
        let hi = min + len - keep;
        if lo > hi {
            fallback
        } else {
            v.clamp(lo, hi)
        }
    };
    (
        axis(avail.min.x, avail.width(), dw, off.0),
        axis(avail.min.y, avail.height(), dh, off.1),
    )
}

/// 把页面左上角偏移换算回 `canvas_pan`（相对同缩放级别下「无平移」布局的位移）。
fn pan_from_offset(
    avail: Rect,
    page_w_mm: f64,
    page_h_mm: f64,
    ruler_on: bool,
    zoom: f32,
    off: (f32, f32),
) -> (f32, f32) {
    let (base, _) = layout_view(avail, page_w_mm, page_h_mm, ruler_on, zoom, (0.0, 0.0));
    (off.0 - base.off_x, off.1 - base.off_y)
}

/// 页面左上角的 **XY 方向箭头**：蓝=X 向右，绿=Y 向下（页面坐标）。
///
/// 反转/对调开启时箭头跟着指，免得「画出来的字是反的」还要靠猜。
fn draw_axes(p: &Painter, map: &PreviewMap, app: &AppState) {
    // 方向直接取自 G-code 的**真实变换**，不在这里另算一套符号：
    // 反转/对调一开，箭头跟着指，绝不会出现「箭头指这边、机器走那边」。
    let (xdir, ydir) = app.gcode_options().axis_dirs();
    let pal = crate::theme::palette(app.dark_mode);
    let (x_color, y_color) = (pal.accent, pal.success);
    // 箭头原点放在页面内左上角一点，避免压住页边
    let (ox, oy) = (6.0_f64, 6.0_f64);
    let len = 22.0_f64; // 毫米
    arrow(p, map, (ox, oy), xdir, len, x_color, "X");
    arrow(p, map, (ox, oy), ydir, len, y_color, "Y");
}

/// 画一支箭头：从 `(ox,oy)` 沿单位方向 `dir` 走 `len` 毫米。
fn arrow(
    p: &Painter,
    map: &PreviewMap,
    (ox, oy): (f64, f64),
    dir: (f64, f64),
    len: f64,
    color: Color32,
    label: &str,
) {
    let a = map.to_screen(ox, oy);
    let b = map.to_screen(ox + dir.0 * len, oy + dir.1 * len);
    p.line_segment([a, b], Stroke::new(2.4, color));
    // 箭头（两条短翼）
    let d = b - a;
    let l = d.length().max(1.0);
    let u = d / l;
    let n = egui::Vec2::new(-u.y, u.x);
    let wing = 7.0_f32;
    for s in [-1.0_f32, 1.0_f32] {
        p.line_segment(
            [b, b - u * wing * 1.7 + n * s * wing * 0.75],
            Stroke::new(2.2, color),
        );
    }
    // 标签放在箭头尖端外侧
    p.text(
        b + u * 12.0,
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(13.0),
        color,
    );
}

/// 文字模块的**虚拟文本框**：虚线矩形 + 框名，**不参与打印**。
///
/// 多个文本框叠在一起时，光看笔迹很难判断哪段落在哪个框里；
/// 画个虚框就能直接看出框的位置与大小，拖动参数时也知道边界在哪。
fn draw_text_frames(p: &Painter, map: &PreviewMap, app: &AppState) {
    let pal = crate::theme::palette(app.dark_mode);
    for (i, b) in app.texts.iter().enumerate() {
        let sel = i == app.sel_text;
        let (col, w) = if sel {
            (pal.accent, 1.6)
        } else if b.on {
            (pal.muted, 1.0)
        } else {
            (pal.warning, 1.0)
        };
        // 四边虚线
        let dash = 5.0_f64;
        let edges = [
            ((b.x, b.y), (b.x + b.w, b.y)),
            ((b.x + b.w, b.y), (b.x + b.w, b.y + b.h)),
            ((b.x + b.w, b.y + b.h), (b.x, b.y + b.h)),
            ((b.x, b.y + b.h), (b.x, b.y)),
        ];
        for ((ax, ay), (bx, by)) in edges {
            let total = ((bx - ax).powi(2) + (by - ay).powi(2)).sqrt();
            if total <= 0.0 {
                continue;
            }
            let (ux, uy) = ((bx - ax) / total, (by - ay) / total);
            let mut t = 0.0;
            while t < total {
                let t2 = (t + dash).min(total);
                p.line_segment(
                    [
                        map.to_screen(ax + ux * t, ay + uy * t),
                        map.to_screen(ax + ux * t2, ay + uy * t2),
                    ],
                    Stroke::new(w, col),
                );
                t = t2 + dash;
            }
        }
        // 左上角标框名
        p.text(
            map.to_screen(b.x + 1.5, b.y + 1.5),
            egui::Align2::LEFT_TOP,
            if b.on { b.name.clone() } else { format!("{}（不输出）", b.name) },
            egui::FontId::proportional(11.0),
            col,
        );
    }
}

/// 自定义网格的等分线（页面毫米坐标），只由「网格」开关控制。
///
/// 不再画不可调的 10mm 实线基准网格，只保留可设置列/行数的虚线网格。
fn grid_overlay_segments(
    page_w_mm: f64,
    page_h_mm: f64,
    cols: usize,
    rows: usize,
    show: bool,
) -> Vec<((f64, f64), (f64, f64))> {
    if !show {
        return Vec::new();
    }
    let mut out = Vec::new();
    let n = cols.max(1);
    for i in 1..n {
        let x = page_w_mm * i as f64 / n as f64;
        out.push(((x, 0.0), (x, page_h_mm)));
    }
    let m = rows.max(1);
    for j in 1..m {
        let y = page_h_mm * j as f64 / m as f64;
        out.push(((0.0, y), (page_w_mm, y)));
    }
    out
}

/// 在页面毫米坐标下画虚线等分网格（仅定位，不打印）。
fn draw_grid_overlay(
    p: &Painter,
    map: &PreviewMap,
    page_w_mm: f64,
    page_h_mm: f64,
    cols: usize,
    rows: usize,
    show: bool,
    pal: &Palette,
) {
    if !show {
        return;
    }
    let stroke = Stroke::new(1.0, pal.accent.gamma_multiply(0.4));
    for (a, b) in grid_overlay_segments(page_w_mm, page_h_mm, cols, rows, show) {
        dashed_line(p, map, a, b, stroke, 4.0);
    }
}

/// 绘制纸张（网格由 `draw_grid_overlay` 单独叠加）。
fn draw_page_bg(p: &Painter, page_rect: &Rect, pal: &Palette) {
    // 纸张
    p.rect_filled(*page_rect, 6.0, pal.paper);
    // 页边
    p.rect_stroke(
        *page_rect,
        6.0,
        Stroke::new(1.0, pal.stroke_strong),
        egui::StrokeKind::Middle,
    );
}

/// 页面外侧标尺带的**厚度**（顶部 + 左侧各一条固定厚度的带子）。
///
/// 关键：标尺必须是「贴着页面边缘」的固定厚度带子，**不能**从可用区边缘一直填到页面边缘——
/// 页面居中时两者之间会有几百像素的留白，那样会变成一大片灰底「空白」。
const RULER_PX: f32 = 30.0;

/// 画一段虚线（在页面毫米坐标下），用于自定义网格。
fn dashed_line(
    p: &Painter,
    map: &PreviewMap,
    a: (f64, f64),
    b: (f64, f64),
    stroke: Stroke,
    dash: f64,
) {
    let total = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
    if total <= 0.0 {
        return;
    }
    let (ux, uy) = ((b.0 - a.0) / total, (b.1 - a.1) / total);
    let mut t = 0.0;
    while t < total {
        let t2 = (t + dash).min(total);
        p.line_segment(
            [
                map.to_screen(a.0 + ux * t, a.1 + uy * t),
                map.to_screen(a.0 + ux * t2, a.1 + uy * t2),
            ],
            stroke,
        );
        t = t2 + dash;
    }
}

/// 在页面**外侧**绘制标尺：顶部 + 左侧，显示毫米刻度（每 10mm 一个刻度，每 50mm 一个数值）。
///
/// 标尺是「**紧贴页面边缘**」的固定厚度（`RULER_PX`）带子，长度与页面对齐，
/// **不依赖可用区到页面的间距**——页面在预览框里居中时，两者之间的空白属于正常留白，
/// 不能被填成一大片灰底（这正是旧实现「左侧大片空白」的根因）。
fn draw_rulers(
    p: &Painter,
    map: &PreviewMap,
    page_rect: Rect,
    page_w_mm: f64,
    page_h_mm: f64,
    pal: &Palette,
) {
    let strip = pal.panel;
    let corner = pal.card_hover;
    let edge = pal.stroke_strong;
    let tick = pal.muted;
    let lbl = pal.muted;

    let (x0, y0) = (page_rect.min.x, page_rect.min.y);
    let (x1, y1) = (page_rect.max.x, page_rect.max.y);

    // 顶部标尺带：贴页面上边，**厚度固定** = RULER_PX，长度 = 页面宽
    p.rect_filled(
        Rect::from_min_max(Pos2::new(x0, y0 - RULER_PX), Pos2::new(x1, y0)),
        0.0,
        strip,
    );
    // 左侧标尺带：贴页面左边，**厚度固定** = RULER_PX，长度 = 页面高
    p.rect_filled(
        Rect::from_min_max(Pos2::new(x0 - RULER_PX, y0), Pos2::new(x0, y1)),
        0.0,
        strip,
    );
    // 左上角块
    p.rect_filled(
        Rect::from_min_max(Pos2::new(x0 - RULER_PX, y0 - RULER_PX), Pos2::new(x0, y0)),
        0.0,
        corner,
    );

    // 与纸张交界的边线（上、左）
    p.line_segment(
        [Pos2::new(x0, y0), Pos2::new(x1, y0)],
        Stroke::new(1.0, edge),
    );
    p.line_segment(
        [Pos2::new(x0, y0), Pos2::new(x0, y1)],
        Stroke::new(1.0, edge),
    );
    // 标尺带外沿（上、左）
    p.line_segment(
        [Pos2::new(x0, y0 - RULER_PX), Pos2::new(x1, y0 - RULER_PX)],
        Stroke::new(1.0, edge),
    );
    p.line_segment(
        [Pos2::new(x0 - RULER_PX, y0), Pos2::new(x0 - RULER_PX, y1)],
        Stroke::new(1.0, edge),
    );

    // 顶部刻度（沿 X）
    let mut x = 0.0;
    while x <= page_w_mm + 1e-6 {
        let sx = map.to_screen(x, 0.0).x;
        let major = (x % 50.0).abs() < 1e-6;
        let h = if major { 9.0 } else { 5.0 };
        p.line_segment(
            [Pos2::new(sx, y0), Pos2::new(sx, y0 - h)],
            Stroke::new(1.0, tick),
        );
        if major && x > 0.5 {
            p.text(
                Pos2::new(sx, y0 - h - 1.0),
                Align2::CENTER_BOTTOM,
                format!("{}", x as i32),
                FontId::proportional(9.0),
                lbl,
            );
        }
        x += 10.0;
    }
    // 左侧刻度（沿 Y，页面 Y 向下为正）
    let mut y = 0.0;
    while y <= page_h_mm + 1e-6 {
        let sy = map.to_screen(0.0, y).y;
        let major = (y % 50.0).abs() < 1e-6;
        let h = if major { 9.0 } else { 5.0 };
        p.line_segment(
            [Pos2::new(x0, sy), Pos2::new(x0 - h, sy)],
            Stroke::new(1.0, tick),
        );
        if major && y > 0.5 {
            p.text(
                Pos2::new(x0 - h - 2.0, sy),
                Align2::RIGHT_CENTER,
                format!("{}", y as i32),
                FontId::proportional(9.0),
                lbl,
            );
        }
        y += 10.0;
    }
}

/// 在编辑画布上绘制整页：纸张 / 网格 / 背景参考图（浅色，不打印）/ 文字笔迹 / 选中图片框。
///
/// - `freehand`：开启手绘捕获（拖动鼠标画笔迹）；
/// - `image_tab`：图片页，允许按住选中图片拖动来移动其位置；
/// - `acc`：手绘过程中的临时点集（毫米）；
/// 返回 `(坐标映射, 本次是否完成了一笔)`。
pub fn draw_canvas(
    ui: &mut egui::Ui,
    app: &mut AppState,
    freehand: bool,
    image_tab: bool,
    acc: &mut Vec<(f64, f64)>,
) -> (PreviewMap, bool) {
    let full_avail = ui.available_rect_before_wrap();
    let (pw, ph) = (app.page_w_mm, app.page_h_mm);
    // 分页时在画布底部留出缩略图条：所有页与图片切分一一对应，可横向滚动。
    let strip_layout = if image_tab && app.tile_on {
        app.tile_layout()
    } else {
        None
    };
    let strip_h = if strip_layout.is_some() && full_avail.height() > 340.0 {
        164.0_f32.min(full_avail.height() * 0.36)
    } else {
        0.0
    };
    let avail = Rect::from_min_max(
        full_avail.min,
        Pos2::new(full_avail.max.x, full_avail.max.y - strip_h),
    );
    let ruler_on = app.ruler_on && avail.width() > 80.0 && avail.height() > 80.0;

    // 画布始终可拖动：中键 / 空格+左键平移需要 drag 响应；
    // 点击、手绘等语义在下面按页签再分派。
    let resp = ui.allocate_rect(avail, egui::Sense::click_and_drag());

    let (mut map, mut page_rect) =
        layout_view(avail, pw, ph, ruler_on, app.canvas_zoom, app.canvas_pan);

    // Ctrl+滚轮 / 触控板捏合：以光标为锚缩放画布（1.0 = 适应窗口）。
    let zoom_delta = ui.input(|i| i.zoom_delta());
    if resp.hovered() && (zoom_delta - 1.0).abs() > 1e-3 {
        let anchor = resp.hover_pos().unwrap_or_else(|| page_rect.center());
        let (mx, my) = map.to_mm(anchor);
        let new_zoom = (app.canvas_zoom * zoom_delta).clamp(1.0, 6.0);
        if (new_zoom - app.canvas_zoom).abs() > 1e-4 {
            app.canvas_zoom = new_zoom;
            if new_zoom <= 1.001 {
                app.canvas_pan = (0.0, 0.0);
            } else {
                let (base, _) = layout_view(avail, pw, ph, ruler_on, new_zoom, (0.0, 0.0));
                let want = (
                    anchor.x - mx as f32 * base.scale,
                    anchor.y - my as f32 * base.scale,
                );
                let off = clamp_view_offset(avail, base.scale, pw, ph, want);
                app.canvas_pan = pan_from_offset(avail, pw, ph, ruler_on, new_zoom, off);
            }
            (map, page_rect) =
                layout_view(avail, pw, ph, ruler_on, app.canvas_zoom, app.canvas_pan);
            app.status = format!(
                "画布缩放 {:.0}%（Ctrl+滚轮缩放、空格+拖动平移）",
                app.canvas_zoom * 100.0
            );
        }
    }

    // 平移：中键拖动，或空格 + 左键拖动（仅在放大后生效）。
    let space_down = ui.input(|i| i.key_down(Key::Space));
    let pan_drag = resp.dragged_by(PointerButton::Middle)
        || (space_down && resp.dragged_by(PointerButton::Primary));
    if pan_drag && app.canvas_zoom > 1.001 {
        let d = resp.drag_delta();
        if d != Vec2::ZERO {
            let (base, _) = layout_view(avail, pw, ph, ruler_on, app.canvas_zoom, (0.0, 0.0));
            let want = (
                base.off_x + app.canvas_pan.0 + d.x,
                base.off_y + app.canvas_pan.1 + d.y,
            );
            let off = clamp_view_offset(avail, base.scale, pw, ph, want);
            app.canvas_pan = (off.0 - base.off_x, off.1 - base.off_y);
            (map, page_rect) =
                layout_view(avail, pw, ph, ruler_on, app.canvas_zoom, app.canvas_pan);
        }
    }
    if resp.hovered() {
        if space_down && app.canvas_zoom > 1.001 {
            ui.ctx().set_cursor_icon(if pan_drag {
                CursorIcon::Grabbing
            } else {
                CursorIcon::Grab
            });
        } else if resp.dragged_by(PointerButton::Middle) {
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        }
    }

    // 鼠标在页面上的毫米位置：左栏「坐标」区要显示「页面坐标 → 机器坐标」，
    // 反转/对调开没开对不对，看这一行最直接。
    app.cursor_mm = resp.hover_pos().and_then(|p| {
        let (x, y) = map.to_mm(p);
        (x >= 0.0 && y >= 0.0 && x <= app.page_w_mm && y <= app.page_h_mm).then_some((x, y))
    });

    // 手绘捕获（按住空格时让位给画布平移）
    let mut committed = false;
    if freehand && !space_down {
        if resp.drag_started() {
            acc.clear();
        }
        if resp.dragged() {
            if let Some(p) = resp.interact_pointer_pos() {
                let (x, y) = map.to_mm(p);
                let last = acc.last().copied();
                if last
                    .map(|(lx, ly)| ((x - lx).powi(2) + (y - ly).powi(2)).sqrt() > 0.5)
                    .unwrap_or(true)
                {
                    acc.push((x, y));
                }
            }
        }
        let released = ui.input(|i| i.pointer.primary_released());
        if released {
            if acc.len() >= 2 {
                committed = true;
            } else {
                acc.clear();
            }
        }
    }

    // 图片页：拖动画布移动选中图片 / 滚轮缩放 / 点击选中
    if image_tab && !app.images.is_empty() && !space_down {
        // 分页预览是「页内局部坐标」，命中测试要换算回图片所在的页面坐标。
        let (tile_dx, tile_dy) = if app.tile_on {
            app.tile_layout()
                .and_then(|l| l.origin(app.tile_page.min(l.count().saturating_sub(1))))
                .unwrap_or((0.0, 0.0))
        } else {
            (0.0, 0.0)
        };
        // 点击（未拖动）时选中指针下的最上层图片
        if resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                let (mx, my) = map.to_mm(p);
                let mut hit = None;
                for (i, im) in app.images.iter().enumerate() {
                    if im.hit(mx + tile_dx, my + tile_dy) {
                        hit = Some(i); // 后添加的在上层
                    }
                }
                if let Some(i) = hit {
                    app.select_image(i);
                    app.status = format!("已选中图片 #{}", i + 1);
                }
            }
        }
        // 滚轮缩放当前选中图片（围绕中心）；Ctrl+滚轮留给画布缩放
        let (scroll, ctrl) = ui.input(|i| {
            (
                i.smooth_scroll_delta.y,
                i.modifiers.ctrl || i.modifiers.command,
            )
        });
        let hovered = resp.hovered();
        if hovered && !ctrl && scroll.abs() > 0.01 && !app.images.is_empty() {
            let f = (1.0 + scroll as f64 * 0.0015).clamp(0.5, 2.0);
            app.scale_image(app.sel_image, f);
            if let Some(im) = app.images.get(app.sel_image) {
                app.status = format!("缩放 ×{:.2}", im.scale);
            }
        }
        if resp.drag_started() {
            if let Some(p) = resp.interact_pointer_pos() {
                let (mx, my) = map.to_mm(p);
                let (hx, hy) = (mx + tile_dx, my + tile_dy);
                // 按图就拖：先选中最上层被按住的图片。以前只认上一次选中的那张，
                // 换一张图得「先点一下、再拖」，很容易以为图根本拖不动。
                let mut hit = None;
                for i in 0..app.images.len() {
                    if app.image_hit(i, hx, hy) {
                        hit = Some(i); // 后添加的在上层
                    }
                }
                if let Some(i) = hit {
                    app.select_image(i);
                    app.img_drag = Some((mx, my));
                } else {
                    app.img_drag = None;
                }
            }
        }
        if resp.dragged() {
            // 拖动开始时可能刚改过选中项，这里取最新的。
            let sel = app.sel_image;
            if let (Some((lx, ly)), Some(p)) = (app.img_drag, resp.interact_pointer_pos()) {
                let (mx, my) = map.to_mm(p);
                app.nudge_image(sel, mx - lx, my - ly);
                app.img_drag = Some((mx, my));
                if let Some(im) = app.images.get(sel) {
                    app.status = format!("图片已移至 ({:.1}, {:.1})", im.x, im.y);
                }
            }
        }
        // 鼠标悬停在图片上时给出抓手光标，避免「不知道这块能不能拖」。
        if resp.hovered() && !pan_drag {
            if let Some(p) = resp.hover_pos() {
                let (mx, my) = map.to_mm(p);
                let (hx, hy) = (mx + tile_dx, my + tile_dy);
                if (0..app.images.len()).any(|i| app.image_hit(i, hx, hy)) {
                    ui.ctx().set_cursor_icon(if app.img_drag.is_some() {
                        CursorIcon::Grabbing
                    } else {
                        CursorIcon::Grab
                    });
                }
            }
        }
        if ui.input(|i| i.pointer.primary_released()) {
            app.img_drag = None;
        }
    } else {
        app.img_drag = None;
    }

    // 文字页：普通滚轮缩放**当前选中的文本框**（Ctrl+滚轮仍是画布视图缩放）。
    if app.tab == 0 && !space_down && !app.texts.is_empty() {
        let (scroll, ctrl) = ui.input(|i| {
            (
                i.smooth_scroll_delta.y,
                i.modifiers.ctrl || i.modifiers.command,
            )
        });
        if resp.hovered() && !ctrl && scroll.abs() > 0.01 {
            let f = (1.0 + scroll as f64 * 0.0015).clamp(0.5, 2.0);
            let i = app.sel_text.min(app.texts.len() - 1);
            app.scale_text_box(i, f);
            let b = &app.texts[i];
            app.status = format!(
                "{}：字号 {:.0}% · {:.0}×{:.0}mm",
                b.name, b.size, b.w, b.h
            );
        }
    }

    let pal = crate::theme::palette(ui.visuals().dark_mode);
    // 画布底色（纸张外的「桌面」）+ 纸张投影。
    let screen_painter = ui.painter().with_clip_rect(ui.clip_rect());
    screen_painter.rect_filled(avail, 0.0, pal.canvas_bg);
    screen_painter.rect_filled(
        page_rect.translate(Vec2::new(0.0, 5.0)).expand(2.0),
        8.0,
        pal.shadow.color,
    );
    // 笔迹裁剪到「纸张 ∩ 画布」：放大后页面超出画布的部分不会画到侧栏上。
    let page_painter = ui.painter().with_clip_rect(page_rect.intersect(ui.clip_rect()));

    draw_page_bg(&page_painter, &page_rect, &pal);

    // 自定义网格覆盖层：可调节行/列数量的浅色虚线网格，**仅用于定位、不打印**。
    draw_grid_overlay(
        &page_painter,
        &map,
        pw,
        ph,
        app.grid_overlay_cols,
        app.grid_overlay_rows,
        app.grid_overlay,
        &pal,
    );

    // 分页模式：画布直接预览「本页真正会画的内容」（已裁剪、平移到页左上角），
    // 换页时所见即所得；不再按虚拟大画布的位置切开显示。
    let tile_preview = if image_tab && app.tile_on {
        match app.tile_layout() {
            Some(layout) => {
                let page = app.tile_page.min(layout.count().saturating_sub(1));
                layout.origin(page).map(|(ox, oy)| {
                    let content = app.tile_page_tagged_cached(page);
                    (layout, page, ox, oy, content)
                })
            }
            None => None,
        }
    } else {
        None
    };

    // 背景参考图（浅色显示，不参与打印）；分页时只画当前页内容，避免参考图错位。
    if tile_preview.is_none() {
        let bg_stroke = Stroke::new(1.0, pal.muted.gamma_multiply(0.75));
        for pl in app.background_image_polylines() {
            if pl.pts.len() < 2 {
                continue;
            }
            for w in pl.pts.windows(2) {
                page_painter.line_segment(
                    [map.to_screen(w[0].0, w[0].1), map.to_screen(w[1].0, w[1].1)],
                    bg_stroke,
                );
            }
        }
    }

    // 文字 / 表格 / 格子 / 绘图 / 笔记 / 前景图片（深色笔迹）。
    // 分页时只输出图片，因此预览也只画当前页的图片裁剪结果。
    //
    // 线宽按**真实笔尖粗细 × 当前显示比例**折算。单线字体和排线在纸上都是
    // 一支笔画的，全用同一个物理宽度；预览固定画 1.3px 的话，0.5mm 的笔在
    // 适应窗口时该是 3px、放大后更粗，屏幕上却始终细得像根头发——排线明明
    // 已经排满，看着却稀稀拉拉，人会以为算法没画够。
    //
    // 多笔模式按笔色和笔宽分别画：哪一段是 HB、哪一段是 8B，换笔前就该看得见。
    let draw_pl = |pl: &Polyline, tag: u8| {
        if pl.pts.len() < 2 {
            return;
        }
        let (col, w_mm) = match app.pen_color(tag) {
            Some(c) => (
                Color32::from_rgb(c[0], c[1], c[2]),
                app.pen_width_mm(tag).unwrap_or(app.preview_pen_mm),
            ),
            None => (pal.ink, app.preview_pen_mm),
        };
        let ink_px = (w_mm * map.scale as f64).clamp(1.0, 24.0) as f32;
        let stroke = Stroke::new(ink_px, col);
        for w in pl.pts.windows(2) {
            page_painter.line_segment(
                [map.to_screen(w[0].0, w[0].1), map.to_screen(w[1].0, w[1].1)],
                stroke,
            );
        }
    };
    match &tile_preview {
        Some((_, _, _, _, tiled)) => {
            for (pl, tag) in tiled {
                draw_pl(pl, *tag);
            }
        }
        None => {
            for (k, pl) in app.preview.iter().enumerate() {
                let tag = app.preview_pens.get(k).copied().unwrap_or(crate::state::NON_PEN);
                draw_pl(pl, tag);
            }
        }
    }

    // 分页：标出单页有效幅面（机器能画到的范围），以及四角 L 定位标记。
    if let Some((layout, page, _, _, _)) = &tile_preview {
        let tile = Rect::from_min_max(
            map.to_screen(0.0, 0.0),
            map.to_screen(layout.width_mm, layout.height_mm),
        );
        // A4 宽 210 > 机器幅面 200 时，右侧 10mm 画不到，压暗提示。
        let t = tile.intersect(page_rect);
        if t.width() > 0.5 && t.height() > 0.5 {
            let dim = Color32::from_black_alpha(54);
            for r in [
                Rect::from_min_max(page_rect.min, Pos2::new(page_rect.max.x, t.min.y)),
                Rect::from_min_max(Pos2::new(page_rect.min.x, t.max.y), page_rect.max),
                Rect::from_min_max(
                    Pos2::new(page_rect.min.x, t.min.y),
                    Pos2::new(t.min.x, t.max.y),
                ),
                Rect::from_min_max(
                    Pos2::new(t.max.x, t.min.y),
                    Pos2::new(page_rect.max.x, t.max.y),
                ),
            ] {
                if r.width() > 0.0 && r.height() > 0.0 {
                    page_painter.rect_filled(r, 0.0, dim);
                }
            }
        }
        if app.tile_marks {
            let mark = Stroke::new(1.6, pal.warning);
            for pl in corner_mark_polylines(layout.width_mm, layout.height_mm, 10.0, (false, true)) {
                for w in pl.pts.windows(2) {
                    page_painter.line_segment(
                        [map.to_screen(w[0].0, w[0].1), map.to_screen(w[1].0, w[1].1)],
                        mark,
                    );
                }
            }
        }
        let label = format!(
            "分页预览 · 第 {} / {} 页 · 换纸后画下一页",
            page + 1,
            layout.count()
        );
        let galley = page_painter.layout_no_wrap(
            label,
            FontId::proportional(12.0),
            pal.accent,
        );
        let at = Pos2::new(
            page_rect.center().x - galley.size().x * 0.5,
            page_rect.min.y + 6.0,
        );
        page_painter.rect_filled(
            Rect::from_min_size(at - Vec2::new(7.0, 3.0), galley.size() + Vec2::new(14.0, 6.0)),
            5.0,
            pal.panel.gamma_multiply(0.92),
        );
        page_painter.galley(at, galley, pal.accent);
    }

    // 手绘中的笔迹（实时反馈）
    if freehand && acc.len() >= 2 {
        let draft = Stroke::new(1.3, pal.danger);
        for w in acc.windows(2) {
            page_painter.line_segment(
                [map.to_screen(w[0].0, w[0].1), map.to_screen(w[1].0, w[1].1)],
                draft,
            );
        }
    }

    // 图片页：未勾选「并入」时，前景图仍要能看见才能拖动/缩放——
    // 用浅色画一遍「仅供编辑」的笔迹，它不会进入 G-code（输出看 images_active）。
    if image_tab && !app.images_active() && !app.images.is_empty() {
        let edit_stroke = Stroke::new(1.1, pal.muted.gamma_multiply(0.75));
        for pl in app.foreground_polylines() {
            if pl.pts.len() < 2 {
                continue;
            }
            for w in pl.pts.windows(2) {
                page_painter.line_segment(
                    [map.to_screen(w[0].0, w[0].1), map.to_screen(w[1].0, w[1].1)],
                    edit_stroke,
                );
            }
        }
    }

    // 文字模块：文本框虚拟框（虚线，仅定位用，**不打印**）
    if app.text_frame_on && !app.texts.is_empty() && app.module_active(0) {
        draw_text_frames(&page_painter, &map, app);
    }

    // XY 方向箭头（蓝=X，绿=Y）：一眼看出机器往哪边走
    if app.axes_on {
        draw_axes(&page_painter, &map, app);
    }

    // 选中图片的包围框（按旋转后的四角绘制，旋转/镜像也跟得上）。
    // 分页预览里内容已平移到页左上角，包围框跟着平移，避免和笔迹错位。
    if image_tab {
        let sel = app.sel_image;
        let (dx, dy) = match &tile_preview {
            Some((_, _, ox, oy, _)) => (-*ox, -*oy),
            None => (0.0, 0.0),
        };
        if let Some(corners) = app.image_corners(sel) {
            let scr: Vec<Pos2> = corners
                .iter()
                .map(|&(x, y)| map.to_screen(x + dx, y + dy))
                .collect();
            let col = if app.images[sel].background {
                pal.accent
            } else if !app.image_outputs(sel) {
                // 未勾选输出：灰色虚框，提示「看得见但不会打印」
                pal.muted
            } else {
                pal.warning
            };
            for i in 0..4 {
                page_painter.line_segment(
                    [scr[i], scr[(i + 1) % 4]],
                    Stroke::new(1.5, col),
                );
            }
            // 四角小方块，提示可选中/拖动
            for p in &scr {
                page_painter.rect_filled(
                    Rect::from_center_size(*p, Vec2::splat(5.0)),
                    1.0,
                    col,
                );
            }
        }
    }

    // 空提示
    if app.preview.is_empty() && acc.is_empty() && app.images.is_empty() {
        page_painter.text(
            page_rect.center(),
            egui::Align2::CENTER_CENTER,
            if freehand {
                "在页面内按住鼠标拖动即可手绘"
            } else if image_tab {
                "在右侧「图片」面板导入图片；拖动可移动、滚轮可缩放、点击可选中"
            } else {
                "暂无笔迹：在左侧选择一个模块并填写内容"
            },
            egui::FontId::proportional(14.0),
            pal.muted,
        );
    }

    // 页面外侧标尺（顶部 + 左侧毫米刻度）：必须用「未裁剪」的 painter，
    // 否则刻度条落在 page_rect 之外会被裁掉。仅当本帧确实按标尺布局时才画。
    if ruler_on {
        draw_rulers(
            &ui.painter(),
            &map,
            page_rect,
            app.page_w_mm,
            app.page_h_mm,
            &pal,
        );
    }

    // 分页缩略图条（画布底部，横向滚动）。
    if let (Some(layout), true) = (strip_layout, strip_h > 0.0) {
        let strip_rect = Rect::from_min_max(
            Pos2::new(full_avail.min.x, full_avail.max.y - strip_h),
            full_avail.max,
        );
        ui.allocate_rect(strip_rect, egui::Sense::hover());
        draw_tile_strip(ui, app, strip_rect, layout);
    }

    (map, committed)
}

/// 分页缩略图条：每页一张小纸 + 该页的图片内容 + 四角标记 + 页码。
///
/// 点击缩略图即切换 `tile_page`，与主画布、Action Bar 的页码选择保持联动。
/// 页数较多时横向滚动（滚动条常显，避免用户不知道右侧还有内容）。
fn draw_tile_strip(
    ui: &mut egui::Ui,
    app: &mut AppState,
    rect: Rect,
    layout: crate::state::TileLayout,
) {
    let pal = crate::theme::palette(ui.visuals().dark_mode);
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    painter.rect_filled(rect, 6.0, pal.panel);
    painter.line_segment(
        [rect.left_top(), rect.right_top()],
        Stroke::new(1.0, pal.stroke),
    );

    let count = layout.count().max(1);
    app.fill_tile_cache();
    let mut sel = app.tile_page.min(count - 1);
    let marks = app.tile_marks;
    let cache = &app.tile_cache_pages;

    let strip_ui_inner = rect.shrink2(Vec2::new(8.0, 6.0));
    let mut strip_ui = ui.new_child(egui::UiBuilder::new().max_rect(strip_ui_inner));
    // 条内滚动条改成常驻（非悬浮），否则浅色主题下浮条几乎看不见。
    strip_ui.spacing_mut().scroll.floating = false;
    egui::ScrollArea::horizontal()
        .id_salt("tile_page_thumbs")
        .auto_shrink([false, false])
        .scroll_bar_visibility(
            egui::containers::scroll_area::ScrollBarVisibility::AlwaysVisible,
        )
        .show(&mut strip_ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                // 卡片高度 = 条高 - 页码行 - 常驻滚动条占位
                let page_h = (strip_ui_inner.height() - 34.0 - 17.0).clamp(48.0, 104.0);
                let aspect = (layout.width_mm / layout.height_mm).clamp(0.2, 2.0);
                let page_w = (page_h as f64 * aspect).clamp(40.0, 160.0) as f32;
                for page in 0..count {
                    let (card, resp) = ui.allocate_exact_size(
                        Vec2::new(page_w + 10.0, page_h + 20.0),
                        egui::Sense::click(),
                    );
                    let mini = Rect::from_min_size(
                        Pos2::new(card.center().x - page_w * 0.5, card.min.y + 2.0),
                        Vec2::new(page_w, page_h),
                    );
                    let selected = page == sel;
                    let card_bg = if selected {
                        pal.accent_soft
                    } else if resp.hovered() {
                        pal.card_hover
                    } else {
                        pal.card
                    };
                    painter.rect_filled(card, 5.0, card_bg);
                    painter.rect_filled(mini, 1.0, pal.paper);
                    painter.rect_stroke(
                        mini,
                        1.0,
                        Stroke::new(
                            if selected { 1.6 } else { 0.8 },
                            if selected { pal.accent } else { pal.stroke },
                        ),
                        egui::StrokeKind::Inside,
                    );

                    // 该页实际会被绘制的图片内容（已裁剪 / 平移）。
                    let s = (mini.width() / layout.width_mm as f32)
                        .min(mini.height() / layout.height_mm as f32);
                    let to_screen = |x: f64, y: f64| {
                        Pos2::new(
                            mini.min.x + x as f32 * s,
                            mini.min.y + y as f32 * s,
                        )
                    };
                    if let Some(Some(polys)) = cache.get(page) {
                        let ink = Stroke::new(0.7, pal.ink);
                        for (pl, _) in polys {
                            for w in pl.pts.windows(2) {
                                page_painter_line(&painter, &to_screen, w[0], w[1], ink);
                            }
                        }
                    }
                    // 四角定位标记：与真正下发的一致，方便核对拼接位置。
                    if marks {
                        for pl in corner_mark_polylines(
                            layout.width_mm,
                            layout.height_mm,
                            10.0,
                            (false, true),
                        ) {
                            for w in pl.pts.windows(2) {
                                page_painter_line(
                                    &painter,
                                    &to_screen,
                                    w[0],
                                    w[1],
                                    Stroke::new(0.9, pal.warning),
                                );
                            }
                        }
                    }

                    let row = page / layout.cols.max(1);
                    let col = page % layout.cols.max(1);
                    let label = if selected {
                        format!("{}/{}", page + 1, count)
                    } else {
                        format!("{}", page + 1)
                    };
                    painter.text(
                        Pos2::new(card.center().x, mini.max.y + 4.0),
                        Align2::CENTER_TOP,
                        label,
                        FontId::proportional(10.5),
                        if selected { pal.accent } else { pal.muted },
                    );
                    let resp = resp.on_hover_text(format!(
                        "第 {} / {} 页（第 {} 行第 {} 列）· 点击切换预览",
                        page + 1,
                        count,
                        row + 1,
                        col + 1
                    ));
                    if resp.clicked() {
                        sel = page;
                    }
                }
            });
        });
    ui.allocate_rect(rect, egui::Sense::hover());

    if sel != app.tile_page {
        app.tile_page = sel;
        app.status = format!("分页预览：第 {}/{} 页", sel + 1, count);
    }
}

/// 在缩略图里画一条线段（毫米坐标经 `to_screen` 映射到卡片）。
fn page_painter_line(
    p: &Painter,
    to_screen: &impl Fn(f64, f64) -> Pos2,
    a: (f64, f64),
    b: (f64, f64),
    stroke: Stroke,
) {
    p.line_segment([to_screen(a.0, a.1), to_screen(b.0, b.1)], stroke);
}

/// 运行中进度页：整页显示，已完成部分黑色、未完成部分浅灰。
/// `frac` ∈ [0,1] 表示完成比例（按行进度估算）。
pub fn draw_progress_page(
    ui: &mut egui::Ui,
    paths: &[Polyline],
    frac: f64,
    page_w_mm: f64,
    page_h_mm: f64,
    show_grid: bool,
    grid_cols: usize,
    grid_rows: usize,
) {
    let avail = ui.available_rect_before_wrap();
    let (map, page_rect) = layout(avail, page_w_mm, page_h_mm);
    let resp = ui.allocate_rect(avail, egui::Sense::hover());
    let _ = resp;

    let pal = crate::theme::palette(ui.visuals().dark_mode);
    let screen_painter = ui.painter().with_clip_rect(ui.clip_rect());
    screen_painter.rect_filled(avail, 0.0, pal.canvas_bg);
    screen_painter.rect_filled(
        page_rect.translate(Vec2::new(0.0, 5.0)).expand(2.0),
        8.0,
        pal.shadow.color,
    );
    let painter = ui.painter().with_clip_rect(page_rect.intersect(ui.clip_rect()));
    draw_page_bg(&painter, &page_rect, &pal);
    draw_grid_overlay(
        &painter,
        &map,
        page_w_mm,
        page_h_mm,
        grid_cols,
        grid_rows,
        show_grid,
        &pal,
    );

    let total = paths.len();
    let done = ((frac.clamp(0.0, 1.0) * total as f64).round() as usize).min(total);
    let done_stroke = Stroke::new(1.4, pal.ink);
    let todo_stroke = Stroke::new(1.2, pal.stroke_strong);

    for (i, pl) in paths.iter().enumerate() {
        if pl.pts.len() < 2 {
            continue;
        }
        let st = if i < done { done_stroke } else { todo_stroke };
        for w in pl.pts.windows(2) {
            painter.line_segment(
                [map.to_screen(w[0].0, w[0].1), map.to_screen(w[1].0, w[1].1)],
                st,
            );
        }
    }

    if total == 0 {
        painter.text(
            page_rect.center(),
            egui::Align2::CENTER_CENTER,
            "尚无运行内容：点底部操作条的「开始绘制」下发任务",
            egui::FontId::proportional(14.0),
            pal.muted,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 标尺开启时页面应在预览框内**居中**（左右留白对称），不再像旧实现那样
    /// 把页面放在「右移了标尺条宽度的偏移区」里再居中，导致左侧整片空白。
    #[test]
    fn ruler_layout_centers_page_in_box() {
        // A4 竖版（210×297）放在宽窗口里：高度约束主导，左右本就有留白，
        // 但必须**对称居中**，不能左偏。
        let avail = Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 600.0));
        let margin = RULER_PX + 6.0;
        let (m, _r) = layout_opts(avail, 210.0, 297.0, false, margin);

        let draw_w = 210.0 * m.scale;
        let expected_off = (avail.width() - draw_w) / 2.0;
        // 页面在预览框内居中
        assert!(
            (m.off_x - expected_off).abs() < 1e-3,
            "页面应居中：左留白应等于 (width-draw_w)/2，实际 {}",
            m.off_x
        );
        // 居中 → 左右留白对称（这是和旧 BUG 的本质区别）
        let left_blank = m.off_x;
        let right_blank = avail.width() - (m.off_x + draw_w);
        assert!(
            (left_blank - right_blank).abs() < 1e-3,
            "左右留白应对称（左 {} / 右 {}）",
            left_blank,
            right_blank
        );
        // 且左留白至少为 margin，保证标尺标签有空间、不会因偏移再多吃一片
        assert!(
            left_blank >= margin - 1e-3,
            "左留白应不小于标尺边距 {}，实际 {}",
            margin,
            left_blank
        );
        // 标尺带是「贴着页面边缘」的固定厚度带子，必须能放进预留边距内，
        // 否则居中时它会从可用区边缘一直填到页面 → 左侧一大片灰底空白（旧 BUG）。
        assert!(
            RULER_PX <= margin,
            "标尺带厚度 {} 必须不大于预留边距 {}，否则会溢出可用区",
            RULER_PX,
            margin
        );
    }

    /// 缩放时页面中心保持不动（无平移时），避免「按 + 之后页面跑出视野」。
    #[test]
    fn zoom_keeps_page_centered() {
        let avail = Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 700.0));
        let (fit, fit_rect) = layout(avail, 210.0, 297.0);
        for zoom in [1.5_f32, 2.0, 4.0] {
            let (m, r) = layout_view(avail, 210.0, 297.0, false, zoom, (0.0, 0.0));
            assert!(
                (m.scale - fit.scale * zoom).abs() < 1e-4,
                "缩放 {zoom} 时比例应为适应比例 × zoom"
            );
            assert!((r.center().x - fit_rect.center().x).abs() < 1e-3);
            assert!((r.center().y - fit_rect.center().y).abs() < 1e-3);
        }
    }

    /// 平移再远，页面也必须保留一块可见区域，不能整页滑出画布。
    #[test]
    fn pan_never_loses_the_page() {
        let avail = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
        let (m, _) = layout_view(avail, 210.0, 297.0, false, 3.0, (0.0, 0.0));
        let dw = 210.0 * m.scale;
        let dh = 297.0 * m.scale;

        let far = clamp_view_offset(avail, m.scale, 210.0, 297.0, (5000.0, 5000.0));
        assert!(far.0 < avail.max.x, "页面左边不能越过画布右边");
        assert!(far.1 < avail.max.y, "页面上边不能越过画布下边");

        let near = clamp_view_offset(avail, m.scale, 210.0, 297.0, (-5000.0, -5000.0));
        assert!(near.0 + dw > avail.min.x, "页面右边不能越过画布左边");
        assert!(near.1 + dh > avail.min.y, "页面下边不能越过画布上边");
    }

    /// 网格只画可配置的虚线等分线；关闭时不能有任何网格线。
    #[test]
    fn grid_overlay_is_opt_in_and_dashed() {
        assert!(
            grid_overlay_segments(210.0, 297.0, 4, 6, false).is_empty(),
            "网格开关关闭时不应产生任何网格线"
        );
        let on = grid_overlay_segments(210.0, 297.0, 4, 6, true);
        assert_eq!(on.len(), 3 + 5, "等分网格线数量不对：{}", on.len());
        for ((x0, y0), (x1, y1)) in &on {
            assert!(
                (0.0..=210.0).contains(x0)
                    && (0.0..=210.0).contains(x1)
                    && (0.0..=297.0).contains(y0)
                    && (0.0..=297.0).contains(y1),
                "网格线不应超出页面"
            );
        }
        // 4 列等分 → 竖线位于 52.5 / 105 / 157.5mm
        let xs: Vec<f64> = on
            .iter()
            .filter(|((x0, _), (x1, _))| (x0 - x1).abs() < 1e-9)
            .map(|((x0, _), _)| *x0)
            .collect();
        assert_eq!(xs.len(), 3, "应有 3 条竖线");
        assert!((xs[1] - 105.0).abs() < 1e-9, "中间竖线应在页面正中");
    }
}
