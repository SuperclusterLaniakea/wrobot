//! 渲染：把排版结果展开为笔尖路径（页面毫米坐标）。

use crate::font::FontStack;
use crate::layout::Placement;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// 一条连续笔迹（落笔→抬笔之间）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Polyline {
    pub pts: Vec<(f64, f64)>,
}

impl Polyline {
    pub fn new(pts: Vec<(f64, f64)>) -> Self {
        Polyline { pts }
    }
    pub fn len(&self) -> usize {
        self.pts.len()
    }
    pub fn is_empty(&self) -> bool {
        self.pts.is_empty()
    }
    /// 路径总长（毫米）
    pub fn length(&self) -> f64 {
        self.pts
            .windows(2)
            .map(|w| ((w[1].0 - w[0].0).powi(2) + (w[1].1 - w[0].1).powi(2)).sqrt())
            .sum()
    }
    pub fn bounds(&self) -> (f64, f64, f64, f64) {
        if self.pts.is_empty() {
            return (0.0, 0.0, 0.0, 0.0);
        }
        let mut x0 = f64::MAX;
        let mut y0 = f64::MAX;
        let mut x1 = f64::MIN;
        let mut y1 = f64::MIN;
        for (x, y) in &self.pts {
            x0 = x0.min(*x);
            y0 = y0.min(*y);
            x1 = x1.max(*x);
            y1 = y1.max(*y);
        }
        (x0, y0, x1, y1)
    }
}

/// 把一批落笔位置渲染为笔迹折线。
///
/// # 居中策略（修复「格子里文字中心和格子中心对不齐」）
/// 各字库的字形原点不同：`.gfont` 原点在**基线**（字身几乎全在 y<0），
/// 内置字体原点在 em 框**左上角**。若按原点直接放置，字会整体偏上并溢出字框。
/// 这里统一用字库的「典型墨迹框」 [`crate::font::InkMetrics`] 做补偿：
/// - 纵向：墨迹中心 → 字框中心（字框边长 = `p.size`）；
/// - 横向：墨迹中心 → 前进宽度框中心（前进宽度 = `p.size × advance/em`）。
pub fn render(placements: &[Placement], stack: &FontStack) -> Vec<Polyline> {
    let mut out = Vec::new();
    for p in placements {
        let font = match stack.font(p.font_idx) {
            Some(f) => f,
            None => continue,
        };
        let g = match font.glyph(p.ch) {
            Some(g) => g,
            None => continue,
        };
        let em = font.em() as f64;
        let s = p.size / em;
        let adv = if g.advance > 0.0 { g.advance as f64 } else { em };
        let ink = font.ink();
        let (icx, icy) = ink.center();
        // 墨迹中心落到目标框中心所需的补偿量（毫米）
        let off_x = p.size * (adv / em) * 0.5 - icx as f64 * s + p.ink_dx;
        let off_y = p.size * 0.5 - icy as f64 * s + p.ink_dy;
        let (cos, sin) = (p.rot.cos(), p.rot.sin());
        for stroke in &g.strokes {
            if stroke.points.len() < 2 {
                continue;
            }
            let pts: Vec<(f64, f64)> = stroke
                .points
                .iter()
                .map(|&(gx, gy)| {
                    let x = p.x + gx as f64 * s + off_x;
                    let y = p.y + gy as f64 * s + off_y;
                    // 绕元素原点旋转
                    let dx = x - p.ox;
                    let dy = y - p.oy;
                    (
                        p.ox + dx * cos - dy * sin,
                        p.oy + dx * sin + dy * cos,
                    )
                })
                .collect();
            out.push(Polyline::new(pts));
        }
    }
    out
}

