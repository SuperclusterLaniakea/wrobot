//! 素描原稿：把图片上真实画出来的铅笔线**一根根提取出来**，而不是照明暗另排一套。
//!
//! 两条通路，按原稿性质自动分流（`SKETCH_PATH` 可强制）：
//! - **线稿**（几乎没有中间调）→ `line_strokes`：沿墨脊逐笔追踪，见该文件内说明；
//! - **色调稿**（有明暗）→ `tonal_strokes` → `ridge_strokes`：提取笔触，见下。
//!
//! # 为什么必须是「提取」而不是「编排」
//!
//! 所有「按明暗重新排一套线」的方案都试过并否决了——调子排线、稀疏补白、
//! 第二轮低门槛追踪，结局一样：画面立刻变成**两层笔迹**，一层跟着形体走、
//! 一层是等距直梳，怎么调都脏。原因是原稿的每根线都是原作画的，位置、粗细、
//! 走向都有理由，编不出来。用户反复反馈"不像"，根子在这里：**之前那一版
//! 根本没在找原稿的线。**
//!
//! # 提取的三步
//!
//! 1. **多尺度 Hessian 暗脊响应**（`ridge_field`）。判据不是「有梯度」，而是
//!    Hessian 两个特征值一大一小——法向曲率大、沿脊几乎不变。一块抹开的阴影
//!    梯度也不小，但它是各向同性的，拿不到响应。所以不需要「画线还是画明暗」
//!    的开关，明暗永远不会变成线。尺度跟着**笔宽**走，不跟着像素走。
//! 2. **阈值化 → Zhang–Suen 细化成单像素骨架**。这一步不能省：早先在响应场上
//!    做非极大值抑制再追线，NMS 沿脊一波动就漏成一串散点，实测每根只能走 4 步,
//!    输出就是满地碎划痕——这是本版之前反复"不像"的直接原因。
//! 3. **沿骨架追踪**成折线，平滑、抽稀，再按「暗处密、亮处疏」拉开间距落笔。
//!
//! # 三条写死的物理约束
//!
//! - **投影笔只有一个墨色**。深浅只能靠疏密，不能靠「铺几遍」（那只会沿同一
//!   轨迹反复摩擦，把纸缝填死）。所以 `cover` 必须随调子增大而**减小**。
//! - **间距硬下限 0.8 笔宽**：0.5mm 的笔，两根线留的缝不到 0.3mm 印出来分不出，
//!   再密只是把纸涂死，不会更黑。
//! - **比笔细十倍的线不找**。原稿由 AI 生成时脸上一层细排线只有 1~2px ≈ 0.05mm，
//!   0.5mm 的笔画不出来；找出来只会加粗成一团黑。所以检测尺度按笔宽定，
//!   不是按像素定。
//!
//! 所有几何都在像素坐标里还原，最后统一乘 `scale` 变成毫米。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

thread_local! {
    static LAST_REASON: RefCell<&'static str> = const { RefCell::new("none") };
    /// 强制走某条通路（0 = 自动，1 = 线稿追踪，2 = 调子排线）。
    /// 测试用：合成图往往「不像」任何一种，靠这里指定，免得测试写死实现细节。
    static FORCE_PATH: Cell<u8> = const { Cell::new(0) };
}

/// 测试用：在本线程强制素描通路（0 自动 / 1 线稿 / 2 调子）。
#[allow(dead_code)]
pub(crate) fn force_path(mode: u8) {
    FORCE_PATH.with(|c| c.set(mode));
}

fn set_reason(r: &'static str) {
    LAST_REASON.with(|c| *c.borrow_mut() = r);
}

use wrobot_core::render::Polyline;

use crate::io::{simplify_path, skeleton_paths, thin_ink};

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
/// 长线（轮廓/起稿）与短线（排线）的分界（毫米）。
const LONG_STROKE_MM: f64 = 12.0;
/// 方向分组：6 个 30° 的扇区。
const DIR_BINS: usize = 6;

