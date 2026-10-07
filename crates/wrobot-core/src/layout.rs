//! 文字排版：把文字框内容排成逐个字的落笔位置。
//!
//! # 坐标与单位约定
//! - 工程文件里 `x/y/width/height/lineGap/space` 为**像素 @96dpi**；
//! - `pageWidth/pageHeight` 为**毫米**；
//! - 内部统一换算到毫米。
//!
//! > 标定提示：`char_mm_at_100`（字号 100 时的字框边长）需用真机输出校准，
//! > 默认 8.0mm。

use crate::doc::{Align, FirstLine, Item};
use crate::font::{FontStack, Glyph, StrokeFont};
use rand::RngCore;

/// 排版参数。
#[derive(Debug, Clone)]
pub struct LayoutOptions {
    /// 像素/毫米（96dpi）
    pub px_per_mm: f64,
    /// 字号 100 时字框边长（毫米）
    pub char_mm_at_100: f64,
    /// 段首缩进字符数（firstLineAlign=TAB）
    pub tab_indent_chars: f64,
    /// 逼真化随机量（毫米 / 比例）
    pub random: RandomSpec,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        LayoutOptions {
            px_per_mm: 96.0 / 25.4,
            char_mm_at_100: 8.0,
            tab_indent_chars: 2.0,
            random: RandomSpec::default(),
        }
    }
}

/// 随机变形（对应软件“字体随机变形 / 字符设置”）。
#[derive(Debug, Clone, Copy, Default)]
pub struct RandomSpec {
    /// 每字水平抖动（毫米）
    pub x_mm: f64,
    /// 每字垂直抖动（毫米）
    pub y_mm: f64,
    /// 每字大小抖动比例（如 0.05 = ±5%）
    pub scale: f64,
    /// 行首随机偏移（毫米），对应“每行第一个字在 0~N 毫米随机出现”
    pub line_head_mm: f64,
}

/// 一个字在页面上的落笔位置。
#[derive(Debug, Clone, Copy)]
pub struct Placement {
    pub ch: char,
    /// 字形左上角 X（毫米）
    pub x: f64,
    /// 字形左上角 Y（毫米）
    pub y: f64,
    /// 字框边长（毫米）
    pub size: f64,
    /// 命中的字体索引
    pub font_idx: usize,
    /// 元素旋转（弧度）
    pub rot: f64,
    /// 旋转中心
    pub ox: f64,
    pub oy: f64,
    /// 逐字墨迹居中补偿（毫米）：使该字自身墨迹中心落在字框中心。
    /// 仅在 `PageInfo::ink_center` 打开时非零（格子示范字 / 单元格短文本）。
    pub ink_dx: f64,
    pub ink_dy: f64,
}

/// 排版结果。
#[derive(Debug, Clone, Default)]
pub struct Layout {
    pub placements: Vec<Placement>,
    /// 实际排出的行数
    pub lines: usize,
}

/// 对一个文字元素排版（确定性）。
pub fn layout_item(item: &Item, stack: &FontStack, opts: &LayoutOptions) -> Layout {
    layout_item_rng(item, stack, opts, None)
}