/// 把线段加粗到 `width_mm`。
///
/// 笔尖无法真正变粗，因此用**多条平行描边**实现视觉加粗：
/// `width_mm ≤ 0.35` 时保持单笔；否则平分成 n 条间距 `width/n` 的并行线
/// （n 上限 6，避免线条过多拖慢绘制）。
pub fn thicken_lines(lines: &[Vec<(f64, f64)>], width_mm: f64) -> Vec<Vec<(f64, f64)>> {
    const SINGLE: f64 = 0.35;
    const MAX_PASS: usize = 6;
    if width_mm <= SINGLE || lines.is_empty() {
        return lines.to_vec();
    }
    let n = ((width_mm / SINGLE).round() as usize).clamp(2, MAX_PASS);
    let step = width_mm / n as f64;
    let mut out = Vec::with_capacity(lines.len() * n);
    for seg in lines {
        let (a, b) = match (seg.first(), seg.last()) {
            (Some(a), Some(b)) if a != b => (*a, *b),
            _ => {
                out.push(seg.clone());
                continue;
            }
        };
        let dx = b.0 - a.0;
        let dy = b.1 - a.1;
        let len = (dx * dx + dy * dy).sqrt().max(1e-9);
        // 单位法向
        let (nx, ny) = (-dy / len, dx / len);
        for k in 0..n {
            let off = (k as f64 - (n as f64 - 1.0) * 0.5) * step;
            let (ox, oy) = (nx * off, ny * off);
            out.push(
                seg.iter()
                    .map(|(x, y)| (x + ox, y + oy))
                    .collect::<Vec<(f64, f64)>>(),
            );
        }
    }
    out
}

/// 全部笔迹的外接矩形 (x0,y0,x1,y1)。

/// 一条线段（页面毫米坐标）。
pub fn segment(x1: f64, y1: f64, x2: f64, y2: f64) -> Polyline {
    Polyline::new(vec![(x1, y1), (x2, y2)])
}

/// 一个矩形的四条边（闭合，4 条独立笔迹）。
pub fn rect(x: f64, y: f64, w: f64, h: f64) -> Vec<Polyline> {
    vec![
        segment(x, y, x + w, y),
        segment(x + w, y, x + w, y + h),
        segment(x + w, y + h, x, y + h),
        segment(x, y + h, x, y),
    ]
}

/// 用 `step` 等间距把矩形切为 `cols × rows` 网格线条（含外框）。
pub fn grid(x: f64, y: f64, w: f64, h: f64, cols: usize, rows: usize) -> Vec<Polyline> {
    let mut out = Vec::new();
    let c = cols.max(1) as f64;
    let r = rows.max(1) as f64;
    for i in 0..=cols {
        let gx = x + w * (i as f64) / c;
        out.push(segment(gx, y, gx, y + h));
    }
    for j in 0..=rows {
        let gy = y + h * (j as f64) / r;
        out.push(segment(x, gy, x + w, gy));
    }
    out
}

/// 米字格：外框 + 十字 + 两条对角线。
pub fn cross_grid(x: f64, y: f64, w: f64, h: f64) -> Vec<Polyline> {
    vec![
        segment(x, y, x + w, y),
        segment(x + w, y, x + w, y + h),
        segment(x + w, y + h, x, y + h),
        segment(x, y + h, x, y),
        // 十字
        segment(x + w / 2.0, y, x + w / 2.0, y + h),
        segment(x, y + h / 2.0, x + w, y + h / 2.0),
        // 斜叉（米字）
        segment(x, y, x + w, y + h),
        segment(x + w, y, x, y + h),
    ]
}
/// 虚线矩形：沿四条边按 `dash` 长度画短划（间隔同为 `dash`）。
pub fn dashed_rect(x: f64, y: f64, w: f64, h: f64, dash: f64) -> Vec<Polyline> {
    let d = dash.max(0.1);
    let corners = [(x, y), (x + w, y), (x + w, y + h), (x, y + h), (x, y)];
    let mut out = Vec::new();
    for w2 in corners.windows(2) {
        let (ax, ay) = w2[0];
        let (bx, by) = w2[1];
        let len = ((bx - ax).powi(2) + (by - ay).powi(2)).sqrt();
        if len <= 0.0 {
            continue;
        }
        let ux = (bx - ax) / len;
        let uy = (by - ay) / len;
        let mut t = 0.0;
        while t < len {
            let t2 = (t + d).min(len);
            out.push(segment(
                ax + ux * t,
                ay + uy * t,
                ax + ux * t2,
                ay + uy * t2,
            ));
            t = t2 + d;
        }
    }
    out
}

