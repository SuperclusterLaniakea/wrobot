//! 纯 Rust 的 DXF（ASCII R12+）导入解析器。
//!
//! 参考程序「其他绘图」模块支持 **SVG/CAD(DXF) 矢量** 背景导入（原版用 kabeja）。
//! 这里不依赖任何外部 crate，手写一个够用的 DXF 读取器，把常见图元
//! （LINE / LWPOLYLINE / POLYLINE+VERTEX / CIRCLE / ARC / ELLIPSE / SPLINE /
//! INSERT+BLOCKS）展平成页面毫米坐标的 [`Polyline`]，供写字机描边。
//!
//! DXF 坐标约定：Y 向上、原点在模型空间任意处。我们用 [`crate::render::fit_to_page`]
//! 把它翻转并等比缩放到 `target_h_mm` 高度，左上角锚定 (0,0)，与图片描边、
//! 手绘笔迹落在同一页面坐标系（y 向下、原点左上）。

use crate::render::{fit_to_page, Polyline};

/// DXF 解析错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DxfError(pub String);

impl std::fmt::Display for DxfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DXF 解析错误：{}", self.0)
    }
}

impl std::error::Error for DxfError {}

type R<T> = Result<T, DxfError>;

/// 一个 2D 仿射变换：先平移 `-base`、再缩放、再旋转、最后平移 `tx/ty`。
/// 足以表达 DXF 的 INSERT（含 BLOCK 基点）与嵌套块引用。
#[derive(Clone, Copy)]
struct Xf {
    tx: f64,
    ty: f64,
    rot: f64,
    sx: f64,
    sy: f64,
}

impl Default for Xf {
    /// 单位矩阵（注意：不能依赖 `#[derive(Default)]`，否则 sx/sy 会变成 0）。
    fn default() -> Self {
        Xf {
            tx: 0.0,
            ty: 0.0,
            rot: 0.0,
            sx: 1.0,
            sy: 1.0,
        }
    }
}

impl Xf {
    fn apply(&self, p: (f64, f64)) -> (f64, f64) {
        let x = p.0 * self.sx;
        let y = p.1 * self.sy;
        let (c, s) = (self.rot.cos(), self.rot.sin());
        let (rx, ry) = (x * c - y * s, x * s + y * c);
        (rx + self.tx, ry + self.ty)
    }
    /// 返回一个先应用 `child` 再应用 `self` 的复合变换。
    fn compose(&self, child: &Xf) -> Xf {
        let t = self.apply((child.tx, child.ty));
        Xf {
            tx: t.0,
            ty: t.1,
            rot: self.rot + child.rot,
            sx: self.sx * child.sx,
            sy: self.sy * child.sy,
        }
    }
}

/// 把若干图元的原始组码序列收集成一个「块」（不含 BLOCK/ENDBLK 标记）。
struct Block {
    base: (f64, f64),
    ents: Vec<(i64, String)>,
}

