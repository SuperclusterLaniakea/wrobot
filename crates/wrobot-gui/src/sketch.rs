//! 素描笔触（模式 10）：以**交叉排线为底**的分层排线，再把原稿的细线单独描一遍。
//!
//! 交叉排线（模式 5）之所以干净，是因为它是一族**严格平行、间距固定**的直线；
//! 5 倍放大后依然能读出原稿的细节。素描笔触要在此基础上补两件事：
//! **不同深浅的表达**（多支铅笔分调子带）和**更细的线**（原稿真实笔触）。
//! 所以这里不再让每个像素各自决定方向与长短——那正是「排线乱、碎成短杠」的来源。
//!
//! # 流水线
//!
//! 1. **调子场**（`Field::build` + `hatch_tone`）：按绝对墨量归一化（纸白 = 图上
//!    真白，不是本图最深的一档），得到 `tone`；`bg` 标记纸面/背景，不参与落笔。
//! 2. **块面调子 → 目标墨量** `target`：调子在 2.2mm 尺度上平均成「这一片该压多黑」，
//!    目标值上限 0.92。1~2px 的暗线（发丝、睫毛、原稿排线）在这一步被平均掉，
//!    它们是细节层的活，不该把整片头发拖成 8B。
//! 3. **细节层**（`source_detail_strokes`）：从墨脊提取原稿真实画出来的线，按原稿
//!    墨量分到 HB/2B/4B 三档笔；它先落纸，其墨量按调子比例从 `target` 里扣掉，
//!    排线只补「细节还不够黑」的部分。
//! 4. **分层排线**：每层 =（笔, 方向），方向只有基准角与基准角+90° 两族；
//!    行距 = 笔宽 / 覆盖率上限，行内按「还差多少墨 / 这一层能补多少」逐行抖动落笔
//!    （`scan_layer_runs`），第二方向的密度由「并集 = 目标覆盖率」反解。层与层之间
//!    是同一族平行线的加密与加深，越暗叠的层越多、笔越深（HB → 2B → 4B → 8B）。
//!
//! # 三条写死的物理约束
//!
//! - **投影笔只有一个墨色**：深浅靠疏密与换笔，不靠同一轨迹反复摩擦（那只会填死纸缝）。
//! - **方向整体给定**：任何「按局部走向逐笔定方向」的做法在放大后都会读出杂乱。
//! - **比笔细得多的线不靠排线描**：原稿的 1~2px 细线交给细节层，排线只负责块面明暗。
//!
//! 所有几何都在像素坐标里还原，最后统一乘 `scale` 变成毫米。
//!
//! 文件里仍保留一部分逐笔追踪/调子分格的旧工具（`trace_strokes`、`tone_map`、
//! `field_ticks` 等）：它们只被 `main.rs` 里 `#[cfg(test)]` 的无头调试通路调用，
//! 正式流水线不使用。

use std::cell::RefCell;
use std::collections::HashMap;

thread_local! {
    static LAST_REASON: RefCell<&'static str> = const { RefCell::new("none") };
}

fn set_reason(r: &'static str) {
    LAST_REASON.with(|c| *c.borrow_mut() = r);
}

use wrobot_core::render::Polyline;

use crate::io::simplify_path;

/// 追踪步长（像素）。1px 与细化后的墨脊分辨率同量级。
const STEP_PX: f64 = 1.0;
/// 单步允许的最大转向（弧度）：30° 以内跟得住曲线，超过就是急转角 → 收笔。
const MAX_TURN: f64 = 0.18;
/// 单笔最大带宽（像素）：更宽的暗部不靠多描几根线，而靠同一根线多铺几遍。
const MAX_WIDTH_PX: f64 = 9.0;
/// 跨越小断口的最大距离（像素）。断口里必须有淡墨，纯白纸是留白，不能跨。
const GAP_PX: f64 = 4.0;
/// 短于该长度的笔迹不落笔（毫米）。
const MIN_STROKE_MM: f64 = 1.6;

/// 归一化后的墨浓度图：0 = 纸白，1 = 这张纸能压出的最深墨。
///
/// 追踪期只读墨浓度和「是否已消费」；纸白判定要用原始亮度，
/// 所以 `lum` / `blank` 单独保留一份。
struct Ink {
    /// 已消费的像素，防止同一条墨脊被反复追踪。
    done: Vec<bool>,
    /// 墨浓度（0..1）。
    tone: Vec<f32>,
    w: u32,
    h: u32,
    /// 判为「墨」的门槛。
    thr: f32,
    /// 弱墨门槛：断口桥接与收笔用。
    weak: f32,
    /// 原始亮度（0..255），只用于 `is_blank`。
    lum: Vec<u8>,
    /// 本图的纸白亮度。
    blank: f32,
    invert: bool,
}

impl Ink {
    #[inline]
    fn idx(&self, x: u32, y: u32) -> usize {
        (y * self.w + x) as usize
    }

    /// 这个点是不是纯纸（留白）。
    fn is_blank(&self, x: f64, y: f64) -> bool {
        let xi = x.round() as i32;
        let yi = y.round() as i32;
        if xi < 0 || yi < 0 || xi >= self.w as i32 || yi >= self.h as i32 {
            return true;
        }
        let v = self.lum[self.idx(xi as u32, yi as u32)] as f32;
        if self.invert {
            v <= self.blank + 6.0
        } else {
            v >= self.blank - 6.0
        }
    }

    /// 双线性采样墨浓度。
    #[inline]
    fn get(&self, x: f64, y: f64) -> f32 {
        let w = self.w as i32;
        let h = self.h as i32;
        if x < 0.0 || y < 0.0 {
            return 0.0;
        }
        let xi = x as i32;
        let yi = y as i32;
        if xi >= w || yi >= h {
            return 0.0;
        }
        let x0 = if xi + 1 < w { xi } else { w - 1 };
        let y0 = if yi + 1 < h { yi } else { h - 1 };
        let fx = (x - x0 as f64) as f32;
        let fy = (y - y0 as f64) as f32;
        let i = self.idx(x0 as u32, y0 as u32);
        let x1 = (x0 + 1).min(w - 1) as u32;
        let y1 = (y0 + 1).min(h - 1) as u32;
        let x0 = x0 as u32;
        let y0 = y0 as u32;
        let a = self.tone[i] * (1.0 - fx) + self.tone[self.idx(x1, y0)] * fx;
        let b = self.tone[self.idx(x0, y1)] * (1.0 - fx) + self.tone[self.idx(x1, y1)] * fx;
        a * (1.0 - fy) + b * fy
    }
}

/// 一次追踪的结果（像素坐标）。
struct Trace {
    pts: Vec<(f64, f64)>,
    widths: Vec<f32>,
    tones: Vec<f32>,
}

/// 一条已还原的笔迹。
struct Stroke {
    pts: Vec<(f64, f64)>,
    loose: [bool; 2],
    len_mm: f64,
    mean_dark: f64,
    mean_width_mm: f64,
    theta: f64,
    /// 全画面里的「笔重」分位（0..1）：最轻的一批是起稿线，最重的是暗部强调。
    /// 用分位而不是绝对明暗，是因为不同原稿的曝光差异很大，
    /// 分位能保证任何稿子都能分出三层。
    rank: f64,
    /// 是否「结构线」：比所在区域明显更黑的线（轮廓、结构转折）。
    ///
    /// 素描里这类线要压得比排线重：同样是投影笔，靠落笔宽度区分——
    /// 结构线横着铺满一小圈，排线只是一根。不区分的话整张图所有线一样重，
    /// 看哪儿都是平的，这也是「认不出明暗」的一部分原因。
    structural: bool,
    /// 明确指定铺几遍（0 = 让落笔按带宽/墨色自己推）。
    ///
    /// 调子排线必须写 1：明暗完全由「线有多密」决定，多铺一遍就等于把
    /// 这片区域的墨量翻倍，调子立刻失真。
    passes: u32,
}

/// 计划里的一条笔迹：索引 + 是否反向绘制。
#[derive(Clone, Copy)]
struct Planned {
    idx: usize,
    rev: bool,
}

/// 主入口：把一张铅笔稿逐笔还原成「像手画出来」的笔迹。
/// 追踪出来的一笔（调试与落笔共用）。
pub(crate) struct Traced {
    pub pts: Vec<(f64, f64)>,
    pub width_px: f64,
    pub tone: f64,
    pub len_mm: f64,
}

/// 逐笔追踪：这是整个素描原稿的核心，落笔之前的所有几何都在这里定下来。
pub(crate) fn trace_strokes(
    gray: &image::GrayImage,
    w: u32,
    h: u32,
    scale: f64,
    threshold: f64,
    invert: bool,
) -> Vec<Traced> {
    let mut ink = build_ink(gray, w, h, invert, threshold);
    let cand = seeds(&ink);
    let mut out: Vec<Traced> = Vec::new();
    if cand.is_empty() {
        return out;
    }
    let min_px = (MIN_STROKE_MM / scale.max(1e-9)).max(2.0);
    let max_steps = ((w.max(h)) as f64 * 2.0) as usize;
    for (_, si) in cand {
        let idx = si as usize;
        if ink.done[idx] {
            continue;
        }
        let px = (si % w) as f64;
        let py = (si / w) as f64;
        let dir = seed_dir(&ink, px, py);
        let fwd = trace(&ink, (px, py), dir, max_steps);
        let bwd = trace(&ink, (px, py), (-dir.0, -dir.1), max_steps);
        let mut pts: Vec<(f64, f64)> = Vec::with_capacity(fwd.pts.len() + bwd.pts.len());
        let mut ws: Vec<f32> = Vec::with_capacity(fwd.pts.len() + bwd.pts.len());
        let mut ts: Vec<f32> = Vec::with_capacity(fwd.pts.len() + bwd.pts.len());
        for k in (1..bwd.pts.len()).rev() {
            pts.push(bwd.pts[k]);
            ws.push(bwd.widths[k]);
            ts.push(bwd.tones[k]);
        }
        pts.push(bwd.pts[0]);
        ws.push(bwd.widths[0]);
        ts.push(bwd.tones[0]);
        for k in 1..fwd.pts.len() {
            pts.push(fwd.pts[k]);
            ws.push(fwd.widths[k]);
            ts.push(fwd.tones[k]);
        }
        if pts.len() < 3 {
            continue;
        }
        trim_ends(&ink, &mut pts, &mut ws, &mut ts);
        if pts.len() < 3 {
            continue;
        }
        // 1px 采样下的墨脊位置天然有 ±0.5px 量化抖动，两遍 1-2-1 平滑
        // 抹掉 2~3px 波长的小摆；真正的弯（罐口、椭圆）曲率半径远大于此，不受影响。
        for _ in 0..2 {
            smooth_in_place(&mut pts);
        }
        let len_px = chain_len(&pts);
        if len_px < min_px {
            continue;
        }
        let tone = quantile(&ts, 0.5) as f64;
        if tone < ink.thr as f64 * 0.98 {
            continue;
        }
        let width = quantile(&ws, 0.5).max(0.8) as f64;
        // 大半都落在已画过的墨上 → 这是某条笔迹的残边，不是新的一笔。
        // 判定要按墨带半宽做邻域，而不是看采样点那一个像素：追踪本身有 ±1px 的
        // 横向漂移，一条 3px 线会被同一个位置重描三四遍，画出几根平行重影线。
        if covered_frac(&ink, &pts, width) > 0.55 {
            continue;
        }
        consume(&mut ink, &pts, &ws);
        out.push(Traced {
            pts,
            width_px: width,
            tone,
            len_mm: len_px * scale,
        });
    }
    if std::env::var("SKETCH_DEBUG").is_ok() {
        let mut ls: Vec<f64> = out.iter().map(|t| t.len_mm).collect();
        ls.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let nt = out.len().max(1);
        eprintln!(
            "追踪 {} 条，阈值 {:.3}，长度中位 {:.1}mm 最长 {:.1}mm，>20mm {} 条",
            out.len(),
            ink.thr,
            ls[ls.len() / 2],
            ls[ls.len() - 1],
            ls.iter().filter(|&&v| v > 20.0).count(),
        );
        let _ = nt;
    }
    out
}


/// 一支参与分层的笔：线宽与满覆盖时能压出的最深色。
#[derive(Clone, Copy)]
pub struct HatchPen {
    pub width_mm: f64,
    pub darkness: f32,
}

/// 排线输出：笔迹 + 每根笔迹属于哪支笔（按输入的笔组下标）。
pub(crate) struct HatchOut {
    pub polylines: Vec<Polyline>,
    pub pens: Vec<u8>,
}

/// 素描笔触共用的调子场。
struct HatchTone {
    /// 对比度校正后的调子 0..1（0 = 纸白，1 = 全画最深）。
    tone: Vec<f32>,
    /// 0.4mm 调子 − 2.2mm 调子：局部比周围暗/亮多少，轮廓层与高光让白都用它。
    detail: Vec<f32>,
    /// 背景 / 纸白掩码。
    bg: Vec<bool>,
}

/// 把原稿归一化成「排线用」的调子场。
///
/// 两个尺度是有分工的：
/// - 1.3mm 大调子决定「这一块该排多密」；
/// - 0.4mm 细节减去大调子，得到发丝高光（负）与眼线、唇缝（正），
///   前者让白、后者加黑，模拟「先铺大调子、再抠亮部与暗部」。
fn hatch_tone(field: &Field, gray: &image::GrayImage, invert: bool, threshold: f64) -> HatchTone {
    let mut norm = vec![0f32; field.w * field.h];
    // 调子要按**绝对墨量**归一化：纸白 = 255，不是「本图最深的 6%」。
    // 否则同一片 0.18 的浅灰，在头发很黑（span 大）的稿子上只剩 0.12，在
    // 没有深调的稿子上却是 0.3——排线的疏密跟着整张图的对比度漂移。
    //
    // 纸白本身有两类：扫描的灰纸（纸色 200~230，整图没有真白）要按纸色
    // 算，否则满背景都被当成墨；而这批手绘/渲染稿的纸是白的，只是背景
    // 铺了一层灰调——图里有成片 >246 的真白就是判据，此时纸白取 255。
    let n_pix = field.w * field.h;
    let mut whites = 0usize;
    if !invert {
        for p in gray.pixels() {
            if p.0[0] >= 246 {
                whites += 1;
            }
        }
    }
    let white = if whites * 50 >= n_pix {
        255.0
    } else {
        field.paper.max(1.0)
    };
    for (i, p) in gray.pixels().enumerate() {
        let v = if invert {
            255.0 - p.0[0] as f32
        } else {
            p.0[0] as f32
        };
        norm[i] = ((white - v) / white).max(0.0);
    }
    let px_mm = field.px_mm.max(1e-9);
    // 1.3mm：保得住头发里「哪几缕是亮的」，又能把单根发丝平均成一块调子。
    let r_tone = ((1.3 / px_mm).round() as usize).clamp(2, 60);
    let tone_hatch = box_blur(&norm, field.w, field.h, r_tone);
    let r_fine = ((0.4 / px_mm).round() as usize).clamp(1, 16);
    let fine = box_blur(&norm, field.w, field.h, r_fine);
    // 0.35mm 刚好跨过单根笔触、保住成条的高光带。
    let r_hi = ((0.35 / px_mm).round() as usize).clamp(1, 10);
    let hi = box_blur(&norm, field.w, field.h, r_hi);
    let mut detail = vec![0f32; field.w * field.h];
    for i in 0..detail.len() {
        // 0.4mm 调子 − 1.3mm 调子：两把尺子必须都换成绝对墨量，
        // 混用 field.tone（旧的 span 归一化）会让「局部比周围暗多少」差一个比例。
        detail[i] = fine[i] - tone_hatch[i];
    }
    let _ = threshold;
    // 漫填的「相邻差」用 0.4mm 平滑图算：原始像素的纸纹起伏超过 2% 阈值，
    // 会把背景切成孤岛，细节提取随后把纸纹描得满纸都是。但「是否背景」
    // 仍看原始调子，且要求它明显高于纸白——1.3mm 平滑会把脸的浅调子
    // 与背景连成缓坡，整张脸被漫填成背景（实测覆盖 76.6%，脸全白）。
    // 纹理屏障：脸的高光比背景还亮，单靠调子高低根本拦不住，漫填会穿过
    // 高光把额头、脸颊一起吞掉（输出脸上出现一大块死白）。纸是平的，
    // 人脸再亮也有细排线和皮肤纹理——0.8mm 尺度的高频能量能分开两者。
    let r_tex = ((0.8 / px_mm).round() as usize).clamp(1, 20);
    let hf: Vec<f32> = (0..field.w * field.h)
        .map(|i| (norm[i] - fine[i]).abs())
        .collect();
    let hf = box_blur(&hf, field.w, field.h, r_tex);
    let bg = background_mask(&norm, &fine, &hf, field.w, field.h);
    let mut tone = vec![0f32; field.w * field.h];
    for (i, ((&t0, &is_bg), &hf)) in tone_hatch
        .iter()
        .zip(bg.iter())
        .zip(hi.iter())
        .enumerate()
    {
        if is_bg {
            continue;
        }
        let mut t = t0;
        // 高光比 1.3mm 均值亮 8% 以上就往下抠，最多抠掉均值的 78%。
        // 旧的 12%/0.55 只保得住大片高光：瞳孔里的 catchlight、发丝上的
        // 亮边只有 2~4px，被 1.3mm 均值抹平后糊成一块灰，眼睛因此没有神。
        // 0.35mm 图刚好跨过单根发丝，用它在深调里把白抠回来。
        // 只在「这块真的亮」时抠白（hf < 0.55）。没有这个绝对门槛，
        // 发丛/衣领的明暗交界处（1.3mm 发黑、0.35mm 跨到亮面）也会被
        // 判成高光，勾出一圈白边，脸和头发像贴了纸边。
        // 只在**像素本身确实亮**的地方抠高光。发丛的调子高、0.35mm 均值
        // 也会被深浅发丝拉开，如果只看 `hf < t0`，整片头发都会被当成高光
        // 抠掉，瞳孔和发芯一起变浅（8B 因此一条线都落不下去）。
        // 只在**像素本身确实接近纸白**时才回收高光。旧门槛 norm<0.55 会把
        // 深色五官旁边 0.3~0.5 的浅肤色也当成高光抹掉，于是鼻侧、眼下、
        // 唇边出现一圈只剩纸白的白晕——放大看像贴了纸边。
        // 现在按像素自身的亮度做渐变：norm 0.12 以下全量回收（发丝亮边、
        // 瞳孔 catchlight），0.12~0.30 线性减弱，0.30 以上只留一点点。
        let bright_fac = ((0.30 - norm[i]) / 0.18).clamp(0.0, 1.0);
        if t0 > 0.45 && hf + 0.12 < t0 && hf < 0.55 && bright_fac > 0.0 {
            let k = ((t0 - hf - 0.12) / 0.30).clamp(0.0, 1.0);
            // 亮斑要直接留白，而不是只减三成：高光如果还剩 30% 调子，
            // 5 遍细排线叠上去仍是肉眼可见的灰块（用户说的「高光一片黑」）。
            t *= 1.0 - 0.95 * k * bright_fac;
        }
        // 小块面（瞳孔、唇缝）不再在这里拉深。以前用 0.4mm 细节把「比大调子
        // 暗一截」的像素补回来，本意是保住虹膜/瞳孔；但在浅调纸上，一根
        // 1~2px 的浅排线也满足「比周围暗 0.16」，被补成一块深调，2.2mm 平均后
        // 整片高光反而被排黑——亮部比原稿黑一倍，正是「明暗调子不对」。
        // 这些线现在由细节层逐根去描，块面调子只负责「这一片有多黑」。
        tone[i] = t.clamp(0.0, 1.0);
    }
    HatchTone { tone, detail, bg }
}

/// 调试用：返回背景掩码（true = 背景）。
pub(crate) fn bg_mask_map(
    gray: &image::GrayImage,
    w: u32,
    h: u32,
    scale: f64,
    invert: bool,
) -> Vec<bool> {
    let field = Field::build(gray, w, h, scale, invert, 0.5);
    hatch_tone(&field, gray, invert, 0.72).bg
}




// ───────────────────── 素描笔触：残差驱动的短直线 ─────────────────────
//
// 前面三条路各缺一块：
// - 原稿逐笔：位置最准，但提取不到的浅灰只能留白；
// - 调子排线：明暗对，但一笔几毫米的等距曲线并不像铅笔排线；
// - 分区重绘：深度层对，却仍用「沿方向场走到底」的曲线，暗部容易织成毛线。
//
// 这一条按真实素描的工序来：
// 1. 先量调子 → 每处需要多少墨（0..1）；
// 2. 已画墨量单独记一张图，模糊到「一眼看一小块」的尺度；
// 3. 只在「还差墨」的地方落**短直线段**，线与线的距离由这块还差多少决定——
//    差得多就排得密，差得少就排得疏，于是暗部自动密、亮部自动留白；
// 4. 方向沿局部轮廓（等照度线）或原稿笔触；暗部再叠交叉角，一层层压深；
// 5. 多笔时按每支笔能压出的最深色分带：浅笔先补它那一档，深笔再补剩下的；
// 6. 最后只留少量墨脊/轮廓压形。


