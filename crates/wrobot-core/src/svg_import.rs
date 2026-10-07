//! 纯 Rust 的 SVG 导入解析器。
//!
//! 参考程序「其他绘图」模块支持 **SVG/CAD(DXF) 矢量** 背景导入。这里手写一个
//! 够用的 SVG 读取器（不依赖任何外部 crate），把 `path` 以及基础图元
//! （`rect` / `line` / `circle` / `ellipse` / `polyline` / `polygon`，含 `transform`
//! 与 `<g>` 分组变换）展平成页面毫米坐标的 [`Polyline`]。
//!
//! SVG 坐标本就是 y 向下、原点左上，与我们的页面坐标系一致，因此用
//! [`crate::render::fit_to_page`] 时 `flip_y = false` 即可。

use crate::render::{fit_to_page, Polyline};
use std::collections::HashMap;

/// SVG 解析错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvgError(pub String);

impl std::fmt::Display for SvgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SVG 解析错误：{}", self.0)
    }
}

impl std::error::Error for SvgError {}

type R<T> = Result<T, SvgError>;

/// 2×3 仿射矩阵 `[a, b, c, d, e, f]`，点 `(x,y)` → `(a*x + c*y + e, b*x + d*y + f)`。
#[derive(Clone, Copy)]
struct Mat {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
}
impl Mat {
    fn id() -> Self {
        Mat {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 0.0,
            f: 0.0,
        }
    }
    fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        (self.a * x + self.c * y + self.e, self.b * x + self.d * y + self.f)
    }
    fn compose(&self, o: &Mat) -> Mat {
        // 先应用 o，再应用 self
        Mat {
            a: self.a * o.a + self.c * o.b,
            b: self.b * o.a + self.d * o.b,
            c: self.a * o.c + self.c * o.d,
            d: self.b * o.c + self.d * o.d,
            e: self.a * o.e + self.c * o.f + self.e,
            f: self.b * o.e + self.d * o.f + self.f,
        }
    }
}

fn parse_transform(s: &str) -> Mat {
    let mut m = Mat::id();
    // 形如 translate(10,20) scale(2) rotate(30 0 0) matrix(a,b,c,d,e,f)
    for (name, args) in transform_calls(s) {
        let n: Vec<f64> = args
            .split([',', ' ', '\t', '\n', '\r'])
            .filter_map(|t| t.trim().parse::<f64>().ok())
            .collect();
        let cm = match name.as_str() {
            "translate" if n.len() >= 1 => {
                let ty = if n.len() >= 2 { n[1] } else { 0.0 };
                Mat {
                    a: 1.0,
                    b: 0.0,
                    c: 0.0,
                    d: 1.0,
                    e: n[0],
                    f: ty,
                }
            }
            "scale" if n.len() >= 1 => {
                let sy = if n.len() >= 2 { n[1] } else { n[0] };
                Mat {
                    a: n[0],
                    b: 0.0,
                    c: 0.0,
                    d: sy,
                    e: 0.0,
                    f: 0.0,
                }
            }
            "rotate" if n.len() >= 1 => {
                let (cx, cy) = if n.len() >= 3 { (n[1], n[2]) } else { (0.0, 0.0) };
                let r = n[0].to_radians();
                let (cs, sn) = (r.cos(), r.sin());
                Mat {
                    a: cs,
                    b: sn,
                    c: -sn,
                    d: cs,
                    e: cx - cs * cx + sn * cy,
                    f: cy - sn * cx - cs * cy,
                }
            }
            "matrix" if n.len() >= 6 => Mat {
                a: n[0],
                b: n[1],
                c: n[2],
                d: n[3],
                e: n[4],
                f: n[5],
            },
            _ => Mat::id(),
        };
        m = m.compose(&cm);
    }
    m
}

