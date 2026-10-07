//! 画布预览：把笔迹折线（`Vec<Polyline>`，页面毫米坐标）绘制到 egui。
//!
//! 提供两类绘制：
//! - `draw_canvas`：编辑态主画布，绘制纸张/网格/文字笔迹/背景参考图，并支持
//!   在「图片」页直接拖动画布移动选中图片；
//! - `draw_progress_page`：运行中进度页，整页显示，已完成笔迹黑色、未完成浅灰。

use egui::{Color32, Painter, Pos2, Rect, Stroke, Vec2};
use wrobot_core::render::Polyline;

use crate::state::AppState;

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

/// 计算整页缩放居中的映射（始终按**整页**缩放居中，页面稳定不跳动）。
fn layout(avail: egui::Rect, page_w_mm: f64, page_h_mm: f64) -> (PreviewMap, Rect) {
    let margin = 18.0_f32;
    let scale = ((avail.width() - 2.0 * margin) / page_w_mm as f32)
        .min((avail.height() - 2.0 * margin) / page_h_mm as f32)
        .max(0.01);
    let draw_w = page_w_mm as f32 * scale;
    let draw_h = page_h_mm as f32 * scale;
    let off_x = avail.min.x + (avail.width() - draw_w) / 2.0;
    let off_y = avail.min.y + (avail.height() - draw_h) / 2.0;
    let map = PreviewMap {
        scale,
        off_x,
        off_y,
    };
    let page_rect = Rect::from_min_size(Pos2::new(off_x, off_y), Vec2::new(draw_w, draw_h));
    (map, page_rect)
}