/// 素描笔触主入口（融合布线 v2）。
///
/// 和上一版的区别不在「还差多少墨」这个出发点，而在**笔怎么组织**：
/// 上一版每个候选点各自落一根 2~4mm 的线段，再用胶囊占位挡住邻居，
/// 同一遍里的线互不相接、遍与遍又各自换方向，读起来像随机刮痕。
/// 现在每一笔先从一个种子出发，**沿走向场连续走出一小段流线**，再按
/// 「短直线段」切成 1.6~5.5mm 的弦；笔与笔之间仍按局部缺口定间距。
/// 线成组、同向、连续地铺上去，暗部才像一层层排线，而不是噪点。
///
/// 另加一层**原稿细节**：从墨脊提取原稿真实笔触（发丝、眼线、衣褶），
/// 用细笔画在最后。它补「精细程度」，合成排线只负责明暗。
#[allow(clippy::too_many_arguments)]
/// 一层排线的扫描：返回一组「行内连续段」的两个端点（像素坐标）。
///
/// 交叉排线之所以干净，是因为它是一族**严格平行、间距固定**的直线；调子如果
/// 交给「每个像素各自决定要不要落笔」，线会碎、方向会乱，放大 5 倍就是毛边。
/// 这里把分工钉死：
/// - 方向和间距由调用方给定，整层恒定（一层一个角度、一个行距）；
/// - 一行画不画、画多长，由 `fmap`（还差多少墨 / 这一层能补多少）决定。
///
/// 每个行取一个黄金分割低差异阈值 `d`：`f > d` 才落笔。密度随 f 连续变化，
/// 但落笔点永远是一整段横贯区域的平行线，而不是噪点。两端用 ±`HYST` 滞回，
/// 免得在边界上抖出一串几像素的碎段。
///
/// 逐像素掩膜（例如「深笔只画到原稿真正暗的地方」）由调用方先乘进 `fmap`。
/// 不能等行内平均之后再乘：那样掩膜边缘的抖动会把长线切成台阶状的短杠，
/// 放大看就是一排碎笔。
fn scan_layer_runs(
    fmap: &[f32],
    w: usize,
    h: usize,
    angle: f64,
    pitch: f64,
    phase: f64,
    step: f64,
    min_len: f64,
    smooth_px: f64,
) -> Vec<((f64, f64), (f64, f64))> {
    let mut runs = Vec::new();
    if w < 2 || h < 2 || fmap.len() < w * h {
        return runs;
    }
    let (sn, cs) = angle.sin_cos();
    // u 沿行方向，n 垂直行（行号方向）；两者正交，t/s 都是像素弧长。
    let (ux, uy) = (cs, sn);
    let (nx, ny) = (-sn, cs);
    let (wf, hf) = (w as f64, h as f64);
    let (mut smin, mut smax) = (f64::INFINITY, f64::NEG_INFINITY);
    let (mut tmin, mut tmax) = (f64::INFINITY, f64::NEG_INFINITY);
    for &(x, y) in &[(0.0, 0.0), (wf, 0.0), (0.0, hf), (wf, hf)] {
        let s = x * nx + y * ny;
        let t = x * ux + y * uy;
        smin = smin.min(s);
        smax = smax.max(s);
        tmin = tmin.min(t);
        tmax = tmax.max(t);
    }
    let pitch = pitch.max(1.0);
    let step = step.clamp(0.35, 2.0);
    let nrows = (((smax - smin) / pitch).floor() as usize) + 1;
    let nsteps = (((tmax - tmin) / step).ceil() as usize) + 2;
    // 每一层错开行相位：否则不同角度的行都从同一个角落起算，交叉点会结块。
    let s0 = smin + pitch * (phase.fract().abs() * 0.97);
    const HYST: f32 = 0.02;
    // 行内先做 1mm 级滑动平均再判「画不画」。
    //
    // 直接用逐像素的 f 卡阈值，原稿调子 1~2mm 的起伏会把每一行切成一截一截的
    // 短杠——放大看还是「碎排线」，不是交叉排线的长直线。沿行方向平均、不跨行，
    // 既能拉直成线，又保留「哪几行该落笔」的横向精度：眉毛是几条暗行，脸颊是
    // 一片浅行，彼此不会糊在一起。细线由细节层负责，不靠排线去描。
    let rs = ((0.5 * smooth_px / step).round() as usize).clamp(1, nsteps.max(1));
    let mut row_f = vec![0f32; nsteps];
    let mut prefix = vec![0f32; nsteps + 1];
    for r in 0..nrows {
        let s = s0 + r as f64 * pitch;
        if s > smax + 0.5 * pitch {
            break;
        }
        // 阈值不能落到 0 附近：一旦 d < HYST，滞回的「继续」条件 `f > d - HYST`
        // 在 f = 0 的纸面上也成立，一根线会从人脸一直划到画布外，正好就是
        // 用户看到的「跨过整张图的长斜线」。最稀的一层也保留约 1/25 的行。
        let d = (((r as f64 + 0.5) * 0.618_033_988_749_894_8 + phase * 7.13).fract() as f32)
            .max(0.04);
        for k in 0..nsteps {
            let t = tmin + k as f64 * step;
            let x = s * nx + t * ux;
            let y = s * ny + t * uy;
            if x < 0.0 || y < 0.0 || x > wf || y > hf {
                row_f[k] = 0.0;
            } else {
                row_f[k] = bil(fmap, w, h, x - 0.5, y - 0.5);
            }
        }
        prefix[0] = 0.0;
        for k in 0..nsteps {
            prefix[k + 1] = prefix[k] + row_f[k];
        }
        // 蛇形走笔：相邻两行反向，出图时笔少空跑，也不容易在同一头堆墨。
        let rev = r % 2 == 1;
        let mut t_start: Option<f64> = None;
        let mut t_last = 0.0f64;
        let mut raw: Vec<(f64, f64)> = Vec::new();
        for k in 0..nsteps {
            let kk = if rev { nsteps - 1 - k } else { k };
            let t = tmin + kk as f64 * step;
            let lo_k = kk.saturating_sub(rs);
            let hi_k = (kk + rs + 1).min(nsteps);
            let f = (prefix[hi_k] - prefix[lo_k]) / (hi_k - lo_k) as f32;
            // 逐行抖动阈值：这一行在「覆盖率要求 > d」的地方落笔。
            // 一行只在平滑后的覆盖率高于自己的抖动值时画，整块浅调里正好
            // 有 d 比例的行被选中，落墨量≈目标。以前用「起笔后只要有活就
            // 一直画」的滞回，行从头发进入脸时不会停，一张图里几乎每行都
            // 会经过某处暗部、被点亮一次，于是浅调区域被所有行刷满——
            // 实测浅格落墨是目标的 2~3 倍，正是用户看到的「浅处发灰、
            // 长线横穿脸」。阈值处的碎断交给后面的断口合并（≤1.5mm）修掉。
            let on = f > d + HYST;
            if on {
                if t_start.is_none() {
                    t_start = Some(t);
                }
                t_last = t;
            } else if let Some(a) = t_start.take() {
                raw.push((a.min(t_last), a.max(t_last)));
            }
        }
        if let Some(a) = t_start.take() {
            raw.push((a.min(t_last), a.max(t_last)));
        }
        // 把同一行里的小断口合回去：放大后 3~5mm 的碎段最像「排线乱」。
        // 只有接近 1mm 的空白才算真正的断开（五官间隙、高光边）。
        raw.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let gap = min_len * 4.0;
        let mut i = 0;
        while i < raw.len() {
            let (lo, mut hi) = raw[i];
            i += 1;
            while i < raw.len() && raw[i].0 - hi <= gap {
                hi = hi.max(raw[i].1);
                i += 1;
            }
            if hi - lo >= min_len {
                runs.push((
                    (s * nx + lo * ux, s * ny + lo * uy),
                    (s * nx + hi * ux, s * ny + hi * uy),
                ));
            }
        }
    }
    runs
}

