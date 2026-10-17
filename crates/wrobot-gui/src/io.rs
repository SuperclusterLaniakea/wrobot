//! 导入 / 导出：工程 JSON、Excel、图片、SVG、G-code、PNG。

use std::path::Path;

use calamine::Reader;
use serde_json;
use wrobot_core::doc::Document;
use wrobot_core::render::Polyline;
use wrobot_core::optimize_path_order;

use crate::state::AppState;

/// 打开工程文件（原 `.json` 工程格式）。
pub fn open_project(path: &Path) -> anyhow::Result<Document> {
    Ok(Document::from_file(path)?)
}

/// 保存当前工程为 JSON（复用核心 `Document` 模型）。
pub fn save_project(state: &AppState, path: &Path) -> anyhow::Result<()> {
    let doc = state.compose_doc();
    let s = doc.to_json_pretty()?;
    std::fs::write(path, s)?;
    Ok(())
}

/// 导出 SVG 预览。
pub fn export_svg(state: &mut AppState, path: &Path) -> anyhow::Result<()> {
    let svg = state.generate_svg();
    std::fs::write(path, svg)?;
    Ok(())
}

/// 导出 G-code（按当前固件选项）。
pub fn export_gcode(state: &mut AppState, path: &Path) -> anyhow::Result<()> {
    let g = state.generate_gcode();
    std::fs::write(path, g)?;
    Ok(())
}

/// 把笔迹栅格化并保存为 PNG（白底黑线）。
pub fn export_png(state: &AppState, path: &Path) -> anyhow::Result<()> {
    let px_per_mm = 96.0 / 25.4 * 2.0; // ~192 dpi
    let w = (state.page_w_mm * px_per_mm).ceil() as u32;
    let h = (state.page_h_mm * px_per_mm).ceil() as u32;
    let mut buf = vec![255u8; (w * h * 4) as usize];
    let set = |buf: &mut [u8], x: i32, y: i32| {
        if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
            return;
        }
        let i = ((y as u32 * w + x as u32) * 4) as usize;
        buf[i] = 20;
        buf[i + 1] = 20;
        buf[i + 2] = 20;
        buf[i + 3] = 255;
    };
    for pl in &state.preview {
        for w2 in pl.pts.windows(2) {
            let (x0, y0) = (w2[0].0 * px_per_mm, w2[0].1 * px_per_mm);
            let (x1, y1) = (w2[1].0 * px_per_mm, w2[1].1 * px_per_mm);
            let dx = x1 - x0;
            let dy = y1 - y0;
            let steps = dx.abs().max(dy.abs()).ceil() as i32 + 1;
            for s in 0..=steps {
                let t = s as f64 / steps as f64;
                let px = (x0 + dx * t).round() as i32;
                let py = (y0 + dy * t).round() as i32;
                set(&mut buf, px, py);
            }
        }
    }
    image::save_buffer(path, &buf, w, h, image::ColorType::Rgba8)?;
    Ok(())
}

/// 把图片二值化后以「横向影线」方式转为绘图折线（毫米坐标，y 向下）。
/// 导出为单页矢量 PDF（纯 Rust 生成，无外部依赖）。
pub fn export_pdf(state: &mut AppState, path: &Path) -> anyhow::Result<()> {
    let bytes = state.generate_pdf();
    std::fs::write(path, bytes)?;
    Ok(())
}