/// 页面左上角的 **XY 方向箭头**：蓝=X 向右，绿=Y 向下（页面坐标）。
///
/// 反转/对调开启时箭头跟着指，免得「画出来的字是反的」还要靠猜。
fn draw_axes(p: &Painter, map: &PreviewMap, app: &AppState) {
    // 方向直接取自 G-code 的**真实变换**，不在这里另算一套符号：
    // 反转/对调一开，箭头跟着指，绝不会出现「箭头指这边、机器走那边」。
    let (xdir, ydir) = app.gcode_options().axis_dirs();
    let (x_color, y_color) = (Color32::from_rgb(40, 110, 240), Color32::from_rgb(40, 170, 90));
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
    for (i, b) in app.texts.iter().enumerate() {
        let sel = i == app.sel_text;
        let (col, w) = if sel {
            (Color32::from_rgb(40, 110, 240), 1.6)
        } else if b.on {
            (Color32::from_rgb(120, 130, 145), 1.0)
        } else {
            (Color32::from_rgb(190, 150, 150), 1.0)
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

/// 绘制纸张背景 + 10mm 淡网格（编辑页与进度页共用）。
fn draw_page_bg(p: &Painter, map: &PreviewMap, page_rect: &Rect, page_w_mm: f64, page_h_mm: f64) {
    // 纸张阴影
    p.rect_filled(
        page_rect.translate(Vec2::new(3.0, 4.0)),
        6.0,
        Color32::from_black_alpha(38),
    );
    // 纸张
    p.rect_filled(*page_rect, 6.0, Color32::from_rgb(252, 252, 250));
    // 页边
    p.rect_stroke(
        *page_rect,
        6.0,
        Stroke::new(1.0, Color32::from_gray(150)),
        egui::StrokeKind::Middle,
    );

    // 10mm 淡网格（便于定位）
    let grid_stroke = Stroke::new(1.0, Color32::from_rgb(226, 232, 238));
    let step = 10.0_f64;
    let mut gx = step;
    while gx < page_w_mm {
        p.line_segment([map.to_screen(gx, 0.0), map.to_screen(gx, page_h_mm)], grid_stroke);
        gx += step;
    }
    let mut gy = step;
    while gy < page_h_mm {
        p.line_segment([map.to_screen(0.0, gy), map.to_screen(page_w_mm, gy)], grid_stroke);
        gy += step;
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
    let avail = ui.available_rect_before_wrap();
    let (map, page_rect) = layout(avail, app.page_w_mm, app.page_h_mm);

    let sense = if freehand || image_tab {
        egui::Sense::click_and_drag()
    } else {
        egui::Sense::hover()
    };
    let resp = ui.allocate_rect(avail, sense);

    // 鼠标在页面上的毫米位置：左栏「坐标」区要显示「页面坐标 → 机器坐标」，
    // 反转/对调开没开对不对，看这一行最直接。
    app.cursor_mm = resp.hover_pos().and_then(|p| {
        let (x, y) = map.to_mm(p);
        (x >= 0.0 && y >= 0.0 && x <= app.page_w_mm && y <= app.page_h_mm).then_some((x, y))
    });

    // 手绘捕获
    let mut committed = false;
    if freehand {
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
    if image_tab && !app.images.is_empty() {
        // 点击（未拖动）时选中指针下的最上层图片
        if resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                let (mx, my) = map.to_mm(p);
                let mut hit = None;
                for (i, im) in app.images.iter().enumerate() {
                    if im.hit(mx, my) {
                        hit = Some(i); // 后添加的在上层
                    }
                }
                if let Some(i) = hit {
                    app.select_image(i);
                    app.status = format!("已选中图片 #{}", i + 1);
                }
            }
        }
        // 滚轮缩放当前选中图片（围绕中心）
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        let hovered = resp.hovered();
        if hovered && scroll.abs() > 0.01 {
            let f = (1.0 + scroll as f64 * 0.0015).clamp(0.5, 2.0);
            app.scale_image(app.sel_image, f);
            app.status = format!("缩放 ×{:.2}", app.images[app.sel_image].scale);
        }
        let sel = app.sel_image;
        if resp.drag_started() {
            if let Some(p) = resp.interact_pointer_pos() {
                let (mx, my) = map.to_mm(p);
                if app.image_hit(sel, mx, my) {
                    app.img_drag = Some((mx, my));
                }
            }
        }
        if resp.dragged() {
            if let (Some((lx, ly)), Some(p)) = (app.img_drag, resp.interact_pointer_pos()) {
                let (mx, my) = map.to_mm(p);
                app.nudge_image(sel, mx - lx, my - ly);
                app.img_drag = Some((mx, my));
                if let Some(im) = app.images.get(sel) {
                    app.status = format!("图片已移至 ({:.1}, {:.1})", im.x, im.y);
                }
            }
        }
        if ui.input(|i| i.pointer.primary_released()) {
            app.img_drag = None;
        }
    } else {
        app.img_drag = None;
    }

    let page_painter = ui.painter().with_clip_rect(page_rect);

    draw_page_bg(&page_painter, &map, &page_rect, app.page_w_mm, app.page_h_mm);

    // 背景参考图（浅色显示，不参与打印）
    let bg_stroke = Stroke::new(1.0, Color32::from_rgb(170, 180, 195));
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

    // 文字 / 表格 / 格子 / 绘图 / 笔记 / 前景图片（深色笔迹）
    let stroke = Stroke::new(1.3, Color32::from_rgb(24, 24, 28));
    for pl in &app.preview {
        if pl.pts.len() < 2 {
            continue;
        }
        for w in pl.pts.windows(2) {
            page_painter.line_segment(
                [map.to_screen(w[0].0, w[0].1), map.to_screen(w[1].0, w[1].1)],
                stroke,
            );
        }
    }

    // 手绘中的笔迹（实时反馈）
    if freehand && acc.len() >= 2 {
        let draft = Stroke::new(1.3, Color32::from_rgb(220, 60, 60));
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
        let edit_stroke = Stroke::new(1.1, Color32::from_rgb(150, 150, 160));
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

    // 选中图片的包围框（按旋转后的四角绘制，旋转/镜像也跟得上）
    if image_tab {
        let sel = app.sel_image;
        if let Some(corners) = app.image_corners(sel) {
            let scr: Vec<Pos2> = corners.iter().map(|&(x, y)| map.to_screen(x, y)).collect();
            let col = if app.images[sel].background {
                Color32::from_rgb(90, 150, 210)
            } else if !app.image_outputs(sel) {
                // 未勾选输出：灰色虚框，提示「看得见但不会打印」
                Color32::from_gray(140)
            } else {
                Color32::from_rgb(210, 120, 40)
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
        ui.painter().text(
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
            Color32::from_gray(150),
        );
    }

    (map, committed)
}

/// 运行中进度页：整页显示，已完成部分黑色、未完成部分浅灰。
/// `frac` ∈ [0,1] 表示完成比例（按行进度估算）。
pub fn draw_progress_page(
    ui: &mut egui::Ui,
    paths: &[Polyline],
    frac: f64,
    page_w_mm: f64,
    page_h_mm: f64,
) {
    let avail = ui.available_rect_before_wrap();
    let (map, page_rect) = layout(avail, page_w_mm, page_h_mm);
    let resp = ui.allocate_rect(avail, egui::Sense::hover());
    let _ = resp;

    let painter = ui.painter().with_clip_rect(page_rect);
    draw_page_bg(&painter, &map, &page_rect, page_w_mm, page_h_mm);

    let total = paths.len();
    let done = ((frac.clamp(0.0, 1.0) * total as f64).round() as usize).min(total);
    let done_stroke = Stroke::new(1.4, Color32::from_rgb(20, 20, 24));
    let todo_stroke = Stroke::new(1.2, Color32::from_rgb(200, 200, 206));

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
            "尚无运行内容：请在「运行中」页点击「开始运行」",
            egui::FontId::proportional(14.0),
            Color32::from_gray(150),
        );
    }
}