/// 与本流水线配套的墨量模拟：线宽严格等于笔宽，只做最近像素填充。
///
/// 旧的 `stamp_ink` 带固定 0.9px 软边；0.2mm 笔在 9px/mm 下本体只有 1.8px，
/// 软边把线抹成近 0.5mm，`view` 会高估墨量两三倍——浅调刚排一层就「够了」，
/// 纸上却几乎空白。这里按真实笔宽刷，模糊交给后面的 `view` 平均。
fn stamp_ink_hard(
    ink: &mut [f32],
    w: usize,
    h: usize,
    a: (f64, f64),
    b: (f64, f64),
    half: f64,
    dark: f32,
) {
    if w == 0 || h == 0 {
        return;
    }
    let half = half.max(0.35);
    let pad = half + 1.0;
    let x0 = (a.0.min(b.0) - pad).floor().max(0.0) as usize;
    let y0 = (a.1.min(b.1) - pad).floor().max(0.0) as usize;
    let x1 = ((a.0.max(b.0) + pad).ceil() as isize)
        .min(w as isize - 1)
        .max(0) as usize;
    let y1 = ((a.1.max(b.1) + pad).ceil() as isize)
        .min(h as isize - 1)
        .max(0) as usize;
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let r2 = half * half;
    for yy in y0..=y1 {
        for xx in x0..=x1 {
            let (px, py) = (xx as f64 + 0.5, yy as f64 + 0.5);
            let t = if len2 > 1e-12 {
                (((px - a.0) * dx + (py - a.1) * dy) / len2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let qx = a.0 + dx * t;
            let qy = a.1 + dy * t;
            let d2 = (px - qx) * (px - qx) + (py - qy) * (py - qy);
            if d2 <= r2 {
                let i = yy * w + xx;
                if dark > ink[i] {
                    ink[i] = dark;
                }
            }
        }
    }
}

/// 交叉排线基底的分层排线。
///
/// 上一版（`stroke_polylines_legacy`）的落笔单位是「每根 2~6mm 的短笔触」，
/// 方向来自逐像素方向场再加抖动。放大看有三个硬伤：
/// 1. 目标场被 0.8mm 平均，原稿 1~2px 的排线/发丝在目标里就被抹掉了；
/// 2. 每根笔触各取各的方向，相邻笔触能差几十度，排线是「乱」的；
/// 3. 落笔位置由随机数决定，线距不匀，暗部堆成黑块、亮部留白。
///
/// 这一版把动作换成和交叉排线同一套：**一族平行、等距、横贯区域的直线**。
/// 不同的只是每一层有自己的角度、笔和「补多少墨」的预算：
/// - 浅调只落一层最细的 HB，单方向、很疏；
/// - 中间调叠第二层 2B，方向转 90°；
/// - 暗部再叠 4B/8B 和对角方向，覆盖率接近满格。
///
/// 每一层画完把墨量并集重新平均一次，下一层只看**还差多少**。调子因此是
/// 由「线的疏密 × 笔的深浅」堆出来的，不会像旧版那样靠反复描同一条线变黑。
pub(crate) fn stroke_polylines(
    gray: &image::GrayImage,
    w: u32,
    h: u32,
    scale: f64,
    threshold: f64,
    invert: bool,
    passes: u32,
    stroke_len_mm: f64,
    follow_mode: u8,
    jitter_mm: f64,
    pen_mm: f64,
    order_mode: u8,
    order_block_px: u32,
    base_angle_deg: f64,
    pens: &[HatchPen],
    multi_pen: bool,
) -> HatchOut {
    let mut out = Vec::new();
    let mut pens_out = Vec::new();
    if w == 0 || h == 0 {
        return HatchOut {
            polylines: out,
            pens: pens_out,
        };
    }
    // 方向和长度都不再逐笔决定：方向由层角度给定（这是「不乱」的前提），
    // 排线本来就横贯整块区域，短笔触长度参数在这里没有意义。
    let _ = (follow_mode, stroke_len_mm);
    let pen_mm = pen_mm.clamp(0.15, 1.6);
    let field = Field::build(gray, w, h, scale, invert, pen_mm);
    let ht = hatch_tone(&field, gray, invert, threshold);
    let fw = field.w;
    let fh = field.h;
    let npix = fw * fh;
    let px_mm = field.px_mm.max(1e-9);
    let base = base_angle_deg.to_radians();

    if let Ok(p) = std::env::var("SKETCH_DUMP_TONE") {
        let mut img = image::GrayImage::new(fw as u32, fh as u32);
        for y in 0..fh {
            for x in 0..fw {
                let v = (ht.tone[y * fw + x] * 255.0).clamp(0.0, 255.0) as u8;
                img.put_pixel(x as u32, y as u32, image::Luma([v]));
            }
        }
        let _ = img.save(&p);
        for (fx, fy, label) in [
            (0.70, 0.28, "右发"),
            (0.38, 0.52, "脸颊"),
            (0.48, 0.38, "额头"),
            (0.07, 0.36, "背景左"),
            (0.92, 0.10, "背景右"),
        ] {
            let x = ((fx * fw as f64) as usize).min(fw - 1);
            let y = ((fy * fh as f64) as usize).min(fh - 1);
            eprintln!("  调子场 {label} = {:.3}", ht.tone[y * fw + x]);
        }
    }

    // 纹理密度与邻近墨只用于纸白判定（见下）。
    let r_tex = ((1.5 / px_mm).round() as usize).clamp(2, 24);
    let ridge_cap: Vec<f32> = field.ridge.iter().map(|&r| r.min(0.12)).collect();
    let tex = box_blur(&ridge_cap, fw, fh, r_tex);
    let r_near = ((1.2 / px_mm).round() as usize).clamp(2, 20);
    let near_ink = box_blur(&ridge_cap, fw, fh, r_near);

    // —— 调子分布（调试用）——
    let mut tops: Vec<f32> = (0..npix)
        .filter(|&i| !ht.bg[i] && ht.tone[i] > 0.004)
        .map(|i| ht.tone[i])
        .collect();
    tops.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if std::env::var("SKETCH_DEBUG").is_ok() && !tops.is_empty() {
        let q = |f: f64| tops[((tops.len() - 1) as f64 * f) as usize];
        eprintln!(
            "分层排线：绝对调子 p10 {:.3} p50 {:.3} p90 {:.3} p98 {:.3}",
            q(0.10),
            q(0.50),
            q(0.90),
            q(0.98)
        );
    }

    // —— 块面调子 ——
    // 排线负责的是「这一片该压多黑」，不是「这个像素有多黑」。原稿里的
    // 1~2px 暗线（发丝、睫毛、排线本身）如果直接进调子场，一根头发就会把
    // 周围几毫米拖进 4B/8B，眉毛和头发被涂成黑板；而它们本来是细节层的活。
    // 所以先做 1.35mm 平均，只留下块面明暗；单像素的线交给细节层去描。
    let mut tone_raw = vec![0f32; npix];
    for i in 0..npix {
        if !ht.bg[i] {
            tone_raw[i] = ht.tone[i];
        }
    }
    let r_mass = ((2.20 / px_mm).round() as usize).clamp(1, 30);
    let tone_mass = box_blur(&tone_raw, fw, fh, r_mass);

    // `target` = 这一小块纸面该压到多黑（0 = 纸白，1 = 全黑）。
    // 排线的覆盖率不等于连续调子：同样 0.15 的墨量，一排 0.2mm 的线在纸上
    // 远看确实是一层浅灰，但落笔覆盖与墨深是两回事，覆盖模型本身会低估
    // 叠线后的实际暗度；整体抬一档再交给物理墨量评测校准。
    const TONE_GAIN: f32 = 1.08;
    let mut target = vec![0f32; npix];
    for i in 0..npix {
        if ht.bg[i] {
            continue;
        }
        // 纸白门槛与旧版一致：只挡纸面/背景，不挡脸。
        let paper = tone_mass[i] < 0.015
            && tex[i] < 0.0055
            && near_ink[i] < 0.0025
            && ht.detail[i].abs() < 0.03;
        if paper {
            continue;
        }
        // 绝对调子就是「这一片纸面该有多黑」：不再按本图最深的 p98 拉伸。
        // 拉伸会让浅调人像整张脸被推到 0，同时把头发钉死在 1——对比度是
        // 由原稿决定的，不是由最黑的那个像素决定的。
        let by_tone = (tone_mass[i] * TONE_GAIN).min(0.92);
        let has_ink = tone_mass[i] > 0.05;
        let by_tex = if has_ink {
            (((tex[i] - 0.008) / 0.030).clamp(0.0, 1.0)).powf(0.8) * 0.018
        } else {
            0.0
        };
        target[i] = by_tone.max(by_tex);
    }
    // 密度场只做 0.9mm 平均：块面调子已经 1.35mm 平均过，这里再把边界抹圆，
    // 排线才不会沿着 1~2mm 的调子起伏断开。
    let r_tone = ((1.20 / px_mm).round() as usize).clamp(1, 16);
    target = box_blur(&target, fw, fh, r_tone);
    if let Ok(p) = std::env::var("SKETCH_DUMP_TARGET") {
        let mut img = image::GrayImage::new(fw as u32, fh as u32);
        for y in 0..fh {
            for x in 0..fw {
                let v = (target[y * fw + x] * 255.0).clamp(0.0, 255.0) as u8;
                img.put_pixel(x as u32, y as u32, image::Luma([v]));
            }
        }
        let _ = img.save(&p);
    }
    if let Ok(p) = std::env::var("SKETCH_DUMP_BG") {
        let mut img = image::GrayImage::new(fw as u32, fh as u32);
        for y in 0..fh {
            for x in 0..fw {
                let v = if ht.bg[y * fw + x] { 0u8 } else { 255u8 };
                img.put_pixel(x as u32, y as u32, image::Luma([v]));
            }
        }
        let _ = img.save(&p);
    }

    let m_eff = if multi_pen {
        pens.len().min(4).max(1)
    } else {
        1
    };
    let pen_dark = |p: usize| -> f32 {
        if m_eff > 1 {
            pens[p].darkness.clamp(0.2, 1.0)
        } else {
            0.85
        }
    };
    let pen_width = |p: usize| -> f64 {
        if m_eff > 1 {
            pens[p].width_mm.clamp(0.15, 1.6)
        } else {
            pen_mm
        }
    };

    let only_hatch = std::env::var("SKETCH_ONLY").ok().as_deref() == Some("hatch");
    let only_detail = std::env::var("SKETCH_ONLY").ok().as_deref() == Some("detail");

    // —— 细节层：原稿真实画出来的线（发丝、眼线、衣褶、浅排线）——
    //
    // 排线的行距最细也有 0.3~0.5mm，斜着穿过一根 0.1mm 的发丝只能留下一个小段；
    // 这些线必须单独描一遍，否则 5 倍放大后脸和头发都是「格子」而没有「线」。
    let detail = if only_hatch {
        Vec::new()
    } else {
        let narrow = (0..m_eff)
            .map(pen_width)
            .fold(f64::INFINITY, f64::min)
            .min(pen_mm.max(0.15));
        source_detail_strokes(&field, &ht, threshold, narrow)
    };
    let abs_at = |x: f64, y: f64| -> f32 {
        let xi = (x as usize).min(fw - 1);
        let yi = (y as usize).min(fh - 1);
        let v = gray.get_pixel(xi as u32, yi as u32).0[0] as f32;
        ((field.paper - v) / field.paper.max(1.0)).clamp(0.0, 1.0)
    };
    // 细节是「线」，不是「块」。以前只发最细的两支笔，结果睫毛、眼线、
    // 鼻底这些原稿压得最重的地方，描出来也只有 2B 的灰（0.50），还没旁边
    // 排线深——放大看五官是糊的。现在按原稿墨量分到 HB/2B/4B 三档；
    // 4B 线宽 0.32mm 仍在「线」的范围内，再往上才有黑棍感。
    let pen_for_abs = |a: f32| -> usize {
        if m_eff <= 1 {
            return 0;
        }
        let limit = 2.min(m_eff - 1);
        for i in 0..=limit {
            if pen_dark(i) >= a * 0.85 {
                return i;
            }
        }
        limit
    };
    let mut buckets: Vec<Vec<Stroke>> = (0..m_eff).map(|_| Vec::new()).collect();
    for s in detail {
        let n = s.pts.len().max(1) as f32;
        let need: f32 = s.pts.iter().map(|&(x, y)| abs_at(x, y)).sum::<f32>() / n;
        buckets[pen_for_abs(need)].push(s);
    }

    // 细节层先落纸：排线只补「细节还不够黑」的那部分。
    // 以前细节最后画、排线不知道它的存在，头发里密集的发丝被同一块 8B
    // 排线压在下面——放大看只剩一片带白点的黑，发丝一根也看不见。
    // 这里按 2.2mm 尺度量出细节已经提供的墨量，从目标里减掉；尺度取得和
    // 块面调子一样，才不会有「绕着每根线留一圈白晕」的浮雕感。
    let mut detail_ink = vec![0f32; npix];
    for (p, list) in buckets.iter().enumerate() {
        let (pw, pdark) = (pen_width(p), pen_dark(p));
        let half = (pw * 0.5 / px_mm).max(0.5);
        for s in list {
            for w in s.pts.windows(2) {
                stamp_ink_hard(&mut detail_ink, fw, fh, w[0], w[1], half, pdark);
            }
        }
    }
    let detail_mass = box_blur(&detail_ink, fw, fh, r_mass);
    for i in 0..npix {
        // 细节层只能提供最细两支笔的墨深（HB/2B）。在暗部它描的是发丝之间的
        // 分界，不是整块的黑：若把细节墨量全额从目标里扣掉，头发会掉一档
        // （实测暗档 0.57→0.48），还会在细节稀疏处留下白色针眼。按调子限制
        // 扣除比例——亮处细节几乎就是全部墨量，暗处排线仍是底色。
        // 暗部（头发、瞳孔）一点都不扣：细节在那里是发丝的分界，排线才是
        // 整块的黑。扣了会在发丛里留下成片白针眼，放大看像掉墨。
        let t = target[i];
        let share = if t < 0.30 {
            0.80
        } else if t < 0.55 {
            0.40
        } else {
            0.0
        };
        target[i] = (t - detail_mass[i] * share).max(0.0);
    }

    // —— 分层落线 ——
    let mut ink = vec![0f32; npix];
    let mut view = vec![0f32; npix];
    // 0.9mm 约等于两个最粗行距：既能把「行」平均成调子，又看得到真实的缝。
    let r_view = ((0.90 / px_mm).round() as usize).clamp(1, 60);
    // 一层的目标覆盖率。线距 = 笔宽 / cov_max，所以一层最多补
    // `笔深 × cov_max` 的墨；剩余缺口由下一层（另一支笔 / 另一个角度）补。
    // 深笔覆盖率给得更高：暗部要靠「线挨线」才压得下去，浅笔留缝才有纸白。
    // 每一层 =（笔, 方向）。全图只用两个方向（基准角与垂直），和交叉排线
    // 一样：层与层之间是同一族平行线的加密与加深，不会织出四向乱网。
    // 浅中调用 HB/2B 两个方向交叉；4B/8B 只在块面足够暗时补同一对方向。
    const LAYER_PLAN: [(usize, f64); 8] = [
        (0, 0.0),
        (0, 90.0),
        (1, 0.0),
        (1, 90.0),
        (2, 0.0),
        (2, 90.0),
        (3, 0.0),
        (3, 90.0),
    ];
    // 单笔模式也只有基准角与垂直两族：层数越多只是同一族线排得更密、叠得更深，
    // 和交叉排线一样。以前这里每层换一个角度（45/135/22/112…），几层下来就是
    // 四向乱网——这正是用户说的「排线混乱」。
    const SINGLE_DEG: [f64; 8] = [0.0, 90.0, 0.0, 90.0, 0.0, 90.0, 0.0, 90.0];
    // 每支笔负责一个调子带：低于下沿不画，高于上沿交棒给下一支笔。
    // 带在交界处重叠 0.08~0.10（渐变），两支笔各画一部分，看不出接缝。
    // 带宽由「笔深 × 覆盖率」决定；这样同一块纸上最多叠两支笔，
    // 不会出现四支笔全压上去的实心黑板（眉毛、发丛被涂黑的主因）。
    const PEN_LO: [f32; 4] = [0.0, 0.14, 0.40, 0.64];
    const PEN_HI: [f32; 4] = [0.28, 0.50, 0.74, 1.01];
    const PEN_RAMP: [f32; 4] = [0.05, 0.06, 0.06, 0.06];
    let n_layers = if only_detail {
        0
    } else {
        passes.clamp(2, 8) as usize
    };
    const MAX_STROKES: usize = 120_000;
    const MAX_LEN_MM: f64 = 900_000.0;
    let mut stroke_no = 0usize;
    let mut total_strokes = 0usize;
    let mut total_len = 0.0f64;
    // 行距按「真实笔宽」算；`view` 的每层墨量按同一套几何折算。
    let hatch_jitter = (jitter_mm * 0.35).min(0.05);
    for li in 0..n_layers {
        if total_strokes >= MAX_STROKES || total_len >= MAX_LEN_MM {
            break;
        }
        // 换笔时重算已落墨量。同一支笔的两个方向（li、li+1）共用这个 view：
        // 第二个方向要的是同一个缺口，不是第一层画完后的残余。
        if li % 2 == 0 && li > 0 {
            view = box_blur(&ink, fw, fh, r_view);
        }
        // 笔和方向都来自固定计划；层数多出来的部分循环这套顺序。
        let (p, deg) = if m_eff > 1 {
            let (plan_p, plan_deg) = LAYER_PLAN[li % LAYER_PLAN.len()];
            (plan_p.min(m_eff - 1), plan_deg)
        } else {
            (0, SINGLE_DEG[li % SINGLE_DEG.len()])
        };
        let (pw, pdark) = (pen_width(p), pen_dark(p));
        let cov_max = if m_eff > 1 {
            [0.72f64, 0.80, 0.88, 1.00][p.min(3)]
        } else {
            0.72
        };
        let half_px = (pw * 0.5 / px_mm).max(0.5);
        let pitch_px = ((pw / cov_max) / px_mm).max(1.5);
        let cov_eff = (2.0 * half_px / pitch_px).clamp(0.10, 1.0);
        let cap = (pdark as f64 * cov_eff) as f32;
        let ang = base + deg.to_radians();
        let (lo, hi, ramp) = if m_eff > 1 {
            (PEN_LO[p], PEN_HI[p], PEN_RAMP[p])
        } else {
            (0.0, 1.01, 0.10)
        };
        // 两个方向的配合：第一方向最多铺到 72% 的行密度，再暗由第二方向补。
        // 浅调（n < 0.72）只有第一方向——和真实排线一样，浅灰是一族平行线，
        // 不是网格；中间调开始出现交叉；暗部两个方向都满，才压得下去。
        // 第二方向的密度由「并集 = 目标覆盖率」反解，不会重叠过量。
        let is_b = li % 2 == 1;
        let mut fmap = vec![0f32; npix];
        let mut peak = 0.0f32;
        for i in 0..npix {
            if ht.bg[i] {
                continue;
            }
            let t = target[i];
            if t <= 0.035 {
                continue;
            }
            let need = t - view[i];
            if need <= 0.004 {
                continue;
            }
            let gate = {
                let up = if lo <= 0.0 {
                    1.0
                } else {
                    let x = ((t - lo) / ramp).clamp(0.0, 1.0);
                    x * x * (3.0 - 2.0 * x)
                };
                let dn = if hi >= 1.0 {
                    1.0
                } else {
                    let x = ((t - hi) / 0.08).clamp(0.0, 1.0);
                    1.0 - x * x * (3.0 - 2.0 * x)
                };
                up * dn
            };
            if gate <= 0.0 {
                continue;
            }
            let n = (need / cap).clamp(0.0, 1.0) as f64;
            let fa = n.min(0.72);
            let f = if is_b {
                let ua = fa * cov_eff;
                let un = n * cov_eff;
                if un > ua + 1e-4 {
                    (((un - ua) / (1.0 - ua).max(1e-6)) / cov_eff).min(1.0)
                } else {
                    0.0
                }
            } else {
                fa
            };
            let f = (f as f32) * gate;
            if f > 0.02 {
                fmap[i] = f;
                if f > peak {
                    peak = f;
                }
            }
        }
        if peak <= 0.0 {
            continue;
        }
        let min_run = (0.37 / px_mm).max(1.5);
        let runs = scan_layer_runs(
            &fmap,
            fw,
            fh,
            ang,
            pitch_px,
            li as f64 * 0.371,
            0.8,
            min_run,
            (2.40 / px_mm).max(3.0),
        );
        if runs.is_empty() {
            continue;
        }
        let mut layer: Vec<Stroke> = Vec::with_capacity(runs.len());
        let mut layer_len = 0.0f64;
        for &(a, b) in &runs {
            let d = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
            let len_mm = d * px_mm;
            if len_mm < 0.30 {
                continue;
            }
            layer.push(Stroke {
                pts: vec![a, b],
                loose: [true, true],
                len_mm,
                mean_dark: pdark as f64,
                mean_width_mm: pw,
                theta: ang,
                rank: 0.5,
                structural: false,
                passes: 1,
            });
            // 墨量按并集记：同一格被 2B 和 8B 各画一次，纸上仍是 8B 那一层。
            stamp_ink_hard(&mut ink, fw, fh, a, b, half_px, pdark);
            layer_len += len_mm;
        }
        if layer.is_empty() {
            continue;
        }
        if std::env::var("SKETCH_DEBUG").is_ok() {
            eprintln!(
                "分层排线：层 {li} 笔 {p} 角度 {:.0}° 行距 {:.2}mm · {} 条 / {:.0}mm",
                ang.to_degrees(),
                pitch_px * px_mm,
                layer.len(),
                layer_len
            );
        }
        total_strokes += layer.len();
        total_len += layer_len;
        emit_hatch_set(
            &layer,
            order_mode,
            order_block_px,
            scale,
            hatch_jitter,
            pw,
            p as u8,
            &mut stroke_no,
            &mut out,
            &mut pens_out,
        );
    }
    if let Ok(p) = std::env::var("SKETCH_DUMP_VIEW") {
        let mut img = image::GrayImage::new(fw as u32, fh as u32);
        for y in 0..fh {
            for x in 0..fw {
                let v = (view[y * fw + x] * 400.0).clamp(0.0, 255.0) as u8;
                img.put_pixel(x as u32, y as u32, image::Luma([v]));
            }
        }
        let _ = img.save(&p);
    }
    if let Ok(p) = std::env::var("SKETCH_DUMP_INK") {
        let mut img = image::GrayImage::new(fw as u32, fh as u32);
        for y in 0..fh {
            for x in 0..fw {
                let v = (ink[y * fw + x] * 255.0).clamp(0.0, 255.0) as u8;
                img.put_pixel(x as u32, y as u32, image::Luma([v]));
            }
        }
        let _ = img.save(&p);
    }

    // 细节最后画：原稿真实存在的线压在排线之上，形才立得住。
    // 不再把细节先记进「已落墨」——旧版那样做会让排线绕着细节留一圈白晕，
    // 放大看就是发丝/眼线旁边一条白边。石墨叠在一起不会更黑，重一点无妨。
    for (p, list) in buckets.iter_mut().enumerate() {
        if list.is_empty() {
            continue;
        }
        let (pw, pdark) = (pen_width(p), pen_dark(p));
        for s in list.iter_mut() {
            s.passes = 1;
            s.mean_width_mm = pw;
            s.mean_dark = pdark as f64;
        }
        if std::env::var("SKETCH_DEBUG").is_ok() {
            eprintln!(
                "分层排线：细节笔 {p} {} 条 {:.0}mm",
                list.len(),
                list.iter().map(|s| s.len_mm).sum::<f64>()
            );
        }
        emit_hatch_set(
            list,
            order_mode,
            order_block_px,
            scale,
            jitter_mm,
            pw,
            p as u8,
            &mut stroke_no,
            &mut out,
            &mut pens_out,
        );
    }

    // 换笔次数要少：按笔号稳定重排，同号笔迹保持原有落笔顺序。
    let mut pairs: Vec<(u8, Polyline)> = pens_out.into_iter().zip(out).collect();
    pairs.sort_by_key(|(p, _)| *p);
    let (pens_out, out) = pairs.into_iter().unzip();

    HatchOut {
        polylines: out,
        pens: pens_out,
    }
}


/// 原稿细节层：墨脊 = 原稿里真实画出来的笔触。
///
/// 只保留够长、够深的脊（发丝、眼线、衣褶），并设总长上限；
/// 纸纹、噪点提取出来的是 1mm 碎段，直接丢掉。
fn source_detail_strokes(
    field: &Field,
    ht: &HatchTone,
    threshold: f64,
    pen_mm: f64,
) -> Vec<Stroke> {
    // 多笔提取要“密”：同一根浅笔触也要拿到，否则只剩头发/五官，
    // 脸上一层浅排线全丢，合成排线再补也不是原稿的位置。
    //
    // 纸白门槛压到 0.012：这张浅调素描的脸只有 0.03，0.18/0.20 会把
    // 恰好是“精细程度”来源的浅排线全部丢掉。背景由 `ht.bg` 与本层中点检查兜底。
    // 不做墨量自校准：目标是复现原稿的线位与线距，不是用 0.3mm 的笔去凑面积。
    let (mut strokes, _) =
        ridge_strokes_gated(field, pen_mm, threshold, 1.6, 0.012, 0.016, false);
    let stage = |label: &str, v: &[Stroke]| {
        if std::env::var("SKETCH_DEBUG").is_ok() {
            eprintln!(
                "  细节{label} {} 条 {:.0}mm",
                v.len(),
                v.iter().map(|s| s.len_mm).sum::<f64>()
            );
        }
    };
    stage("提取", &strokes);
    let fw = field.w;
    let fh = field.h;
    // 粗黑团块（瞳孔、虹膜、浓密发丛）内部的骨架不是「线」而是填充：
    // 照线描出来是一坨坨粗黑折块，把眼睛画成碎玻璃。轮廓、发丝都在
    // 0.7mm 以内；超过 1.05mm 的宽脊交给排线去填。
    strokes.retain(|s| s.mean_width_mm <= 1.05);
    stage("宽度", &strokes);
    // 只保留「真线」：发丝、眼线、唇缝、衣褶，以及原稿里明确画重的笔触；
    // 另外把**浅排线**也放进来（门槛从 0.50 降到 0.20）。
    //
    // 0.50 是为了挡「骨架把相邻浅排线串成弯曲线」——那批伪线确实难看。
    // 但一刀切掉浅排线等于把原稿最细的一层调子全丢了，只剩合成排线，
    // 放大看就「没细节」。现在改用几何 + 方向 + 墨量三重校验：
    // 只在「短、与局部排线同向、确实压在墨上」的地方保留浅排线，
    // 乱串出来的弯线仍然会被丢掉。
    strokes.retain(|s| s.mean_dark >= 0.16);
    stage("墨深", &strokes);
    strokes.retain(|s| {
        if s.len_mm < 1.2 {
            return false;
        }
        // 只看中点会漏掉「一半在纸上、一半飘在背景里」的碎发与纸纹链，
        // 它们在背景上就是几根没来由的乱线。整条有三分之一落在背景就丢掉。
        let mut bg_n = 0usize;
        for &(x, y) in &s.pts {
            let i = (y as usize).min(fh.saturating_sub(1)) * fw
                + (x as usize).min(fw.saturating_sub(1));
            if ht.bg[i] {
                bg_n += 1;
            }
        }
        if bg_n * 100 > s.pts.len() * 15 {
            return false;
        }
        // 实心黑团（瞳孔、虹膜、最深的发芯）内部没有「线」，只有填充。
        // 骨架会在团块里分出许多支叉，照线描出来是一团乱麻，眼睛变成毛线球，
        // 深发芯则被 2B 沿脊描成一块实心黑。1.3mm 平滑调子仍然接近全黑的
        // 笔触交给排线去压深——排线能靠笔档和线距控制灰度，骨架描线只会叠墨。
        // 门槛从 0.93 收到 0.84：实测顶灯下发丛主体 tone 常在 0.85~0.92，
        // 0.93 时全被放过，正好在那块最亮的高光旁边糊成一团黑。
        // 眼线只有 0.4~0.6mm 宽，平滑后会掉到 0.7 以下，不会被误删。
        if s.mean_dark >= 0.85 {
            let n = s.pts.len().max(1);
            let mt: f32 = s
                .pts
                .iter()
                .map(|&(x, y)| {
                    ht.tone[(y as usize).min(fh.saturating_sub(1)) * fw
                        + (x as usize).min(fw.saturating_sub(1))]
                })
                .sum::<f32>()
                / n as f32;
            if mt > 0.84 {
                return false;
            }
        }
        // 原稿浅调里的长直线几乎都是追踪器把相邻几根短排线接出来的伪线：
        // 真正的铅笔排线不会一笔横跨半张脸。曲率检查放过真轮廓
        // （弧长/端点距 > 1.01），只砍近乎直线（>0.988）且超过 18mm 的链。
        if s.mean_dark < 0.30 && s.len_mm > 18.0 {
            let a = s.pts.first().copied().unwrap_or((0.0, 0.0));
            let b = s.pts.last().copied().unwrap_or(a);
            let chord = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt() * field.px_mm;
            if chord > 1e-6 && s.len_mm / chord > 0.988 {
                return false;
            }
        }
        true
    });
    // —— 浅排线的三重校验 ——
    //
    // 提取器在浅调区最容易犯两种错：把相邻几根排线串成一条斜跨的
    // 「划痕」，或者沿纸纹/噪点走出一段弯线。两者都会在画面上变成
    // 原稿没有的重线。这里对 mean_dark < 0.45 的浅笔触逐个检查：
    //   1) 整条确实压在墨上（不是纸白）；
    //   2) 走向与周围排线（1.2mm / 6mm 脊向）基本一致；
    //   3) 不出现十几毫米的近乎直线的一笔——原稿的浅排线都有起落，
    //      那种长直线是缝合伪线。
    // 深调（发丝、眼线、唇缝）本来就在墨脊上，不再过这三道。
    let dir_ok = |s: &Stroke| -> (bool, bool) {
        // 返回 (是否可判定, 是否一致)。整条附近都没有可靠方向时不可判定。
        let n_pts = s.pts.len();
        if n_pts < 3 {
            return (false, true);
        }
        let mut n = 0usize;
        let mut ok = 0usize;
        let mut k = 1usize;
        while k + 1 < n_pts {
            let a = s.pts[k - 1];
            let b = s.pts[k + 1];
            let dx = b.0 - a.0;
            let dy = b.1 - a.1;
            if dx * dx + dy * dy < 4.0 {
                k += 3;
                continue;
            }
            let th = dy.atan2(dx);
            let (ga, gc) = field.smooth_ridge_ang_at(s.pts[k].0, s.pts[k].1);
            let (ba, bc) = field.broad_ridge_ang_at(s.pts[k].0, s.pts[k].1);
            let (fa, fc) = if gc >= bc {
                (ga as f64, gc)
            } else {
                (ba as f64, bc)
            };
            if fc < 0.18 {
                k += 3;
                continue;
            }
            n += 1;
            let mut d = (th - fa).abs() % std::f64::consts::PI;
            if d > std::f64::consts::FRAC_PI_2 {
                d = std::f64::consts::PI - d;
            }
            // 40° / 半数为界：原稿的浅排线是斜向交叉的，局部主方向（结构张量）
            // 只会记住更密的那一族，另一族在 26° 判据下会被整批当成「方向不符」
            // 丢掉——实测 1200 条里 1178 条死在这一步，浅调细节因此只剩头发。
            // 放宽后仍能挡住沿鼻侧、下颌乱走的伪线：它们与主方向的夹角接近 90°。
            if d < 40f64.to_radians() {
                ok += 1;
            }
            k += 3;
        }
        (n > 0, n == 0 || ok * 2 >= n)
    };
    let ppm = field.px_mm.max(1e-9);
    let mut rej = [0usize; 6];
    strokes.retain(|s| {
        if s.mean_dark >= 0.45 {
            return true;
        }
        if s.mean_width_mm > 0.80 {
            rej[0] += 1;
            return false;
        }
        let n = s.pts.len().max(1);
        let mut tone_sum = 0f32;
        let mut inked = 0usize;
        for &(x, y) in &s.pts {
            let xi = (x as usize).min(fw.saturating_sub(1));
            let yi = (y as usize).min(fh.saturating_sub(1));
            let t = ht.tone[yi * fw + xi];
            tone_sum += t;
            if t > 0.025 {
                inked += 1;
            }
        }
        let mean_tone = tone_sum / n as f32;
        if mean_tone < 0.050 || inked * 20 < n * 13 {
            rej[1] += 1;
            return false;
        }
        // 中浅调里比半根笔宽还宽的脊不是「线」，是明暗过渡的坡：
        // 鼻侧、唇角、脸颊阴影的边界在 Hessian 上也是脊，但宽度 1mm 以上，
        // 照线描出来就是原稿没有的硬轮廓。真正的排线/发丝都在 0.75mm 内。
        if s.mean_dark < 0.60 && s.mean_width_mm > 0.75 {
            rej[2] += 1;
            return false;
        }
        let (known, agree) = dir_ok(s);
        if known && !agree {
            rej[3] += 1;
            return false;
        }
        if !known && s.len_mm > 4.0 {
            rej[4] += 1;
            return false;
        }
        // 浅调里十几毫米的近直线几乎都是缝合伪线；明显的弯是下颌、唇线，保留。
        if s.len_mm > 10.0 {
            let a = s.pts.first().copied().unwrap_or((0.0, 0.0));
            let b = s.pts.last().copied().unwrap_or(a);
            let chord = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt() * ppm;
            if chord > 1e-6 && s.len_mm / chord > 0.985 {
                rej[5] += 1;
                return false;
            }
        }
        true
    });
    stage("浅校验", &strokes);
    if std::env::var("SKETCH_DEBUG").is_ok() {
        eprintln!(
            "  浅校验拒因 宽 {}/ 墨 {}/ 坡度 {}/ 方向 {}/ 无向长 {}/ 直链 {}",
            rej[0], rej[1], rej[2], rej[3], rej[4], rej[5]
        );
    }
    // 预算分两池：浅调（脸颊排线）和深调（发丝、五官）各有上限。
    // 单池按「长 × 深²」排序时，浅排线的分数只有头发的一个零头，预算再大
    // 也轮不到它们——脸就永远空着。分池后浅调至少有自己的一份。
    const BUDGET_MM: f64 = 70_000.0;
    const LIGHT_SHARE: f64 = 0.55;
    let is_light = |s: &Stroke| s.mean_dark < 0.30;
    let light_budget = BUDGET_MM * LIGHT_SHARE;
    let dark_budget = BUDGET_MM - light_budget;
    let score = |s: &Stroke| {
        s.len_mm.min(30.0).powf(0.7)
            * (0.15 + s.mean_dark.clamp(0.0, 1.0) as f64).powf(1.2)
    };
    let n_cand = strokes.len();
    let (mut light_pool, mut dark_pool): (Vec<Stroke>, Vec<Stroke>) =
        strokes.into_iter().partition(|s| is_light(s));
    if std::env::var("SKETCH_DEBUG").is_ok() {
        for (label, pool) in [("浅", &light_pool), ("深", &dark_pool)] {
            let mut wv: Vec<f64> = pool.iter().map(|s| s.mean_width_mm).collect();
            let mut tv: Vec<f64> = pool.iter().map(|s| s.mean_dark as f64).collect();
            wv.sort_by(|a, b| a.partial_cmp(b).unwrap());
            tv.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let q = |v: &Vec<f64>, f: f64| v[((v.len() - 1) as f64 * f) as usize];
            if !wv.is_empty() {
                eprintln!(
                    "  细节池{label} n={} 宽 p50/{:.2} p90/{:.2} p99/{:.2}mm；墨 p50/{:.2} p90/{:.2}",
                    wv.len(),
                    q(&wv, 0.5),
                    q(&wv, 0.9),
                    q(&wv, 0.99),
                    q(&tv, 0.5),
                    q(&tv, 0.9)
                );
            }
        }
    }
    let mut kept_light: Vec<Stroke> = Vec::new();
    let mut kept_dark: Vec<Stroke> = Vec::new();
    for (pool, budget, kept) in [
        (&mut light_pool, light_budget, &mut kept_light),
        (&mut dark_pool, dark_budget, &mut kept_dark),
    ] {
        pool.sort_by(|a, b| {
            score(b)
                .partial_cmp(&score(a))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut acc = 0.0f64;
        pool.retain(|s| {
            if acc + s.len_mm > budget && acc > 0.0 {
                return false;
            }
            acc += s.len_mm;
            true
        });
        kept.append(pool);
    }
    let before_len: f64 = kept_light.iter().chain(kept_dark.iter()).map(|s| s.len_mm).sum();
    let before_n = kept_light.len() + kept_dark.len();
    if std::env::var("SKETCH_DEBUG").is_ok() {
        let light_len: f64 = kept_light.iter().map(|s| s.len_mm).sum();
        eprintln!(
            "素描笔触：原稿细节 {before_n} 条 {before_len:.0}mm（浅调 {light_len:.0}mm / 上限 {BUDGET_MM:.0}mm；候选 {n_cand} 条）"
        );
    }
    // 原稿的铅笔笔触几乎都是 2~5mm 的短直线；提取出来的流线却常常跨过
    // 几根排线的交点，连成 10~40mm 的摆动长线。
    //
    // 浅调切 2mm 弦并在两端各留 0.2mm 断口：原稿脸上的浅排线本来就是
    // 一截一截的，共享端点的连续弦在图上会连成一根几十毫米的「划痕」，
    // 看着像刮擦而不是排线。深调（发丝、眼线）保持 3mm 连续弦——
    // 头发要的是连成一气的长发丝。
    let px_mm = field.px_mm.max(1e-9);
    let mut short = Vec::with_capacity(before_n * 2);
    // 浅调（脸颊排线）是「调子」：按短直线切。原稿的浅排线本来就是一截一截的，
    // 共享端点的连续长弦在图上会连成一根几十毫米的划痕。
    //
    // 但浅调里也混着「形」——下颌、鼻翼、唇缝、发根这些轮廓线在 1.3mm
    // 均值下同样是浅的。把它们也切成 2mm 直弦，轮廓就变成了排线那样
    // 一节节的直线，正是用户说的「轮廓线不该像调子」。用弧长/端点距
    // 区分：排线接近直线（比值≈1.0），真轮廓明显弯（>1.08）。
    for s in kept_light {
        let first = s.pts.first().copied().unwrap_or((0.0, 0.0));
        let last = s.pts.last().copied().unwrap_or(first);
        let arc: f64 = s
            .pts
            .windows(2)
            .map(|w| ((w[1].0 - w[0].0).powi(2) + (w[1].1 - w[0].1).powi(2)).sqrt())
            .sum();
        let chord = ((last.0 - first.0).powi(2) + (last.1 - first.1).powi(2)).sqrt();
        let curved = arc * px_mm > 4.0 && chord > 1.0 && arc / chord > 1.08;
        if curved {
            slice_detail_curve(s, px_mm, 4.0, &mut short);
        } else {
            split_detail_chords(s, px_mm, 2.6, 0.25, &mut short);
        }
    }
    // 深调（发丝、眼线、衣褶、下颌）是「形」，不是调子：沿原流线切片、
    // **保留中间点**，绝不把一根弯曲的轮廓捋成几段直线。
    //
    // 上限从 6mm 放宽到 18mm：6mm 时一缕 30~60mm 的长发被切成五六截，
    // 放大看头发是一堆短划痕，没有「发丝」的连续性——正是用户说的
    // 「细节丢失、线条不够细」。18mm 仍能挡住追踪把几根线连成 40~57mm
    // 的伪线（那种一笔横穿半张脸的长线），又保得住一根发丝的起伏。
    for s in kept_dark {
        slice_detail_curve(s, px_mm, 18.0, &mut short);
    }
    if std::env::var("SKETCH_DEBUG").is_ok() {
        eprintln!(
            "素描笔触：细节切短 {} 条 {:.0}mm",
            short.len(),
            short.iter().map(|s| s.len_mm).sum::<f64>()
        );
    }
    short
}

/// 把一条提取出来的长流线切成直线弦；`gap_mm` 在每段两端留断口。
fn split_detail_chords(
    s: Stroke,
    px_mm: f64,
    chord_mm: f64,
    gap_mm: f64,
    out: &mut Vec<Stroke>,
) {
    let total_px: f64 = s
        .pts
        .windows(2)
        .map(|w| ((w[1].0 - w[0].0).powi(2) + (w[1].1 - w[0].1).powi(2)).sqrt())
        .sum();
    let total_mm = total_px * px_mm;
    // 注意不能用「点数 < 3」跳过：抽稀后的长直线常常只剩两个端点，
    // 30~200mm 一根，正好就是脸上最扎眼的那种「划痕」。只按长度判断。
    if s.pts.len() < 2 || total_mm <= chord_mm * 1.35 {
        out.push(s);
        return;
    }
    let n = (total_mm / chord_mm).round().max(1.0) as usize;
    let step = total_px / n as f64;
    let gap_px = (gap_mm / px_mm).max(0.0);
    for k in 0..n {
        let a = point_at(&s.pts, step * k as f64);
        let b = point_at(&s.pts, step * (k + 1) as f64);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let seg_px = (dx * dx + dy * dy).sqrt();
        let lmm = seg_px * px_mm;
        if lmm < 0.8 || seg_px <= gap_px * 2.0 + 1.0 {
            // 太短：留原样（不然断口会把它吃没）
            if lmm >= 0.8 {
                out.push(Stroke {
                    theta: path_theta(&[a, b]),
                    pts: vec![a, b],
                    loose: s.loose,
                    len_mm: lmm,
                    mean_dark: s.mean_dark,
                    mean_width_mm: s.mean_width_mm,
                    rank: s.rank,
                    structural: s.structural,
                    passes: s.passes,
                });
            }
            continue;
        }
        let (ux, uy) = (dx / seg_px, dy / seg_px);
        let a2 = (a.0 + ux * gap_px, a.1 + uy * gap_px);
        let b2 = (b.0 - ux * gap_px, b.1 - uy * gap_px);
        let l2 = (seg_px - 2.0 * gap_px) * px_mm;
        out.push(Stroke {
            theta: path_theta(&[a2, b2]),
            pts: vec![a2, b2],
            loose: s.loose,
            len_mm: l2,
            mean_dark: s.mean_dark,
            mean_width_mm: s.mean_width_mm,
            rank: s.rank,
            structural: s.structural,
            passes: s.passes,
        });
    }
}

/// 把一条「形」的流线按 14mm 切片，但**保留中间点**，不拉直。
///
/// 发丝、眼线、衣褶、下颌这些是原稿的形；切成首尾直线弦会把一根弯的轮廓
/// 捋成几段折线，看起来和排线一样「直」，形就散了。这里只负责限制单笔长度
/// （防止追踪把几根线连成几十毫米的伪线），曲率原样保留。
fn slice_detail_curve(s: Stroke, px_mm: f64, max_mm: f64, out: &mut Vec<Stroke>) {
    let seg = |a: (f64, f64), b: (f64, f64)| {
        ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt()
    };
    let total_px: f64 = s.pts.windows(2).map(|w| seg(w[0], w[1])).sum();
    if s.pts.len() < 2 || total_px * px_mm <= max_mm {
        out.push(s);
        return;
    }
    let n = (total_px * px_mm / max_mm).ceil().max(1.0) as usize;
    let step = total_px / n as f64;
    let mut pieces: Vec<Vec<(f64, f64)>> = Vec::with_capacity(n);
    for k in 0..n {
        let p = slice_path(&s.pts, step * k as f64, step * (k + 1) as f64);
        if p.len() >= 2 {
            pieces.push(p);
        }
    }
    for mut piece in pieces {
        // 骨架是逐像素走的，1~2px 宽的墨迹会让中线来回抖；直接描出来
        // 眼睛、鼻翼全是锯齿状的碎折线。两遍三点平均只抹掉像素级抖动，
        // 原稿的弯还是原来的弯——「形」不能被拉直，也不能被画成锯齿。
        if piece.len() >= 3 {
            smooth_in_place(&mut piece);
            smooth_in_place(&mut piece);
        }
        let len: f64 = piece.windows(2).map(|w| seg(w[0], w[1])).sum::<f64>() * px_mm;
        if len < 0.5 {
            continue;
        }
        out.push(Stroke {
            theta: path_theta(&piece),
            pts: piece,
            loose: s.loose,
            len_mm: len,
            mean_dark: s.mean_dark,
            mean_width_mm: s.mean_width_mm,
            rank: s.rank,
            structural: s.structural,
            passes: s.passes,
        });
    }
}

/// 取折线在弧长区间 `[a0, a1]` 内的部分（含中间顶点与两端插值点）。
fn slice_path(pts: &[(f64, f64)], a0: f64, a1: f64) -> Vec<(f64, f64)> {
    let mut out: Vec<(f64, f64)> = Vec::new();
    if pts.len() < 2 || a1 <= a0 {
        return out;
    }
    out.push(point_at(pts, a0));
    let mut acc = 0.0f64;
    for w in pts.windows(2) {
        let l = ((w[1].0 - w[0].0).powi(2) + (w[1].1 - w[0].1).powi(2)).sqrt();
        let end = acc + l;
        if end > a0 && end < a1 {
            out.push(w[1]);
        }
        acc = end;
        if acc >= a1 {
            break;
        }
    }
    out.push(point_at(pts, a1));
    out.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9);
    out
}

fn point_at(pts: &[(f64, f64)], s: f64) -> (f64, f64) {
    if pts.len() < 2 || s <= 0.0 {
        return pts[0];
    }
    let mut acc = 0.0f64;
    for w in pts.windows(2) {
        let (dx, dy) = (w[1].0 - w[0].0, w[1].1 - w[0].1);
        let l = (dx * dx + dy * dy).sqrt();
        if acc + l >= s {
            let t = if l > 1e-9 { (s - acc) / l } else { 0.0 };
            return (w[0].0 + dx * t, w[0].1 + dy * t);
        }
        acc += l;
    }
    *pts.last().unwrap()
}








/// 从四周边框泛洪，找出连成一片的背景（背景当纸白）。
///
/// `norm` 是原始调子，用来判断「这里是不是纸」；`smooth` 是 0.4mm 平滑图，
/// 只用来判断相邻像素的缓变。两者分工：纸纹的逐像素起伏不该切断漫填，
/// 而人脸那种由轮廓线界定的跳变必须切断。
fn background_mask(norm: &[f32], smooth: &[f32], tex: &[f32], w: usize, h: usize) -> Vec<bool> {
    if w == 0 || h == 0 {
        return Vec::new();
    }
    // 纸白从**四周边框**估计，而不是只看顶部一条带。
    //
    // 这张肖像顶上大半是头发：顶部带的中位调子直接落到头发上，`bg > 0.35`
    // 一触发就返回空掩码，背景于是被当成调子铺满细排线。边框上背景占多数，
    // 取较亮的那一档（p90）再漫填，才不会被人像自身带偏。
    let edge = (h / 80).max(2).min(8);
    let mut hist = [0u32; 64];
    for y in 0..h {
        for x in 0..w {
            if y < edge || y + edge >= h || x < edge || x + edge >= w {
                hist[((norm[y * w + x].clamp(0.0, 1.0) * 63.0) as usize).min(63)] += 1;
            }
        }
    }
    // 边框上出现最多的调子 = 纸白。p90 会被下边框的衬衫、头发推高，
    // 容差一放宽就把脸颊的浅调子也漫填进背景。
    let mut bg = 0.0f32;
    let mut best = 0u32;
    for (i, &c) in hist.iter().enumerate() {
        if c > best {
            best = c;
            bg = (i as f32 + 0.5) / 64.0;
        }
    }
    // 只有「浅色底」才当背景。深色底（暗调照片、夜景）本身就是画面的一部分，
    // 整片抹白会把主体也一起抹掉。
    if bg > 0.35 {
        return vec![false; w * h];
    }
    let seed_lo = (bg - 0.05).max(0.0);
    let seed_hi = (bg + 0.05).min(0.34);
    // 相邻像素材质差：背景是平滑的（哪怕有渐变），笔触/人脸边界会跳变。
    // 用「相邻差」而不是固定区间，才能沿着背景的渐变一直走，又在头发、
    // 下颌这些地方停住。
    //
    let step_max = 0.018f32;
    let abs_max = 0.42f32;
    // 漫填只能停在「比纸白略深一点」的地方。只靠相邻差不够：脸是平滑的，
    // 从纸白一路按 0.018/px 爬，几十像素就爬进脸颊——背景掩码把整张脸吞掉，
    // 脸上一根调子都不排，就是「画面发白、明暗没层次」的直接原因。
    // 纸的渐变/暗角不会超过纸白 0.09；超过就是画面内容。
    let climb_max = 0.09f32;
    // 纹理屏障：纸面只有均匀的颗粒，脸/衣褶即使很亮，也有 0.8mm 尺度上的
    // 排线起伏。高频能量超过 0.012 就停下，漫填不会穿过高光进入画面内容。
    let tex_max = 0.012f32;
    // 纯纸白不参与漫填：它要么是背景（本来就不会排线），要么是额头/鼻梁
    // 高光。收紧步进试过（0.006），结果反把半张脸判成背景——高调人像的
    // 脸颊本来就与纸白连续。白走廊的问题交给下面的「墨量邻近度」处理。
    let raw_floor = 0.006f32;
    let mut mask = vec![false; w * h];
    let mut stack: Vec<usize> = Vec::new();
    // 四条边都可能是背景的入口，全部入栈。
    for y in 0..h {
        for x in 0..w {
            if !(y < edge || y + edge >= h || x < edge || x + edge >= w) {
                continue;
            }
            let i = y * w + x;
            if !mask[i] && norm[i] >= seed_lo && norm[i] <= seed_hi {
                mask[i] = true;
                stack.push(i);
            }
        }
    }
    while let Some(i) = stack.pop() {
        let x = i % w;
        let y = i / w;
        let base = smooth[i];
        let visit = |j: usize, stack: &mut Vec<usize>, mask: &mut Vec<bool>| {
            if mask[j] || norm[j] <= raw_floor || norm[j] > abs_max || norm[j] > bg + climb_max
                || tex[j] > tex_max
            {
                return;
            }
            if (smooth[j] - base).abs() <= step_max {
                mask[j] = true;
                stack.push(j);
            }
        };
        if x > 0 {
            visit(i - 1, &mut stack, &mut mask);
        }
        if x + 1 < w {
            visit(i + 1, &mut stack, &mut mask);
        }
        if y > 0 {
            visit(i - w, &mut stack, &mut mask);
        }
        if y + 1 < h {
            visit(i + w, &mut stack, &mut mask);
        }
    }
    // 掩码外扩 2px。
    //
    // 漫填按「相邻平滑差」走，纸纹上偶尔有一两个像素的暗点通不过，留下
    // 针孔；针孔里会落进一根 1~3mm 的短笔，在大图背景里就是一根根
    // 没来由的碎线（放大最扎眼）。外扩 2px（约 0.14mm）把细孔吃掉，
    // 边界只内收一条线宽，发梢/衣角不受影响。
    let mut grown = mask.clone();
    for y in 0..h {
        for x in 0..w {
            if !mask[y * w + x] {
                continue;
            }
            for dy in -2i32..=2 {
                let yy = y as i32 + dy;
                if yy < 0 || yy >= h as i32 {
                    continue;
                }
                for dx in -2i32..=2 {
                    let xx = x as i32 + dx;
                    if xx >= 0 && xx < w as i32 {
                        grown[yy as usize * w + xx as usize] = true;
                    }
                }
            }
        }
    }
    let mask = grown;
    if std::env::var("SKETCH_DEBUG").is_ok() {
        let n = mask.iter().filter(|&&v| v).count();
        eprintln!(
            "背景掩码：纸白 {bg:.3}，覆盖 {:.1}%",
            n as f64 * 100.0 / (w * h) as f64
        );
        for (x, y, label) in [
            (500usize, 600usize, "额头"),
            (400, 1200, "左脸"),
            (100, 1200, "背景左下"),
            (100, 400, "背景左上"),
            (50, 800, "背景左中"),
            (600, 40, "背景顶"),
            (1100, 900, "背景右"),
            (900, 400, "头发"),
            (800, 1300, "右脸"),
        ] {
            if x < w && y < h {
                let i = y * w + x;
                eprintln!(
                    "  掩码 {label} ({x},{y}) = {}，调子 {:.3} 平滑 {:.3} 残差 {:.3}",
                    mask[i],
                    norm[i],
                    smooth[i],
                    (norm[i] - smooth[i]).abs()
                );
            }
        }
    }
    mask
}


/// 按层顺序落笔：同一层内按所选顺序串联，层与层之间保持「浅 → 深」。
#[allow(clippy::too_many_arguments)]
fn emit_hatch_set(
    strokes: &[Stroke],
    order_mode: u8,
    block_px: u32,
    scale: f64,
    jitter_mm: f64,
    pen_mm: f64,
    pen: u8,
    n: &mut usize,
    out: &mut Vec<Polyline>,
    pens_out: &mut Vec<u8>,
) {
    if strokes.is_empty() {
        return;
    }
    let plan = match order_mode {
        2 => scan_plan(strokes),
        3 => block_plan(strokes, block_px, scale),
        _ if strokes.len() > 1200 => block_plan(strokes, 80, scale),
        _ => nearest_plan(strokes),
    };
    for p in plan {
        let before = out.len();
        emit_stroke(
            &strokes[p.idx],
            p.rev,
            *n,
            2,
            jitter_mm,
            pen_mm,
            scale,
            out,
        );
        // 结构线可能叠两遍，一笔会吐出一根以上折线；它们都属于同一支笔。
        for _ in before..out.len() {
            pens_out.push(pen);
        }
        *n += 1;
    }
}



/// 调试用：把算法内部看到的调子图按 0..255 返回（深 = 调子高）。
#[allow(dead_code)]
pub(crate) fn field_tone(
    gray: &image::GrayImage,
    w: u32,
    h: u32,
    px_mm: f64,
    invert: bool,
) -> Vec<u8> {
    let f = Field::build(gray, w, h, px_mm, invert, 0.5);
    f.tone
        .iter()
        .map(|&t| (255.0 * (1.0 - t.clamp(0.0, 1.0))) as u8)
        .collect()
}

/// 调试用：把方向场按网格采样成 (x, y, 方向, 一致性)，用来判断「排线为什么乱」。
#[allow(dead_code)]
pub(crate) fn field_ticks(
    gray: &image::GrayImage,
    w: u32,
    h: u32,
    px_mm: f64,
    invert: bool,
    step_px: f64,
) -> Vec<(f64, f64, f64, f64)> {
    let f = Field::build(gray, w, h, px_mm, invert, 0.5);
    let mut out = Vec::new();
    let mut y = step_px * 0.5;
    while y < h as f64 {
        let mut x = step_px * 0.5;
        while x < w as f64 {
            let (a, c) = f.ang_at(x, y);
            out.push((x, y, a as f64, c as f64));
            x += step_px;
        }
        y += step_px;
    }
    out
}

/// 供预览 / 验收使用的「哪些像素算墨」——与追踪时用的是同一套判定。
#[allow(dead_code)]
pub(crate) fn ink_mask(
    gray: &image::GrayImage,
    w: u32,
    h: u32,
    threshold: f64,
    invert: bool,
) -> Vec<bool> {
    let ink = build_ink(gray, w, h, invert, threshold);
    ink.tone.iter().map(|&v| v >= ink.thr).collect()
}

/// 局部调子图：一小块在原稿里「有多暗」。
///
/// 这是素描复刻的关键，也是上一版最大的错：上一版把「原稿上每一根铅笔记号」
/// 都当成必须描的线，画出来等于复印——整页一样黑，明暗关系反而没了。
/// 素描的明暗是**用线条的疏密**堆出来的：暗部排满，亮部留白。
///
/// 所以先要有一张「明暗图」：把墨浓度按调子格取均值。格子必须够大
/// （几个毫米，远大于笔宽），否则一根线就能把整格填成「深色」——
/// 那又会退回「凡有线就画」的老路。
pub(crate) struct Tone {
    /// 每格归一化明暗 0..1（0 = 纸白，1 = 全画面上最深的一档）。
    pub cells: Vec<f32>,
    pub cw: usize,
    pub ch: usize,
    pub cell_mm: f64,
}

impl Tone {
    /// 某个毫米坐标处的调子（0 = 纸白，1 = 最深）。供外部按需查询。
    #[allow(dead_code)]
    pub fn at(&self, x_mm: f64, y_mm: f64) -> f32 {
        if self.cw == 0 || self.ch == 0 {
            return 0.0;
        }
        let cx = ((x_mm / self.cell_mm) as isize).clamp(0, self.cw as isize - 1) as usize;
        let cy = ((y_mm / self.cell_mm) as isize).clamp(0, self.ch as isize - 1) as usize;
        self.cells[cy * self.cw + cx]
    }
}


/// 建调子图：墨浓度 → 分格均值 → 分位拉伸成 0..1。
///
/// 拉伸用 15%/92% 分位而不是最大最小值：原稿的纸白和最深的一笔都是离群值，
/// 直接归一化会让整张图挤在中间调，明暗层次出不来。
pub(crate) fn tone_map(
    gray: &image::GrayImage,
    w: u32,
    h: u32,
    invert: bool,
    threshold: f64,
    scale: f64,
) -> Tone {
    let ink = build_ink(gray, w, h, invert, threshold);
    let h_mm = h as f64 * scale;
    // 一张 120mm 高的稿子分成 30 档，约 4mm 一格。
    let cell_mm = (h_mm / 30.0).clamp(1.5, 6.0);
    let cell_px = (cell_mm / scale.max(1e-9)).max(2.0);
    let cw = ((w as f64 / cell_px).ceil() as usize).max(1);
    let ch = ((h as f64 / cell_px).ceil() as usize).max(1);
    let mut sum = vec![0.0f32; cw * ch];
    let mut cnt = vec![0u32; cw * ch];
    for y in 0..h {
        let cy = ((y as f64 / cell_px) as usize).min(ch - 1);
        for x in 0..w {
            let cx = ((x as f64 / cell_px) as usize).min(cw - 1);
            let i = cy * cw + cx;
            sum[i] += ink.tone[(y * w + x) as usize];
            cnt[i] += 1;
        }
    }
    for i in 0..sum.len() {
        if cnt[i] > 0 {
            sum[i] /= cnt[i] as f32;
        }
    }
    let mut sorted = sum.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let q = |f: f64| -> f32 {
        let k = ((sorted.len() as f64 - 1.0) * f) as usize;
        sorted[k.min(sorted.len() - 1)]
    };
    let lo = q(0.15);
    let hi = q(0.92);
    let span = (hi - lo).max(1e-4);
    for v in sum.iter_mut() {
        *v = ((*v - lo) / span).clamp(0.0, 1.0);
    }
    Tone {
        cells: sum,
        cw,
        ch,
        cell_mm,
    }
}


/// 建墨浓度图。
///
/// 关键的一步是**用本图自己的纸白与最深墨做归一化**：扫描偏灰、照片有阴影时，
/// 绝对亮度阈值会把整片浅线判成纸；归一化之后，铅笔轻轻划过的那一层也还是墨。
fn build_ink(
    gray: &image::GrayImage,
    w: u32,
    h: u32,
    invert: bool,
    threshold: f64,
) -> Ink {
    let n = (w * h) as usize;
    let mut hist = [0u32; 256];
    for p in gray.pixels() {
        hist[p.0[0] as usize] = hist[p.0[0] as usize].saturating_add(1);
    }
    let total: u64 = n as u64;
    let pct = |frac: f64, from_top: bool| -> u8 {
        let target = (total as f64 * frac).max(1.0) as u64;
        let mut acc = 0u64;
        if from_top {
            for v in (0..256usize).rev() {
                acc += hist[v] as u64;
                if acc >= target {
                    return v as u8;
                }
            }
            255
        } else {
            for v in 0..256usize {
                acc += hist[v] as u64;
                if acc >= target {
                    return v as u8;
                }
            }
            0
        }
    };
    let paper = pct(0.01, true) as f32;
    let deep = pct(0.01, false) as f32;
    // 反差太小的图（几乎全白）没有笔迹可找。
    let span = (paper - deep).max(24.0);
    let mut tone = vec![0f32; n];
    for (i, p) in gray.pixels().enumerate() {
        let v = p.0[0] as f32;
        let t = if invert {
            (v - deep) / span
        } else {
            (paper - v) / span
        };
        tone[i] = t.clamp(0.0, 1.0);
    }
    // 不做模糊：1px 细线过一遍 3×3 均值，峰值只剩一半，浅线当场掉到阈值以下。
    // 纸纹只有几个灰阶（0.01 级），靠阈值就能挡；平滑留给追踪时的双线性采样。
    let t = threshold.clamp(0.02, 0.98) as f32;
    // 滑杆直接映射成「比纸深多少才算墨」：默认 0.55 → 0.084（铅笔的正常线条都在
    // 0.1 以上），推到 0.75 连最浅的起稿线也纳进来；纸纹只有 0.01~0.02，仍在下面。
    let thr = (0.155 - 0.13 * t).clamp(0.025, 0.30);
    let lum: Vec<u8> = gray.pixels().map(|p| p.0[0]).collect();
    let done = vec![false; n];
    Ink {
        done,
        tone,
        w,
        h,
        thr,
        weak: thr * 0.55,
        lum,
        blank: if invert { deep } else { paper },
        invert,
    }
}

/// 候选种子：墨脊上的局部极大点，按墨浓度从深到浅。
///
/// 从最深的开始追踪，等于「先画重笔」；而且重笔先把墨吃掉之后，
/// 浅笔的种子只会落在还没画过的地方。
fn seeds(ink: &Ink) -> Vec<(f32, u32)> {
    let (w, h) = (ink.w as i32, ink.h as i32);
    let mut out = Vec::new();
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let i = ink.idx(x as u32, y as u32);
            let v = ink.tone[i];
            if v < ink.thr {
                continue;
            }
            let mut is_max = true;
            for dy in -1..=1i32 {
                for dx in -1..=1i32 {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let ni = ink.idx((x + dx) as u32, (y + dy) as u32);
                    if ink.tone[ni] > v + 1e-4 {
                        is_max = false;
                    }
                }
            }
            if is_max {
                out.push((v, i as u32));
            }
        }
    }
    out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// 朝 `dir` 附近 `half` 弧度内扫一遍，挑「前方墨最多」的方向。
///
/// 这是整个追踪的方向来源：不看图像的全局走向，只看「沿着这个方向再走几像素，
/// 前面还有没有墨」。交叉点上两个方向都有墨，所以由「转得最少」来定胜负，
/// 笔尖就会直穿过去；曲线处墨本身在弯，扫出来自然跟着弯。
#[allow(clippy::too_many_arguments)]
fn dir_scan(
    ink: &Ink,
    p: (f64, f64),
    dir: (f64, f64),
    half: f64,
    n: i32,
    turn_cost: f64,
    min_gain: f64,
    tone_ref: f32,
) -> (f64, f64) {
    // 采样距离：2..7px。近处权重大，远一点的墨用来判「这条线是否继续」。
    const PROBE: [f64; 5] = [2.0, 3.2, 4.4, 5.8, 7.2];
    let base = dir.1.atan2(dir.0);
    let mut best = dir;
    let mut best_score = f64::NEG_INFINITY;
    let mut base_score = f64::NEG_INFINITY;
    for k in -n..=n {
        let a = base + half * k as f64 / n as f64;
        let d = (a.cos(), a.sin());
        let mut s = 0.0;
        let mut wsum = 0.0;
        // 「同一根笔迹的墨色是差不多的」——这是把两条交叉的线分开的唯一依据。
        // 更黑的一段墨不算更好的延续，否则浅排线一遇到深线就会被带跑。
        let cap = if tone_ref > 0.0 {
            (tone_ref * 1.35).max(0.12)
        } else {
            1.0
        };
        for (i, dist) in PROBE.iter().enumerate() {
            let w = 1.0 - i as f64 * 0.14;
            let v = ink.get(p.0 + d.0 * dist, p.1 + d.1 * dist);
            s += v.min(cap) as f64 * w;
            wsum += w;
        }
        // 转角代价：同等墨量的情况下优先直行，交叉点才不会被带偏。
        // 播种时没有「当前方向」，所以代价按 0 传进来。
        let turn = (a - base).abs();
        let score = s / wsum - turn_cost * turn;
        if k == 0 {
            base_score = score;
        }
        if score > best_score {
            best_score = score;
            best = d;
        }
    }
    // 墨量只多出一点点，不值得改变方向：宁可直行，也不要在交叉点拐到别的线上。
    if best_score - base_score < min_gain {
        return dir;
    }
    best
}

/// 播种时的初始方向：整圈扫一遍，哪边墨最多就往哪边走。
fn seed_dir(ink: &Ink, x: f64, y: f64) -> (f64, f64) {
    dir_scan(
        ink,
        (x, y),
        (1.0, 0.0),
        std::f64::consts::FRAC_PI_2,
        36,
        0.0,
        0.0,
        0.0,
    )
}

/// 沿法向的墨浓度剖面：返回 (墨脊横向偏移, 峰值, 半高宽/px)。
///
/// 两个细节决定了追踪会不会「串线」：
/// - 只在**离当前点最近的那个局部极大**上落位，而不是整个窗口的最大值。
///   排线之间只隔十几个像素，取全局最大就会一路跳到旁边那根线上。
/// - 宽度沿「值的下降方向」量到谷底为止（分水岭），两根挨着的线不会被量成一根。
fn profile(ink: &Ink, q: (f64, f64), n: (f64, f64), r: f64) -> (f64, f32, f32) {
    const DS: f64 = 0.4;
    let k = ((r / DS).ceil() as i32).clamp(2, 12);
    let mut vals = [0f32; 25];
    let mut gmax = -1.0f32;
    let mut gbest = 0i32;
    for j in -k..=k {
        let s = j as f64 * DS;
        let v = ink.get(q.0 + n.0 * s, q.1 + n.1 * s);
        vals[(j + k) as usize] = v;
        if v > gmax {
            gmax = v;
            gbest = j;
        }
    }
    let floor = ink.thr * 0.8;
    // 从 0 向两侧交替找最近的局部极大
    let mut pick = None;
    for d in 0..=k {
        let cand = if d == 0 { [0, 0] } else { [d, -d] };
        for j in cand {
            let v = vals[(j + k) as usize];
            let l = if j > -k { vals[(j - 1 + k) as usize] } else { 0.0 };
            let rr = if j < k { vals[(j + 1 + k) as usize] } else { 0.0 };
            if v >= l - 1e-6 && v >= rr - 1e-6 && v >= floor {
                pick = Some(j);
                break;
            }
        }
        if pick.is_some() {
            break;
        }
    }
    let bi = pick.unwrap_or(gbest);
    let peak = vals[(bi + k) as usize];
    let half = peak * 0.55;
    let mut lo = bi;
    let mut hi = bi;
    while lo > -k {
        let v = vals[(lo - 1 + k) as usize];
        if v < half || v > vals[(lo + k) as usize] + 1e-6 {
            break;
        }
        lo -= 1;
    }
    while hi < k {
        let v = vals[(hi + 1 + k) as usize];
        if v < half || v > vals[(hi + k) as usize] + 1e-6 {
            break;
        }
        hi += 1;
    }
    // 峰值位置用三点抛物线插值求。这里**不能**用「半高段重心」：
    // 交叉点上另一根线的墨会把半高段撑宽，重心被拽过去，追踪就会在每个
    // 交叉处绕一个小圈（画出来的排线是网眼状的）。
    let v0 = vals[(bi + k) as usize] as f64;
    let vm = if bi > -k {
        vals[(bi - 1 + k) as usize] as f64
    } else {
        v0
    };
    let vp = if bi < k {
        vals[(bi + 1 + k) as usize] as f64
    } else {
        v0
    };
    let den = vm - 2.0 * v0 + vp;
    let delta = if den.abs() > 1e-9 {
        (0.5 * (vm - vp) / den).clamp(-0.5, 0.5)
    } else {
        0.0
    };
    let off = (bi as f64 + delta) * DS;
    let width = ((hi - lo + 1) as f32 * DS as f32).max(0.8);
    (off, peak, width)
}

/// 跨过一个小断口：断口里必须有淡墨（纯白纸是留白，不能跨）。
fn jump(ink: &Ink, q: (f64, f64), d: (f64, f64)) -> Option<(f64, f64)> {
    let mut k = 1.0;
    while k <= GAP_PX {
        let p = (q.0 + d.0 * k, q.1 + d.1 * k);
        if ink.is_blank(p.0, p.1) {
            return None;
        }
        let v = ink.get(p.0, p.1);
        if v < ink.weak {
            return None;
        }
        if v >= ink.thr * 0.95 {
            return Some(p);
        }
        k += 0.5;
    }
    None
}

/// 从一点朝一个方向追一笔。
///
/// 三条纪律决定了「一笔」能不能真的连成一条：
/// - **转角限幅而不是急停**：1px 的墨脊位置是量化的，单步朝向天然会抖十几度，
///   一抖就收笔的话，整根排线会被切成几十段碎线。这里限幅后继续走，
///   真正的转角靠「一小段内累计转过 90°」来判。
/// - **交叉处不重新落位**：墨脊突然跑到一两像素外，说明踩到了另一根线上
///   （交叉点）。这时保持直行最多 3 步，等回到自己的墨脊再落位。
/// - **闭合轮廓只走一圈**：回到起点就收笔，不在同一根墨上打转。
fn trace(ink: &Ink, p0: (f64, f64), d0: (f64, f64), max_steps: usize) -> Trace {
    let mut pts = vec![p0];
    let mut widths = Vec::new();
    let mut tones = Vec::new();
    let n0 = (-d0.1, d0.0);
    let (_, pk0, wd0) = profile(ink, p0, n0, 4.0);
    widths.push(wd0);
    tones.push(pk0);
    let mut w_est = wd0.max(1.0) as f64;
    // 这一笔自己的墨色（跟着走慢慢更新，只用来判断「哪段墨才是自己」）。
    let mut tone_ref = pk0.max(0.05);
    let mut p = p0;
    let mut d = {
        let l = (d0.0 * d0.0 + d0.1 * d0.1).sqrt().max(1e-9);
        (d0.0 / l, d0.1 / l)
    };
    let mut faint = 0;
    let mut lost = 0;
    let mut turn_win: Vec<f64> = Vec::with_capacity(12);
    for _ in 0..max_steps {
        let n = (-d.1, d.0);
        let mut q = (p.0 + d.0 * STEP_PX, p.1 + d.1 * STEP_PX);
        if q.0 < 0.0 || q.1 < 0.0 || q.0 >= ink.w as f64 || q.1 >= ink.h as f64 {
            set_reason("out");
            break;
        }
        let r = (w_est * 0.5 + 1.5).clamp(1.6, 4.0);
        let (off, peak, width) = profile(ink, q, n, r);
        if peak < ink.thr * 0.9 {
            match jump(ink, q, d) {
                Some(q2) => q = q2,
                None => {
                    set_reason("noink");
                    break;
                }
            }
        }
        // 横向落位。墨脊跑到 1.2px 以外时先别跟——那里多半是交叉的另一根线，
        // 直行几像素就能回到自己的墨脊；连着 4 步都找不回来才收笔。
        // 单步横向修正限幅：修 1px 就是在造一个 45° 的折角。宁可用两三步慢慢贴回墨脊。
        //
        // 交叉点（墨带突然变宽 = 两根线叠在一起）一律**不落位，直穿过去**。
        // 这是密集排线区串线的主要入口：踩上交叉点时剖面被另一根线撑宽，
        // 峰值被拽过去，一两步之后笔迹就顺着那根线走了，纸上于是出现
        // 「斜线横穿竖排线」的乱网。
        let crossing = (width as f64) > w_est * 1.45;
        let o = if crossing {
            0.0
        } else if off.abs() <= 1.8 {
            lost = 0;
            off.clamp(-0.35, 0.35)
        } else {
            lost += 1;
            if lost > 3 {
                set_reason("lost");
                break;
            }
            0.0
        };
        let np = (q.0 + n.0 * o, q.1 + n.1 * o);
        if near_xy(np, p) < 0.2 {
            set_reason("stuck");
            break;
        }
        // 方向只由「前方还有没有墨」决定，单步最多转 MAX_TURN。
        // 必须在**落位之后**再扫：站在墨脊上扫出来的扇区才是对称的，
        // 否则每次都会多转一点去追墨脊，十几步下来就绕成螺旋。
        let nd = dir_scan(ink, np, d, MAX_TURN, 6, 4.0, 0.05, tone_ref);
        let turn = (d.0 * nd.1 - d.1 * nd.0).atan2(d.0 * nd.0 + d.1 * nd.1);
        turn_win.push(turn);
        if turn_win.len() > 16 {
            turn_win.remove(0);
        }
        let acc_turn: f64 = turn_win.iter().sum();
        if acc_turn.abs() > 2.2 && turn_win.len() >= 16 {
            set_reason("curl");
            break;
        }
        // 绕回起点：闭合轮廓（罐口、椭圆）走一圈就够，走第二圈是在同一根墨上打转。
        if pts.len() > 24 && near_xy(np, pts[0]) < 1.6 {
            set_reason("loop");
            break;
        }
        // 自交检测：回到自己 8~24 步前的位置 = 在绕圈。
        let n_back = pts.len();
        if n_back > 24 {
            let mut curls = false;
            for k in 8..=24.min(n_back - 1) {
                if near_xy(np, pts[n_back - 1 - k]) < 0.6 {
                    curls = true;
                    break;
                }
            }
            if curls {
                set_reason("cross");
                break;
            }
        }
        p = np;
        d = nd;
        pts.push(p);
        widths.push(width);
        tones.push(peak);
        w_est = w_est * 0.8 + width as f64 * 0.2;
        tone_ref = tone_ref * 0.92 + peak * 0.08;
        if peak < ink.weak {
            faint += 1;
            if faint >= 2 {
                set_reason("faint");
                break;
            }
        } else {
            faint = 0;
        }
    }
    if std::env::var("SKETCH_TRACE").is_ok() {
        eprintln!(
            "    trace p0=({:.0},{:.0}) d0=({:.2},{:.2}) steps={} reason={}",
            p0.0, p0.1, d0.0, d0.1,
            pts.len(),
            LAST_REASON.with(|r| *r.borrow())
        );
    }
    Trace { pts, widths, tones }
}

/// 切掉两端的「多出来的尾巴」。
///
/// 笔迹走到另一根线上时，沿法向的剖面会被横向穿过的那根墨撑宽——
/// 带宽突然变大就是这个信号，把这段切掉，线就不会在交叉处多出一截。
fn trim_ends(ink: &Ink, pts: &mut Vec<(f64, f64)>, ws: &mut Vec<f32>, ts: &mut Vec<f32>) {
    if pts.len() < 5 {
        return;
    }
    let med = quantile(ws, 0.5).max(0.9);
    let bad = |i: usize| ws[i] > med * 2.6 || ts[i] < ink.weak * 0.85;
    let mut a = 0usize;
    while a + 2 < pts.len() && bad(a) {
        a += 1;
    }
    let mut b = pts.len() - 1;
    while b > a + 2 && bad(b) {
        b -= 1;
    }
    if a > 0 {
        pts.drain(0..a);
        ws.drain(0..a);
        ts.drain(0..a);
    }
    let keep = (b + 1).saturating_sub(a);
    pts.truncate(keep);
    ws.truncate(keep);
    ts.truncate(keep);
}

/// 一条路径有多大比例压在「已经画过」的墨带上。
///
/// 判据和 `consume` 标 `done` 的口径一致（墨带核心半径），所以「画过的」一定
/// 被算成已覆盖、「没画过的」不会被算进去。不看采样点那一个像素，是因为追踪
/// 沿墨脊走会有 ±1px 的横向漂移：同一条 3px 墨带反复播种，就会得到三四条略微
/// 错开的等效笔迹，叠在纸上就是几根平行重影线。
fn covered_frac(ink: &Ink, pts: &[(f64, f64)], width: f64) -> f64 {
    let (wi, hi) = (ink.w as i32, ink.h as i32);
    let r = (width * 0.5 + 0.7).clamp(0.9, MAX_WIDTH_PX * 0.5 + 1.5);
    let core = r * 0.62 + 0.35;
    let stride = (pts.len() / 24).max(1);
    let mut covered = 0usize;
    let mut sampled = 0usize;
    for p in pts.iter().step_by(stride) {
        sampled += 1;
        let x0 = (p.0 - core).floor() as i32;
        let x1 = (p.0 + core).ceil() as i32;
        let y0 = (p.1 - core).floor() as i32;
        let y1 = (p.1 + core).ceil() as i32;
        'probe: for y in y0.max(0)..=y1.min(hi - 1) {
            for x in x0.max(0)..=x1.min(wi - 1) {
                let i = (y as u32 * ink.w + x as u32) as usize;
                if !ink.done[i] {
                    continue;
                }
                let dx = x as f64 - p.0;
                let dy = y as f64 - p.1;
                if (dx * dx + dy * dy).sqrt() <= core {
                    covered += 1;
                    break 'probe;
                }
            }
        }
    }
    if sampled == 0 {
        return 0.0;
    }
    covered as f64 / sampled as f64
}

/// 把一条笔迹占掉的墨扣掉，后续种子就不会再落在它身上。
fn consume(ink: &mut Ink, pts: &[(f64, f64)], ws: &[f32]) {
    let (wi, hi) = (ink.w as i32, ink.h as i32);
    for (p, &wd) in pts.iter().zip(ws.iter()) {
        let r = (wd as f64 * 0.5 + 0.7).clamp(0.9, MAX_WIDTH_PX * 0.5 + 1.5);
        let x0 = (p.0 - r).floor() as i32;
        let x1 = (p.0 + r).ceil() as i32;
        let y0 = (p.1 - r).floor() as i32;
        let y1 = (p.1 + r).ceil() as i32;
        for y in y0.max(0)..=y1.min(hi - 1) {
            for x in x0.max(0)..=x1.min(wi - 1) {
                let dx = x as f64 - p.0;
                let dy = y as f64 - p.1;
                let dist = (dx * dx + dy * dy).sqrt();
                if dist > r {
                    continue;
                }
                // 墨带核心整片标掉，边缘留着：旁边挨着的另一根线不会被误标。
                if dist <= r * 0.62 + 0.35 {
                    let i = (y as u32 * ink.w + x as u32) as usize;
                    ink.done[i] = true;
                }
            }
        }
    }
}

fn quantile(v: &[f32], q: f64) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s: Vec<f32> = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let i = ((s.len() as f64 - 1.0) * q.clamp(0.0, 1.0)).round() as usize;
    s[i.min(s.len() - 1)]
}