/// 墨浓度图 + 「已画掉」的剩余墨。
struct Ink {
    /// 归一化墨浓度 0..1（相对本图纸白与最深墨）。
    tone: Vec<f32>,
    /// 已被某条笔迹占掉的像素。种子落在这里就不再重复描。
    done: Vec<bool>,
    w: u32,
    h: u32,
    /// 判定「这是墨」的阈值。
    thr: f32,
    /// 判定「这仍是墨、只是很淡」的阈值（跨断口、收笔回缩用）。
    weak: f32,
    /// 原始亮度：用来判断某个点是「纯纸」还是「淡墨」。
    /// 平滑会把墨渗进相邻的空白像素，只看 tone 会把纯白留白当成断口接上。
    lum: Vec<u8>,
    /// 空白的亮度基准（普通图是纸白，反相图是底色）。
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

/// 主入口：把一张铅笔稿逐笔还原成「像手画出来」的笔迹。
#[allow(clippy::too_many_arguments)]
pub(crate) fn sketch_polylines(
    gray: &image::GrayImage,
    w: u32,
    h: u32,
    scale: f64,
    threshold: f64,
    invert: bool,
    pressure: u32,
    jitter_mm: f64,
    pen_mm: f64,
    order_mode: u8,
    block_px: u32,
) -> Vec<Polyline> {
    if w == 0 || h == 0 {
        return Vec::new();
    }
    // 单遍落笔宽度 = 实际画笔的线宽。**不能从原稿反推**：取「原稿最细的线」
    // 当笔宽会得出 0.1mm，算法就会以为一笔只能盖 0.1mm、每格塞得下四倍线条。
    // 笔宽是机器的物理属性。
    let pen_mm = pen_mm.clamp(0.15, 1.6);

    // 先看这是哪种稿子。色调稿（有渐变、成片暗部）走排线渲染；
    // 线稿（白纸 + 细线）仍走沿墨脊追踪——那正是追踪器擅长的。
    let field = Field::build(gray, w, h, scale, invert, pen_mm);
    // 通路选择：测试强制 > 环境变量 > 按图的“味道”自动判断
    let from_env = match std::env::var("SKETCH_PATH").ok().as_deref() {
        Some("tone") => 2u8,
        Some("line") => 1,
        _ => 0,
    };
    let tonal = match FORCE_PATH.with(|c| c.get()).max(from_env) {
        2 => true,
        1 => false,
        _ => field.is_tonal(),
    };

    let strokes = if tonal {
        tonal_strokes(&field, pen_mm, threshold, pressure, 0x51ed_2701_b4e9_2f0d)
    } else {
        line_strokes(gray, w, h, scale, threshold, invert, pen_mm, field.mid_frac)
    };
    if strokes.is_empty() {
        return Vec::new();
    }
    finish(strokes, pressure, jitter_mm, pen_mm, order_mode, block_px, scale)
}

// ───────────────────────── 素描调子：灰度分层排线 ─────────────────────────
//
// 和上面的「逐笔提取」是两条相反的路：
// - 原稿提取问「这张纸上原来画了哪些线」，适合线稿；
// - 调子排线问「这块地方有多暗」，再用线条**密度**把它铺出来，适合照片/素描。
//
// 单色笔画不出灰，只能靠「留出多少纸」：
//   亮部 → 一根不画；中间调 → 一层单向稀排线；越暗 → 叠上越多层交叉排线。
// 轮廓另走一条梯度骨架，最后压重，保证脸型、五官不散。

/// 各层排线的方向：第一层沿基准角，往后按这些角差交叉。
/// 不用均匀的 180/N —— 层数变化时每层仍要保持明显不同的走向。
/// 各层方向只交替两个「线族」：真实排线是两组交叉线反复加深，
/// 每层换一个新角度会织成乱网，暗部像一团毛线。
const HATCH_DIR_DEG: [f64; 8] = [0.0, 42.0, 0.0, 42.0, 0.0, 42.0, 0.0, 42.0];

/// 强方向区域（头发、衣褶）里各层的角度偏移。
/// 不能照搬上面的固定交叉角：头发一交叉就织成渔网。这里只偏几度，
/// 既保持沿发丝的走向，又让各层错开、叠得暗。
const HATCH_FLOW_DEG: [f64; 8] = [0.0, 9.0, -8.0, 16.0, -14.0, 5.0, 12.0, -10.0];

/// 一支笔的物理能力。
///
/// `darkness` 不是「笔的颜色有多深」，而是**满覆盖时纸面能压到多深**：
/// HB 反复排满也只有 0.45 左右，8B 可以到 0.95。多笔模式靠这个值把
/// 明暗带分给不同的笔，最后一档黑由最深的笔负责，不再让 HB 用密度硬撑。
#[derive(Clone, Copy, Debug)]
pub struct HatchPen {
    pub width_mm: f64,
    pub darkness: f32,
}

/// 排线输出：笔迹 + 每根笔迹属于哪支笔（按输入的笔组下标）。
pub(crate) struct HatchOut {
    pub polylines: Vec<Polyline>,
    pub pens: Vec<u8>,
}

/// 排线 / 分区重绘共用的调子场。
struct HatchTone {
    /// 对比度校正后的调子 0..1（0 = 纸白，1 = 全画最深）。
    tone: Vec<f32>,
    /// 0.4mm 调子 − 2.2mm 调子：局部比周围暗/亮多少，轮廓层与高光让白都用它。
    detail: Vec<f32>,
    /// 背景 / 纸白掩码。
    bg: Vec<bool>,
    /// 明暗对比系数（各层门槛反算时要用同一个值）。
    contrast: f32,
}

/// 把原稿归一化成「排线用」的调子场。
///
/// 两个尺度是有分工的：
/// - 1.3mm 大调子决定「这一块该排多密」；
/// - 0.4mm 细节减去大调子，得到发丝高光（负）与眼线、唇缝（正），
///   前者让白、后者加黑，模拟「先铺大调子、再抠亮部与暗部」。
fn hatch_tone(field: &Field, gray: &image::GrayImage, invert: bool, threshold: f64) -> HatchTone {
    let mut norm = vec![0f32; field.w * field.h];
    for (i, p) in gray.pixels().enumerate() {
        let v = if invert {
            255.0 - p.0[0] as f32
        } else {
            p.0[0] as f32
        };
        norm[i] = ((field.paper - v) / field.span.max(1e-6)).max(0.0);
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
        detail[i] = fine[i] - field.tone[i];
    }
    let contrast = (1.0 + 0.35 * threshold.clamp(0.05, 0.95)).clamp(1.0, 1.35) as f32;
    // 漫填的「相邻差」用 0.4mm 平滑图算：原始像素的纸纹起伏超过 2% 阈值，
    // 会把背景切成孤岛，细节提取随后把纸纹描得满纸都是。但「是否背景」
    // 仍看原始调子，且要求它明显高于纸白——1.3mm 平滑会把脸的浅调子
    // 与背景连成缓坡，整张脸被漫填成背景（实测覆盖 76.6%，脸全白）。
    let bg = background_mask(&norm, &fine, field.w, field.h);
    let mut tone = vec![0f32; field.w * field.h];
    for (i, (((&t0, &d), &is_bg), &hf)) in tone_hatch
        .iter()
        .zip(detail.iter())
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
        if t0 > 0.45 && hf + 0.12 < t0 && hf < 0.55 {
            let k = ((t0 - hf - 0.12) / 0.30).clamp(0.0, 1.0);
            t *= 1.0 - 0.65 * k;
        }
        let local = if d < 0.0 { d * 0.85 } else { d * 0.30 };
        tone[i] = (t + local).clamp(0.0, 1.0);
    }
    for t in tone.iter_mut() {
        *t = t.powf(contrast);
    }
    HatchTone {
        tone,
        detail,
        bg,
        contrast,
    }
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

/// 素描调子模式主入口：灰度 → 分层排线 + 轮廓重线。
#[allow(clippy::too_many_arguments)]
pub(crate) fn hatch_polylines(
    gray: &image::GrayImage,
    w: u32,
    h: u32,
    scale: f64,
    threshold: f64,
    invert: bool,
    levels: u32,
    jitter_mm: f64,
    pen_mm: f64,
    order_mode: u8,
    block_px: u32,
    base_angle_deg: f64,
    spacing_px: u32,
    edge_threshold: f64,
    pens: &[HatchPen],
    multi_pen: bool,
) -> HatchOut {
    if w == 0 || h == 0 {
        return HatchOut {
            polylines: Vec::new(),
            pens: Vec::new(),
        };
    }
    let pen_mm = pen_mm.clamp(0.15, 1.6);
    let levels = levels.clamp(3, 8) as usize;
    let field = Field::build(gray, w, h, scale, invert, pen_mm);
    let ht = hatch_tone(&field, gray, invert, threshold);
    let tone_c = &ht.tone;
    let contrast = ht.contrast;
    let need: Vec<f32> = tone_c.iter().map(|t| t * levels as f32).collect();
    // 两根线最近也要留 3.2 个笔宽：单色笔的「黑」全靠线密度，
    // 间距再小一层就盖住 40%，六层叠完全是死黑，纸白全丢。
    let spacing_mm = (spacing_px.max(1) as f64 * scale).clamp(pen_mm * 3.2, pen_mm * 7.0);
    let base = base_angle_deg.to_radians();

    // 多笔：至少两支笔且层数够分时才启用。比层数还多的笔没有层可挂，
    // 强行用会让最深那支一笔都轮不到，黑场永远上不去。
    let m_eff = if multi_pen {
        pens.len().min(4).min(levels).max(1)
    } else {
        1
    };
    let mut layers: Vec<Vec<Stroke>> = Vec::with_capacity(levels);
    let mut layer_pen: Vec<u8> = Vec::with_capacity(levels);
    let mut extracted: Option<Vec<Vec<Stroke>>> = None;
    if m_eff > 1 {
        // **多笔首先走「提取原稿真实笔触」**：每一支笔画的都是原画里存在的
        // 线，而不是照明暗另排一套。换笔只决定「这段明暗由哪支笔来画」，
        // 叠加后仍然贴着原稿；提取不到足够笔触（照片、纯色块）才回落排线。
        extracted = extracted_multi_pen(&field, threshold, pen_mm, m_eff);
    }
    if m_eff > 1 && extracted.is_none() {
        // 明暗带的起点沿用单笔第 0 层的门槛：调高「明暗对比」时，
        // 连最浅的一支笔也更晚介入，亮部留给纸白。
        let light_gate = ((0.45 + 1.2 * threshold as f32) / levels as f32)
            .powf(1.0f32 / contrast)
            .clamp(0.12, 0.42);
        // 带的分界按各笔能压出的最深色分配：8B 比 HB 多出的那段深色，
        // 正好由它自己负责，而不是让 HB 排到死黑。
        let ds: Vec<f32> = pens[..m_eff]
            .iter()
            .map(|p| p.darkness.clamp(0.2, 1.0))
            .collect();
        let dsum: f32 = ds.iter().sum::<f32>().max(1e-6);
        let mut bounds = Vec::with_capacity(m_eff + 1);
        bounds.push(light_gate);
        let mut acc = 0.0f32;
        for &d in &ds {
            acc += d;
            bounds.push(light_gate + (1.0 - light_gate) * (acc / dsum));
        }
        if let Some(last) = bounds.last_mut() {
            *last = 1.0;
        }
        // 层按顺序连续分配给各笔（亮 → 暗）。每支笔至少一层，层数多于笔数时
        // 按比例多分，让每支笔都能在自己的明暗带里叠出层次。
        let mut counts = [0usize; 4];
        for k in 0..levels {
            counts[k * m_eff / levels] += 1;
        }
        let mut seen = [0usize; 4];
        for k in 0..levels {
            let p = k * m_eff / levels;
            let j = seen[p];
            seen[p] += 1;
            let spec = pens[p];
            layers.push(pen_hatch_layer(
                &field,
                tone_c,
                bounds[p],
                bounds[p + 1],
                spec.width_mm.clamp(0.15, 1.6),
                spec.darkness.clamp(0.2, 1.0),
                counts[p],
                j,
                k,
                base,
                scale,
                contrast,
            ));
            layer_pen.push(p as u8);
        }
        if std::env::var("SKETCH_DEBUG").is_ok() {
            eprintln!(
                "多笔分带（回落排线）：{} 支笔，起点 {:.2}，边界 {:?}，每笔层数 {:?}",
                m_eff,
                light_gate,
                bounds.iter().map(|v| (v * 100.0).round() / 100.0).collect::<Vec<_>>(),
                &counts[..m_eff]
            );
        }
    } else if m_eff == 1 {
        for k in 0..levels {
            // 第 k 层在「至少需要 k+1 层才铺得出来」的地方起笔。
            // 第 0 层的门槛跟「明暗对比」走：调高就把浅灰整片留给纸白，
            // 亮面只由轮廓层交代；调低则连浅灰也铺一层稀排线。
            let want = if k == 0 {
                0.45 + 1.2 * threshold as f32
            } else {
                k as f32 + 0.35
            };
            layers.push(hatch_layer(
                &field,
                &need,
                want,
                k,
                base,
                spacing_mm,
                scale,
                pen_mm,
                levels,
                contrast,
            ));
            layer_pen.push(0);
        }
    }
    // 轮廓始终归最深的一支笔：它是画面里最重的一层，用浅笔会立刻糊掉。
    let dark_pen = (m_eff - 1) as u8;
    let dark_width = if m_eff > 1 {
        pens[m_eff - 1].width_mm.clamp(0.15, 1.6)
    } else {
        pen_mm
    };
    // 提取通路里结构线已经在笔迹里（并已分给最深笔），再叠一遍轮廓会画成
    // 两根平行的重线；只有回落到合成排线时才需要单独提轮廓。
    let structural = if extracted.is_some() {
        Vec::new()
    } else {
        contour_strokes(&field, &ht.detail, &ht.bg, scale, dark_width, edge_threshold)
    };
    if std::env::var("SKETCH_DEBUG").is_ok() {
        let counts: Vec<usize> = layers.iter().map(|l| l.len()).collect();
        eprintln!(
            "调子排线：{} 层 {:?}，笔 {:?}，轮廓 {} 条，间距 {:.2}mm",
            levels,
            counts,
            layer_pen,
            structural.len(),
            spacing_mm
        );
    }
    let mut out = Vec::new();
    let mut pens_out = Vec::new();
    let mut n = 0usize;
    // 调试开关：只看轮廓层，判断五官丢在哪一步。
    let only_contour = std::env::var("SKETCH_ONLY").ok().as_deref() == Some("contour");
    if !only_contour {
        if let Some(lanes) = &extracted {
            // 浅笔 → 深笔，每支笔一层；层内按选定的走笔顺序串联。
            for (p, lane) in lanes.iter().enumerate() {
                let w = pens[p].width_mm.clamp(0.15, 1.6);
                emit_hatch_set(
                    lane,
                    order_mode,
                    block_px,
                    scale,
                    jitter_mm,
                    w,
                    p as u8,
                    &mut n,
                    &mut out,
                    &mut pens_out,
                );
            }
        } else {
            for (layer, &pen) in layers.iter().zip(layer_pen.iter()) {
                let w = if m_eff > 1 {
                    pens[pen as usize].width_mm.clamp(0.15, 1.6)
                } else {
                    pen_mm
                };
                emit_hatch_set(
                    layer,
                    order_mode,
                    block_px,
                    scale,
                    jitter_mm,
                    w,
                    pen,
                    &mut n,
                    &mut out,
                    &mut pens_out,
                );
            }
        }
    }
    // 轮廓最后压上：排线可以穿过形体，但轮廓必须是最清楚的一层。
    emit_hatch_set(
        &structural,
        order_mode,
        block_px,
        scale,
        jitter_mm,
        dark_width,
        dark_pen,
        &mut n,
        &mut out,
        &mut pens_out,
    );
    HatchOut {
        polylines: out,
        pens: pens_out,
    }
}

/// 分区重绘：把黑白深度量化成 N 层，细粒度分块决定排线方向，
/// 每层由一支笔在自己的深度带里铺线，逐层叠到最深。
///
/// 和模式 8 的区别：
/// - 模式 8 是「按调子连续排线」，一层里整片区域同向；
/// - 分区重绘先量出「这块到底有多黑」，落到第几层，再按小块的**局部走向**
///   排该层的线。亮的层先铺、暗的层后叠，最后最深的一支笔压轮廓——
///   这正是铅笔素描「先浅后深、一层层压暗」的顺序。
///
/// 后几层只画在更暗的地方（`lvl >= k`），所以叠加后不会把亮部一起涂黑；
/// 各层用不同交叉角，叠出来的暗部才有铅笔的网状质感。
#[allow(clippy::too_many_arguments)]
pub(crate) fn region_polylines(
    gray: &image::GrayImage,
    w: u32,
    h: u32,
    scale: f64,
    threshold: f64,
    invert: bool,
    levels: u32,
    block_mm: f64,
    jitter_mm: f64,
    pen_mm: f64,
    order_mode: u8,
    order_block_px: u32,
    base_angle_deg: f64,
    edge_threshold: f64,
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
    let pen_mm = pen_mm.clamp(0.15, 1.6);
    let levels = levels.clamp(3, 8) as usize;
    let block_mm = block_mm.clamp(1.5, 12.0);
    let field = Field::build(gray, w, h, scale, invert, pen_mm);
    let ht = hatch_tone(&field, gray, invert, threshold);
    let fw = field.w;
    let fh = field.h;
    let npix = fw * fh;
    let m_eff = if multi_pen {
        pens.len().min(4).min(levels).max(1)
    } else {
        1
    };
    // 第 0 层的起点：和模式 8 用同一套门槛，亮部照样留给纸白。
    let gate = ((0.45 + 1.2 * threshold as f32) / levels as f32)
        .powf(1.0f32 / ht.contrast)
        .clamp(0.10, 0.42);
    // 层号要按**这张图自己的调子范围**摊开，不能拿固定的 0..1：
    // 铅笔稿的深部通常只到 0.5~0.7（线条之间的纸白把 1.3mm 均值拉亮了），
    // 若按 0..1 分层，5 层里往往只有前两层用得上，深笔永远轮不到。
    // 取非纸白像素的 p98 当作「这张图最黑的地方」，门槛以上均分成 levels 档。
    let mut tops: Vec<f32> = (0..npix)
        .filter(|&i| !ht.bg[i] && ht.tone[i] >= gate)
        .map(|i| ht.tone[i])
        .collect();
    tops.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let tone_hi = if tops.is_empty() {
        (gate + 0.06).min(1.0)
    } else {
        tops[(tops.len() * 98 / 100).min(tops.len() - 1)]
            .max(gate + 0.06)
            .min(1.0)
    };
    // 逐像素深度层：0 最浅 … levels-1 最深；255 = 纸白/背景，不参与。
    // 用像素级（而不是块均值）定层，块的颗粒度只决定方向与起笔节奏，
    // 这样眼窝、唇缝这些只有一两毫米的深色不会因为块平均而被抹浅。
    let mut lvl = vec![255u8; npix];
    for i in 0..npix {
        if ht.bg[i] {
            continue;
        }
        let t = ht.tone[i];
        if t < gate {
            continue;
        }
        let u = (t - gate) / (tone_hi - gate).max(1e-6);
        lvl[i] = ((u * levels as f32).floor() as usize).min(levels - 1) as u8;
    }
    // 每个小块采样一次原稿的笔触 / 结构走向，块内所有层共用。
    // 块要细（默认 2.5mm）：同一张脸上，鼻梁、脸颊、眼窝的排线方向本来就不同。
    let px_block = (block_mm / field.px_mm.max(1e-9)).round().max(2.0) as usize;
    let bw = (fw + px_block - 1) / px_block;
    let bh = (fh + px_block - 1) / px_block;
    let mut bdir = vec![0f32; bw * bh];
    let mut bgate = vec![0f32; bw * bh];
    let base = base_angle_deg.to_radians();
    for by in 0..bh {
        for bx in 0..bw {
            let x = ((bx * px_block) as f64 + px_block as f64 * 0.5).min(fw as f64 - 1.0);
            let y = ((by * px_block) as f64 + px_block as f64 * 0.5).min(fh as f64 - 1.0);
            let (ra, rc) = field.ridge_ang_at(x, y);
            let (sa, sc) = field.ang_at(x, y);
            let cseed = hash_u64(
                (bx as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ ((by as u64) << 32),
            );
            // 原稿有明确笔触方向就参考原稿（头发、衣褶、排线稿）；
            // 只有成片明暗时退回结构方向；两者都不可信就用规则斜排线，
            // 但每个块错开一点角度——手不会每块都用同一把尺子。
            let (dir, g) = if rc > 0.30 {
                (ra, ((rc - 0.30) / 0.45).clamp(0.0, 1.0))
            } else if sc > 0.45 {
                (sa, ((sc - 0.45) / 0.40).clamp(0.0, 1.0))
            } else {
                (base as f32 + (hash01(cseed) as f32 - 0.5) * 0.16, 0.0)
            };
            let i = by * bw + bx;
            bdir[i] = dir;
            bgate[i] = g;
        }
    }
    let base_ppm = field.px_mm.max(1e-9);
    let pen_of = |k: usize| -> usize {
        if m_eff <= 1 {
            0
        } else {
            (k * m_eff / levels).min(m_eff - 1)
        }
    };
    let only_contour = std::env::var("SKETCH_ONLY").ok().as_deref() == Some("contour");
    let mut stroke_no = 0usize;
    let mut hatch_len = 0.0f64;
    for k in 0..levels {
        let p = pen_of(k);
        let (pw, pdark) = if m_eff > 1 {
            let spec = pens[p];
            (
                spec.width_mm.clamp(0.15, 1.6),
                spec.darkness.clamp(0.2, 1.0),
            )
        } else {
            (pen_mm, 1.0)
        };
        let min_mm = pw * 2.4;
        let pitch = ((min_mm * 0.82) / base_ppm).max(2.0);
        let mut used = vec![false; npix];
        let mut layer: Vec<Stroke> = Vec::new();
        let seed = 0x51ed_2701_b4e9_2f0d ^ (k as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let seed_off = 0.29 + 0.23 * k as f64;
        let mut gy = 0i32;
        loop {
            let y = gy as f64 * pitch + seed_off;
            if y >= fh as f64 {
                break;
            }
            gy += 1;
            let stagger = if gy & 1 == 0 { 0.0 } else { pitch * 0.5 };
            let mut gx = 0i32;
            loop {
                let x = gx as f64 * pitch + stagger + seed_off * 0.3;
                if x >= fw as f64 {
                    break;
                }
                gx += 1;
                let idx = (y as usize) * fw + (x as usize);
                if used[idx] || lvl[idx] < k as u8 {
                    continue;
                }
                let t = ht.tone[idx];
                let q = lvl[idx] as usize;
                let sseed = hash_u64(
                    (x as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
                        ^ ((y as i64 as u64) << 1)
                        ^ seed,
                );
                let bi = (y as usize / px_block) * bw + (x as usize / px_block);
                // 后面各层要**真的和第一层交叉**：把本层的交叉角同时加到
                // 规则角和「跟随原稿」角上。只加在规则角上的话，原稿笔触方向
                // 明确时（实拍素描的排线、头发），跟随角占主导，每一层都顺着
                // 原稿走，叠多少层都只是同一方向的平行线，暗部永远不出网。
                let cross = HATCH_DIR_DEG[k.min(HATCH_DIR_DEG.len() - 1)].to_radians();
                let rule = base + cross;
                let follow = bdir[bi] as f64
                    + cross
                    + HATCH_FLOW_DEG[k.min(HATCH_FLOW_DEG.len() - 1)].to_radians();
                let th = lerp_dir(rule, follow, bgate[bi] as f64);
                // 一笔的长度：亮处短、暗处长一点，再乘个体差异，避免排成一个模子。
                let len_mm = (2.5 + 4.5 * t as f64) * (0.6 + 0.55 * hash01(sseed));
                let s_max = len_mm / base_ppm;
                let fwd = trace_region(
                    &field,
                    &lvl,
                    &ht.tone,
                    (x, y),
                    1.0,
                    th,
                    k as u8,
                    gate,
                    s_max,
                    sseed,
                );
                let bwd = trace_region(
                    &field,
                    &lvl,
                    &ht.tone,
                    (x, y),
                    -1.0,
                    th,
                    k as u8,
                    gate,
                    s_max,
                    sseed,
                );
                if fwd.len() + bwd.len() < 2 {
                    continue;
                }
                let mut pts: Vec<(f64, f64)> = Vec::with_capacity(fwd.len() + bwd.len() + 1);
                for &q in bwd.iter().rev() {
                    pts.push(q);
                }
                pts.push((x, y));
                pts.extend(fwd.iter().copied());
                let len_px = chain_len(&pts);
                if len_px < (1.6 / base_ppm).max(2.0) {
                    continue;
                }
                // 这一条线覆盖的调子 → 本层要补的覆盖率 → 实际线距。
                // 层越深、层数越多，单层要补的越少，线自然越稀；
                // 暗部层多、每层再按笔的满覆盖深度折算，铅笔的深浅就出来了。
                let mut sum = 0.0f64;
                let mut cnt = 0usize;
                for &(px, py) in &pts {
                    let i = (py as usize).min(fh - 1) * fw + (px as usize).min(fw - 1);
                    sum += ht.tone[i] as f64;
                    cnt += 1;
                }
                let mean_t = (sum / cnt.max(1) as f64).clamp(0.0, 1.0) as f32;
                let n_act = (q + 1).min(levels) as f32;
                let want = (1.0 - (1.0 - mean_t).powf(1.0 / n_act)).clamp(0.02, 0.55);
                let cov = (want / pdark).clamp(0.03, 0.58) as f64;
                let local_mm = (pw / cov).clamp(min_mm, pw * 18.0);
                let mark_r = ((local_mm / base_ppm).round() as i32).max(1);
                let r2 = mark_r * mark_r;
                let step = (mark_r / 2).max(1) as usize;
                for &(px, py) in pts.iter().step_by(step) {
                    let cx = px.round() as i32;
                    let cy = py.round() as i32;
                    for yy in (cy - mark_r).max(0)..=(cy + mark_r).min(fh as i32 - 1) {
                        for xx in (cx - mark_r).max(0)..=(cx + mark_r).min(fw as i32 - 1) {
                            let dx = xx - cx;
                            let dy = yy - cy;
                            if dx * dx + dy * dy <= r2 {
                                used[(yy as usize) * fw + xx as usize] = true;
                            }
                        }
                    }
                }
                layer.push(Stroke {
                    theta: path_theta(&pts),
                    pts,
                    loose: [true, true],
                    len_mm: len_px * base_ppm,
                    mean_dark: mean_t as f64,
                    mean_width_mm: pw * 0.9,
                    rank: k as f64 / (levels - 1).max(1) as f64,
                    structural: false,
                    passes: 1,
                });
            }
        }
        if std::env::var("SKETCH_DEBUG").is_ok() {
            eprintln!("分区重绘：层 {k} → 笔 {p}，{} 条", layer.len());
        }
        hatch_len += layer.iter().map(|s| s.len_mm).sum::<f64>();
        if !only_contour {
            emit_hatch_set(
                &layer,
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
    }
    // 轮廓归最深的笔，最后压一遍，五官边界不散。
    let dark_pen = (m_eff - 1) as u8;
    let dark_width = if m_eff > 1 {
        pens[m_eff - 1].width_mm.clamp(0.15, 1.6)
    } else {
        pen_mm
    };
    // 结构层（最后压的最深一支笔）。优先用**墨脊中线**，而不是等值轮廓：
    // 轮廓会把原稿每一笔的两侧边缘各描一遍，排线稿上立刻变成成对的重复线
    // （实测长竖线全是双线）。墨脊提取每条线只给一根中线，既保住形体，
    // 又不会把原稿复制两遍。照片/纯色块没有可靠墨脊时，才退回轮廓。
    let ridge_mid = {
        let (strokes, _) = ridge_strokes(&field, dark_width, threshold, 1.4);
        strokes
            .into_iter()
            .filter(|s| s.structural && s.len_mm >= 3.0)
            .collect::<Vec<_>>()
    };
    let ridge_len: f64 = ridge_mid.iter().map(|s| s.len_mm).sum();
    let mut structural = if ridge_mid.len() >= 8 && ridge_len >= 40.0 {
        ridge_mid
    } else {
        contour_strokes(&field, &ht.detail, &ht.bg, scale, dark_width, edge_threshold)
    };
    // 结构层只做「形的骨架」，不能把原稿每一笔的边缘都重复描一遍：
    // 按长度×墨色保留最重要的那部分，总长不超过排线总长的 45%——
    // 足够压住五官和物体边界，又不至于盖过调子层。
    let budget = (hatch_len * 0.45).max(120.0);
    let total: f64 = structural.iter().map(|s| s.len_mm).sum();
    if total > budget {
        structural.sort_by(|a, b| {
            (b.len_mm * b.mean_dark)
                .partial_cmp(&(a.len_mm * a.mean_dark))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut acc = 0.0f64;
        structural.retain(|s| {
            if acc + s.len_mm > budget && acc > 0.0 {
                return false;
            }
            acc += s.len_mm;
            true
        });
        if std::env::var("SKETCH_DEBUG").is_ok() {
            eprintln!(
                "分区重绘：轮廓 {total:.0}mm 超出预算 {budget:.0}mm，保留 {} 条",
                structural.len()
            );
        }
    }
    if !only_contour {
        emit_hatch_set(
            &structural,
            order_mode,
            order_block_px,
            scale,
            jitter_mm,
            dark_width,
            dark_pen,
            &mut stroke_no,
            &mut out,
            &mut pens_out,
        );
    }
    HatchOut {
        polylines: out,
        pens: pens_out,
    }
}

/// 分区重绘的一步：沿排线方向走，碰到更浅的层或纸白就收笔。
#[allow(clippy::too_many_arguments)]
fn trace_region(
    field: &Field,
    lvl: &[u8],
    tone: &[f32],
    start: (f64, f64),
    sign: f64,
    theta: f64,
    level: u8,
    gate: f32,
    max_len_px: f64,
    seed: u64,
) -> Vec<(f64, f64)> {
    let w = field.w as i32;
    let h = field.h as i32;
    let mut out = Vec::new();
    let (mut x, mut y) = start;
    let (mut dx, mut dy) = hatch_dir(theta, x, y, seed);
    dx *= sign;
    dy *= sign;
    let mut walked = 0.0f64;
    while walked < max_len_px {
        let nx = x + dx;
        let ny = y + dy;
        if nx < 0.0 || ny < 0.0 || nx >= w as f64 || ny >= h as f64 {
            break;
        }
        let ix = nx as i32;
        let iy = ny as i32;
        let i = (iy as usize) * w as usize + ix as usize;
        // 只在本层及更暗的地方走：走到亮部就收笔，层与层之间才有边界。
        if lvl[i] < level || tone[i] < gate {
            break;
        }
        let (ndx, ndy) = hatch_dir(theta, nx, ny, seed);
        let (mut sx, mut sy) = (
            dx * 0.82 + ndx * sign * 0.18,
            dy * 0.82 + ndy * sign * 0.18,
        );
        let l = (sx * sx + sy * sy).sqrt();
        if l < 1e-9 {
            break;
        }
        sx /= l;
        sy /= l;
        dx = sx;
        dy = sy;
        x = nx;
        y = ny;
        walked += 1.0;
        out.push((x, y));
    }
    out
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

/// 一层最多补多少覆盖率。素描的暗部是靠「一层层交叉排线」压出来的，
/// 每一层本身要留出纸缝：一层就压满会变成实心黑块，后面的交叉层也无缝可补，
/// 画面读起来只有一层。0.80 让单层线距约 1.25 倍笔宽（HB 约 0.38mm），
/// 再靠 2~3 个不同角度的交叉层叠出深调——线更细、更密、层次更多。
const STROKE_PASS_MAX_COV: f64 = 0.80;

/// 各遍的角差（度）。第 0 遍顺形体/原稿排线方向，之后每层错开一点角度。
///
/// 不要用 38°/-34° 那种大交叉：实测原稿额头/脖子的排线方向一致性 R≈0.2~0.5，
/// 大交叉叠完后输出的方向一致性掉到 R≈0.07——一片六七个方向的乱网，
/// 正是用户说的「间距乱、像划痕」。但也不能只错 4°~17°：那样每层几乎同向，
/// 叠完读起来只有一层（用户「明暗调子的重叠…只有一层」）。原稿本身是
/// 2~3 层同向为主、层间错 10°~25° 的排线，这里按同样幅度分档。
const STROKE_CROSS_DEG: [f64; 8] = [0.0, 15.0, -13.0, 8.0, 24.0, -20.0, 11.0, -9.0];
const STROKE_CONTOUR_DEG: [f64; 8] = [0.0, 4.0, -4.0, 6.0, -6.0, 3.0, -3.0, 2.0];

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
    edge_threshold: f64,
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
    let pen_mm = pen_mm.clamp(0.15, 1.6);
    let passes = passes.clamp(2, 8) as usize;
    let stroke_len = stroke_len_mm.clamp(1.5, 8.0);
    let follow = follow_mode.min(2);
    let field = Field::build(gray, w, h, scale, invert, pen_mm);
    let ht = hatch_tone(&field, gray, invert, threshold);
    let fw = field.w;
    let fh = field.h;
    let npix = fw * fh;
    let px_mm = field.px_mm.max(1e-9);
    let base = base_angle_deg.to_radians();
    // 局部「线条密度」：细尺度脊响应在 1.5mm 内的平均。它量的不是明暗，
    // 而是「这片地方原稿排了多少线」——浅而密的排线调子很低，但密度很高，
    // 只按调子反算间距会把它们排成大间距的深线，完全不是原稿的样子。
    let r_tex = ((1.5 / px_mm).round() as usize).clamp(2, 24);
    let ridge_cap: Vec<f32> = field.ridge.iter().map(|&r| r.min(0.12)).collect();
    let tex = box_blur(&ridge_cap, fw, fh, r_tex);
    if std::env::var("SKETCH_DEBUG").is_ok() {
        let mut tv: Vec<f32> = (0..npix)
            .filter(|&i| !ht.bg[i] && ht.tone[i] > 0.004)
            .map(|i| tex[i])
            .collect();
        tv.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let q = |f: f64| {
            if tv.is_empty() {
                0.0
            } else {
                tv[((tv.len() - 1) as f64 * f) as usize]
            }
        };
        eprintln!(
            "纹理密度：p10 {:.4} p25 {:.4} p50 {:.4} p75 {:.4} p90 {:.4} p98 {:.4}（场 {}×{}，px_mm {:.3}）",
            q(0.10),
            q(0.25),
            q(0.50),
            q(0.75),
            q(0.90),
            q(0.98),
            fw,
            fh,
            px_mm
        );
        for (x, y, label) in [
            (500usize, 600usize, "额头"),
            (400, 1200, "左脸"),
            (700, 700, "脸颊"),
            (100, 400, "背景左上"),
            (900, 400, "头发"),
            (800, 1300, "右脸"),
            (960, 1700, "颈"),
            (1190, 330, "发右"),
            (480, 850, "颈半"),
            (350, 475, "额半"),
            (240, 1100, "左颊半"),
            (700, 350, "发1"),
            (780, 400, "发2"),
            (820, 480, "发3"),
            (760, 560, "发4"),
            (880, 330, "发5"),
            (700, 480, "发6"),
            (620, 420, "发7"),
            (390, 1045, "衬衫左"),
            (430, 1085, "衬衫中"),
            (480, 1140, "衬衫下"),
            (515, 1060, "衬衫右"),
            (250, 700, "左脸"),
            (330, 760, "面中"),
        ] {
            if x < fw && y < fh {
                let i = y * fw + x;
                let (a1, c1) = field.ang_at(x as f64, y as f64);
                let (a2, c2) = field.smooth_ridge_ang_at(x as f64, y as f64);
                let (a3, c3) = field.broad_ridge_ang_at(x as f64, y as f64);
                let (a4, c4) = stroke_dir(&field, follow, base, x as f64, y as f64);
                eprintln!(
                    "  纹理 {label} ({x},{y})：调子 {:.3} 脊 {:.4} 归一脊 {:.3} 密度 {:.4} | \
                     等照 {:.0}°/{:.2} 脊向 {:.0}°/{:.2} 块向 {:.0}°/{:.2} 合成 {:.0}°/{:.2}",
                    field.tone[i],
                    field.ridge[i],
                    field.nridge[i],
                    tex[i],
                    a1.to_degrees(),
                    c1,
                    a2.to_degrees(),
                    c2,
                    a3.to_degrees(),
                    c3,
                    a4.to_degrees(),
                    c4
                );
            }
        }
    }

    // —— 把调子摊到这张图自己的范围上 ——
    //
    // 素描稿的「最黑」通常只有 0.5~0.8：线条之间的纸白把 1.3mm 均值拉亮了。
    // p98 归一化后再留一个很低的死区挡纸纹，其余摊开；threshold 只调对比。
    let mut tops: Vec<f32> = (0..npix)
        .filter(|&i| !ht.bg[i] && ht.tone[i] > 0.004)
        .map(|i| ht.tone[i])
        .collect();
    tops.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let tone_hi = if tops.is_empty() {
        0.10
    } else {
        tops[(tops.len() * 98 / 100).min(tops.len() - 1)]
            .max(0.06)
            .min(1.0)
    };
    // 死区只挡纸纹。原稿左脸实测只有 0.031 的浅调子（还有可见排线），
    // 0.015 的死区 + 0.015 的起笔门槛正好把它吃干净，脸就白了。
    // 纸白本身是 0.000，靠背景掩码排除；这里只留 0.5% 给扫描噪点。
    let dead = tone_hi * 0.005;
    // 调子曲线：gamma < 1 才留得住中间调。
    //
    // 上一版用 0.82+0.70*threshold（默认 1.32），把脸颊 0.1 的调子压到 0.05：
    // 实测亮档墨量只有原稿的 23%，脸像没画。素描的中间调本来就是靠
    // 「细而密的浅排线」堆出来的，压暗曲线等于把这一层直接抹掉。
    // 门槛仍留给用户调对比，但映射回到 0.70~0.95，最亮档也有肉。
    let gamma = 0.70 + 0.25 * threshold as f32;
    if std::env::var("SKETCH_DEBUG").is_ok() && !tops.is_empty() {
        let q = |f: f64| tops[((tops.len() - 1) as f64 * f) as usize];
        eprintln!(
            "素描笔触：有效调子 p10 {:.3} p25 {:.3} p50 {:.3} p75 {:.3} p90 {:.3} p98 {:.3}；死区 {dead:.3}",
            q(0.10), q(0.25), q(0.50), q(0.75), q(0.90), q(0.98)
        );
    }
    let mut target = vec![0f32; npix];
    for i in 0..npix {
        if ht.bg[i] {
            continue;
        }
        let u = ((ht.tone[i] - dead) / (tone_hi - dead).max(1e-6)).clamp(0.0, 1.0);
        let by_tone = u.powf(gamma);
        // 纹理密度给出的下限：原稿这里排了多少线，就用细笔排多密。
        // 只按平均调子算，浅而密的排线（脸）会被当成「几乎白」而排成大间距
        // 的深线，稀疏得完全不像原稿；只按纹理算，暗部又不够黑。两者取大。
        let by_tex = (((tex[i] - 0.006) / 0.030).clamp(0.0, 1.0)).powf(0.8) * 0.17;
        // 浅调子的「肉」：0.02~0.15 的柔和阴影在原稿里是一层看得见的细排线，
        // 但幂映射后低于起笔门槛，脸会大片留白。给浅调一个缓坡下限，
        // 只作用于明确高于纸纹的像素（>0.018），纸白/背景仍然不落笔。
        //
        // 这里不能再用 sqrt：开方会把 0.02 的浅调抬到 0.2 以上，而且很早就
        // 撞上一层固定的天花板——脸、脖子、嘴唇全被抬成同一档灰，原稿的
        // 「浅→中」层次就没了（实测脖子、嘴唇的墨量是原稿的 1.9~2.2 倍）。
        // 改成单调缓坡：浅处只微微提，越深提得越多，层次跟着调子走。
        let by_shade = if ht.tone[i] > 0.018 {
            by_tone.powf(0.80) * 0.72
        } else {
            0.0
        };
        // 最黑处留一点纸，并且把「大片深发」和「真重音」分开：
        // 实测按 0.78 排线，6mm 格的暗部墨量是原稿的 1.4 倍，头发比原稿还黑。
        // 大片深发压到 0.46：发丛主体只让 HB+2B 细排。4B 0.8mm、8B 1.0mm
        // 铺在整片头发上就是一根根粗黑杠，原稿的头发是 0.2mm 细线密的；
        // 粗笔只留瞳孔/眼线（>0.90 → 0.88）。
        let cap = if ht.tone[i] > 0.90 {
            0.88
        } else if ht.tone[i] > 0.70 {
            0.66
        } else {
            0.88
        };
        target[i] = by_tone.max(by_tex).max(by_shade).min(cap);
    }
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
    if std::env::var("SKETCH_DEBUG").is_ok() {
        for (x, y, label) in [
            (300usize, 450usize, "左额上"),
            (350, 650, "左额"),
            (330, 850, "左颊"),
            (430, 800, "左颊内"),
            (600, 950, "鼻梁"),
            (800, 950, "右颊"),
            (900, 500, "发中"),
            (1000, 700, "发右"),
            (350, 750, "左眼"),
            (500, 745, "右眼"),
            (460, 745, "右眼内"),
        ] {
            if x < fw && y < fh {
                let i = y * fw + x;
                let (sa, sc) = field.smooth_ridge_ang_at(x as f64, y as f64);
                let (ia, ic) = field.ang_at(x as f64, y as f64);
                let (ba, bc) = field.broad_ridge_ang_at(x as f64, y as f64);
                let (ua, uc) = stroke_dir(&field, follow, base, x as f64, y as f64);
                eprintln!(
                    "  目标 {label} ({x},{y})：调子 {:.3} 归一脊 {:.3} 密度 {:.4} 背景 {} → 目标 {:.3}；脊向 {:.0}°/{:.2} 等照 {:.0}°/{:.2} 宽向 {:.0}°/{:.2} → 用 {:.0}°/{:.2}",
                    ht.tone[i],
                    field.nridge[i],
                    tex[i],
                    ht.bg[i],
                    target[i],
                    sa.to_degrees(),
                    sc,
                    ia.to_degrees(),
                    ic,
                    ba.to_degrees(),
                    bc,
                    ua.to_degrees(),
                    uc
                );
            }
        }
    }

    // 纸白门槛：只挡纸面与背景，不再把整张脸一起挡掉。
    //
    // 上一版按 0.05+0.22*threshold（约 0.21）设门槛，而这张稿子的脸颊
    // 归一化调子只有 0.09：整张脸都低于门槛，浅笔（HB/2B）一笔都落不下去，
    // 最后只剩 8B 在黑发上压出粗条。门槛回到纸纹量级后，浅调靠缺口自然排疏，
    // 多笔分层也才真正生效。
    let paper_gate = 0.006f32;

    // —— 多笔：按各笔满覆盖深度分带，浅笔先补它那一档 ——
    let m_eff = if multi_pen {
        pens.len().min(4).max(1)
    } else {
        1
    };
    let mut bounds = Vec::with_capacity(m_eff + 1);
    bounds.push(0.0f32);
    if m_eff > 1 {
        // 分界按各笔自己的深度：HB 补到 0.45、2B 到 0.65、4B 到 0.82、
        // 8B 收尾。同样的墨量，0.3mm 的线比 0.5mm 的线细一倍——用户要的
        // 「线更细、更密」只能靠让最细的 HB 多干活：目标覆盖率不变时，
        // 细笔要多排一倍行数才够墨，密度自然上去，单根线却更细。
        //
        // 之前用过两个映射，都让粗笔过早接管：
        // - 按用墨量正比（HB 0.16 / 2B 0.38 / 4B 0.67 / 8B 1.0）：8B 从 0.67
        //   就接管，整片头发被 1mm 黑条盖死，细发丝全丢；
        // - 按 1/笔深（0.37 / 0.63 / 0.83 / 1.0）：HB/2B 被推到 0.83，
        //   脸和衬衫为了补足墨量反复叠线，画面发脏。
        for p in &pens[..m_eff] {
            bounds.push(p.darkness.clamp(0.2, 1.0));
        }
        if let Some(last) = bounds.last_mut() {
            *last = 1.0;
        }
        if m_eff == 4 {
            // 分界按「每支笔铺到这一档时覆盖率还在排线范围（0.8 左右）」来定，
            // 而不是按笔的浓度。按浓度分（0.42/0.62/0.80/1.0）时，HB 为了够
            // 0.42 要在暗部铺满 100% 覆盖——画面变成灰饼、笔数爆炸（HB 一条
            // 笔就 25 万毫米），4B/8B 反而被预算挤掉，最深的头发全靠 HB 的 150
            // 灰撑着。改成 0.34/0.52/0.70 后，HB 在 0.81 覆盖处交棒，深发
            // 由 4B 在 0.84 覆盖处接手，8B 才轮得到瞳孔和发芯。
            bounds[1] = 0.34;
            bounds[2] = 0.52;
            bounds[3] = 0.70;
        }
    } else {
        bounds.push(1.0);
    }

    let only_contour = std::env::var("SKETCH_ONLY").ok().as_deref() == Some("contour");
    let only_hatch = std::env::var("SKETCH_ONLY").ok().as_deref() == Some("hatch");
    // 诊断用：只留原稿墨脊（细节层），用来分辨「形不对」是提取的问题
    // 还是合成排线/结构层压上去的问题。
    let only_detail = std::env::var("SKETCH_ONLY").ok().as_deref() == Some("detail");

    // 细节线归「细而够深」的那支笔，而不是最深最粗的 8B：
    // 拿 1.0mm 笔去描发丝/眼线，会把它们涂成黑带。
    let detail_pen = if m_eff > 1 {
        let mut best = 0usize;
        let mut best_d = f64::INFINITY;
        for (i, p) in pens[..m_eff].iter().enumerate() {
            // 细节线取「浓度最接近 0.65」的那支笔。旧的「最深 − k×线宽」
            // 打分在四支笔线宽都收窄后会选到 8B，发丝立刻变粗。
            let d = (p.darkness as f64 - 0.65).abs();
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        best as u8
    } else {
        0
    };
    let detail_width = if m_eff > 1 {
        pens[detail_pen as usize].width_mm.clamp(0.15, 1.6)
    } else {
        pen_mm
    };
    let detail_depth = if m_eff > 1 {
        pens[detail_pen as usize].darkness.clamp(0.2, 1.0)
    } else {
        1.0
    };
    // 浅结构线用小笔：下颌、鼻梁这些是「形」而不是「重音」，
    // 拿 2B 画会和发丝、眼线抢重。最细的那支笔（HB）刚好。
    let fine_pen = if m_eff > 1 {
        let mut best = 0usize;
        for i in 1..m_eff {
            if pens[i].width_mm < pens[best].width_mm {
                best = i;
            }
        }
        best as u8
    } else {
        0
    };
    let fine_width = if m_eff > 1 {
        pens[fine_pen as usize].width_mm.clamp(0.15, 1.6)
    } else {
        pen_mm
    };
    let fine_depth = if m_eff > 1 {
        pens[fine_pen as usize].darkness.clamp(0.2, 1.0)
    } else {
        1.0
    };

    let detail = if only_hatch {
        Vec::new()
    } else {
        // 检测尺度用「画细节的那支细笔」，不用 2B：原稿脸颊排线只有 1~2px，
        // 按 0.5mm 的笔宽去拉开间距，会把相邻两根浅排线并成一根。
        source_detail_strokes(&field, &ht, threshold, fine_width)
    };
    // 分开落笔：浅线用细笔、深线用 2B。投影笔没有压力，
    // 一条线只能靠「哪支笔画」来决定轻重；混在一支笔里，
    // 下颌线要么太黑、要么太浅。
    let (mut detail_light, mut detail_dark): (Vec<Stroke>, Vec<Stroke>) = detail
        .into_iter()
        .partition(|s| s.mean_dark < 0.50);

    // 细节线先记进「已落墨」：合成排线看到发丝/眼线已经画过，就不会在上面
    // 再糊一层同向线，既保住细节，也省笔墨。
    let mut ink = vec![0f32; npix];
    for (list, w_mm, d) in [
        (&detail_light, fine_width, fine_depth),
        (&detail_dark, detail_width, detail_depth),
    ] {
        for s in list {
            for seg in s.pts.windows(2) {
                stamp_ink(
                    &mut ink,
                    fw,
                    fh,
                    seg[0],
                    seg[1],
                    // 占位要按细节线的真实墨量记：上一版只记 0.85 深度，
                    // 合成排线以为发丝间还空着 15%，又补了一层，头发就糊成黑块。
                    // 放大到 0.68 笔宽：细节线画在最后，排线必须让位，否则同一根
                    // 发丝会被「提取线 + 合成线」叠两遍，黑成一片。
                    (w_mm * 0.68) / px_mm,
                    d,
                );
            }
        }
    }
    let r_view = ((0.70 / px_mm).round() as usize).clamp(1, 12);
    let mut view = box_blur(&ink, fw, fh, r_view);
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
    if std::env::var("SKETCH_DEBUG").is_ok() {
        for (x, y, label) in [
            (300usize, 450usize, "左额上"),
            (350, 650, "左额"),
            (330, 850, "左颊"),
            (430, 800, "左颊内"),
            (600, 950, "鼻梁"),
            (800, 950, "右颊"),
            (900, 500, "发中"),
            (1000, 700, "发右"),
        ] {
            if x < fw && y < fh {
                let i = y * fw + x;
                eprintln!(
                    "  预落墨 {label}：目标 {:.3} 已墨 {:.3} 缺口 {:.3}",
                    target[i],
                    view[i],
                    target[i] - view[i]
                );
            }
        }
    }

    // 预算按「最细的笔」留足。笔宽收到 0.20mm 后，同样墨量要排近两倍
    // 的线：HB 一遍就要 3.7 万笔，旧的 6 万上限让 2B/4B/8B 一笔都轮不到
    // （实测 4B/8B = 0 条，暗部掉到原稿的 0.71 倍）。这里放宽到 16 万笔，
    // 由各遍自己的缺口收敛把关，不至于无限排下去。
    const MAX_STROKES: usize = 160_000;
    const MAX_LEN_MM: f64 = 420_000.0;
    let mut total_len = 0.0f64;
    let mut total_strokes = 0usize;
    let mut stroke_no = 0usize;

    for p in 0..m_eff {
        let (pw, pdark) = if m_eff > 1 {
            (
                pens[p].width_mm.clamp(0.15, 1.6),
                pens[p].darkness.clamp(0.2, 1.0),
            )
        } else {
            (pen_mm, 1.0)
        };
        let lo = bounds[p];
        let hi = bounds[p + 1];
        if hi - lo < 1e-4 {
            continue;
        }
        for pass in 0..passes {
            if only_contour || only_detail {
                break;
            }
            if total_strokes >= MAX_STROKES || total_len >= MAX_LEN_MM {
                break;
            }
            let seed = 0x51ed_2701_b4e9_2f0d
                ^ (p as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
                ^ (pass as u64).wrapping_mul(0xD1B5_4A32_D192_ED03);
            let layer = stroke_pass_v2(
                &field,
                &target,
                &ht.bg,
                &view,
                &mut ink,
                follow,
                base,
                lo.max(paper_gate),
                hi,
                pdark,
                pw,
                stroke_len,
                pass,
                seed,
                MAX_STROKES.saturating_sub(total_strokes),
            );
            let n = layer.len();
            if std::env::var("SKETCH_DEBUG").is_ok() {
                eprintln!(
                    "素描笔触：笔 {p} 第 {pass} 遍，{n} 条，累计 {:.0}mm",
                    total_len + layer.iter().map(|s| s.len_mm).sum::<f64>()
                );
            }
            if n == 0 {
                break;
            }
            total_len += layer.iter().map(|s| s.len_mm).sum::<f64>();
            total_strokes += n;
            emit_hatch_set(
                &layer,
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
            view = box_blur(&ink, fw, fh, r_view);
        }
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

    // —— 结构层：少量真正的边（眼线、唇缝、下颌），细笔单遍压形 ——
    if !only_hatch && !only_detail {
        let contour =
            contour_strokes(&field, &ht.detail, &ht.bg, scale, detail_width, edge_threshold);
        let mut structural: Vec<Stroke> = contour
            .into_iter()
            .filter(|s| s.len_mm >= 2.2 && s.mean_dark >= 0.10)
            .collect();
        // 轮廓层只留「真形体」，不能把原稿脸上的浅排线也串成一条长线：
        // 细线排线在 detail 图里同样是暗响应，骨架会把这些短笔触连成
        // 几十毫米的「划痕」，压在排线上非常扎眼。用 1.3mm 归一调子
        // （ht.tone，不是原始 tone）区分：五官、发丝处调子高，排线只是
        // 0.02~0.10 的浅纹理。均值低的链一律丢掉。
        //
        // 门槛从 0.28 抬到 0.40：0.28 时额头、脸颊的密排线也被串成
        // 几百条长斜线，整张脸读起来像刮痕。真轮廓（眼线、唇缝、下颌、
        // 鼻孔）的 1.3mm 调子都在 0.5 以上，抬起门槛只砍伪线。
        let before_n = structural.len();
        structural.retain(|s| {
            let n = s.pts.len().max(1);
            let mean_tone: f32 = s
                .pts
                .iter()
                .map(|&(x, y)| {
                    let xi = (x as usize).min(fw.saturating_sub(1));
                    let yi = (y as usize).min(fh.saturating_sub(1));
                    ht.tone[yi * fw + xi]
                })
                .sum::<f32>()
                / n as f32;
            // 上限 0.90：瞳孔、眼线这些实心黑线的边界会被边缘检测抽出
            // 一内一外两条平行线，再各描一笔就成了「双线 + 白芯」的方框。
            // 这类黑线本身已经由原稿墨脊（细节深层）沿中线描出，轮廓层
            // 只补中间调的形体边（脸颊阴影、鼻梁、下颌）。
            //
            // `passes == 1` 再挡一道：`contour_strokes` 把「响应超过强阈 3 倍」
            // 的链标成双遍（眼线、睫毛、瞳孔边）。原稿真正画出来的线已经在
            // 墨脊层里，轮廓层再描一遍就是两条叠在一起的粗线——眼睛因此
            // 变成「双线白芯」。轮廓层只留弱响应（形体转折），强线全部让给墨脊。
            mean_tone > 0.40 && mean_tone < 0.90 && s.passes == 1
        });
        if std::env::var("SKETCH_DEBUG").is_ok() {
            eprintln!(
                "素描笔触：轮廓去浅排线 {} → {} 条",
                before_n,
                structural.len()
            );
        }
        // 结构层不能跟着排线总量一起缩：排线一稀，预算掉到 80mm，
        // 只够留下 5 条边，整张脸就没有下颌线。给一个下限，至少保住形。
        let budget = if only_contour {
            1e9
        } else {
            (total_len * 0.10).clamp(900.0, 2_800.0)
        };
        let total: f64 = structural.iter().map(|s| s.len_mm).sum();
        if total > budget {
            let score = |s: &Stroke| s.len_mm.min(40.0) * s.mean_dark.clamp(0.0, 1.0).powi(2);
            structural.sort_by(|a, b| {
                score(b)
                    .partial_cmp(&score(a))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let mut acc = 0.0f64;
            structural.retain(|s| {
                if acc + s.len_mm > budget && acc > 0.0 {
                    return false;
                }
                acc += s.len_mm;
                true
            });
        }
        for s in structural.iter_mut() {
            s.passes = 1;
            // 等值线骨架在 1px 网格上是折线，直接描出来眼角、鼻翼全是方角。
            // 三遍三点平均保留原稿的弯，只去掉像素锯齿——轮廓层要像原稿的
            // 手绘线一样连续圆滑，而不是像排线那样一节节直段。
            if s.pts.len() >= 3 {
                smooth_in_place(&mut s.pts);
                smooth_in_place(&mut s.pts);
                smooth_in_place(&mut s.pts);
            }
        }
        if std::env::var("SKETCH_DEBUG").is_ok() {
            eprintln!(
                "素描笔触：结构 {} 条 {:.0}mm（预算 {budget:.0}mm），总笔触 {:.0}mm",
                structural.len(),
                structural.iter().map(|s| s.len_mm).sum::<f64>(),
                total_len
            );
            let mut longest: Vec<&Stroke> = structural.iter().collect();
            longest.sort_by(|a, b| {
                b.len_mm
                    .partial_cmp(&a.len_mm)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            for s in longest.iter().take(5) {
                let n = s.pts.len().max(1);
                let mt: f32 = s
                    .pts
                    .iter()
                    .map(|&(x, y)| {
                        let xi = (x as usize).min(fw.saturating_sub(1));
                        let yi = (y as usize).min(fh.saturating_sub(1));
                        ht.tone[yi * fw + xi]
                    })
                    .sum::<f32>()
                    / n as f32;
                eprintln!("  结构长线 {:.1}mm 调子 {:.2}", s.len_mm, mt);
            }
        }
        emit_hatch_set(
            &structural,
            order_mode,
            order_block_px,
            scale,
            jitter_mm,
            detail_width,
            detail_pen,
            &mut stroke_no,
            &mut out,
            &mut pens_out,
        );
    }

    // 细节最后画：原稿里的发丝、眼线、衣褶压在排线之上，形才立得住。
    // 先浅后深：下颌/鼻梁的浅结构用细笔，发丝/眼线的重音用 2B 压在上面。
    if only_contour {
        detail_light.clear();
        detail_dark.clear();
    }
    for (list, w_mm, depth, pen, label) in [
        (&mut detail_light, fine_width, fine_depth, fine_pen, "浅细节"),
        (&mut detail_dark, detail_width, detail_depth, detail_pen, "深细节"),
    ] {
        if list.is_empty() {
            continue;
        }
        for s in list.iter_mut() {
            s.passes = 1;
            s.mean_width_mm = w_mm;
            s.mean_dark = depth as f64;
        }
        if std::env::var("SKETCH_DEBUG").is_ok() {
            eprintln!(
                "素描笔触：{label} {} 条 {:.0}mm，笔 {pen}",
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
            w_mm,
            pen,
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
    let fw = field.w;
    let fh = field.h;
    // 粗黑团块（瞳孔、虹膜、浓密发丛）内部的骨架不是「线」而是填充：
    // 照线描出来是一坨坨粗黑折块，把眼睛画成碎玻璃。轮廓、发丝都在
    // 0.7mm 以内；超过 1.05mm 的宽脊交给排线去填。
    strokes.retain(|s| s.mean_width_mm <= 1.05);
    strokes.retain(|s| {
        // 原稿的浅排线本身就有 1.5~5mm 的短段；位置和走向都是原作定的。
        if s.len_mm < 1.5 {
            return false;
        }
        if s.mean_dark < 0.010 {
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
        if bg_n * 3 > s.pts.len() {
            return false;
        }
        // 实心黑团（瞳孔、虹膜、最深的发芯）内部没有「线」，只有填充。
        // 骨架会在团块里分出许多支叉，照线描出来是一团乱麻，眼睛变成毛线球。
        // 1.3mm 平滑调子仍然接近全黑（>0.93）的深笔触交给排线去压深；
        // 眼线只有 0.4~0.6mm 宽，平滑后会掉到 0.7 以下，不会被误删。
        if s.mean_dark >= 0.90 {
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
            if mt > 0.93 {
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
            split_detail_chords(s, px_mm, 2.2, 0.45, &mut short);
        }
    }
    // 深调（发丝、眼线、衣褶、下颌）是「形」，不是调子：沿原流线切片、
    // **保留中间点**，绝不把一根弯曲的轮廓捋成几段直线。切片上限压到 8mm：
    // 追踪经常把一根眉线、一缕头发一路拉到 40~57mm（端点距/弧长 0.96，
    // 几乎是直线），横穿眼睛和脸颊，正是用户说的「像调子的线段都是直的」。
    // 8mm 一段仍是连续折线，形没变，但一笔不会横跨半个五官。
    for s in kept_dark {
        slice_detail_curve(s, px_mm, 8.0, &mut short);
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

/// 一遍「流线短直段」。
///
/// 种子是打乱的抖动网格；每个种子按「还差多少墨」反算线距，再沿走向场
/// 连续走一段，最后切成 1.6~5.5mm 的直线弦。同一遍里用胶囊占位控制线距；
/// 流线本身是连续的，所以线距对了就是一片排线，而不是一格一根短杠。
#[allow(clippy::too_many_arguments)]
fn stroke_pass_v2(
    field: &Field,
    target: &[f32],
    bg: &[bool],
    view: &[f32],
    ink: &mut [f32],
    follow: u8,
    base: f64,
    lo: f32,
    hi: f32,
    dark: f32,
    pen_mm: f64,
    stroke_len: f64,
    pass: usize,
    seed: u64,
    max_strokes: usize,
) -> Vec<Stroke> {
    let fw = field.w;
    let fh = field.h;
    let px = field.px_mm.max(1e-9);
    let mut layer: Vec<Stroke> = Vec::new();
    if fw == 0 || fh == 0 || max_strokes == 0 {
        return layer;
    }
    let mut occupied = vec![false; fw * fh];
    // 种子网格要细到能承载目标线距：HB 单层线距约 0.38mm，种子却按 1.2mm
    // 放，一格里最多一根，再密的目标也排不出来。按 1.10 倍笔宽铺种子
    // （HB 约 0.24mm），线距够近时占位自然会筛掉多余的起点。
    // 下限从 0.30 降到 0.24：笔宽收到 0.20 之后，0.30 的格子会让最细的笔
    // 也排不出原稿那种 0.3~0.5mm 的细密排线。
    // 种子步距必须小于目标线距，否则每格只能落一根，覆盖被网格量化卡死：
    // 4B 目标线距 0.36mm、步距却 0.35mm，把上限从 0.80 放到 0.90 一根都没多。
    // 收到 0.95 倍笔宽后，深调的 0.86~0.94 覆盖率才真正落得下去。
    let step_mm = (pen_mm * 0.95).max(0.20);
    let step_px = step_mm / px;
    let cols = ((fw as f64 / step_px).ceil() as usize) + 1;
    let rows = ((fh as f64 / step_px).ceil() as usize) + 1;
    let mut cand: Vec<u32> = Vec::with_capacity(cols * rows);
    for cy in 0..rows {
        for cx in 0..cols {
            cand.push((cy * cols + cx) as u32);
        }
    }
    let mut s = seed | 1;
    for i in (1..cand.len()).rev() {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        let j = (s % (i as u64 + 1)) as usize;
        cand.swap(i, j);
    }
    let phase_x = (hash01(seed.rotate_left(11)) * 0.94) * step_px;
    let phase_y = (hash01(seed.rotate_left(29)) * 0.94) * step_px;
    // 8B 负责瞳孔、虹膜、发芯这些「实心黑块」：原稿那里没有缝，
    // 0.80 的上限会让 0.4mm 的笔排成带白缝的条纹，眼睛读成灰色。
    // 只对最深的笔放开到 0.92，4B 及以下仍保留排线间隙。
    // 深调每支笔的线距都要收到「几乎挨着」：原稿的暗发实测 66% 像素是墨，
    // 4B 按 0.80 覆盖铺出来、预览里仍有 34% 纯白缝，整片读成灰色。
    // 线距是 1/覆盖，0.80→0.88 就把白缝砍掉一半，深调也才真正压得住。
    // 亮调的笔（HB）保持 0.80，脸上不会糊成一片。
    let max_cov = if dark > 0.88 {
        0.94
    } else if dark > 0.74 {
        0.90
    } else if dark > 0.56 {
        0.86
    } else {
        STROKE_PASS_MAX_COV
    };
    let cross_deg = if follow == 0 {
        STROKE_CONTOUR_DEG[pass.min(STROKE_CONTOUR_DEG.len() - 1)]
    } else {
        STROKE_CROSS_DEG[pass.min(STROKE_CROSS_DEG.len() - 1)]
    };
    let mut n_bg = 0usize;
    let mut n_low = 0usize;
    let mut n_full = 0usize;
    let mut n_occ = 0usize;
    let mut n_short = 0usize;

    for ci in cand {
        if layer.len() >= max_strokes {
            break;
        }
        let gx = ci as usize % cols;
        let gy = ci as usize / cols;
        let h = hash_u64(seed ^ (ci as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let h2 = hash_u64(h ^ 0xD1B5_4A32_D192_ED03);
        // 每遍把整张抖动网格平移一个随机相位：第 1 遍的种子才会落在
        // 第 0 遍两根线之间，而不是反复落在同一格、被占位挡掉。
        // 没有这个相位，脸的墨量只能到网格上限（0.3/0.6 = 0.5 覆盖），
        // 目标 0.30 永远补不满，画面就会出现「中间空一块」。
        let x = (gx as f64 + 0.08 + 0.84 * hash01(h)) * step_px + phase_x;
        let y = (gy as f64 + 0.08 + 0.84 * hash01(h2)) * step_px + phase_y;
        if x < 1.0 || y < 1.0 || x >= fw as f64 - 1.0 || y >= fh as f64 - 1.0 {
            continue;
        }
        let ix = x as usize;
        let iy = y as usize;
        let i = iy * fw + ix;
        if bg[i] {
            n_bg += 1;
            continue;
        }
        let t = target[i];
        if t <= lo + 0.006 {
            n_low += 1;
            continue;
        }
        let desired = t.min(hi);
        let deficit = desired - view[i];
        if deficit <= 0.010 {
            n_full += 1;
            continue;
        }
        let (mut theta, conf) = stroke_dir(field, follow, base, x, y);
        // 交叉层从中间调就开始启用：亮部也交叉会立刻变成乱网。
        // 起点从 0.30 提前到 0.18：原稿额头的浅排线本身就带一层反向的
        // 短笔，只按 0.30 起算，脸和衬衫上的每一遍都同向，叠多少遍都
        // 只读出一层。提前后浅调只是 ±几度，中间调才真正交叉开。
        let dark_fac = smoothstep01((t - 0.18) / 0.42);
        // 角差本来就小（十几度），这里只需把发流/眼线处的方向再压一点。
        let hair_guard = if follow == 0 {
            1.0f64
        } else {
            1.0 - 0.30 * conf as f64
        };
        // 每一遍都用满自己的交叉角：0° / 15° / -13° / 8° / 24° 拉开，
        // 深调靠 2~3 层交叉叠出来，而不是把线加粗。
        let cross_eff = cross_deg;
        theta += cross_eff.to_radians() * dark_fac * hair_guard;
        // 同一遍里再给每根线一个按调子放大的小角差。真实排线没有两根是
        // 严格平行的；完全没有角差时，即使叠了五遍也像一把梳子只梳了一遍。
        // 浅调 ±2°，深调 ±8°，和原稿实测的层间角差（约 10°）同一个量级。
        theta += (hash01(h2) - 0.5) * (0.07 + 0.21 * dark_fac);
        // 再叠一层约 9mm 的「组向」：同一片排线整体偏几度，隔壁那片偏另一边。
        // 原稿的排线区就是这么一块块换方向的；没有它，几百根线共用一个角，
        // 放大看是木纹，不是手画。
        theta += (vnoise01(x * px / 9.0, y * px / 9.0, seed ^ 0x51ED) - 0.5)
            * 9.0f64.to_radians()
            * (0.45 + 0.55 * dark_fac)
            * hair_guard;

        let cov = ((deficit as f64) / (dark.max(0.05) as f64)).clamp(0.02, max_cov);
        // 线距不仅由「还差多少墨」决定，还要服从排线本身的梳齿尺度：
        // 浅调按缺口反算能到 2~3mm 一行，画出来是一根根孤零零的斜线，
        // 和原稿 1mm 量级的细排线完全不是一回事。上限压到 1.10mm，
        // 缺口小的时候由后面的交叉遍插空补齐，而不是把行距拉大。
        // 笔宽收到 0.20~0.40 之后，2.6 倍笔宽正好是原稿那种 0.5~1.0mm
        // 的梳齿尺度；旧值 3.2 倍会在浅调留下 0.9mm 的空档。
        // 但最亮的一档（脸、衬衫高光）不能按 2.6 倍硬排：那里原稿几乎
        // 是纸白，0.5mm 的间距叠到 0.20mm 细笔上会把整张脸压灰。目标
        // 低于 0.18 时放宽到 3.8 倍，笔数仍比旧版多，但脸留得住白。
        let cap_k = if t < 0.18 { 3.8 } else { 2.6 };
        // 3.5mm 的密度成片：松的地方线距 ×1.2，密的地方 ×0.8。
        // 「该密的密、该疏的疏」不只是明暗差异——原稿同一块调子里，
        // 排线本身也是一簇一簇的，均匀间距会让整片读成网纹。
        let clump = 0.80 + 0.40 * vnoise01(x * px / 3.5, y * px / 3.5, seed ^ 0x9A7);
        let pitch = (pen_mm / cov * clump)
            .clamp(pen_mm * 1.02, (pen_mm * cap_k).min(1.10));
        let keep_px = pitch * 0.5 / px;
        if occupied_near(&occupied, fw, fh, ix, iy) {
            n_occ += 1;
            continue;
        }
        // 除「随轮廓」外，方向在起笔时锁定：素描排线是一组同向短直线，
        // 走到哪儿改到哪儿只会得到一根长曲线。跟随轮廓才逐点取等照度线，
        // 让一条线顺着发流/形体转过去。深发（t>0.58）也逐点取：发丝本来
        // 就是顺着发流弯的长线，锁成直线会横穿发丛，看着像划痕。
        let straight = follow > 0 && t <= 0.58;
        let base_l = if straight {
            stroke_len * (0.95 + 0.85 * conf as f64)
        } else {
            stroke_len * (1.55 + 2.4 * conf as f64)
        };
        let lmax = (base_l
            * (1.0 - 0.12 * pass.min(3) as f64)
            * (0.86 + 0.28 * hash01(h2)))
        .clamp(stroke_len * 0.9, if straight { 6.0 } else { 16.0 });

        let mut cx = x;
        let mut cy = y;
        let mut pts: Vec<(f64, f64)> = vec![(x, y)];
        let mut dx = theta.cos();
        let mut dy = theta.sin();
        let step_trace = 0.35f64;
        let step_trace_px = step_trace / px;
        let max_steps = ((lmax / step_trace).ceil() as usize).clamp(2, 90);
        let mut walked = 0.0f64;
        for _ in 1..=max_steps {
            let th = if straight {
                theta
            } else {
                let (mut th, _) = stroke_dir(field, follow, base, cx, cy);
                th += cross_eff.to_radians() * dark_fac * hair_guard;
                th
            };
            let (mut nx, mut ny) = (th.cos(), th.sin());
            if !straight {
                // 与上一段方向混合：脊方向逐像素会翻，混合后才是「一根线」。
                nx = dx * 0.55 + nx * 0.45;
                ny = dy * 0.55 + ny * 0.45;
            }
            let m = (nx * nx + ny * ny).sqrt();
            if m < 1e-6 {
                break;
            }
            nx /= m;
            ny /= m;
            let nxp = cx + nx * step_trace_px;
            let nyp = cy + ny * step_trace_px;
            if nxp < 1.0 || nyp < 1.0 || nxp >= fw as f64 - 1.0 || nyp >= fh as f64 - 1.0 {
                break;
            }
            let ni = (nyp as usize) * fw + (nxp as usize);
            if bg[ni] || target[ni] < lo + 0.008 {
                break;
            }
            // 走到已经够墨的地方就收笔，别把暗部的线拖进亮部。
            //
            // 但「够墨」要按遍数留一点余量：第 1、2 遍如果一进墨就收，
            // 种子几乎全被判成「已补满」，中间调只剩第 0 遍那一层同向线，
            // 叠不出来交叉层。允许压过目标 2%~7%（越暗余量越大），
            // 后一遍才会在前一遍的缝里再补一层不同角度的短线。
            // 暗部余量仍比亮部大：那里本来就要靠多遍同向叠加才够黑，
            // 上一版一进墨就收，头发的黑色砖块就是这么来的。
            let over = (0.015 + 0.035 * smoothstep01((t - 0.30) / 0.42)) as f32;
            if view[ni] >= desired + over {
                break;
            }
            walked += step_trace;
            cx = nxp;
            cy = nyp;
            pts.push((cx, cy));
            dx = nx;
            dy = ny;
            if walked >= lmax {
                break;
            }
        }
        if pts.len() < 2 {
            n_short += 1;
            continue;
        }
        let total: f64 = pts
            .windows(2)
            .map(|w| ((w[1].0 - w[0].0).powi(2) + (w[1].1 - w[0].1).powi(2)).sqrt())
            .sum();
        if total * px < pen_mm * 1.6 {
            n_short += 1;
            continue;
        }
        // 本遍占位：胶囊沿整条流线，线距由 pitch 决定。
        for seg in pts.windows(2) {
            stamp_mask(&mut occupied, fw, fh, seg[0], seg[1], keep_px);
        }
        // 切成短直线弦。相邻弦共享端点，视觉上仍是连续的一笔。
        //
        // 原稿实测一根排线只有 2~5mm；十几毫米的直线横在脸上就是「划痕」。
        // 这里把弦压到 1.5~4.5mm：线距变密后，短弦才不会让整片调子变糊。
        // 深部（发丝、眼线）稍长一点，但也不超过 4.5mm——方向随形，
        // 每一笔仍是铅笔的短直线段，而不是抽象的弯曲线。
        let chord_mm = if follow == 0 {
            (stroke_len * 0.8).clamp(1.5, 3.5).max(pen_mm * 2.2)
        } else if t > 0.62 {
            (stroke_len * 1.0).clamp(2.0, 4.5).max(pen_mm * 2.2)
        } else {
            (stroke_len * 0.8).clamp(1.8, 3.2).max(pen_mm * 2.2)
        };
        let n_ch = ((total * px / chord_mm).round() as usize).max(1);
        let seg = total / n_ch as f64;
        // 明暗调子的弦两端各留 12% 空档：相邻弦首尾相接时，plotter 一笔画下来
        // 就是一根 20mm 的长线，整片读成「木纹」；原稿排线是分开的一段段，
        // 笔尖反复起落。深发（t>0.58）不留缝——发丝本来就是连续的。
        let gap = if t > 0.58 { 0.0 } else { 0.12 };
        for k in 0..n_ch {
            if layer.len() >= max_strokes {
                break;
            }
            let a = point_at(&pts, seg * (k as f64 + gap));
            let b = point_at(&pts, seg * ((k + 1) as f64 - gap));
            let lmm = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt() * px;
            if lmm < pen_mm * 1.2 {
                n_short += 1;
                continue;
            }
            stamp_ink(ink, fw, fh, a, b, pen_mm * 0.5 / px, dark);
            layer.push(Stroke {
                theta: path_theta(&[a, b]),
                pts: vec![a, b],
                loose: [true, true],
                len_mm: lmm,
                mean_dark: desired as f64,
                mean_width_mm: pen_mm,
                rank: pass as f64 / 7.0,
                structural: false,
                passes: 1,
            });
        }
    }
    if std::env::var("SKETCH_DEBUG").is_ok() {
        eprintln!(
            "  种子 {}：接受 {}，背景 {}，浅于层 {}，已补满 {}，占用 {}，收得太短 {}",
            cols * rows,
            layer.len(),
            n_bg,
            n_low,
            n_full,
            n_occ,
            n_short
        );
    }
    layer
}

/// 合成笔触的走向。
///
/// - 0 跟随轮廓：结构张量的等照度线切向（形体转折、发流），整条线随形弯曲。
/// - 1 跟随原稿：平滑后的墨脊方向；没有可靠脊的地方退回基准角。
/// - 2 混合（默认）：原稿笔触方向优先，平坦区退到等照度线，再退到基准角。
///
/// 关键是**不给平坦区逐像素取脊方向**：原稿没有脊的地方，那个方向是噪声，
/// 上一版就是在这里把整张脸画成草。另外除模式 0 外，落笔后方向锁定在
/// 起点——素描排线是短直线，只有「随轮廓」才允许一条线跟着形走。
fn stroke_dir(field: &Field, follow: u8, base: f64, x: f64, y: f64) -> (f64, f32) {
    match follow {
        0 => {
            let (a, c) = field.ang_at(x, y);
            if c > 0.16 {
                (a as f64, c)
            } else {
                (base, 0.0)
            }
        }
        1 => {
            let (a, c) = field.smooth_ridge_ang_at(x, y);
            if c > 0.30 {
                (a as f64, c.min(1.0))
            } else {
                (base, 0.0)
            }
        }
        _ => {
            // 混合模式。暗部和亮部的「可靠方向」不是同一个场：
            //
            // 暗发里的脊检测尺度跟的是单根细发丝（1~2px），而实心暗发是
            // 5~10px 宽的深色带，小尺度 Hessian 在带内看到的是噪声——实测
            // 发中脊向 46°、发右 17°，而发流实际约 -22°。按脊向排线就是把
            // 头发织成乱网。2.5mm 等照度线反而正好沿着「暗带/亮缝」走。
            //
            // 亮部则相反：脸上的浅排线本身就是一排细脊，等照度线是形体走向，
            // 用它会把每一笔各自转向。所以亮部优先脊向，暗部优先等照度线。
            //
            // 判据是「逐像素方向与原稿一致的比例」（tools/_dir.py）：暗部用
            // 等照度线时头发一致率 93.3%、平均角差 11.3°；改用整块 6mm 块向后
            // 只剩 78.3%/18.6°——块向在发丛里跟着一缕缕的局部走向摆，整体方向
            // 反而散了。额头同样从 91.3% 掉到 79.2%。这条实测推翻了「块向
            // 统一更好」的直觉，保留原策略。
            let dark = field.tone_at(x, y) > 0.42;
            let (ia, ic) = field.ang_at(x, y);
            if dark {
                if ic > 0.30 {
                    return (ia as f64, ic);
                }
                let (a, c) = field.smooth_ridge_ang_at(x, y);
                if c > 0.30 {
                    return (a as f64, c.min(1.0));
                }
                return (base, 0.0);
            }
            // 浅调同样优先 2.5mm 等照线。这一版原本用 6mm 块向，实测在衬衫上
            // 排成了和原稿镜像的 52~66°：块向是「按脊响应加权」的平均，原稿
            // 交叉排线里稍深的那一层会把权重拉过去，主排线（约 124°）反而丢了
            // （tools/_orient.py 逐块统计）。等照线在浅色区的一致性本来就高
            // （衬衫 0.80~0.94），锁在落笔点也不会逐笔乱转。
            if ic > 0.55 {
                return (ia as f64, ic);
            }
            let (ba, bc) = field.broad_ridge_ang_at(x, y);
            if bc > 0.40 {
                return (ba as f64, bc.min(1.0));
            }
            (base, 0.0)
        }
    }
}

/// 取值在 0~1 的平滑值噪声（格点哈希 + 双线性 smoothstep）。
///
/// 用来给排线加「一块一块」的手感：真实素描的排线是分组成片的，有的地方
/// 密、有的地方松，组与组之间还会差几度方向。完全均匀的间距和走向，
/// 再细也会像机织布。
fn vnoise01(x: f64, y: f64, seed: u64) -> f64 {
    let xi = x.floor();
    let yi = y.floor();
    let fx = x - xi;
    let fy = y - yi;
    let h = |cx: i64, cy: i64| {
        hash01(
            seed ^ (cx as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
                ^ (cy as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F),
        )
    };
    let (x0, y0) = (xi as i64, yi as i64);
    let a = h(x0, y0);
    let b = h(x0 + 1, y0);
    let c = h(x0, y0 + 1);
    let d = h(x0 + 1, y0 + 1);
    let u = fx * fx * (3.0 - 2.0 * fx);
    let v = fy * fy * (3.0 - 2.0 * fy);
    (a * (1.0 - u) + b * u) * (1.0 - v) + (c * (1.0 - u) + d * u) * v
}

fn smoothstep01(t: f32) -> f64 {
    let u = t.clamp(0.0, 1.0) as f64;
    u * u * (3.0 - 2.0 * u)
}

/// 折线上按弧长取点。
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

fn occupied_near(mask: &[bool], w: usize, h: usize, x: usize, y: usize) -> bool {
    for yy in y.saturating_sub(1)..=(y + 1).min(h.saturating_sub(1)) {
        for xx in x.saturating_sub(1)..=(x + 1).min(w.saturating_sub(1)) {
            if mask[yy * w + xx] {
                return true;
            }
        }
    }
    false
}

/// 把一条线段按半宽刷进「视觉墨量」图：中心是满墨，边缘按 1px 过渡。
fn stamp_ink(
    ink: &mut [f32],
    w: usize,
    h: usize,
    a: (f64, f64),
    b: (f64, f64),
    r: f64,
    dark: f32,
) {
    if w == 0 || h == 0 {
        return;
    }
    let r = r.max(0.5);
    let pad = r + 1.5;
    let x0 = (a.0.min(b.0) - pad).floor().max(0.0) as usize;
    let y0 = (a.1.min(b.1) - pad).floor().max(0.0) as usize;
    let x1 = ((a.0.max(b.0) + pad).ceil() as isize).min(w as isize - 1).max(0) as usize;
    let y1 = ((a.1.max(b.1) + pad).ceil() as isize).min(h as isize - 1).max(0) as usize;
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
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
            let d = ((px - qx) * (px - qx) + (py - qy) * (py - qy)).sqrt();
            let cov = ((r + 0.45 - d) / 0.9).clamp(0.0, 1.0);
            if cov <= 0.0 {
                continue;
            }
            let add = dark * cov as f32;
            let i = yy * w + xx;
            // 取最黑的一笔，而不是 α 叠加同色笔迹。
            //
            // 5 遍 × 4 支笔会在同一处反复压线，α 叠加把 0.42 的 HB 算成
            // 1-(0.58)^8≈0.99，缺口立刻关闭，后面的笔一根都不落；实际纸上
            // 同一支笔叠十遍仍只是 0.42 的灰度。暗发因此一直停在 0.37，
            // 而目标 0.68——整整差一半。取 max 后缺口记账才和画面一致。
            if add > ink[i] {
                ink[i] = add;
            }
        }
    }
}

/// 把线段周围 `r` 半径内的候选点标成占用（胶囊），保证同一遍的线距。
fn stamp_mask(
    mask: &mut [bool],
    w: usize,
    h: usize,
    a: (f64, f64),
    b: (f64, f64),
    r: f64,
) {
    if w == 0 || h == 0 {
        return;
    }
    let r = r.max(0.5);
    let x0 = (a.0.min(b.0) - r).floor().max(0.0) as usize;
    let y0 = (a.1.min(b.1) - r).floor().max(0.0) as usize;
    let x1 = ((a.0.max(b.0) + r).ceil() as isize).min(w as isize - 1).max(0) as usize;
    let y1 = ((a.1.max(b.1) + r).ceil() as isize).min(h as isize - 1).max(0) as usize;
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let r2 = r * r;
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
                mask[yy * w + xx] = true;
            }
        }
    }
}

/// 多笔提取：每一笔都来自原稿的墨脊，按笔重分给不同铅笔。
///
/// 返回 `None` 表示「这张图上没有足够多真实笔触」（照片、纯色块、渐变），
/// 调用方回落到按明暗带排线。
fn extracted_multi_pen(
    field: &Field,
    threshold: f64,
    pen_mm: f64,
    m_eff: usize,
) -> Option<Vec<Vec<Stroke>>> {
    // 密度 2.6：多笔要的是「原稿上画过的线尽量都拿到」。
    // 单笔调子模式才需要严格按墨量取舍，否则浅笔触会被整体砍掉，
    // 分成 4 支笔后每支只剩零星几根——这正是「线条提取过少」的来源。
    let (mut strokes, _cover) = ridge_strokes(field, pen_mm, threshold, 2.6);
    strokes.retain(|s| s.len_mm >= 1.2);
    let src = field.tone.iter().map(|&t| t as f64).sum::<f64>()
        * field.px_mm
        * field.px_mm;
    let ink = strokes.iter().map(|s| s.len_mm).sum::<f64>() * pen_mm;
    if strokes.len() < m_eff * 8 || ink < src * 0.22 {
        if std::env::var("SKETCH_DEBUG").is_ok() {
            eprintln!(
                "多笔提取：{} 条 / {:.0}mm，原稿墨量 {:.0}，不足 → 回落排线",
                strokes.len(),
                ink,
                src
            );
        }
        return None;
    }
    // 按笔重（明暗 + 线宽）排序后等分成 m 段：最轻的一段给 HB、最重的给 8B。
    // 等分而不是按固定明暗阈值切，是因为实拍素描的笔触大多挤在中暗调，
    // 按阈值切会让浅笔一根都分不到。
    let n = strokes.len();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        stroke_weight(&strokes[a])
            .partial_cmp(&stroke_weight(&strokes[b]))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut assign = vec![0u8; n];
    // 结构线强制归最深笔只在它们确实是少数时成立。整张图都由高响应长线组成
    // （纯线稿、只描边的人像）时，`ridge_strokes` 会把每一根都标成结构线——
    // 若照单全收，四支笔里就只剩 8B 有活，多笔分层名存实亡。这种情况下
    // 退回纯按笔重等分，让深浅不同的线各自找到对应的铅笔。
    let structural_n = strokes.iter().filter(|s| s.structural).count();
    let force_darkest = structural_n * 3 <= n;
    for (rank, &idx) in order.iter().enumerate() {
        let mut p = (rank * m_eff / n).min(m_eff - 1);
        // 结构线（轮廓、眼线、唇缝）无条件归最深笔：换浅笔画会立刻糊掉。
        if force_darkest && strokes[idx].structural {
            p = m_eff - 1;
        }
        assign[idx] = p as u8;
    }
    let mut out: Vec<Vec<Stroke>> = (0..m_eff).map(|_| Vec::new()).collect();
    for (idx, mut s) in strokes.into_iter().enumerate() {
        let p = assign[idx] as usize;
        s.rank = p as f64 / m_eff as f64;
        out[p].push(s);
    }
    if std::env::var("SKETCH_DEBUG").is_ok() {
        let counts: Vec<usize> = out.iter().map(|v| v.len()).collect();
        let lens: Vec<f64> = out
            .iter()
            .map(|v| v.iter().map(|s| s.len_mm).sum::<f64>())
            .collect();
        eprintln!(
            "多笔提取：{} 笔，墨量 {:.0} / 原稿 {:.0}；每笔条数 {:?} 长度 {:?}",
            n, ink, src, counts, lens
        );
    }
    Some(out)
}

/// 一层排线：沿该层的固定交叉角铺线，**间距随局部明暗连续变化**。
///
/// 这里刻意不让排线跟着形体方向走。跟着等照度线排，亮部会绕成同心环，
/// 一整块暗部看起来像一摊水；而人手排线是几个固定走向的短笔，靠疏密和
/// 叠加层数表现灰。方向固定还有一个好处：所有层之间一定能交叉出网，
/// 不会出现「两层恰好同向、叠了等于没叠」。
#[allow(clippy::too_many_arguments)]
fn hatch_layer(
    field: &Field,
    need: &[f32],
    want: f32,
    layer: usize,
    base: f64,
    spacing_mm: f64,
    scale: f64,
    pen_mm: f64,
    levels: usize,
    contrast: f32,
) -> Vec<Stroke> {
    let w = field.w as i32;
    let h = field.h as i32;
    let n = (w * h) as usize;
    let theta = base + HATCH_DIR_DEG[layer.min(HATCH_DIR_DEG.len() - 1)].to_radians();
    let min_mm = spacing_mm.max(pen_mm * 3.2);
    // 候选网格按最小间距铺；更稀的排线靠「占位圆盘」把实际间距撑大，
    // 这样同一层里既能铺很淡的灰，也能在暗部压到最小间距。
    let pitch = ((min_mm * 0.82) / scale.max(1e-9)).max(2.0);
    let mut used = vec![false; n];
    let mut out = Vec::new();
    let seed = 0x8d31_6f2a_9c47_b105 ^ (layer as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    // 每层种子错开，避免各层的线恰好压在同一个交点上，织成规整的网。
    let seed_off = 0.31 + 0.27 * layer as f64;
    let min_len_px = (1.2 / scale.max(1e-9)).max(2.0);
    // 本层的起笔门槛：比它更亮的地方这一层一根都不排。
    let stop = want;
    let accept_lo = stop;

    let mut gy = 0i32;
    loop {
        let y = gy as f64 * pitch + seed_off;
        if y >= h as f64 {
            break;
        }
        let stagger = if gy & 1 == 0 { 0.0 } else { pitch * 0.5 };
        let mut gx = 0i32;
        loop {
            let x = gx as f64 * pitch + stagger + seed_off * 0.5;
            if x >= w as f64 {
                break;
            }
            gx += 1;
            let idx = (y as usize) * w as usize + x as usize;
            if used[idx] || need[idx] < stop {
                continue;
            }
            // 每根笔各带一个种子：方向漂移、起止点、长度都不同，
            // 整片排线才不是一张印出来的网。
            let sseed = hash_u64(
                (x as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
                    ^ ((y as i64 as u64) << 1)
                    ^ seed,
            );
            let tone_seed = ((need[idx] / levels as f32).clamp(0.0, 1.0)).powf(1.0f32 / contrast);
            // 头发、衣褶这类强方向区域改沿「等调子线」排——在铅笔稿里就是发丝
            // 和布褶自己的走向。方向场可信度低（皮肤、平涂阴影）时仍回落到固定
            // 斜角；否则会跟着噪点走，满纸蠕虫。
            let (flow_a, flow_coh) = field.ang_at(x as f64 + 0.5, y as f64 + 0.5);
            // 只在「很暗 + 方向很一致」的地方跟走向，也就是头发。
            // 门放得太宽时，脸颊的明暗交界也会被当成方向区，排线会沿鼻梁
            // 绕出一个个长钩子，脸就散了。
            let dark_gate = ((tone_seed - 0.50) / 0.25).clamp(0.0, 1.0);
            let coh_gate = ((flow_coh - 0.45) / 0.35).clamp(0.0, 1.0);
            let gate = (dark_gate * coh_gate) as f64;
            let flow_th = flow_a as f64
                + HATCH_FLOW_DEG[layer.min(HATCH_FLOW_DEG.len() - 1)].to_radians();
            let th = lerp_dir(theta, flow_th, gate);
            // 亮处用短笔（4~8mm），暗处可以拉长到 12mm；手在浅灰区更爱抬笔。
            // 排线太长会像绘图仪拉尺子，短笔才有手劲的顿挫。
            let s_max = (4.0 + 8.0 * tone_seed as f64) / scale.max(1e-9)
                * (0.55 + 0.65 * hash01(sseed));
            let fwd = trace_hatch(field, need, (x, y), 1.0, th, stop, s_max, sseed);
            let bwd = trace_hatch(field, need, (x, y), -1.0, th, stop, s_max, sseed);
            let mut pts: Vec<(f64, f64)> = Vec::with_capacity(fwd.len() + bwd.len() + 1);
            for &p in bwd.iter().rev() {
                pts.push(p);
            }
            pts.push((x, y));
            pts.extend(fwd.iter().copied());
            if pts.len() < 3 {
                continue;
            }
            let len_px = chain_len(&pts);
            if len_px < min_len_px {
                continue;
            }
            // 这一条线覆盖的调子（沿线平均），决定它该用多稀的间距。
            let mut sum = 0.0f64;
            let mut cnt = 0usize;
            for &(px, py) in &pts {
                let i = (py as usize).min(h as usize - 1) * w as usize + (px as usize).min(w as usize - 1);
                sum += need[i] as f64;
                cnt += 1;
            }
            let mean = (sum / cnt.max(1) as f64) as f32;
            if mean < accept_lo {
                continue;
            }
            // 由明暗反算排线间距：
            // 目标覆盖率 = 调子；这块地方一共有 n 层可用，就把覆盖率均摊到每层，
            // 单层需要的线距 = 笔宽 / 单层覆盖率。
            // 例：调子 0.2 只需一层 → 线距 5 个笔宽；调子 0.8 叠 5 层 →
            // 每层约 27%，线距约 3.7 个笔宽。灰阶因此和原图对得上，
            // 不会再出现「亮部第一层就太黑、中间调全挤成一块」。
            let mean_t = ((mean / levels as f32).clamp(0.0, 1.0)).powf(1.0f32 / contrast);
            let n_act = (((mean - 0.35).floor().max(0.0)) as usize + 1).min(levels);
            let cov = (1.0 - (1.0 - mean_t.min(0.985)).powf(1.0f32 / n_act as f32))
                .clamp(0.02, 0.32) as f64;
            let local_mm = (pen_mm / cov).clamp(min_mm, pen_mm * 22.0);
            // 占位半径 = 期望线距（不是一半）：要挡住的是「离这条线一个线距以内」
            // 的所有起笔，半径取一半的话下一行起笔正好落在圈外，线距失效，
            // 而且追踪时一碰到邻居的圈就得停——整片排线会被切成小短线。
            let mark_r = ((local_mm / scale.max(1e-9)).round() as i32).max(1);
            let mark_r2 = mark_r * mark_r;
            let mark_step = (mark_r / 2).max(1) as usize;
            for &(px, py) in pts.iter().step_by(mark_step) {
                let cx = px.round() as i32;
                let cy = py.round() as i32;
                for yy in (cy - mark_r).max(0)..=(cy + mark_r).min(h - 1) {
                    let dy = yy - cy;
                    for xx in (cx - mark_r).max(0)..=(cx + mark_r).min(w - 1) {
                        let dx = xx - cx;
                        if dx * dx + dy * dy <= mark_r2 {
                            used[(yy as usize) * w as usize + xx as usize] = true;
                        }
                    }
                }
            }
            let theta_s = path_theta(&pts);
            out.push(Stroke {
                pts,
                loose: [true, true],
                len_mm: len_px * scale,
                mean_dark: (mean / want).clamp(0.0, 1.0) as f64,
                mean_width_mm: pen_mm * 0.9,
                theta: theta_s,
                rank: layer as f64 / (HATCH_DIR_DEG.len().max(1) - 1) as f64,
                structural: false,
                // 调子排线只走一遍：明暗完全由层数/密度决定，再叠一遍会把灰阶翻倍。
                passes: 1,
            });
        }
        gy += 1;
    }
    out
}

/// 多笔模式的一层排线：只负责一支笔的一段明暗带。
///
/// 和单笔的 `hatch_layer` 不同，这里的「暗」不再全由密度承担：
/// 这一层最多只需要补出 `band_hi − band_lo` 的深度，除以这支笔的
/// `darkness` 就是它需要的覆盖率。于是 HB 只铺浅灰、8B 只压深黑，
/// 每支笔都工作在自己能表现的那一段，不会让 HB 排到死黑还是不够黑。
#[allow(clippy::too_many_arguments)]
fn pen_hatch_layer(
    field: &Field,
    tone: &[f32],
    band_lo: f32,
    band_hi: f32,
    pen_mm: f64,
    darkness: f32,
    layers_in_pen: usize,
    layer_in_pen: usize,
    global_layer: usize,
    base: f64,
    scale: f64,
    _contrast: f32,
) -> Vec<Stroke> {
    let w = field.w as i32;
    let h = field.h as i32;
    let n = (w * h) as usize;
    let lp = layers_in_pen.max(1);
    let j = layer_in_pen.min(lp - 1);
    let span = (band_hi - band_lo).max(1e-4);
    let theta = base
        + HATCH_DIR_DEG[global_layer.min(HATCH_DIR_DEG.len() - 1)].to_radians();
    // 多笔时每支笔都有明确的深浅分工，不需要像单笔那样留 3.2 个笔宽防糊死；
    // 2.4 个笔宽已经能把纸缝保留下来，再密就只是把暗部涂成一块。
    let min_mm = pen_mm * 2.4;
    let pitch = ((min_mm * 0.82) / scale.max(1e-9)).max(2.0);
    let mut used = vec![false; n];
    let mut out = Vec::new();
    let seed = 0x8d31_6f2a_9c47_b105
        ^ (global_layer as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let seed_off = 0.31 + 0.27 * global_layer as f64;
    let min_len_px = (1.2 / scale.max(1e-9)).max(2.0);
    // 第 0 层在带底就开始，后面的层依次接管更深的一段。
    let sub = if j == 0 { 0.05 } else { j as f32 / lp as f32 };
    let stop = band_lo + span * sub.clamp(0.0, 0.95);
    let cov_cap = (0.30 + 0.22 * (darkness - 0.50).max(0.0)).clamp(0.30, 0.44) as f64;

    let mut gy = 0i32;
    loop {
        let y = gy as f64 * pitch + seed_off;
        if y >= h as f64 {
            break;
        }
        let stagger = if gy & 1 == 0 { 0.0 } else { pitch * 0.5 };
        let mut gx = 0i32;
        loop {
            let x = gx as f64 * pitch + stagger + seed_off * 0.5;
            if x >= w as f64 {
                break;
            }
            gx += 1;
            let idx = (y as usize) * w as usize + x as usize;
            if used[idx] || tone[idx] < stop {
                continue;
            }
            let sseed = hash_u64(
                (x as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
                    ^ ((y as i64 as u64) << 1)
                    ^ seed,
            );
            let tone_seed = tone[idx].clamp(0.0, 1.0);
            let (flow_a, flow_coh) = field.ang_at(x as f64 + 0.5, y as f64 + 0.5);
            let dark_gate = ((tone_seed - 0.50) / 0.25).clamp(0.0, 1.0);
            let coh_gate = ((flow_coh - 0.45) / 0.35).clamp(0.0, 1.0);
            let gate = (dark_gate * coh_gate) as f64;
            let flow_th = flow_a as f64
                + HATCH_FLOW_DEG[global_layer.min(HATCH_FLOW_DEG.len() - 1)].to_radians();
            let th = lerp_dir(theta, flow_th, gate);
            let s_max = (4.0 + 8.0 * tone_seed as f64) / scale.max(1e-9)
                * (0.55 + 0.65 * hash01(sseed));
            let fwd = trace_hatch(field, tone, (x, y), 1.0, th, stop, s_max, sseed);
            let bwd = trace_hatch(field, tone, (x, y), -1.0, th, stop, s_max, sseed);
            let mut pts: Vec<(f64, f64)> = Vec::with_capacity(fwd.len() + bwd.len() + 1);
            for &p in bwd.iter().rev() {
                pts.push(p);
            }
            pts.push((x, y));
            pts.extend(fwd.iter().copied());
            if pts.len() < 3 {
                continue;
            }
            let len_px = chain_len(&pts);
            if len_px < min_len_px {
                continue;
            }
            let mut sum = 0.0f64;
            let mut cnt = 0usize;
            for &(px, py) in &pts {
                let i = (py as usize).min(h as usize - 1) * w as usize
                    + (px as usize).min(w as usize - 1);
                sum += tone[i] as f64;
                cnt += 1;
            }
            let mean_tone = (sum / cnt.max(1) as f64) as f32;
            if mean_tone < stop {
                continue;
            }
            let u = ((mean_tone - band_lo) / span).clamp(0.0, 1.0);
            let n_act = (((u * lp as f32 - 0.05).floor().max(0.0)) as usize + 1).min(lp);
            // 这一层要补的深度 = 带内位置 × 带宽；除以笔的满覆盖深度才是覆盖率。
            let want_dark = (span * u / darkness.max(0.2)).clamp(0.0, 0.85);
            let cov = (1.0 - (1.0 - want_dark.min(0.985)).powf(1.0f32 / n_act as f32))
                .clamp(0.03, cov_cap as f32) as f64;
            let local_mm = (pen_mm / cov).clamp(min_mm, pen_mm * 20.0);
            let mark_r = ((local_mm / scale.max(1e-9)).round() as i32).max(1);
            let mark_r2 = mark_r * mark_r;
            let mark_step = (mark_r / 2).max(1) as usize;
            for &(px, py) in pts.iter().step_by(mark_step) {
                let cx = px.round() as i32;
                let cy = py.round() as i32;
                for yy in (cy - mark_r).max(0)..=(cy + mark_r).min(h - 1) {
                    let dy = yy - cy;
                    for xx in (cx - mark_r).max(0)..=(cx + mark_r).min(w - 1) {
                        let dx = xx - cx;
                        if dx * dx + dy * dy <= mark_r2 {
                            used[(yy as usize) * w as usize + xx as usize] = true;
                        }
                    }
                }
            }
            let theta_s = path_theta(&pts);
            out.push(Stroke {
                pts,
                loose: [true, true],
                len_mm: len_px * scale,
                mean_dark: u as f64,
                mean_width_mm: pen_mm * 0.9,
                theta: theta_s,
                rank: global_layer as f64 / (HATCH_DIR_DEG.len().max(1) - 1) as f64,
                structural: false,
                passes: 1,
            });
        }
        gy += 1;
    }
    out
}

/// 从种子向一个方向追踪一根排线（像素坐标，不含种子点）。
#[allow(clippy::too_many_arguments)]
fn trace_hatch(
    field: &Field,
    need: &[f32],
    start: (f64, f64),
    sign: f64,
    theta: f64,
    stop: f32,
    max_len_px: f64,
    seed: u64,
) -> Vec<(f64, f64)> {
    let w = field.w as i32;
    let h = field.h as i32;
    let mut out = Vec::new();
    let (mut x, mut y) = start;
    let (mut dx, mut dy) = hatch_dir(theta, x, y, seed);
    dx *= sign;
    dy *= sign;
    let mut walked = 0.0f64;
    while walked < max_len_px {
        let nx = x + dx;
        let ny = y + dy;
        if nx < 0.0 || ny < 0.0 || nx >= w as f64 || ny >= h as f64 {
            break;
        }
        let ix = nx as i32;
        let iy = ny as i32;
        let i = (iy as usize) * w as usize + ix as usize;
        // 只看调子，不看占位：占位是给「起笔」用的。追踪时一碰邻居就停，
        // 排线会被剪成一根根小短线，看着像虚线。
        if need[i] < stop {
            break;
        }
        // 方向只做很慢的低频漂移：每步吸收 18%，整根线才会缓弯而不是折线。
        let (ndx, ndy) = hatch_dir(theta, nx, ny, seed);
        let (mut sx, mut sy) = (dx * 0.82 + ndx * sign * 0.18, dy * 0.82 + ndy * sign * 0.18);
        let l = (sx * sx + sy * sy).sqrt();
        if l < 1e-9 {
            break;
        }
        sx /= l;
        sy /= l;
        dx = sx;
        dy = sy;
        x = nx;
        y = ny;
        walked += 1.0;
        out.push((x, y));
    }
    out
}

/// 两个「无向」角度之间插值：排线的 0° 和 180° 是同一根线，
/// 直接线性插值会在 0/π 附近转一整圈。
fn lerp_dir(a: f64, b: f64, w: f64) -> f64 {
    if w <= 0.0 {
        return a;
    }
    let mut d = (b - a).rem_euclid(std::f64::consts::PI);
    if d > std::f64::consts::FRAC_PI_2 {
        d -= std::f64::consts::PI;
    }
    a + d * w
}

/// 排线方向：本层固定角 + 每根笔自己的低频手漂和约 ±2° 的落笔角差。
///
/// 不跟局部脊方向走：原稿的铅笔颗粒、纸纹都会给出「假脊」，
/// 跟着它排会得到满纸蠕动的曲线；等照度线又会绕成同心环。
/// 固定交叉角 + 疏密才是单色笔最稳的表达。
fn hatch_dir(theta: f64, x: f64, y: f64, seed: u64) -> (f64, f64) {
    // 每根笔的固定角差：手不会每次都落在完全相同的角度上。
    let fixed = (hash01(seed ^ 0x51ed_2701_b4e9_2f0d) - 0.5) * 0.07;
    // 波长约 20~30mm：一根 10mm 的排线上只弯一两度，像手腕的轻微摆动。
    let wob = value_noise(x * 0.004 + y * 0.003, seed) * 0.035;
    let th = theta + fixed + wob;
    (th.cos(), th.sin())
}

/// 从四周边框泛洪，找出连成一片的背景（背景当纸白）。
///
/// `norm` 是原始调子，用来判断「这里是不是纸」；`smooth` 是 0.4mm 平滑图，
/// 只用来判断相邻像素的缓变。两者分工：纸纹的逐像素起伏不该切断漫填，
/// 而人脸那种由轮廓线界定的跳变必须切断。
fn background_mask(norm: &[f32], smooth: &[f32], w: usize, h: usize) -> Vec<bool> {
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
    let step_max = 0.018f32;
    let abs_max = 0.42f32;
    // 纯纸白（调子 0~0.006）不参与漫填：它要么是背景（本来就不会排线），
    // 要么是额头/鼻梁高光。把它标成背景的好处为零，却给「从背景漏进人脸」
    // 留了一条白走廊。
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
        let mut visit = |j: usize, stack: &mut Vec<usize>, mask: &mut Vec<bool>| {
            if mask[j] || norm[j] <= raw_floor || norm[j] > abs_max {
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

/// 轮廓/五官层：局部细节调子 → 骨架 → 折线。
///
/// 不直接用梯度找边：全局梯度被头发、衣褶这些强纹理占满，脸上的眼睛、
/// 鼻翼、唇线幅度小得多，按分位切永远轮不到它们。改用
/// `0.4mm 调子 − 2.2mm 调子`——「这一小块比周围暗多少」。
/// 眼睛、唇缝、发丝、领口这一类「形」都会稳定地浮出来，纸面噪点则接近 0。
fn contour_strokes(
    field: &Field,
    detail: &[f32],
    bg: &[bool],
    scale: f64,
    pen_mm: f64,
    edge_threshold: f64,
) -> Vec<Stroke> {
    let (w, h) = (field.w, field.h);
    // 阈值跟着「轮廓阈值」滑块走：调高只留最明确的五官与轮廓，调低连浅发丝也要。
    let thr = (0.12 - 0.08 * edge_threshold.clamp(0.05, 0.9) as f32).clamp(0.045, 0.14);
    // 双阈值滞后：强响应决定「哪些是真的形」，弱响应只负责把同一段形连起来。
    // 单阈值会把五官切成一段段虚线，滞后能补上中间掉下去的那几像素，
    // 又不至于把纸面噪点整体放进骨架。
    let mask = hysteresis_mask(detail, w, h, thr);
    let skel = thin_ink(&mask, w as u32, h as u32);
    let paths = skeleton_paths(&skel, w as u32, h as u32);
    let min_px = (2.5 / scale.max(1e-9)).max(4.0);
    let mut out = Vec::new();
    for path in paths {
        if (path.len() as f64) < min_px {
            continue;
        }
        // 整条平均强度太弱的不要：纸纹和压缩噪点也能连成细链，
        // 它们正是轮廓外那一圈「虚线头屑」。真五官、真发丝的均值明显更高。
        let mut sum = 0f32;
        let mut bg_n = 0usize;
        for &(x, y) in &path {
            let i = y as usize * w + x as usize;
            sum += detail[i];
            if bg[i] {
                bg_n += 1;
            }
        }
        let mean_d = sum / (path.len() as f32);
        if mean_d < thr * 1.5 {
            continue;
        }
        // 背景里没有形体，只有纸纹连成的链；整条落在背景上的一律丢掉。
        if bg_n * 5 > path.len() * 3 {
            continue;
        }
        let px: Vec<(f64, f64)> = path.iter().map(|&(x, y)| (x as f64, y as f64)).collect();
        let mm: Vec<(f64, f64)> = px.iter().map(|&(x, y)| (x * scale, y * scale)).collect();
        let simple = simplify_path(&mm, 0.12);
        if simple.len() < 2 {
            continue;
        }
        let pts: Vec<(f64, f64)> = simple.iter().map(|&(x, y)| (x / scale, y / scale)).collect();
        let len_mm = chain_len(&pts) * scale;
        if len_mm < 2.5 {
            continue;
        }
        let theta = path_theta(&pts);
        out.push(Stroke {
            pts,
            loose: [true, true],
            len_mm,
            mean_dark: if mean_d > thr * 3.0 { 1.0 } else { 0.92 },
            mean_width_mm: pen_mm * 1.0,
            theta,
            rank: 1.0,
            structural: true,
            // 一笔画成的线，不再横铺成宽墨带：双遍 + 1.6 笔宽会让五官变成
            // 一团粗黑疙瘩，像描了三遍边，反而不像铅笔画。
            // 只有最强的那类（眼线、唇缝、发丝根）叠第二遍压重，
            // 浅结构仍是一笔带过，画面上才有轻重。
            passes: if mean_d > thr * 3.0 { 2 } else { 1 },
        });
    }
    link_structural(out, scale)
}

/// 把首尾靠近、方向也接得上的轮廓碎片接成一条长线。
///
/// 骨架在浅灰处会断成一段段：单独看每段都像随手划的短线，接起来才是
/// 手画的那一根下颌线。门槛卡得比较死（1.2mm、约 28°），宁可少接，
/// 不能把眉毛和眼线焊在一起，也不能把两段夹角大的边接成 Z 形折线。
fn link_structural(strokes: Vec<Stroke>, scale: f64) -> Vec<Stroke> {
    let n = strokes.len();
    if n < 2 {
        return strokes;
    }
    let gap = 1.2 / scale.max(1e-9);
    let mut used = vec![false; n];
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        strokes[b]
            .len_mm
            .partial_cmp(&strokes[a].len_mm)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut out: Vec<Stroke> = Vec::with_capacity(n);
    for &s in &order {
        if used[s] {
            continue;
        }
        used[s] = true;
        let mut chain = strokes[s].pts.clone();
        let mut dark = strokes[s].mean_dark;
        let mut width = strokes[s].mean_width_mm;
        let mut passes = strokes[s].passes;
        // 先向尾端接，再整体掉头接另一端（两轮后方向复原）。
        for _ in 0..2 {
            loop {
                let m = chain.len();
                if m < 2 {
                    break;
                }
                let end = chain[m - 1];
                let prev = chain[m - 2];
                let (mut ex, mut ey) = (end.0 - prev.0, end.1 - prev.1);
                let el = (ex * ex + ey * ey).sqrt().max(1e-9);
                ex /= el;
                ey /= el;
                let mut best: Option<(usize, bool, f64)> = None;
                for j in 0..n {
                    if used[j] || strokes[j].pts.len() < 2 {
                        continue;
                    }
                    let p = &strokes[j].pts;
                    let last = p.len() - 1;
                    let cand = [(p[0], p[1]), (p[last], p[last - 1])];
                    for (qi, &(q, qn)) in cand.iter().enumerate() {
                        let (vx, vy) = (q.0 - end.0, q.1 - end.1);
                        let d = (vx * vx + vy * vy).sqrt();
                        if d > gap {
                            continue;
                        }
                        let inv = 1.0 / d.max(1e-9);
                        let (vx, vy) = (vx * inv, vy * inv);
                        let (mut qdx, mut qdy) = (qn.0 - q.0, qn.1 - q.1);
                        let ql = (qdx * qdx + qdy * qdy).sqrt().max(1e-9);
                        qdx /= ql;
                        qdy /= ql;
                        if ex * vx + ey * vy < 0.88 || qdx * vx + qdy * vy < 0.88 {
                            continue;
                        }
                        if best.map_or(true, |(_, _, bd)| d < bd) {
                            best = Some((j, qi == 1, d));
                        }
                    }
                }
                let Some((j, rev, _)) = best else { break };
                used[j] = true;
                let mut pts = strokes[j].pts.clone();
                if rev {
                    pts.reverse();
                }
                if chain.last() == pts.first() {
                    pts.remove(0);
                }
                chain.extend(pts);
                dark = dark.max(strokes[j].mean_dark);
                width = width.max(strokes[j].mean_width_mm);
                passes = passes.max(strokes[j].passes);
                if chain_len(&chain) * scale > 45.0 {
                    break;
                }
            }
            chain.reverse();
        }
        let len_mm = chain_len(&chain) * scale;
        out.push(Stroke {
            theta: path_theta(&chain),
            pts: chain,
            loose: [true, true],
            len_mm,
            mean_dark: dark,
            mean_width_mm: width,
            rank: 1.0,
            structural: true,
            passes,
        });
    }
    out
}

/// 双阈值滞后：弱阈值连通域里只要有两处强响应就整段保留。
fn hysteresis_mask(detail: &[f32], w: usize, h: usize, thr: f32) -> Vec<bool> {
    let lo = thr * 0.55;
    let low: Vec<bool> = detail.iter().map(|&d| d > lo).collect();
    let mut keep = vec![false; detail.len()];
    let mut seen = vec![false; detail.len()];
    let mut comp: Vec<usize> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    for i in 0..detail.len() {
        if !low[i] || seen[i] {
            continue;
        }
        comp.clear();
        stack.push(i);
        seen[i] = true;
        let mut strong = 0usize;
        while let Some(j) = stack.pop() {
            comp.push(j);
            if detail[j] > thr {
                strong += 1;
            }
            let x = j % w;
            let y = j / w;
            let x0 = x.saturating_sub(1);
            let x1 = (x + 1).min(w - 1);
            let y0 = y.saturating_sub(1);
            let y1 = (y + 1).min(h - 1);
            for yy in y0..=y1 {
                let row = yy * w;
                for xx in x0..=x1 {
                    let k = row + xx;
                    if low[k] && !seen[k] {
                        seen[k] = true;
                        stack.push(k);
                    }
                }
            }
        }
        if strong >= 2 {
            for &j in &comp {
                keep[j] = true;
            }
        }
    }
    keep
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

/// 线稿通路：沿墨脊逐笔追踪 + 调子取舍（见本文件开头的说明）。
fn line_strokes(
    gray: &image::GrayImage,
    w: u32,
    h: u32,
    scale: f64,
    threshold: f64,
    invert: bool,
    pen_mm: f64,
    mid_frac: f64,
) -> Vec<Stroke> {
    if std::env::var("SKETCH_DEBUG").is_ok() {
        eprintln!("调子渲染：中调占比 {mid_frac:.3} → 线稿；走墨脊追踪");
    }
    let traced = trace_strokes(gray, w, h, scale, threshold, invert);
    if traced.is_empty() {
        return Vec::new();
    }
    let mut strokes: Vec<Stroke> = traced
        .into_iter()
        .map(|t| Stroke {
            theta: path_theta(&t.pts),
            len_mm: t.len_mm,
            mean_dark: t.tone,
            mean_width_mm: t.width_px * scale,
            loose: [true, true],
            pts: t.pts,
            rank: 0.5,
            structural: false,
            passes: 0,
        })
        .collect();
    // 明暗取舍：素描靠「线条疏密」表现明暗，不是把原稿每一根线都照抄一遍。
    let tone = tone_map(gray, w, h, invert, threshold, scale);
    let (keep, structural) = tone_gate(&strokes, &tone, scale, pen_mm);
    for (i, f) in structural.iter().enumerate() {
        strokes[i].structural = *f;
    }
    let mut it = keep.into_iter();
    strokes.retain(|_| it.next().unwrap_or(true));
    strokes
}

/// 收尾：排序 → 规划走笔 → 落笔。
fn finish(
    mut strokes: Vec<Stroke>,
    pressure: u32,
    jitter_mm: f64,
    pen_mm: f64,
    order_mode: u8,
    block_px: u32,
    scale: f64,
) -> Vec<Polyline> {
    assign_ranks(&mut strokes);
    strokes.sort_by(|a, b| {
        a.pts[0]
            .1
            .partial_cmp(&b.pts[0].1)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let plan = plan_order(&strokes, order_mode, block_px, scale);
    let pressure = pressure.clamp(1, 6);
    let jitter_mm = jitter_mm.max(0.0);
    let mut out = Vec::new();
    for (n, p) in plan.into_iter().enumerate() {
        emit_stroke(
            &strokes[p.idx],
            p.rev,
            n,
            pressure,
            jitter_mm,
            pen_mm,
            scale,
            &mut out,
        );
    }
    out
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

/// 按调子做「墨量预算」取舍：这一块该有多少线，由它有多暗决定。
///
/// 每格允许的墨面积 = 明暗值^γ × 增益 × 格面积；一条线落在格里的长度 × 笔宽就是
/// 它要花的墨。花得起就留下，花不起就整条丢掉——亮部于是自然留白，而不是靠用户
/// 把阈值调高（调阈值只会把暗部的线也一起丢掉，明暗关系更糟）。
///
/// γ > 1 把中间调往下压：投影笔只有一种黑度，浅灰的排线画出来和重线一样黑，
/// 所以只能靠「更少」来表现「更浅」。增益把整页墨量收在原稿附近，避免一边
/// 说要明暗、一边把整张纸涂黑。
///
/// 先后顺序按「深、宽」排：轮廓线在自己的格里最黑，所以亮部也能留下来；
/// 浅排线在同样的格里花不起墨，第一批被删掉。这正是人画素描的取舍。
fn tone_gate(
    strokes: &[Stroke],
    tone: &Tone,
    scale: f64,
    pen_mm: f64,
) -> (Vec<bool>, Vec<bool>) {
    let area = tone.cell_mm * tone.cell_mm;
    let env = |k: &str, d: f64| -> f64 {
        std::env::var(k).ok().and_then(|s| s.parse().ok()).unwrap_or(d)
    };
    let gain = env("SKETCH_TONE_GAIN", 0.70);
    let gamma = env("SKETCH_TONE_GAMMA", 1.5);
    // 死区：比这更亮的地方**一根排线都不画**。人画素描时最亮的部分就是留白，
    // 不会把纸的纹理、辅助线也描出来；投影笔一笔就是实黑，描了反而把亮部压暗。
    let dead = env("SKETCH_TONE_DEAD", 0.22);
    let caps: Vec<f32> = tone
        .cells
        .iter()
        .map(|&v| {
            let t = (v as f64).powf(gamma) * gain;
            if (v as f64) < dead {
                0.0
            } else {
                t.min(1.4) as f32 * area as f32
            }
        })
        .collect();
    // 结构线（形）：全画面最黑的那一批长线——轮廓、结构转折，素描里必须先立住。
    // 用**绝对**墨色分位而不是「比周围黑多少」：浅网格线在亮区里也是「最黑的」，
    // 相对判据会把整片背景网格当轮廓，画面立刻又变成满页线条。
    let mut tones: Vec<f64> = strokes.iter().map(|s| s.mean_dark).collect();
    tones.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let dark_cut = tones[(tones.len() * 3 / 4).min(tones.len().saturating_sub(1))]
        .max(0.55)
        .min(0.98);
    let mut used = vec![0.0f32; caps.len()];
    let mut order: Vec<usize> = (0..strokes.len()).collect();
    let imp = |s: &Stroke| -> f64 {
        // 只按「有多黑多宽」排，**不能给长度加权**：串线（从一根线窜到另一根）
        // 的产物恰恰又长又乱，一旦让长线优先，预算会被这些错线吃光，
        // 正确的短排线反而被挤掉——画面就从「密」变成「乱」。
        let width_term = (0.7 + 0.3 * (s.mean_width_mm / pen_mm).min(2.0)).min(1.3);
        s.mean_dark.powi(2) * width_term
    };
    order.sort_by(|&a, &b| {
        imp(&strokes[b])
            .partial_cmp(&imp(&strokes[a]))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut keep = vec![true; strokes.len()];
    let mut structural = vec![false; strokes.len()];
    let mut touch: Vec<(usize, f32)> = Vec::new();
    for idx in order {
        let s = &strokes[idx];
        touch.clear();
        let mut tot = 0.0f32;
        for pair in s.pts.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt() * scale;
            if len <= 0.0 {
                continue;
            }
            let mx = (a.0 + b.0) * 0.5 * scale;
            let my = (a.1 + b.1) * 0.5 * scale;
            let cx = (mx / tone.cell_mm) as isize;
            let cy = (my / tone.cell_mm) as isize;
            if cx < 0 || cy < 0 || cx >= tone.cw as isize || cy >= tone.ch as isize {
                continue;
            }
            let cell = cy as usize * tone.cw + cx as usize;
            let l = len as f32;
            tot += l;
            match touch.iter_mut().find(|e| e.0 == cell) {
                Some(e) => e.1 += l,
                None => touch.push((cell, l)),
            }
        }
        if tot <= 0.0 {
            continue;
        }
        let pen = pen_mm as f32;
        // 一条线真正耗的墨是「长度 × 笔宽 × 铺几遍」。遍数由**几何**决定，和落笔时
        // 用的是同一套算法：墨带比一笔宽才需要横向铺满，否则就是一遍。
        // （早先这里按墨色估遍数，把细线按 2.4 遍记，预算提前一倍半见底，
        // 暗部只剩一半线条——暗部于是不够暗，刚好反了。）
        let est = ((s.mean_width_mm / pen_mm).clamp(1.0, 2.5)) as f32;
        // 这条线沿路的「背景调子」：取最亮的那一格，而不是平均值。
        // 平均值会被线自己撑高——轮廓在自己的格里占了大半，平均下来反而像暗部，
        // 判据就永远不成立。最亮的格子才代表它走过的背景。
        let local = touch
            .iter()
            .map(|&(c, _)| tone.cells[c])
            .fold(1.0f32, f32::min);
        // **结构线免死**：墨色进了全画面最重的那一批、又够长，就是「形」。
        // 它们常常走在亮区边上，亮区的预算接近 0，按纯墨量算必然被砍；
        // 砍掉轮廓，画面就散成一堆排线，明暗再对也认不出画的是什么。
        let contour = s.mean_dark >= dark_cut && s.len_mm >= 4.0 && local < 0.75;
        if contour {
            keep[idx] = true;
            structural[idx] = true;
            for &(c, l) in touch.iter() {
                // 结构线也要占一点预算，否则亮区会被轮廓塞满。
                used[c] = (used[c] + l * pen * est * 0.45).min(caps[c] * 1.4);
            }
            continue;
        }
        // **整条取舍，不能拦腰截断**。线段的预算按格算，但决定要按整条线做：
        // 一条 80mm 的竖排线横穿 20 个格，只要过半的格还有余量就整条画下去。
        // 早先按「超过 1/4 的墨落不下就整条丢掉」，长线几乎必被砍；改成逐段
        // 截断又会让线在半空中断掉、画面显得残缺——那不像人画的，像打印机缺墨。
        let mut fit = 0usize;
        let mut nc = 0usize;
        for &(c, l) in touch.iter() {
            nc += 1;
            if used[c] + l * pen * est <= caps[c] * 1.15 {
                fit += 1;
            }
        }
        if nc > 0 && fit * 2 < nc {
            keep[idx] = false;
            continue;
        }
        for &(c, l) in touch.iter() {
            let cap = caps[c] * 1.4;
            used[c] = (used[c] + l * pen * est).min(cap);
        }
    }
    (keep, structural)
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
    /// 中调（0.15..0.7）像素占比：用来判断这是色调稿还是线稿
    mid_frac: f64,
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
        let mut mid = 0usize;
        for v in raw.iter_mut() {
            let t = ((paper - *v) / span).max(0.0);
            *v = t;
            if t > 0.15 && t < 0.7 {
                mid += 1;
            }
        }
        let mid_frac = mid as f64 / n as f64;

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
            mid_frac,
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
        let hx = (x * 0.5 - 0.5).max(0.0);        let hy = (y * 0.5 - 0.5).max(0.0);
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

    /// (原稿笔触走向, 可信度)：跟得住单根发丝、排线，尺度约 3px。
    ///
    /// 和 `ang_at` 的区别是尺度：`ang_at` 是 2.5mm 的「一片区域一个走向」，
    /// 这个场只看 0.6mm 内、按脊响应加权的方向。分区重绘要「参考原稿的
    /// 排线方向」，用的是后者；成片明暗里没有脊，可信度自然为 0，回落到
    /// 手绘规则角。
    fn ridge_ang_at(&self, x: f64, y: f64) -> (f32, f32) {
        let c2 = bil(&self.rdc, self.w, self.h, x, y);
        let s2 = bil(&self.rds, self.w, self.h, x, y);
        (0.5 * s2.atan2(c2), bil(&self.rcoh, self.w, self.h, x, y))
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

    /// 这张图是「色调稿」还是「线稿」。
    ///
    /// 线稿的白纸与线条之间几乎没有中间调；铅笔素描靠渐变与排线出调子，
    /// 中调像素占比高得多。分不清这两类就会用错算法：拿追踪器画素描是乱涂，
    /// 拿排线画线稿则会把干净的线条糊成一片阴影。
    fn is_tonal(&self) -> bool {
        self.mid_frac > 0.09
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

/// 笔触提取：把原稿上真实画出来的铅笔线条一根根描出来。
///
/// 和「调子排线」的根本差别：排线是**照明暗重新编**一组线，位置和走向都跟原稿
/// 对不上，所以怎么调都"不像"；这里描的是原稿本来那一根——发丝、眼线、唇线、
/// 衬衫的斜排线，位置、走向、长短都是原作定的。
///
/// 流程有四步，每一步都对应一次实测失败：
/// 1. **多尺度 Hessian 暗脊响应** → 只有「法向弯、沿脊平」的像素才有响应，
///    抹开的阴影拿不到响应，所以不需要"画线还是画明暗"的开关；
/// 2. **法向 NMS** 削成 1px 脊线。直接在响应平台上阈值化会连成一片；
/// 3. **滞后阈值** 接断点。只用一个门槛切，同一根发丝会被切成好几截——
///    实测头发区每行只有 22 段、段长中位 3.2mm，暗部墨量只有原稿的 25%。
///    接上之后暗部墨量回到 76%；
/// 4. **沿骨架追踪**成长折线，平滑、抽稀，再按笔宽拉开间距落笔。
/// 墨量自校准：先按默认间距跑一遍，量出「画了多少墨 / 原稿有多少墨」，
/// 偏了就把间距按比例重跑一遍。
///
/// 为什么必须有这一步：不同稿子用同一套常量，墨量能差到 0.45~1.29——
/// 09_1 淡得只剩轮廓，05_1 又把脸糊住。差别不在参数，在于**每张稿子能被
/// 提取出来的笔触有多密**：有的原稿笔触清晰、一笔一画都提得到，有的大片
/// 是擦揉出来的调子、根本没有线可提。间距是唯一的墨量旋钮，而它该拧多少
/// 只有量过才知道，所以先量再定。
fn ridge_strokes(
    f: &Field,
    pen_mm: f64,
    thr_slider: f64,
    density: f64,
) -> (Vec<Stroke>, Vec<u8>) {
    ridge_strokes_gated(f, pen_mm, thr_slider, density, 0.20, 0.004, true)
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

/// 色调稿的完整笔迹：把原稿上真实画出来的铅笔线一根根描下来，再缝合断头。
///
/// 只有这一条路。所有"按明暗重新排一套线"的尝试（调子排线、稀疏补白、
/// 第二轮低门槛追踪）都试过了，结果一样：画面立刻变成两层笔迹——一层跟着
/// 形体走、一层是等距直梳，怎么调都脏。原稿的每根线都是原作画的，位置、粗细、
/// 走向都有理由，编不出来。
fn tonal_strokes(f: &Field, pen_mm: f64, threshold: f64, _pressure: u32, _seed: u64) -> Vec<Stroke> {
    let (out, _covered) = ridge_strokes(f, pen_mm, threshold, 1.0);
    if std::env::var("SKETCH_DEBUG").is_ok() {
        eprintln!(
            "笔触渲染：中调占比 {:.3} → 色调稿；{} 条，总长 {:.0}mm",
            f.mid_frac,
            out.len(),
            out.iter().map(|s| s.len_mm).sum::<f64>()
        );
    }
    out
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

/// 「笔重」：越黑、越粗越重。用来给全部笔迹排一个分位。
fn stroke_weight(s: &Stroke) -> f64 {
    let w = ((s.mean_width_mm - 0.25) / 0.75).clamp(0.0, 1.0);
    (0.8 * s.mean_dark + 0.2 * w).clamp(0.0, 1.0)
}

/// 给每条笔迹打上笔重分位（0..1），供分层与叠描遍数使用。
fn assign_ranks(strokes: &mut [Stroke]) {
    let n = strokes.len();
    if n == 0 {
        return;
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        stroke_weight(&strokes[a])
            .partial_cmp(&stroke_weight(&strokes[b]))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for (i, &idx) in order.iter().enumerate() {
        strokes[idx].rank = if n > 1 {
            i as f64 / (n - 1) as f64
        } else {
            0.5
        };
    }
}

/// 按笔重分位分层：0 起稿轻线 / 1 主体 / 2 暗部强调。
///
/// 手绘的顺序就是先轻后重：先打稿定位，再描主体，最后压暗部。
/// 用分位而不是绝对明暗，任何曝光差异的原稿都能分出同样的三层。
fn tone_layers(strokes: &[Stroke]) -> Vec<u8> {
    strokes
        .iter()
        .map(|s| {
            if s.rank < 0.35 {
                0
            } else if s.rank < 0.72 {
                1
            } else {
                2
            }
        })
        .collect()
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

fn direction_bin(theta: f64) -> usize {
    let b = (theta / (std::f64::consts::PI / DIR_BINS as f64)).floor() as i64;
    (b.rem_euclid(DIR_BINS as i64)) as usize
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

/// 默认顺序：按力度分层；每层先长线、再短线按方向分组（排线感）。
fn layered_plan(strokes: &[Stroke]) -> Vec<Planned> {
    let layers = tone_layers(strokes);
    let mut buckets: [Vec<usize>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for (i, &l) in layers.iter().enumerate() {
        buckets[l as usize].push(i);
    }
    let mut plan = Vec::with_capacity(strokes.len());
    let mut cur = (0.0, 0.0);
    for bucket in buckets.iter() {
        let mut longs = Vec::new();
        let mut shorts = Vec::new();
        for &i in bucket {
            if strokes[i].len_mm >= LONG_STROKE_MM {
                longs.push(i);
            } else {
                shorts.push(i);
            }
        }
        longs.sort_by(|&a, &b| {
            let ba = bounds(&strokes[a].pts);
            let bb = bounds(&strokes[b].pts);
            ((ba.1 / 10.0).floor() as i64)
                .cmp(&((bb.1 / 10.0).floor() as i64))
                .then(ba.0.partial_cmp(&bb.0).unwrap_or(std::cmp::Ordering::Equal))
        });
        for i in longs {
            let (rev, exit) = orient_near(&strokes[i], cur);
            cur = exit;
            plan.push(Planned { idx: i, rev });
        }
        let mut bins: [Vec<usize>; DIR_BINS] = std::array::from_fn(|_| Vec::new());
        for i in shorts {
            bins[direction_bin(strokes[i].theta)].push(i);
        }
        let mut bin_order: Vec<usize> = (0..DIR_BINS).collect();
        bin_order.sort_by(|&a, &b| {
            let la: f64 = bins[a].iter().map(|&i| strokes[i].len_mm).sum();
            let lb: f64 = bins[b].iter().map(|&i| strokes[i].len_mm).sum();
            lb.partial_cmp(&la)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.cmp(&b))
        });
        for b in bin_order {
            if bins[b].is_empty() {
                continue;
            }
            let theta = (b as f64 + 0.5) * std::f64::consts::PI / DIR_BINS as f64;
            let (nx, ny) = (-theta.sin(), theta.cos());
            let mut ids = bins[b].clone();
            ids.sort_by(|&a, &c| {
                let ca = centroid(&strokes[a].pts);
                let cc = centroid(&strokes[c].pts);
                let na = ca.0 * nx + ca.1 * ny;
                let nc = cc.0 * nx + cc.1 * ny;
                na.partial_cmp(&nc)
                    .unwrap_or(std::cmp::Ordering::Equal)
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

fn plan_order(
    strokes: &[Stroke],
    mode: u8,
    block_px: u32,
    scale: f64,
) -> Vec<Planned> {
    match mode {
        1 => {
            if strokes.len() <= 4000 {
                nearest_plan(strokes)
            } else {
                layered_plan(strokes)
            }
        }
        2 => scan_plan(strokes),
        3 => block_plan(strokes, block_px, scale),
        _ => layered_plan(strokes),
    }
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