/// 把一段组码解析为折线，追加到 `out`。`xf` 为当前生效的变换（用于 INSERT）。
fn emit_entities(
    g: &[(i64, String)],
    blocks: &std::collections::HashMap<String, Block>,
    xf: &Xf,
    out: &mut Vec<Polyline>,
) {
    let mut i = 0;
    // 正在拼装的 POLYLINE（旧式：POLYLINE + 若干 VERTEX + SEQEND）
    let mut poly: Option<(Vec<(f64, f64)>, bool)> = None;
    while i < g.len() {
        if g[i].0 != 0 {
            i += 1;
            continue;
        }
        let typ = g[i].1.trim().to_uppercase();
        i += 1;
        match typ.as_str() {
            "LINE" => {
                if let (Some(a), Some(b)) = (read_point(g, i), read_point2(g, i)) {
                    out.push(Polyline::new(vec![xf.apply(a), xf.apply(b)]));
                }
            }
            "LWPOLYLINE" => {
                let (pts, closed) = collect_vertices(g, i, true);
                if !pts.is_empty() {
                    let mut pl = pts.into_iter().map(|p| xf.apply(p)).collect::<Vec<_>>();
                    if closed {
                        pl.push(pl[0]);
                    }
                    out.push(Polyline::new(pl));
                }
            }
            "POLYLINE" => {
                let closed = read_flag(g, i) & 1 != 0;
                poly = Some((Vec::new(), closed));
            }
            "VERTEX" => {
                if let Some(p) = read_point(g, i) {
                    if let Some((v, _)) = poly.as_mut() {
                        v.push(p);
                    }
                }
            }
            "SEQEND" => {
                if let Some((v, closed)) = poly.take() {
                    if v.len() >= 2 {
                        let mut pl = v.into_iter().map(|p| xf.apply(p)).collect::<Vec<_>>();
                        if closed {
                            pl.push(pl[0]);
                        }
                        out.push(Polyline::new(pl));
                    }
                }
            }
            "CIRCLE" => {
                if let (Some(c), Some(r)) = (read_point(g, i), read_f(g, i, 40)) {
                    out.push(flatten_circle(c, r, xf));
                }
            }
            "ARC" => {
                if let (Some(c), Some(r), Some(a0), Some(a1)) = (
                    read_point(g, i),
                    read_f(g, i, 40),
                    read_f(g, i, 50),
                    read_f(g, i, 51),
                ) {
                    out.push(flatten_arc(c, r, a0, a1, xf));
                }
            }
            "ELLIPSE" => {
                if let (Some(c), Some(axis), Some(ratio), Some(a0), Some(a1)) = (
                    read_point(g, i),
                    read_point(g, i + 1), // 11/21 = 长轴向量
                    read_f(g, i, 40),
                    read_f(g, i, 41),
                    read_f(g, i, 42),
                ) {
                    out.push(flatten_ellipse(c, axis, ratio, a0, a1, xf));
                }
            }
            "SPLINE" => {
                let ctrl = collect_points_code(g, i, 10);
                let fit = collect_points_code(g, i, 11);
                let pts = if !fit.is_empty() { fit } else { ctrl };
                if pts.len() >= 2 {
                    out.push(flatten_catmull(&pts, xf));
                }
            }
            "INSERT" => {
                let name = read_str(g, i, 2).to_uppercase();
                let ins = read_point(g, i).unwrap_or((0.0, 0.0));
                let sx = read_f(g, i, 41).unwrap_or(1.0);
                let sy = read_f(g, i, 42).unwrap_or(1.0);
                let rot = read_f(g, i, 50).unwrap_or(0.0).to_radians();
                if let Some(blk) = blocks.get(&name) {
                    // 世界坐标 = 插入点 + R*S*(局部 - 块基点)
                    let base_xf = Xf {
                        tx: -blk.base.0,
                        ty: -blk.base.1,
                        rot: 0.0,
                        sx: 1.0,
                        sy: 1.0,
                    };
                    let ins_xf = Xf {
                        tx: ins.0,
                        ty: ins.1,
                        rot,
                        sx,
                        sy,
                    };
                    let child = ins_xf.compose(&base_xf);
                    emit_entities(&blk.ents, blocks, &xf.compose(&child), out);
                }
            }
            // TEXT / MTEXT / DIMENSION / HATCH / POINT 等：跳过（无法以单线笔迹表达）
            _ => {}
        }
    }
    // 防御：若 POLYLINE 没遇到 SEQEND 直接结束
    if let Some((v, closed)) = poly.take() {
        if v.len() >= 2 {
            let mut pl = v.into_iter().map(|p| xf.apply(p)).collect::<Vec<_>>();
            if closed {
                pl.push(pl[0]);
            }
            out.push(Polyline::new(pl));
        }
    }
}

fn flatten_circle(c: (f64, f64), r: f64, xf: &Xf) -> Polyline {
    let n = 64;
    let pts: Vec<(f64, f64)> = (0..=n)
        .map(|k| {
            let a = k as f64 / n as f64 * std::f64::consts::TAU;
            xf.apply((c.0 + r * a.cos(), c.1 + r * a.sin()))
        })
        .collect();
    Polyline::new(pts)
}