/// 端点保持不动的 1-2-1 平滑。
fn smooth_in_place(pts: &mut [(f64, f64)]) {
    if pts.len() < 3 {
        return;
    }
    let mut prev = pts[0];
    for i in 1..pts.len() - 1 {
        let cur = pts[i];
        let next = pts[i + 1];
        pts[i] = (
            (prev.0 + 2.0 * cur.0 + next.0) * 0.25,
            (prev.1 + 2.0 * cur.1 + next.1) * 0.25,
        );
        prev = cur;
    }
}

// ===========================================================================
// 调子排线渲染：把一张「有明暗的画」画成铅笔素描
// ===========================================================================
//
// 为什么需要另一条通路：沿墨脊追踪适合**线稿**（白纸 + 细线），但铅笔素描
// 是**色调稿**——脸几乎没有线，全靠渐变；头发是一整片暗部。这种图里没有
// 「墨脊」可找：追踪器在渐变上取不到峰值，只能随机游走，在暗部里越绕越乱。
// 上一版把用户的女青年肖像画成一团乱涂，根因就在这里。
//
// 素描真正的画法是**用线条疏密堆明暗**：
// 1. 先算一张调子图（模糊到「一眼看一块」的尺度），它决定每块地方要多黑；
// 2. 每块地方线的间距 = 笔宽 / 调子：调子 0.9 时线几乎挨着，调子 0.2 时
//    隔好几个笔宽才一根，调子低于起笔门槛就**留白**；
// 3. 线的走向沿等照度线（贴着形体的转折走），所以苹果的弧面、脸颊的过渡
//    会画出顺着形体的排线，而不是横七竖八的影子线；
// 4. 眼睛、鼻翼、唇线这类「形」靠边缘线单独勾一遍，比排线重，保证认得出人。
//
// 这样笔尖的墨量天然守恒：黑的地方是线多，不是笔重；亮的地方是留白。