/// 提取 transform 串里的 `name(args)`，支持嵌套括号。
fn transform_calls(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_alphabetic() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric()) {
                i += 1;
            }
            let name = s[start..i].to_string();
            // 跳过空白直到 '('
            while i < bytes.len() && bytes[i] != b'(' {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'(' {
                let mut depth = 1;
                let args_start = i + 1;
                i += 1;
                while i < bytes.len() && depth > 0 {
                    if bytes[i] == b'(' {
                        depth += 1;
                    } else if bytes[i] == b')' {
                        depth -= 1;
                    }
                    i += 1;
                }
                let args = s[args_start..i.saturating_sub(1)].to_string();
                out.push((name, args));
            }
        } else {
            i += 1;
        }
    }
    out
}

/// 解析 `transform="..."` 之外的简单属性为 (小写名 → 值)。
fn parse_attrs(inner: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let bytes = inner.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // 跳到属性名起点
        while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t' || bytes[i] == b'\n' || bytes[i] == b'\r' || bytes[i] == b'/') {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let ns = i;
        while i < bytes.len() && bytes[i] != b'=' && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let name = inner[ns..i].trim().to_lowercase();
        if name.is_empty() {
            break;
        }
        // 跳过空白与 '='
        while i < bytes.len() && (bytes[i] == b'=' || bytes[i].is_ascii_whitespace()) {
            i += 1;
        }
        let val = if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
            let q = bytes[i];
            i += 1;
            let vs = i;
            while i < bytes.len() && bytes[i] != q {
                i += 1;
            }
            let s = inner[vs..i.min(bytes.len())].to_string();
            i += 1; // 跳过收尾引号
            s
        } else {
            let vs = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'/' && bytes[i] != b'>' {
                i += 1;
            }
            inner[vs..i].to_string()
        };
        if !name.is_empty() {
            map.insert(name, val.trim().to_string());
        }
    }
    map
}

#[derive(Clone, Copy)]
enum Kind {
    Path,
    Rect,
    Line,
    Circle,
    Ellipse,
    Polyline,
    Polygon,
}

/// 遍历 SVG，按文档顺序产出 (图元种类, 属性, 当前累积变换矩阵)。
/// 支持 `<g transform>` 分组：进入时把该变换压栈，离开 `</g>` 弹栈。
fn iter_shapes(svg: &str) -> Vec<(Kind, HashMap<String, String>, Mat)> {
    let mut out = Vec::new();
    let mut stack: Vec<Mat> = vec![Mat::id()];
    let bytes = svg.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        // 找标签结束 '>'
        let start = i + 1;
        let mut j = start;
        let mut quote = 0u8;
        while j < bytes.len() {
            let c = bytes[j];
            if quote != 0 {
                if c == quote {
                    quote = 0;
                }
            } else if c == b'"' || c == b'\'' {
                quote = c;
            } else if c == b'>' {
                break;
            }
            j += 1;
        }
        let inner = svg[start..j.min(bytes.len())].to_string();
        i = j + 1;

        let trimmed = inner.trim();
        if trimmed.starts_with('/') {
            // 闭合标签
            let name = trimmed[1..].trim().split_whitespace().next().unwrap_or("");
            if name.eq_ignore_ascii_case("g") {
                stack.pop();
                if stack.is_empty() {
                    stack.push(Mat::id());
                }
            }
            continue;
        }
        if trimmed.starts_with('?') || trimmed.starts_with('!') {
            continue; // 声明 / 注释，跳过
        }
        // 取标签名（inner 开头到第一个空白或 '/' 或 '>'）
        let tag_end = trimmed
            .find(|c| c == ' ' || c == '\t' || c == '/' || c == '>')
            .unwrap_or(trimmed.len());
        let tag = trimmed[..tag_end].to_lowercase();
        let is_g = tag == "g";
        let attrs = parse_attrs(&trimmed[tag_end..]);
        // 处理 g 的变换压栈
        if is_g {
            let tm = attrs
                .get("transform")
                .map(|s| parse_transform(s))
                .unwrap_or(Mat::id());
            let cur = *stack.last().unwrap_or(&Mat::id());
            stack.push(cur.compose(&tm));
            continue; // g 自身不产生图形
        }
        let kind = match tag.as_str() {
            "path" => Some(Kind::Path),
            "rect" => Some(Kind::Rect),
            "line" => Some(Kind::Line),
            "circle" => Some(Kind::Circle),
            "ellipse" => Some(Kind::Ellipse),
            "polyline" => Some(Kind::Polyline),
            "polygon" => Some(Kind::Polygon),
            _ => None,
        };
        if let Some(k) = kind {
            // 图元自身 transform 再叠加到当前矩阵
            let tm = attrs
                .get("transform")
                .map(|s| parse_transform(s))
                .unwrap_or(Mat::id());
            let cur = *stack.last().unwrap_or(&Mat::id());
            out.push((k, attrs.clone(), cur.compose(&tm)));
        }
    }
    out
}

