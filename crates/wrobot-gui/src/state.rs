//! 应用状态与「参数 → 工程元素」构建逻辑（不含 UI）。

use rand::rngs::StdRng;
use rand::SeedableRng;
use std::path::Path;
use wrobot_core::doc::{ContentFormat, Document, Item, PageInfo};
use wrobot_core::font::{BuiltinFont, StrokeFont};
use wrobot_core::render::{dashed_rect, rect, Polyline};
use wrobot_core::{Firmware, GcodeOptions, LayoutOptions, PenMode};

use crate::device::DeviceController;
use crate::fontmgr::FontManager;

/// 常规纸张规格（毫米，**纵向** 宽×高）。
///
/// 之前页面尺寸写死成 175×255 —— 既不是任何标准纸，界面也没有地方能选，
/// 用户只能靠两个滑块猜。这里给出常用规格一键切换，不匹配时落到「自定义」。
pub const PAPER_PRESETS: &[(&str, f64, f64)] = &[
    ("A4", 210.0, 297.0),
    ("A5", 148.0, 210.0),
    ("A3", 297.0, 420.0),
    ("B4", 250.0, 353.0),
    ("B5", 176.0, 250.0),
    ("16K", 185.0, 260.0),
    ("32K", 130.0, 184.0),
    ("Letter", 215.9, 279.4),
    ("Legal", 215.9, 355.6),
    ("明信片", 100.0, 148.0),
];

/// 自定义尺寸的预设名（不在 [`PAPER_PRESETS`] 里时用这个）。
pub const PAPER_CUSTOM: &str = "自定义";

/// 默认纸张：A4（最常见的写字机幅面）。
pub fn default_paper() -> String {
    "A4".to_string()
}

/// 按名字查纸张尺寸（找不到返回 None）。
pub fn paper_size(name: &str) -> Option<(f64, f64)> {
    PAPER_PRESETS
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, w, h)| (*w, *h))
}

/// 笔记模块的一个自由文本块。
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct NoteBlock {
    pub text: String,
    pub font: String,
    pub size_mm: f64,
    pub x: f64,
    pub y: f64,
    pub align: usize, // 0 左 1 中 2 右
}

/// 一个**文本框**。
///
/// 参考项目（奎享雕刻）的工程存档里，一页可以有任意多个文字元素，
/// 每个都带自己的字体、字号、行距、对齐与位置。原先本程序只有「一页一个文本框」，
/// 做不了多段落/多区域排版，这里补齐为一组可增删的文本框。
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct TextBox {
    /// 框名（仅用于界面列表显示，不参与输出）
    pub name: String,
    pub text: String,
    pub font: String,
    /// 字号（基准 100）
    pub size: f64,
    /// 行距（mm）
    pub line_gap: f64,
    /// 字间距（mm）
    pub space: f64,
    /// 对齐 0 左 1 中 2 右
    pub align: usize,
    /// 段首 0 无 1 居中 2 缩进
    pub first: usize,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// 是否输出（可临时关掉某个框而不删掉内容）
    pub on: bool,
    /// **本框是否应用随机变形**。
    ///
    /// 以前「启用随机变形」是对整页所有文字一刀切生效的，用户只想让其中一段
    /// 更像手写、其余保持工整时做不到。现在按框勾选，只对勾上的框施加抖动。
    #[serde(default = "default_true")]
    pub random: bool,
}

impl Default for TextBox {
    fn default() -> Self {
        Self {
            name: "文本框 1".to_string(),
            text: String::new(),
            font: String::new(),
            size: 100.0,
            line_gap: 3.0,
            space: 0.0,
            align: 0,
            first: 0,
            x: 10.0,
            y: 15.0,
            w: 180.0,
            h: 260.0,
            on: true,
            random: true,
        }
    }
}

impl TextBox {
    /// 列表上的一行摘要：优先显示框名，加上前几个字，便于在多个框之间辨认。
    pub fn summary(&self) -> String {
        let head: String = self.text.chars().take(8).collect();
        if head.is_empty() {
            format!("{}（空）", self.name)
        } else {
            format!("{} · {}", self.name, head)
        }
    }
}

/// 图片绘制类型。
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum ImgDraw {
    /// 线框：提取轮廓边缘
    Wire = 0,
    /// 素描：横向影线填充暗部
    Sketch = 1,
    /// 高效：把相邻行的墨段串成连续笔画（蛇形）+ 最优路径排序
    Continuous = 2,
    /// 中心线：细化成单像素骨架后再描线，适合线稿/logo/字帖
    Centerline = 3,
    /// 描边：等值轮廓线，只画明暗交界，不填影线
    Contour = 4,
    /// 交叉排线：主方向影线 + 暗部垂直排线
    CrossHatch = 5,
    /// 网点：按灰度疏密生成半调网点
    Halftone = 6,
    /// 素描原稿：沿原稿每一笔的墨脊逐笔追踪，穿过交叉点不串线，按明暗分层落笔
    SketchOriginal = 7,
    /// 素描调子：把明暗量化成多层，亮部留白，暗部叠交叉排线，轮廓单独加重
    SketchHatch = 8,
    /// 分区重绘：按深度分层、细分块定方向，浅层先铺、深层后叠，逐层压暗
    RegionRedraw = 9,
    /// 素描笔触：残差驱动短直线，暗部自动加密，方向沿轮廓或原稿笔触
    StrokeSketch = 10,
}

impl ImgDraw {
    pub fn names() -> &'static [&'static str] {
        &[
            "线框（边缘影线）",
            "素描（影线）",
            "高效（连续笔画）",
            "中心线（骨架）",
            "描边（等值轮廓）",
            "交叉排线（明暗）",
            "网点（半调）",
            "素描原稿",
            "素描调子（分层排线）",
            "分区重绘（深度分层）",
            "素描笔触（融合布线）",
        ]
    }

    pub fn name(self) -> &'static str {
        Self::names()[self as usize]
    }

    /// 当前模式的一句话说明（面板上直接显示，避免用户猜参数含义）。
    pub fn hint(self) -> &'static str {
        match self {
            ImgDraw::Wire => "沿图像边缘铺影线，适合线条清晰的照片或扫描稿。",
            ImgDraw::Sketch => "按明暗阈值逐行铺影线，最经典的素描效果。",
            ImgDraw::Continuous => "把影线串成长笔画，抬落笔最少、绘制最快。",
            ImgDraw::Centerline => "先把墨迹细化成单像素骨架再描线，适合线稿、logo、硬笔字。",
            ImgDraw::Contour => "只画明暗交界的等值轮廓线，画面最干净，适合简笔画。",
            ImgDraw::CrossHatch => "主方向影线打底，暗部再叠一层垂直排线，明暗层次更丰富。",
            ImgDraw::Halftone => "按灰度生成大小不一的网点，适合照片/版画风格；网点越多越慢。",
            ImgDraw::SketchOriginal => {
                "逐笔还原原稿，但按**明暗**取舍：亮部留白、中间调稀疏、暗部排满，\
                 轮廓/结构线自动加重。素描的明暗是靠线条疏密表现的，不是把原稿每根线\
                 都描一遍。适合铅笔素描/速写草稿。"
            }
            ImgDraw::SketchHatch => {
                "把明暗翻译成排线：亮部留白，中间调一层单向稀排线，越暗叠越多层\
                 交叉排线；轮廓再用梯度单独提一遍重线。同一支笔靠疏密表现灰阶，\
                 适合照片、铅笔素描和成片暗部。"
            }
            ImgDraw::RegionRedraw => {
                "先量出画面里每一小片的深度层（约 5 层），再把每层切成细块单独定\
                 排线方向：浅层用浅铅笔先铺，深层用深铅笔逐层叠加，最后压轮廓。\
                 比调子模式更贴原稿的局部走向，适合铅笔素描、人像。"
            }
            ImgDraw::StrokeSketch => {
                "按真实素描的工序走：先量每一小块还差多少墨，再用**短直线段**去补。\
                 差得多就排得密、差得少就留得疏，线距不再全图统一；方向可沿轮廓\
                 （等照度线）或原稿笔触，暗部再叠交叉层，最后压少量结构线。"
            }
        }
    }

    /// 由下拉序号构造；越界时退回「素描」（保留旧存档兼容）。
    pub fn from_usize(i: usize) -> Self {
        match i {
            0 => ImgDraw::Wire,
            1 => ImgDraw::Sketch,
            2 => ImgDraw::Continuous,
            3 => ImgDraw::Centerline,
            4 => ImgDraw::Contour,
            5 => ImgDraw::CrossHatch,
            6 => ImgDraw::Halftone,
            7 => ImgDraw::SketchOriginal,
            8 => ImgDraw::SketchHatch,
            9 => ImgDraw::RegionRedraw,
            10 => ImgDraw::StrokeSketch,
            _ => ImgDraw::Sketch,
        }
    }
}

/// 一支真实的笔：线宽、满覆盖能压出的最深色、笔色、以及换笔后的套准偏移。
///
/// 写字机没有笔压，但可以换笔。HB、2B、4B、8B 或 0.25/0.5/1.0mm 的针管笔
/// 能压出的最深处不同——把明暗带按这个值分给不同的笔，黑色才真正黑得下去；
/// 只让一支 HB 靠排线密度硬撑，暗部永远是一层灰。
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct PenSpec {
    /// 界面显示名（如「2B 0.5」）
    pub name: String,
    /// 笔尖线宽（毫米）
    pub width_mm: f64,
    /// 满覆盖时的视觉深度（0~1）：HB≈0.45，2B≈0.65，4B≈0.82，8B≈0.95
    pub darkness: f64,
    /// 笔色（预览与分笔标签用；铅笔用灰阶）
    pub color: [u8; 3],
    /// 换笔后的笔尖套准偏移 `(dx,dy)`（毫米）。换笔位置不可能完全重合，
    /// 用这个小偏移把整层推回去，比重新对刀快。
    pub offset_mm: [f64; 2],
    /// 是否参与多笔分层（关掉后这支笔的层会分给其他笔）
    #[serde(default = "default_true")]
    pub on: bool,
}

impl PenSpec {
    /// 转成算法只关心的两个量。
    pub fn to_hatch(&self) -> crate::sketch::HatchPen {
        crate::sketch::HatchPen {
            width_mm: self.width_mm.clamp(0.15, 1.6),
            darkness: self.darkness.clamp(0.2, 1.0) as f32,
        }
    }
}

/// 默认四支笔：从浅到深。
///
/// 真实铅笔换的是软硬（石墨浓度），不是笔尖粗细：一支削好的 8B 照样能画出
/// 0.3mm 的线。原稿实测的发丝、睫毛都只有 0.2~0.4mm，上一版 0.7mm 的 8B
/// 落在纸上仍是一根根粗黑杠。现在线宽收到 0.20~0.40mm：原稿的浅排线只有
/// 1px（300mm 高时约 0.13mm），0.28mm 的 HB 叠上相邻几根就糊成一条粗带，
/// 和「细而密」正好相反。明暗仍交给石墨浓度，密度交给更细的排线间距。
pub fn default_pens() -> Vec<PenSpec> {
    vec![
        PenSpec {
            name: "HB 0.20".to_string(),
            width_mm: 0.20,
            darkness: 0.42,
            color: [150, 150, 150],
            offset_mm: [0.0, 0.0],
            on: true,
        },
        PenSpec {
            name: "2B 0.26".to_string(),
            width_mm: 0.26,
            darkness: 0.62,
            color: [105, 105, 105],
            offset_mm: [0.0, 0.0],
            on: true,
        },
        PenSpec {
            name: "4B 0.32".to_string(),
            width_mm: 0.32,
            darkness: 0.80,
            color: [62, 62, 62],
            offset_mm: [0.0, 0.0],
            on: true,
        },
        PenSpec {
            name: "8B 0.40".to_string(),
            width_mm: 0.40,
            darkness: 0.93,
            color: [25, 25, 25],
            offset_mm: [0.0, 0.0],
            on: true,
        },
    ]
}

/// 多笔绘制里「文字 / 边框」默认用第二支笔（2B 0.5）——比 HB 更清楚，
/// 又不至于像 8B 那样把字压得发糊。
fn default_main_pen() -> usize {
    1
}

/// 针管笔组：0.25 / 0.5 / 0.8 / 1.0mm。针管笔颜色都接近纯黑，
/// 明暗只靠线宽与密度，所以这里把「可压深」也按线宽排开。
pub fn needle_pens() -> Vec<PenSpec> {
    let mut v = default_pens();
    let spec = [
        ("0.25 针管", 0.25, 0.55, 90u8),
        ("0.5 针管", 0.5, 0.72, 60),
        ("0.8 针管", 0.8, 0.85, 35),
        ("1.0 针管", 1.0, 0.95, 15),
    ];
    for (p, (name, w, d, c)) in v.iter_mut().zip(spec.iter()) {
        p.name = (*name).to_string();
        p.width_mm = *w;
        p.darkness = *d;
        p.color = [*c, *c, *c];
    }
    v
}

/// 一张已导入的图片（前景可打印 / 背景参考不打印）。
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct ImageObject {
    /// 来源路径（仅用于显示）
    pub path: String,
    /// 解码后的图片字节（保留以便重新生成）
    pub bytes: Vec<u8>,
    /// 页面上的左上角（毫米，y 向下）
    pub x: f64,
    pub y: f64,
    /// 缩放（1.0 = 以 target_h_mm 为基准高度）
    pub scale: f64,
    /// 旋转角度（度，顺时针为正）
    pub rotation: f64,
    /// 水平镜像
    pub flip_h: bool,
    /// 垂直镜像
    pub flip_v: bool,
    /// 绘制类型
    pub draw: ImgDraw,
    /// 影线/采样行间隔（像素）
    pub step_px: u32,
    /// 暗部阈值（0..1，越低越黑）
    pub threshold: f64,
    /// 边缘阈值（0..1，仅线框）
    pub edge_threshold: f64,
    /// 反相（黑变白、白变黑）
    pub invert: bool,
    /// **裁剪区域**（相对原图宽高的比例）：`(左, 上, 右, 下)`；`(0,0,1,1)` = 不裁剪。
    #[serde(default = "default_crop")]
    pub crop: (f64, f64, f64, f64),
    /// **扫描方向**：0 横向 / 1 对角 45° / 2 纵向 / 3 对角 135°。
    #[serde(default)]
    pub scan_angle: u8,
    /// **连笔跨行容差**（像素，连续笔画模式）：行间墨段端点距离 ≤ 行距+该值就接续。
    #[serde(default = "default_merge_gap")]
    pub merge_gap_px: u32,
    /// **笔画顺序**：0 默认 / 1 最优路径 / 2 按扫描顺序 / 3 分块蛇形。
    #[serde(default)]
    pub order_mode: u8,
    /// 分块边长（像素，`order_mode == 3` 时生效）。
    #[serde(default = "default_block_px")]
    pub block_px: u32,
    /// **素描原稿模式**：暗部线条最多叠描几遍（1 = 只描一遍）。
    ///
    /// 叠描次数由每条笔迹的局部明暗决定：起稿轻线一遍，暗部按力度多描，
    /// 偏移小于笔宽，叠成一根更重的线而不是并排几根。
    #[serde(default = "default_sketch_pressure")]
    pub sketch_pressure: u32,
    /// **素描原稿模式**：手抖与重描的偏移幅度（毫米）。
    ///
    /// 沿线条法向做低频起伏（不是周期性正弦），并给长线加一点弓形；
    /// 0 = 严格描摹原稿，0.2~0.4 接近铅笔的手感。
    #[serde(default = "default_sketch_jitter")]
    pub sketch_jitter: f64,
    /// **素描原稿模式**：实际画笔线宽（毫米）。决定一条线盖多宽、两根线最近能排多密。
    #[serde(default = "default_sketch_pen")]
    pub sketch_pen: f64,
    /// **素描调子模式**：明暗分成几层（3..=8）。层数越多，暗部叠的交叉排线越多。
    #[serde(default = "default_hatch_levels")]
    pub hatch_levels: u32,
    /// **素描调子模式**：第一层排线的基准角（度）。暗部后续各层按固定角差交叉。
    #[serde(default = "default_hatch_angle")]
    pub hatch_angle: f64,
    /// **分区重绘模式**：方向采样块的边长（毫米）。块越细，局部走向越贴原稿。
    #[serde(default = "default_region_block_mm")]
    pub region_block_mm: f64,
    /// **素描笔触模式**：单根短直线的长度（毫米）。
    #[serde(default = "default_stroke_len_mm")]
    pub stroke_len_mm: f64,
    /// **素描笔触模式**：走向参考（0 轮廓 / 1 原稿 / 2 混合）。
    #[serde(default = "default_stroke_follow")]
    pub stroke_follow: u8,
    /// **多笔分层**：把明暗带分给不同的笔（HB/2B/4B/8B），分别出图、换笔绘制。
    #[serde(default)]
    pub multi_pen: bool,
    /// 是否作为背景参考（浅色显示、不参与打印）
    pub background: bool,
    /// **是否并入输出**（每张图单独控制）。
    ///
    /// BUG：以前只要进了「图片」模块就整包并入页面，用户没勾选也能看到别的图。
    /// 现在两张开关都要开——模块开关 `images_on` + 本图的 `out`——才会写入 G-code。
    #[serde(default = "default_true")]
    pub out: bool,
    /// 生成后的基础笔迹（页面毫米，未含 x/y/scale/rotation/flip 变换，尺寸为 natural_w × natural_h）
    pub base: Vec<Polyline>,
    /// 与 `base` 一一对应：每根笔迹由哪支笔绘制（下标对应 [`AppState::pens`]）。
    /// 旧工程没有这个字段，载入后视为全 0（单笔）。
    #[serde(default)]
    pub base_pens: Vec<u8>,
    /// 基础尺寸（毫米，scale=1 时）
    pub natural_w: f64,
    pub natural_h: f64,
    /// 生成此笔迹所用的参数快照（用于判断是否需要重算）
    pub gen_key: u64,
}

impl ImageObject {
    /// 由导入参数计算「生成密钥」，任一参数变化即触发重算。
    pub fn key(&self, pens_hash: u64) -> u64 {
        let mut h = self.bytes.len() as u64;
        h = h.wrapping_mul(31).wrapping_add(self.draw as u64);
        h = h.wrapping_mul(31).wrapping_add(self.multi_pen as u64);
        h = h.wrapping_mul(31).wrapping_add(pens_hash);
        h = h.wrapping_mul(31).wrapping_add(self.step_px as u64);
        h = h.wrapping_mul(31).wrapping_add((self.threshold * 1000.0) as u64);
        h = h.wrapping_mul(31).wrapping_add((self.edge_threshold * 1000.0) as u64);
        h = h.wrapping_mul(31).wrapping_add(self.invert as u64);
        h = h.wrapping_mul(31).wrapping_add(self.natural_h as u64);
        // 裁剪 / 扫描方向 / 连笔容差 / 顺序模式 / 分块
        for v in [
            self.crop.0,
            self.crop.1,
            self.crop.2,
            self.crop.3,
            self.scan_angle as f64,
            self.merge_gap_px as f64,
            self.order_mode as f64,
            self.block_px as f64,
            self.sketch_pressure as f64,
            self.sketch_jitter,
            self.sketch_pen,
            self.hatch_levels as f64,
            self.hatch_angle,
            self.region_block_mm,
            self.stroke_len_mm,
            self.stroke_follow as f64,
        ] {
            h = h.wrapping_mul(31).wrapping_add((v * 1000.0) as u64);
        }
        h
    }

    /// 是否真的裁掉了内容（`(0,0,1,1)` 视为不裁剪）。
    pub fn is_cropped(&self) -> bool {
        self.crop.0 > 1e-6 || self.crop.1 > 1e-6 || self.crop.2 < 1.0 - 1e-6 || self.crop.3 < 1.0 - 1e-6
    }

    /// 基础坐标（未变换，范围 `[0,natural_w]×[0,natural_h]`）→ 页面毫米坐标。
    ///
    /// 变换顺序：平移到中心 → 镜像 → 旋转 → 缩放 → 平移到 (x,y) 包围盒左上角。
    pub fn to_page(&self, px: f64, py: f64) -> (f64, f64) {
        let (nw, nh) = (self.natural_w, self.natural_h);
        let mut cx = px - nw * 0.5;
        let mut cy = py - nh * 0.5;
        if self.flip_h {
            cx = -cx;
        }
        if self.flip_v {
            cy = -cy;
        }
        let a = self.rotation.to_radians();
        let (cs, sn) = (a.cos(), a.sin());
        let rx = (cx * cs - cy * sn) * self.scale;
        let ry = (cx * sn + cy * cs) * self.scale;
        let ocx = self.x + nw * 0.5 * self.scale;
        let ocy = self.y + nh * 0.5 * self.scale;
        (ocx + rx, ocy + ry)
    }