/// 方向场不可靠（纯平区域）时用的排线方向：右上 50°，最像手画的斜排线。
/// 调子场：一张画「哪里多黑、往哪个方向排线」的全部信息。
struct Field {
    w: usize,
    h: usize,
    /// 毫米 / 像素
    px_mm: f64,
    /// 模糊后的调子 0..1（0 = 纸白，1 = 全画最深）
    tone: Vec<f32>,
    /// 半分辨率的「倍角方向」：2θ 的 (cos, sin)。用倍角是为了绕开 θ 与 θ+π
    /// 在数值上不连续的麻烦——插值不会在 0/π 交界处翻转。
    ac2: Vec<f32>,
    as2: Vec<f32>,
    /// 方向一致性 0..1
    coh: Vec<f32>,
    aw: usize,
    ah: usize,
    /// 调子归一化的纸白与黑白跨度（灰阶 0..255），轮廓层复用它算细节调子。
    paper: f32,
    span: f32,
    /// **暗脊响应**：这一像素有多像「一根画出来的铅笔线」。
    ///
    /// 判据是 Hessian 的两个特征值「一大一小」——法向曲率很大、沿脊几乎不变。
    /// 这正是铅笔线与柔和渐变的本质区别，也是「只描线、不重新编明暗」的根据：
    /// 一块抹开的阴影两个方向曲率相近，响应接近 0，绝不会被当成线。
    ridge: Vec<f32>,
    /// **归一化脊响应** 0..1 = `ridge / 局部上包络`。
    ///
    /// 一张素描里头发区的脊响应天生比脸颊强一个量级。拿全局分位去切，前 24% 会
    /// 被头发占满，脸上的浅笔触排都排不进来——实测脸颊整片空白、五官全丢。
    /// 除掉局部上包络之后，「弱的地方它自己那根最强的线」也能进得了榜。
    nridge: Vec<f32>,
    /// **局部脊方向场**（沿脊走向，倍角表示）：`rdc = cos2θ`、`rds = sin2θ`。
    ///
    /// 和 `ac2/as2` 是两套东西，别混用：`ac2/as2` 是结构张量的等照度线方向，
    /// 平滑到 2.5mm，那是给「一片区域一个走向」的排线用的；这个是 `rangle`
    /// 按响应加权、只做 0.6mm 小范围平均的结果，跟得住一根发丝。
    /// 拿前者跑流线，实测方向差中位 52°——走的是一片区域的平均走向，
    /// 不是那根线自己的走向。
    rdc: Vec<f32>,
    rds: Vec<f32>,
    /// 脊方向可信度 0..1：附近确实有脊、方向也一致才接近 1。
    rcoh: Vec<f32>,
    /// **平滑后的脊方向**（倍角，约 1.2mm 半径）：`sdc/sds` + 可信度 `scoh`。
    ///
    /// `rdc/rds` 只适合「跟着一根发丝走」的提取；拿它给合成排线逐点取方向，
    /// 相邻像素会来回翻 30°以上（素描原稿方向差中位 39°，画出来像草）。
    /// 这里按 rcoh 加权、在大一点的窗口里平均，给「成片笔触」一个稳定走向；
    /// 无脊处可信度自然接近 0，交回基准角。
    sdc: Vec<f32>,
    sds: Vec<f32>,
    scoh: Vec<f32>,
    /// **成片排线方向**（倍角，约 6mm 半径）：`bdc/bds` + 可信度 `bcoh`。
    ///
    /// 脸的浅排线每根都很弱，1.2mm 平均后仍然一根一个方向；跨 6mm 统计
    /// 才得到「这一块排线朝哪儿」。合成排线用它，整块同向，才像手画的调子。
    bdc: Vec<f32>,
    bds: Vec<f32>,
    bcoh: Vec<f32>,
}

impl Field {
    fn build(
        gray: &image::GrayImage,
        w: u32,
        h: u32,
        px_mm: f64,
        invert: bool,
        pen_mm: f64,
    ) -> Field {
        let (w, h) = (w as usize, h as usize);
        let n = w * h;
        let src = gray.as_raw();
        let mut raw = vec![0f32; n];
        let mut hist = [0u32; 256];
        for i in 0..n {
            let v = if invert { 255u8.saturating_sub(src[i]) } else { src[i] };
            raw[i] = v as f32;
            hist[v as usize] += 1;
        }
        // 纸白 = **亮侧里第一个「够大」的峰**。
        //
        // 只取全局众数不行：整图偏暗时（大面积暗部占多数）众数落到暗部上，
        // 纸白被当成黑，整张图直接变空白。取最亮值也不行：打光/高光比纸还亮，
        // 一归一化整张脸都成了「灰」，脸的亮部也被排满线。
        // 折中：先找直方图的最大峰，再从亮端往下找第一个「达到最大峰一半」的
        // 灰阶——它就是纸（或照片里的底色），高光的峰通常远小于它。
        let mut peak = 0u64;
        for i in 0usize..256 {
            let a = i.saturating_sub(2);
            let b = (i + 2).min(255);
            let s: u64 = hist[a..=b].iter().map(|&v| v as u64).sum();
            if s > peak {
                peak = s;
            }
        }
        let mut paper_i = 255usize;
        for i in (0usize..256).rev() {
            let a = i.saturating_sub(2);
            let b = (i + 2).min(255);
            let s: u64 = hist[a..=b].iter().map(|&v| v as u64).sum();
            if s * 2 >= peak {
                paper_i = i;
                break;
            }
        }
        let paper = paper_i as f32;
        // 「实黑」取暗端 6% 分位：素描里最深的头发/瞳孔大约占画面这么多。
        // 取 0.4% 分位会被极少数噪点拉低，整幅画的调子被压扁——头发只剩 0.45，
        // 排线按中调排，整片头发画不黑（这是"画不出明暗"的直接原因）。
        let cut = (n as u64 * 6 / 100).max(1);
        let mut acc = 0u64;
        let mut deep = 0f32;
        for (i, &c) in hist.iter().enumerate() {
            acc += c as u64;
            if acc >= cut {
                deep = i as f32;
                break;
            }
        }
        let span = (paper - deep).max(16.0);
        for v in raw.iter_mut() {
            *v = ((paper - *v) / span).max(0.0);
        }

        // 笔触提取要在**未做任何平滑**的调子图上算：铅笔笔触只有 1~3px 宽，
        // 先糊一遍就抹平了。平滑由 Hessian 在每个尺度里自己做。
        let scales = ridge_scales(px_mm, pen_mm);
        let (ridge, rangle) = ridge_field(&raw, w, h, &scales);
        // 局部上包络 → 归一化响应。半径取 2.2mm（和调子同尺度）：再小会退化成
        // 「自己跟自己比」，噪声像素也能拿 1.0；再大就失去局部性了。
        let r_env = ((2.2 / px_mm).round() as usize).clamp(2, 60);
        let env = box_max(&ridge, w, h, r_env);
        let mut nridge = vec![0f32; n];
        for i in 0..n {
            nridge[i] = if ridge[i] > 0.0 {
                (ridge[i] / env[i].max(1e-9)).min(1.0)
            } else {
                0.0
            };
        }
        // ——— 局部脊方向场 ———
        //
        // `rangle` 是逐像素的沿脊方向，直接用太抖（一根发丝上相邻像素能差
        // 30°）。按**脊响应**加权、在一个小半径里平均：响应大的像素说了算，
        // 没脊的地方权重是 0，自然不会把方向带偏。
        //
        // 半径跟着**最小检测尺度**走，不跟着毫米走。实测这条最狠：取 0.6mm
        // （这张图上是 5px、11×11 窗口）时，相邻发丝（间距 4~9px）的方向被平均到
        // 一起，方向差中位 39.8°；取 3×3 时降到 18.7°，和基准稿 15.7° 一个量级。
        let rd = (scales[0].round() as usize).clamp(1, 4);
        let mut cx = vec![0f32; n];
        let mut cy = vec![0f32; n];
        let mut cw = vec![0f32; n];
        for i in 0..n {
            if ridge[i] <= 0.0 {
                continue;
            }
            // 倍角：θ 与 θ+π 是同一个走向，平均时必须先翻成 2θ，
            // 否则两头的线会互相抵消。
            let a2 = 2.0 * rangle[i];
            cx[i] = ridge[i] * a2.cos();
            cy[i] = ridge[i] * a2.sin();
            cw[i] = ridge[i];
        }
        let cx = box_blur(&cx, w, h, rd);
        let cy = box_blur(&cy, w, h, rd);
        let cw = box_blur(&cw, w, h, rd);
        let mut rdc = vec![1f32; n];
        let mut rds = vec![0f32; n];
        let mut rcoh = vec![0f32; n];
        for i in 0..n {
            let m = (cx[i] * cx[i] + cy[i] * cy[i]).sqrt();
            if m > 1e-9 {
                rdc[i] = cx[i] / m;
                rds[i] = cy[i] / m;
                // 权重和 = 附近脊的总强度；向量和的模 = 其中「方向一致」的那部分。
                // 两者之比就是相干性，纯噪声邻域接近 0。
                rcoh[i] = (m / cw[i].max(1e-9)).clamp(0.0, 1.0);
            }
        }
        // 调子必须模糊到「一眼看一块」的尺度（约 1.4mm），不能只到发丝级：
        // 头发在 0.5mm 尺度上看是「深发丝 + 亮缝」，平均下来只有 0.5，排线就会
        // 按中调去排，整片头发变浅。人看的是**整块的明暗**，所以调子取大尺度。
        // 方向场反过来要用小尺度（见下面 detail/张量），才跟得住发丝与形体转折。
        let r_tone = ((2.2 / px_mm).round() as usize).clamp(2, 60);
        let tone = box_blur(&raw, w, h, r_tone);
        // 结构线要的是「形」的边界，不是纸纹：先模糊到 0.35mm 再求梯度。
        // 用 1~2px 的锐图找边，会把原稿的铅笔颗粒、纸纹全当成结构线，
        // 画出来就是满脸长划痕。
        let r_detail = ((0.35 / px_mm).round() as usize).clamp(1, 12);
        let detail = box_blur(&raw, w, h, r_detail);
        // 结构张量只在半分辨率上算：方向场本来就是平滑量，全分辨率纯属浪费。
        let aw = (w + 1) / 2;
        let ah = (h + 1) / 2;
        let mut jxx = vec![0f32; aw * ah];
        let mut jxy = vec![0f32; aw * ah];
        let mut jyy = vec![0f32; aw * ah];
        for ay in 0..ah {
            for ax in 0..aw {
                let x = ax * 2;
                let y = ay * 2;
                let x2 = (x + 2).min(w - 1);
                let x0 = x.saturating_sub(2);
                let y2 = (y + 2).min(h - 1);
                let y0 = y.saturating_sub(2);
                let gx = detail[y * w + x2] - detail[y * w + x0];
                let gy = detail[y2 * w + x] - detail[y0 * w + x];
                let i = ay * aw + ax;
                jxx[i] = gx * gx;
                jxy[i] = gx * gy;
                jyy[i] = gy * gy;
            }
        }
        // 方向场要平滑到「一片区域一个走向」：1mm 量级。平滑不够时相邻像素
        // 的走向各说各话，排线就会互相交叉，织成一张网。
        let r2 = ((1.2 / px_mm / 2.0).round() as usize).clamp(1, 24);
        let jxx = box_blur(&jxx, aw, ah, r2);
        let jxy = box_blur(&jxy, aw, ah, r2);
        let jyy = box_blur(&jyy, aw, ah, r2);
        // 第二遍按「一致性加权」做大尺度平均（约 2.5mm）。
        //
        // 为什么要加权平均而不是直接用局部方向：布料褶皱、抹开的阴影这类地方
        // 张量是各向同性的，局部方向纯属噪声；而它旁边的形体方向是可信的。
        // 按一致性加权之后，噪声区会自然「继承」邻居的走向，整片区域的排线
        // 于是连成一个方向——这才是手画的样子。权重和同时给出一个可信度，
        // 可信度低的区域退回统一斜排线也不会跟旁边打架。
        let mut ux = vec![0f32; aw * ah];
        let mut uy = vec![0f32; aw * ah];
        let mut wt = vec![0f32; aw * ah];
        for i in 0..aw * ah {
            let (a, b, c) = (jxx[i], jxy[i], jyy[i]);
            let m = ((a - c) * (a - c) + 4.0 * b * b).sqrt();
            let s = (a + c).max(1e-12);
            let wgt = (m / s).clamp(0.0, 1.0);
            // 结构张量的主方向是**梯度**方向；排线要沿等照度线，差 90°，
            // 在倍角上就是整体取反。
            ux[i] = -((a - c) / m.max(1e-12)) * wgt;
            uy[i] = -(2.0 * b / m.max(1e-12)) * wgt;
            wt[i] = wgt;
        }
        let r3 = ((2.5 / px_mm / 2.0).round() as usize).clamp(1, 48);
        let ux = box_blur(&ux, aw, ah, r3);
        let uy = box_blur(&uy, aw, ah, r3);
        let wt = box_blur(&wt, aw, ah, r3);
        let mut ac2 = vec![0f32; aw * ah];
        let mut as2 = vec![0f32; aw * ah];
        let mut coh = vec![0f32; aw * ah];
        for i in 0..aw * ah {
            let mag = (ux[i] * ux[i] + uy[i] * uy[i]).sqrt();
            if mag > 1e-6 {
                ac2[i] = ux[i] / mag;
                as2[i] = uy[i] / mag;
                coh[i] = (mag / wt[i].max(1e-6)).clamp(0.0, 1.0);
            } else {
                ac2[i] = 1.0;
                as2[i] = 0.0;
                coh[i] = 0.0;
            }
        }
        // 给合成排线用的「成片走向」：把可信度加权的脊方向再平均到 1.2mm。
        // 半径不能到 2.5mm——那会把相邻的两缕头发并成一个方向。
        let rs = ((1.2 / px_mm).round() as usize).clamp(1, 24);
        let mut scx = vec![0f32; n];
        let mut scy = vec![0f32; n];
        let mut scw = vec![0f32; n];
        for i in 0..n {
            if rcoh[i] <= 0.0 {
                continue;
            }
            scx[i] = rdc[i] * rcoh[i];
            scy[i] = rds[i] * rcoh[i];
            scw[i] = rcoh[i];
        }
        let scx = box_blur(&scx, w, h, rs);
        let scy = box_blur(&scy, w, h, rs);
        let scw = box_blur(&scw, w, h, rs);
        let mut sdc = vec![1f32; n];
        let mut sds = vec![0f32; n];
        let mut scoh = vec![0f32; n];
        for i in 0..n {
            let m = (scx[i] * scx[i] + scy[i] * scy[i]).sqrt();
            if m > 1e-9 {
                sdc[i] = scx[i] / m;
                sds[i] = scy[i] / m;
                scoh[i] = (m / scw[i].max(1e-9)).clamp(0.0, 1.0);
            }
        }
        // 成片排线方向（约 6mm）：脸上那种「浅而密」的排线每根都很弱，
        // 1.2mm 平均仍会一根一个方向，合成排线照它画就会互相打架。
        // 跨 6mm 统计后，同一块排线区只剩一个主方向；置信度不够的平坦区
        // 不采用（见 stroke_dir），退回手绘基准角，整块保持同向。
        let rb = ((6.0 / px_mm).round() as usize).clamp(2, 96);
        let mut bcx = vec![0f32; n];
        let mut bcy = vec![0f32; n];
        let mut bcw = vec![0f32; n];
        for i in 0..n {
            if scoh[i] <= 0.0 {
                continue;
            }
            bcx[i] = sdc[i] * scoh[i];
            bcy[i] = sds[i] * scoh[i];
            bcw[i] = scoh[i];
        }
        let bcx = box_blur(&bcx, w, h, rb);
        let bcy = box_blur(&bcy, w, h, rb);
        let bcw = box_blur(&bcw, w, h, rb);
        let mut bdc = vec![1f32; n];
        let mut bds = vec![0f32; n];
        let mut bcoh = vec![0f32; n];
        for i in 0..n {
            let m = (bcx[i] * bcx[i] + bcy[i] * bcy[i]).sqrt();
            if m > 1e-9 {
                bdc[i] = bcx[i] / m;
                bds[i] = bcy[i] / m;
                bcoh[i] = (m / bcw[i].max(1e-9)).clamp(0.0, 1.0);
            }
        }
        Field {
            w,
            h,
            px_mm,
            tone,
            ac2,
            as2,
            coh,
            aw,
            ah,
            paper,
            span,
            ridge,
            nridge,
            rdc,
            rds,
            rcoh,
            sdc,
            sds,
            scoh,
            bdc,
            bds,
            bcoh,
        }
    }

