//! 单线（笔画）矢量字体抽象。
//!
//! 写字机器人不使用填充字形，而是每个字由若干**笔画折线**组成，
//! 折线即笔尖行走路径（落笔→移动→抬笔）。

pub mod builtin;
pub mod gfont;
pub mod inflate;

pub use builtin::BuiltinFont;
pub use gfont::{GfontFile, GfontRaw};
pub use gfont::ink_metrics_for_glyphs;

/// 字形坐标点（字体单位，y 向下）。
pub type Point = (f32, f32);

/// 一条笔画：连续的折线点序列。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stroke {
    pub points: Vec<Point>,
}

impl Stroke {
    pub fn new(points: Vec<Point>) -> Self {
        Stroke { points }
    }
    pub fn len(&self) -> usize {
        self.points.len()
    }
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }
}

/// 单个字形。
#[derive(Debug, Clone, Default)]
pub struct Glyph {
    /// 笔画集合
    pub strokes: Vec<Stroke>,
    /// 字符前进宽度（字体单位）
    pub advance: f32,
}

impl Glyph {
    pub fn new(strokes: Vec<Stroke>, advance: f32) -> Self {
        Glyph { strokes, advance }
    }
    /// 是否为空白（无笔画）
    pub fn is_blank(&self) -> bool {
        self.strokes.is_empty()
    }
    /// 笔画总点数
    pub fn point_count(&self) -> usize {
        self.strokes.iter().map(|s| s.len()).sum()
    }
}

/// 字体「典型墨迹框」（字体单位，**y 轴向下**）。
///
/// 单线手写字库的字形坐标原点各不相同：
/// - `.gfont`：原点在**基线**上，y 向下，字身绝大部分位于 y<0（实测 y ∈ [-0.8em, +0.3em]）；
/// - 内置兜底字体：原点在 em 框**左上角**，y 向下，字身位于 y ∈ [0.08em, 0.88em]。
///
/// 若直接按「原点对齐」渲染，字会整体偏上/偏下、且超出字框——这正是
/// 「格子里文字中心和格子中心对不齐」的根因。
/// 因此每个字库在加载时统计一个**典型墨迹框**（取各字形墨迹框四边的中位数，
/// 抗个别超框字形干扰），渲染时按它把墨迹中心对齐到字框中心。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct InkMetrics {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl InkMetrics {
    pub fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        InkMetrics { x0, y0, x1, y1 }
    }
    /// 墨迹框中心（字体单位）。
    pub fn center(&self) -> (f32, f32) {
        ((self.x0 + self.x1) * 0.5, (self.y0 + self.y1) * 0.5)
    }
    /// 墨迹框宽 / 高（字体单位）。
    pub fn width(&self) -> f32 {
        (self.x1 - self.x0).max(0.0)
    }
    pub fn height(&self) -> f32 {
        (self.y1 - self.y0).max(0.0)
    }
    /// 是否有效（至少有一个字形参与统计）。
    pub fn valid(&self) -> bool {
        self.x1 > self.x0 && self.y1 > self.y0
    }
}

/// 由一批字形统计「典型墨迹框」：每条边取**中位数**，比 min/max 稳健。
///
/// 只统计有笔画的字形；空白字形（空格等）跳过。
pub fn ink_metrics<'a, I: IntoIterator<Item = &'a Glyph>>(glyphs: I) -> InkMetrics {
    let mut x0s: Vec<f32> = Vec::new();
    let mut y0s: Vec<f32> = Vec::new();
    let mut x1s: Vec<f32> = Vec::new();
    let mut y1s: Vec<f32> = Vec::new();
    for g in glyphs {
        let mut a = f32::MAX;
        let mut b = f32::MAX;
        let mut c = f32::MIN;
        let mut d = f32::MIN;
        let mut any = false;
        for s in &g.strokes {
            for &(x, y) in &s.points {
                any = true;
                a = a.min(x);
                b = b.min(y);
                c = c.max(x);
                d = d.max(y);
            }
        }
        if any {
            x0s.push(a);
            y0s.push(b);
            x1s.push(c);
            y1s.push(d);
        }
    }
    if x0s.is_empty() {
        return InkMetrics::default();
    }
    let med = |v: &mut Vec<f32>| -> f32 {
        v.sort_by(|p, q| p.partial_cmp(q).unwrap_or(std::cmp::Ordering::Equal));
        v[v.len() / 2]
    };
    InkMetrics::new(med(&mut x0s), med(&mut y0s), med(&mut x1s), med(&mut y1s))
}