fn num(attrs: &HashMap<String, String>, key: &str) -> Option<f64> {
    attrs.get(key).and_then(|v| v.trim().parse::<f64>().ok())
}

/// 把 `points="x1,y1 x2,y2 ..."` 解析成点序列。
fn parse_points(s: &str) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    for pair in s.split_whitespace() {
        let mut it = pair.split(',');
        if let (Some(x), Some(y)) = (it.next(), it.next()) {
            if let (Ok(x), Ok(y)) = (x.trim().parse::<f64>(), y.trim().parse::<f64>()) {
                out.push((x, y));
            }
        }
    }
    out
}

/// 把 SVG 字节解析为页面毫米坐标的折线列表（已等比缩放到 `target_h_mm` 高度）。
pub fn import_svg(data: &[u8], target_h_mm: f64) -> R<Vec<Polyline>> {
    let svg = String::from_utf8_lossy(data);
    let shapes = iter_shapes(&svg);
    if svg.trim().is_empty() {
        return Err(SvgError("SVG 内容为空".into()));
    }
    let mut out: Vec<Polyline> = Vec::new();
    for (kind, attrs, m) in &shapes {
        match kind {
            Kind::Path => {
                if let Some(d) = attrs.get("d") {
                    out.extend(parse_path(d, m));
                }
            }
            Kind::Rect => {
                let x = num(attrs, "x").unwrap_or(0.0);
                let y = num(attrs, "y").unwrap_or(0.0);
                let w = num(attrs, "width").unwrap_or(0.0);
                let h = num(attrs, "height").unwrap_or(0.0);
                if w > 0.0 && h > 0.0 {
                    out.push(Polyline::new(vec![
                        m.apply(x, y),
                        m.apply(x + w, y),
                        m.apply(x + w, y + h),
                        m.apply(x, y + h),
                        m.apply(x, y),
                    ]));
                }
            }
            Kind::Line => {
                if let (Some(x1), Some(y1), Some(x2), Some(y2)) = (
                    num(attrs, "x1"),
                    num(attrs, "y1"),
                    num(attrs, "x2"),
                    num(attrs, "y2"),
                ) {
                    out.push(Polyline::new(vec![m.apply(x1, y1), m.apply(x2, y2)]));
                }
            }
            Kind::Circle => {
                let cx = num(attrs, "cx").unwrap_or(0.0);
                let cy = num(attrs, "cy").unwrap_or(0.0);
                let r = num(attrs, "r").unwrap_or(0.0);
                if r > 0.0 {
                    out.push(flatten_ellipse_path(cx, cy, r, r, m));
                }
            }
            Kind::Ellipse => {
                let cx = num(attrs, "cx").unwrap_or(0.0);
                let cy = num(attrs, "cy").unwrap_or(0.0);
                let rx = num(attrs, "rx").unwrap_or(0.0);
                let ry = num(attrs, "ry").unwrap_or(0.0);
                if rx > 0.0 && ry > 0.0 {
                    out.push(flatten_ellipse_path(cx, cy, rx, ry, m));
                }
            }
            Kind::Polyline => {
                if let Some(p) = attrs.get("points") {
                    let pts = parse_points(p);
                    if pts.len() >= 2 {
                        out.push(Polyline::new(pts.into_iter().map(|(x, y)| m.apply(x, y)).collect()));
                    }
                }
            }
            Kind::Polygon => {
                if let Some(p) = attrs.get("points") {
                    let pts = parse_points(p);
                    if pts.len() >= 2 {
                        let mut v: Vec<(f64, f64)> = pts.into_iter().map(|(x, y)| m.apply(x, y)).collect();
                        v.push(v[0]); // 闭合
                        out.push(Polyline::new(v));
                    }
                }
            }
        }
    }
    if out.is_empty() {
        return Err(SvgError(
            "SVG 中未找到可导入的图元（path/rect/line/circle/ellipse/polyline/polygon）".into(),
        ));
    }
    Ok(fit_to_page(&out, target_h_mm, false))
}