    fn tone_at(&self, x: f64, y: f64) -> f32 {
        bil(&self.tone, self.w, self.h, x, y)
    }

    /// (排线方向, 一致性)
    fn ang_at(&self, x: f64, y: f64) -> (f32, f32) {
        let hx = (x * 0.5 - 0.5).max(0.0);
        let hy = (y * 0.5 - 0.5).max(0.0);
        let x0 = (hx as usize).min(self.aw - 1);
        let y0 = (hy as usize).min(self.ah - 1);
        let x1 = (x0 + 1).min(self.aw - 1);
        let y1 = (y0 + 1).min(self.ah - 1);
        let fx = (hx - x0 as f64) as f32;
        let fy = (hy - y0 as f64) as f32;
        let pick = |v: &[f32]| {
            let a = v[y0 * self.aw + x0];
            let b = v[y0 * self.aw + x1];
            let c = v[y1 * self.aw + x0];
            let d = v[y1 * self.aw + x1];
            (a * (1.0 - fx) + b * fx) * (1.0 - fy) + (c * (1.0 - fx) + d * fx) * fy
        };
        let c2 = pick(&self.ac2);
        let s2 = pick(&self.as2);
        (0.5 * s2.atan2(c2), pick(&self.coh))
    }

    /// (成片笔触走向, 可信度)：平滑到约 1.2mm 的脊方向。
    fn smooth_ridge_ang_at(&self, x: f64, y: f64) -> (f32, f32) {
        let c2 = bil(&self.sdc, self.w, self.h, x, y);
        let s2 = bil(&self.sds, self.w, self.h, x, y);
        (0.5 * s2.atan2(c2), bil(&self.scoh, self.w, self.h, x, y))
    }

    /// (整块排线走向, 可信度)：平滑到约 6mm 的脊方向，用于浅调合成排线。
    fn broad_ridge_ang_at(&self, x: f64, y: f64) -> (f32, f32) {
        let c2 = bil(&self.bdc, self.w, self.h, x, y);
        let s2 = bil(&self.bds, self.w, self.h, x, y);
        (0.5 * s2.atan2(c2), bil(&self.bcoh, self.w, self.h, x, y))
    }

}


/// 可分离均值模糊（边界按边缘像素延拓）。O(N)，与半径无关。
/// 分离式滑动窗口**最大值**，用来求脊响应的局部上包络。
///
/// 用途见 `Field::nridge`：一张素描里头发区的脊响应天生比脸颊强一个量级，
/// 拿全局分位去切，前 24% 会被头发占满，脸上那些浅笔触排都排不进来。
/// 取局部（约 2.2mm，和调子同尺度）最大值做分母，弱的地方才有机会。
fn box_max(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    if r == 0 || w == 0 || h == 0 {
        return src.to_vec();
    }
    let mut tmp = vec![0f32; w * h];
    for y in 0..h {
        let row = &src[y * w..y * w + w];
        for x in 0..w {
            let a = x.saturating_sub(r);
            let b = (x + r).min(w - 1);
            let mut m = 0f32;
            for k in a..=b {
                m = m.max(row[k]);
            }
            tmp[y * w + x] = m;
        }
    }
    let mut out = vec![0f32; w * h];
    for x in 0..w {
        for y in 0..h {
            let y0 = y.saturating_sub(r);
            let y1 = (y + r).min(h - 1);
            let mut m = 0f32;
            for k in y0..=y1 {
                m = m.max(tmp[k * w + x]);
            }
            out[y * w + x] = m;
        }
    }
    out
}

fn box_blur(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    if r == 0 || w == 0 || h == 0 {
        return src.to_vec();
    }
    let ri = r as isize;
    let cnt = (2 * r + 1) as f32;
    let mut tmp = vec![0f32; src.len()];
    for y in 0..h {
        let row = &src[y * w..y * w + w];
        let mut acc = 0f32;
        for k in -ri..=ri {
            acc += row[k.clamp(0, w as isize - 1) as usize];
        }
        for x in 0..w {
            tmp[y * w + x] = acc / cnt;
            let add = (x as isize + ri + 1).clamp(0, w as isize - 1) as usize;
            let rem = (x as isize - ri).clamp(0, w as isize - 1) as usize;
            acc += row[add] - row[rem];
        }
    }
    let mut out = vec![0f32; src.len()];
    for x in 0..w {
        let mut acc = 0f32;
        for k in -ri..=ri {
            acc += tmp[k.clamp(0, h as isize - 1) as usize * w + x];
        }
        for y in 0..h {
            out[y * w + x] = acc / cnt;
            let add = (y as isize + ri + 1).clamp(0, h as isize - 1) as usize;
            let rem = (y as isize - ri).clamp(0, h as isize - 1) as usize;
            acc += tmp[add * w + x] - tmp[rem * w + x];
        }
    }
    out
}

/// 双线性取样（越界按边缘夹取）。
/// 小核高斯模糊（可分离）。
///
/// 不用 `box_blur`：Hessian 要的是 1px 量级的尺度，而 box 核半宽只能是整数，
/// σ=0.75 会被近似成 σ=1.0，细笔触的响应一下被抹掉几成。
fn gauss_blur(src: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    if w == 0 || h == 0 || sigma <= 0.05 {
        return src.to_vec();
    }
    let r = (sigma * 3.0).ceil().max(1.0) as isize;
    let mut k: Vec<f32> = (0..=(2 * r))
        .map(|j| {
            let d = j as f32 - r as f32;
            (-d * d / (2.0 * sigma * sigma)).exp()
        })
        .collect();
    let s: f32 = k.iter().sum();
    for v in k.iter_mut() {
        *v /= s;
    }
    let mut tmp = vec![0f32; w * h];
    for y in 0..h {
        let row = &src[y * w..y * w + w];
        for x in 0..w {
            let mut acc = 0f32;
            for (j, &kv) in k.iter().enumerate() {
                let xi = (x as isize + j as isize - r).clamp(0, w as isize - 1) as usize;
                acc += kv * row[xi];
            }
            tmp[y * w + x] = acc;
        }
    }
    let mut out = vec![0f32; w * h];
    for x in 0..w {
        for y in 0..h {
            let mut acc = 0f32;
            for (j, &kv) in k.iter().enumerate() {
                let yi = (y as isize + j as isize - r).clamp(0, h as isize - 1) as usize;
                acc += kv * tmp[yi * w + x];
            }
            out[y * w + x] = acc;
        }
    }
    out
}

/// 检测尺度跟着**笔宽**走，不跟像素走。
///
/// 这一条是整个笔触提取能不能看的关键：0.5mm 笔在 120mm 高的图上占 9.6px，
/// 而原稿发丝只有 1~2px 宽。用 1px 的尺度去检测，抓到的是「比笔细十倍」的
/// 东西，逐根描出来必然又短又碎、还会互相糊成一团黑——这正是"不像"的样子。
/// 尺度提到笔宽量级之后，检测到的是「一笔画得下的一缕」，才是能落到纸上的东西。
/// 更细的层次留给调子处理，不硬描。
fn ridge_scales(px_mm: f64, pen_mm: f64) -> [f32; 2] {
    let pen_px = (pen_mm / px_mm).max(1.0);
    // 最小尺度跟着**笔宽**走，不是跟着像素走：0.5mm 的笔在 120mm 幅面上占 9.6px，
    // 而原稿里一根发丝只有 1~2px。用 1px 的尺度去检测，抓到的是「比笔细十倍」
    // 的东西，逐根描出来必然又短又碎，还糊成一团黑。
    //
    // **只取两档，不要第三档**。这一点是实测逼出来的：这批素描的排线间距
    // 实测 0.49~0.99mm（约 4~9px），而第三档 σ=3×s0≈2.4px 的高斯核半径已经
    // 盖到了 5px——正好跨到相邻的排线上，两根线被糊成一根，脊位置还偏移。
    // 实测同一张图：两档 2791 条 / 中位长 16px / 掩码 30.8%；三档 1288 条 /
    // 中位长 10px / 掩码 37.9%；再加 σ=4 只剩 795 条 / 7px。大的那一档不但
    // 没抓到「更粗的线」，反而把细节全糊掉了。
    let s0 = (pen_px * 0.11).max(0.8).min(1.6) as f32;
    [s0, s0 * 2.0]
}

/// 多尺度 Hessian 暗脊检测：从原稿里找出「真正画出来的铅笔笔触」。
///
/// **为什么用 Hessian 而不是梯度**：梯度只说明「这里有明暗变化」。可一块抹开的
/// 阴影、布料的一道软褶皱，梯度也不小——按梯度找边，这些全会被当成线。
/// 铅笔线的特征不在于「有变化」，而在于**只在一个方向变化**：法向曲率很大、
/// 沿脊几乎不变。这个「各向异性」就是线状结构与面状明暗的分水岭，
/// 所以抹开的阴影永远不会被描成线，不用另设「画线还是画明暗」的开关。
///
/// 尺度取 0.75 / 1.5 / 3.0 px：细排线约 1px、发丝与脸部轮廓 2~3px、眼线与
/// 睫毛更粗。取各尺度最大响应，乘 σ² 归一（二阶导 ∝ 1/σ²）让三个尺度可比。
fn ridge_field(src: &[f32], w: usize, h: usize, scales: &[f32]) -> (Vec<f32>, Vec<f32>) {
    let n = w * h;
    let mut best = vec![0f32; n];
    let mut ang = vec![0f32; n];
    if w < 8 || h < 8 {
        return (best, ang);
    }
    // 各向异性下限：|λ2| 要比 |λ1| 明显大。0.30 即 λ2 至少是 λ1 的 1.9 倍——
    // 低于这个值就是一块圆形的暗斑（抹开的阴影），不是线。
    let aniso_min = 0.30f32;
    // 先翻成「亮度」再算。调子场里纸是 0、墨是正数，与亮度图的符号正好相反；
    // 不翻的话「暗线」会落在 λ1 上而不是 λ2 上——判据 `l2 > 0` 就永远不成立，
    // 一根线都提取不到（这个坑踩过一次，整幅直接空白）。
    let bri: Vec<f32> = src.iter().map(|&v| -v).collect();
    for &s in scales {
        let f = gauss_blur(&bri, w, h, s);
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                let i = y * w + x;
                let fxx = f[i - 1] - 2.0 * f[i] + f[i + 1];
                let fyy = f[i - w] - 2.0 * f[i] + f[i + w];
                let fxy = (f[i - w + 1] - f[i - w - 1] - f[i + w + 1] + f[i + w - 1]) * 0.25;
                let tr = fxx + fyy;
                let disc = (0.25 * (fxx - fyy) * (fxx - fyy) + fxy * fxy).sqrt();
                let l1 = 0.5 * tr - disc;
                let l2 = 0.5 * tr + disc;
                // 亮度图里「暗线」是局部极小 → 法向二阶导为正 → λ2 > 0。
                if l2 <= 0.0 {
                    continue;
                }
                let (a2, a1) = (l2.abs(), l1.abs());
                if (a2 - a1) / (a2 + a1 + 1e-9) < aniso_min {
                    continue;
                }
                let r = l2 * s * s;
                if r > best[i] {
                    best[i] = r;
                    // λ1 的特征向量就是沿脊方向
                    ang[i] = fxy.atan2(fxx - l2);
                }
            }
        }
    }
    (best, ang)
}

/// 折线平滑（滑动平均）+ 等距重采样，去掉八邻域走出来的台阶。
fn smooth_poly(pts: &[(f64, f64)], r: usize) -> Vec<(f64, f64)> {
    let n = pts.len();
    if n < 3 || r == 0 {
        return pts.to_vec();
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let lo = i.saturating_sub(r);
        let hi = (i + r).min(n - 1);
        let mut sx = 0.0;
        let mut sy = 0.0;
        for p in &pts[lo..=hi] {
            sx += p.0;
            sy += p.1;
        }
        let k = (hi - lo + 1) as f64;
        out.push((sx / k, sy / k));
    }
    // 端点别被拉进来（滑窗在端点只取了一半，会被拽向内侧）
    out[0] = pts[0];
    out[n - 1] = pts[n - 1];
    out
}

/// Ramer–Douglas–Peucker 抽稀：把平滑后的密集点压成少量拐点。
/// 投影笔走的是直线段，1px 一个点没有任何意义，只会把 G-code 撑大十倍。
fn rdp(pts: &[(f64, f64)], eps: f64) -> Vec<(f64, f64)> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let mut keep = vec![false; pts.len()];
    keep[0] = true;
    keep[pts.len() - 1] = true;
    let mut stack: Vec<(usize, usize)> = vec![(0, pts.len() - 1)];
    while let Some((a, b)) = stack.pop() {
        if b <= a + 1 {
            continue;
        }
        let (p, q) = (pts[a], pts[b]);
        let (dx, dy) = (q.0 - p.0, q.1 - p.1);
        let l = (dx * dx + dy * dy).sqrt();
        let mut best = 0.0f64;
        let mut bi = a;
        for (k, &c) in pts.iter().enumerate().take(b).skip(a + 1) {
            let d = if l < 1e-9 {
                ((c.0 - p.0).powi(2) + (c.1 - p.1).powi(2)).sqrt()
            } else {
                ((c.0 - p.0) * dy - (c.1 - p.1) * dx).abs() / l
            };
            if d > best {
                best = d;
                bi = k;
            }
        }
        if best > eps {
            keep[bi] = true;
            stack.push((a, bi));
            stack.push((bi, b));
        }
    }
    pts.iter()
        .enumerate()
        .filter(|(i, _)| keep[*i])
        .map(|(_, &p)| p)
        .collect()
}