pub fn bounds_all(paths: &[Polyline]) -> (f64, f64, f64, f64) {
    let mut x0 = f64::MAX;
    let mut y0 = f64::MAX;
    let mut x1 = f64::MIN;
    let mut y1 = f64::MIN;
    for p in paths {
        if p.is_empty() {
            continue;
        }
        let (a, b, c, d) = p.bounds();
        x0 = x0.min(a);
        y0 = y0.min(b);
        x1 = x1.max(c);
        y1 = y1.max(d);
    }
    if x0 == f64::MAX {
        (0.0, 0.0, 0.0, 0.0)
    } else {
        (x0, y0, x1, y1)
    }
}

/// 笔迹总长度（毫米）。
pub fn total_length(paths: &[Polyline]) -> f64 {
    paths.iter().map(|p| p.length()).sum()
}

/// 把任意坐标系下的折线**归一化到页面毫米坐标**（y 向下、原点左上），便于矢量导入后直接并入画布。
///
/// - `flip_y = true`：源坐标系 Y 向上（如 DXF），翻转后顶部对齐页面顶部；
/// - `flip_y = false`：源坐标系已是 y 向下（如 SVG），直接平移。
///
/// 等比缩放使**高度**恰好等于 `target_h_mm`，左上角锚定在 (0,0)。
/// 宽度若超出页面，只表示图比纸宽，由用户后续缩放/平移。
pub fn fit_to_page(paths: &[Polyline], target_h_mm: f64, flip_y: bool) -> Vec<Polyline> {
    if paths.is_empty() {
        return Vec::new();
    }
    let (x0, y0, _x1, y1) = bounds_all(paths);
    let h = (y1 - y0).max(1e-6);
    let scale = (target_h_mm / h).max(1e-9);
    paths
        .iter()
        .map(|p| {
            Polyline::new(
                p.pts
                    .iter()
                    .map(|&(x, y)| {
                        let px = (x - x0) * scale;
                        let py = if flip_y {
                            (y1 - y) * scale
                        } else {
                            (y - y0) * scale
                        };
                        (px, py)
                    })
                    .collect(),
            )
        })
        .collect()
}

/// 两点距离。
fn dist_pt(p: (f64, f64), q: (f64, f64)) -> f64 {
    let dx = p.0 - q.0;
    let dy = p.1 - q.1;
    (dx * dx + dy * dy).sqrt()
}

/// 重排前的一条笔画：缓存两端点与长度，便于快速决策「从哪一端起笔」。
struct Stroke {
    poly: Polyline,
    a: (f64, f64),
    b: (f64, f64),
    len: f64,
}

/// 按 **G-code 实际会走的样子** 计算整批笔画的笔尖总行程（毫米）：
/// 每条从 `pts[0]` 画到末点，再抬笔空移到下一条的 `pts[0]`。
///
/// 这是衡量绘制耗时的核心指标——空移 + 抬落笔次数才是慢的主因，而不是墨迹长度。
pub fn path_travel(paths: &[Polyline], start: (f64, f64)) -> f64 {
    let (mut total, mut cur) = (0.0, start);
    for p in paths {
        if p.len() < 2 {
            continue;
        }
        let a = p.pts[0];
        let b = *p.pts.last().unwrap();
        total += dist_pt(cur, a) + p.length();
        cur = b;
    }
    total
}

/// 给定播放顺序，按「就近起笔」决定每条是否反向后的实际行程（毫米）。
fn cost_of_order(strokes: &[Stroke], order: &[usize], start: (f64, f64)) -> f64 {
    let (mut total, mut cur) = (0.0, start);
    for &i in order {
        let s = &strokes[i];
        let da = dist_pt(cur, s.a);
        let db = dist_pt(cur, s.b);
        if da <= db {
            total += da + s.len;
            cur = s.b;
        } else {
            total += db + s.len;
            cur = s.a;
        }
    }
    total
}

/// 按给定顺序输出笔画，并按「就近起笔」自动定向——使实际行程与 [`cost_of_order`] 完全一致。
fn orient_sequence(strokes: &[Stroke], order: &[usize], start: (f64, f64)) -> Vec<Polyline> {
    let mut cur = start;
    order
        .iter()
        .map(|&i| {
            let s = &strokes[i];
            let mut pts = s.poly.pts.clone();
            // 判断要不要反过来画：从离笔尖更近的那端起笔
            if dist_pt(cur, s.b) < dist_pt(cur, s.a) {
                pts.reverse();
            }
            cur = *pts.last().unwrap();
            Polyline::new(pts)
        })
        .collect()
}