fn flatten_ellipse_path(cx: f64, cy: f64, rx: f64, ry: f64, m: &Mat) -> Polyline {
    let n = 64;
    let pts: Vec<(f64, f64)> = (0..=n)
        .map(|k| {
            let a = k as f64 / n as f64 * std::f64::consts::TAU;
            m.apply(cx + rx * a.cos(), cy + ry * a.sin())
        })
        .collect();
    Polyline::new(pts)
}

// ── path 解析 ───────────────────────────────────────────────

/// 解析 path 的 `d` 属性，返回多条折线（M 起新子路径，Z 闭合）。
///
/// 采用「遇到命令字母才停」的取数模型：每个命令只消费其后连续的数值 token，
/// 一旦碰到下一个命令字母就停止，避免把命令误当数字（旧实现在 `M` 后的 `L` 上会读错）。
fn parse_path(d: &str, m: &Mat) -> Vec<Polyline> {
    let toks = path_tokens(d);
    let n = toks.len();
    let mut paths: Vec<Polyline> = Vec::new();
    let mut cur: Vec<(f64, f64)> = Vec::new();
    let (mut cx, mut cy) = (0.0_f64, 0.0_f64);
    let (mut sx, mut sy) = (0.0_f64, 0.0_f64);
    let mut prev_ctrl: Option<(f64, f64)> = None;
    let mut i = 0;
    while i < n {
        if let Tok::Cmd(c) = toks[i] {
            let rel = c.is_ascii_lowercase();
            let cmd = c.to_ascii_uppercase();
            i += 1;
            match cmd {
                'M' => {
                    if !cur.is_empty() {
                        paths.push(Polyline::new(std::mem::take(&mut cur)));
                    }
                    let (x, y) = read_pair(&toks, &mut i, rel, &mut cx, &mut cy);
                    cur.push(m.apply(x, y));
                    sx = x;
                    sy = y;
                    prev_ctrl = None;
                    while is_num(&toks, i) {
                        let (x, y) = read_pair(&toks, &mut i, rel, &mut cx, &mut cy);
                        cur.push(m.apply(x, y));
                    }
                }
                'L' => {
                    while is_num(&toks, i) {
                        let (x, y) = read_pair(&toks, &mut i, rel, &mut cx, &mut cy);
                        cur.push(m.apply(x, y));
                        prev_ctrl = None;
                    }
                }
                'H' => {
                    while is_num(&toks, i) {
                        let x = read_num(&toks, &mut i, rel, cx);
                        cur.push(m.apply(x, cy));
                        prev_ctrl = None;
                    }
                }
                'V' => {
                    while is_num(&toks, i) {
                        let y = read_num(&toks, &mut i, rel, cy);
                        cur.push(m.apply(cx, y));
                        prev_ctrl = None;
                    }
                }
                'C' => {
                    while has_nums(&toks, i, 6) {
                        let (x1, y1) = read_pair(&toks, &mut i, rel, &mut cx, &mut cy);
                        let (x2, y2) = read_pair(&toks, &mut i, rel, &mut cx, &mut cy);
                        let (x, y) = read_pair(&toks, &mut i, rel, &mut cx, &mut cy);
                        let last = cur.last().copied().unwrap_or((sx, sy));
                        cur.extend(flatten_cubic(last.0, last.1, x1, y1, x2, y2, x, y));
                        prev_ctrl = Some((x2, y2));
                    }
                }
                'S' => {
                    while has_nums(&toks, i, 4) {
                        let (x2, y2) = read_pair(&toks, &mut i, rel, &mut cx, &mut cy);
                        let (x, y) = read_pair(&toks, &mut i, rel, &mut cx, &mut cy);
                        let (rx, ry) = prev_ctrl
                            .map(|(px, py)| (2.0 * cx - px, 2.0 * cy - py))
                            .unwrap_or((cx, cy));
                        let last = cur.last().copied().unwrap_or((sx, sy));
                        cur.extend(flatten_cubic(last.0, last.1, rx, ry, x2, y2, x, y));
                        prev_ctrl = Some((x2, y2));
                    }
                }
                'Q' => {
                    while has_nums(&toks, i, 4) {
                        let (x1, y1) = read_pair(&toks, &mut i, rel, &mut cx, &mut cy);
                        let (x, y) = read_pair(&toks, &mut i, rel, &mut cx, &mut cy);
                        let last = cur.last().copied().unwrap_or((sx, sy));
                        cur.extend(flatten_quad(last.0, last.1, x1, y1, x, y));
                        prev_ctrl = Some((x1, y1));
                    }
                }
                'T' => {
                    while has_nums(&toks, i, 2) {
                        let (x, y) = read_pair(&toks, &mut i, rel, &mut cx, &mut cy);
                        let (rx, ry) = prev_ctrl
                            .map(|(px, py)| (2.0 * cx - px, 2.0 * cy - py))
                            .unwrap_or((cx, cy));
                        let last = cur.last().copied().unwrap_or((sx, sy));
                        cur.extend(flatten_quad(last.0, last.1, rx, ry, x, y));
                        prev_ctrl = Some((rx, ry));
                    }
                }
                'A' => {
                    while has_nums(&toks, i, 7) {
                        let rx = read_num(&toks, &mut i, false, 0.0);
                        let ry = read_num(&toks, &mut i, false, 0.0);
                        let xrot = read_num(&toks, &mut i, false, 0.0);
                        let large = read_num(&toks, &mut i, false, 0.0);
                        let sweep = read_num(&toks, &mut i, false, 0.0);
                        let (x, y) = read_pair(&toks, &mut i, rel, &mut cx, &mut cy);
                        let last = cur.last().copied().unwrap_or((sx, sy));
                        match flatten_arc(last.0, last.1, rx, ry, xrot, large, sweep, x, y) {
                            Some(pts) => cur.extend(pts),
                            None => cur.push(m.apply(x, y)),
                        }
                        prev_ctrl = None;
                    }
                }
                'Z' => {
                    if !cur.is_empty() {
                        cur.push(m.apply(sx, sy));
                        paths.push(Polyline::new(std::mem::take(&mut cur)));
                    }
                    prev_ctrl = None;
                }
                _ => {}
            }
        } else {
            i += 1;
        }
    }
    if !cur.is_empty() {
        paths.push(Polyline::new(cur));
    }
    paths
}