/// 沿方向场走一趟流线（`sgn` 决定往哪头，另一头由调用者再走一遍）。
///
/// 返回 `(点串, 重叠比例)`。后者是「这一趟走过的像素里已被别人画过」的比例，
/// 调用方靠它决定这条线要不要——见 `ridge_strokes` 里的「不画第二遍」。
///
/// 三个停止条件，缺一不可：
/// - **响应掉了**（`miss > gap`）：这里没有线了；
/// - **走进别人已画的地方**（`occ > gap`）：见上层注释，少了它墨量会到 6.5 倍；
/// - 走出画布。
///
/// 写成独立函数而不是闭包，是因为 `covered` 在调用方还要被 `mark_blocked`
/// 可变借用——闭包会一路持有不可变借用到函数结束，编译不过。
fn stream_walk(
    f: &Field,
    covered: &[u8],
    bdx: &[f32],
    bdy: &[f32],
    visit: &mut [u32],
    gen: u32,
    stop: &mut [usize; 8],
    turn_max: f64,
    x0: f64,
    y0: f64,
    sgn: f64,
    thr_lo: f32,
    coh_min: f32,
    gap: usize,
    occ_max: usize,
    snap: f64,
    inertia: f64,
    max_steps: usize,
) -> (Vec<(f64, f64)>, f64) {
    let (w, h) = (f.w, f.h);
    let mut pts: Vec<(f64, f64)> = Vec::new();
    let (mut x, mut y) = (x0, y0);
    let mut pdir: Option<(f64, f64)> = None;
    let (mut miss, mut occ) = (0usize, 0usize);
    let mut n_hit = 0usize;
    // 累积转角（弧度）：走一步转了多少度，累加起来。
    //
    // 用来挡**噪声卷出来的小圈**。方向场在弱响应区基本是噪声，流线顺着它一路
    // 小角度偏转，走个 20 来像素就绕回自己踩过的像素——实测这批稿子上「回头」
    // 是最大的一项止因（7212 次，占全部终止的四成），绕出来的就是看图看到的
    // 「圈状笔触」，半径只有 7px 上下。真正的发丝不会在 7px 里转满一圈，
    // 所以按累积转角卡住它，比按「撞到自己」卡更准。
    // 上限取 150°：允许发丝自然的弯折，挡掉转过半圈以上的卷曲。
    let mut turn = 0.0f64;
    let mut pth: Option<f64> = None;
    let (mut s_zero, mut s_low, mut s_occ, mut s_edge, mut s_loop, mut s_coh, mut s_par, mut s_turn) =
        (0usize, 0usize, 0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    for _ in 0..max_steps {
        // 局部脊方向场（倍角）→ 角度 → 单位向量
        let c = bil(&f.rdc, w, h, x, y);
        let sc = bil(&f.rds, w, h, x, y);
        // **方向不可信 → 立刻停**。纯色块、抹匀的渐变里脊响应为 0，
        // 方向是噪声；沿着它走就是在画原稿没有的线。
        let co = bil(&f.rcoh, w, h, x, y);
        let th = 0.5 * (sc as f64).atan2(c as f64);
        // **惯性**：方向场给的走向，和「上一步的方向」按 `inertia : 1` 混合。
        //
        // 原稿的头发是一堆**互相交叉**的发丝，交叉点上方向场必然是几根线的平均，
        // 谁都不像。没有惯性时流线一到交叉点就拐向别的发丝，描出来的全是碎折线
        // （实测笔长中位数 2.2mm、方向差 40°）；有了惯性，铅笔线「本来就是平滑的」
        // 这个先验让流线直行穿过交叉点，长弧才接得起来。
        let (mut ux, mut uy) = (th.cos(), th.sin());
        if inertia > 0.0 {
            if let Some((px_, py_)) = pdir {
                // 方向只有「轴」没有「向」，混合前先把上一步翻到同侧，
                // 否则反向的两个向量会互相抵消。
                let (ax, ay) = if px_ * ux + py_ * uy < 0.0 {
                    (-px_, -py_)
                } else {
                    (px_, py_)
                };
                let mx = ux + ax * inertia;
                let my = uy + ay * inertia;
                let m = (mx * mx + my * my).sqrt();
                if m > 1e-6 {
                    ux = mx / m;
                    uy = my / m;
                }
            }
        }
        {
            let th2 = uy.atan2(ux);
            if let Some(t0) = pth {
                let mut d = th2 - t0;
                while d > std::f64::consts::PI {
                    d -= 2.0 * std::f64::consts::PI;
                }
                while d < -std::f64::consts::PI {
                    d += 2.0 * std::f64::consts::PI;
                }
                turn += d;
            }
            pth = Some(th2);
        }
        if turn.abs() > turn_max {
            s_turn += 1;
            break;
        }
        pdir = Some((ux, uy));
        x += ux * sgn;
        y += uy * sgn;
        if x < 1.0 || y < 1.0 || x >= w as f64 - 1.0 || y >= h as f64 - 1.0 {
            s_edge += 1;
            break;
        }
        // **法向吸附**：走完一步后，沿法向 ±snap 像素内找响应最大的位置贴过去。
        //
        // 少了这一步，流线走不过一根发丝。这批稿子的发丝只有 1~2px 宽，脊响应
        // 也就 1~2px 宽的一条「谷底」；方向场再准，一步偏 1px 就掉进 0 响应，
        // 于是每根线只能描出十几个像素就被判定「这里没线了」——实测笔长中位数
        // 只有 1.55mm（12px），而原稿的发丝是贯通全幅的长弧。
        // 吸附半径取得比笔宽一半还小（默认 2px），不会跨到隔壁那根排线上。
        if snap > 0.0 {
            // 法向取**混合后**方向的正交，才贴得回真正走的这条脊上
            let (nx, ny) = (-uy, ux);
            let mut bv = bil(&f.ridge, w, h, x, y);
            let (mut bx, mut by) = (x, y);
            let mut k = 1.0f64;
            while k <= snap {
                for s in [k, -k] {
                    let px = x + nx * s;
                    let py = y + ny * s;
                    if px < 1.0 || py < 1.0 || px >= w as f64 - 1.0 || py >= h as f64 - 1.0 {
                        continue;
                    }
                    let v = bil(&f.ridge, w, h, px, py);
                    if v > bv {
                        bv = v;
                        bx = px;
                        by = py;
                    }
                }
                k += 1.0;
            }
            x = bx;
            y = by;
        }
        // 「方向不可信」和「响应低」要分开对待，这是试出来的：
        // - **响应低**可以容忍几步（`gap`）：发丝在交叉点、被擦淡的地方响应会
        //   掉下去，不允许跨过去的话，一根长发丝会被切成一堆 18px 的短刺；
        // - **方向不可信**必须**立刻停**，一步都不能再走。一块纯色、一片抹匀的
        //   渐变，结构张量是各向同性的，那里根本没有方向可言，凭噪声继续走就是
        //   在画原稿没有的线——实测把这两者合并成同一个 `miss` 计数时，纯色块
        //   内部被编进了 24.6% 的墨（`sketch_tonal_flat_block_only_gets_its_outline`
        //   直接挂掉）。
        let rr = bil(&f.nridge, w, h, x, y);
        // **完全没有脊响应 → 立刻停**，一步都不许再走。
        // 这和「响应低」是两回事：一根发丝经过交叉点、被擦淡时响应是**低但非零**，
        // 那里该给它 `gap` 步的机会跨过去；而一块纯色、一片抹匀的渐变，脊响应
        // 干脆是 0——那里根本没有任何「线」的证据，凭方向场继续走就是在编线。
        // 把这两种情况混成一个计数器，纯色块内部会被编进 24.6% 的墨。
        if rr <= 0.0 {
            s_zero += 1;
            break;
        }
        // 低响应 **和** 低相干都只是「可疑」，不是「到此为止」，都给 `gap` 步机会。
        //
        // 早先把「方向不可信」写成**立刻停**，结果流线全断在发丝的断口上——
        // 实测这是最大的一项止因（5207 次），笔长中位数只有 4.5mm，而原稿的
        // 发丝是贯通全幅的长弧。发丝本来就会因为叠压、擦揉而时隐时现，
        // 断口处既没响应也没方向，必须允许跨过去。
        // 真正该立刻停的只有 `rr <= 0`（那里连「有根线」的证据都没有）。
        if rr < thr_lo || co < coh_min {
            if co < coh_min {
                s_coh += 1;
            }
            miss += 1;
            if miss > gap {
                s_low += 1;
                break;
            }
        } else {
            miss = 0;
        }
        let vi = (y as usize).min(h - 1) * w + (x as usize).min(w - 1);
        if covered[vi] != 0 {
            n_hit += 1;
            // 撞到已画的线：先看**夹角**再决定放不放行（见 `mark_blocked`）。
            // 两根线的夹角 φ 与倍角点积的关系是 d = cos(2φ)：φ=0（平行）时 d=1，
            // φ=45° 时 d=0，φ=90° 时 d=-1。所以 d > 0 就是「顺着走」。
            let (ox, oy) = ((ux * ux - uy * uy) as f32, (2.0 * ux * uy) as f32);
            let d = bdx[vi] * ox + bdy[vi] * oy;
            // 阈值取 cos(80°)：夹角小于 40° 就算在描同一条线。
            if d > 0.17 {
                s_par += 1;
                break;
            }
            occ += 1;
            if occ > occ_max {
                s_occ += 1;
                break;
            }
        } else {
            occ = 0;
        }
        // **不走回头路**：这根笔迹自己已经走过的像素，不许再走第二遍。
        //
        // 少了这一条，遇到闭合的脊带（一个圆块的边缘、一处高光的轮廓）流线会
        // 沿着它一圈一圈绕下去——实测这块色斑上单条笔迹绕了 4.5 圈、长 1543px，
        // 而那一圈的周长只有 339px。绕圈的部分在纸上会反复压同一条线，
        // 既不是原稿的笔触，也会把覆盖率指标抬到 0.41。
        // `visit` 用「世代号」而不是每次清零：一次分配，整幅复用。
        let vi = vi;
        if visit[vi] == gen {
            s_loop += 1;
            break;
        }
        visit[vi] = gen;
        pts.push((x, y));
    }
    for (k, c) in
        [s_zero, s_low, s_occ, s_edge, s_loop, s_coh, s_par, s_turn].iter().enumerate()
    {
        stop[k] += *c;
    }
    let dup = if pts.is_empty() {
        0.0
    } else {
        n_hit as f64 / pts.len() as f64
    };
    (pts, dup)
}


/// 可调「纸白门槛」的提取。
///
/// `tone_gate` 决定多淡的笔画仍算「画过」。旧版写死 0.20（当时基准稿的脸平均
/// 调子 0.24）；换成浅调素描后，脸颊实测只有 0.03，整张脸的排线都被当成纸白，
/// 只留下头发和五官，叠出来自然「比原稿空」。
///
/// `calibrate` 控制末端的墨量自校准。它比的是「线条长度 × 笔宽」与原稿调子面积，
/// 适合 0.15mm 的笔；素描笔触层要的是**线位与密度**，用 0.3mm 的笔去对齐墨量
/// 只会把线距拉大一倍，所以这一层关掉校准。
fn ridge_strokes_gated(
    f: &Field,
    pen_mm: f64,
    thr_slider: f64,
    density: f64,
    tone_gate: f32,
    abs_floor: f32,
    calibrate: bool,
) -> (Vec<Stroke>, Vec<u8>) {
    let (out, cov) = ridge_strokes_once(f, pen_mm, thr_slider, 1.0, density, tone_gate, abs_floor);
    if !calibrate {
        return (out, cov);
    }
    let src: f64 = f.tone.iter().map(|&t| t as f64).sum::<f64>() * f.px_mm * f.px_mm;
    let ink = |v: &[Stroke]| v.iter().map(|s| s.len_mm).sum::<f64>() * pen_mm;
    let r1 = if src > 1e-9 { ink(&out) / src } else { 1.0 };
    if out.is_empty() || (0.80..=1.25).contains(&r1) {
        return (out, cov);
    }
    // 墨量 ∝ 1/间距，所以间距直接按墨量比缩放。夹在 0.55~1.80 之间：
    // 一次重跑拉不回来也别把线挤成一团或撒成稀网。
    let k = r1.clamp(0.55, 1.80);
    let (out2, cov2) =
        ridge_strokes_once(f, pen_mm, thr_slider, k, density, tone_gate, abs_floor);
    let r2 = if src > 1e-9 { ink(&out2) / src } else { 1.0 };
    if (r2 - 1.0).abs() < (r1 - 1.0).abs() {
        if std::env::var("SKETCH_DEBUG").is_ok() {
            eprintln!("  墨量自校准：{r1:.2} → {r2:.2}（间距 ×{k:.2}）");
        }
        (out2, cov2)
    } else {
        (out, cov)
    }
}

fn ridge_strokes_once(
    f: &Field,
    pen_mm: f64,
    thr_slider: f64,
    cover_k: f64,
    density: f64,
    tone_gate: f32,
    abs_floor: f32,
) -> (Vec<Stroke>, Vec<u8>) {
    let (w, h) = (f.w, f.h);
    let n = w * h;
    let mut covered = vec![0u8; n];
    if w < 16 || h < 16 {
        return (Vec::new(), covered);
    }
    let pen_px = (pen_mm / f.px_mm).max(1.0);
    // 门槛 = 分位 ∩ 绝对下限。
    // 只用分位不行：纸面占的面积最大，分位会把纸纹也算进「线」；
    // 只用绝对值也不行：不同稿子的对比度差十倍。两个一起卡。
    // 排序、取分位一律用**归一化**响应 `nridge`，不用 `ridge`。
    //
    // 这一点是看出来的，不是算出来的：全局分位下 05_1 这张脸整片空白、五官全丢。
    // 原因是头发区的脊响应天生比脸颊强一个量级，"最强 24%" 被头发占满，脸上
    // 那些浅笔触排都排不进来。除掉局部上包络之后两边才在同一把尺子上。
    let mut pos: Vec<f32> = f.nridge.iter().copied().filter(|&v| v > 0.0).collect();
    if pos.is_empty() {
        return (Vec::new(), covered);
    }
    pos.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    // 归一化的绝对下限：连自己邻域最强的 8% 都够不上，那点起伏就是纸纹。
    let floor = 0.08f32;
    // 原始响应的绝对下限，单独卡一道：归一化会把**纯噪声区**也抬成 1.0
    // （那里上包络本身就是噪声），必须另外要求响应在绝对量级上也够看。
    // 模式 10 的浅调素描要调高：扫描背景的云斑在归一化后同样「很强」，
    // 但绝对响应只有 0.005~0.015，正是那些满背景的波浪线来源。
    let n_pos = pos.len();
    // 「识别密度」滑杆决定要原稿上哪一部分线：它对应的是「这些线我要不要」，
    // 而不是明暗。滑杆 0..1 映射到「取响应最强的 6%..31%」。取多了纸纹会跟着
    // 进来，取少了五官就散架——0.24 附近是这批稿子的甜点。
    // 多笔提取时 `density > 1`：同一根浅笔触也要拿到，否则分给 4 支笔后
    // 每支只剩几根线，叠加起来必然比原稿空。单笔调子模式仍按 1.0 严格取舍。
    let frac = ((0.06 + 0.25 * thr_slider.clamp(0.0, 1.0)) * density.max(0.2))
        .clamp(0.02, 0.88);
    let idx = ((n_pos as f64 * (1.0 - frac)) as usize).min(n_pos - 1);
    let thr = pos[idx].max(floor);
    // 强响应 = 归一化后仍接近局部最强的那一档，是「形」；其余是「调子」。
    // 原来写的是 `ridge > thr * 2`，归一化后取值域变了（0..1），倍数没有意义。
    let strong_k = 0.68f32;
    // 纸白门限由调用方给：模式 7/8/9 的基准稿脸平均调子 0.24，用 0.20 正好；
    // 模式 10 的浅调素描脸只有 0.03，沿用 0.20 会把整张脸的浅排线判成纸白。
    // 背景仍靠 `thr` 的分位门槛与本层的背景掩码挡。
    let lo = tone_gate;
    // 最短笔画。短于此的碎段在原稿上都是纸纹/颗粒，画出来只是一把点。
    // 再往上加（1.8 / 2.6 都试过）会把暗部的短笔触一起砍掉——头发本来就是
    // 一堆短弧，一收紧暗档墨量从 0.83 掉到 0.53。1.2mm ≈ 原稿眼颤的大小。
    // 最短笔画跟着**幅面**走：140mm 的画像取 3.9mm，60mm 的小图取 1.7mm。
    let minl = (0.018 * (h as f64 * f.px_mm)).clamp(1.2, 6.0);

    // —— 沿方向场做**流线追踪** ——
    //
    // 为什么不是「阈值化 → 细化 → 沿骨架走」：那条路在这批素描上必死。
    // 头发区的排线间距只有 4~9px，掩码稍微一放宽就连成一片，Zhang–Suen 细化
    // 一片连通区域得到的是**环状网络**——输出全是螺旋小圈，主观评分只有 3/10；
    // 掩码收紧一点线又碎成 2px 的点串（p50 长度实测只有 2px）。细与长在那条
    // 路上不可兼得。
    //
    // 流线换个思路：不追求「掩码恰好等于线宽」，而是**沿方向场积分**。从真实的
    // 响应点上出发，顺着局部脊方向场走，沿途一串碎响应自然被串成一条长弧。
    // 这既消除了环状病态，也符合人画头发的方式——一缕就是一笔长弧，不是几十根短刺。
    //
    // 低门槛（Canny 那套滞后阈值）：高门槛定种子，低门槛负责**延伸**。
    // 一根铅笔线的脊响应沿途高低起伏（发丝有粗细变化、经过被擦揉的地方会变淡），
    // 只用一个高门槛去切，同一根线会被切成好几截。实测这批素描的头发区，单阈值下
    // 每行只有 22 段、段长中位数 3.2mm——描出来就是一把碎划痕，暗部墨量只有原稿的
    // 25%。低门槛取高门槛容量的 3.5 倍（上限 95%）时线明显接长了，仍是一根根分开
    // 的平行排线，没有糊成实心块。
    let frac_lo = (frac * 3.5).clamp(0.55, 0.95);
    let ilo = ((n_pos as f64 * (1.0 - frac_lo)) as usize).min(n_pos - 1);
    let thr_lo = pos[ilo].max(floor * 0.3);
    // 跨间隙容错：发丝在交叉点、被擦淡的地方响应会掉下去。原型实测这一步
    // 是长度的分水岭——gap=0 时 p50 只有 18px，gap=4 时 55px，gap=6 时 107px。
    let gap = 4usize;
    // 允许穿过别人已画的地方多少步。这一条决定「长发丝到底是长是碎」：
    // 发丝之间是**互相交叉**的，先把 A 描完，B 走到交叉点就会撞上 A 的占用带。
    // 容忍步数小于一个笔画的宽度时，每根长发丝都会在每个交叉点被切断——
    // 实测笔长中位数只有 2.2mm。取到「刚好能横穿一根线、但沿它走会一直撞」
    // 的量级（`pen_px*4 + 4`），长弧才接得起来，同时不会重复描同一条。
    let occ_max = (pen_px * 4.0 + 4.0).clamp(6.0, 40.0) as usize;
    // 法向吸附半径（像素）：见 `stream_walk`。2px 比笔宽一半还小，不会跨到隔壁排线。
    let snap = 2.0f64;
    // 惯性权重：见 `stream_walk`。5 : 1 是实测出来的，方向差中位 40° → 18.7°。
    let inertia = 5.0f64;
    let max_steps = 600usize;
    // 一条流线最多允许累计转多少弧度（见 `stream_walk`）。
    let turn_max = 1.75f64;
    // 方向场一致性的下限。低于此值就认为「这里没有方向」——纯色块、抹匀的
    // 渐变都是这样，不能让流线在里面乱拐。
    let coh_min = 0.25f32;
    // 一条线允许有多大比例踩在已画过的线上。超过就整条不画。
    let dup_max = 0.30f64;
    // 种子必须是脊的**局部峰值**（3×3 里的最大值），不只是「响应够高」。
    //
    // 少了这一条，同一条线会被描出两遍：脊响应从线心向外是单调下降的，离心
    // 3~4px 的像素响应仍然过线，拿到种子资格后再被法向吸附拉回线心，于是同一根
    // 线走出两条几乎重合的笔迹——实测合成平行线那张图上 8 条线走出 16 条笔迹，
    // 缝合后变成 118.8mm 的折返路径（线本身才 61mm），整条线被来回画两遍。
    let peak = box_max(&f.ridge, w, h, 1);
    let mut seeds: Vec<(f32, usize)> = f
        .nridge
        .iter()
        .enumerate()
        .filter(|(i, &v)| {
            v >= thr && f.ridge[*i] >= abs_floor && f.ridge[*i] >= peak[*i] - 1e-9
        })
        .map(|(i, &v)| (v, i))
        .collect();
    seeds.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    // 走过的像素：同一根线只描一遍
    let mut walked = vec![0u8; n];
    // 已画笔迹的**走向**（倍角单位向量），只在 `covered != 0` 处有意义
    let (mut bdx, mut bdy) = (vec![0f32; n], vec![0f32; n]);
    // 「这一笔自己走过哪」：世代号，每根笔迹 +1，见 `stream_walk` 的说明。
    let mut visit = vec![0u32; n];
    let mut gen = 0u32;
    let n_seed = seeds.len();
    let mut out: Vec<Stroke> = Vec::new();
    let (mut n_short, mut n_cov, mut n_pale, mut n_dup) = (0usize, 0usize, 0usize, 0usize);
    let mut lens: Vec<f64> = Vec::new();
    let mut stop = [0usize; 8];
    for (resp, i) in seeds {
        let (sx, sy) = ((i % w) as f64, (i / w) as f64);
        if walked[i] != 0 || covered[i] != 0 {
            n_cov += 1;
            continue;
        }
        let tb = f.tone_at(sx, sy);
        if tb < lo {
            n_pale += 1;
            continue;
        }
        gen += 1;
        if gen == u32::MAX {
            visit.fill(0);
            gen = 1;
        }
        let (bw, db) = stream_walk(
            f, &covered, &bdx, &bdy, &mut visit, gen, &mut stop, turn_max, sx, sy, -1.0, thr_lo,
            coh_min, gap, occ_max, snap, inertia, max_steps,
        );
        let (fw, df) = stream_walk(
            f, &covered, &bdx, &bdy, &mut visit, gen, &mut stop, turn_max, sx, sy, 1.0, thr_lo,
            coh_min, gap, occ_max, snap, inertia, max_steps,
        );
        // **不画第二遍**：这一趟要是大半都踩在别人已经画过的线上，就整条丢掉。
        //
        // 少了这一条，墨会重复涂在同一批像素上：实测机器总墨量是原稿的 1.37 倍，
        // 可原稿还有 54% 的墨迹根本没被覆盖——墨没画到该画的地方，全浪费在重叠上。
        // 方向敏感的 `covered` 只在流线**顺着**别人走时叫停，可叫停之前那几毫米
        // 已经走完了、照样被采纳画出来，和前人重合。这里按整条的重叠比例兜底。
        let ntot = bw.len() + fw.len();
        let dup = if ntot > 0 {
            (db * bw.len() as f64 + df * fw.len() as f64) / ntot as f64
        } else {
            0.0
        };
        let mut pts: Vec<(f64, f64)> = bw.into_iter().rev().collect();
        pts.push((sx, sy));
        pts.extend(fw);
        for p in &pts {
            let (px, py) = ((p.0 as usize).min(w - 1), (p.1 as usize).min(h - 1));
            walked[py * w + px] = 1;
        }
        if dup > dup_max {
            n_dup += 1;
            continue;
        }
        let strong = resp > strong_k;
        // 「越暗 → 排得越密」
        // 投影笔只有一个墨色，明暗只能靠疏密表现，所以暗处要更密才对。
        // 早先写成了 (0.92 + 0.30*tb)——越暗间距越大，结果暗部（头发、衣领）
        // 被自己撑开，而脸上本该留白的浅区反而最密、堆满杂线；实测明暗比
        // 亮 0.95 / 暗 0.80，灰阶关系是反的。改成 1 - k*tb 之后才对得上原稿。
        // 倍数的**量级**是实测卡死的：这批素材的排线间距实测 0.49~0.99mm，
        // 也就是 1.0~2.0 个笔宽。间距取到 2.5~4.0 笔宽（10~15px）时比原稿排线
        // 还宽——画一根就封死一大片，相邻的线再也排不进去，召回只有 31%，
        // 五官因为抢不到位置整片空白，剩下的墨全挤在暗部成团。
        let cover = if strong {
            (pen_px * 0.56 * (1.0 - 0.58 * tb as f64)).clamp(pen_px * 0.25, pen_px * 0.56)
        } else {
            (pen_px * 0.90 * (1.0 - 0.80 * tb as f64)).clamp(pen_px * 0.32, pen_px * 0.90)
        } * cover_k;
        // 流线是 1px 一步的密点，先滑动平均抹平方向场的抖动，再抽稀成拐点
        let sm = smooth_poly(&pts, 6);
        let pts = rdp(&sm, 0.45);
        let len_mm = chain_len(&pts) * f.px_mm;
        // 注意判「太短」不能看点数：RDP 会把一条**笔直**的线压成 2 个端点，
        // 笔直恰恰是最该画出来的形状。曾经写成 `pts.len() < 3`，结果合成的
        // 平行直线一张图只认出 0 条（`sketch_tonal_extracts_parallel_lines`
        // 直接挂掉）——直线被自己的抽稀判了死刑。
        if pts.len() < 2 || len_mm < minl {
            n_short += 1;
            continue;
        }
        lens.push(len_mm);
        // 一根线就是一个墨色。**不要靠「铺几遍」加深**：投影笔没有深浅之分，
        // 铺一遍和铺三遍在纸上是同一条线，只会沿同一轨迹反复摩擦，把线画粗、
        // 把旁边的纸缝填死。明暗只能靠「排得多密」。
        let passes = if tb > 0.82 { 2 } else { 1 };
        out.push(Stroke {
            pts: pts.clone(),
            loose: [false; 2],
            len_mm,
            mean_dark: tb as f64,
            mean_width_mm: pen_mm,
            theta: path_theta(&pts),
            rank: 0.0,
            structural: strong,
            passes,
        });
        mark_blocked(&mut covered, &mut bdx, &mut bdy, w, h, &pts, cover);
    }
    if std::env::var("SKETCH_DEBUG").is_ok() {
        eprintln!(
            "笔触提取：门槛 {thr:.4}（脊像素 {n_pos}，取最强 {:.0}%）、骨架线 {n_seed} → 采 {}/{n_seed} \
             （短段 {n_short}、纸白 {n_pale}、位置已占 {n_cov}、重复 {n_dup}）",
            frac * 100.0,
            out.len()
        );
        eprintln!(
            "  流线止因：脊归零 {} 响应低 {} 横跨 {} 出界 {} 回头 {} 无方向 {} 顺行 {} 转角 {}",
            stop[0], stop[1], stop[2], stop[3], stop[4], stop[5], stop[6], stop[7]
        );
        if !lens.is_empty() {
            let mut v = lens.clone();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let q = |p: f64| v[((v.len() - 1) as f64 * p) as usize];
            eprintln!(
                "  笔长 mm：p10 {:.2} p50 {:.2} p90 {:.2} max {:.2}",
                q(0.10),
                q(0.50),
                q(0.90),
                v[v.len() - 1]
            );
        }
    }
    // 缝合：把断掉的同一根线接回去（只动拼接，不动提取结果）
    let before = out.len();
    // 缝合距离 = 1.8 个笔宽。缝得再宽（3 笔宽试过）会把本不相干的线段连成
    // 一道假弧，尤其在眼睛周围。
    let out = stitch_strokes(out, pen_px * 1.8, f.px_mm);
    if std::env::var("SKETCH_DEBUG").is_ok() {
        eprintln!(
            "笔触缝合：{} → {} 条；总长 {:.0}mm",
            before,
            out.len(),
            out.iter().map(|s| s.len_mm).sum::<f64>()
        );
    }
    (out, covered)
}

/// 端点朝外的单位方向（`which` 0 = 起点，1 = 终点）。
fn end_dir(s: &Stroke, which: usize) -> (f64, f64) {
    let n = s.pts.len();
    if n < 2 {
        return (1.0, 0.0);
    }
    let k = 4.min(n - 1);
    let (a, b) = if which == 0 {
        (s.pts[0], s.pts[k])
    } else {
        (s.pts[n - 1], s.pts[n - 1 - k])
    };
    let d = (a.0 - b.0, a.1 - b.1);
    let l = (d.0 * d.0 + d.1 * d.1).sqrt();
    if l < 1e-9 {
        (1.0, 0.0)
    } else {
        (d.0 / l, d.1 / l)
    }
}

/// 把首尾相接、走向一致的短段缝成一根长笔。
///
/// 为什么需要这一步：发丝在交叉点、变细处，脊响应会短暂掉下去，追踪只能断在那里。
/// 硬把追踪门槛压低去硬闯，代价是笔迹会顺着纸纹漂走，整幅墨量翻好几倍。
/// 断掉的短段本身**位置是对的**，那就别改提取，改拼接——只要两段的端点足够近、
/// 走向对得上（差 180° 以内 40°），本来就是同一条线，接起来即可。
/// 接完之后头发才是一缕缕的长弧，而不是一堆柴火棍。
fn stitch_strokes(list: Vec<Stroke>, gap_px: f64, px_mm: f64) -> Vec<Stroke> {
    let mut items: Vec<Option<Stroke>> = list.into_iter().map(Some).collect();
    let gap2 = gap_px * gap_px;
    for _round in 0..10 {
        let mut merged = 0usize;
        for i in 0..items.len() {
            if items[i].is_none() {
                continue;
            }
            loop {
                let mut best: Option<(usize, f64, bool)> = None;
                {
                    let a = items[i].as_ref().unwrap();
                    if a.pts.len() < 2 {
                        break;
                    }
                    let pa = *a.pts.last().unwrap();
                    let da = end_dir(a, 1);
                    for (j, slot) in items.iter().enumerate() {
                        if j == i {
                            continue;
                        }
                        let Some(b) = slot.as_ref() else { continue };
                        if b.pts.len() < 2 {
                            continue;
                        }
                        // 两条都要试：b 的尾巴接过来，或者 b 掉个头接过来
                        for rev in [false, true] {
                            let pb = if rev { b.pts[0] } else { *b.pts.last().unwrap() };
                            let db = if rev { end_dir(b, 0) } else { end_dir(b, 1) };
                            let dd = (pa.0 - pb.0) * (pa.0 - pb.0) + (pa.1 - pb.1) * (pa.1 - pb.1);
                            if dd > gap2 {
                                continue;
                            }
                            // 朝向要接得上：a 往外走的方向，与 b 往外走的方向应相反
                            if da.0 * db.0 + da.1 * db.1 > -0.78 {
                                continue;
                            }
                            if best.map_or(true, |(_, bd, _)| dd < bd) {
                                best = Some((j, dd, rev));
                            }
                        }
                    }
                }
                let Some((j, _, rev)) = best else { break };
                let mut b = items[j].take().unwrap();
                let mut a = items[i].take().unwrap();
                // 接上之后要能顺着 a 继续往外走，所以 b 必须**从接缝那一头排起**。
                // 接缝是 `pb`：`rev` 为真时它是 b 的首点（不用翻），为假时它是 b 的
                // 末点（要翻过来）。
                //
                // 这里曾经写成 `if rev { b.pts.reverse() }`——正好反了：翻完之后
                // 首点变成远端，去掉首点就只剩「从远端走回接缝」的一段，接出来的
                // 是折返路径（a→接缝→远端→接缝）。实测合成平行线那张图上，8 条线
                // 接完变成 8 条 118.8mm 的折返笔迹（线本身才 61mm），后面抽稀
                // 又把折返路径压成首尾两点，只剩 12px 的残根，整条线等于没画。
                if !rev {
                    b.pts.reverse();
                }
                // 端点可能重合，去掉重复的首点，免得出现零长度段
                if b.pts.len() > 1 {
                    b.pts.remove(0);
                }
                a.pts.extend(b.pts);
                a.pts.dedup_by(|p, q| (p.0 - q.0).abs() < 1e-6 && (p.1 - q.1).abs() < 1e-6);
                a.len_mm = chain_len(&a.pts) * px_mm;
                a.theta = path_theta(&a.pts);
                a.mean_dark = a.mean_dark.max(b.mean_dark);
                a.passes = a.passes.max(b.passes);
                a.structural = a.structural || b.structural;
                items[i] = Some(a);
                merged += 1;
                if merged > 20000 {
                    break;
                }
            }
        }
        if merged == 0 {
            break;
        }
        // 每轮把整体翻一次，这样「接头部」和「接尾部」用同一段代码就能都覆盖到。
        for s in items.iter_mut().flatten() {
            s.pts.reverse();
        }
    }
    items.into_iter().flatten().collect()
}

/// 双线性采样。
fn bil(a: &[f32], w: usize, h: usize, x: f64, y: f64) -> f32 {
    if w == 0 || h == 0 {
        return 0.0;
    }
    let x = x.clamp(0.0, w as f64 - 1.0);
    let y = y.clamp(0.0, h as f64 - 1.0);
    let x0 = x as usize;
    let y0 = y as usize;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let fx = (x - x0 as f64) as f32;
    let fy = (y - y0 as f64) as f32;
    let p = |xx: usize, yy: usize| a[yy * w + xx];
    (p(x0, y0) * (1.0 - fx) + p(x1, y0) * fx) * (1.0 - fy)
        + (p(x0, y1) * (1.0 - fx) + p(x1, y1) * fx) * fy
}

/// 把一条已画的笔迹写进占用图。
///
/// 除了「这里被占了」，还一起记下**这根线自己朝哪走**（倍角单位向量）。
/// 后面流线撞到占用区时要靠它分辨两件事：
/// - **横跨**：踩到别人的线、一两步就出去了，这是原稿本来就有的交叉，必须放行，
///   否则每根长发丝都会在每个交叉点被切断（实测笔长中位只有 1.85mm）；
/// - **顺着走**：和别人的线几乎同向，这就是在把同一条线描第二遍——必须立刻停，
///   否则墨量会涨到原稿的 3 倍以上（`occ` 开大时实测明暗比 3.84）。
///
/// 早先只用各向同性的占用标记，把这两件事混为一谈：要么一起放行（墨量爆炸），
/// 要么一起拦（线碎成渣）。
fn mark_blocked(
    blocked: &mut [u8],
    bdx: &mut [f32],
    bdy: &mut [f32],
    w: usize,
    h: usize,
    pts: &[(f64, f64)],
    r: f64,
) {
    let ri = r.ceil() as isize;
    let r2 = r * r;
    for (k, &(px, py)) in pts.iter().enumerate() {
        // 这一段的方向：用前后邻点估，端点用单边
        let (ax, ay) = if k > 0 { pts[k - 1] } else { (px, py) };
        let (bx, by) = if k + 1 < pts.len() { pts[k + 1] } else { (px, py) };
        let (mut vx, mut vy) = (bx - ax, by - ay);
        let m = (vx * vx + vy * vy).sqrt();
        if m < 1e-9 {
            (vx, vy) = (1.0, 0.0);
        } else {
            (vx, vy) = (vx / m, vy / m);
        }
        // 倍角：一根线的方向只有「轴」没有「向」
        let (ux, uy) = ((vx * vx - vy * vy) as f32, (2.0 * vx * vy) as f32);
        let x0 = (px as isize - ri).max(0) as usize;
        let x1 = ((px as isize + ri) as usize).min(w - 1);
        let y0 = (py as isize - ri).max(0) as usize;
        let y1 = ((py as isize + ri) as usize).min(h - 1);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let dx = x as f64 - px;
                let dy = y as f64 - py;
                if dx * dx + dy * dy <= r2 {
                    let i = y * w + x;
                    blocked[i] = 1;
                    bdx[i] = ux;
                    bdy[i] = uy;
                }
            }
        }
    }
}