/// 从零点开始做最优路径规划，见 [`optimize_path_order_from`]。
pub fn optimize_path_order(paths: Vec<Polyline>) -> Vec<Polyline> {
    optimize_path_order_from(paths, (0.0, 0.0))
}

/// 重排笔画顺序，让笔尖**少跑冤枉路**，绘制时间最短。
///
/// 写字机上每条折线对应一次「抬笔 → 快速移动 → 落笔」（见 `generate`），
/// 所以总耗时 ≈ 空移距离 + 抬落笔次数，而逐行扫描正是这两种浪费的典型：
/// 每行都是左→右画完再跳回下一行最左端，空移等于整幅宽度。
///
/// 算法（TSP 启发式，不保证全局最优，但远好于原顺序）：
/// 1. **构造**：最近邻——每次挑选「离当前笔尖最近的那一端」所在的笔画，并入路径；
/// 2. **精修**：相邻元素交换的 2-opt，最多 [`OPT_ROUNDS`] 轮，用同一个代价函数整体评估；
/// 3. **定向**：输出时按就近起笔决定每条是否反向，保证 G-code 实走距离就是算出来的距离。
///
/// 笔画本身不会被改动（不增删点、长度守恒），只调整**顺序与方向**。
pub fn optimize_path_order_from(paths: Vec<Polyline>, start: (f64, f64)) -> Vec<Polyline> {
    /// 精修轮数上限。
    const OPT_ROUNDS: usize = 4;
    /// 超过这么多笔画就跳过精修（O(n²) 每轮，避免大图拖慢界面）。
    const REFINE_MAX: usize = 2000;

    let strokes: Vec<Stroke> = paths
        .into_iter()
        .filter(|p| p.len() >= 2)
        .map(|poly| {
            let a = poly.pts[0];
            let b = *poly.pts.last().unwrap();
            let len = poly.length();
            Stroke { poly, a, b, len }
        })
        .collect();
    let n = strokes.len();
    if n < 2 {
        // 一条笔画没有顺序可言，原样返回
        return strokes.into_iter().map(|s| s.poly).collect();
    }

    // ① 最近邻构造
    let mut used = vec![false; n];
    let mut order: Vec<usize> = Vec::with_capacity(n);
    let mut cur = start;
    for _ in 0..n {
        let mut best = usize::MAX;
        let mut best_d = f64::INFINITY;
        for i in 0..n {
            if used[i] {
                continue;
            }
            let s = &strokes[i];
            let d = dist_pt(cur, s.a).min(dist_pt(cur, s.b));
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        if best == usize::MAX {
            break;
        }
        used[best] = true;
        let s = &strokes[best];
        // 走到这一笔画完后的落点
        cur = if dist_pt(cur, s.a) <= dist_pt(cur, s.b) {
            s.b
        } else {
            s.a
        };
        order.push(best);
    }

    // ② 2-opt 精修：相邻两条互换，代价变小就接受
    if order.len() <= REFINE_MAX {
        let mut cost = cost_of_order(&strokes, &order, start);
        for _ in 0..OPT_ROUNDS {
            let mut improved = false;
            for i in 0..order.len().saturating_sub(1) {
                let mut cand = order.clone();
                cand.swap(i, i + 1);
                let c = cost_of_order(&strokes, &cand, start);
                if c < cost - 1e-9 {
                    order = cand;
                    cost = c;
                    improved = true;
                }
            }
            if !improved {
                break;
            }
        }
    }

    // ③ 定向输出
    orient_sequence(&strokes, &order, start)
}

/// G-code 路径优化方式（只改顺序与起笔端，不增删笔迹）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PathOrder {
    /// 保持生成顺序。
    #[default]
    Keep,
    /// 蛇形扫描：按笔迹的主要走向分成条带，条带间来回走。
    Serpentine,
    /// 全局最近邻（网格加速，适合几千到十几万条笔迹）。
    Nearest,
    /// 同向聚类：先把角度相近的笔迹分到一组，再在组内顺序排布。
    Direction,
    /// 分区最近邻：先按网格分块，块间最近邻，块内再最近邻。
    Block,
}

