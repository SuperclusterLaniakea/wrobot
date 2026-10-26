//! 导入 / 导出：工程 JSON、Excel、图片、SVG、G-code、PNG。

use std::path::Path;

use calamine::Reader;
use serde_json;
use wrobot_core::render::Polyline;
use wrobot_core::optimize_path_order;

use crate::state::AppState;

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
    // 多笔：除了合并版，再给每支笔各写一份可直接单独运行的文件。
    // 换笔时不用再在界面里点，文件名就写着第几支、什么笔。
    let jobs = state.build_pen_jobs();
    if jobs.len() >= 2 {
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "output".to_string());
        for (i, job) in jobs.iter().enumerate() {
            let safe: String = job
                .name
                .chars()
                .map(|c| {
                    if c.is_alphanumeric() || c == '-' || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            let p = path.with_file_name(format!("{stem}-pen{}-{safe}.nc", i + 1));
            std::fs::write(&p, &job.gcode)?;
        }
        state.status = format!(
            "已导出合并版 + {} 份分笔 G-code（换笔后各跑一份）",
            jobs.len()
        );
    }
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

/// 空笔组（单笔模式用）。`pen_count = 0` 时算法不会读它。
pub const NO_PENS: [crate::sketch::HatchPen; 4] = [
    crate::sketch::HatchPen {
        width_mm: 0.5,
        darkness: 0.6,
    };
    4
];

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
    /// 绘制类型：0 线框 / 1 素描 / 2 连续笔画 / 3 中心线 / 4 描边 /
    /// 5 交叉排线 / 6 网点 / 7 素描原稿 / 8 素描调子（分层排线）/
    /// 9 分区重绘 / 10 素描笔触（融合布线）。
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
    /// **素描原稿模式**（实验）：暗部线条最多叠描几遍（1 = 只描一遍）。
    ///
    /// 叠几遍由每条笔迹的明暗和宽度决定；偏移小于笔宽，叠成一根更重的线。
    pub sketch_pressure: u32,
    /// **素描原稿模式**：手抖 / 重描的偏移幅度（毫米）。
    ///
    /// 沿线条法向做低频值噪声起伏，并给长线加一点弓形；0 = 严格描摹原稿。
    pub sketch_jitter: f64,
    /// **素描原稿模式**：实际画笔的线宽（毫米）。
    ///
    /// 这是素描还原的物理锚点：笔有多宽，决定了一笔能盖住多少纸、两根线最近能
    /// 排多密（再密就糊成一块黑）。算法按它算「这块地方最多能放多少线」，
    /// 所以填错了明暗层次一定是错的。0.3 = 细针管笔，0.5 = 中性笔，0.8 = 马克笔。
    pub sketch_pen: f64,
    /// **素描调子模式**：明暗层数（3..=8）。亮部只用第一层，暗部逐层叠加交叉排线。
    pub hatch_levels: u32,
    /// **素描调子模式**：第一层排线的基准角（度）。
    pub hatch_angle: f64,
    /// **分区重绘模式**：方向采样块的边长（毫米）。
    ///
    /// 块只用来决定「这一小片排线往哪儿走」，不参与定层：块越细，鼻梁、
    /// 眼窝这些相邻但走向不同的地方越能各走各的；默认 2.5mm。
    pub region_block_mm: f64,
    /// **素描笔触模式**：单根短直线的长度（毫米）。
    ///
    /// 真实素描除轮廓外大多是 1~5mm 的短直线；调长会成排线，调短会变碎。
    pub stroke_len_mm: f64,
    /// **素描笔触模式**：走向参考。0 = 跟随轮廓，1 = 跟随原稿笔触，2 = 混合。
    pub stroke_follow: u8,
    /// **多笔分层**：把明暗带分给笔组里的不同笔，而不是一支笔硬排到底。
    pub multi_pen: bool,
    /// 参与多笔分层的笔（最多 4 支，按亮 → 暗排列）。
    pub pens: [crate::sketch::HatchPen; 4],
    /// `pens` 里实际有效的支数。
    pub pen_count: u8,
}