    /// 页面毫米坐标 → 基础坐标（[`to_page`] 的逆变换），用于画布命中测试。
    pub fn from_page(&self, mx: f64, my: f64) -> (f64, f64) {
        let (nw, nh) = (self.natural_w, self.natural_h);
        let sc = self.scale.abs().max(1e-9);
        let ocx = self.x + nw * 0.5 * self.scale;
        let ocy = self.y + nh * 0.5 * self.scale;
        let rx = mx - ocx;
        let ry = my - ocy;
        let a = self.rotation.to_radians();
        let (cs, sn) = (a.cos(), a.sin());
        let mut cx = (rx * cs + ry * sn) / sc;
        let mut cy = (-rx * sn + ry * cs) / sc;
        if self.flip_h {
            cx = -cx;
        }
        if self.flip_v {
            cy = -cy;
        }
        (cx + nw * 0.5, cy + nh * 0.5)
    }

    /// 命中测试：页面毫米点是否落在这张图片（含旋转/镜像）的范围内。
    pub fn hit(&self, mx: f64, my: f64) -> bool {
        if self.natural_w <= 0.0 || self.natural_h <= 0.0 {
            return false;
        }
        let (px, py) = self.from_page(mx, my);
        px >= 0.0 && px <= self.natural_w && py >= 0.0 && py <= self.natural_h
    }

    /// 四角在页面毫米坐标中的位置（用于绘制旋转后的选中框）。
    pub fn corners(&self) -> [(f64, f64); 4] {
        let (nw, nh) = (self.natural_w, self.natural_h);
        [
            self.to_page(0.0, 0.0),
            self.to_page(nw, 0.0),
            self.to_page(nw, nh),
            self.to_page(0.0, nh),
        ]
    }
}

/// 大纸张分页布局：把页面按「可绘制幅面」切成 `cols × rows` 个图块。
///
/// 图块原点固定在页面尺寸的整数倍上（`start_col/start_row` 允许为负），
/// 这样每页绘制的都是页面坐标的同一块区域；按行优先逐页绘制后，
/// 只要把纸张按顺序边对边拼接即可复原整幅图。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileLayout {
    /// 单页可绘制宽/高（页面与机器幅面的较小者）
    pub width_mm: f64,
    pub height_mm: f64,
    pub cols: usize,
    pub rows: usize,
    /// 第一列/行对应的页面网格序号（可能为负）
    pub start_col: i64,
    pub start_row: i64,
}

impl TileLayout {
    pub fn count(&self) -> usize {
        self.cols.saturating_mul(self.rows)
    }

    /// 第 `page` 页（0 起、行优先）左上角在页面毫米坐标中的位置。
    pub fn origin(&self, page: usize) -> Option<(f64, f64)> {
        if page >= self.count() || self.cols == 0 {
            return None;
        }
        let c = (page % self.cols) as i64;
        let r = (page / self.cols) as i64;
        Some((
            (self.start_col + c) as f64 * self.width_mm,
            (self.start_row + r) as f64 * self.height_mm,
        ))
    }
}

/// 四角 1cm L 形定位标记：每个角的横/竖两条臂**各自从角点起笔**。
///
/// 之前把每个角写成一条 `(a,0)→(0,0)→(0,a)` 的三点折线，看似没问题，但
/// 「角点」是折线的**中间点**：笔在臂端落笔后要走到角点再折返，落笔延迟 /
/// 抽稀 / 空移任一环节出偏差，角点（尤其是起点 (0,0)）就会缺一小截。
/// 拆成每条臂两点后，每条都从角点精确落笔，原点角一定有笔迹。
///
/// `origin` = `(原点在右边?, 原点在下边?)`：**原点那一角的两条臂加长一倍，
/// 再加一道 45° 斜线**。四个 L 本身是完全对称的，光看 L 分不出 (0,0) 落在
/// 哪个角——Y 反了也看不出来，只能等画正文时才发现整页颠倒。加了这标记，
/// 一抬手就能确认方向对不对。
pub fn corner_mark_polylines(w: f64, h: f64, arm: f64, origin: (bool, bool)) -> Vec<Polyline> {
    let a = arm.min(w * 0.5).min(h * 0.5);
    if a <= 0.0 || w <= 0.0 || h <= 0.0 {
        return Vec::new();
    }
    let big = (a * 2.0).min(w).min(h);
    let mut out: Vec<Polyline> = Vec::new();
    for &(cx, cy) in &[(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)] {
        // 臂一律朝纸内
        let sx = if cx > 0.0 { -1.0 } else { 1.0 };
        let sy = if cy > 0.0 { -1.0 } else { 1.0 };
        let is_origin = (cx > 0.0) == origin.0 && (cy > 0.0) == origin.1;
        let len = if is_origin { big } else { a };
        out.push(Polyline::new(vec![(cx, cy), (cx + sx * len, cy)]));
        out.push(Polyline::new(vec![(cx, cy), (cx, cy + sy * len)]));
        if is_origin {
            // 从角点出发的 45° 短线，长度与普通臂相同：一眼就能认出原点角
            let d = a * std::f64::consts::FRAC_1_SQRT_2;
            out.push(Polyline::new(vec![(cx, cy), (cx + sx * d, cy + sy * d)]));
        }
    }
    out
}

/// 把一条折线按矩形裁剪成若干连续可见段（Liang–Barsky，逐段求交）。
fn clip_polyline_to_rect(
    pts: &[(f64, f64)],
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
) -> Vec<Vec<(f64, f64)>> {
    if pts.len() < 2 {
        return Vec::new();
    }
    let mut out: Vec<Vec<(f64, f64)>> = Vec::new();
    let mut run: Vec<(f64, f64)> = Vec::new();
    for w in pts.windows(2) {
        let clipped = clip_segment(w[0], w[1], x0, y0, x1, y1);
        let Some((a, b)) = clipped else {
            if run.len() >= 2 {
                out.push(std::mem::take(&mut run));
            } else {
                run.clear();
            }
            continue;
        };
        if run.is_empty() {
            run.push(a);
            run.push(b);
        } else {
            let last = *run.last().unwrap();
            if (last.0 - a.0).abs() < 1e-7 && (last.1 - a.1).abs() < 1e-7 {
                run.push(b);
            } else {
                if run.len() >= 2 {
                    out.push(std::mem::take(&mut run));
                } else {
                    run.clear();
                }
                run.push(a);
                run.push(b);
            }
        }
    }
    if run.len() >= 2 {
        out.push(run);
    }
    out.into_iter()
        .filter(|r| {
            r.len() >= 2
                && r.first()
                    .zip(r.last())
                    .is_some_and(|(a, b)| (a.0 - b.0).abs() > 1e-9 || (a.1 - b.1).abs() > 1e-9)
        })
        .collect()
}

/// 线段 → 矩形内的可见部分（含两端都落在矩形外但穿过矩形的情况）。
fn clip_segment(
    a: (f64, f64),
    b: (f64, f64),
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
) -> Option<((f64, f64), (f64, f64))> {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let (mut t0, mut t1) = (0.0_f64, 1.0_f64);
    let p = [-dx, dx, -dy, dy];
    let q = [a.0 - x0, x1 - a.0, a.1 - y0, y1 - a.1];
    for i in 0..4 {
        if p[i].abs() < 1e-12 {
            if q[i] < 0.0 {
                return None;
            }
        } else {
            let r = q[i] / p[i];
            if p[i] < 0.0 {
                if r > t1 {
                    return None;
                }
                if r > t0 {
                    t0 = r;
                }
            } else {
                if r < t0 {
                    return None;
                }
                if r < t1 {
                    t1 = r;
                }
            }
        }
    }
    if t0 > t1 {
        return None;
    }
    Some((
        (a.0 + dx * t0, a.1 + dy * t0),
        (a.0 + dx * t1, a.1 + dy * t1),
    ))
}

/// 格子类型。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GridKind {
    Blank = 0,
    Tian = 1, // 田字格
    Mi = 2,   // 米字格
    Hui = 3,  // 回宫格
}

impl GridKind {
    pub fn names() -> &'static [&'static str] {
        &["空白格", "田字格", "米字格", "回宫格"]
    }
}

/// 设备连接方式。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DevKind {
    Serial = 0,
    Tcp = 1,
    Mock = 2,
}

/// 内容模块索引（与 tab 0..4 对应）。
pub const MODULE_COUNT: usize = 5;

/// 保活心跳默认间隔（毫秒）：**5 秒**。
///
/// 写字机那端的蓝牙 SPP 模块普遍有空闲断链机制，几十秒到几分钟没数据流就断开，
/// 5 秒一次心跳足以稳住链路，又不会给固件造成负担。
fn default_keepalive_ms() -> u64 {
    5000
}

fn default_true() -> bool {
    true
}

/// 图片裁剪默认不裁（左0 上0 右1 下1）。
fn default_crop() -> (f64, f64, f64, f64) {
    (0.0, 0.0, 1.0, 1.0)
}

/// 连笔跨行容差默认 8px：既能串出蛇形长笔画，又不会把隔太远的段硬连。
fn default_merge_gap() -> u32 {
    8
}

/// 分块蛇形的默认块边长 40px。
fn default_block_px() -> u32 {
    40
}

/// 素描原稿：默认叠 3 遍（起稿 1 遍、暗部加到 3 遍），
/// 这样原稿里的轻重层次才看得出来。
fn default_sketch_pressure() -> u32 {
    3
}

/// 素描原稿模式默认手抖/重描幅度 0.12mm。
///
/// 这是笔尖落在墨迹上的自然偏移量：原稿的线宽通常才 0.3mm，
/// 抖动一旦超过半个线宽，线就开始从墨迹上滑开、多遍也会散成几根头发丝。
fn default_sketch_jitter() -> f64 {
    0.12
}

/// 素描原稿模式默认画笔线宽 0.5mm（中性笔/圆珠笔量级）。
///
/// 这个值直接决定「一格里能排几根线」：填得太细，算法会以为还能再密，
/// 结果整页铺满；填得太粗，暗部会被压得过稀、层次塌掉。
fn default_sketch_pen() -> f64 {
    0.5
}

/// 素描调子默认 6 层：4 层以下暗部层次会明显断层，8 层以上同一支笔画不出更细的灰。
fn default_hatch_levels() -> u32 {
    6
}

/// 素描调子默认 35°：最接近人手自然的斜向排线，又能和 90° 的轮廓区分开。
fn default_hatch_angle() -> f64 {
    35.0
}

/// 分区重绘默认分块 2.5mm：A4 人像上约能分出 60×90 个方向块，
/// 眼窝、鼻梁、脸颊各走各的；再细就只剩像素噪声，方向反而不稳。
fn default_region_block_mm() -> f64 {
    2.5
}

/// 素描笔触默认 3.2mm：真实素描除轮廓外大多是 2~4mm 的短直线。
/// 再长就成排线，再短则碎成点画，明暗块面连不起来。
fn default_stroke_len_mm() -> f64 {
    3.2
}

/// 默认「混合」：先沿原稿笔触/轮廓，暗部再叠交叉层，兼顾形与调子。
fn default_stroke_follow() -> u8 {
    2
}

/// 四角定位框默认宽度（毫米）。
fn default_corner_w() -> f64 {
    200.0
}

/// 四角定位框默认高度（毫米）：留出 A4 上下的纸边，避免 L 标记贴到纸边。
fn default_corner_h() -> f64 {
    280.0
}

/// 页面外侧标尺（顶部 + 左侧毫米刻度）默认开启。
fn default_ruler_on() -> bool {
    true
}

/// 页面网格默认关闭：不勾选时预览页不显示任何网格线。
fn default_grid_overlay() -> bool {
    false
}

/// 自定义网格默认列数（竖向分割线数量）。
fn default_grid_div_x() -> usize {
    10
}

/// 自定义网格默认行数（横向分割线数量；A4 纵向，行略多于列）。
fn default_grid_div_y() -> usize {
    14
}

fn one() -> usize {
    1
}

/// 画布视图缩放默认值：1.0 = 整页适应窗口。
fn default_canvas_zoom() -> f32 {
    1.0
}

/// 参考项目 `default.properties`：jog_speed=10000
fn default_jog_speed() -> f64 {
    10000.0
}

/// 参考项目 `default.properties`：mm_to_steps=100
fn default_mm_to_steps() -> f64 {
    100.0
}

/// 参考项目 `default.properties`：max_pen_speed=32767
fn default_max_pen_speed() -> f64 {
    32767.0
}

/// 参考项目 `default.properties`：marlin_pen_down_mm=2
fn default_marlin_pen_down() -> f64 {
    2.0
}

/// 参考项目 `default.properties`：marlin_z_speed=20000
fn default_marlin_z_speed() -> f64 {
    20000.0
}

/// 参考项目 `default.properties`：marlin_pen_up=50（舵机抬笔角度）
fn default_marlin_servo_up() -> f64 {
    50.0
}

/// XY 最小步进：0.2mm。
///
/// 单线字库每段笔画只有百分之几毫米，逐点下发会让 G-code 又长又慢；
/// 按 0.2mm 抽稀后行数降一个数量级，笔尖 0.5mm 下肉眼无差别。
fn default_min_step() -> f64 {
    wrobot_core::DEFAULT_MIN_STEP
}

/// 写字机蓝牙模块的默认广播名（奎享雕刻出厂名）。
fn default_bt_name() -> String {
    "BtWriter".to_string()
}

