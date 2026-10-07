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
pub fn gray_to_polylines(gray: &image::GrayImage, opts: &ImageGenOpts) -> ImageGenResult {
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

    // draw_type == 2：把行与行串起来画（连续笔画 + 最优路径）
    if opts.draw_type == 2 {
        let polylines = trace_continuous_strokes(&ink, w, h, step, scale);
        return ImageGenResult {
            polylines,
            w_mm,
            h_mm,
        };
    }

    // 横向扫描：每行按 step_px 采样，把连续墨段画成一条横线
    let mut out = Vec::new();
    let mut y = 0u32;
    while y < h {
        let row = (y * w) as usize;
        let mut x = 0u32;
        while x < w {
            let idx = row + x as usize;
            if ink[idx] {
                let start = x;
                while x < w && ink[row + x as usize] {
                    x += 1;
                }
                let end = x.saturating_sub(1);
                let x0 = start as f64 * scale;
                let x1 = (end as f64 + 1.0) * scale;
                let yy = y as f64 * scale;
                out.push(Polyline::new(vec![(x0, yy), (x1, yy)]));
            } else {
                x += 1;
            }
        }
        y += step;
    }

    ImageGenResult {
        polylines: out,
        w_mm,
        h_mm,
    }
}

/// 把墨点掩膜的行内墨段**串成长笔画**（蛇形往复），再做最优路径排序。
///
/// 逐行扫描的写法是：每个墨段自成一条两点折线。写字机上一条折线就等于一次
/// 「抬笔 → 快速空移 → 落笔」（见 `generate`），于是几百行 = 几百次抬落笔，
/// 且每行都是左→右画完再跳回下一行最左端，空移等于整幅画宽——慢就慢在这里。
///
/// 本函数的做法：
/// 1. 逐行取出墨段，若某段的横向范围**覆盖了上一条线段的收笔位置**，就把笔留在纸上顺下去画，
///    并从离笔尖更近的那一端入笔 → 相邻行自然形成左右往复的**蛇形长笔画**；
/// 2. 无法续接的链就地收笔；
/// 3. 最后交给 [`optimize_path_order`] 重排所有长笔画的顺序，把剩余的空移压到最短。
///
/// 抬落笔次数通常能从「几百次」降到「几次」，绘制时间随之大幅下降，
/// 而墨迹内容不变（每一段该画的还是会被画到）。
fn trace_continuous_strokes(
    ink: &[bool],
    w: u32,
    h: u32,
    step: u32,
    scale: f64,
) -> Vec<Polyline> {
    /// 一条正在生长的笔画。
    struct Chain {
        pts: Vec<(f64, f64)>,
        /// 当前收笔位置的 x（**像素**，用于判断能否续接下一行的某个墨段）
        last_x_px: f64,
    }

    let step = step.max(1);
    let mut done: Vec<Polyline> = Vec::new(); // 已收笔的笔画
    let mut open: Vec<Chain> = Vec::new(); // 上一行还能续接的笔画
    let mut y = 0u32;

    while y < h {
        let row = (y * w) as usize;

        // ① 取出本行所有连续墨段 [起, 止]（像素，闭区间）
        let mut runs: Vec<(u32, u32)> = Vec::new();
        let mut x = 0u32;
        while x < w {
            if ink[row + x as usize] {
                let s = x;
                while x < w && ink[row + x as usize] {
                    x += 1;
                }
                runs.push((s, x - 1));
            } else {
                x += 1;
            }
        }

        // ② 每段优先续接上一条线；接不上就另起一笔
        let mut next: Vec<Chain> = Vec::new();
        let yy = y as f64 * scale;
        for &(a, b) in &runs {
            let (fa, fb) = (a as f64, b as f64);
            // 候选：收笔位置落在这段范围内（留 1px 容差）的上一条线，取横向最近的
            let mut pick = usize::MAX;
            let mut pick_d = f64::INFINITY;
            for (i, c) in open.iter().enumerate() {
                if c.last_x_px < fa - 1.0 || c.last_x_px > fb + 1.0 {
                    continue;
                }
                let d = (c.last_x_px - fa).abs().min((c.last_x_px - fb).abs());
                if d < pick_d {
                    pick_d = d;
                    pick = i;
                }
            }
            // 墨段两端对应的毫米坐标（+1 是为了覆盖最后一个像素的完整宽度）
            let pa = fa * scale;
            let pb = (fb + 1.0) * scale;
            if pick != usize::MAX {
                let mut c = open.remove(pick);
                if (c.last_x_px - fb).abs() < (c.last_x_px - fa).abs() {
                    // 从右端入笔、往左扫 → 下一轮的收笔点在左端
                    c.pts.push((pb, yy));
                    c.pts.push((pa, yy));
                    c.last_x_px = fa;
                } else {
                    c.pts.push((pa, yy));
                    c.pts.push((pb, yy));
                    c.last_x_px = fb;
                }
                next.push(c);
            } else {
                next.push(Chain {
                    pts: vec![(pa, yy), (pb, yy)],
                    last_x_px: fb,
                });
            }
        }

        // ③ 上一行没能续上的，就此收笔
        for c in open.drain(..) {
            if c.pts.len() >= 2 {
                done.push(Polyline::new(c.pts));
            }
        }
        open = next;
        y += step;
    }
    for c in open {
        if c.pts.len() >= 2 {
            done.push(Polyline::new(c.pts));
        }
    }

    // ④ 最优路径：重排这些长笔画的顺序并按需反向，把剩余空移压到最短
    optimize_path_order(done)
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