/// 笔画字体接口。
pub trait StrokeFont: Send + Sync {
    /// 字体名
    fn name(&self) -> &str;
    /// em 方框尺寸（字体单位）
    fn em(&self) -> f32;
    /// 取字形；缺失返回 None
    fn glyph(&self, ch: char) -> Option<Glyph>;
    /// 是否包含该字符
    fn has(&self, ch: char) -> bool {
        self.glyph(ch).is_some()
    }
    /// 本字库的「典型墨迹框」（字体单位，y 向下），用于把字身居中到字框。
    ///
    /// 默认实现返回退化值（空框），此时渲染层按「原点对齐」处理。
    fn ink(&self) -> InkMetrics {
        InkMetrics::default()
    }
    /// 典型字高占 em 的比例（用于「字号自适应格高」等需要按实际字身计算的场景）。
    fn ink_height_ratio(&self) -> f64 {
        let ink = self.ink();
        if ink.valid() {
            ink.height() as f64 / self.em().max(1.0) as f64
        } else {
            0.7
        }
    }
}

/// 字体栈：按 `fontPath` 顺序回退查找字符，支持混用（按权重随机）。
pub struct FontStack {
    fonts: Vec<(Box<dyn StrokeFont>, f64)>,
}

impl FontStack {
    /// 等权重构造。
    pub fn new(fonts: Vec<Box<dyn StrokeFont>>) -> Self {
        FontStack {
            fonts: fonts.into_iter().map(|f| (f, 1.0)).collect(),
        }
    }
    /// 带权重构造（对应软件里“设置对应字体的优先几率”）。
    pub fn with_weights(fonts: Vec<(Box<dyn StrokeFont>, f64)>) -> Self {
        FontStack { fonts }
    }
    pub fn len(&self) -> usize {
        self.fonts.len()
    }
    pub fn is_empty(&self) -> bool {
        self.fonts.is_empty()
    }
    /// 取字形：返回 (字形, 字体索引)。`pick` 为 [0,1) 随机数，用于混用。
    pub fn glyph(&self, ch: char, pick: Option<f64>) -> Option<(Glyph, usize)> {
        if self.fonts.is_empty() {
            return None;
        }
        // 混用：按权重挑选一个字体，命中则用它，否则顺序回退
        let start = match pick {
            Some(p) => weighted_index(&self.fonts, p),
            None => 0,
        };
        let n = self.fonts.len();
        for k in 0..n {
            let i = (start + k) % n;
            if let Some(g) = self.fonts[i].0.glyph(ch) {
                return Some((g, i));
            }
        }
        None
    }
    pub fn em(&self) -> f32 {
        self.fonts.first().map(|f| f.0.em()).unwrap_or(100.0)
    }
    /// 按下标取字体。
    pub fn font(&self, i: usize) -> Option<&dyn StrokeFont> {
        self.fonts.get(i).map(|f| f.0.as_ref())
    }
}

fn weighted_index(fonts: &[(Box<dyn StrokeFont>, f64)], p: f64) -> usize {
    let total: f64 = fonts.iter().map(|(_, w)| w.max(0.0)).sum();
    if total <= 0.0 {
        return 0;
    }
    let mut acc = 0.0;
    let target = p.clamp(0.0, 1.0) * total;
    for (i, (_, w)) in fonts.iter().enumerate() {
        acc += w.max(0.0);
        if target <= acc {
            return i;
        }
    }
    fonts.len() - 1
}