#[derive(Clone)]
enum Tok {
    Cmd(char),
    Num(f64),
}

/// 把 path 的 `d` 拆成命令字母与数字 token。
fn path_tokens(d: &str) -> Vec<Tok> {
    let bytes = d.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_alphabetic() {
            out.push(Tok::Cmd(c as char));
            i += 1;
        } else if c == b'-' || c == b'+' || c.is_ascii_digit() || c == b'.' {
            let s = i;
            if c == b'-' || c == b'+' {
                i += 1;
            }
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.' || bytes[i] == b'e' || bytes[i] == b'E' || bytes[i] == b'-' || bytes[i] == b'+') {
                i += 1;
            }
            if let Ok(v) = d[s..i].parse::<f64>() {
                out.push(Tok::Num(v));
            }
        } else {
            i += 1; // 空白、逗号等分隔符
        }
    }
    out
}

fn is_num(toks: &[Tok], i: usize) -> bool {
    matches!(toks.get(i), Some(Tok::Num(_)))
}

fn read_num(toks: &[Tok], i: &mut usize, rel: bool, base: f64) -> f64 {
    let v = if is_num(toks, *i) {
        if let Some(Tok::Num(v)) = toks.get(*i) {
            *v
        } else {
            0.0
        }
    } else {
        0.0
    };
    *i += 1;
    if rel {
        base + v
    } else {
        v
    }
}