impl Default for ImageGenOpts {
    fn default() -> Self {
        Self {
            target_h_mm: 60.0,
            step_px: 3,
            threshold: 0.5,
            edge_threshold: 0.5,
            invert: false,
            draw_type: 0,
            crop: (0.0, 0.0, 1.0, 1.0),
            scan_angle: 0,
            merge_gap_px: 8,
            order_mode: 0,
            block_px: 40,
            sketch_pressure: 3,
            sketch_jitter: 0.12,
            sketch_pen: 0.5,
            hatch_levels: 6,
            hatch_angle: 35.0,
            region_block_mm: 2.5,
            stroke_len_mm: 3.2,
            stroke_follow: 2,
            multi_pen: false,
            pens: NO_PENS,
            pen_count: 0,
        }
    }
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
    /// 每根折线由哪支笔绘制（下标对应 `ImageGenOpts::pens`）。
    /// 单笔模式或非调子模式全是 0。
    pub pens: Vec<u8>,
    /// 基础尺寸（毫米，scale=1 时）
    pub w_mm: f64,
    pub h_mm: f64,
}

/// 把图片字节解码并按指定模式转为矢量笔迹折线（基础毫米坐标，y 向下，原点在左上角）。
///
/// - `draw_type == 0`（线框）：Sobel 边缘检测 + 影线描边；
/// - `draw_type == 1`（素描）：暗部逐行影线填充；
/// - `draw_type == 2`（高效）：相邻墨段串成连续笔画并做路径优化；
/// - `draw_type == 3`（中心线）：Zhang–Suen 细化成骨架后描线；
/// - `draw_type == 4`（描边）：marching squares 等值轮廓；
/// - `draw_type == 5`（交叉排线）：主方向影线 + 暗部垂直排线；
/// - `draw_type == 6`（网点）：按灰度生成半调网点；
/// - `draw_type == 7`（素描原稿）：逐笔识别骨架、按切线接回交叉点，
///   分层还原成手绘笔迹（见 [`crate::sketch`]）。
/// 反相会把黑/白对调。返回的折线尺寸为 `target_h_mm` 高、按图片宽高比计算宽。
pub fn image_to_polylines(data: &[u8], opts: &ImageGenOpts) -> anyhow::Result<ImageGenResult> {
    let img = image::load_from_memory(data)?;
    let mut gray = img.to_luma8();
    // 扫描/翻拍的素描稿纸是米黄、米灰的，不是纯白：实测这批素材纸色在
    // 200~230（换算成墨量 0.10~0.22）。不归一化的话整张背景都带着"墨"，
    // 纸纹会被当成线提出来，而真正的浅笔触又沉在底色里分不出来。
    // 色阶拉伸是所有阈值化模式的前提，不只素描原稿。
    stretch_levels(&mut gray);
    // 裁剪统一在 `gray_to_polylines` 里做，这里不再重复裁
    Ok(gray_to_polylines(&gray, opts))
}