fn near_xy(a: (f64, f64), b: (f64, f64)) -> f64 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

fn chain_len(pts: &[(f64, f64)]) -> f64 {
    pts.windows(2)
        .map(|w| {
            let dx = w[1].0 - w[0].0;
            let dy = w[1].1 - w[0].1;
            (dx * dx + dy * dy).sqrt()
        })
        .sum()
}

/// 主方向（0..π）：按弧长加权的倍角平均，弯曲的笔迹也能给出一个稳定朝向。
fn path_theta(pts: &[(f64, f64)]) -> f64 {
    let mut sx = 0.0;
    let mut sy = 0.0;
    for w in pts.windows(2) {
        let dx = w[1].0 - w[0].0;
        let dy = w[1].1 - w[0].1;
        let l = (dx * dx + dy * dy).sqrt();
        if l < 1e-9 {
            continue;
        }
        let a = dy.atan2(dx);
        sx += (2.0 * a).cos() * l;
        sy += (2.0 * a).sin() * l;
    }
    let mut theta = 0.5 * sy.atan2(sx);
    if theta < 0.0 {
        theta += std::f64::consts::PI;
    }
    if theta >= std::f64::consts::PI {
        theta -= std::f64::consts::PI;
    }
    theta
}




fn bounds(pts: &[(f64, f64)]) -> (f64, f64, f64, f64) {
    let mut b = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for &(x, y) in pts {
        b.0 = b.0.min(x);
        b.1 = b.1.min(y);
        b.2 = b.2.max(x);
        b.3 = b.3.max(y);
    }
    b
}

fn centroid(pts: &[(f64, f64)]) -> (f64, f64) {
    if pts.is_empty() {
        return (0.0, 0.0);
    }
    let n = pts.len() as f64;
    let mut x = 0.0;
    let mut y = 0.0;
    for &(px, py) in pts {
        x += px;
        y += py;
    }
    (x / n, y / n)
}


fn orient_near(s: &Stroke, cur: (f64, f64)) -> (bool, (f64, f64)) {
    let a = s.pts[0];
    let b = *s.pts.last().unwrap();
    if near_xy(cur, b) < near_xy(cur, a) {
        (true, a)
    } else {
        (false, b)
    }
}


fn scan_plan(strokes: &[Stroke]) -> Vec<Planned> {
    let mut ids: Vec<usize> = (0..strokes.len()).collect();
    ids.sort_by(|&a, &b| {
        let ba = bounds(&strokes[a].pts);
        let bb = bounds(&strokes[b].pts);
        ba.1
            .partial_cmp(&bb.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                ba.0.partial_cmp(&bb.0)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });
    let mut plan = Vec::with_capacity(ids.len());
    for i in ids {
        let s = &strokes[i];
        let a = s.pts[0];
        let b = *s.pts.last().unwrap();
        let rev = (b.1, b.0) < (a.1, a.0);
        plan.push(Planned { idx: i, rev });
    }
    plan
}

fn nearest_plan(strokes: &[Stroke]) -> Vec<Planned> {
    let n = strokes.len();
    let mut used = vec![false; n];
    let mut plan = Vec::with_capacity(n);
    let mut cur = (0.0, 0.0);
    for _ in 0..n {
        let mut best = usize::MAX;
        let mut best_d = f64::INFINITY;
        let mut best_rev = false;
        for (i, s) in strokes.iter().enumerate() {
            if used[i] {
                continue;
            }
            let a = s.pts[0];
            let b = *s.pts.last().unwrap();
            let da = (a.0 - cur.0).powi(2) + (a.1 - cur.1).powi(2);
            let db = (b.0 - cur.0).powi(2) + (b.1 - cur.1).powi(2);
            let (d, rev) = if db < da { (db, true) } else { (da, false) };
            if d < best_d {
                best_d = d;
                best = i;
                best_rev = rev;
            }
        }
        if best == usize::MAX {
            break;
        }
        used[best] = true;
        let s = &strokes[best];
        cur = if best_rev {
            s.pts[0]
        } else {
            *s.pts.last().unwrap()
        };
        plan.push(Planned {
            idx: best,
            rev: best_rev,
        });
    }
    plan
}

fn block_plan(strokes: &[Stroke], block_px: u32, scale: f64) -> Vec<Planned> {
    let bs = (block_px.max(8) as f64 * scale).max(5.0);
    let mut groups: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (i, s) in strokes.iter().enumerate() {
        let (cx, cy) = centroid(&s.pts);
        groups
            .entry(((cy / bs).floor() as i64, (cx / bs).floor() as i64))
            .or_default()
            .push(i);
    }
    let mut rows: Vec<i64> = groups.keys().map(|k| k.0).collect();
    rows.sort_unstable();
    rows.dedup();
    let mut plan = Vec::new();
    let mut cur = (0.0, 0.0);
    for row in rows {
        let mut cols: Vec<i64> = groups.keys().filter(|k| k.0 == row).map(|k| k.1).collect();
        cols.sort_unstable();
        if row.rem_euclid(2) == 1 {
            cols.reverse();
        }
        for col in cols {
            let Some(mut ids) = groups.remove(&(row, col)) else {
                continue;
            };
            ids.sort_by(|&a, &b| {
                let ca = centroid(&strokes[a].pts);
                let cb = centroid(&strokes[b].pts);
                ca.1.partial_cmp(&cb.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(ca.0.partial_cmp(&cb.0).unwrap_or(std::cmp::Ordering::Equal))
            });
            for i in ids {
                let (rev, exit) = orient_near(&strokes[i], cur);
                cur = exit;
                plan.push(Planned { idx: i, rev });
            }
        }
    }
    plan
}



fn hash_u64(mut x: u64) -> u64 {
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 33;
    x = x.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    x ^= x >> 33;
    x
}

fn hash01(x: u64) -> f64 {
    (hash_u64(x) >> 11) as f64 / (1u64 << 53) as f64
}

/// 一维平滑值噪声，返回 -1..1；用来做不规则的手抖，避免正弦的机械感。
fn value_noise(x: f64, seed: u64) -> f64 {
    let i = x.floor();
    let f = x - i;
    let a = hash01((i as i64 as u64).wrapping_add(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15)));
    let b = hash01(((i as i64 + 1) as u64).wrapping_add(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15)));
    let u = f * f * (3.0 - 2.0 * f);
    (a + (b - a) * u) * 2.0 - 1.0
}

fn resample(pts: &[(f64, f64)], spacing: f64) -> Vec<(f64, f64)> {
    if pts.len() < 2 {
        return pts.to_vec();
    }
    let mut out = vec![pts[0]];
    let mut carry = 0.0;
    for w in pts.windows(2) {
        let (x0, y0) = w[0];
        let (x1, y1) = w[1];
        let dx = x1 - x0;
        let dy = y1 - y0;
        let seg = (dx * dx + dy * dy).sqrt();
        if seg < 1e-9 {
            continue;
        }
        let mut t = spacing - carry;
        while t <= seg {
            let f = t / seg;
            out.push((x0 + dx * f, y0 + dy * f));
            t += spacing;
        }
        carry = seg - (t - spacing);
        if carry >= spacing {
            carry = 0.0;
        }
    }
    if let Some(&last) = pts.last() {
        if near_xy(last, *out.last().unwrap()) > 1e-6 {
            out.push(last);
        }
    }
    out
}

fn smooth(pts: &mut [(f64, f64)]) {
    if pts.len() < 3 {
        return;
    }
    let n = pts.len();
    let mut prev = pts[0];
    for i in 1..n - 1 {
        let cur = pts[i];
        let next = pts[i + 1];
        pts[i] = (
            (prev.0 + 2.0 * cur.0 + next.0) * 0.25,
            (prev.1 + 2.0 * cur.1 + next.1) * 0.25,
        );
        prev = cur;
    }
}

/// 按弧长裁掉两端 / 在两端沿切线过冲。
fn trim_extend(
    pts: &[(f64, f64)],
    trim_start: f64,
    trim_end: f64,
    over_start: f64,
    over_end: f64,
) -> Vec<(f64, f64)> {
    if pts.len() < 2 {
        return pts.to_vec();
    }
    let total = chain_len(pts);
    let s0 = trim_start.clamp(0.0, total * 0.45);
    let s1 = (total - trim_end).clamp(s0 + 0.05, total);
    let at = |target: f64| -> (f64, f64) {
        let mut acc = 0.0;
        for w in pts.windows(2) {
            let seg = near_xy(w[0], w[1]);
            let a1 = acc + seg;
            if target >= acc - 1e-9 && target <= a1 + 1e-9 {
                let t = if seg > 1e-9 {
                    ((target - acc) / seg).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                return (
                    w[0].0 + (w[1].0 - w[0].0) * t,
                    w[0].1 + (w[1].1 - w[0].1) * t,
                );
            }
            acc = a1;
        }
        *pts.last().unwrap()
    };
    let mut core = vec![at(s0)];
    let mut acc = 0.0;
    for w in pts.windows(2) {
        let a1 = acc + near_xy(w[0], w[1]);
        if a1 > s0 && a1 < s1 {
            core.push(w[1]);
        }
        if a1 >= s1 {
            break;
        }
        acc = a1;
    }
    let ep = at(s1);
    if near_xy(ep, *core.last().unwrap()) > 1e-6 {
        core.push(ep);
    }
    if core.len() < 2 {
        return core;
    }
    if over_start > 1e-6 {
        let (a, b) = (core[0], core[1]);
        let l = near_xy(a, b).max(1e-9);
        core.insert(
            0,
            (
                a.0 - (b.0 - a.0) / l * over_start,
                a.1 - (b.1 - a.1) / l * over_start,
            ),
        );
    }
    if over_end > 1e-6 {
        let n = core.len();
        let (a, b) = (core[n - 1], core[n - 2]);
        let l = near_xy(a, b).max(1e-9);
        core.push((
            a.0 + (a.0 - b.0) / l * over_end,
            a.1 + (a.1 - b.1) / l * over_end,
        ));
    }
    core
}

/// 画一条笔迹：手抖 + 弓形 + 收放。返回的折线是毫米坐标。
///
/// `seed` 决定这条线**走哪儿**（低频摆动、弓形），`var_seed` 只决定这一遍
/// 的**起落点与过冲**。同一条笔迹的多遍必须共用 `seed`——否则每一遍各走各的
/// 抖动，同一条线会被画成两三根散开的「头发丝」。
#[allow(clippy::too_many_arguments)]
fn render_one(
    pts_px: &[(f64, f64)],
    loose: [bool; 2],
    reverse: bool,
    seed: u64,
    var_seed: u64,
    jitter_mm: f64,
    lateral_mm: f64,
    trim_frac: f64,
    allow_over: bool,
    scale: f64,
    out: &mut Vec<Polyline>,
) {
    if pts_px.len() < 2 {
        return;
    }
    let pts: Vec<(f64, f64)> = pts_px
        .iter()
        .map(|&(x, y)| (x * scale, y * scale))
        .collect();
    let pts = simplify_path(&pts, 0.06);
    let mut pts = resample(&pts, 0.28);
    // jitter=0 表示「严格按原稿」：不做平滑位移，避免线从墨迹上滑开。
    if jitter_mm > 0.0 {
        // 细化出来的中线在粗笔画的凹凸边界之间来回摆，直接描会是一串小折角；
        // 两遍 3 点平均把这类像素级锯齿抹平，真正的转折（罐口、桌沿）仍留着。
        smooth(&mut pts);
        smooth(&mut pts);
    }
    if pts.len() < 2 {
        return;
    }
    if reverse {
        pts.reverse();
    }
    let loose = if reverse {
        [loose[1], loose[0]]
    } else {
        loose
    };
    let total = chain_len(&pts);
    if total < 0.35 {
        return;
    }
    let mut trim0 = if loose[0] { trim_frac * total } else { 0.0 };
    let mut trim1 = if loose[1] { trim_frac * total } else { 0.0 };
    let mut over0 = 0.0;
    let mut over1 = 0.0;
    if allow_over && jitter_mm > 0.0 && total > 18.0 {
        let r = hash01(var_seed ^ 0x51ed_2701_b4e9_2f0d);
        if loose[0] {
            over0 = r * 1.8;
            trim0 = trim0.min(0.3);
        }
        if loose[1] {
            over1 = (1.0 - r) * 1.8;
            trim1 = trim1.min(0.3);
        }
    }
    let trimmed = trim_extend(&pts, trim0, trim1, over0, over1);
    if trimmed.len() < 2 {
        return;
    }
    let total = chain_len(&trimmed);
    if total < 0.35 {
        return;
    }
    let bow_sign = if hash01(seed ^ 0x7f4a_7c15_9e37_79b9) < 0.5 {
        -1.0
    } else {
        1.0
    };
    let bow_amp = (jitter_mm * 0.5).min(0.5) * (total / 35.0).min(1.0);
    let n = trimmed.len();
    let mut acc = 0.0;
    let mut displaced = Vec::with_capacity(n);
    for i in 0..n {
        let p = trimmed[i];
        let a = trimmed[i.saturating_sub(1)];
        let b = trimmed[(i + 1).min(n - 1)];
        let dx = b.0 - a.0;
        let dy = b.1 - a.1;
        let l = (dx * dx + dy * dy).sqrt();
        let (nx, ny) = if l > 1e-9 {
            (-dy / l, dx / l)
        } else {
            (0.0, 0.0)
        };
        if i > 0 {
            acc += near_xy(trimmed[i - 1], p);
        }
        let t = (acc / total).clamp(0.0, 1.0);
        // 端点处收敛到 0：交叉点保持相接，收笔处也不会凭空甩出去。
        let taper = (acc / 1.2).min(1.0).min(((total - acc) / 1.2).min(1.0));
        // 摆动按「笔迹自身的弧长」取样：反向落笔时用同一段相位，
        // 多遍才是叠在同一根线上，而不是各抖各的。
        let s = if reverse { total - acc } else { acc };
        // 低频为主：笔在纸上走是一条缓和的线，不是锯齿。高频分量只留一点点毛边。
        // 幅度按笔触长度收敛：1~4mm 的排线在人手里就是一笔短直线，抖满
        // 0.12mm 会变成小钩子；长线（发丝、长轮廓）才需要整根起伏。
        let wob_scale = (total / 10.0).min(1.0);
        let wob = value_noise(s * 0.045, seed) * 0.72
            + value_noise(s * 0.14, seed ^ 0x2c1b_3c6d_4e5f_6071) * 0.22
            + value_noise(s * 0.38, seed ^ 0x9d8e_7f60_5a4b_3c2d) * 0.06;
        let bow = bow_amp * (std::f64::consts::PI * t).sin() * bow_sign;
        let lateral = (lateral_mm + jitter_mm * wob * wob_scale + bow) * taper;
        displaced.push((p.0 + nx * lateral, p.1 + ny * lateral));
    }
    let eps = 0.045 + jitter_mm * 0.04;
    let pts = simplify_path(&displaced, eps);
    if pts.len() < 2 || chain_len(&pts) < 0.35 {
        return;
    }
    out.push(Polyline::new(pts));
}

/// 一条笔迹按「墨量」落笔若干遍。
///
/// 绘图笔的粗细是固定的，而原稿一笔的粗细是变的（铅笔压得重、笔尖钝 → 墨带更宽）。
/// 所以一条笔迹要还原成一组横向错开的平行笔痕：
/// - 带宽 / 单遍宽度 决定「至少要铺几遍」才能盖住原来的粗细；
/// - 明暗再决定「补几遍」，遍数越密墨越重；
/// - 每遍各走各的抖动与起落点，看起来是同一处来回描，而不是机械复制。
fn emit_stroke(
    s: &Stroke,
    rev: bool,
    index: usize,
    pressure: u32,
    jitter_mm: f64,
    pen_mm: f64,
    scale: f64,
    out: &mut Vec<Polyline>,
) {
    let seed = hash_u64(
        (s.pts[0].0 * 131.0) as i64 as u64
            ^ ((s.pts[0].1 * 977.0) as i64 as u64).rotate_left(17)
            ^ (s.len_mm * 31.0) as u64
            ^ (index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15),
    );
    // 墨量 = 宽度 × 深浅。绘图笔只有一种粗细，只能靠「铺几遍」来还原：
    // 又宽又黑的线要横着铺满整条墨带；浅灰的起稿线即使带宽也不能铺，
    // 否则一根轻线会被描成一条重线，整幅画的轻重层次就没了。
    //
    // 结构线（轮廓/转折）要压得比排线重：原稿里它本来就更黑更宽，但投影笔的
    // 宽度是固定的，所以给它铺满一小圈（约 1.6 个笔宽），排线只走一根。
    // 不区分的话，整张图所有线一样重，暗部与轮廓分不出来，视觉上就是平的。
    let base = if s.structural {
        s.mean_width_mm.max(pen_mm * 1.6)
    } else {
        s.mean_width_mm.max(pen_mm)
    };
    let band = base;
    let geom = band / pen_mm.max(0.05);
    let dark_norm = s.mean_dark.clamp(0.0, 1.0);
    let rank = s.rank.clamp(0.0, 1.0);
    let want = geom * (0.55 + 0.45 * dark_norm) + rank * 0.6;
    let passes = if s.passes > 0 {
        s.passes
    } else {
        (want.round() as u32).clamp(1, pressure.clamp(1, 6))
    };

    // 第一遍永远压在中线上：几何位置最准，也保证细线就是一笔。
    render_one(
        &s.pts,
        s.loose,
        rev,
        seed,
        seed,
        jitter_mm,
        0.0,
        0.012,
        true,
        scale,
        out,
    );
    if passes <= 1 || s.len_mm < 1.5 {
        return;
    }
    // 其余各遍把带宽铺满：slot 落在 −0.5..0.5，正好从中线铺到墨带两侧边缘。
    let span = ((band - pen_mm) * 0.5).max(0.0);
    for pass in 1..passes {
        let slot = (pass as f64 + 0.5) / passes as f64 - 0.5;
        let wobble = (hash01(seed ^ (pass as u64).wrapping_mul(0x1656_67B1_9E37_79F9)) - 0.5)
            * (span * 2.0 / passes as f64)
            * 0.8;
        let lateral = slot * 2.0 * span + wobble;
        // 外侧那几遍略短一点，墨带两端自然收口，不会齐齐像刀切。
        let trim = 0.05 + 0.04 * slot.abs() + 0.04 * hash01(seed ^ (pass as u64 + 9).wrapping_mul(0x27d4_eb2f_5165_6771));
        render_one(
            &s.pts,
            [true, true],
            rev ^ (pass % 2 == 1),
            seed,
            seed ^ (pass as u64).wrapping_mul(0xc2b2_ae3d_27d4_eb4f),
            jitter_mm,
            lateral,
            trim,
            false,
            scale,
            out,
        );
    }
}