fn read_pair(toks: &[Tok], i: &mut usize, rel: bool, cx: &mut f64, cy: &mut f64) -> (f64, f64) {
    let x = read_num(toks, i, rel, *cx);
    let y = read_num(toks, i, rel, *cy);
    *cx = x;
    *cy = y;
    (x, y)
}

/// 从 `i` 起是否有连续 `k` 个数值 token（命令的可消费参数个数）。
fn has_nums(toks: &[Tok], i: usize, k: usize) -> bool {
    (0..k).all(|d| is_num(toks, i + d))
}

fn flatten_cubic(x0: f64, y0: f64, x1: f64, y1: f64, x2: f64, y2: f64, x: f64, y: f64) -> Vec<(f64, f64)> {
    let n = 24;
    (1..=n)
        .map(|k| {
            let t = k as f64 / n as f64;
            let u = 1.0 - t;
            (
                u * u * u * x0 + 3.0 * u * u * t * x1 + 3.0 * u * t * t * x2 + t * t * t * x,
                u * u * u * y0 + 3.0 * u * u * t * y1 + 3.0 * u * t * t * y2 + t * t * t * y,
            )
        })
        .collect()
}

fn flatten_quad(x0: f64, y0: f64, x1: f64, y1: f64, x: f64, y: f64) -> Vec<(f64, f64)> {
    let n = 20;
    (1..=n)
        .map(|k| {
            let t = k as f64 / n as f64;
            let u = 1.0 - t;
            (
                u * u * x0 + 2.0 * u * t * x1 + t * t * x,
                u * u * y0 + 2.0 * u * t * y1 + t * t * y,
            )
        })
        .collect()
}