/// 毫米数显示：去掉多余小数（`210.0` → `210`，`215.9` → `215.9`）。
fn trim_mm(v: f64) -> String {
    let s = format!("{:.1}", v);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// 当前页面的任务统计（只保留展示需要的量，避免每帧重复计算折线长度）。
#[derive(Clone, Copy, Default)]
pub struct JobStats {
    /// 笔画条数（抬笔次数 + 1）
    pub strokes: usize,
    /// 折线点总数
    pub points: usize,
    /// 落笔墨线总长（mm）
    pub ink_mm: f64,
    /// 笔画之间的空移总长（mm）
    pub travel_mm: f64,
}

/// 多笔绘制里的一支笔：一份独立 G-code + 进度页要显示的笔迹。
#[derive(Clone)]
pub struct PenJob {
    /// 笔在**启用笔组**里的下标（与 [`ImageObject::base_pens`] 同一套编号）
    pub pen: usize,
    /// 界面显示名
    pub name: String,
    pub gcode: String,
    pub paths: Vec<Polyline>,
}

/// 「不属于任何一支笔」的笔迹（文字、边框、四角标记、单笔图片）。
///
/// 多笔绘制时这些内容会被分给「主笔」；其余时候只用于预览按主题墨色显示。
pub const NON_PEN: u8 = u8::MAX;

/// 整个应用的可变状态。
///
/// 注：`serde` 仅用于「工程完整保存/载入」——对字体与设备连接等运行时对象
/// 标 `#[serde(skip)]`，载入时保留当前会话的字库与设备连接，只恢复可编辑的页面数据。
#[derive(serde::Serialize, serde::Deserialize)]
pub struct AppState {
    #[serde(skip)]
    pub fonts: FontManager,
    #[serde(skip)]
    pub font_dir: String,

    // 页面（毫米）
    pub page_w_mm: f64,
    pub page_h_mm: f64,
    /// 纸张规格名（[`PAPER_PRESETS`] 里的名字或 [`PAPER_CUSTOM`]）。
    ///
    /// 单独存一份是为了下拉框能正确回显：只靠宽高反推的话，
    /// 自定义成 210×297 会被误显示成 A4。
    #[serde(default = "default_paper")]
    pub paper: String,
    pub char_mm_at_100: f64,
    pub orientation: i32,

    /// 各模块是否并入页面（除当前模块外，可额外叠加）。
    pub module_on: [bool; MODULE_COUNT],

    // —— 页面边框（源程序 edge 模块） ——
    pub border_on: bool,
    pub border_margin: f64,
    pub border_style: usize, // 0 实线 1 虚线 2 双线

    // —— 模块① 文字 ——
    /// **多个文本框**：一页可以有任意个文字区域，各自独立设置字体/字号/位置。
    /// 为空时（旧存档）由下面的单框字段自动迁移出一个。
    #[serde(default)]
    pub texts: Vec<TextBox>,
    /// 当前正在编辑第几个文本框
    #[serde(default)]
    pub sel_text: usize,
    /// 旧存档兼容：单文本框时代的字段，加载后迁移进 `texts`。
    pub text: String,
    pub text_font: String,
    pub text_font_size: f64, // 基准 100
    pub text_line_gap: f64,
    pub text_space: f64,
    pub text_align: usize,
    pub text_first: usize,
    pub text_x: f64,
    pub text_y: f64,
    pub text_w: f64,
    pub text_h: f64,

    // —— 逼真化 ——
    pub use_random: bool,
    pub rand_seed: u64,
    pub rand_x: f64,
    pub rand_y: f64,
    pub rand_scale: f64,
    pub rand_head: f64,

    // —— 模块② 表格 ——
    pub table_rows: usize,
    pub table_cols: usize,
    pub table_cell: f64,
    pub table_font: String,
    pub table_font_size: f64,
    pub table_auto_font: bool,
    pub table_text: String,
    pub table_x: f64,
    pub table_y: f64,
    /// 表格线条宽度（mm，>0.35 用多次并行描边实现加粗）
    pub table_line_width: f64,
    /// 显示横向线条
    pub table_show_h: bool,
    /// 显示纵向线条
    pub table_show_v: bool,

    // —— 模块③ 格子 ——
    pub grid_kind: usize,
    pub grid_rows: usize,
    pub grid_cols: usize,
    pub grid_cell: f64,
    pub grid_font: String,
    pub grid_font_size: f64,
    pub grid_auto_font: bool,
    pub grid_sample: String,
    pub grid_x: f64,
    pub grid_y: f64,
    /// 显示格子（总开关：关闭则线条与示范字都不输出）
    pub grid_show: bool,
    /// 显示横向线条（横行）
    pub grid_show_h: bool,
    /// 显示纵向线条（竖行）
    pub grid_show_v: bool,
    /// 格子线条宽度（mm，>0.35 用多次并行描边实现加粗）
    pub grid_line_width: f64,

    // —— 模块④ 绘图 ——
    pub draw_paths: Vec<Polyline>,
    pub draw_path: String,
    pub draw_target_h: f64,
    pub draw_step: f64,
    pub draw_thresh: f64,
    /// 手绘模式：在画布上拖动直接生成笔迹
    pub freehand_on: bool,
    /// 手绘过程中的临时点集（毫米）
    #[serde(skip)]
    pub freehand: Vec<(f64, f64)>,

    // —— 模块⑤ 笔记（多文本块） ——
    pub notes: Vec<NoteBlock>,

    // —— 导入图片（前景可打印 / 背景参考） ——
    pub images: Vec<ImageObject>,
    /// 图片是否并入当前页面（对应左侧「并入当前页面 → 图片」勾选）。
    ///
    /// 之前前景图片是**无条件**叠加进笔迹的，导致用户在文字页明明没勾选叠加，
    /// 页面上却仍出现别的模块导入的图片。后来改成「停在图片页就并入」，
    /// 结果又变成：一点「图片」模块，所有图片（包括别的页留下的）全被写进 G-code。
    ///
    /// 现在：**默认不并入**，只有显式勾选这个开关才并入；
    /// 且每张图还有自己的「输出」开关，只有勾上的那几张才会并入。
    /// 停在图片页时画布仍会显示它们（浅色，供编辑定位），但不会写进输出。
    #[serde(default)]
    pub images_on: bool,
    /// 当前选中的图片（用于变换/参数编辑）
    pub sel_image: usize,
    /// 图片页拖拽中记录的起始毫米坐标（None 表示未在拖动）
    #[serde(skip)]
    pub img_drag: Option<(f64, f64)>,
    /// 待重算的「图片参数键」+ 它最后一次变化的时间。
    ///
    /// 拖滑杆时参数每帧都在变，若每帧都重算，界面会一顿一顿（素描模式一张图
    /// 要跑一百多毫秒）。这里只记下「参数变了」，等它稳定片刻再算一次。
    #[serde(skip)]
    pub img_regen_at: Option<(u64, std::time::Instant)>,

    // —— 大纸张分页（图片超出机器幅面时拆页绘制） ——
    /// 是否启用分页绘制：把前景输出图片按可绘制幅面切成多页，逐页下发。
    #[serde(default)]
    pub tile_on: bool,
    /// 每页是否绘制 1cm 四角 L 形定位标记（尖端朝页角、开口朝内）。
    #[serde(default = "default_true")]
    pub tile_marks: bool,
    /// 当前要绘制/预览的分页序号（0 起，按行优先）。
    #[serde(default)]
    pub tile_page: usize,

    // —— 四角定位框 ——
    /// 定位框宽度（毫米）：四角 L 标记与分页幅面共用的有效绘制宽度。
    #[serde(default = "default_corner_w")]
    pub corner_w_mm: f64,
    /// 定位框高度（毫米）：默认 280，避免 L 标记贴到 A4 纸边（297）。
    #[serde(default = "default_corner_h")]
    pub corner_h_mm: f64,

    // —— 分页缩略图缓存（运行时，不入存档） ——
    /// 缓存对应的参数指纹，变化时整表重置。
    #[serde(skip)]
    pub tile_cache_key: u64,
    /// 各页图片笔迹缓存（`None` = 尚未计算）；与笔标签一起缓存，预览按笔色画。
    #[serde(skip)]
    pub tile_cache_pages: Vec<Option<Vec<(Polyline, u8)>>>,

    // —— PDF 导入 ——
    /// 最近选择的 PDF 路径
    #[serde(skip)]
    pub pdf_path: String,
    /// 该 PDF 的总页数（0 = 还没读）
    #[serde(skip)]
    pub pdf_pages: usize,
    /// 要导入第几页（**1 起**）
    #[serde(default = "one")]
    pub pdf_page: usize,
    /// 按 PDF 页面的**原始物理尺寸 1:1** 导入（不缩放到目标高度）
    #[serde(default = "default_true")]
    pub pdf_1to1: bool,

    // —— 图片生成默认参数（导入时套用） ——
    pub img_target_h: f64,
    pub img_step: u32,
    pub img_thr: f64,
    pub img_edge: f64,
    pub img_invert: bool,
    /// 默认绘制类型：0 线框 / 1 素描 / 2 连续 / 3 中心线 / 4 描边 / 5 交叉排线 / 6 网点
    pub img_draw_type: usize,
    /// 默认扫描方向：0 横向 / 1 对角 45° / 2 纵向 / 3 对角 135°
    #[serde(default)]
    pub img_scan_angle: u8,
    /// 默认连笔跨行容差（像素）
    #[serde(default = "default_merge_gap")]
    pub img_merge_gap: u32,
    /// 默认笔画顺序：0 默认 / 1 最优路径 / 2 按扫描顺序 / 3 分块蛇形
    #[serde(default)]
    pub img_order_mode: u8,
    /// 默认分块边长（像素）
    #[serde(default = "default_block_px")]
    pub img_block_px: u32,
    /// 默认素描原稿叠描遍数
    #[serde(default = "default_sketch_pressure")]
    pub img_sketch_pressure: u32,
    /// 默认素描原稿重描偏移（毫米）
    #[serde(default = "default_sketch_jitter")]
    pub img_sketch_jitter: f64,
    /// 默认素描原稿画笔线宽（毫米）
    #[serde(default = "default_sketch_pen")]
    pub img_sketch_pen: f64,
    /// 默认素描调子明暗层数
    #[serde(default = "default_hatch_levels")]
    pub img_hatch_levels: u32,
    /// 默认素描调子排线基准角（度）
    #[serde(default = "default_hatch_angle")]
    pub img_hatch_angle: f64,
    /// 默认分区重绘方向分块边长（毫米）
    #[serde(default = "default_region_block_mm")]
    pub img_region_block_mm: f64,
    /// 默认素描笔触长度（毫米）
    #[serde(default = "default_stroke_len_mm")]
    pub img_stroke_len_mm: f64,
    /// 默认素描笔触走向参考（0 轮廓 / 1 原稿 / 2 混合）
    #[serde(default = "default_stroke_follow")]
    pub img_stroke_follow: u8,

    // —— 多笔分层 ——
    /// 当前机器上的笔组（亮 → 暗）。换笔绘制时按这个顺序一支一支画。
    #[serde(default = "default_pens")]
    pub pens: Vec<PenSpec>,
    /// 多笔绘制时文字 / 边框 / 四角标记用哪支笔（**启用笔组**里的下标）。
    #[serde(default = "default_main_pen")]
    pub main_pen: usize,

    // —— 预览 ——
    /// 笔尖实际粗细（毫米），**只用于预览显示**，不参与 G-code。
    ///
    /// 预览以前一律按 1.3px 画线，而 0.5mm 的笔在 1:1 显示下是
    /// ~3px、放大后更粗。结果纸上已经排满的线，屏幕上看着稀稀拉拉，
    /// 用户自然觉得「算法画得不像」。按真实笔宽折算才看得准。
    #[serde(default = "default_sketch_pen")]
    pub preview_pen_mm: f64,

    // —— 运行边界自检 ——
    /// 生成 G-code 后先与机器幅面（`machine_w_mm`×`machine_h_mm`）比对，超界弹窗询问
    #[serde(default = "default_true")]
    pub bounds_check: bool,
    /// 超界确认弹窗当前是否可见
    #[serde(default)]
    pub show_bounds_warn: bool,
    /// 超界确认弹窗里待用户裁决的那份 G-code
    #[serde(default)]
    pub pending_run_gcode: String,
    /// 超界提示内容（哪些方向超出、超出多少）
    #[serde(default)]
    pub bounds_warn_msg: String,

    // —— 固件 / G-code ——
    pub firmware_sel: usize, // 0 grbl 1 marlin 2 ebb
    pub pen_mode_sel: usize, // 0 z 1 servo
    pub feed_rate: f64,
    pub z_speed: f64,
    pub pen_up: f64,
    pub pen_down: f64,
    pub reverse_x: bool,
    pub reverse_y: bool,
    pub swap_xy: bool,
    pub custom_start: String,
    pub custom_end: String,
    /// 坐标原点所在角：0 左上 1 右上 2 左下 3 右下
    pub origin_corner: usize,
    /// 激光模式（替代落笔）
    pub laser_on: bool,
    pub laser_power: f64,
    /// 完成后自动休眠
    pub auto_sleep: bool,
    /// 落笔后暂停（秒）
    pub tool_on_delay: f64,
    /// 抬笔前暂停（秒）
    pub tool_off_delay: f64,
    /// EBB 舵机速度
    pub ebb_servo_speed: f64,
    /// EBB 最大舵机值
    pub ebb_max_pen: f64,

    /// **XY 最小步进（毫米）**：G-code 里相邻两点的距离小于它就合并，
    /// 避免海量 0.0xmm 的微移动把机器拖慢。范围 0.1~1.0，默认 0.2。
    #[serde(default = "default_min_step")]
    pub min_step_mm: f64,

    /// 画布上是否画出 **XY 方向箭头**（蓝=X，绿=Y）。
    #[serde(default = "default_true")]
    pub axes_on: bool,
    /// 文字模块是否画出**文本框虚拟框**（虚线，仅用于定位，不打印）。
    #[serde(default = "default_true")]
    pub text_frame_on: bool,

    /// 页面外侧**标尺**（顶部 + 左侧毫米刻度），帮助把内容放到精确位置。
    #[serde(default = "default_ruler_on")]
    pub ruler_on: bool,
    /// **页面网格开关**：关闭时预览（编辑页/进度页）不画任何网格线；
    /// 打开后显示 10mm 基准网格，并按下方行/列数叠加浅色等分参考线。
    #[serde(default = "default_grid_overlay")]
    pub grid_overlay: bool,
    /// 自定义网格的**列数**（竖向分割线数量）。
    #[serde(default = "default_grid_div_x")]
    pub grid_overlay_cols: usize,
    /// 自定义网格的**行数**（横向分割线数量）。
    #[serde(default = "default_grid_div_y")]
    pub grid_overlay_rows: usize,
    /// 坐标自动查询间隔（毫秒，0=关闭）。
    ///
    /// GRBL 只在收到 `?` 时回报一次坐标、不主动推送，想让左栏坐标实时刷新
    /// 就得定时问一句。关掉可以省链路带宽。
    #[serde(default)]
    pub status_poll_ms: u64,
    /// 鼠标在页面上的位置（毫米），画布每帧写入，左栏坐标区显示用。
    #[serde(skip)]
    pub cursor_mm: Option<(f64, f64)>,
    /// 已经下发给设备线程的轮询参数 `(间隔ms, 固件)`，变了才重发。
    #[serde(skip)]
    pub status_poll_key: (u64, usize),

    // —— 以下参数与参考项目 `default.properties` 一一对应 ——
    /// 点动速度（mm/min）｜参考：jog_speed=10000
    #[serde(default = "default_jog_speed")]
    pub jog_speed: f64,
    /// 运行前自动设原点（G92 X0 Y0）｜参考：auto_set_zero=true
    #[serde(default = "default_true")]
    pub auto_set_zero: bool,
    /// 启动后自动连接机器｜参考：auto_connect=true
    #[serde(default = "default_true")]
    pub auto_connect: bool,
    /// EBB：毫米→步进电机步数｜参考：mm_to_steps=100
    #[serde(default = "default_mm_to_steps")]
    pub mm_to_steps: f64,
    /// EBB：最大抬/落笔速度（进度条最右值）｜参考：max_pen_speed=32767
    #[serde(default = "default_max_pen_speed")]
    pub max_pen_speed: f64,
    /// Marlin：Z 轴落笔高度(mm)｜参考：marlin_pen_down_mm=2
    #[serde(default = "default_marlin_pen_down")]
    pub marlin_pen_down: f64,
    /// Marlin：Z 轴抬笔高度(mm)｜参考：marlin_pen_up_mm=0
    pub marlin_pen_up: f64,
    /// Marlin：Z 轴抬落笔速度｜参考：marlin_z_speed=20000
    #[serde(default = "default_marlin_z_speed")]
    pub marlin_z_speed: f64,
    /// Marlin 舵机抬笔角度｜参考：marlin_pen_up=50
    #[serde(default = "default_marlin_servo_up")]
    pub marlin_servo_up: f64,
    /// Marlin 舵机落笔角度｜参考：marlin_pen_down=0
    pub marlin_servo_down: f64,

    /// 点动步长（毫米）
    pub jog_step: f64,
    /// 单步下发（逐行发送并等待）
    pub single_step: bool,
    /// 「清空内容」需要二次确认，防止误删整页
    #[serde(skip)]
    pub confirm_clear: bool,

    // —— 设备 ——
    pub dev_kind: usize,
    pub dev_addr: String,
    #[serde(skip)]
    pub dev_log: String,
    #[serde(skip)]
    pub dev_connected: bool,
    #[serde(skip)]
    pub device: DeviceController,
    /// 串口端口名（如 COM3）
    pub serial_port: String,
    /// 串口波特率
    pub serial_baud: u32,
    /// 扫描到的可用串口列表（可打开的）
    #[serde(skip)]
    pub serial_ports: Vec<String>,
    /// 全部登记端口 + 是否可打开（含蓝牙已配对但未建链路的离线口）
    #[serde(skip)]
    pub serial_ports_all: Vec<(String, bool)>,
    /// 每个端口的**类型与设备名**：`(端口, 标签, 是否可打开)`，
    /// 标签形如 `串口线 · USB-SERIAL CH340 (COM3)`，用来把蓝牙和有线串口线分开显示。
    #[serde(skip)]
    pub port_labels: Vec<(String, String, bool)>,
    /// 后台扫描线程的结果通道；为 `Some` 表示扫描仍在进行。
    #[serde(skip)]
    pub port_scan_rx: Option<std::sync::mpsc::Receiver<Vec<wrobot_core::serial::PortEntry>>>,
    /// 启动时先等端口扫描完成再自动连接，避免两个线程同时试开同一个口。
    #[serde(skip)]
    pub auto_connect_after_scan: bool,
    /// 机器物理绘图幅面（毫米），用于画布比例提示与归零边界
    pub machine_w_mm: f64,
    pub machine_h_mm: f64,
    /// 运行流控：是否等待固件回 `ok` 再发下一行
    pub flow_wait_ok: bool,
    /// 运行流控：运行前是否先发解锁与单位/坐标指令（GRBL 上电常为 Alarm 锁定态）
    pub flow_unlock: bool,
    /// 运行流控：等待 `ok` 的超时（毫秒）
    pub flow_ok_timeout: u64,
    /// 链路保活心跳间隔（毫秒，0=关闭）。写字机蓝牙空闲几分钟就断链，靠心跳维持。
    #[serde(default = "default_keepalive_ms")]
    pub keepalive_ms: u64,
    /// 断链后是否自动重连
    #[serde(default = "default_true")]
    pub auto_reconnect: bool,
    /// 写字机的蓝牙设备名（默认 BtWriter）。改过名字就在这里改。
    #[serde(default = "default_bt_name")]
    pub bt_name: String,
    /// 最近一次蓝牙体检 / 可见性观察的报告全文。
    #[serde(skip)]
    pub bt_report: String,
    /// 可见性观察的轮数（每轮一次主动查询）
    pub bt_watch_rounds: usize,
    /// 可见性观察的每轮间隔秒数
    pub bt_watch_every: u64,
    /// 最近一次 G-code 传输的结果摘要（从设备线程同步，状态栏显示）。
    #[serde(skip)]
    pub transfer_status: String,
    /// 设置对话框是否打开
    #[serde(skip)]
    pub settings_open: bool,
    /// 底部「实时日志」栏是否显示（窗口最下面一栏）。
    pub log_open: bool,
    /// 最近保存/打开过的工程路径；有值时 Ctrl+S 直接覆盖保存，不再弹对话框。
    #[serde(skip)]
    pub project_path: Option<std::path::PathBuf>,
    /// 界面缩放倍率（在显示器原生缩放之上再乘这个系数）。
    /// 高分屏上 UI 太小时调大；窗口放不下时调小。
    pub ui_scale: f32,
    /// 上一次打开/保存文件的目录（用于文件对话框定位）
    #[serde(skip)]
    pub last_dir: Option<std::path::PathBuf>,
    /// 机器测试：测试方框尺寸（毫米）
    pub test_frame_w: f64,
    pub test_frame_h: f64,
    /// 机器测试：自定义单行 G-code
    pub test_custom_gcode: String,

    // —— 界面 / 画布视图 ——
    /// 深色主题（false = 浅色）。
    #[serde(default)]
    pub dark_mode: bool,
    /// 画布缩放倍率：1.0 = 整页适应窗口，最高 6 倍。持久化，便于继续上次的观察位置。
    #[serde(default = "default_canvas_zoom")]
    pub canvas_zoom: f32,
    /// 画布平移（屏幕点；仅在放大后生效，运行时不保存）。
    #[serde(skip)]
    pub canvas_pan: (f32, f32),
    /// 快速上手 / 帮助窗口。
    #[serde(skip)]
    pub help_open: bool,
    /// 关于窗口。
    #[serde(skip)]
    pub about_open: bool,
    /// 缓存的任务统计（笔迹数 / 墨线 / 空移）。
    #[serde(skip)]
    pub stats: JobStats,
    /// 已应用的主题（`None` 表示尚未初始化），用于按需重设样式。
    #[serde(skip)]
    pub theme_applied: Option<bool>,

    // —— 运行态 ——
    #[serde(skip)]
    pub tab: usize, // 0文字 1表格 2格子 3绘图 4笔记 5图片 6设备 7保留 8导入导出 9运行中
    #[serde(skip)]
    pub preview: Vec<Polyline>,
    /// 与 `preview` 一一对应：每根笔迹由哪支笔绘制。
    /// `NON_PEN` = 不属于笔组（文字/边框/单笔内容），按主题墨色显示。
    #[serde(skip)]
    pub preview_pens: Vec<u8>,
    /// 开始运行那一刻被「冻结」的页面笔迹（用于运行中进度页，与主编辑页互不干扰）。
    #[serde(skip)]
    pub run_preview: Vec<Polyline>,
    /// 多笔绘制的分笔任务队列（每支笔一份 G-code）。
    #[serde(skip)]
    pub pen_jobs: Vec<PenJob>,
    /// 当前正在画第几支笔（`pen_jobs` 下标）。
    #[serde(skip)]
    pub pen_job_idx: usize,
    /// 已画完、等待用户换笔确认。
    #[serde(skip)]
    pub pen_wait: bool,
    /// 队列运行中（用来捕捉「上一支笔刚画完」这一瞬间）。
    #[serde(skip)]
    pub pen_running: bool,
    /// 当前这支笔已下发，等设备回报 running 后才能判断「画完了」。
    #[serde(skip)]
    pub pen_active: bool,
    /// 本次下发是否已经见过 `running = true`（防止刚下发、线程还没起来就误判结束）。
    #[serde(skip)]
    pub pen_seen_running: bool,
    /// 本次下发时刻（兜底：设备没连上时也会在超时后让用户重试/跳过）。
    #[serde(skip)]
    pub pen_dispatched_at: Option<std::time::Instant>,
    #[serde(skip)]
    pub gcode_text: String,
    #[serde(skip)]
    pub status: String,
}

fn align_str(a: usize) -> &'static str {
    match a {
        1 => "center",
        2 => "right",
        _ => "left",
    }
}
fn first_str(a: usize) -> &'static str {
    match a {
        1 => "CENTER",
        2 => "TAB",
        _ => "LEFT",
    }
}

impl AppState {
    pub fn new(fonts: FontManager, font_dir: String) -> Self {
        let default_font = fonts
            .first_name()
            .unwrap_or_else(|| BuiltinFont::new().name().to_string());
        let mut s = AppState {
            fonts,
            font_dir,
            page_w_mm: 210.0,
            page_h_mm: 297.0,
            paper: default_paper(),
            char_mm_at_100: 8.0,
            orientation: 0,
            module_on: [false; MODULE_COUNT],
            images_on: false,
            confirm_clear: false,
            border_on: false,
            border_margin: 10.0,
            border_style: 0,
            text: "永和九年岁在癸丑\n暮春之初会于会稽山阴之兰亭\n此地有崇山峻岭茂林修竹".to_string(),
            texts: Vec::new(),
            sel_text: 0,
            text_font: default_font.clone(),
            text_font_size: 100.0,
            text_line_gap: 4.0,
            text_space: 0.0,
            text_align: 0,
            text_first: 2,
            text_x: 10.0,
            text_y: 15.0,
            text_w: 180.0,
            text_h: 260.0,
            use_random: false,
            rand_seed: 0xC0FFEE,
            rand_x: 0.0,
            rand_y: 0.0,
            rand_scale: 0.0,
            rand_head: 0.0,
            table_rows: 5,
            table_cols: 4,
            table_cell: 14.0,
            table_font: default_font.clone(),
            table_font_size: 100.0,
            table_auto_font: false,
            table_text: "姓名\t语文\t数学\t英语\n张三\t88\t92\t79\n李四\t76\t85\t91\n王五\t95\t89\t83\n赵六\t68\t74\t90".to_string(),
            table_x: 15.0,
            table_y: 20.0,
            table_line_width: 0.35,
            table_show_h: true,
            table_show_v: true,
            grid_kind: 1,
            grid_rows: 8,
            grid_cols: 6,
            grid_cell: 18.0,
            grid_font: default_font.clone(),
            grid_font_size: 100.0,
            grid_auto_font: false,
            grid_sample: "永字八法天地玄黄宇宙洪荒日月盈昃辰宿列张寒来暑往秋收冬藏".to_string(),
            grid_x: 15.0,
            grid_y: 15.0,
            grid_show: true,
            grid_show_h: true,
            grid_show_v: true,
            grid_line_width: 0.35,
            draw_paths: Vec::new(),
            draw_path: String::new(),
            draw_target_h: 120.0,
            draw_step: 3.0,
            draw_thresh: 0.55,
            freehand_on: false,
            freehand: Vec::new(),
            notes: vec![NoteBlock {
                text: "标题：兰亭集序".to_string(),
                font: default_font.clone(),
                size_mm: 12.0,
                x: 30.0,
                y: 10.0,
                align: 1,
            }],
            images: Vec::new(),
            sel_image: 0,
            img_drag: None,
            img_regen_at: None,
            tile_on: false,
            tile_marks: true,
            tile_page: 0,
            pdf_path: String::new(),
            pdf_pages: 0,
            pdf_page: 1,
            pdf_1to1: true,
            img_target_h: 120.0,
            img_step: 3,
            img_thr: 0.55,
            img_edge: 0.25,
            img_invert: false,
            img_draw_type: 0,
            img_scan_angle: 0,
            img_merge_gap: 8,
            img_order_mode: 0,
            img_block_px: 40,
            img_sketch_pressure: 3,
            img_sketch_jitter: 0.12,
            img_sketch_pen: 0.5,
            img_hatch_levels: 6,
            img_hatch_angle: 35.0,
            img_region_block_mm: 2.5,
            img_stroke_len_mm: 3.2,
            img_stroke_follow: 2,
            pens: default_pens(),
            main_pen: default_main_pen(),
            preview_pen_mm: 0.5,
            bounds_check: true,
            show_bounds_warn: false,
            pending_run_gcode: String::new(),
            bounds_warn_msg: String::new(),
            firmware_sel: 0,
            pen_mode_sel: 0,
            feed_rate: 10000.0,
            z_speed: 10000.0,
            pen_up: 0.0,
            pen_down: 5.0,
            reverse_x: false,
            reverse_y: true,
            swap_xy: false,
            custom_start: String::new(),
            custom_end: String::new(),
            origin_corner: 0,
            laser_on: false,
            laser_power: 1000.0,
            auto_sleep: false,
            tool_on_delay: 0.0,
            tool_off_delay: 0.0,
            ebb_servo_speed: 750.0,
            ebb_max_pen: 30000.0,
            min_step_mm: default_min_step(),
            axes_on: true,
            text_frame_on: true,
            ruler_on: true,
            grid_overlay: false,
            grid_overlay_cols: 10,
            grid_overlay_rows: 14,
            status_poll_ms: 0,
            cursor_mm: None,
            status_poll_key: (0, usize::MAX),
            jog_speed: default_jog_speed(),
            auto_set_zero: true,
            auto_connect: true,
            mm_to_steps: default_mm_to_steps(),
            max_pen_speed: default_max_pen_speed(),
            marlin_pen_down: default_marlin_pen_down(),
            marlin_pen_up: 0.0,
            marlin_z_speed: default_marlin_z_speed(),
            marlin_servo_up: default_marlin_servo_up(),
            marlin_servo_down: 0.0,
            jog_step: 10.0,
            single_step: false,
            dev_kind: 0,
            dev_addr: "192.168.4.1:23".to_string(),
            dev_log: String::new(),
            dev_connected: false,
            device: DeviceController::new(),
            serial_port: "COM3".to_string(),
            serial_baud: 115200,
            serial_ports: Vec::new(),
            serial_ports_all: Vec::new(),
            port_labels: Vec::new(),
            port_scan_rx: None,
            auto_connect_after_scan: false,
            machine_w_mm: 200.0,
            machine_h_mm: 300.0,
            corner_w_mm: 200.0,
            corner_h_mm: 280.0,
            tile_cache_key: u64::MAX,
            tile_cache_pages: Vec::new(),
            flow_wait_ok: true,
            flow_unlock: true,
            flow_ok_timeout: 1200,
            keepalive_ms: default_keepalive_ms(),
            auto_reconnect: true,
            bt_name: default_bt_name(),
            bt_report: String::new(),
            transfer_status: String::new(),
            bt_watch_rounds: 12,
            bt_watch_every: 5,
            settings_open: false,
            ui_scale: 1.0,
            log_open: true,
            project_path: None,
            last_dir: None,
            test_frame_w: 100.0,
            test_frame_h: 100.0,
            test_custom_gcode: String::new(),
            dark_mode: false,
            canvas_zoom: 1.0,
            canvas_pan: (0.0, 0.0),
            help_open: false,
            about_open: false,
            stats: JobStats::default(),
            theme_applied: None,
            preview: Vec::new(),
            preview_pens: Vec::new(),
            run_preview: Vec::new(),
            pen_jobs: Vec::new(),
            pen_job_idx: 0,
            pen_wait: false,
            pen_running: false,
            pen_active: false,
            pen_seen_running: false,
            pen_dispatched_at: None,
            gcode_text: String::new(),
            status: "就绪".to_string(),
            tab: 0,
        };
        // 旧存档 / 默认值里的单文本框字段迁移成第一个 TextBox
        s.ensure_texts();
        s
    }