/// 图片生成选项。
#[derive(Clone, Copy)]
pub struct ImageGenOpts {
    /// 目标高度（毫米）：图片按该高度等比缩放
    pub target_h_mm: f64,
    /// 影线/采样行间隔（像素）：越大线越疏
    pub step_px: u32,
    /// 暗部阈值（0..1，越低越黑）——素描模式
    pub threshold: f64,
    /// 边缘强度阈值（0..1）——线框模式
    pub edge_threshold: f64,
    /// 反相（黑变白、白变黑）
    pub invert: bool,
    /// 绘制类型：0 线框（轮廓） / 1 素描（影线） / 2 高效（连续笔画）
    pub draw_type: usize,
    /// **裁剪区域**（相对原图宽高的比例）：`(左, 上, 右, 下)`；`(0,0,1,1)` = 不裁剪。
    pub crop: (f64, f64, f64, f64),
    /// **扫描方向**：0 横向(→) / 1 对角 45°(↘) / 2 纵向(↓) / 3 对角 135°(↗)。
    ///
    /// 影线不再只能横着走：竖版人像用纵向、斜纹质感用 45°/135°。
    pub scan_angle: u8,
    /// **连笔跨行容差**（像素，连续笔画模式）：相邻扫描线墨段端点的距离
    /// ≤ 行距 + 该值 就笔不离纸接续。0 = 只接紧挨着的；调大可以把隔得较远的
    /// 段也连成长笔画（抬笔更少，但会多画连接线）。
    pub merge_gap_px: u32,
    /// **笔画顺序**：0 默认（线框/素描按扫描顺序、高效按最优路径）/
    /// 1 最优路径（最近邻+2-opt）/ 2 严格按扫描顺序 / 3 分块蛇形。
    pub order_mode: u8,
    /// 分块边长（像素，`order_mode == 3` 时生效）：先块内最优，块间蛇形走。
    pub block_px: u32,
}

/// 判断裁剪比例是否真的裁掉了内容（全 0/1 视为不裁剪）。
fn is_cropped(c: (f64, f64, f64, f64)) -> bool {
    c.0 > 1e-6 || c.1 > 1e-6 || c.2 < 1.0 - 1e-6 || c.3 < 1.0 - 1e-6
}

/// 按比例裁剪灰度图，返回裁剪后的子图。
fn crop_gray(gray: &image::GrayImage, c: (f64, f64, f64, f64)) -> image::GrayImage {
    let (w, h) = gray.dimensions();
    let x0 = (c.0.clamp(0.0, 1.0) * w as f64).floor() as u32;
    let y0 = (c.1.clamp(0.0, 1.0) * h as f64).floor() as u32;
    let x1 = (c.2.clamp(0.0, 1.0) * w as f64).ceil() as u32;
    let y1 = (c.3.clamp(0.0, 1.0) * h as f64).ceil() as u32;
    let x0 = x0.min(w.saturating_sub(1));
    let y0 = y0.min(h.saturating_sub(1));
    let x1 = x1.max(x0 + 1).min(w);
    let y1 = y1.max(y0 + 1).min(h);
    image::imageops::crop_imm(gray, x0, y0, x1 - x0, y1 - y0).to_image()
}

/// 图片生成结果。
pub struct ImageGenResult {
    pub polylines: Vec<Polyline>,
    /// 基础尺寸（毫米，scale=1 时）
    pub w_mm: f64,
    pub h_mm: f64,
}

/// 把图片字节解码并按指定模式转为矢量笔迹折线（基础毫米坐标，y 向下，原点在左上角）。
///
/// - `draw_type == 0`（线框）：用 Sobel 边缘检测提取轮廓，再以横向影线描绘边缘；
/// - `draw_type == 1`（素描）：直接对暗部做横向影线填充；
/// - `draw_type == 2`（高效）：同上判定暗部，但把相邻行的墨段**串成连续笔画**
///   并做最优路径排序，抬落笔次数大幅下降，绘制时间最短。
/// 反相会把黑/白对调。返回的折线尺寸为 `target_h_mm` 高、按图片宽高比计算宽。
pub fn image_to_polylines(data: &[u8], opts: &ImageGenOpts) -> anyhow::Result<ImageGenResult> {
    let img = image::load_from_memory(data)?;
    let gray = img.to_luma8();
    // 裁剪统一在 `gray_to_polylines` 里做，这里不再重复裁
    Ok(gray_to_polylines(&gray, opts))
}

/// 把 PDF 里取出的图像（灰度 / RGB / JPEG）按同样规则转成笔迹。
pub fn pdf_image_to_polylines(
    im: &wrobot_core::PdfImage,
    opts: &ImageGenOpts,
) -> anyhow::Result<ImageGenResult> {
    use wrobot_core::PdfImageData;
    let gray: image::GrayImage = match &im.data {
        PdfImageData::Jpeg(b) => image::load_from_memory(b)?.to_luma8(),
        PdfImageData::Gray(v) => image::GrayImage::from_raw(im.w, im.h, v.clone())
            .ok_or_else(|| anyhow::anyhow!("PDF 灰度图像数据长度不符"))?,
        PdfImageData::Rgb(v) => {
            let rgb = image::RgbImage::from_raw(im.w, im.h, v.clone())
                .ok_or_else(|| anyhow::anyhow!("PDF RGB 图像数据长度不符"))?;
            let mut g = image::GrayImage::new(im.w, im.h);
            for (d, s) in g.pixels_mut().zip(rgb.pixels()) {
                let [r, gg, b] = s.0;
                d.0[0] = (0.299 * r as f32 + 0.587 * gg as f32 + 0.114 * b as f32) as u8;
            }
            g
        }
    };
    Ok(gray_to_polylines(&gray, opts))
}