/// **色阶归一化**（扫描稿的白场/黑场设定）。
///
/// 把「这张纸有多白」测出来，再把它拉到纯白；把「最深的墨」测出来，拉到纯黑。
/// 之后 0 = 这张纸的本色，1 = 笔尖能压出的最黑。
///
/// 两个端点都从直方图里量，不写死：
/// - **白场** = 亮端（170..250）出现最多的那个灰阶，也就是纸的本色。只取这个
///   区间是为了避开「大片高光」（额头、白衣服）被误当成纸；
/// - **黑场** = 最暗的 1% 分位，即画里真正压下去的那几笔。
///
/// 为什么不能用固定值：同一批素材里纸色从 200 到 230 都有差，而阈值判据（纸白
/// 门限 0.20、Hessian 的对比度）全部建立在「纸 = 0 墨」这个前提上。纸色 200 的
/// 一张，如果不拉伸，整张背景的墨量就是 0.216——比门限还高，背景直接被当画。
///
/// 安全边界：纸色低于 170（图本身偏暗/不是纸本素描）或明暗跨度不足 60 时不动，
/// 免得把一张正常的黑白图拉坏。
pub(crate) fn stretch_levels(gray: &mut image::GrayImage) {
    let (w, h) = gray.dimensions();
    let n = (w as usize) * (h as usize);
    if n < 256 {
        return;
    }
    let mut hist = [0u32; 256];
    for p in gray.pixels() {
        hist[p[0] as usize] += 1;
    }
    // 白场：亮端众数
    let mut paper = 0usize;
    let mut best = 0u32;
    for v in 170..=250 {
        if hist[v] > best {
            best = hist[v];
            paper = v;
        }
    }
    if best == 0 || paper < 170 {
        return;
    }
    // 黑场：最暗的 1%
    let mut acc = 0u32;
    let mut dark = 0usize;
    let want = (n as u32 / 100).max(1);
    for v in 0..256 {
        acc += hist[v];
        if acc >= want {
            dark = v;
            break;
        }
    }
    if dark > 100 || paper.saturating_sub(dark) < 60 {
        return;
    }
    let span = (paper - dark) as f32;
    let d0 = dark as f32;
    for p in gray.pixels_mut() {
        let v = (p[0] as f32 - d0) / span;
        p[0] = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    }
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
    let base: &image::GrayImage = if is_cropped(opts.crop) {
        cropped = crop_gray(gray, opts.crop);
        &cropped
    } else {
        gray
    };
    // 骨架细化是迭代算法、等值轮廓会生成大量小线段：大图先缩到长边 1600px，
    // 既避免界面卡死，也保证绘图精度（再高的分辨率对写字机没有意义）。
    let down;
    let gray: &image::GrayImage = if matches!(opts.draw_type, 3 | 4 | 7 | 8 | 9 | 10)
        && base.width().max(base.height()) > 1600
    {
        let k = 1600.0 / base.width().max(base.height()) as f64;
        let nw = ((base.width() as f64 * k).round() as u32).max(1);
        let nh = ((base.height() as f64 * k).round() as u32).max(1);
        down = image::imageops::thumbnail(base, nw, nh);
        &down
    } else {
        base
    };
    let (w, h) = gray.dimensions();
    if w == 0 || h == 0 {
        return ImageGenResult {
            polylines: vec![],
            pens: vec![],
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
        let mag = sobel_magnitude(gray, w, h);
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
        // 素描/连续/中心线/描边/交叉排线：暗部
        for y in 0..h {
            for x in 0..w {
                let lum = gray.get_pixel(x, y).0[0] as f64;
                let on = lum < thr;
                ink[(y * w + x) as usize] = if opts.invert { !on } else { on };
            }
        }
    }

    let step = opts.step_px.max(1);

    match opts.draw_type {
        // 高效：把相邻扫描线的墨段串起来画，再按顺序模式排序
        2 => {
            let lines = scan_lines(w, h, step, opts.scan_angle);
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
                pens: vec![0; polylines.len()],
                polylines,
                w_mm,
                h_mm,
            };
        }
        // 中心线（骨架）
        3 => {
            let skel = thin_ink(&ink, w, h);
            let polylines = trace_skeleton(&skel, w, h, scale, opts.order_mode, opts.block_px);
            return ImageGenResult {
                pens: vec![0; polylines.len()],
                polylines,
                w_mm,
                h_mm,
            };
        }
        // 描边（等值轮廓）
        4 => {
            let polylines = contour_polylines(&ink, w, h, scale, opts.order_mode, opts.block_px);
            return ImageGenResult {
                pens: vec![0; polylines.len()],
                polylines,
                w_mm,
                h_mm,
            };
        }
        // 网点（半调）：不按扫描线，直接按灰度生成大小不一的网点
        6 => {
            let polylines = halftone_polylines(
                gray,
                w,
                h,
                scale,
                step,
                opts.threshold,
                opts.invert,
                opts.order_mode,
                opts.block_px,
            );
            return ImageGenResult {
                pens: vec![0; polylines.len()],
                polylines,
                w_mm,
                h_mm,
            };
        }
        // 素描原稿（实验）：逐笔识别 + 分层手绘还原。
        7 => {
            let polylines = crate::sketch::sketch_polylines(
                gray,
                w,
                h,
                scale,
                opts.threshold,
                opts.invert,
                opts.sketch_pressure,
                opts.sketch_jitter,
                opts.sketch_pen,
                opts.order_mode,
                opts.block_px,
            );
            return ImageGenResult {
                pens: vec![0; polylines.len()],
                polylines,
                w_mm,
                h_mm,
            };
        }
        // 素描调子（实验）：灰度 → 多层排线 + 轮廓重线。
        8 => {
            let base_deg = match opts.scan_angle {
                1 => 45.0,
                2 => 90.0,
                3 => 135.0,
                _ => 0.0,
            };
            let hatch = crate::sketch::hatch_polylines(
                gray,
                w,
                h,
                scale,
                opts.threshold,
                opts.invert,
                opts.hatch_levels,
                opts.sketch_jitter,
                opts.sketch_pen,
                opts.order_mode,
                opts.block_px,
                opts.hatch_angle + base_deg,
                opts.step_px,
                opts.edge_threshold,
                &opts.pens[..usize::from(opts.pen_count).min(opts.pens.len())],
                opts.multi_pen,
            );
            return ImageGenResult {
                polylines: hatch.polylines,
                pens: hatch.pens,
                w_mm,
                h_mm,
            };
        }
        // 分区重绘（实验）：量出逐像素深度层 → 细块定方向 → 分层用不同铅笔压暗。
        9 => {
            let base_deg = match opts.scan_angle {
                1 => 45.0,
                2 => 90.0,
                3 => 135.0,
                _ => 0.0,
            };
            let region = crate::sketch::region_polylines(
                gray,
                w,
                h,
                scale,
                opts.threshold,
                opts.invert,
                opts.hatch_levels,
                opts.region_block_mm,
                opts.sketch_jitter,
                opts.sketch_pen,
                opts.order_mode,
                opts.block_px,
                opts.hatch_angle + base_deg,
                opts.edge_threshold,
                &opts.pens[..usize::from(opts.pen_count).min(opts.pens.len())],
                opts.multi_pen,
            );
            return ImageGenResult {
                polylines: region.polylines,
                pens: region.pens,
                w_mm,
                h_mm,
            };
        }
        // 素描笔触（融合）：残差决定密度，短直线决定质感，方向沿轮廓/原稿。
        10 => {
            let base_deg = match opts.scan_angle {
                1 => 45.0,
                2 => 90.0,
                3 => 135.0,
                _ => 0.0,
            };
            let sketched = crate::sketch::stroke_polylines(
                gray,
                w,
                h,
                scale,
                opts.threshold,
                opts.invert,
                opts.hatch_levels,
                opts.stroke_len_mm,
                opts.stroke_follow,
                opts.sketch_jitter,
                opts.sketch_pen,
                opts.order_mode,
                opts.block_px,
                opts.hatch_angle + base_deg,
                opts.edge_threshold,
                &opts.pens[..usize::from(opts.pen_count).min(opts.pens.len())],
                opts.multi_pen,
            );
            return ImageGenResult {
                polylines: sketched.polylines,
                pens: sketched.pens,
                w_mm,
                h_mm,
            };
        }
        _ => {}
    }

    // 线框 / 素描 / 交叉排线：每条扫描线上的一段连续墨 → 一条直线段
    let mut out = Vec::new();
    let lines = scan_lines(w, h, step, opts.scan_angle);
    runs_as_polylines(&ink, &lines, w, scale, &mut out);
    if opts.draw_type == 5 {
        // 交叉排线：更暗的区域再叠一层垂直方向的影线。
        let thr2 = (opts.threshold * 0.72 * 255.0).clamp(0.0, 255.0);
        let mut core = vec![false; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let lum = gray.get_pixel(x, y).0[0] as f64;
                let on = lum < thr2;
                core[(y * w + x) as usize] = if opts.invert { !on } else { on };
            }
        }
        let cross = scan_lines(w, h, step, perpendicular_angle(opts.scan_angle));
        runs_as_polylines(&core, &cross, w, scale, &mut out);
    }
    // 顺序模式：默认保持扫描顺序（历史行为）；显式选了才重排
    let out = order_strokes(out, opts.order_mode, false, opts.block_px, w, scale);

    ImageGenResult {
        pens: vec![0; out.len()],
        polylines: out,
        w_mm,
        h_mm,
    }
}