    /// 打开文件对话框（记住上次目录）。
    pub fn pick_file(&mut self, name: &str, exts: &[&str]) -> Option<std::path::PathBuf> {
        let p = self.file_dialog(name, exts).pick_file();
        if let Some(ref q) = p {
            self.remember_dir(q);
        }
        p
    }

    /// 保存文件对话框（记住上次目录，并给出默认文件名，用户可自行改名与选择位置）。
    pub fn save_file_as(
        &mut self,
        name: &str,
        exts: &[&str],
        default_name: &str,
    ) -> Option<std::path::PathBuf> {
        let p = self
            .file_dialog(name, exts)
            .set_file_name(default_name)
            .save_file();
        if let Some(ref q) = p {
            self.remember_dir(q);
        }
        p
    }

    /// 构造带上次目录的文件对话框。
    fn file_dialog(&self, name: &str, exts: &[&str]) -> rfd::FileDialog {
        let mut d = rfd::FileDialog::new().add_filter(name, exts);
        if let Some(dir) = &self.last_dir {
            d = d.set_directory(dir);
        }
        d
    }

    /// 记住某次文件操作的目录，供下次对话框定位。
    pub fn remember_dir(&mut self, p: &std::path::Path) {
        if let Some(dir) = p.parent() {
            self.last_dir = Some(dir.to_path_buf());
        }
    }

    /// 构造一个文字元素（x/y 直接以毫米计，px_per_mm=1）。
    /// `vcenter` 为 true 时文字块在 `(w,h)` 框内垂直居中（用于单元格）。
    fn make_text_item(
        &self,
        text: &str,
        font: &str,
        font_size: f64,
        line_gap: f64,
        space: f64,
        align: usize,
        first: usize,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        vcenter: bool,
    ) -> Item {
        let cf = ContentFormat {
            charset: Some("GBK".to_string()),
            auto_gp: false,
            gp_num: 1,
            first_line_align: Some(first_str(first).to_string()),
            ..Default::default()
        };
        let pi = PageInfo {
            line_gap,
            space,
            content_align: Some(align_str(align).to_string()),
            page_width: w,
            page_height: h,
            vcenter,
            ink_center: vcenter,
            font_size,
            font_path: vec![font.to_string()],
            ..Default::default()
        };
        Item {
            r#type: "text".to_string(),
            x,
            y,
            width: w,
            height: h,
            text: Some(text.to_string()),
            content_format: Some(serde_json::to_string(&cf).unwrap_or_default()),
            page_info: Some(pi),
            ..Default::default()
        }
    }

    /// 模块①：文字 → 单个文字元素。
    pub fn build_text_items(&self) -> Vec<Item> {
        self.build_text_tagged()
            .into_iter()
            .map(|(it, _)| it)
            .collect()
    }

    /// 模块①：文字元素 + 「本框是否应用随机变形」标记。
    ///
    /// 随机变形以前是整页开关：一开就对所有文字生效。现在按框勾选，
    /// 只对勾上的框施加抖动，其余保持工整。
    pub fn build_text_tagged(&self) -> Vec<(Item, bool)> {
        // 旧存档 / 未迁移：退回单文本框（沿用旧的「全部生效」行为）
        if self.texts.is_empty() {
            if self.text.trim().is_empty() {
                return vec![];
            }
            return vec![(
                self.make_text_item(
                    &self.text,
                    &self.text_font,
                    self.text_font_size,
                    self.text_line_gap,
                    self.text_space,
                    self.text_align,
                    self.text_first,
                    self.text_x,
                    self.text_y,
                    self.text_w,
                    self.text_h,
                    false,
                ),
                true,
            )];
        }
        let mut out = Vec::new();
        for b in &self.texts {
            if !b.on || b.text.trim().is_empty() {
                continue;
            }
            out.push((
                self.make_text_item(
                    &b.text,
                    &b.font,
                    b.size,
                    b.line_gap,
                    b.space,
                    b.align,
                    b.first,
                    b.x,
                    b.y,
                    b.w,
                    b.h,
                    false,
                ),
                b.random,
            ));
        }
        out
    }

    /// 保证至少有一个文本框；旧存档（只有 `text` 单字段）迁移成一个框。
    pub fn ensure_texts(&mut self) {
        if !self.texts.is_empty() {
            if self.sel_text >= self.texts.len() {
                self.sel_text = self.texts.len() - 1;
            }
            return;
        }
        let mut b = TextBox::default();
        b.text = self.text.clone();
        b.font = self.text_font.clone();
        b.size = self.text_font_size;
        b.line_gap = self.text_line_gap;
        b.space = self.text_space;
        b.align = self.text_align;
        b.first = self.text_first;
        b.x = self.text_x;
        b.y = self.text_y;
        b.w = self.text_w;
        b.h = self.text_h;
        self.texts.push(b);
        self.sel_text = 0;
    }

    /// 新增一个文本框（参数沿用当前框，位置自动下移避免完全重叠）。
    pub fn add_text_box(&mut self) {
        self.ensure_texts();
        let mut b = self
            .texts
            .get(self.sel_text)
            .cloned()
            .unwrap_or_default();
        b.name = format!("文本框 {}", self.texts.len() + 1);
        b.text.clear();
        b.y += 30.0;
        self.texts.push(b);
        self.sel_text = self.texts.len() - 1;
    }

    /// 把文本框收进页面内（留 10mm 边距）。
    ///
    /// 换纸张 / 载入别的工程后，框可能整块落在纸外——画布上看不见、G-code 也画到界外，
    /// 一键归位比手动拖四个滑块省事。
    pub fn clamp_text_box(&mut self, i: usize) {
        let (pw, ph) = (self.page_w_mm, self.page_h_mm);
        let m = 10.0;
        let Some(b) = self.texts.get_mut(i) else {
            return;
        };
        b.w = b.w.min((pw - 2.0 * m).max(10.0));
        b.h = b.h.min((ph - 2.0 * m).max(10.0));
        b.x = b.x.clamp(m, (pw - m - b.w).max(m));
        b.y = b.y.clamp(m, (ph - m - b.h).max(m));
    }

    /// 删除当前文本框（至少保留一个）。
    pub fn remove_text_box(&mut self) {
        if self.texts.len() <= 1 {
            if let Some(b) = self.texts.first_mut() {
                b.text.clear();
            }
            return;
        }
        let i = self.sel_text.min(self.texts.len() - 1);
        self.texts.remove(i);
        if self.sel_text >= self.texts.len() {
            self.sel_text = self.texts.len() - 1;
        }
        for (k, b) in self.texts.iter_mut().enumerate() {
            b.name = format!("文本框 {}", k + 1);
        }
    }

    /// 按比例缩放某个文本框：字号 / 行距 / 字间距 / 框宽高一起变，围绕框中心。
    ///
    /// 画布上「不按 Ctrl 滚轮」走这里（Ctrl+滚轮留给画布视图缩放）。
    pub fn scale_text_box(&mut self, i: usize, f: f64) {
        let f = f.clamp(0.2, 5.0);
        let (pw, ph) = (self.page_w_mm, self.page_h_mm);
        let Some(b) = self.texts.get_mut(i) else {
            return;
        };
        let cx = b.x + b.w * 0.5;
        let cy = b.y + b.h * 0.5;
        b.size = (b.size * f).clamp(6.0, 600.0);
        b.line_gap = (b.line_gap * f).clamp(0.5, 50.0);
        b.space = (b.space * f).clamp(0.0, 50.0);
        b.w = (b.w * f).clamp(10.0, (pw - 2.0).max(10.0));
        b.h = (b.h * f).clamp(10.0, (ph - 2.0).max(10.0));
        b.x = (cx - b.w * 0.5).clamp(0.0, (pw - b.w).max(0.0));
        b.y = (cy - b.h * 0.5).clamp(0.0, (ph - b.h).max(0.0));
    }

    /// 某字库「典型字身高度 / em」的比例，用于「字号自适应格高」按真实字身计算。
    /// 取不到时回落 0.7（手写体常见值）。
    pub fn font_ink_ratio(&self, name: &str) -> f64 {
        match self.fonts.get(name) {
            Some(f) => {
                let r = StrokeFont::ink_height_ratio(f);
                if r > 0.05 && r < 3.0 {
                    r
                } else {
                    0.7
                }
            }
            None => 0.7,
        }
    }

    /// 某字库「典型字身宽度 / em」的比例，用于估算单元格能放下几个字。
    pub fn font_ink_width_ratio(&self, name: &str) -> f64 {
        match self.fonts.get(name) {
            Some(f) => {
                let em = StrokeFont::em(f).max(1.0) as f64;
                let r = f.ink_metrics().width() as f64 / em;
                if r > 0.05 && r < 3.0 {
                    r
                } else {
                    0.6
                }
            }
            None => 0.6,
        }
    }

    /// 让「典型字身高度 = `cell_h × fill`」所需的字号（基准 100）。
    fn auto_font_size(&self, font: &str, cell_h: f64, fill: f64) -> f64 {
        let ratio = self.font_ink_ratio(font);
        let fs = 100.0 * (cell_h * fill) / (self.char_mm_at_100 * ratio);
        fs.clamp(10.0, 600.0)
    }

    /// 表格单元格自适应字号：既让字身尽量填满格子，又保证**最多的字不会换行**。
    ///
    /// 单线手写字库的「前进宽度 = em」远大于字身宽度（实测约 1.9 倍），
    /// 若只按字身高度反推字号，两字单元格会被排版逻辑判定超宽而强制换行。
    /// 这里同时用「字身宽度」和「前进宽度上限」两把尺子取小值。
    fn table_auto_font_size(&self, font: &str, cell_w: f64, max_chars: usize) -> f64 {
        let n = max_chars.clamp(1, 8) as f64;
        let wr = self.font_ink_width_ratio(font).max(0.05);
        // ① 按字身宽度：每字墨迹宽 ≈ 可用宽度的 86%
        let by_ink = 100.0 * (cell_w * 0.86 / n / wr) / self.char_mm_at_100;
        // ② 按前进宽度：n 个字的前进宽度不得超过格宽（否则换行）
        let by_adv = 100.0 * (cell_w * 0.95 / n) / self.char_mm_at_100;
        by_ink.min(by_adv).clamp(10.0, 600.0)
    }

    /// 表格内容里「单元格最多有几个字」（用于自适应字号与换行判断）。
    fn table_max_cell_chars(&self) -> usize {
        let mut m = 1usize;
        for line in self.table_text.lines() {
            let cells: Vec<&str> = if line.contains('\t') {
                line.split('\t').collect()
            } else if line.contains(',') {
                line.split(',').collect()
            } else {
                line.split_whitespace().collect()
            };
            for c in cells {
                m = m.max(c.chars().count());
            }
        }
        m
    }

    /// 模块②：表格 → 边框线条 + 每格文本。
    pub fn build_table_items(&self) -> Vec<Item> {
        let mut items = Vec::new();
        let rows = self.table_rows.max(1);
        let cols = self.table_cols.max(1);
        let cw = self.table_cell;
        let ch = self.table_cell;
        let x0 = self.table_x;
        let y0 = self.table_y;
        // 边框：整行/整列一条线，避免相邻格共享边被重复描两遍
        let mut lines: Vec<Vec<(f64, f64)>> = Vec::new();
        if self.table_show_h {
            for j in 0..=rows {
                let gy = y0 + ch * j as f64;
                lines.push(vec![(x0, gy), (x0 + cw * cols as f64, gy)]);
            }
        }
        if self.table_show_v {
            for i in 0..=cols {
                let gx = x0 + cw * i as f64;
                lines.push(vec![(gx, y0), (gx, y0 + ch * rows as f64)]);
            }
        }
        lines = wrobot_core::render::thicken_lines(&lines, self.table_line_width);
        items.push(Item {
            r#type: "line".to_string(),
            shapes: Some(lines),
            ..Default::default()
        });
        // 每格文本：按真实字身高度自适应，避免字糊成一团或只有格高的零头
        let fs = if self.table_auto_font {
            self.table_auto_font_size(&self.table_font, cw, self.table_max_cell_chars())
        } else {
            self.table_font_size
        };
        for (r, line) in self.table_text.lines().enumerate() {
            if r >= rows {
                break;
            }
            // 修复：原先 if/else 两个分支都按 '\t' 切分（死代码）。
            // 现按优先级支持 tab → 逗号 → 空格 分隔。
            let cells: Vec<&str> = if line.contains('\t') {
                line.split('\t').collect()
            } else if line.contains(',') {
                line.split(',').collect()
            } else {
                line.split_whitespace().collect()
            };
            for (c, cell) in cells.iter().enumerate() {
                if c >= cols || cell.trim().is_empty() {
                    continue;
                }
                // 以单元格左上角为锚点，文字在单元格内水平居中 + 垂直居中
                let cx = x0 + cw * c as f64;
                let cy = y0 + ch * r as f64;
                items.push(self.make_text_item(
                    cell,
                    &self.table_font,
                    fs,
                    0.0,
                    0.0,
                    1, // 居中
                    0,
                    cx,
                    cy,
                    cw,
                    ch,
                    true, // 垂直居中
                ));
            }
        }
        items
    }

    /// 模块③：格子 → 网格线条 + 可选示范字。
    pub fn build_grid_items(&self) -> Vec<Item> {
        let mut items = Vec::new();
        if !self.grid_show {
            return items;
        }
        let rows = self.grid_rows.max(1);
        let cols = self.grid_cols.max(1);
        let cw = self.grid_cell;
        let ch = self.grid_cell;
        let x0 = self.grid_x;
        let y0 = self.grid_y;
        let kind = match self.grid_kind {
            0 => GridKind::Blank,
            1 => GridKind::Tian,
            2 => GridKind::Mi,
            _ => GridKind::Hui,
        };
        // 行列线：整行 / 整列各一条（不按格逐格画框，避免共享边被描两遍）
        let mut lines: Vec<Vec<(f64, f64)>> = Vec::new();
        if self.grid_show_h {
            for r in 0..=rows {
                let gy = y0 + ch * r as f64;
                lines.push(vec![(x0, gy), (x0 + cw * cols as f64, gy)]);
            }
        }
        if self.grid_show_v {
            for c in 0..=cols {
                let gx = x0 + cw * c as f64;
                lines.push(vec![(gx, y0), (gx, y0 + ch * rows as f64)]);
            }
        }
        // 格型内部辅助线（田字 / 米字 / 回宫格）
        for r in 0..rows {
            for c in 0..cols {
                let gx = x0 + cw * c as f64;
                let gy = y0 + ch * r as f64;
                match kind {
                    GridKind::Blank => {}
                    GridKind::Tian => {
                        if self.grid_show_v {
                            lines.push(vec![(gx + cw / 2.0, gy), (gx + cw / 2.0, gy + ch)]);
                        }
                        if self.grid_show_h {
                            lines.push(vec![(gx, gy + ch / 2.0), (gx + cw, gy + ch / 2.0)]);
                        }
                    }
                    GridKind::Mi => {
                        if self.grid_show_v {
                            lines.push(vec![(gx + cw / 2.0, gy), (gx + cw / 2.0, gy + ch)]);
                        }
                        if self.grid_show_h {
                            lines.push(vec![(gx, gy + ch / 2.0), (gx + cw, gy + ch / 2.0)]);
                        }
                        // 斜叉既非纯横也非纯竖，仅在横竖都显示时给出
                        if self.grid_show_h && self.grid_show_v {
                            lines.push(vec![(gx, gy), (gx + cw, gy + ch)]);
                            lines.push(vec![(gx + cw, gy), (gx, gy + ch)]);
                        }
                    }
                    GridKind::Hui => {
                        let k = cw * 0.18;
                        let inner = rect(gx + k, gy + k, cw - 2.0 * k, ch - 2.0 * k);
                        for (i, p) in inner.into_iter().enumerate() {
                            // rect 顺序：上(横) 右(竖) 下(横) 左(竖)
                            let horizontal = i % 2 == 0;
                            if (horizontal && self.grid_show_h) || (!horizontal && self.grid_show_v)
                            {
                                lines.push(p.pts);
                            }
                        }
                    }
                }
            }
        }
        lines = wrobot_core::render::thicken_lines(&lines, self.grid_line_width);
        items.push(Item {
            r#type: "line".to_string(),
            shapes: Some(lines),
            ..Default::default()
        });
        // 示范字：逐格填充，字心精确落在格心
        if !self.grid_sample.trim().is_empty() {
            let chars: Vec<char> = self.grid_sample.chars().filter(|c| !c.is_whitespace()).collect();
            // 自适应时按「真实字身高度 = 格高 0.8」反推字号
            let fs = if self.grid_auto_font {
                self.auto_font_size(&self.grid_font, ch, 0.8)
            } else {
                self.grid_font_size
            };
            for (idx, &ch0) in chars.iter().enumerate() {
                let r = idx / cols;
                let c = idx % cols;
                if r >= rows {
                    break;
                }
                // 示范字在单元格内水平居中 + 垂直居中 + 逐字墨迹居中（字心 == 格心）
                let gx = x0 + cw * c as f64;
                let gy = y0 + ch * r as f64;
                items.push(self.make_text_item(
                    &ch0.to_string(),
                    &self.grid_font,
                    fs,
                    0.0,
                    0.0,
                    1,
                    0,
                    gx,
                    gy,
                    cw,
                    ch,
                    true,
                ));
            }
        }
        items
    }

    /// 模块④：绘图 → 由 `draw_paths` 直接生成形状元素。
    pub fn build_draw_items(&self) -> Vec<Item> {
        if self.draw_paths.is_empty() {
            return vec![];
        }
        let shapes: Vec<Vec<(f64, f64)>> = self.draw_paths.iter().map(|p| p.pts.clone()).collect();
        vec![Item {
            r#type: "line".to_string(),
            shapes: Some(shapes),
            ..Default::default()
        }]
    }

    /// 模块⑤：笔记 → 每个文本块一个文字元素。
    pub fn build_notes_items(&self) -> Vec<Item> {
        let mut items = Vec::new();
        for b in &self.notes {
            if b.text.trim().is_empty() {
                continue;
            }
            let w = (self.page_w_mm - b.x - 5.0).max(10.0);
            items.push(self.make_text_item(
                &b.text,
                &b.font,
                (b.size_mm / self.char_mm_at_100) * 100.0,
                2.0,
                0.0,
                b.align,
                0,
                b.x,
                b.y,
                w,
                30.0,
                false,
            ));
        }
        items
    }

    /// 页面边框（对应源程序 edge 模块）：实线 / 虚线 / 双线。
    pub fn build_border_items(&self) -> Vec<Item> {
        if !self.border_on {
            return vec![];
        }
        let m = self.border_margin.max(0.0);
        let w = (self.page_w_mm - 2.0 * m).max(1.0);
        let h = (self.page_h_mm - 2.0 * m).max(1.0);
        let lines: Vec<Vec<(f64, f64)>> = match self.border_style {
            1 => dashed_rect(m, m, w, h, 6.0).into_iter().map(|p| p.pts).collect(),
            2 => {
                let mut v: Vec<Vec<(f64, f64)>> =
                    rect(m, m, w, h).into_iter().map(|p| p.pts).collect();
                let k = 2.0;
                v.extend(
                    rect(m + k, m + k, w - 2.0 * k, h - 2.0 * k)
                        .into_iter()
                        .map(|p| p.pts),
                );
                v
            }
            _ => rect(m, m, w, h).into_iter().map(|p| p.pts).collect(),
        };
        vec![Item {
            r#type: "line".to_string(),
            shapes: Some(lines),
            ..Default::default()
        }]
    }