/// SVG 椭圆弧 → 中心参数，再采样成多段线。失败时返回 None（调用方退化为直线）。
fn flatten_arc(
    x0: f64,
    y0: f64,
    rx: f64,
    ry: f64,
    xrot_deg: f64,
    large: f64,
    sweep: f64,
    x: f64,
    y: f64,
) -> Option<Vec<(f64, f64)>> {
    let rx = rx.abs();
    let ry = ry.abs();
    if rx < 1e-6 || ry < 1e-6 {
        return None;
    }
    let phi = xrot_deg.to_radians();
    let (sinp, cosp) = (phi.sin(), phi.cos());
    let dx = (x0 - x) / 2.0;
    let dy = (y0 - y) / 2.0;
    let x1p = cosp * dx + sinp * dy;
    let y1p = -sinp * dx + cosp * dy;
    let rx2 = rx * rx;
    let ry2 = ry * ry;
    let x1p2 = x1p * x1p;
    let y1p2 = y1p * y1p;
    let num = rx2 * ry2 - rx2 * y1p2 - ry2 * x1p2;
    let den = rx2 * y1p2 + ry2 * x1p2;
    let coef = if den < 1e-9 {
        0.0
    } else {
        (num / den).max(0.0).sqrt()
    };
    // large 与 sweep 同号时取负（详见 SVG 椭圆弧中心解算）
    let sign = if (large != 0.0) == (sweep != 0.0) { -1.0 } else { 1.0 };
    let cxp = sign * coef * (rx * y1p / ry);
    let cyp = sign * coef * (-ry * x1p / rx);
    let cx = cosp * cxp - sinp * cyp + (x0 + x) / 2.0;
    let cy = sinp * cxp + cosp * cyp + (y0 + y) / 2.0;

    let ang = |ux: f64, uy: f64, vx: f64, vy: f64| {
        let dot = ux * vx + uy * vy;
        let len = (ux * ux + uy * uy).sqrt() * (vx * vx + vy * vy).sqrt();
        let a = if len < 1e-9 { 0.0 } else { (dot / len).clamp(-1.0, 1.0).acos() };
        if ux * vy - uy * vx < 0.0 {
            -a
        } else {
            a
        }
    };
    let theta1 = ang(1.0, 0.0, (x1p - cxp) / rx, (y1p - cyp) / ry);
    let mut delta = ang(
        (x1p - cxp) / rx,
        (y1p - cyp) / ry,
        (-x1p - cxp) / rx,
        (-y1p - cyp) / ry,
    );
    if sweep == 0.0 && delta > 0.0 {
        delta -= 2.0 * std::f64::consts::PI;
    } else if sweep == 1.0 && delta < 0.0 {
        delta += 2.0 * std::f64::consts::PI;
    }
    let n = (delta.abs() / (std::f64::consts::PI / 32.0)).ceil().max(2.0) as usize;
    let mut pts = Vec::with_capacity(n + 1);
    for k in 0..=n {
        let t = theta1 + delta * k as f64 / n as f64;
        let ex = rx * t.cos();
        let ey = ry * t.sin();
        pts.push((
            cosp * ex - sinp * ey + cx,
            sinp * ex + cosp * ey + cy,
        ));
    }
    Some(pts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rect_and_line() {
        let svg = r#"<svg><rect x="10" y="20" width="100" height="50"/><line x1="0" y1="0" x2="30" y2="40"/></svg>"#;
        let paths = import_svg(svg.as_bytes(), 100.0).unwrap();
        assert_eq!(paths.len(), 2, "rect + line");
        // rect 应是 5 点闭合
        assert_eq!(paths[0].pts.len(), 5);
        // 归一化按「高度」缩放并锚定 (0,0)：y 必在 [0,100]，x 可能因宽>高而>100
        let (x0, y0, _x1, y1) = crate::render::bounds_all(&paths);
        assert!(y1 <= 100.0 + 1e-6 && y0 >= -1e-6, "y 应落在 [0,100]，实际 {y0}..{y1}");
        assert!(x0 >= -1e-6, "x 不应为负，实际 {x0}");
    }

    #[test]
    fn path_mlz_closed() {
        // M0,0 L100,0 L100,100 Z → 闭合三角形（4 点）
        let svg = r#"<svg><path d="M0,0 L100,0 L100,100 Z"/></svg>"#;
        let paths = import_svg(svg.as_bytes(), 100.0).unwrap();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].pts.len(), 4, "闭合路径应回到起点");
        let first = paths[0].pts[0];
        let last = paths[0].pts[3];
        assert!((first.0 - last.0).abs() < 1e-6 && (first.1 - last.1).abs() < 1e-6);
    }

    #[test]
    fn path_relative_and_curve() {
        // 相对命令 + 三次贝塞尔
        let svg = r#"<svg><path d="m10,10 l20,0 c10,0 10,10 0,10"/></svg>"#;
        let paths = import_svg(svg.as_bytes(), 100.0).unwrap();
        assert_eq!(paths.len(), 1);
        assert!(paths[0].pts.len() >= 3, "曲线应被采样为多段");
    }

    #[test]
    fn group_transform_applies() {
        // 直接验证 transform 解析与矩阵组合（translate∘scale 正确）
        let m = parse_transform("translate(10,20) scale(2)");
        let (x, y) = m.apply(1.0, 1.0);
        assert!((x - 12.0).abs() < 1e-9 && (y - 22.0).abs() < 1e-9, "translate∘scale 应得 (12,22)，实际 ({x},{y})");
        let m2 = parse_transform("translate(100,100)");
        assert!((m2.e - 100.0).abs() < 1e-9 && (m2.f - 100.0).abs() < 1e-9);
        // 真实 SVG：带变换的组其几何会被变换后再归一化
        let svg = r#"<svg><g transform="scale(2)"><rect x="0" y="0" width="10" height="10"/></g></svg>"#;
        let paths = import_svg(svg.as_bytes(), 100.0).unwrap();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].pts.len(), 5, "缩放后的矩形仍是 5 点闭合");
    }

    #[test]
    fn circle_is_rounded() {
        let svg = r#"<svg><circle cx="50" cy="50" r="40"/></svg>"#;
        let paths = import_svg(svg.as_bytes(), 100.0).unwrap();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].pts.len(), 65, "圆应被采样为 64 段");
    }

    #[test]
    fn rejects_non_svg() {
        assert!(import_svg(b"not svg at all", 100.0).is_err());
    }
}