fn flatten_arc(c: (f64, f64), r: f64, a0: f64, a1: f64, xf: &Xf) -> Polyline {
    let s = a0.to_radians();
    let mut e = a1.to_radians();
    if e < s {
        e += std::f64::consts::TAU;
    }
    let span = e - s;
    let n = (span / std::f64::consts::TAU * 64.0).ceil().max(2.0) as usize;
    let pts: Vec<(f64, f64)> = (0..=n)
        .map(|k| {
            let a = s + span * k as f64 / n as f64;
            xf.apply((c.0 + r * a.cos(), c.1 + r * a.sin()))
        })
        .collect();
    Polyline::new(pts)
}

fn flatten_ellipse(
    c: (f64, f64),
    axis: (f64, f64),
    ratio: f64,
    a0: f64,
    a1: f64,
    xf: &Xf,
) -> Polyline {
    let major = (axis.0.hypot(axis.1)).max(1e-9);
    let u = (axis.0 / major, axis.1 / major);
    let v = (-u.1, u.0);
    let minor = major * ratio.max(1e-6);
    let (s, mut e) = (a0, a1);
    if e < s {
        e += std::f64::consts::TAU;
    }
    let span = e - s;
    let n = (span / std::f64::consts::TAU * 64.0).ceil().max(2.0) as usize;
    let pts: Vec<(f64, f64)> = (0..=n)
        .map(|k| {
            let t = s + span * k as f64 / n as f64;
            let (ct, st) = (t.cos(), t.sin());
            let x = c.0 + major * ct * u.0 + minor * st * v.0;
            let y = c.1 + major * ct * u.1 + minor * st * v.1;
            xf.apply((x, y))
        })
        .collect();
    Polyline::new(pts)
}

/// 用 Catmull-Rom 把控制点序列采样成平滑折线（样条近似）。
fn flatten_catmull(ctrl: &[(f64, f64)], xf: &Xf) -> Polyline {
    if ctrl.len() < 3 {
        return Polyline::new(ctrl.iter().map(|&p| xf.apply(p)).collect());
    }
    let n = ctrl.len();
    let seg = 16usize;
    let mut pts = Vec::with_capacity(n * seg);
    let get = |i: isize| ctrl[clamp_idx(i, n)];
    for i in 0..n - 1 {
        let p0 = get(i as isize - 1);
        let p1 = get(i as isize);
        let p2 = get(i as isize + 1);
        let p3 = get(i as isize + 2);
        for k in 0..seg {
            let t = k as f64 / seg as f64;
            let t2 = t * t;
            let t3 = t2 * t;
            let x = 0.5 * ((2.0 * p1.0)
                + (-p0.0 + p2.0) * t
                + (2.0 * p0.0 - 5.0 * p1.0 + 4.0 * p2.0 - p3.0) * t2
                + (-p0.0 + 3.0 * p1.0 - 3.0 * p2.0 + p3.0) * t3);
            let y = 0.5 * ((2.0 * p1.1)
                + (-p0.1 + p2.1) * t
                + (2.0 * p0.1 - 5.0 * p1.1 + 4.0 * p2.1 - p3.1) * t2
                + (-p0.1 + 3.0 * p1.1 - 3.0 * p2.1 + p3.1) * t3);
            pts.push(xf.apply((x, y)));
        }
    }
    pts.push(xf.apply(*ctrl.last().unwrap()));
    Polyline::new(pts)
}

fn clamp_idx(i: isize, n: usize) -> usize {
    if i < 0 {
        0
    } else if i >= n as isize {
        n - 1
    } else {
        i as usize
    }
}

// ── 组码读取小工具 ───────────────────────────────────────────────

/// 从下标 `i` 起向前扫，找指定组码对应的 f64 值（遇下一个 0 组停止）。
fn read_f(g: &[(i64, String)], i: usize, code: i64) -> Option<f64> {
    let mut j = i;
    while j < g.len() {
        if g[j].0 == 0 {
            return None;
        }
        if g[j].0 == code {
            return g[j].1.trim().parse::<f64>().ok();
        }
        j += 1;
    }
    None
}

fn read_str(g: &[(i64, String)], i: usize, code: i64) -> String {
    let mut j = i;
    while j < g.len() {
        if g[j].0 == 0 {
            return String::new();
        }
        if g[j].0 == code {
            return g[j].1.trim().to_string();
        }
        j += 1;
    }
    String::new()
}