/// 灰度图 → 笔迹（影线/轮廓），页面毫米坐标，y 向下，原点在左上角。
///
/// 裁剪在这里统一处理（按 `opts.crop` 比例裁掉后再描线），
/// 因此 `image_to_polylines` / `pdf_image_to_polylines` 都不必各自再裁一次。
pub fn gray_to_polylines(gray: &image::GrayImage, opts: &ImageGenOpts) -> ImageGenResult {
    let cropped;
    let gray: &image::GrayImage = if is_cropped(opts.crop) {
        cropped = crop_gray(gray, opts.crop);
        &cropped
    } else {
        gray
    };
    let (w, h) = gray.dimensions();
    if w == 0 || h == 0 {
        return ImageGenResult {
            polylines: vec![],
            w_mm: 0.0,
            h_mm: 0.0,
        };
    }
    let scale = (opts.target_h_mm / h as f64).max(1e-4); // mm / px
    let w_mm = scale * w as f64;
    let h_mm = scale * h as f64;

    // 计算每像素的「墨」判定（true = 需要描线）
    let thr = (opts.threshold * 255.0).clamp(0.0, 255.0);
    let edge_thr = (opts.edge_threshold * 255.0).clamp(0.0, 255.0);
    let mut ink = vec![false; (w * h) as usize];
    if opts.draw_type == 0 {
        // 线框：Sobel 边缘强度
        let mag = sobel_magnitude(&gray, w, h);
        let mut maxv = 1.0f64;
        for &m in &mag {
            if m > maxv {
                maxv = m;
            }
        }
        for i in 0..(w * h) as usize {
            let on = (mag[i] / maxv * 255.0) >= edge_thr;
            ink[i] = if opts.invert { !on } else { on };
        }
    } else {
        // 素描：暗部
        for y in 0..h {
            for x in 0..w {
                let lum = gray.get_pixel(x, y).0[0] as f64;
                let on = lum < thr;
                ink[(y * w + x) as usize] = if opts.invert { !on } else { on };
            }
        }
    }

    let step = opts.step_px.max(1);
    let lines = scan_lines(w, h, step, opts.scan_angle);
    let to_mm = |p: (u32, u32)| (p.0 as f64 * scale, p.1 as f64 * scale);

    // draw_type == 2：把相邻扫描线的墨段**串起来**画（连续笔画），再按顺序模式排序
    if opts.draw_type == 2 {
        let polylines = trace_continuous_strokes(
            &ink,
            &lines,
            w,
            step,
            scale,
            opts.merge_gap_px,
            opts.order_mode,
            opts.block_px,
        );
        return ImageGenResult {
            polylines,
            w_mm,
            h_mm,
        };
    }

    // 线框 / 素描：每条扫描线上的一段连续墨 → 一条横线（两点折线）
    let mut out = Vec::new();
    for line in &lines {
        for (s, e) in line_runs(line, &ink, w) {
            out.push(Polyline::new(vec![to_mm(line[s]), to_mm(line[e])]));
        }
    }
    // 顺序模式：默认保持扫描顺序（历史行为）；显式选了才重排
    out = order_strokes(out, opts.order_mode, false, opts.block_px, w, scale);

    ImageGenResult {
        polylines: out,
        w_mm,
        h_mm,
    }
}