impl PathOrder {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Serpentine,
            2 => Self::Nearest,
            3 => Self::Direction,
            4 => Self::Block,
            _ => Self::Keep,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Keep => "保持生成顺序",
            Self::Serpentine => "蛇形扫描（横/纵自动）",
            Self::Nearest => "最近邻（全局）",
            Self::Direction => "同向聚类（按笔向分组）",
            Self::Block => "分区最近邻（网格）",
        }
    }
}

fn cell_of(p: (f64, f64), cell: f64) -> (i32, i32) {
    ((p.0 / cell).floor() as i32, (p.1 / cell).floor() as i32)
}

fn mid_of(s: &Stroke) -> (f64, f64) {
    ((s.a.0 + s.b.0) * 0.5, (s.a.1 + s.b.1) * 0.5)
}

/// 端点网格索引：一条笔迹的起点和终点都登记进去，找最近的下一笔时
/// 从当前笔尖所在格向外一圈圈找，避免 O(n²) 全表扫描。
struct EndGrid {
    cell: f64,
    map: HashMap<(i32, i32), Vec<usize>>,
}

impl EndGrid {
    fn new(strokes: &[Stroke], subset: &[usize], cell: f64) -> Self {
        let mut map: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
        for &i in subset {
            let s = &strokes[i];
            map.entry(cell_of(s.a, cell)).or_default().push(i);
            let cb = cell_of(s.b, cell);
            let ca = cell_of(s.a, cell);
            if cb != ca {
                map.entry(cb).or_default().push(i);
            }
        }
        EndGrid { cell, map }
    }

    /// 返回 `(笔迹下标, 是否反向)`。
    fn nearest(&self, strokes: &[Stroke], used: &[bool], cur: (f64, f64)) -> Option<(usize, bool)> {
        let (cx, cy) = cell_of(cur, self.cell);
        let mut best: Option<(usize, bool, f64)> = None;
        let mut found_ring = usize::MAX;
        for r in 0..=240usize {
            for dx in -(r as i32)..=(r as i32) {
                for dy in -(r as i32)..=(r as i32) {
                    if dx.abs().max(dy.abs()) != r as i32 {
                        continue;
                    }
                    let Some(list) = self.map.get(&(cx + dx, cy + dy)) else {
                        continue;
                    };
                    for &i in list {
                        if used[i] {
                            continue;
                        }
                        let s = &strokes[i];
                        let da = dist_pt(cur, s.a);
                        let db = dist_pt(cur, s.b);
                        let (d, rev) = if da <= db { (da, false) } else { (db, true) };
                        if best.map_or(true, |(_, _, bd)| d < bd) {
                            best = Some((i, rev, d));
                        }
                    }
                }
            }
            if best.is_some() {
                if found_ring == usize::MAX {
                    found_ring = r;
                }
                // 多扫一圈，避免「第一个非空格里的最远点」输给隔壁格的近点。
                if r >= found_ring + 1 {
                    break;
                }
            }
        }
        best.map(|(i, rev, _)| (i, rev))
    }
}

/// 最近邻顺序；返回访问顺序和最终笔尖位置。
fn nearest_sequence(
    strokes: &[Stroke],
    subset: &[usize],
    start: (f64, f64),
) -> (Vec<usize>, (f64, f64)) {
    let mut used = vec![false; strokes.len()];
    let grid = EndGrid::new(strokes, subset, 8.0);
    let mut order = Vec::with_capacity(subset.len());
    let mut cur = start;
    for _ in 0..subset.len() {
        let Some((i, rev)) = grid.nearest(strokes, &used, cur) else {
            break;
        };
        used[i] = true;
        order.push(i);
        cur = if rev { strokes[i].a } else { strokes[i].b };
    }
    (order, cur)
}