/// 读 10/20 组（第一点）。
fn read_point(g: &[(i64, String)], i: usize) -> Option<(f64, f64)> {
    let x = read_f(g, i, 10)?;
    let y = read_f(g, i, 20)?;
    Some((x, y))
}

/// 读 11/21 组（第二点）。
fn read_point2(g: &[(i64, String)], i: usize) -> Option<(f64, f64)> {
    let x = read_f(g, i, 11)?;
    let y = read_f(g, i, 21)?;
    Some((x, y))
}

/// 读 POLYLINE 的 70 闭合标志。
fn read_flag(g: &[(i64, String)], i: usize) -> i64 {
    read_f(g, i, 70).map(|v| v as i64).unwrap_or(0)
}

/// 收集 LWPOLYLINE 的 10/20 顶点对；`lw`=true 时只取连续 10/20 对，忽略闭合标志以外的组。
fn collect_vertices(g: &[(i64, String)], i: usize, _lw: bool) -> (Vec<(f64, f64)>, bool) {
    let mut pts = Vec::new();
    let mut closed = false;
    let mut j = i;
    while j < g.len() {
        let code = g[j].0;
        if code == 0 {
            break;
        }
        if code == 70 {
            closed = g[j].1.trim().parse::<i64>().unwrap_or(0) & 1 != 0;
        } else if code == 10 {
            if let (Ok(x), Some(y)) = (g[j].1.trim().parse::<f64>(), read_f(g, j + 1, 20)) {
                pts.push((x, y));
            }
        }
        j += 1;
    }
    (pts, closed)
}

/// 收集某组码（10 或 11）起、紧跟 20/21 的点序列（用于 SPLINE 控制/拟合点）。
fn collect_points_code(g: &[(i64, String)], i: usize, code: i64) -> Vec<(f64, f64)> {
    let mut pts = Vec::new();
    let mut j = i;
    while j < g.len() {
        let c = g[j].0;
        if c == 0 {
            break;
        }
        if c == code {
            if let (Ok(x), Some(y)) = (g[j].1.trim().parse::<f64>(), read_f(g, j + 1, code + 10)) {
                pts.push((x, y));
            }
        }
        j += 1;
    }
    pts
}

/// 把 DXF 字节解析为页面毫米坐标的折线列表（已翻转 Y、等比缩放到 `target_h_mm` 高度）。
pub fn import_dxf(data: &[u8], target_h_mm: f64) -> R<Vec<Polyline>> {
    let text = String::from_utf8_lossy(data);
    let groups = parse_groups(&text);
    if groups.len() < 2 {
        return Err(DxfError("文件为空或不是 DXF".into()));
    }
    if !groups
        .iter()
        .any(|(c, v)| *c == 0 && v.trim().eq_ignore_ascii_case("SECTION"))
    {
        return Err(DxfError("未找到 SECTION，可能不是 DXF 文件".into()));
    }

    let mut blocks: std::collections::HashMap<String, Block> = std::collections::HashMap::new();
    let mut ents: Vec<(i64, String)> = Vec::new();

    let mut i = 0;
    while i < groups.len() {
        if groups[i].0 == 0 && groups[i].1.trim().eq_ignore_ascii_case("SECTION") {
            i += 1;
            let sec = groups
                .get(i)
                .map(|(_, v)| v.trim().to_uppercase())
                .unwrap_or_default();
            i += 1;
            match sec.as_str() {
                "BLOCKS" => parse_blocks(&groups, &mut i, &mut blocks),
                "ENTITIES" => {
                    // 收集 ENTITIES 段内所有组（到 ENDSEC 为止）
                    let start = i;
                    while i < groups.len()
                        && !(groups[i].0 == 0 && groups[i].1.trim().eq_ignore_ascii_case("ENDSEC"))
                    {
                        ents.push(groups[i].clone());
                        i += 1;
                    }
                    let _ = start;
                }
                _ => {
                    // 跳过其它段直到 ENDSEC
                    while i < groups.len()
                        && !(groups[i].0 == 0 && groups[i].1.trim().eq_ignore_ascii_case("ENDSEC"))
                    {
                        i += 1;
                    }
                }
            }
        } else {
            i += 1;
        }
    }

    let mut out = Vec::new();
    let id = Xf::default();
    emit_entities(&ents, &blocks, &id, &mut out);
    if out.is_empty() {
        return Err(DxfError("DXF 中未找到可导入的图元（LINE/LWPOLYLINE/CIRCLE/ARC/...）".into()));
    }
    Ok(fit_to_page(&out, target_h_mm, true))
}