/// 与扫描方向垂直的方向（用于交叉排线）。
fn perpendicular_angle(angle: u8) -> u8 {
    match angle {
        0 => 2,
        1 => 3,
        2 => 0,
        3 => 1,
        _ => 2,
    }
}

/// 把一组扫描线里的连续墨段转成两点折线。
fn runs_as_polylines(
    ink: &[bool],
    lines: &[Vec<(u32, u32)>],
    w: u32,
    scale: f64,
    out: &mut Vec<Polyline>,
) {
    for line in lines {
        for (s, e) in line_runs(line, ink, w) {
            out.push(Polyline::new(vec![
                (line[s].0 as f64 * scale, line[s].1 as f64 * scale),
                (line[e].0 as f64 * scale, line[e].1 as f64 * scale),
            ]));
        }
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

/// 8 邻域方向（左上起顺时针）。
const SKEL_DIRS: [(i32, i32); 8] = [
    (-1, -1),
    (0, -1),
    (1, -1),
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
];

/// Zhang–Suen 细化：把二值墨迹削成单像素骨架。
pub(crate) fn thin_ink(ink: &[bool], w: u32, h: u32) -> Vec<bool> {
    if w < 3 || h < 3 {
        return ink.to_vec();
    }
    let (wi, hi) = (w as i32, h as i32);
    let (pw, ph) = (w + 2, h + 2);
    let idx = |x: i32, y: i32| (y as u32 * pw + x as u32) as usize;
    let mut im = vec![0u8; (pw * ph) as usize];
    for y in 0..hi {
        for x in 0..wi {
            if ink[(y as u32 * w + x as u32) as usize] {
                im[idx(x + 1, y + 1)] = 1;
            }
        }
    }

    let mut changed = true;
    let mut rounds = 0;
    while changed && rounds < 256 {
        changed = false;
        rounds += 1;
        for pass in 0..2 {
            let mut del = Vec::new();
            for y in 1..ph as i32 - 1 {
                for x in 1..pw as i32 - 1 {
                    if im[idx(x, y)] == 0 {
                        continue;
                    }
                    let p2 = im[idx(x, y - 1)];
                    let p3 = im[idx(x + 1, y - 1)];
                    let p4 = im[idx(x + 1, y)];
                    let p5 = im[idx(x + 1, y + 1)];
                    let p6 = im[idx(x, y + 1)];
                    let p7 = im[idx(x - 1, y + 1)];
                    let p8 = im[idx(x - 1, y)];
                    let p9 = im[idx(x - 1, y - 1)];
                    let b = p2 + p3 + p4 + p5 + p6 + p7 + p8 + p9;
                    if !(2..=6).contains(&b) {
                        continue;
                    }
                    let seq = [p2, p3, p4, p5, p6, p7, p8, p9, p2];
                    let transitions = seq
                        .windows(2)
                        .filter(|pair| pair[0] == 0 && pair[1] == 1)
                        .count();
                    if transitions != 1 {
                        continue;
                    }
                    let remove = if pass == 0 {
                        p2 * p4 * p6 == 0 && p4 * p6 * p8 == 0
                    } else {
                        p2 * p4 * p8 == 0 && p2 * p6 * p8 == 0
                    };
                    if remove {
                        del.push(idx(x, y));
                    }
                }
            }
            for i in del {
                if im[i] == 1 {
                    im[i] = 0;
                    changed = true;
                }
            }
        }
    }

    let mut out = vec![false; (w * h) as usize];
    for y in 0..hi {
        for x in 0..wi {
            out[(y as u32 * w + x as u32) as usize] = im[idx(x + 1, y + 1)] != 0;
        }
    }
    out
}

/// 骨架图中 (x,y) 的 8 邻域墨像素数量。
pub(crate) fn skel_degree(skel: &[bool], w: u32, h: u32, x: i32, y: i32) -> u8 {
    skel_neighbors(skel, w, h, x, y).len() as u8
}

/// 骨架追踪用的邻居：8 邻域，但去掉「有正交捷径时」的斜向捷径。
///
/// 数字骨架在平缓曲线上常出现「水平一段 + 斜上一步」的结构，若把斜向也算一条边，
/// 每个像素会凭空多出 2~3 条三角形边，追踪时碎成大量两点线段。规则是：
/// 只有当斜向的两个正交中间像素都**不是**骨架时，才认为这条斜边有效。
fn skel_neighbors(skel: &[bool], w: u32, h: u32, x: i32, y: i32) -> Vec<(i32, i32)> {
    let at = |px: i32, py: i32| {
        px >= 0 && py >= 0 && px < w as i32 && py < h as i32 && skel[(py as u32 * w + px as u32) as usize]
    };
    let mut out = Vec::with_capacity(8);
    for (dx, dy) in SKEL_DIRS {
        let nx = x + dx;
        let ny = y + dy;
        if !at(nx, ny) {
            continue;
        }
        if dx != 0 && dy != 0 && (at(x + dx, y) || at(x, y + dy)) {
            continue;
        }
        out.push((nx, ny));
    }
    out
}

/// 从 `start → first` 沿骨架走到端点/交叉点（只把度为 2 的链上像素标记为已访问）。
fn skel_walk(
    start: (i32, i32),
    first: (i32, i32),
    skel: &[bool],
    w: u32,
    h: u32,
    visited: &mut [bool],
) -> Vec<(i32, i32)> {
    let mut path = vec![start, first];
    let (mut px, mut py) = start;
    let (mut cx, mut cy) = first;
    loop {
        if skel_degree(skel, w, h, cx, cy) != 2 {
            break;
        }
        let mut step = None;
        for (nx, ny) in skel_neighbors(skel, w, h, cx, cy) {
            if (nx, ny) == (px, py) {
                continue;
            }
            let ni = (ny as u32 * w + nx as u32) as usize;
            if visited[ni] {
                continue;
            }
            step = Some((nx, ny));
            break;
        }
        let Some((nx, ny)) = step else { break };
        if (nx, ny) == start {
            path.push(start);
            break;
        }
        let ni = (ny as u32 * w + nx as u32) as usize;
        // 交叉点/端点不作为本段的内部点，留给后续分支继续生长。
        if skel_degree(skel, w, h, nx, ny) != 2 {
            path.push((nx, ny));
            break;
        }
        visited[ni] = true;
        path.push((nx, ny));
        px = cx;
        py = cy;
        cx = nx;
        cy = ny;
    }
    path
}

/// 骨架 → 折线（相邻像素点做 RDP 简化，减少 G-code 点数）。
fn trace_skeleton(
    skel: &[bool],
    w: u32,
    h: u32,
    scale: f64,
    order_mode: u8,
    block_px: u32,
) -> Vec<Polyline> {
    let mut out = Vec::new();
    for path in skeleton_paths(skel, w, h) {
        let pts: Vec<(f64, f64)> = path.iter().map(|&(x, y)| (x as f64, y as f64)).collect();
        let pts = simplify_path(&pts, 0.6);
        if pts.len() < 2 {
            continue;
        }
        out.push(Polyline::new(
            pts.iter().map(|&(x, y)| (x * scale, y * scale)).collect(),
        ));
    }
    order_strokes(out, order_mode, false, block_px, w, scale)
}

/// 提取骨架的原始像素路径（未简化、未缩放）。
///
/// 从端点 / 交叉点出发逐条分支生长，最后处理纯闭环；返回的每条路径都是
/// 一笔连续像素。中心线描线与「素描原稿」共用这一份追踪逻辑。
pub(crate) fn skeleton_paths(skel: &[bool], w: u32, h: u32) -> Vec<Vec<(i32, i32)>> {
    let (wi, hi) = (w as i32, h as i32);
    let n = (w * h) as usize;
    let mut visited = vec![false; n];
    let mut raw: Vec<Vec<(i32, i32)>> = Vec::new();

    // ① 从端点 / 交叉点出发，逐条分支生长。
    for y in 0..hi {
        for x in 0..wi {
            let i = (y as u32 * w + x as u32) as usize;
            if !skel[i] || visited[i] || skel_degree(skel, w, h, x, y) == 2 {
                continue;
            }
            for (nx, ny) in skel_neighbors(skel, w, h, x, y) {
                let ni = (ny as u32 * w + nx as u32) as usize;
                if visited[ni] {
                    continue;
                }
                let path = skel_walk((x, y), (nx, ny), skel, w, h, &mut visited);
                if path.len() >= 2 {
                    raw.push(path);
                }
            }
            visited[i] = true;
        }
    }

    // ② 纯闭环（全部度为 2）：任选一个未访问点绕一圈。
    for y in 0..hi {
        for x in 0..wi {
            let i = (y as u32 * w + x as u32) as usize;
            if !skel[i] || visited[i] {
                continue;
            }
            for (nx, ny) in skel_neighbors(skel, w, h, x, y) {
                let ni = (ny as u32 * w + nx as u32) as usize;
                if visited[ni] {
                    continue;
                }
                let path = skel_walk((x, y), (nx, ny), skel, w, h, &mut visited);
                if path.len() >= 2 {
                    raw.push(path);
                }
                break;
            }
            visited[i] = true;
        }
    }
    raw
}

/// 8 邻域距离变换：每个像素到最近「命中像素」的距离（chamfer 3/4 近似）。
///
/// 用于量化「原稿的墨有多少被落笔覆盖」：`hit` 是落笔覆盖图，
/// 返回值在墨像素上就是它离最近笔迹有多远。目前由素描模式的视觉验收工具使用。
#[allow(dead_code)]
pub(crate) fn chamfer_distance(hit: &[bool], w: u32, h: u32) -> Vec<f32> {
    let n = (w * h) as usize;
    let mut d = vec![0f32; n];
    for (i, &on) in hit.iter().enumerate() {
        d[i] = if on { 0.0 } else { 1e6 };
    }
    let (wi, hi) = (w as i32, h as i32);
    let diag = std::f32::consts::SQRT_2;
    for y in 0..hi {
        for x in 0..wi {
            let i = (y as u32 * w + x as u32) as usize;
            if d[i] <= 0.0 {
                continue;
            }
            let mut best = d[i];
            if x > 0 {
                best = best.min(d[i - 1] + 1.0);
            }
            if y > 0 {
                let up = i - w as usize;
                if x > 0 {
                    best = best.min(d[up - 1] + diag);
                }
                best = best.min(d[up] + 1.0);
                if x + 1 < wi {
                    best = best.min(d[up + 1] + diag);
                }
            }
            d[i] = best;
        }
    }
    for y in (0..hi).rev() {
        for x in (0..wi).rev() {
            let i = (y as u32 * w + x as u32) as usize;
            if d[i] <= 0.0 {
                continue;
            }
            let mut best = d[i];
            if x + 1 < wi {
                best = best.min(d[i + 1] + 1.0);
            }
            if y + 1 < hi {
                let dn = i + w as usize;
                if x > 0 {
                    best = best.min(d[dn - 1] + diag);
                }
                best = best.min(d[dn] + 1.0);
                if x + 1 < wi {
                    best = best.min(d[dn + 1] + diag);
                }
            }
            d[i] = best;
        }
    }
    d
}

/// Ramer–Douglas–Peucker 折线简化（迭代实现，避免深递归）。
pub(crate) fn simplify_path(pts: &[(f64, f64)], epsilon: f64) -> Vec<(f64, f64)> {
    if pts.len() <= 2 {
        return pts.to_vec();
    }
    let mut keep = vec![false; pts.len()];
    keep[0] = true;
    keep[pts.len() - 1] = true;
    let mut stack = vec![(0usize, pts.len() - 1)];
    while let Some((a, b)) = stack.pop() {
        if b <= a + 1 {
            continue;
        }
        let (ax, ay) = pts[a];
        let (bx, by) = pts[b];
        let (dx, dy) = (bx - ax, by - ay);
        let len = (dx * dx + dy * dy).sqrt();
        let mut best = 0.0;
        let mut best_i = None;
        for (i, &(px, py)) in pts.iter().enumerate().take(b).skip(a + 1) {
            let d = if len < 1e-9 {
                ((px - ax).powi(2) + (py - ay).powi(2)).sqrt()
            } else {
                ((dx * (ay - py) - (ax - px) * dy) / len).abs()
            };
            if d > best {
                best = d;
                best_i = Some(i);
            }
        }
        if best > epsilon {
            if let Some(i) = best_i {
                keep[i] = true;
                stack.push((a, i));
                stack.push((i, b));
            }
        }
    }
    pts.iter()
        .enumerate()
        .filter(|(i, _)| keep[*i])
        .map(|(_, &p)| p)
        .collect()
}

/// 二值墨迹 → 等值轮廓线（marching squares），每条轮廓尽量串成一条折线。
fn contour_polylines(
    ink: &[bool],
    w: u32,
    h: u32,
    scale: f64,
    order_mode: u8,
    block_px: u32,
) -> Vec<Polyline> {
    let (wi, hi) = (w as i32, h as i32);
    let get = |x: i32, y: i32| {
        x >= 0 && y >= 0 && x < wi && y < hi && ink[(y as u32 * w + x as u32) as usize]
    };

    // 节点坐标用「像素坐标 × 2」的整数表示：交点只会在整数或半像素上。
    let mut segs: Vec<((i32, i32), (i32, i32))> = Vec::new();
    for cy in -1..hi {
        for cx in -1..wi {
            let p0 = get(cx, cy);
            let p1 = get(cx + 1, cy);
            let p2 = get(cx + 1, cy + 1);
            let p3 = get(cx, cy + 1);
            let case = (p0 as u8) | ((p1 as u8) << 1) | ((p2 as u8) << 2) | ((p3 as u8) << 3);
            if case == 0 || case == 15 {
                continue;
            }
            let t = (2 * cx + 1, 2 * cy);
            let r = (2 * cx + 2, 2 * cy + 1);
            let b = (2 * cx + 1, 2 * cy + 2);
            let l = (2 * cx, 2 * cy + 1);
            match case {
                1 => segs.push((l, t)),
                2 => segs.push((t, r)),
                3 => segs.push((l, r)),
                4 => segs.push((r, b)),
                5 => {
                    segs.push((l, t));
                    segs.push((r, b));
                }
                6 => segs.push((t, b)),
                7 => segs.push((l, b)),
                8 => segs.push((l, b)),
                9 => segs.push((t, b)),
                10 => {
                    segs.push((t, r));
                    segs.push((b, l));
                }
                11 => segs.push((r, b)),
                12 => segs.push((l, r)),
                13 => segs.push((t, r)),
                14 => segs.push((l, t)),
                _ => {}
            }
        }
    }
    if segs.is_empty() {
        return Vec::new();
    }

    let mut adj: std::collections::HashMap<(i32, i32), Vec<usize>> = Default::default();
    for (i, &(a, b)) in segs.iter().enumerate() {
        adj.entry(a).or_default().push(i);
        adj.entry(b).or_default().push(i);
    }
    let degree = |p: (i32, i32)| adj.get(&p).map_or(0, |v| v.len());
    // 开放轮廓优先（从非度为 2 的节点开始），闭环随后随便挑一条边。
    let mut order: Vec<usize> = (0..segs.len()).collect();
    order.sort_by_key(|&i| {
        let (a, b) = segs[i];
        (degree(a) == 2 && degree(b) == 2) as u8
    });

    let mut used = vec![false; segs.len()];
    let mut chains: Vec<Vec<(i32, i32)>> = Vec::new();
    for i in order {
        if used[i] {
            continue;
        }
        used[i] = true;
        let (a, b) = segs[i];
        let start = a;
        let mut end = b;
        let mut chain = vec![a, b];
        loop {
            let next = adj
                .get(&end)
                .and_then(|v| v.iter().copied().find(|&j| !used[j]));
            let Some(j) = next else { break };
            used[j] = true;
            let (c, d) = segs[j];
            let far = if c == end { d } else { c };
            chain.push(far);
            end = far;
            if end == start {
                break;
            }
        }
        chains.push(chain);
    }

    let mut out = Vec::new();
    for chain in chains {
        let pts: Vec<(f64, f64)> = chain
            .iter()
            .map(|&(x, y)| {
                (
                    (x as f64 * 0.5).clamp(0.0, w as f64),
                    (y as f64 * 0.5).clamp(0.0, h as f64),
                )
            })
            .collect();
        let pts = simplify_path(&pts, 0.5);
        if pts.len() >= 2 {
            out.push(Polyline::new(
                pts.iter().map(|&(x, y)| (x * scale, y * scale)).collect(),
            ));
        }
    }
    order_strokes(out, order_mode, false, block_px, w, scale)
}

/// 网点（半调）：按单元格平均灰度决定网点半径，越暗网点越大。
fn halftone_polylines(
    gray: &image::GrayImage,
    w: u32,
    h: u32,
    scale: f64,
    step_px: u32,
    threshold: f64,
    invert: bool,
    order_mode: u8,
    block_px: u32,
) -> Vec<Polyline> {
    let mut cell = step_px.max(2);
    // 上限 8000 个网点：再密也只是徒增 G-code 行数，机器画不出更多细节。
    while ((w + cell - 1) / cell) as u64 * ((h + cell - 1) / cell) as u64 > 8_000 {
        cell += 1;
        if cell > w.max(h) {
            break;
        }
    }
    let cutoff = (1.0 - threshold).clamp(0.0, 0.99);
    let mut out = Vec::new();
    let mut y0 = 0;
    while y0 < h {
        let y1 = (y0 + cell).min(h);
        let mut x0 = 0;
        while x0 < w {
            let x1 = (x0 + cell).min(w);
            let mut sum = 0u64;
            let mut cnt = 0u64;
            for y in y0..y1 {
                for x in x0..x1 {
                    sum += gray.get_pixel(x, y).0[0] as u64;
                    cnt += 1;
                }
            }
            let mean = sum as f64 / cnt.max(1) as f64;
            let mut dark = 1.0 - mean / 255.0;
            if invert {
                dark = 1.0 - dark;
            }
            if dark > cutoff {
                let t = ((dark - cutoff) / (1.0 - cutoff).max(1e-6)).clamp(0.0, 1.0);
                let radius = 0.45 * (x1 - x0).min(y1 - y0) as f64 * t.sqrt();
                if radius >= 0.2 {
                    let cx = (x0 as f64 + x1 as f64) * 0.5;
                    let cy = (y0 as f64 + y1 as f64) * 0.5;
                    out.push(Polyline::new(vec![
                        ((cx - radius) * scale, cy * scale),
                        (cx * scale, (cy - radius) * scale),
                        ((cx + radius) * scale, cy * scale),
                        (cx * scale, (cy + radius) * scale),
                        ((cx - radius) * scale, cy * scale),
                    ]));
                }
            }
            x0 = x1;
        }
        y0 = y1;
    }
    order_strokes(out, order_mode, false, block_px, w, scale)
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