/// 生成**扫描线**：每条线是沿扫描方向依次排列的像素坐标。
///
/// `angle`：0 横向（按 y 分行，沿 +x） / 1 对角 45°（按 `x−y` 分线，沿 (1,1)） /
/// 2 纵向（按 x 分列，沿 +y） / 3 对角 135°（按 `x+y` 分线，沿 (1,−1)）。
/// 线与线之间相隔 `step` 个像素。对角线使影线能斜着走，出斜纹质感。
fn scan_lines(w: u32, h: u32, step: u32, angle: u8) -> Vec<Vec<(u32, u32)>> {
    let step = step.max(1);
    let mut lines = Vec::new();
    match angle {
        2 => {
            // 纵向：固定 x，沿 y 走
            for x in (0..w).step_by(step as usize) {
                lines.push((0..h).map(|y| (x, y)).collect());
            }
        }
        1 | 3 => {
            // 对角线。45°：c = x−y ∈ [−(h−1), w−1]；135°：c = x+y ∈ [0, w+h−2]
            let (cmin, cmax) = if angle == 1 {
                (-(h as i64 - 1), w as i64 - 1)
            } else {
                (0i64, (w + h - 2) as i64)
            };
            let mut c = cmin;
            while c <= cmax {
                let (xa, xb) = if angle == 1 {
                    (c.max(0), (w as i64 - 1).min(c + h as i64 - 1))
                } else {
                    ((c - (h as i64 - 1)).max(0), (w as i64 - 1).min(c))
                };
                let mut line = Vec::new();
                let mut x = xa;
                while x <= xb {
                    let y = if angle == 1 { x - c } else { c - x };
                    if y >= 0 && y < h as i64 {
                        line.push((x as u32, y as u32));
                    }
                    x += 1;
                }
                if !line.is_empty() {
                    lines.push(line);
                }
                c += step as i64;
            }
        }
        _ => {
            // 横向：固定 y，沿 x 走（历史默认）
            for y in (0..h).step_by(step as usize) {
                lines.push((0..w).map(|x| (x, y)).collect());
            }
        }
    }
    lines
}

/// 取一条扫描线上的**连续墨段**（行内向量下标连续即像素相邻），返回 `(起, 止)` 下标对。
fn line_runs(line: &[(u32, u32)], ink: &[bool], w: u32) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut i = 0;
    while i < line.len() {
        let (x, y) = line[i];
        if ink[(y * w + x) as usize] {
            let s = i;
            while i < line.len() {
                let (x2, y2) = line[i];
                if !ink[(y2 * w + x2) as usize] {
                    break;
                }
                i += 1;
            }
            runs.push((s, i - 1));
        } else {
            i += 1;
        }
    }
    runs
}

/// 按「顺序模式」重排笔画。
///
/// - `mode == 1`：最优路径（最近邻 + 2-opt，允许笔画反向）；
/// - `mode == 2`：严格按扫描顺序（不重排）；
/// - `mode == 3`：分块蛇形——按 `block_px` 把画面切成方格，先块内最优，
///   块与块之间按**蛇形**（偶数行从左到右、奇数行从右到左）走，像犁地一样；
/// - `mode == 0`：默认——`default_opt` 为 true（高效连续笔画）时最优路径，
///   否则（线框/素描）保持扫描顺序，与历史行为一致。
fn order_strokes(
    done: Vec<Polyline>,
    mode: u8,
    default_opt: bool,
    block_px: u32,
    w: u32,
    scale: f64,
) -> Vec<Polyline> {
    match mode {
        1 => optimize_path_order(done),
        2 => done,
        3 => block_serpentine(done, block_px, w, scale),
        _ => {
            if default_opt {
                optimize_path_order(done)
            } else {
                done
            }
        }
    }
}

/// 分块蛇形排序：把画面切成 `block_px` 见方的块，块内做最优路径，
/// 块间按蛇形（boustrophedon）顺序串联，来回「犁地」，避免整幅图乱跳。
fn block_serpentine(done: Vec<Polyline>, block_px: u32, w: u32, scale: f64) -> Vec<Polyline> {
    let bw = (block_px.max(8) as f64 * scale).max(1e-6); // 块边长（毫米）
    let cols = (((w as f64 * scale) / bw).ceil() as i64).max(1);
    let mut groups: std::collections::BTreeMap<(i64, i64), Vec<Polyline>> = Default::default();
    for pl in done {
        let (x, y) = pl.pts.first().copied().unwrap_or((0.0, 0.0));
        let bx = ((x / bw) as i64).clamp(0, cols - 1);
        let by = (y / bw) as i64;
        groups.entry((by, bx)).or_default().push(pl);
    }
    let rows = groups.keys().map(|k| k.0).max().unwrap_or(0) + 1;
    let mut out = Vec::new();
    for by in 0..rows {
        let mut xs: Vec<i64> = groups.keys().filter(|k| k.0 == by).map(|k| k.1).collect();
        xs.sort_unstable();
        if by % 2 == 1 {
            xs.reverse(); // 蛇形：奇数行倒着走
        }
        for bx in xs {
            if let Some(g) = groups.remove(&(by, bx)) {
                out.extend(optimize_path_order(g));
            }
        }
    }
    out
}