/// 把 DXF 文本切成 (组码, 值) 序列。
///
/// DXF 是「两行一组」：第一行整数组码，第二行值。注释（组码 999）整组跳过。
/// 同时容错：遇到无法解析组码的行直接跳过，避免单点脏数据拖垮整个文件。
fn parse_groups(text: &str) -> Vec<(i64, String)> {
    let lines: Vec<&str> = text.lines().map(|l| l.trim()).collect();
    let mut groups = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let code = lines[i].parse::<i64>();
        let Ok(code) = code else {
            i += 1;
            continue;
        };
        if code == 999 {
            // 注释组：跳过注释值那一行
            i += 2;
            continue;
        }
        let val = lines.get(i + 1).unwrap_or(&"").to_string();
        groups.push((code, val));
        i += 2;
    }
    groups
}

/// 解析 BLOCKS 段：把每个 BLOCK…ENDBLK 之间的图元收集成一个块。
fn parse_blocks(
    g: &[(i64, String)],
    i: &mut usize,
    blocks: &mut std::collections::HashMap<String, Block>,
) {
    let mut cur_name = String::new();
    let mut cur_base = (0.0_f64, 0.0_f64);
    let mut cur_ents: Vec<(i64, String)> = Vec::new();
    let mut in_block = false;

    while *i < g.len() {
        let (code, val) = &g[*i];
        let t = val.trim().to_uppercase();
        if *code == 0 {
            if t == "BLOCK" {
                in_block = true;
                cur_ents = Vec::new();
                cur_name.clear();
                cur_base = (0.0, 0.0);
            } else if t == "ENDBLK" {
                if !cur_name.is_empty() {
                    blocks.insert(
                        cur_name.clone(),
                        Block {
                            base: cur_base,
                            ents: std::mem::take(&mut cur_ents),
                        },
                    );
                }
                cur_name.clear();
                in_block = false;
            } else if t == "ENDSEC" {
                return;
            } else if in_block {
                // 块内图元的起始标记（LINE/LWPOLYLINE/...），连同后续数值组一起收集
                cur_ents.push(g[*i].clone());
            }
        } else if in_block {
            // 块内数值组（图元坐标 + BLOCK 头部的 2/10/20，无害）全部收集
            cur_ents.push(g[*i].clone());
            if *code == 2 {
                cur_name = val.trim().to_uppercase();
            } else if *code == 10 {
                cur_base.0 = val.trim().parse::<f64>().unwrap_or(0.0);
            } else if *code == 20 {
                cur_base.1 = val.trim().parse::<f64>().unwrap_or(0.0);
            }
        }
        *i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个最小 DXF（R12 文本），含：LINE、CIRCLE、闭合 LWPOLYLINE。
    fn sample_dxf() -> String {
        let mut s = String::new();
        s.push_str("0\nSECTION\n2\nENTITIES\n");
        // 一条线 (0,0)->(100,0)
        s.push_str("0\nLINE\n10\n0.0\n20\n0.0\n11\n100.0\n21\n0.0\n");
        // 圆 中心(50,50) 半径 25
        s.push_str("0\nCIRCLE\n10\n50.0\n20\n50.0\n40\n25.0\n");
        // 闭合 LWPOLYLINE：正方形 (0,0)(0,100)(100,100)(100,0)
        s.push_str("0\nLWPOLYLINE\n70\n1\n10\n0.0\n20\n0.0\n10\n0.0\n20\n100.0\n10\n100.0\n20\n100.0\n10\n100.0\n20\n0.0\n");
        s.push_str("0\nENDSEC\n0\nEOF\n");
        s
    }

    #[test]
    fn parses_basic_entities() {
        let dxf = sample_dxf();
        let paths = import_dxf(dxf.as_bytes(), 100.0).expect("应能解析");
        // LINE(1) + CIRCLE(1) + LWPOLYLINE(1) = 3 条
        assert_eq!(paths.len(), 3, "应解析出 3 条闭合/开放折线");
        // 所有点都应落在 [0,100]×[0,100]（高度归一化到 100、宽度等比）
        for p in &paths {
            for (x, y) in &p.pts {
                assert!(*x >= -1e-6 && *x <= 101.0, "x 越界: {x}");
                assert!(*y >= -1e-6 && *y <= 101.0, "y 越界: {y}");
            }
        }
    }

    #[test]
    fn y_is_flipped_top_aligned() {
        // 一条从 (0,0) 到 (0,100) 的竖线：DXF 中 y 向上，翻转后顶部(y大)应映射到页面 y≈0
        let mut s = String::new();
        s.push_str("0\nSECTION\n2\nENTITIES\n");
        s.push_str("0\nLINE\n10\n0\n20\n0\n11\n0\n21\n100\n");
        s.push_str("0\nENDSEC\n0\nEOF\n");
        let paths = import_dxf(s.as_bytes(), 50.0).unwrap();
        let line = &paths[0];
        // 翻转后，原 y=100（顶部）应落在页面 y≈0，原 y=0 落在 y≈50
        let ys: Vec<f64> = line.pts.iter().map(|p| p.1).collect();
        assert!(
            ys.iter().all(|&y| y >= -1e-6 && y <= 50.0 + 1e-6),
            "翻转后 y 不应超过高度 50，实际 pts={:?} ys={:?}",
            line.pts,
            ys
        );
        assert!(
            (ys[0] - 50.0).abs() < 1e-6 || (ys[1] - 50.0).abs() < 1e-6,
            "底端应映射到 y=50，实际 pts={:?}",
            line.pts
        );
    }

    #[test]
    fn closed_lwpolyline_has_closing_segment() {
        let dxf = sample_dxf();
        let paths = import_dxf(dxf.as_bytes(), 100.0).unwrap();
        let sq = paths
            .iter()
            .find(|p| p.pts.len() == 5)
            .expect("闭合正方形应为 5 点（含回环）");
        let first = sq.pts[0];
        let last = sq.pts[4];
        assert!(
            (first.0 - last.0).abs() < 1e-6 && (first.1 - last.1).abs() < 1e-6,
            "闭合折线首尾应重合"
        );
    }

    #[test]
    fn block_insert_is_transformed() {
        // 块内一个以原点为中心的 10x10 方框，INSERT 到 (200,200)
        let mut s = String::new();
        s.push_str("0\nSECTION\n2\nBLOCKS\n");
        s.push_str("0\nBLOCK\n2\nBOX\n10\n0\n20\n0\n");
        s.push_str("0\nLWPOLYLINE\n70\n1\n10\n-5\n20\n-5\n10\n5\n20\n-5\n10\n5\n20\n5\n10\n-5\n20\n5\n");
        s.push_str("0\nENDBLK\n");
        s.push_str("0\nENDSEC\n");
        s.push_str("0\nSECTION\n2\nENTITIES\n");
        s.push_str("0\nINSERT\n2\nBOX\n10\n200\n20\n200\n");
        s.push_str("0\nENDSEC\n0\nEOF\n");
        let paths = import_dxf(s.as_bytes(), 100.0).unwrap();
        // INSERT 把方块移到 (200,200)，归一化后其尺寸与形状必须保持：10×10 正方形 → 5 点闭合
        assert_eq!(paths.len(), 1);
        let sq = &paths[0];
        assert_eq!(sq.pts.len(), 5, "INSERT 后的方块仍应闭合为 5 点");
        // 归一化按高度缩放，宽高相等 → 包围盒应为正方形
        let (x0, y0, x1, y1) = crate::render::bounds_all(&paths);
        let w = x1 - x0;
        let h = y1 - y0;
        assert!((w - h).abs() < 1e-6, "10×10 方块缩放后应为正方形，实际 {w}×{h}");
    }

    #[test]
    fn rejects_non_dxf() {
        assert!(import_dxf(b"hello world", 100.0).is_err());
        assert!(import_dxf(b"0\nSECTION\n2\nHEADER\n0\nENDSEC\n0\nEOF\n", 100.0).is_err());
    }
}