/// 对一个文字元素排版，可传入随机源以启用逼真化。
pub fn layout_item_rng(
    item: &Item,
    stack: &FontStack,
    opts: &LayoutOptions,
    mut rng: Option<&mut dyn RngCore>,
) -> Layout {
    let mut out = Layout::default();
    if !item.is_text() || stack.is_empty() {
        return out;
    }
    let pi = match &item.page_info {
        Some(p) => p.clone(),
        None => Default::default(),
    };
    let u = 1.0 / opts.px_per_mm; // 毫米/像素
    let ox = item.x * u;
    let oy = item.y * u;
    let rot = item.rotation;

    let font_size = if pi.font_size > 0.0 { pi.font_size } else { 100.0 };
    let size = opts.char_mm_at_100 * font_size / 100.0;
    let em = stack.em() as f64;
    let letter_space = pi.space * u;

    // 可写宽度：优先 pageWidth（毫米），否则用元素宽度换算
    let usable = if pi.page_width > 0.0 {
        pi.page_width
    } else {
        item.width * u
    };

    let line_gap = pi.line_gap * u;
    let gap_offset = pi.line_gap_offset * u;
    // 行距允许为负（把行压得更紧），但行步进必须为正，否则行会往上倒着排：
    // 下限取字框的 15%，保证字还能一行行往下走。
    let line_step = (size + line_gap + gap_offset).max(size * 0.15);

    // 字间距允许为负（把字挤紧），但单字前进宽度必须为正，
    // 否则光标会往回走、换行判断也会失效。
    let min_adv = (size * 0.05).max(0.01);
    let adv_of = |ch: char| (advance_mm(stack, ch, size, em) + letter_space).max(min_adv);

    let align = item.align();
    let first = item.first_line();
    let tab = opts.tab_indent_chars * size;

    let text = item.text();
    let mut line_idx = 0usize;

    for raw_line in text.split('\n') {
        // 段首处理只作用于每个自然段的第一行（这里按 \n 分段，故即 raw_line 的第 1 行）
        let mut wrapped: Vec<Vec<char>> = Vec::new();
        let mut cur: Vec<char> = Vec::new();
        let mut cur_w = if line_idx == 0 && first == FirstLine::Tab {
            tab
        } else {
            0.0
        };

        for ch in raw_line.chars() {
            let adv = adv_of(ch);
            if !cur.is_empty() && cur_w + adv > usable {
                wrapped.push(std::mem::take(&mut cur));
                cur_w = 0.0;
            }
            cur.push(ch);
            cur_w += adv;
        }
        wrapped.push(cur);

        for (wi, line) in wrapped.into_iter().enumerate() {
            if line.is_empty() {
                // 空行也占位
                line_idx += 1;
                continue;
            }
            let width: f64 = line.iter().map(|&c| adv_of(c)).sum();
            let indent = if line_idx == 0 && first == FirstLine::Tab {
                tab
            } else {
                0.0
            };
            let start_x = match align {
                Align::Left => ox + indent,
                Align::Center => ox + (usable - width) * 0.5 + indent * 0.0,
                Align::Right => ox + (usable - width) - indent,
            };
            // 首行居中：firstLineAlign=CENTER 时第一行整行居中
            let start_x = if first == FirstLine::Center && line_idx == 0 {
                ox + (usable - width) * 0.5
            } else {
                start_x
            };
            let base_y = oy + (line_idx as f64) * line_step;

            // 行首随机偏移
            let head_off = if opts.random.line_head_mm > 0.0 {
                match &mut rng {
                    Some(r) => rnd(r) * opts.random.line_head_mm,
                    None => 0.0,
                }
            } else {
                0.0
            };

            let mut x = start_x + head_off;
            for &ch in &line {
                let (dx, dy, ds) = if opts.random.x_mm > 0.0 || opts.random.y_mm > 0.0 {
                    match &mut rng {
                        Some(r) => (
                            (rnd(r) - 0.5) * 2.0 * opts.random.x_mm,
                            (rnd(r) - 0.5) * 2.0 * opts.random.y_mm,
                            1.0 + (rnd(r) - 0.5) * 2.0 * opts.random.scale,
                        ),
                        None => (0.0, 0.0, 1.0),
                    }
                } else {
                    (0.0, 0.0, 1.0)
                };
                let (g, fi) = match stack.glyph(ch, None) {
                    Some(v) => v,
                    None => continue,
                };
                // 逐字墨迹居中：把「本字墨迹中心」与「字库典型墨迹中心」的差值补上。
                let (ink_dx, ink_dy) = if pi.ink_center {
                    per_glyph_ink_offset(stack.font(fi), &g, size)
                } else {
                    (0.0, 0.0)
                };
                let _ = g; // 字形本体在渲染阶段取用
                out.placements.push(Placement {
                    ch,
                    x: x + dx,
                    y: base_y + dy,
                    size: size * ds,
                    font_idx: fi,
                    rot,
                    ox,
                    oy,
                    ink_dx,
                    ink_dy,
                });
                x += adv_of(ch);
                let _ = wi;
            }
            line_idx += 1;
        }
        out.lines = out.lines.max(line_idx);
    }
    out.lines = out.lines.max(line_idx);

    // 垂直居中：把整块文字在可写区域高度内居中（用于表格单元格 / 格子示范字）。
    if pi.vcenter && pi.page_height > 0.0 && out.lines > 0 {
        let block_h = (out.lines as f64 - 1.0) * line_step + size;
        let shift = (pi.page_height - block_h) * 0.5;
        if shift.abs() > 1e-9 {
            for pl in &mut out.placements {
                pl.y += shift;
                pl.oy += shift;
            }
        }
    }

    out
}

/// 单个字形自身的墨迹框（字体单位），无笔画时返回 None。
fn glyph_ink_box(g: &Glyph) -> Option<(f32, f32, f32, f32)> {
    let mut x0 = f32::MAX;
    let mut y0 = f32::MAX;
    let mut x1 = f32::MIN;
    let mut y1 = f32::MIN;
    let mut any = false;
    for s in &g.strokes {
        for &(x, y) in &s.points {
            any = true;
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    if any {
        Some((x0, y0, x1, y1))
    } else {
        None
    }
}

/// 计算「本字墨迹中心 → 字库典型墨迹中心」的补偿量（毫米）。
///
/// 渲染层已按字库典型墨迹框把字身居中；这里再补上单个字与典型值的偏差，
/// 使每个字都精确落在格心（练字格要求「字心 == 格心」）。
fn per_glyph_ink_offset(font: Option<&dyn StrokeFont>, g: &Glyph, size_mm: f64) -> (f64, f64) {
    let (Some(f), Some((x0, y0, x1, y1))) = (font, glyph_ink_box(g)) else {
        return (0.0, 0.0);
    };
    let em = f.em().max(1.0) as f64;
    let s = size_mm / em;
    let (fcx, fcy) = f.ink().center();
    (
        (fcx as f64 - (x0 + x1) as f64 * 0.5) * s,
        (fcy as f64 - (y0 + y1) as f64 * 0.5) * s,
    )
}

/// 字符前进宽度（毫米）。
fn advance_mm(stack: &FontStack, ch: char, size: f64, em: f64) -> f64 {
    match stack.glyph(ch, None) {
        Some((g, _)) => size * (g.advance as f64 / em),
        None => size,
    }
}

fn rnd(r: &mut dyn RngCore) -> f64 {
    (r.next_u32() as f64) / (u32::MAX as f64)
}

/// 对整个工程排版。
pub fn layout_document(
    doc: &crate::doc::Document,
    stack: &FontStack,
    opts: &LayoutOptions,
) -> Vec<Layout> {
    doc.text_items()
        .map(|it| layout_item(it, stack, opts))
        .collect()
}