/// 蛇形扫描：按笔迹主走向切成约 20 条带，带内沿长边排序，带间交替方向。
fn serpentine_sequence(strokes: &[Stroke], subset: &[usize]) -> Vec<usize> {
    let (mut sx, mut sy) = (0.0f64, 0.0f64);
    for &i in subset {
        let s = &strokes[i];
        sx += (s.b.0 - s.a.0).abs() * s.len.max(1e-6);
        sy += (s.b.1 - s.a.1).abs() * s.len.max(1e-6);
    }
    // 横线多 → 按行（y）分带、带内按 x 走；竖线多则反过来。
    let rows = sx >= sy;
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for &i in subset {
        let m = mid_of(&strokes[i]);
        let p = if rows { m.1 } else { m.0 };
        lo = lo.min(p);
        hi = hi.max(p);
    }
    let band = ((hi - lo).max(1.0) / 20.0).clamp(5.0, 40.0);
    let mut groups: BTreeMap<i32, Vec<usize>> = BTreeMap::new();
    for &i in subset {
        let m = mid_of(&strokes[i]);
        let p = if rows { m.1 } else { m.0 };
        let key = (((p - lo) / band).floor() as i32).min(i32::MAX - 1);
        groups.entry(key).or_default().push(i);
    }
    let mut out = Vec::with_capacity(subset.len());
    for (k, (_, mut list)) in groups.into_iter().enumerate() {
        list.sort_by(|&i, &j| {
            let a = mid_of(&strokes[i]);
            let b = mid_of(&strokes[j]);
            let (pa, pb) = if rows { (a.0, b.0) } else { (a.1, b.1) };
            pa.partial_cmp(&pb).unwrap_or(std::cmp::Ordering::Equal)
        });
        if k % 2 == 1 {
            list.reverse();
        }
        out.extend(list);
    }
    out
}

fn angle_of(s: &Stroke) -> f64 {
    let a = (s.b.1 - s.a.1).atan2(s.b.0 - s.a.0);
    if a < 0.0 {
        a + std::f64::consts::PI
    } else {
        a
    }
}