    /// 判断某内容模块是否并入当前页面。
    ///
    /// 规则（修复「设备页内容全部叠在一起」+ BUG #59「切换模式旧内容残留」）：
    /// - 显式勾选（`module_on`）的模块始终并入；
    /// - 停留在**内容页**（文字/表格/格子/绘图/笔记）时，并入该页对应模块；
    /// - 停留在**非内容页**（图片 / 设备 / 测试 / 导入导出 / 运行中）时，
    ///   **不再**自动带入「最近停留过的内容模块」——必须显式勾选「并入当前页面」
    ///   才会显示。否则切到图片/设备页后，上一个模块的笔迹仍残留在画布上，
    ///   既干扰定位、也让用户误以为没切换成功。
    ///
    /// 旧实现在非内容页返回「全部并入」，导致五个模块同时按各自默认坐标渲染、
    /// 全都堆在页面左上角互相重叠，而且左侧勾选框取消也无效——看到的还是全部。
    pub fn module_active(&self, idx: usize) -> bool {
        if idx >= MODULE_COUNT {
            return false;
        }
        if self.module_on[idx] {
            return true;
        }
        // 仅在「停留在本内容编辑页」时，自动并入当前页模块；
        // 切到其它页（含图片/设备/测试/导入导出/运行中）不再自动带出旧内容。
        self.tab < MODULE_COUNT && self.tab == idx
    }

    /// 汇总元素到工程对象：只并入「当前模块 + 显式开启的模块」，避免多模块坐标重叠。
    pub fn compose_doc(&self) -> Document {
        let mut items = self.build_border_items();
        if self.module_active(0) {
            items.extend(self.build_text_items());
        }
        if self.module_active(1) {
            items.extend(self.build_table_items());
        }
        if self.module_active(2) {
            items.extend(self.build_grid_items());
        }
        if self.module_active(3) {
            items.extend(self.build_draw_items());
        }
        if self.module_active(4) {
            items.extend(self.build_notes_items());
        }
        // 工程存档里 width/height 以厘米为单位（与原始 .json 一致），故 /10。
        Document {
            orientation: self.orientation,
            width: self.page_w_mm / 10.0,
            height: Some(self.page_h_mm / 10.0),
            axes_pos_x: 0.0,
            axes_pos_y: 0.0,
            items,
            ..Default::default()
        }
    }

    /// 统一的「元素 → 笔迹折线」管线（原先在两处重复实现）。
    ///
    /// 元素按模块顺序收集，并带上「该元素是否允许随机变形」的标记：
    /// 文字框按各自的 `random` 勾选决定，其余模块沿用全局开关。
    ///
    /// 返回的每根笔迹都带笔标签（[`NON_PEN`] = 文字/边框/单笔内容）。
    /// 多笔输出时标签会解析成**启用笔组**里的下标，并加上该笔的套准偏移。
    pub fn collect_tagged_polylines(&self, random: bool) -> Vec<(Polyline, u8)> {
        let stack = crate::fontmgr::build_stack(&self.fonts, &self.compose_doc());
        let mut opts = LayoutOptions {
            px_per_mm: 1.0,
            char_mm_at_100: self.char_mm_at_100,
            tab_indent_chars: 2.0,
            random: Default::default(),
        };
        if random {
            opts.random.x_mm = self.rand_x;
            opts.random.y_mm = self.rand_y;
            opts.random.scale = self.rand_scale;
            opts.random.line_head_mm = self.rand_head;
        }
        let mut rng = StdRng::seed_from_u64(self.rand_seed);
        // (元素, 是否跟随随机变形)
        let mut tagged: Vec<(Item, bool)> = Vec::new();
        for it in self.build_border_items() {
            tagged.push((it, false));
        }
        if self.module_active(0) {
            tagged.extend(self.build_text_tagged());
        }
        if self.module_active(1) {
            tagged.extend(self.build_table_items().into_iter().map(|i| (i, true)));
        }
        if self.module_active(2) {
            tagged.extend(self.build_grid_items().into_iter().map(|i| (i, true)));
        }
        if self.module_active(3) {
            tagged.extend(self.build_draw_items().into_iter().map(|i| (i, true)));
        }
        if self.module_active(4) {
            tagged.extend(self.build_notes_items().into_iter().map(|i| (i, true)));
        }
        let mut all: Vec<(Polyline, u8)> = Vec::new();
        for (it, allow_random) in &tagged {
            if it.is_text() {
                let use_rng = random && *allow_random;
                let lay = if use_rng {
                    wrobot_core::layout::layout_item_rng(it, &stack, &opts, Some(&mut rng))
                } else {
                    wrobot_core::layout::layout_item(it, &stack, &opts)
                };
                all.extend(
                    wrobot_core::render::render(&lay.placements, &stack)
                        .into_iter()
                        .map(|p| (p, NON_PEN)),
                );
            } else if it.is_shape() {
                if let Some(shapes) = &it.shapes {
                    for sp in shapes {
                        if sp.len() >= 2 {
                            all.push((Polyline::new(sp.clone()), NON_PEN));
                        }
                    }
                }
            }
        }
        // 前景图片（可打印）：模块开关 + 每张图自己的「输出」开关都打开才并入
        if self.images_active() {
            for img in &self.images {
                if img.background || !img.out {
                    continue;
                }
                all.extend(self.image_polylines_tagged(img));
            }
        }
        // 多笔输出时，不属于笔组的内容（文字/边框）由「主笔」来画；
        // 同时把每支笔各自的套准偏移补上，让预览与真正落笔一致。
        if self.multi_pen_active() {
            let main = self.main_pen_idx();
            let specs = self.active_pens();
            for (pl, tag) in &mut all {
                if *tag == NON_PEN {
                    *tag = main as u8;
                }
                if let Some(spec) = specs.get(*tag as usize) {
                    let (dx, dy) = (spec.offset_mm[0], spec.offset_mm[1]);
                    if dx != 0.0 || dy != 0.0 {
                        for p in &mut pl.pts {
                            p.0 += dx;
                            p.1 += dy;
                        }
                    }
                }
            }
        }
        all
    }

    /// 只取笔迹（绝大多数调用方不关心笔标签）。
    pub fn collect_polylines(&self, random: bool) -> Vec<Polyline> {
        self.collect_tagged_polylines(random)
            .into_iter()
            .map(|(p, _)| p)
            .collect()
    }

    /// 是否有「多笔分层」的图片参与输出。
    ///
    /// 只有真正要输出、且启用了多笔、且至少有两支笔时才成立；否则一切照旧走单笔。
    pub fn multi_pen_active(&self) -> bool {
        self.images_active()
            && self.active_pens().len() >= 2
            && self
                .images
                .iter()
                .any(|im| !im.background && im.out && im.multi_pen)
    }

    /// 主笔在**启用笔组**里的下标（夹到有效范围）。
    pub fn main_pen_idx(&self) -> usize {
        let n = self.active_pens().len();
        if n == 0 {
            0
        } else {
            self.main_pen.min(n - 1)
        }
    }

    /// 取笔色；越界或 `NON_PEN` 时返回 `None`（用主题墨色）。
    pub fn pen_color(&self, tag: u8) -> Option<[u8; 3]> {
        if tag == NON_PEN {
            return None;
        }
        self.active_pens().get(tag as usize).map(|p| p.color)
    }

    /// 取笔宽；越界或 `NON_PEN` 时返回 `None`（用预览笔宽）。
    pub fn pen_width_mm(&self, tag: u8) -> Option<f64> {
        if tag == NON_PEN {
            return None;
        }
        self.active_pens()
            .get(tag as usize)
            .map(|p| p.width_mm.clamp(0.15, 1.6))
    }

    /// 第 i 张图的页面毫米笔迹（不判断是否输出）。
    fn image_polylines_idx(&self, i: usize) -> Vec<Polyline> {
        match self.images.get(i) {
            Some(img) => self.image_polylines(img),
            None => Vec::new(),
        }
    }

    /// 全部前景图片的页面毫米笔迹（**仅用于画布显示**，不参与输出）。
    ///
    /// 图片页需要能看到图才能拖动/缩放，但「看得见」不等于「要打印」，
    /// 所以显示走这条路，输出走 [`Self::images_active`]。
    pub fn foreground_polylines(&self) -> Vec<Polyline> {
        let mut v = Vec::new();
        for (i, img) in self.images.iter().enumerate() {
            if img.background {
                continue;
            }
            v.extend(self.image_polylines_idx(i));
        }
        v
    }

    /// 图片是否并入当前页面：**只看显式勾选**。
    ///
    /// 停不停在图片页不再影响输出——否则「点一下图片模块就把所有图并进来」，
    /// 而多数时候用户只是想进去调整某一张。要并入就勾左侧的「图片」。
    pub fn images_active(&self) -> bool {
        self.images_on
    }

    /// 单张图片是否参与输出（模块开关 + 本图开关都要开，且不是背景参考图）。
    pub fn image_outputs(&self, i: usize) -> bool {
        match self.images.get(i) {
            Some(im) => !im.background && im.out,
            None => false,
        }
    }

    /// 把单张图片的基础笔迹（未变换，尺寸 natural_w×natural_h）按
    /// x/y/scale/rotation/flip 变换到页面毫米坐标。
    fn image_polylines(&self, img: &ImageObject) -> Vec<Polyline> {
        self.image_polylines_tagged(img)
            .into_iter()
            .map(|(p, _)| p)
            .collect()
    }