/// 把扫描线上的墨段**串成长笔画**，再做顺序排序。
///
/// 逐线扫描的写法是：每个墨段自成一条两点折线。写字机上一条折线就等于一次
/// 「抬笔 → 快速空移 → 落笔」（见 `generate`），于是几百行 = 几百次抬落笔，
/// 且每行都是左→右画完再跳回下一行最左端，空移等于整幅画宽——慢就慢在这里。
///
/// 本函数的做法：
/// 1. 逐条扫描线取出墨段；若某段的端点与上一条开放笔画的收笔位置距离
///    ≤ 行距 + `merge_gap_px`，就把笔留在纸上顺下去画，并从离笔尖更近的那一端入笔
///    → 相邻线自然形成左右往复的**蛇形长笔画**；
/// 2. 续不上的链就地收笔；
/// 3. 最后交给 [`order_strokes`] 按 `order_mode` 重排（默认最优路径），把剩余空移压到最短。
///
/// `merge_gap_px = 0` 时只接「紧挨着」的段（跨行距离≈行距）；
/// 调大可以把隔得较远的段也连成长笔画——抬笔更少，但会多画连接线。
fn trace_continuous_strokes(
    ink: &[bool],
    lines: &[Vec<(u32, u32)>],
    w: u32,
    step_px: u32,
    scale: f64,
    merge_gap_px: u32,
    order_mode: u8,
    block_px: u32,
) -> Vec<Polyline> {
    /// 一条正在生长的笔画。
    struct Chain {
        pts: Vec<(f64, f64)>,
        /// 当前收笔位置（**像素**，用于判断能否续接下一条线的某个墨段）
        last: (f64, f64),
    }

    let gap = merge_gap_px as f64;
    // 允许续接的距离：扫描线间距（step）+ 用户容差。
    // 用像素欧氏距离统一处理横向/纵向/对角线各种扫描方向。
    let threshold = step_px.max(1) as f64 + gap;

    let to_mm = |p: (u32, u32)| (p.0 as f64 * scale, p.1 as f64 * scale);
    let dist = |a: (f64, f64), b: (u32, u32)| {
        ((a.0 - b.0 as f64).powi(2) + (a.1 - b.1 as f64).powi(2)).sqrt()
    };

    let mut done: Vec<Polyline> = Vec::new(); // 已收笔的笔画
    let mut open: Vec<Chain> = Vec::new(); // 上一条线还能续接的笔画

    for line in lines {
        // ① 取出本条线所有连续墨段 [起, 止]（行内下标，闭区间）
        let mut next: Vec<Chain> = Vec::new();
        for (s, e) in line_runs(line, ink, w) {
            let p_start = line[s];
            let p_end = line[e];
            let (ms, me) = (to_mm(p_start), to_mm(p_end));

            // ② 找一条收笔位置离这段两端最近、且在容差内的开放笔画
            let mut pick = usize::MAX;
            let mut pick_d = f64::INFINITY;
            let mut pick_near_start = true;
            for (ci, c) in open.iter().enumerate() {
                let d_s = dist(c.last, p_start);
                let d_e = dist(c.last, p_end);
                let (d, near_start) = if d_s <= d_e { (d_s, true) } else { (d_e, false) };
                if d <= threshold && d < pick_d {
                    pick_d = d;
                    pick = ci;
                    pick_near_start = near_start;
                }
            }

            if pick != usize::MAX {
                let mut c = open.remove(pick);
                if pick_near_start {
                    // 从段首入笔：…收笔点 → 段首 → 段尾
                    c.pts.push(ms);
                    c.pts.push(me);
                    c.last = (p_end.0 as f64, p_end.1 as f64);
                } else {
                    // 从段尾入笔：…收笔点 → 段尾 → 段首（反向扫）
                    c.pts.push(me);
                    c.pts.push(ms);
                    c.last = (p_start.0 as f64, p_start.1 as f64);
                }
                next.push(c);
            } else {
                next.push(Chain {
                    pts: vec![ms, me],
                    last: (p_end.0 as f64, p_end.1 as f64),
                });
            }
        }

        // ③ 上一条线没能续上的，就此收笔
        for c in open.drain(..) {
            if c.pts.len() >= 2 {
                done.push(Polyline::new(c.pts));
            }
        }
        open = next;
    }
    for c in open {
        if c.pts.len() >= 2 {
            done.push(Polyline::new(c.pts));
        }
    }

    // ④ 按顺序模式重排（高效模式默认最优路径），把剩余空移压到最短
    order_strokes(done, order_mode, true, block_px, w, scale)
}