/// 同向聚类：按 15° 分桶，桶按离笔尖远近依次处理，桶内沿方向投影排序。
fn direction_sequence(strokes: &[Stroke], subset: &[usize], start: (f64, f64)) -> Vec<usize> {
    const BINS: usize = 12;
    let mut bins: Vec<Vec<usize>> = vec![Vec::new(); BINS];
    for &i in subset {
        let b = ((angle_of(&strokes[i]) / std::f64::consts::PI * BINS as f64) as usize).min(BINS - 1);
        bins[b].push(i);
    }
    let mut remaining: Vec<usize> = (0..BINS).filter(|&b| !bins[b].is_empty()).collect();
    let mut out = Vec::with_capacity(subset.len());
    let mut cur = start;
    while !remaining.is_empty() {
        let centroid = |b: usize| -> (f64, f64) {
            let list = &bins[b];
            let mut sx = 0.0;
            let mut sy = 0.0;
            for &i in list {
                let m = mid_of(&strokes[i]);
                sx += m.0;
                sy += m.1;
            }
            (sx / list.len() as f64, sy / list.len() as f64)
        };
        let pick = *remaining
            .iter()
            .min_by(|&&a, &&b| {
                dist_pt(cur, centroid(a))
                    .partial_cmp(&dist_pt(cur, centroid(b)))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap();
        remaining.retain(|&b| b != pick);
        let angle = (pick as f64 + 0.5) / BINS as f64 * std::f64::consts::PI;
        let dir = (angle.cos(), angle.sin());
        let mut list = std::mem::take(&mut bins[pick]);
        let proj = |i: usize| {
            let m = mid_of(&strokes[i]);
            m.0 * dir.0 + m.1 * dir.1
        };
        list.sort_by(|&i, &j| {
            proj(i)
                .partial_cmp(&proj(j))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let first = mid_of(&strokes[list[0]]);
        let last = mid_of(&strokes[*list.last().unwrap()]);
        if dist_pt(cur, last) < dist_pt(cur, first) {
            list.reverse();
        }
        cur = mid_of(&strokes[*list.last().unwrap()]);
        out.extend(list);
    }
    out
}

/// 分区最近邻：50mm 网格分块，块间最近邻，块内再最近邻。
fn block_sequence(
    strokes: &[Stroke],
    subset: &[usize],
    start: (f64, f64),
    page_w: f64,
    page_h: f64,
) -> Vec<usize> {
    let cell = (page_w.max(page_h) / 8.0).clamp(25.0, 60.0);
    let mut groups: BTreeMap<(i32, i32), Vec<usize>> = BTreeMap::new();
    for &i in subset {
        let m = mid_of(&strokes[i]);
        groups.entry(cell_of(m, cell)).or_default().push(i);
    }
    let mut remaining: Vec<((i32, i32), Vec<usize>)> = groups.into_iter().collect();
    let mut out = Vec::with_capacity(subset.len());
    let mut cur = start;
    while !remaining.is_empty() {
        let center = |list: &[usize]| -> (f64, f64) {
            let mut sx = 0.0;
            let mut sy = 0.0;
            for &i in list {
                let m = mid_of(&strokes[i]);
                sx += m.0;
                sy += m.1;
            }
            (sx / list.len() as f64, sy / list.len() as f64)
        };
        let pick = remaining
            .iter()
            .enumerate()
            .min_by(|(_, (_, a)), (_, (_, b))| {
                dist_pt(cur, center(a))
                    .partial_cmp(&dist_pt(cur, center(b)))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(k, _)| k)
            .unwrap();
        let (_, list) = remaining.swap_remove(pick);
        let (seq, end) = nearest_sequence(strokes, &list, cur);
        out.extend(seq);
        cur = end;
    }
    out
}

/// 按指定方式重排笔迹（并就近决定起笔端），让空移尽量短。
///
/// 只调整**顺序与方向**：笔迹数量、点数、墨线长度都不变。
pub fn order_paths(
    paths: Vec<Polyline>,
    order: PathOrder,
    page_w: f64,
    page_h: f64,
) -> Vec<Polyline> {
    if order == PathOrder::Keep {
        return paths;
    }
    let strokes: Vec<Stroke> = paths
        .into_iter()
        .filter(|p| p.len() >= 2)
        .map(|poly| {
            let a = poly.pts[0];
            let b = *poly.pts.last().unwrap();
            let len = poly.length();
            Stroke { poly, a, b, len }
        })
        .collect();
    let n = strokes.len();
    if n < 2 {
        return strokes.into_iter().map(|s| s.poly).collect();
    }
    let subset: Vec<usize> = (0..n).collect();
    let seq = match order {
        PathOrder::Keep => subset,
        PathOrder::Serpentine => serpentine_sequence(&strokes, &subset),
        PathOrder::Nearest => nearest_sequence(&strokes, &subset, (0.0, 0.0)).0,
        PathOrder::Direction => direction_sequence(&strokes, &subset, (0.0, 0.0)),
        PathOrder::Block => block_sequence(&strokes, &subset, (0.0, 0.0), page_w, page_h),
    };
    orient_sequence(&strokes, &seq, (0.0, 0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 两条平行横线：模拟「逐行扫描」里相邻两行。
    /// 原顺序都是左→右，画完第一条要跳回最左端，空移等于整条宽度。
    fn two_rows() -> Vec<Polyline> {
        vec![
            Polyline::new(vec![(0.0, 0.0), (10.0, 0.0)]),
            Polyline::new(vec![(0.0, 1.0), (10.0, 1.0)]),
        ]
    }

    /// 行程必须按「G-code 实际走法」丈量：抬笔空移 + 墨迹长度。
    #[test]
    fn path_travel_measures_as_drawn() {
        let paths = two_rows();
        // 0→(0,0) 走 0 + 笔画 10 =10；再空移 (10,0)→(0,1)=√101≈10.05 + 10
        let t = path_travel(&paths, (0.0, 0.0));
        assert!((t - (10.0 + 101.0f64.sqrt() + 10.0)).abs() < 1e-9, "实际 t={t}");
    }

    /// 优化后第二条应当**反向**（从右端起笔），并据此接上第一条的终点。
    #[test]
    fn optimize_reverses_next_row_into_serpentine() {
        let out = optimize_path_order(two_rows());
        assert_eq!(out.len(), 2, "笔画不能丢");
        // 第一条从 (0,0) 起笔
        assert!((out[0].pts[0].0 - 0.0).abs() < 1e-9, "{:?}", out[0].pts);
        // 第二条应从右端 (10,1) 起笔 → 与第一条的终点 (10,0) 只差 1mm
        let head = out[1].pts[0];
        assert!((head.0 - 10.0).abs() < 1e-9, "应从右端起笔：{head:?}");
        assert!((head.1 - 1.0).abs() < 1e-9, "{head:?}");
    }

    /// 核心收益：抬笔**空移**必须明显变短（墨迹长度不可压缩，要单独剥出来比）。
    #[test]
    fn optimized_travel_is_shorter() {
        let before = two_rows();
        let after = optimize_path_order(before.clone());
        let idle = |p: &[Polyline]| path_travel(p, (0.0, 0.0)) - total_length(p);
        let (i0, i1) = (idle(&before), idle(&after));
        assert!(
            i1 < i0 - 1.0,
            "空移应明显下降：before={i0} after={i1}\n{after:?}"
        );
    }

    /// 重排只改顺序与方向，笔画长度必须守恒（不能丢笔也不能改形状）。
    #[test]
    fn reordering_preserves_strokes() {
        let input = vec![
            Polyline::new(vec![(0.0, 0.0), (3.0, 4.0)]),
            Polyline::new(vec![(50.0, 50.0), (60.0, 50.0)]),
            Polyline::new(vec![(1.0, 1.0), (1.0, 2.0)]),
            Polyline::new(vec![(20.0, 0.0), (20.0, 9.0)]),
        ];
        let total = total_length(&input);
        let out = optimize_path_order(input);
        assert_eq!(out.len(), 4, "一条都不能少");
        assert!(
            (total_length(&out) - total).abs() < 1e-6,
            "总墨迹长度必须守恒：{} vs {total}",
            total_length(&out)
        );
    }

    /// 面对「逐行左->右」的大批笔画（图片影线的典型形态），优化应把空移压下来。
    #[test]
    fn optimize_handles_many_row_strokes() {
        let mut rows = Vec::new();
        for y in 0..20 {
            rows.push(Polyline::new(vec![(0.0, y as f64), (30.0, y as f64)]));
        }
        let idle = |p: &[Polyline]| path_travel(p, (0.0, 0.0)) - total_length(p);
        let t0 = idle(&rows);
        let out = optimize_path_order(rows);
        let t1 = idle(&out);
        assert_eq!(out.len(), 20);
        // 逐行跳回首端 ≈ 19×30=570mm 空移；蛇形后 ≈ 19mm —— 差一个数量级以上
        assert!(t1 * 5.0 < t0, "空移应数量级下降：before={t0} after={t1}");
    }

    /// 四种可扩展排序（供「开始绘制」时选择）都必须守恒笔迹，并把空移压下来。
    #[test]
    fn scalable_orders_preserve_and_shorten() {
        // 20 行 × 6 段短横线：模拟图片排线的真实量级与形态。
        let mut paths = Vec::new();
        for y in 0..20 {
            for k in 0..6 {
                let x = k as f64 * 5.0;
                paths.push(Polyline::new(vec![(x, y as f64), (x + 3.0, y as f64)]));
            }
        }
        let total = total_length(&paths);
        let idle = |p: &[Polyline]| path_travel(p, (0.0, 0.0)) - total_length(p);
        let base = idle(&paths);
        for order in [
            PathOrder::Serpentine,
            PathOrder::Nearest,
            PathOrder::Direction,
            PathOrder::Block,
        ] {
            let out = order_paths(paths.clone(), order, 40.0, 30.0);
            assert_eq!(out.len(), paths.len(), "{order:?} 丢了笔迹");
            assert!(
                (total_length(&out) - total).abs() < 1e-6,
                "{order:?} 改了墨迹长度"
            );
            assert!(
                idle(&out) < base * 0.6,
                "{order:?} 空移没降下来：{} vs {base}",
                idle(&out)
            );
        }
    }

    /// Keep 必须原样返回（连笔迹对象一起），避免无意改动生成顺序。
    #[test]
    fn keep_order_is_identity() {
        let input = vec![
            Polyline::new(vec![(5.0, 5.0), (6.0, 5.0)]),
            Polyline::new(vec![(0.0, 0.0), (0.0, 1.0)]),
        ];
        let out = order_paths(input.clone(), PathOrder::Keep, 10.0, 10.0);
        assert_eq!(out[0].pts, input[0].pts);
        assert_eq!(out[1].pts, input[1].pts);
    }
}