    /// 同 [`Self::image_polylines`]，但每根笔迹带上「由哪支笔绘制」。
    ///
    /// 单笔图片一律返回 [`NON_PEN`]：它应该跟文字一样用「主笔」，
    /// 而不是在多笔队列里被误当成 HB 那一层。
    fn image_polylines_tagged(&self, img: &ImageObject) -> Vec<(Polyline, u8)> {
        if img.base.is_empty() || img.natural_w <= 0.0 || img.natural_h <= 0.0 {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(img.base.len());
        for (k, pl) in img.base.iter().enumerate() {
            let tag = if img.multi_pen {
                img.base_pens.get(k).copied().unwrap_or(0)
            } else {
                NON_PEN
            };
            let pts: Vec<(f64, f64)> = pl
                .pts
                .iter()
                .map(|&(px, py)| img.to_page(px, py))
                .collect();
            if pts.len() >= 2 {
                out.push((Polyline::new(pts), tag));
            }
        }
        out
    }

    /// 全部背景参考图片的页面毫米笔迹（浅色显示，不打印）。
    pub fn background_image_polylines(&self) -> Vec<Polyline> {
        let mut v = Vec::new();
        for img in &self.images {
            if img.background {
                v.extend(self.image_polylines(img));
            }
        }
        v
    }

    /// 定位框尺寸（毫米）：用户设定值夹到页面与机器幅面内，至少 10mm。
    ///
    /// 四角 L 标记与分页幅面都用这一个尺寸——两处必须完全一致，
    /// 否则换纸拼接时上一页的角标对不上下一页的内容。
    pub fn corner_frame_size(&self) -> (f64, f64) {
        (
            self.corner_w_mm
                .min(self.page_w_mm)
                .min(self.machine_w_mm)
                .max(10.0),
            self.corner_h_mm
                .min(self.page_h_mm)
                .min(self.machine_h_mm)
                .max(10.0),
        )
    }

    /// 机器原点 (0,0) 落在纸面的哪个位置（页面坐标，y 向下）；`(0,0)` 就是纸的左上角。
    ///
    /// `origin_corner` 和 `reverse_x/reverse_y` **都**能镜像坐标，叠加后才是真实
    /// 结果。写反了会互相抵消——比如把「原点角」选成左下、而「反转 Y」默认又是
    /// 开着的，两次镜像抵消，整页反而上下颠倒。四角 L 本身完全对称，画完也看不出
    /// 来，只能等正文出错才发现。所以必须有个地方把真实结果算出来给人看。
    pub fn origin_page_point(&self) -> (f64, f64) {
        let mx = (self.origin_corner & 1 != 0) ^ self.reverse_x; // 镜像的是「机器 X」
        let my = (self.origin_corner & 2 != 0) ^ self.reverse_y; // 镜像的是「机器 Y」
        let (pw, ph) = (self.page_w_mm, self.page_h_mm);
        if self.swap_xy {
            // 对调后：机器 X 装的是页面 Y，机器 Y 装的是页面 X
            (if my { pw } else { 0.0 }, if mx { ph } else { 0.0 })
        } else {
            (if mx { pw } else { 0.0 }, if my { ph } else { 0.0 })
        }
    }

    /// 单页实际可绘制尺寸：即定位框尺寸（已夹到页面与机器幅面的较小者）。
    pub fn effective_draw_size(&self) -> (f64, f64) {
        self.corner_frame_size()
    }

    /// 所有「参与输出」的前景图片在页面坐标中的包围盒 `(min_x,min_y,max_x,max_y)`。
    pub fn output_image_bounds(&self) -> Option<(f64, f64, f64, f64)> {
        if !self.images_active() {
            return None;
        }
        let (mut min_x, mut min_y) = (f64::INFINITY, f64::INFINITY);
        let (mut max_x, mut max_y) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        let mut found = false;
        for (i, img) in self.images.iter().enumerate() {
            if !self.image_outputs(i) || img.natural_w <= 0.0 || img.natural_h <= 0.0 {
                continue;
            }
            for (x, y) in img.corners() {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
                found = true;
            }
        }
        found.then_some((min_x, min_y, max_x, max_y))
    }

    /// 当前图片输出对应的分页布局；没有可输出图片时返回 `None`。
    pub fn tile_layout(&self) -> Option<TileLayout> {
        let (min_x, min_y, max_x, max_y) = self.output_image_bounds()?;
        let (tw, th) = self.effective_draw_size();
        let eps = 1e-6;
        // 图块与页面网格对齐：第 0 块是 0..tw，第 -1 块是 -tw..0。
        let start_col = (min_x / tw).floor() as i64;
        let end_col = ((max_x - eps) / tw).floor() as i64;
        let start_row = (min_y / th).floor() as i64;
        let end_row = ((max_y - eps) / th).floor() as i64;
        Some(TileLayout {
            width_mm: tw,
            height_mm: th,
            cols: (end_col - start_col + 1).max(1) as usize,
            rows: (end_row - start_row + 1).max(1) as usize,
            start_col,
            start_row,
        })
    }

    /// 第 `page` 页的图片笔迹（不含四角标记）：裁剪到该页范围、平移到页左上角。
    #[cfg(test)]
    pub fn tiled_page_content_polylines(&self, page: usize) -> Vec<Polyline> {
        self.tiled_page_content_tagged_polylines(page)
            .into_iter()
            .map(|(p, _)| p)
            .collect()
    }

    /// 与 [`Self::tiled_page_polylines`] 同源，但带上「由哪支笔绘制」。
    pub fn tiled_page_content_tagged_polylines(&self, page: usize) -> Vec<(Polyline, u8)> {
        let Some(layout) = self.tile_layout() else {
            return Vec::new();
        };
        let Some((ox, oy)) = layout.origin(page) else {
            return Vec::new();
        };
        let multi = self.multi_pen_active();
        let specs = self.active_pens();
        let mut out = Vec::new();
        for (i, img) in self.images.iter().enumerate() {
            if !self.image_outputs(i) || img.natural_w <= 0.0 || img.natural_h <= 0.0 {
                continue;
            }
            for (mut pl, tag) in self.image_polylines_tagged(img) {
                // 套准偏移必须在裁剪前补：否则贴边的笔迹会被裁掉一截。
                if multi {
                    if let Some(spec) = specs.get(tag as usize) {
                        let (dx, dy) = (spec.offset_mm[0], spec.offset_mm[1]);
                        if dx != 0.0 || dy != 0.0 {
                            for p in &mut pl.pts {
                                p.0 += dx;
                                p.1 += dy;
                            }
                        }
                    }
                }
                for run in clip_polyline_to_rect(
                    &pl.pts,
                    ox,
                    oy,
                    ox + layout.width_mm,
                    oy + layout.height_mm,
                ) {
                    out.push((
                        Polyline::new(run.into_iter().map(|(x, y)| (x - ox, y - oy)).collect()),
                        tag,
                    ));
                }
            }
        }
        out
    }

    /// 第 `page` 页的全部笔迹（带笔标签）：图片内容 +（可选）1cm 四角定位 L 标记。
    pub fn tiled_page_tagged_polylines(&self, page: usize) -> Vec<(Polyline, u8)> {
        let mut out = self.tiled_page_content_tagged_polylines(page);
        if let Some(layout) = self.tile_layout() {
            if self.tile_marks {
                out.extend(
                    corner_mark_polylines(
                        layout.width_mm,
                        layout.height_mm,
                        10.0,
                        (false, true),
                    )
                    .into_iter()
                    .map(|p| (p, NON_PEN)),
                );
            }
        }
        out
    }

    /// 第 `page` 页的全部笔迹：图片内容 +（可选）1cm 四角定位 L 标记。
    pub fn tiled_page_polylines(&self, page: usize) -> Vec<Polyline> {
        self.tiled_page_tagged_polylines(page)
            .into_iter()
            .map(|(p, _)| p)
            .collect()
    }

    /// 当前分页序号（自动夹到有效范围内）。
    pub fn tile_page_clamped(&self) -> usize {
        match self.tile_layout() {
            Some(layout) => self.tile_page.min(layout.count().saturating_sub(1)),
            None => 0,
        }
    }

    /// 分页内容缓存指纹：布局 + 每张输出图片的几何与生成参数。
    ///
    /// 缩略图条一次要画所有页，如果每帧都重新裁剪 + 平移整张图，页数一多
    /// 界面就会明显卡顿。任一影响内容的参数变化时指纹变化，整表重算。
    pub fn tile_cache_key(&self) -> u64 {
        let Some(layout) = self.tile_layout() else {
            return 0;
        };
        let mut h = 0xcbf2_9ce4_8422_2325u64; // FNV-1a
        let mut add = |v: u64| {
            h ^= v;
            h = h.wrapping_mul(0x100_0000_01b3);
        };
        add(layout.width_mm.to_bits());
        add(layout.height_mm.to_bits());
        add(layout.cols as u64);
        add(layout.rows as u64);
        add(layout.start_col as u64);
        add(layout.start_row as u64);
        add(self.images.len() as u64);
        add(self.multi_pen_active() as u64);
        for p in self.active_pens() {
            add(p.offset_mm[0].to_bits());
            add(p.offset_mm[1].to_bits());
        }
        for (i, img) in self.images.iter().enumerate() {
            if !self.image_outputs(i) {
                continue;
            }
            for v in [
                img.x,
                img.y,
                img.scale,
                img.rotation,
                img.natural_w,
                img.natural_h,
            ] {
                add(v.to_bits());
            }
            add(img.flip_h as u64);
            add(img.flip_v as u64);
            add(img.gen_key);
            add(img.base.len() as u64);
        }
        h
    }

    /// 确认分页缓存与当前参数一致（不一致就清空，按需重算）。
    pub fn ensure_tile_cache(&mut self) {
        let key = self.tile_cache_key();
        let count = self.tile_layout().map_or(0, |l| l.count());
        if self.tile_cache_key == key && self.tile_cache_pages.len() == count {
            return;
        }
        self.tile_cache_key = key;
        self.tile_cache_pages = vec![None; count];
    }

    /// 取第 `page` 页的图片笔迹与笔标签（带缓存；不含四角标记）。
    pub fn tile_page_tagged_cached(&mut self, page: usize) -> Vec<(Polyline, u8)> {
        self.ensure_tile_cache();
        if page >= self.tile_cache_pages.len() {
            return Vec::new();
        }
        if self.tile_cache_pages[page].is_none() {
            let p = self.tiled_page_content_tagged_polylines(page);
            self.tile_cache_pages[page] = Some(p);
        }
        self.tile_cache_pages[page].clone().unwrap_or_default()
    }

    /// 一次性补齐所有分页的缓存（缩略图条要展示每一页）。
    pub fn fill_tile_cache(&mut self) {
        self.ensure_tile_cache();
        for page in 0..self.tile_cache_pages.len() {
            if self.tile_cache_pages[page].is_none() {
                let p = self.tiled_page_content_tagged_polylines(page);
                self.tile_cache_pages[page] = Some(p);
            }
        }
    }

    /// 生成第 `page` 页的分页 G-code（页面尺寸按单页幅面、强制以当前位置为零点）。
    pub fn generate_tile_gcode(&self, page: usize) -> Option<String> {
        let layout = self.tile_layout()?;
        if page >= layout.count() {
            return None;
        }
        let paths = self.tiled_page_polylines(page);
        if paths.is_empty() {
            return None;
        }
        let mut opts = self.gcode_options();
        opts.page_w = layout.width_mm;
        opts.page_h = layout.height_mm;
        // 每页都是「先把笔放到该页左上角 → 清零 → 画」，不能沿用绝对坐标。
        opts.use_current_origin = true;
        let mut g = format!(
            "; 大纸张分页：第 {}/{} 页（{} 列 × {} 行，单页 {:.0}×{:.0}mm）\n",
            page + 1,
            layout.count(),
            layout.cols,
            layout.rows,
            layout.width_mm,
            layout.height_mm
        );
        g.push_str(&wrobot_core::generate(&paths, &opts));
        Some(g)
    }

    // ── 多笔绘制队列 ─────────────────────────────────────────────

    /// 当前页面（分页时为当前页）的多笔任务：按启用笔组顺序，每支笔一份 G-code。
    ///
    /// 每份任务都是独立、完整、带锚点的 G-code：上一支画完会 `G0 X0 Y0` 回到页面零点，
    /// 下一支开头再 `G92 X0 Y0` 重新认原点，所以换笔不会累积坐标漂移。
    pub fn build_pen_jobs(&self) -> Vec<PenJob> {
        if !self.multi_pen_active() {
            return Vec::new();
        }
        let specs = self.active_pens();
        if specs.len() < 2 {
            return Vec::new();
        }
        let mut gopts = self.gcode_options();
        let (tagged, tile) = if self.tile_on {
            let Some(layout) = self.tile_layout() else {
                return Vec::new();
            };
            let page = self.tile_page_clamped();
            gopts.page_w = layout.width_mm;
            gopts.page_h = layout.height_mm;
            gopts.use_current_origin = true;
            (
                self.tiled_page_tagged_polylines(page),
                Some((layout, page)),
            )
        } else {
            (self.collect_tagged_polylines(self.use_random), None)
        };
        if tagged.is_empty() {
            return Vec::new();
        }
        let main = self.main_pen_idx() as u8;
        let last = (specs.len() - 1) as u8;
        let mut groups: std::collections::BTreeMap<u8, Vec<Polyline>> =
            std::collections::BTreeMap::new();
        for (pl, tag) in tagged {
            let pen = if tag == NON_PEN { main } else { tag.min(last) };
            groups.entry(pen).or_default().push(pl);
        }
        let total = groups.len();
        groups
            .into_iter()
            .enumerate()
            .map(|(k, (pen, paths))| {
                let spec = &specs[pen as usize];
                let mut g = format!(
                    "; 多笔绘制 {}/{} · 第 {} 支笔「{}」· 线宽 {:.2}mm · 深度 {:.0}%\n",
                    k + 1,
                    total,
                    pen as usize + 1,
                    spec.name,
                    spec.width_mm,
                    spec.darkness * 100.0
                );
                if let Some((layout, page)) = &tile {
                    g.push_str(&format!(
                        "; 大纸张分页：第 {}/{} 页\n",
                        page + 1,
                        layout.count()
                    ));
                }
                g.push_str(&wrobot_core::generate(&paths, &gopts));
                PenJob {
                    pen: pen as usize,
                    name: spec.name.clone(),
                    gcode: g,
                    paths,
                }
            })
            .collect()
    }

    /// 尝试把本次绘制拆成多笔任务。成功返回 `true`（已就绪，等用户装笔）。
    ///
    /// 只有多笔内容、且真的分成了两支以上时才返回 `true`；
    /// 其余情况返回 `false`，调用方照旧走单笔流程。
    pub fn start_pen_jobs(&mut self) -> bool {
        let jobs = self.build_pen_jobs();
        if jobs.len() < 2 {
            return false;
        }
        self.gcode_text = jobs
            .iter()
            .map(|j| j.gcode.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        self.pen_jobs = jobs;
        self.pen_job_idx = 0;
        self.pen_running = true;
        self.pen_wait = true;
        self.pen_active = false;
        self.pen_seen_running = false;
        self.pen_dispatched_at = None;
        let name = self.pen_jobs[0].name.clone();
        let n = self.pen_jobs.len();
        self.status = format!("多笔绘制：共 {n} 支笔。装入第 1 支「{name}」后点「开始这支笔」");
        true
    }

    /// 下发当前这支笔（由换笔确认窗口调用）。
    pub fn start_pen_job(&mut self) {
        if !self.pen_running {
            return;
        }
        let Some(job) = self.pen_jobs.get(self.pen_job_idx) else {
            self.finish_pen_jobs();
            return;
        };
        let (g, paths, name, idx, total) = (
            job.gcode.clone(),
            job.paths.clone(),
            job.name.clone(),
            self.pen_job_idx,
            self.pen_jobs.len(),
        );
        self.start_run_with_preview(g, paths);
        self.pen_wait = false;
        self.pen_active = true;
        self.pen_seen_running = false;
        self.pen_dispatched_at = Some(std::time::Instant::now());
        self.status = format!("第 {}/{} 支笔「{}」绘制中…", idx + 1, total, name);
    }

    /// 取消整个多笔队列（已下发的这支不会停——要停请点「停止」）。
    pub fn cancel_pen_jobs(&mut self) {
        let had = self.pen_running;
        self.pen_running = false;
        self.pen_wait = false;
        self.pen_active = false;
        self.pen_seen_running = false;
        self.pen_dispatched_at = None;
        self.pen_jobs.clear();
        self.pen_job_idx = 0;
        if had {
            self.status = "已取消多笔队列（当前这支仍会画完）".into();
        }
    }

    fn finish_pen_jobs(&mut self) {
        self.pen_running = false;
        self.pen_wait = false;
        self.pen_active = false;
        self.pen_seen_running = false;
        self.pen_dispatched_at = None;
        self.pen_jobs.clear();
        self.pen_job_idx = 0;
        self.status = "多笔绘制完成：所有笔都已画完".into();
    }

    /// 每帧检查队列进度：上一支笔停下后，关掉队列或提示换下一支。
    ///
    /// `running` 来自设备线程。**不能用「G-code 已发完」判断画完**——
    /// 发完最后一行时机器还在走，换笔会撞到正在运动的笔架，所以这里等
    /// `running == false`（设备线程在最后一行确认后才清除），并要求用户
    /// 在弹窗里亲眼确认机器已停稳才继续。
    pub fn poll_pen_queue(&mut self, running: bool) {
        if !self.pen_running || self.pen_wait || !self.pen_active {
            return;
        }
        if running {
            self.pen_seen_running = true;
            return;
        }
        // 刚下发、后台线程还没把 running 置 true 的瞬间不能算画完。
        if !self.pen_seen_running {
            let grace = std::time::Duration::from_millis(1500);
            if self
                .pen_dispatched_at
                .map_or(false, |t| t.elapsed() < grace)
            {
                return;
            }
        }
        self.pen_active = false;
        self.pen_seen_running = false;
        self.pen_dispatched_at = None;
        self.pen_job_idx += 1;
        if self.pen_job_idx < self.pen_jobs.len() {
            let name = self.pen_jobs[self.pen_job_idx].name.clone();
            self.pen_wait = true;
            self.status = format!(
                "第 {} 支笔画完。请换「{}」并确认机器停稳",
                self.pen_job_idx, name
            );
        } else {
            self.finish_pen_jobs();
        }
    }

    /// 由图片对象当前的参数构造生成选项。
    fn image_opts(&self, img: &ImageObject) -> crate::io::ImageGenOpts {
        let (pens, pen_count) = self.hatch_pens();
        crate::io::ImageGenOpts {
            target_h_mm: img.natural_h.max(1.0), // 已按 target_h 生成，这里仅用于重新生成时保持高度
            step_px: img.step_px,
            threshold: img.threshold,
            edge_threshold: img.edge_threshold,
            invert: img.invert,
            draw_type: img.draw as usize,
            crop: img.crop,
            scan_angle: img.scan_angle,
            merge_gap_px: img.merge_gap_px,
            order_mode: img.order_mode,
            block_px: img.block_px,
            sketch_pressure: img.sketch_pressure,
            sketch_jitter: img.sketch_jitter,
            sketch_pen: img.sketch_pen,
            hatch_levels: img.hatch_levels,
            hatch_angle: img.hatch_angle,
            region_block_mm: img.region_block_mm,
            stroke_len_mm: img.stroke_len_mm,
            stroke_follow: img.stroke_follow,
            multi_pen: img.multi_pen && pen_count >= 2,
            pens,
            pen_count,
            ..Default::default()
        }
    }

    /// 参与多笔分层的笔（只取勾选的，按亮 → 暗的列表顺序）。
    pub fn active_pens(&self) -> Vec<&PenSpec> {
        self.pens.iter().filter(|p| p.on).collect()
    }

    /// 转成算法用的定长笔组。
    fn hatch_pens(&self) -> ([crate::sketch::HatchPen; 4], u8) {
        let mut out = crate::io::NO_PENS;
        let active = self.active_pens();
        let n = active.len().min(4);
        for (i, p) in active.iter().take(n).enumerate() {
            out[i] = p.to_hatch();
        }
        (out, n as u8)
    }

    /// 笔组参数指纹：线宽 / 深浅 / 启用状态变化时，多笔结果必须重算。
    /// 颜色和套准偏移不改变笔迹，只影响预览与输出，所以不计入。
    fn pen_set_hash(&self) -> u64 {
        let mut h = self.pens.len() as u64;
        for p in &self.pens {
            for v in [p.width_mm, p.darkness, p.on as u8 as f64] {
                h = h.wrapping_mul(31).wrapping_add((v * 1000.0) as u64);
            }
        }
        h
    }

    /// 按图片自身参数重新生成基础笔迹（参数变化时调用）。
    pub fn regenerate_image(&mut self, i: usize) {
        if i >= self.images.len() {
            return;
        }
        let key = self.images[i].key(self.pen_set_hash());
        if self.images[i].gen_key == key {
            self.img_regen_at = None;
            return; // 参数未变，无需重算
        }
        // 防抖：参数刚变时只记一笔，等它稳定 160ms 再算。
        // 拖滑杆时键每帧都在变，于是整个拖动过程一次都不算——松手后补算一次。
        // 不这样做的话，素描模式每拖一格都要停顿一百多毫秒，界面完全是卡死的。
        let now = std::time::Instant::now();
        match self.img_regen_at {
            Some((k, t)) if k == key => {
                if now.duration_since(t) < std::time::Duration::from_millis(160) {
                    return;
                }
            }
            _ => {
                self.img_regen_at = Some((key, now));
                return;
            }
        }
        self.img_regen_at = None;
        // PDF 导入的矢量页没有原始图片字节，无法重算（也不该被重算）
        if self.images[i].bytes.is_empty() {
            self.images[i].gen_key = key;
            return;
        }
        let opts = self.image_opts(&self.images[i]);
        if let Ok(r) = crate::io::image_to_polylines(&self.images[i].bytes, &opts) {
            self.images[i].base = r.polylines;
            self.images[i].base_pens = r.pens;
            self.images[i].natural_w = r.w_mm;
            self.images[i].natural_h = r.h_mm;
            self.images[i].gen_key = key;
        }
    }

    /// 导入一张图片为图片对象（foreground 或 background）。
    pub fn import_image_file(&mut self, path: &Path, background: bool) -> anyhow::Result<usize> {
        let data = std::fs::read(path)?;
        let (pens, pen_count) = self.hatch_pens();
        let draw = ImgDraw::from_usize(self.img_draw_type);
        // 素描调子 / 分区重绘 / 素描笔触默认走多笔：这正是导入用户最常要的效果；其他模式仍单笔。
        let multi_pen = matches!(
            draw,
            ImgDraw::SketchHatch | ImgDraw::RegionRedraw | ImgDraw::StrokeSketch
        ) && pen_count >= 2;
        let opts = crate::io::ImageGenOpts {
            target_h_mm: self.img_target_h,
            step_px: self.img_step,
            threshold: self.img_thr,
            edge_threshold: self.img_edge,
            invert: self.img_invert,
            draw_type: self.img_draw_type,
            crop: (0.0, 0.0, 1.0, 1.0),
            scan_angle: self.img_scan_angle,
            merge_gap_px: self.img_merge_gap,
            order_mode: self.img_order_mode,
            block_px: self.img_block_px,
            sketch_pressure: self.img_sketch_pressure,
            sketch_jitter: self.img_sketch_jitter,
            sketch_pen: self.img_sketch_pen,
            hatch_levels: self.img_hatch_levels,
            hatch_angle: self.img_hatch_angle,
            region_block_mm: self.img_region_block_mm,
            stroke_len_mm: self.img_stroke_len_mm,
            stroke_follow: self.img_stroke_follow,
            multi_pen,
            pens,
            pen_count,
            ..Default::default()
        };
        let r = crate::io::image_to_polylines(&data, &opts)?;
        let n = r.polylines.len();
        // 默认放在页面左上角附近，避免与文字完全重叠
        let (x, y) = if background { (10.0, 10.0) } else { (30.0, 30.0) };
        let img = ImageObject {
            path: path.display().to_string(),
            bytes: data,
            x,
            y,
            scale: 1.0,
            rotation: 0.0,
            flip_h: false,
            flip_v: false,
            draw,
            step_px: self.img_step,
            threshold: self.img_thr,
            edge_threshold: self.img_edge,
            invert: self.img_invert,
            crop: (0.0, 0.0, 1.0, 1.0),
            scan_angle: self.img_scan_angle,
            merge_gap_px: self.img_merge_gap,
            order_mode: self.img_order_mode,
            block_px: self.img_block_px,
            sketch_pressure: self.img_sketch_pressure,
            sketch_jitter: self.img_sketch_jitter,
            sketch_pen: self.img_sketch_pen,
            hatch_levels: self.img_hatch_levels,
            hatch_angle: self.img_hatch_angle,
            region_block_mm: self.img_region_block_mm,
            stroke_len_mm: self.img_stroke_len_mm,
            stroke_follow: self.img_stroke_follow,
            multi_pen,
            background,
            out: true,
            base: r.polylines,
            base_pens: r.pens,
            natural_w: r.w_mm,
            natural_h: r.h_mm,
            gen_key: 0,
        };
        self.images.push(img);
        self.sel_image = self.images.len() - 1;
        Ok(n)
    }

    // ── 矢量文件（DXF / SVG）导入 ───────────────────────────────

    /// 导入矢量文件（DXF 或 SVG），解析为页面毫米坐标的笔迹并并入「绘图」模块。
    ///
    /// 解析后的折线**直接追加到 `draw_paths`**（与手绘同一落点，y 向下、原点左上），
    /// 因此绘图模块开启或停在其上时即可见、可参与 G-code、可一键清空。
    pub fn import_vector_file(&mut self, path: &Path, target_h_mm: f64) -> anyhow::Result<usize> {
        let data = std::fs::read(path)?;
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();
        let paths = match ext.as_str() {
            "dxf" => wrobot_core::import_dxf(&data, target_h_mm)
                .map_err(|e| anyhow::anyhow!("{e}"))?,
            "svg" => wrobot_core::import_svg(&data, target_h_mm)
                .map_err(|e| anyhow::anyhow!("{e}"))?,
            other => anyhow::bail!("不支持的矢量格式：{other}（仅支持 DXF / SVG）"),
        };
        let n = paths.len();
        self.draw_paths.extend(paths);
        self.rebuild_preview();
        self.status = format!("已导入矢量：{n} 条笔迹（「绘图」模块可清空）");
        Ok(n)
    }

    // ── PDF 导入 ───────────────────────────────────────────────

    /// 读一个 PDF 的总页数（供界面挑页码），失败时把原因写进 `status`。
    pub fn load_pdf_info(&mut self, path: &Path) -> anyhow::Result<usize> {
        let data = std::fs::read(path)?;
        let n = wrobot_core::pdf_page_count(&data)?;
        self.pdf_path = path.display().to_string();
        self.pdf_pages = n;
        if self.pdf_page < 1 || self.pdf_page > n {
            self.pdf_page = 1;
        }
        Ok(n)
    }

    /// 导入 PDF 的指定页（`page1` 从 1 起）。
    ///
    /// 页面里的三类内容分别处理：
    /// - **矢量路径**：直接转成笔迹；
    /// - **嵌入图像**：各成一个图片对象，按 PDF 里摆放的位置放置，走影线/轮廓描边；
    /// - **文字**：不做描边，而是提取文本与位置，用**当前手写字体重新排版**——
    ///   这才是写字机器人该干的事（把 PDF 内容「写」出来，而不是描它的字形轮廓）。
    ///
    /// 尺寸默认 **1:1**（按 PDF 页面的真实物理尺寸），取消勾选后按「目标高度」缩放。
    ///
    /// 返回 `(矢量+文字笔迹条数, 图片对象数)`。
    pub fn import_pdf_page(
        &mut self,
        path: &Path,
        page1: usize,
    ) -> anyhow::Result<(usize, usize)> {
        let data = std::fs::read(path)?;
        let pg = wrobot_core::import_page(
            &data,
            page1.saturating_sub(1),
            &wrobot_core::PdfImportOpts::default(),
        )?;
        // 1:1 → 按页面真实毫米尺寸；否则缩放到「目标高度」
        let scale = if self.pdf_1to1 {
            1.0
        } else {
            (self.img_target_h / pg.h_mm.max(0.1)).clamp(0.02, 20.0)
        };
        let (step_px, thr, edge, invert, draw_type) = (
            self.img_step,
            self.img_thr,
            self.img_edge,
            self.img_invert,
            self.img_draw_type,
        );
        let (scan_angle, merge_gap, order_mode, block_px) = (
            self.img_scan_angle,
            self.img_merge_gap,
            self.img_order_mode,
            self.img_block_px,
        );
        let (sketch_pressure, sketch_jitter) =
            (self.img_sketch_pressure, self.img_sketch_jitter);
        let sketch_pen = self.img_sketch_pen;
        let (hatch_levels, hatch_angle, region_block_mm) = (
            self.img_hatch_levels,
            self.img_hatch_angle,
            self.img_region_block_mm,
        );
        let (stroke_len_mm, stroke_follow) = (self.img_stroke_len_mm, self.img_stroke_follow);
        let (pens, pen_count) = self.hatch_pens();
        let multi_pen = matches!(
            ImgDraw::from_usize(draw_type),
            ImgDraw::SketchHatch | ImgDraw::RegionRedraw | ImgDraw::StrokeSketch
        ) && pen_count >= 2;
        let tag = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "PDF".to_string());

        // ① 嵌入图像
        let mut img_count = 0usize;
        for im in &pg.images {
            let opts = crate::io::ImageGenOpts {
                target_h_mm: (im.rect.h * scale).max(1.0),
                step_px,
                threshold: thr,
                edge_threshold: edge,
                invert,
                draw_type,
                crop: (0.0, 0.0, 1.0, 1.0),
                scan_angle,
                merge_gap_px: merge_gap,
                order_mode,
                block_px,
                sketch_pressure,
                sketch_jitter,
                sketch_pen,
                hatch_levels,
                hatch_angle,
                region_block_mm,
                stroke_len_mm,
                stroke_follow,
                multi_pen,
                pens,
                pen_count,
                ..Default::default()
            };
            let r = crate::io::pdf_image_to_polylines(im, &opts)?;
            if r.polylines.is_empty() {
                continue;
            }
            self.images.push(ImageObject {
                path: format!("{} · 第{}页 · 图片", tag, page1),
                bytes: Vec::new(),
                x: im.rect.x * scale,
                y: im.rect.y * scale,
                scale: 1.0,
                rotation: 0.0,
                flip_h: false,
                flip_v: false,
                draw: ImgDraw::from_usize(draw_type),
                step_px,
                threshold: thr,
                edge_threshold: edge,
                invert,
                crop: (0.0, 0.0, 1.0, 1.0),
                scan_angle,
                merge_gap_px: merge_gap,
                order_mode,
                block_px,
                sketch_pressure,
                sketch_jitter,
                sketch_pen,
                hatch_levels,
                hatch_angle,
                region_block_mm,
                stroke_len_mm,
                stroke_follow,
                multi_pen,
                background: false,
                out: true,
                base: r.polylines,
                base_pens: r.pens,
                natural_w: r.w_mm,
                natural_h: r.h_mm,
                gen_key: 0,
            });
            self.sel_image = self.images.len() - 1;
            img_count += 1;
        }

        // ② 矢量路径 + 文本（文本用手写字体重排）
        let mut base: Vec<Polyline> = pg.paths.clone();
        if !pg.text_runs.is_empty() {
            base.extend(self.pdf_runs_to_polylines(&pg.text_runs, pg.w_mm));
        }
        if (scale - 1.0).abs() > 1e-9 {
            for pl in &mut base {
                for p in &mut pl.pts {
                    p.0 *= scale;
                    p.1 *= scale;
                }
            }
        }
        let n_paths = base.len();
        if n_paths > 0 {
            self.images.push(ImageObject {
                path: format!("{} · 第{}页", tag, page1),
                bytes: Vec::new(), // 矢量页没有原始位图，改参数时也不会去重算
                x: 0.0,
                y: 0.0,
                scale: 1.0,
                rotation: 0.0,
                flip_h: false,
                flip_v: false,
                draw: ImgDraw::Wire,
                step_px,
                threshold: thr,
                edge_threshold: edge,
                invert,
                crop: (0.0, 0.0, 1.0, 1.0),
                scan_angle,
                merge_gap_px: merge_gap,
                order_mode,
                block_px,
                sketch_pressure,
                sketch_jitter,
                sketch_pen,
                hatch_levels,
                hatch_angle,
                region_block_mm,
                stroke_len_mm,
                stroke_follow,
                multi_pen: false,
                background: false,
                out: true,
                base,
                base_pens: vec![],
                natural_w: (pg.w_mm * scale).max(1.0),
                natural_h: (pg.h_mm * scale).max(1.0),
                gen_key: 0,
            });
            self.sel_image = self.images.len() - 1;
        }
        let ph = self.pen_set_hash();
        for i in 0..self.images.len() {
            if self.images[i].bytes.is_empty() {
                self.images[i].gen_key = self.images[i].key(ph);
            }
        }
        Ok((n_paths, img_count))
    }

    /// 把 PDF 里的文本段按原位置用**当前手写字体**排成笔迹。
    ///
    /// PDF 只给了基线位置与字号，这里按「字框顶端 ≈ 基线 − 0.8×字号」换算成
    /// 本程序排版用的元素坐标（手写字库的字身几乎全在基线之上）。
    fn pdf_runs_to_polylines(
        &self,
        runs: &[wrobot_core::PdfTextRun],
        page_w: f64,
    ) -> Vec<Polyline> {
        if runs.is_empty() {
            return Vec::new();
        }
        let Some(font) = self.fonts.first_name() else {
            return Vec::new();
        };
        let mut items: Vec<Item> = Vec::new();
        for r in runs {
            let size_mm = r.size_mm;
            if size_mm < 0.5 || size_mm > 400.0 || r.text.trim().is_empty() {
                continue;
            }
            let font_size = (size_mm / self.char_mm_at_100.max(0.01)) * 100.0;
            let w = (page_w - r.x_mm).max(size_mm);
            let h = size_mm * 1.3;
            let cf = ContentFormat {
                charset: Some("UTF-8".to_string()),
                auto_gp: false,
                gp_num: 1,
                first_line_align: Some("LEFT".to_string()),
                ..Default::default()
            };
            let pi = PageInfo {
                line_gap: 0.0,
                space: 0.0,
                content_align: Some("left".to_string()),
                page_width: w,
                page_height: h,
                vcenter: false,
                ink_center: false,
                font_size,
                font_path: vec![font.clone()],
                ..Default::default()
            };
            items.push(Item {
                r#type: "text".to_string(),
                x: r.x_mm,
                y: r.baseline_mm - size_mm * 0.8,
                width: w,
                height: h,
                text: Some(r.text.trim().to_string()),
                content_format: Some(serde_json::to_string(&cf).unwrap_or_default()),
                page_info: Some(pi),
                ..Default::default()
            });
        }
        if items.is_empty() {
            return Vec::new();
        }
        let doc = Document {
            items,
            ..Default::default()
        };
        let stack = crate::fontmgr::build_stack(&self.fonts, &doc);
        let opts = LayoutOptions {
            px_per_mm: 1.0,
            char_mm_at_100: self.char_mm_at_100,
            tab_indent_chars: 2.0,
            random: Default::default(),
        };
        wrobot_core::document_to_polylines(&doc, &stack, &opts)
    }

    /// 删除指定图片。
    pub fn remove_image(&mut self, i: usize) {
        if i < self.images.len() {
            self.images.remove(i);
            if self.sel_image >= self.images.len() {
                self.sel_image = self.images.len().saturating_sub(1);
            }
        }
    }

    /// 清空所有图片。
    pub fn clear_images(&mut self) {
        self.images.clear();
        self.sel_image = 0;
        self.img_drag = None;
    }

    /// 选中图片（越界时安全回落）。
    pub fn select_image(&mut self, i: usize) {
        if self.images.is_empty() {
            self.sel_image = 0;
        } else {
            self.sel_image = i.min(self.images.len() - 1);
        }
    }

    /// 按倍率缩放（围绕图片中心，位置不变）。
    pub fn scale_image(&mut self, i: usize, factor: f64) {
        let Some(im) = self.images.get_mut(i) else {
            return;
        };
        let old = im.scale;
        let new = (old * factor).clamp(0.05, 20.0);
        if (new - old).abs() < 1e-12 {
            return;
        }
        // 中心不动 → 左上角随之偏移
        let cx = im.x + im.natural_w * old * 0.5;
        let cy = im.y + im.natural_h * old * 0.5;
        im.scale = new;
        im.x = cx - im.natural_w * new * 0.5;
        im.y = cy - im.natural_h * new * 0.5;
    }

    /// 直接设定缩放（围绕中心）。
    pub fn set_image_scale(&mut self, i: usize, scale: f64) {
        if let Some(im) = self.images.get(i) {
            let f = scale / im.scale.max(1e-9);
            self.scale_image(i, f);
        }
    }

    /// 微调位置（毫米）。
    pub fn nudge_image(&mut self, i: usize, dx: f64, dy: f64) {
        if let Some(im) = self.images.get_mut(i) {
            im.x += dx;
            im.y += dy;
        }
    }

    /// 直接设定位置（左上角，毫米）。
    pub fn set_image_pos(&mut self, i: usize, x: f64, y: f64) {
        if let Some(im) = self.images.get_mut(i) {
            im.x = x;
            im.y = y;
        }
    }

    /// 把图片在页面上居中（按未旋转的包围盒计算）。
    pub fn center_image(&mut self, i: usize) {
        let Some(im) = self.images.get(i) else {
            return;
        };
        let (w, h) = (im.natural_w * im.scale, im.natural_h * im.scale);
        let (x, y) = (
            (self.page_w_mm - w) * 0.5,
            (self.page_h_mm - h) * 0.5,
        );
        self.set_image_pos(i, x, y);
    }

    /// 缩放到「适应页面」（留 5% 边距）。
    pub fn fit_image_to_page(&mut self, i: usize) {
        let Some(im) = self.images.get(i) else {
            return;
        };
        if im.natural_w <= 0.0 || im.natural_h <= 0.0 {
            return;
        }
        let sx = (self.page_w_mm * 0.9) / im.natural_w;
        let sy = (self.page_h_mm * 0.9) / im.natural_h;
        self.set_image_scale(i, sx.min(sy));
        self.center_image(i);
    }

    /// 重置选中图片的变换（回到原始尺寸并居中）。
    pub fn reset_image_transform(&mut self, i: usize) {
        self.set_image_scale(i, 1.0);
        if let Some(im) = self.images.get_mut(i) {
            im.rotation = 0.0;
            im.flip_h = false;
            im.flip_v = false;
        }
        self.center_image(i);
    }

    /// 命中测试：页面毫米点是否落在第 i 张图片上（含旋转 / 镜像）。
    pub fn image_hit(&self, i: usize, mx: f64, my: f64) -> bool {
        self.images.get(i).map(|im| im.hit(mx, my)).unwrap_or(false)
    }

    /// 第 i 张图片的四角（页面毫米），用于绘制旋转后的选中框。
    pub fn image_corners(&self, i: usize) -> Option<[(f64, f64); 4]> {
        self.images.get(i).map(|im| im.corners())
    }

    /// 重建预览笔迹（受「逼真化随机」开关控制，默认确定性）。
    pub fn rebuild_preview(&mut self) {
        let tagged = self.collect_tagged_polylines(self.use_random);
        self.preview_pens = tagged.iter().map(|(_, t)| *t).collect();
        self.preview = tagged.into_iter().map(|(p, _)| p).collect();
        self.refresh_stats();
    }

    /// 重新播种并重建预览——真正产生一次「新的」随机手写变形。
    pub fn rebuild_preview_random(&mut self) {
        self.rand_seed ^= 0x9E3779B97F4A7C15u64;
        self.rand_seed = self.rand_seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.use_random = true;
        let tagged = self.collect_tagged_polylines(true);
        self.preview_pens = tagged.iter().map(|(_, t)| *t).collect();
        self.preview = tagged.into_iter().map(|(p, _)| p).collect();
        self.refresh_stats();
    }

    /// 重算缓存的任务统计（笔迹数、点数、墨线长、空移长）。
    ///
    /// 只在预览重建时算一次：折线可能有几万个点，放到每帧 UI 里算会明显卡顿。
    pub fn refresh_stats(&mut self) {
        let paths = &self.preview;
        let mut strokes = 0usize;
        let mut points = 0usize;
        for p in paths {
            if p.len() >= 2 {
                strokes += 1;
            }
            points += p.len();
        }
        let ink_mm = wrobot_core::total_length(paths);
        let travel_mm = wrobot_core::path_travel(paths, (0.0, 0.0));
        self.stats = JobStats {
            strokes,
            points,
            ink_mm,
            travel_mm,
        };
    }

    /// 按当前速度与抬落笔延迟估算运行耗时（秒）。
    ///
    /// 用空移 + 墨线总行程除以进给速度，再加上每笔的落笔 / 抬笔等待；
    /// 没有把加减速算进去，实际会略慢一些，用来说明数量级足够。
    pub fn estimated_seconds(&self) -> f64 {
        let f = self.feed_rate.max(1.0); // mm/min
        let motion = self.stats.travel_mm / f * 60.0;
        let pens = self.stats.strokes as f64;
        let delays = pens * (self.tool_on_delay + self.tool_off_delay);
        motion + delays
    }

    /// 按倍率缩放画布视图（围绕整页适应，1.0~6.0）。
    pub fn zoom_by(&mut self, factor: f32) {
        let z = self.canvas_zoom * factor;
        self.set_zoom(z);
    }

    /// 设置画布缩放倍率；回到 1.0 时同时复位平移。
    pub fn set_zoom(&mut self, zoom: f32) {
        self.canvas_zoom = zoom.clamp(1.0, 6.0);
        if self.canvas_zoom <= 1.001 {
            self.canvas_pan = (0.0, 0.0);
        }
    }

    /// 恢复「整页适应」视图。
    pub fn reset_view(&mut self) {
        self.canvas_zoom = 1.0;
        self.canvas_pan = (0.0, 0.0);
    }

    /// 生成 G-code 文本（与当前预览使用同一随机种子，保证所见即所得）。
    ///
    /// 开启「大纸张分页」时只生成当前页：先将机器笔尖放到该页左上角，
    /// 再从「运行监控」逐页绘制；每页自带定位标记与回原点。
    pub fn generate_gcode(&mut self) -> String {
        if self.tile_on {
            if let Some(layout) = self.tile_layout() {
                let page = self.tile_page.min(layout.count().saturating_sub(1));
                if let Some(g) = self.generate_tile_gcode(page) {
                    return g;
                }
            }
        }
        let all = self.collect_polylines(self.use_random);
        let gopts = self.gcode_options();
        wrobot_core::generate(&all, &gopts)
    }

    /// **边界自检**：解析 G-code 的 XY 范围并与机器幅面（`machine_w_mm`×`machine_h_mm`）比对。
    ///
    /// 超界时返回给用户看的提示文本（哪些方向超出、超出多少），未超界返回 `None`：
    /// - **跨度超界**：内容宽/高超过机器行程宽/高 —— 以「当前位置为原点」时机器
    ///   从起点出发最多也只能走一个幅面大小；
    /// - **绝对坐标超界**：关闭「以当前位置为原点」后坐标按机械原点解析，
    ///   内容任一角落到不了 `[0,0]~[机器宽,机器高]` 工作区域内都会软限位报警。
    pub fn bounds_warning(&self, gcode: &str) -> Option<String> {
        let (min_x, min_y, max_x, max_y) = wrobot_core::gcode_xy_bounds(gcode)?;
        let mut probs: Vec<String> = Vec::new();
        let span_x = max_x - min_x;
        let span_y = max_y - min_y;
        if span_x > self.machine_w_mm + 1e-6 {
            probs.push(format!(
                "内容宽 {:.1}mm > 机器行程宽 {:.1}mm",
                span_x, self.machine_w_mm
            ));
        }
        if span_y > self.machine_h_mm + 1e-6 {
            probs.push(format!(
                "内容高 {:.1}mm > 机器行程高 {:.1}mm",
                span_y, self.machine_h_mm
            ));
        }
        if !self.gcode_options().use_current_origin
            && (min_x < -1e-6
                || min_y < -1e-6
                || max_x > self.machine_w_mm + 1e-6
                || max_y > self.machine_h_mm + 1e-6)
        {
            probs.push(format!(
                "绝对坐标模式：内容范围 ({:.1},{:.1})~({:.1},{:.1}) 超出工作区域 (0,0)~({:.0},{:.0})",
                min_x, min_y, max_x, max_y, self.machine_w_mm, self.machine_h_mm
            ));
        }
        if probs.is_empty() {
            None
        } else {
            Some(probs.join("；"))
        }
    }

    /// 当前固件/G-code 选项。
    pub fn gcode_options(&self) -> GcodeOptions {
        let firmware = match self.firmware_sel {
            1 => Firmware::Marlin,
            2 => Firmware::Ebb,
            _ => Firmware::Grbl,
        };
        let pen_mode = if self.pen_mode_sel == 1 {
            PenMode::Servo
        } else {
            PenMode::Z
        };
        // 抬/落笔参数按固件分开保存（与参考项目 default.properties 一致）：
        // GRBL 用 zOn/zOff/zSpeed，Marlin 用 marlin_pen_*_mm / marlin_z_speed（或舵机角度），
        // 否则切一次固件就得手改一遍数值。
        let (pen_up, pen_down, z_speed) = match firmware {
            Firmware::Marlin => {
                if pen_mode == PenMode::Servo {
                    (
                        self.marlin_servo_up,
                        self.marlin_servo_down,
                        self.marlin_z_speed,
                    )
                } else {
                    (self.marlin_pen_up, self.marlin_pen_down, self.marlin_z_speed)
                }
            }
            _ => (self.pen_up, self.pen_down, self.z_speed),
        };
        GcodeOptions {
            firmware,
            pen_mode,
            feed_rate: self.feed_rate,
            z_speed,
            pen_up,
            pen_down,
            mm_to_steps: self.mm_to_steps,
            reverse_x: self.reverse_x,
            reverse_y: self.reverse_y,
            swap_xy: self.swap_xy,
            page_w: self.page_w_mm,
            page_h: self.page_h_mm,
            custom_start: self.custom_start.clone(),
            custom_end: self.custom_end.clone(),
            origin_corner: self.origin_corner,
            laser_mode: self.laser_on,
            laser_power: self.laser_power,
            auto_sleep: self.auto_sleep,
            tool_on_delay: self.tool_on_delay,
            tool_off_delay: self.tool_off_delay,
            ebb_servo_speed: self.ebb_servo_speed,
            ebb_max_pen: self.ebb_max_pen,
            min_step: self.min_step_mm.clamp(0.0, 5.0),
            // 运行前自动设原点 → G-code 自带 `G92 X0 Y0`，坐标相对机器当前位置计算
            use_current_origin: self.auto_set_zero,
            ..Default::default()
        }
    }

    // ── 页面尺寸 / 纸张 ─────────────────────────────────

    /// 套用一个纸张规格；**沿用当前横竖取向**（横着放 A4 时切 B5 仍是横的）。
    pub fn set_paper(&mut self, name: &str) {
        let Some((w, h)) = paper_size(name) else {
            self.paper = PAPER_CUSTOM.to_string();
            return;
        };
        let (w, h) = if self.page_w_mm > self.page_h_mm {
            (h, w)
        } else {
            (w, h)
        };
        self.page_w_mm = w;
        self.page_h_mm = h;
        self.paper = name.to_string();
        self.status = format!("纸张已切换：{}（{}×{} mm）", name, trim_mm(w), trim_mm(h));
    }

    /// 横 / 竖向切换：宽高互换（内容不动，只换纸的方向）。
    pub fn toggle_orientation(&mut self) {
        let (w, h) = (self.page_w_mm, self.page_h_mm);
        self.page_w_mm = h;
        self.page_h_mm = w;
        self.status = format!(
            "已切换为{}（{}×{} mm）",
            if self.page_w_mm > self.page_h_mm {
                "横向"
            } else {
                "纵向"
            },
            trim_mm(self.page_w_mm),
            trim_mm(self.page_h_mm)
        );
    }

    /// 手动改过宽高后重新判定规格名：仍是某个规格就显示规格名，否则「自定义」。
    ///
    /// 载入旧工程 / PDF 导入改了尺寸之后调用，避免下拉框回显错名字。
    pub fn sync_paper(&mut self) {
        let (w, h) = (self.page_w_mm, self.page_h_mm);
        let hit = PAPER_PRESETS.iter().find(|(_, pw, ph)| {
            ((w - pw).abs() < 0.5 && (h - ph).abs() < 0.5)
                || ((w - ph).abs() < 0.5 && (h - pw).abs() < 0.5)
        });
        self.paper = hit
            .map(|(n, _, _)| (*n).to_string())
            .unwrap_or_else(|| PAPER_CUSTOM.to_string());
    }

    /// 下拉框/标题栏显示的尺寸文本，例如 `A4 · 210×297 mm`。
    pub fn paper_label(&self) -> String {
        format!(
            "{} · {}×{} mm",
            self.paper,
            trim_mm(self.page_w_mm),
            trim_mm(self.page_h_mm)
        )
    }

    /// 生成 SVG 预览文本。
    pub fn generate_svg(&mut self) -> String {
        let doc = self.compose_doc();
        let stack = crate::fontmgr::build_stack(&self.fonts, &doc);
        let opts = LayoutOptions {
            px_per_mm: 1.0,
            char_mm_at_100: self.char_mm_at_100,
            tab_indent_chars: 2.0,
            random: Default::default(),
        };
        wrobot_core::document_to_svg(&doc, &stack, &opts)
    }

    /// 生成单页矢量 PDF 字节流。
    pub fn generate_pdf(&mut self) -> Vec<u8> {
        let doc = self.compose_doc();
        let stack = crate::fontmgr::build_stack(&self.fonts, &doc);
        let opts = LayoutOptions {
            px_per_mm: 1.0,
            char_mm_at_100: self.char_mm_at_100,
            tab_indent_chars: 2.0,
            random: Default::default(),
        };
        wrobot_core::document_to_pdf(&doc, &stack, &opts, self.page_w_mm, self.page_h_mm)
    }

    /// 自动连接：把任务交给后台线程（扫描串口 + 握手固件 + 连接），不阻塞 UI。
    pub fn auto_connect(&mut self) {
        self.status = "正在自动连接…".into();
        self.device.send(crate::device::DevCmd::AutoConnect);
    }

    /// 启动一次后台串口扫描。
    ///
    /// 打开离线蓝牙口（只配对未连接）时 `CreateFile` 可能卡十几秒，
    /// 绝不能放在 UI 线程上——启动时的自动扫描曾因此把窗口卡成“未响应”。
    pub fn start_port_scan(&mut self) {
        if self.port_scan_rx.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let entries = wrobot_core::enumerate_ports();
            let _ = tx.send(entries);
        });
        self.port_scan_rx = Some(rx);
        self.status = "正在后台扫描串口…".into();
    }

    /// 每帧检查后台扫描是否完成；完成则应用结果。
    pub fn poll_port_scan(&mut self) {
        let Some(rx) = self.port_scan_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(entries) => {
                self.port_scan_rx = None;
                self.apply_port_entries(entries);
                if self.auto_connect_after_scan {
                    self.auto_connect_after_scan = false;
                    self.auto_connect();
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.port_scan_rx = None;
                self.status = "串口扫描线程异常结束".into();
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
    }

    /// 串口扫描是否仍在后台进行。
    pub fn port_scan_running(&self) -> bool {
        self.port_scan_rx.is_some()
    }

    /// 应用扫描结果（不重复枚举端口）。
    fn apply_port_entries(&mut self, mut entries: Vec<wrobot_core::serial::PortEntry>) -> usize {
        // 用带类型信息的枚举：蓝牙 / 有线串口线分开，排查建议也就不一样
        // 本程序已连着的口，试开必然失败（被自己占用，错误码 5）——直接标为在线，
        // 否则用户会看到「自己的端口离线」这种误导性结论。
        if self.dev_connected && self.dev_kind == 0 && !self.serial_port.is_empty() {
            for e in entries.iter_mut() {
                if e.port == self.serial_port {
                    e.online = true;
                }
            }
        }
        let detailed: Vec<(String, bool)> = entries
            .iter()
            .map(|e| (e.port.clone(), e.online))
            .collect();
        self.port_labels = entries
            .iter()
            .map(|e| {
                let label = if e.desc.is_empty() {
                    format!("{} · {}", e.kind.label(), e.port)
                } else {
                    format!("{} · {}", e.kind.label(), e.desc)
                };
                (e.port.clone(), label, e.online)
            })
            .collect();
        let online: Vec<String> = detailed
            .iter()
            .filter(|(_, ok)| *ok)
            .map(|(p, _)| p.clone())
            .collect();
        self.serial_ports = online.clone();
        self.serial_ports_all = detailed.clone();
        let n = online.len();
        let total = detailed.len();
        if total == 0 {
            self.dev_log.push_str(
                "✗ 本机没有任何登记在册的 COM 口\n  有线：插上 USB-TTL（CH340/CP210x）后重扫；蓝牙：先配对【并连接】BtWriter，再添加“传出”COM 口\n",
            );
        } else {
            // 按类型分组汇报：一眼看出「有没有串口线」「蓝牙链路建没建」。
            // 直接复用本次扫描结果，别再调 wired_ports()/bluetooth_ports()——
            // 它们会各自重新枚举并逐个试开端口，离线蓝牙口会把一次扫描拖成三倍。
            let wired: Vec<&wrobot_core::serial::PortEntry> = entries
                .iter()
                .filter(|e| e.kind.is_wired())
                .collect();
            let bt: Vec<&wrobot_core::serial::PortEntry> = entries
                .iter()
                .filter(|e| e.kind == wrobot_core::PortKind::Bluetooth)
                .collect();
            self.dev_log.push_str(&format!(
                "● 已登记 {} 个端口：串口线 {} 个、蓝牙 {} 个\n",
                total,
                wired.len(),
                bt.len()
            ));
            for e in &entries {
                self.dev_log.push_str(&format!(
                    "  {}\n",
                    e.display()
                ));
            }
            // 保留用户已选的端口（即使暂时打不开，比如蓝牙还没连上）；
            // 只有当前选择已从系统中消失时，才自动挑第一个。
            let known = detailed.iter().any(|(p, _)| *p == self.serial_port);
            if self.serial_port.is_empty() || !known {
                if let Some(first) = self.serial_ports.first() {
                    self.serial_port = first.clone();
                } else if let Some((p, _)) = detailed.first() {
                    self.serial_port = p.clone();
                }
            }
            if wired.is_empty() {
                self.dev_log.push_str(
                    "  ⚠ 没有检测到串口线（USB-TTL）：插上适配器后点“扫描”，设备名带 USB/SERIAL 的就是它\n",
                );
            }
            if !bt.is_empty() {
                let offline: Vec<String> = bt
                    .iter()
                    .filter(|e| !e.online)
                    .map(|e| e.port.clone())
                    .collect();
                if !offline.is_empty() {
                    self.dev_log.push_str(&format!(
                        "  ⚠ 蓝牙口 {} 打不开 = 只配对未连接（机器通电了吗？Windows 蓝牙里连上了吗？）\n",
                        offline.join(", ")
                    ));
                }
            }
        }
        self.status = format!("登记 {} 个端口，其中 {} 个可打开", total, n);
        n
    }

    /// 对当前选中的**有线串口**做一次体检（后台线程执行，结果进日志）。
    ///
    /// 体检会独占端口，所以先断开已有连接，避免「被自己占用（错误码 5）」。
    pub fn wire_test(&mut self) {
        if self.serial_port.is_empty() {
            self.status = "请先选一个 COM 口".into();
            return;
        }
        let kind = self
            .port_labels
            .iter()
            .find(|(p, _, _)| *p == self.serial_port)
            .map(|(_, l, _)| l.clone())
            .unwrap_or_default();
        if kind.starts_with("蓝牙") {
            self.dev_log.push_str(&format!(
                "⚠ {} 是蓝牙口：蓝牙链路不通时请按蓝牙流程排查（{}）\n",
                self.serial_port,
                wrobot_core::PortKind::Bluetooth.tips()
            ));
        }
        self.device.send(crate::device::DevCmd::WireTest(
            self.serial_port.clone(),
            self.serial_baud,
        ));
        self.status = format!("正在体检 {}…结果见底部日志", self.serial_port);
    }

    /// 端口诊断（后台线程执行，把每个口的错误码与建议写进日志）。
    pub fn diagnose_ports(&mut self) {
        self.device.send(crate::device::DevCmd::Diagnose);
        self.status = "正在诊断端口…结果见设备日志".into();
    }

    /// 蓝牙体检：适配器 / 写字机在不在 / COM 口绑没绑对设备。
    pub fn bt_report(&mut self) {
        let name = self.bt_name.trim().to_string();
        if name.is_empty() {
            self.status = "请先填写写字机的蓝牙名称".into();
            return;
        }
        self.device.send(crate::device::DevCmd::BtReport(name.clone()));
        self.status = format!("正在体检蓝牙「{}」…", name);
    }

    /// 给已配对的写字机启用 SPP 串口服务，让系统自动建出**绑定它**的传出 COM 口。
    ///
    /// 「设置里看得到设备」和「有能用的串口」是两回事：很多机器上留下的
    /// 蓝牙 COM 口其实没绑任何设备（打开正常、写进去 0 字节），必须重建。
    pub fn bt_bind(&mut self) {
        let name = self.bt_name.trim().to_string();
        if name.is_empty() {
            self.status = "请先填写写字机的蓝牙名称".into();
            return;
        }
        self.device.send(crate::device::DevCmd::BtBind(name.clone()));
        self.status = format!("正在为「{}」绑定蓝牙串口…", name);
    }

    /// 观察写字机的可见性时间线：判断它是不是「一会儿看得到一会儿看不到」。
    ///
    /// 若呈周期性消失又出现，基本可以断定是写字机端蓝牙模块在反复复位（供电问题），
    /// 而不是电脑蓝牙的毛病。
    pub fn bt_watch(&mut self) {
        let name = self.bt_name.trim().to_string();
        if name.is_empty() {
            self.status = "请先填写写字机的蓝牙名称".into();
            return;
        }
        let rounds = self.bt_watch_rounds.max(1);
        let every = self.bt_watch_every.max(2);
        self.device
            .send(crate::device::DevCmd::BtWatch(name.clone(), rounds, every));
        self.status = format!(
            "正在观察「{}」的可见性（{} 轮 × {} 秒，约 {} 秒）…",
            name,
            rounds,
            every,
            rounds as u64 * every
        );
    }

    /// 在选中的端口上自动扫描波特率（后台线程执行）。
    ///
    /// 「连上了但机器完全没反应」最常见的原因就是波特率不对——写字机出厂可能是
    /// 9600 / 38400 / 115200 中的任意一个，靠猜不行，只能逐个试。
    pub fn auto_baud(&mut self) {
        if self.serial_port.is_empty() {
            self.status = "请先选一个 COM 口".into();
            return;
        }
        self.device.auto_baud(self.serial_port.clone());
        self.status = format!("正在 {} 上扫描波特率…结果见底部日志", self.serial_port);
    }

    /// 按当前面板设置连接设备（命令投递到后台线程，不阻塞 UI）。
    pub fn connect_device(&mut self) {
        let kind = match self.dev_kind {
            0 => DevKind::Serial,
            1 => DevKind::Tcp,
            _ => DevKind::Mock,
        };
        let addr = match kind {
            DevKind::Serial => format!("{}:{}", self.serial_port, self.serial_baud),
            DevKind::Tcp => self.dev_addr.clone(),
            DevKind::Mock => "mock".to_string(),
        };
        self.status = format!("正在连接 {} …", addr);
        self.device.send(crate::device::DevCmd::Connect(kind, addr));
    }

    /// 断开连接（命令投递到后台线程）。
    pub fn disconnect_device(&mut self) {
        self.device.send(crate::device::DevCmd::Disconnect);
        self.status = "已断开".into();
    }

    /// 查询固件信息（GRBL 的 `$I` / Marlin 的 `M115`），回显写入共享日志。
    pub fn query_firmware(&mut self) {
        self.device.send(crate::device::DevCmd::QueryFirmware);
        self.status = "已查询固件信息".into();
    }

    /// 运行整段 G-code（后台逐行流控，不阻塞 UI；自动跳到「运行中」页）。
    /// 同时把当前页面笔迹「冻结」到 run_preview，供运行中进度页独立显示。
    pub fn start_run(&mut self, gcode: String) {
        let pv = self.collect_polylines(self.use_random);
        self.start_run_with_preview(gcode, pv);
    }

    /// 运行 G-code，并显式指定进度页要显示的笔迹（分页 / 定位标记时用）。
    pub fn start_run_with_preview(&mut self, gcode: String, preview: Vec<Polyline>) {
        self.run_preview = preview;
        self.device.send(crate::device::DevCmd::StartRun(gcode));
        self.status = "开始运行…".into();
    }

    /// 一次性下发一段 G-code（不计入运行进度，仅回显到后台日志）。非阻塞。
    pub fn send_gcode(&mut self, gcode: &str) {
        self.device.send(crate::device::DevCmd::Send(gcode.to_string()));
    }

    /// 暂停运行。
    pub fn pause_run(&mut self) {
        self.device.send(crate::device::DevCmd::PauseRun);
    }

    /// 继续运行。
    pub fn resume_run(&mut self) {
        self.device.send(crate::device::DevCmd::ResumeRun);
    }

    /// 停止运行。
    pub fn stop_run(&mut self) {
        self.device.send(crate::device::DevCmd::StopRun);
        // 停止是明确的「不画了」：多笔队列也一起取消，避免停笔后又弹换笔窗。
        if self.pen_running {
            self.cancel_pen_jobs();
            self.status = "已停止，多笔队列已取消".into();
        }
    }

    /// 生成点动 G-code（GRBL 用 $J=，其余用相对 G91）。
    pub fn jog_gcode(&self, dx: f64, dy: f64) -> String {
        let firmware = match self.firmware_sel {
            1 => Firmware::Marlin,
            2 => Firmware::Ebb,
            _ => Firmware::Grbl,
        };
        // 点动用专门的 jog_speed（参考项目 jog_speed=10000），不占用绘图速度
        let f = self.jog_speed;
        // **点动必须跟随「反转 X/Y / 对调 XY」**：按钮上写的是「页面方向」
        // （X+ = 纸面向右），机器轴装的是变换后的值。不换算的话开了反转，
        // 按「X +」机器会往左走，跟画布上的蓝箭头正好相反。
        let (xd, yd) = self.gcode_options().axis_dirs();
        // `xd`/`yd` 是单位页面向量，把页面位移投影上去即得机器轴位移
        let mx = dx * xd.0 + dy * xd.1;
        let my = dx * yd.0 + dy * yd.1;
        match firmware {
            Firmware::Grbl => format!("$J=G91 X{} Y{} F{}", mx, my, f),
            Firmware::Marlin => {
                format!("G91\nG0 X{} Y{} F{}\nG90", mx, my, f)
            }
            Firmware::Ebb => {
                let s = 100.0_f64;
                format!("SM,100,{},{},0", (mx * s) as i64, (my * s) as i64)
            }
        }
    }

    // ── 机器测试（XY 移动 / 回零 / 定位零点 / 抬落笔）────────────

    fn fw(&self) -> Firmware {
        match self.firmware_sel {
            1 => Firmware::Marlin,
            2 => Firmware::Ebb,
            _ => Firmware::Grbl,
        }
    }

    /// 解锁（GRBL 上电/急停后为 Alarm 锁定态，不解锁所有运动指令都会被忽略）。
    pub fn unlock_machine(&mut self) {
        self.send_gcode("$X");
        self.status = "已发送解锁 $X".into();
    }

    /// 回原点：GRBL 用 `$H`（需限位开关），Marlin 用 `G28`；不支持时退化为 `G0 X0 Y0`。
    pub fn home_machine(&mut self) {
        match self.fw() {
            Firmware::Grbl => self.send_gcode("$H"),
            Firmware::Marlin => self.send_gcode("G28"),
            Firmware::Ebb => self.send_gcode("HM,1"), // 部分 EBB 固件；无效时改用回零坐标
        }
        self.status = "已发送回原点指令".into();
    }

    /// 移动到机械原点 (0,0)。
    pub fn go_zero(&mut self) {
        let f = self.feed_rate;
        self.send_gcode(&format!("G90\nG0 X0 Y0 F{}", f));
        self.status = "已移动到原点 (0,0)".into();
    }

    /// 把「当前位置」设为坐标零点（不移动）：GRBL/Marlin 用 `G92 X0 Y0`，Marlin 也可用 `G10` 写入。
    pub fn set_zero_here(&mut self) {
        match self.fw() {
            Firmware::Marlin => self.send_gcode("G92 X0 Y0 ; 当前点设为零点"),
            _ => self.send_gcode("G92 X0 Y0 ; 当前点设为零点"),
        }
        self.status = "已把当前位置设为零点 (G92 X0 Y0)".into();
    }

    /// 取消坐标偏置，回到机器坐标系。
    pub fn clear_zero_offset(&mut self) {
        self.send_gcode("G92.1 ; 清除坐标偏置");
        self.status = "已清除坐标偏置".into();
    }

    /// 查询一次当前位置（GRBL `?` / Marlin `M114` / EBB `QS`）。
    pub fn read_position(&mut self) {
        match self.fw() {
            Firmware::Marlin => self.send_gcode("M114"),
            Firmware::Grbl => self.send_gcode("?"),
            Firmware::Ebb => self.send_gcode("QS"),
        }
        self.status = "已查询当前位置".into();
    }

    /// 把「坐标自动查询」的开关下发给设备线程（变了才发）。
    ///
    /// 固件换了查询指令也要跟着换（`?` / `M114` / `QS`），否则 Marlin 收 `?` 没反应。
    pub fn apply_status_poll(&mut self) {
        let key = (self.status_poll_ms, self.firmware_sel);
        if key == self.status_poll_key {
            return;
        }
        self.status_poll_key = key;
        let q = match self.fw() {
            Firmware::Marlin => "M114",
            Firmware::Ebb => "QS",
            Firmware::Grbl => "?",
        };
        self.device
            .send(crate::device::DevCmd::SetStatusPoll(q.to_string(), self.status_poll_ms));
    }

    /// 软复位（Ctrl-X，0x18）。
    pub fn soft_reset(&mut self) {
        self.send_gcode("\u{18}");
        self.status = "已发送软复位".into();
    }

    /// 抬笔 / 落笔（按当前笔控方式与 pen_up/pen_down 值）。
    pub fn pen_up_now(&mut self) {
        let g = match self.fw() {
            Firmware::Ebb => format!("SP,0,{}", self.ebb_max_pen as i64),
            _ => format!("G0 Z{}", self.pen_up),
        };
        self.send_gcode(&g);
        self.status = "抬笔".into();
    }
    pub fn pen_down_now(&mut self) {
        let g = match self.fw() {
            Firmware::Ebb => format!("SP,1,{}", self.ebb_max_pen as i64),
            _ => format!("G0 Z{}", self.pen_down),
        };
        self.send_gcode(&g);
        self.status = "落笔".into();
    }

    /// 画一个测试方框（验证方向、比例、笔控是否正常）。不改动用户内容。
    pub fn draw_test_frame(&mut self, w: f64, h: f64) {
        let g = Self::test_frame_gcode(w, h, self.pen_up, self.pen_down, self.feed_rate, self.fw());
        self.start_run(g);
        self.status = format!("开始画测试方框 {}×{}mm", w, h);
    }

    /// 绘制四角定位标记：一键在定位框（默认 200×280mm）四角各画一个 1cm L。
    ///
    /// **定位框贴在机器原点所在的那两个边上**（默认就是纸的左下角），并且
    /// G-code 的镜像基准用**纸张尺寸**而不是框尺寸。之前这两处都写成了
    /// 「左上角 + 用框高做基准」，于是：
    /// - 预览里标记画在纸的左上角，机器却画在左下角；
    /// - 标记的镜像基准比正文少 `纸张高 − 框高`（A4 是 17mm），
    ///   标记和正文就整整齐齐地错开 17mm。
    /// 现在标记与正文共用同一套纸张坐标，(0,0) 一定落在框的左下角。
    pub fn draw_corner_marks(&mut self) {
        let (w, h) = self.corner_frame_size();
        let (pw, ph) = (self.page_w_mm.max(1.0), self.page_h_mm.max(1.0));
        let (ox0, oy0) = self.origin_page_point();
        let o_right = ox0 > pw * 0.5;
        let o_bottom = oy0 > ph * 0.5;
        // 框贴在原点那一侧
        let fx = if o_right { (pw - w).max(0.0) } else { 0.0 };
        let fy = if o_bottom { (ph - h).max(0.0) } else { 0.0 };
        let paths: Vec<Polyline> = corner_mark_polylines(w, h, 10.0, (o_right, o_bottom))
            .into_iter()
            .map(|p| Polyline::new(p.pts.iter().map(|&(x, y)| (x + fx, y + fy)).collect()))
            .collect();
        let mut opts = self.gcode_options();
        opts.page_w = pw;
        opts.page_h = ph;
        // 以落笔前的位置为 (0,0)，画完由 generate 统一回 G0 X0 Y0。
        opts.use_current_origin = true;
        let g = wrobot_core::generate(&paths, &opts);
        self.start_run_with_preview(g, paths);
        self.status = format!(
            "开始绘制四角定位标记（{:.0}×{:.0}mm，长臂+斜线那角是原点 (0,0)，完成后回原点）",
            w, h
        );
    }

    /// 生成「测试方框」G-code。
    ///
    /// 必须以「当前位置」为原点（自带 `G92 X0 Y0`），否则 `G90` 绝对坐标会按机器原点解析
    /// → 超出机器行程（同类 BUG #56：见 `generate`）。EBB 不认 G92，跳过。
    pub fn test_frame_gcode(
        w: f64,
        h: f64,
        pen_up: f64,
        pen_down: f64,
        feed: f64,
        fw: Firmware,
    ) -> String {
        let (pu, pd) = (pen_up, pen_down);
        let f = feed;
        let mut g = String::new();
        g.push_str("G21 ; mm\nG90 ; absolute\n");
        if !matches!(fw, Firmware::Ebb) {
            g.push_str("G92 X0 Y0 ; 以当前位置为原点（后续坐标相对该点）\n");
        }
        g.push_str("$X\n");
        g.push_str(&format!("G0 X0 Y0 F{}\n", f));
        g.push_str(&format!("G0 Z{}\n", pu));
        g.push_str(&format!("G0 X{w} Y0 F{f}\n"));
        g.push_str(&format!("G0 Z{}\n", pd));
        g.push_str(&format!("G1 X{w} Y0 F{f}\n"));
        g.push_str(&format!("G1 X{w} Y{h} F{f}\n"));
        g.push_str(&format!("G1 X0 Y{h} F{f}\n"));
        g.push_str(&format!("G1 X0 Y0 F{f}\n"));
        g.push_str(&format!("G0 Z{}\n", pu));
        g.push_str("G0 X0 Y0 ; 回到方框起点（当前原点）\n");
        g
    }

    /// 设置运行流控（等待 ok / 运行前解锁 / ok 超时）。
    pub fn apply_flow_settings(&mut self) {
        self.device
            .set_flow(self.flow_wait_ok, self.flow_unlock, self.flow_ok_timeout);
        self.device.set_auto_zero(self.auto_set_zero);
        self.device
            .set_keepalive(self.keepalive_ms, self.auto_reconnect);
    }

    /// 设置链路保活（心跳间隔 / 自动重连）。
    ///
    /// 写字机蓝牙模块空闲几分钟就断链，靠定期心跳维持；断了能自动重连。
    pub fn apply_keepalive_settings(&mut self) {
        self.device
            .set_keepalive(self.keepalive_ms, self.auto_reconnect);
    }

    /// 清空当前模块的内容（一键清除）。
    pub fn clear_current_module(&mut self) {
        match self.tab {
            0 => {
                // 只清当前文本框（多文本框时不能一刀切全清）
                if let Some(b) = self.texts.get_mut(self.sel_text) {
                    b.text.clear();
                } else {
                    self.text.clear();
                }
                self.status = "已清空当前文本框的内容".into();
            }
            1 => {
                self.table_text.clear();
                self.status = "已清空表格内容".into();
            }
            2 => {
                self.grid_sample.clear();
                self.status = "已清空示范字".into();
            }
            3 => {
                self.draw_paths.clear();
                self.freehand.clear();
                self.draw_path.clear();
                self.status = "已清空手绘笔迹".into();
            }
            4 => {
                self.notes.clear();
                self.status = "已清空全部文本块".into();
            }
            5 => {
                self.clear_images();
                self.status = "已清除全部图片".into();
            }
            6 | 7 | 8 => {
                // 设备 / 测试 / 导入导出页：顶栏「清空内容」清的是页面内容，
                // 设备日志另有面板上的「清空日志」按钮。
                self.clear_all_content();
            }
            9 => {
                self.run_preview.clear();
                self.status = "已清空运行进度快照".into();
            }
            _ => self.status = "该页无可清除内容".into(),
        }
    }

    /// 清空**全部页面内容**（文字 / 表格 / 格子 / 手绘 / 笔记 / 图片），
    /// 保留字库、页面尺寸与设备连接。
    pub fn clear_all_content(&mut self) {
        self.text.clear();
        self.table_text.clear();
        self.grid_sample.clear();
        self.draw_paths.clear();
        self.freehand.clear();
        self.draw_path.clear();
        self.notes.clear();
        self.clear_images();
        self.preview.clear();
        self.run_preview.clear();
        self.confirm_clear = false;
        self.status = "已清空全部页面内容".into();
    }

    /// 新建工程：清空全部编辑内容（保留字库与设备连接）。
    pub fn new_project(&mut self) {
        self.text.clear();
        self.table_text.clear();
        self.grid_sample.clear();
        self.draw_paths.clear();
        self.freehand.clear();
        self.draw_path.clear();
        self.notes.clear();
        self.images.clear();
        self.sel_image = 0;
        self.gcode_text.clear();
        self.run_preview.clear();
        self.status = "已新建（清空全部编辑内容）".into();
    }
}