/// Sobel 边缘强度（0..~根号2*255），返回按 (y*w+x) 索引的向量。
fn sobel_magnitude(img: &image::GrayImage, w: u32, h: u32) -> Vec<f64> {
    let at = |x: i32, y: i32| -> f64 {
        if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
            0.0
        } else {
            img.get_pixel(x as u32, y as u32).0[0] as f64
        }
    };
    let mut out = vec![0.0; (w * h) as usize];
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let gx = -at(x - 1, y - 1) - 2.0 * at(x - 1, y) - at(x - 1, y + 1)
                + at(x + 1, y - 1)
                + 2.0 * at(x + 1, y)
                + at(x + 1, y + 1);
            let gy = -at(x - 1, y - 1) - 2.0 * at(x, y - 1) - at(x + 1, y - 1)
                + at(x - 1, y + 1)
                + 2.0 * at(x, y + 1)
                + at(x + 1, y + 1);
            out[(y * w as i32 + x) as usize] = (gx * gx + gy * gy).sqrt();
        }
    }
    out
}

/// 导入 Excel 首个工作表，填充到「表格」模块。
pub fn import_excel(state: &mut AppState, path: &Path) -> anyhow::Result<usize> {
    let mut wb = calamine::open_workbook_auto(path)?;
    let sheet = wb
        .sheet_names()
        .get(0)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("工作簿为空"))?;
    let range = wb.worksheet_range(&sheet)?;
    let mut rows: Vec<String> = Vec::new();
    let mut max_cols = 0usize;
    for r in range.rows() {
        let cells: Vec<String> = r
            .iter()
            .map(|c| c.to_string().trim().to_string())
            .collect();
        max_cols = max_cols.max(cells.len());
        rows.push(cells.join("\t"));
    }
    state.table_text = rows.join("\n");
    state.table_rows = state.table_text.lines().count().max(1);
    state.table_cols = max_cols.max(1);
    Ok(state.table_rows)
}

/// 完整工程保存：序列化除「字库 / 设备连接」等运行时对象外的全部可编辑状态为 JSON。
///
/// 字库和设备连接不写入文件——载入时保留当前会话的字库与硬件连接，只恢复页面内容。
pub fn save_project_full(state: &AppState, path: &Path) -> anyhow::Result<()> {
    let s = serde_json::to_string_pretty(state)?;
    std::fs::write(path, s)?;
    Ok(())
}

/// 完整工程载入：把文件里的可编辑状态恢复到 `app`，保留当前字库与设备连接。
pub fn load_project_full(app: &mut AppState, path: &Path) -> anyhow::Result<()> {
    let s = std::fs::read_to_string(path)?;
    let other: AppState = serde_json::from_str(&s)?;
    // 保留当前会话的运行时对象（字库 / 设备连接 / 串口列表）
    let device = std::mem::take(&mut app.device);
    let connected = device.is_connected();
    let log = device.log_snapshot();
    let mut loaded = other;
    loaded.fonts = std::mem::take(&mut app.fonts);
    loaded.font_dir = std::mem::take(&mut app.font_dir);
    loaded.device = device;
    loaded.serial_ports = std::mem::take(&mut app.serial_ports);
    loaded.dev_connected = connected;
    loaded.dev_log = log;
    loaded.tab = app.tab; // 保持当前所在页
    loaded.preview = Vec::new();
    loaded.run_preview = Vec::new();
    loaded.gcode_text = String::new();
    loaded.status = "工程已载入".into();
    loaded.img_drag = None;
    loaded.freehand = Vec::new();
    *app = loaded;
    app.rebuild_preview();
    Ok(())
}
