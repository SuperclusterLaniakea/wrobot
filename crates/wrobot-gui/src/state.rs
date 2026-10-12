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
#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ImgDraw {
    /// 线框：提取轮廓边缘
    Wire = 0,
    /// 素描：横向影线填充暗部
    Sketch = 1,
    /// 高效：把相邻行的墨段串成连续笔画（蛇形）+ 最优路径排序
    Continuous = 2,
}

impl ImgDraw {
    pub fn names() -> &'static [&'static str] {
        &["线框（轮廓）", "素描（影线）", "高效（连续笔画）"]
    }

    /// 由下拉序号构造；越界时退回「素描」（保留旧存档兼容）。
    pub fn from_usize(i: usize) -> Self {
        match i {
            0 => ImgDraw::Wire,
            2 => ImgDraw::Continuous,
            _ => ImgDraw::Sketch,
        }
    }
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
    /// 基础尺寸（毫米，scale=1 时）
    pub natural_w: f64,
    pub natural_h: f64,
    /// 生成此笔迹所用的参数快照（用于判断是否需要重算）
    pub gen_key: u64,
}

impl ImageObject {
    /// 由导入参数计算「生成密钥」，任一参数变化即触发重算。
    pub fn key(&self) -> u64 {
        let mut h = self.bytes.len() as u64;
        h = h.wrapping_mul(31).wrapping_add(self.draw as u64);
        h = h.wrapping_mul(31).wrapping_add(self.step_px as u64);
        h = h.wrapping_mul(31).wrapping_add((self.threshold * 1000.0) as u64);
        h = h.wrapping_mul(31).wrapping_add((self.edge_threshold * 1000.0) as u64);
        h = h.wrapping_mul(31).wrapping_add(self.invert as u64);
        h = h.wrapping_mul(31).wrapping_add(self.natural_h as u64);
        h
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

/// 页面外侧标尺（顶部 + 左侧毫米刻度）默认开启。
fn default_ruler_on() -> bool {
    true
}

/// 自定义网格覆盖层默认关闭（保留原有 10mm 淡网格作为基线）。
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
    pub img_draw_type: usize,

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
    /// 自定义**网格覆盖层**：可调节行/列数量的浅色虚线网格（仅用于定位，不打印）。
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

    // —— 运行态 ——
    #[serde(skip)]
    pub tab: usize, // 0文字 1表格 2格子 3绘图 4笔记 5图片 6设备 7导入导出 8运行中
    #[serde(skip)]
    pub preview: Vec<Polyline>,
    /// 开始运行那一刻被「冻结」的页面笔迹（用于运行中进度页，与主编辑页互不干扰）。
    #[serde(skip)]
    pub run_preview: Vec<Polyline>,
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
            machine_w_mm: 200.0,
            machine_h_mm: 280.0,
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
            preview: Vec::new(),
            run_preview: Vec::new(),
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
    pub fn collect_polylines(&self, random: bool) -> Vec<Polyline> {
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
        let mut all: Vec<Polyline> = Vec::new();
        for (it, allow_random) in &tagged {
            if it.is_text() {
                let use_rng = random && *allow_random;
                let lay = if use_rng {
                    wrobot_core::layout::layout_item_rng(it, &stack, &opts, Some(&mut rng))
                } else {
                    wrobot_core::layout::layout_item(it, &stack, &opts)
                };
                all.extend(wrobot_core::render::render(&lay.placements, &stack));
            } else if it.is_shape() {
                if let Some(shapes) = &it.shapes {
                    for sp in shapes {
                        if sp.len() >= 2 {
                            all.push(Polyline::new(sp.clone()));
                        }
                    }
                }
            }
        }
        // 前景图片（可打印）：模块开关 + 每张图自己的「输出」开关都打开才并入
        if self.images_active() {
            for (i, img) in self.images.iter().enumerate() {
                if img.background || !img.out {
                    continue;
                }
                all.extend(self.image_polylines_idx(i));
            }
        }
        all
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
        if img.base.is_empty() || img.natural_w <= 0.0 || img.natural_h <= 0.0 {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(img.base.len());
        for pl in &img.base {
            let pts: Vec<(f64, f64)> = pl
                .pts
                .iter()
                .map(|&(px, py)| img.to_page(px, py))
                .collect();
            if pts.len() >= 2 {
                out.push(Polyline::new(pts));
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

    /// 由图片对象当前的参数构造生成选项。
    fn image_opts(img: &ImageObject) -> crate::io::ImageGenOpts {
        crate::io::ImageGenOpts {
            target_h_mm: img.natural_h.max(1.0), // 已按 target_h 生成，这里仅用于重新生成时保持高度
            step_px: img.step_px,
            threshold: img.threshold,
            edge_threshold: img.edge_threshold,
            invert: img.invert,
            draw_type: img.draw as usize,
        }
    }

    /// 按图片自身参数重新生成基础笔迹（参数变化时调用）。
    pub fn regenerate_image(&mut self, i: usize) {
        if i >= self.images.len() {
            return;
        }
        let key = self.images[i].key();
        if self.images[i].gen_key == key {
            return; // 参数未变，无需重算
        }
        // PDF 导入的矢量页没有原始图片字节，无法重算（也不该被重算）
        if self.images[i].bytes.is_empty() {
            self.images[i].gen_key = key;
            return;
        }
        let opts = Self::image_opts(&self.images[i]);
        if let Ok(r) = crate::io::image_to_polylines(&self.images[i].bytes, &opts) {
            self.images[i].base = r.polylines;
            self.images[i].natural_w = r.w_mm;
            self.images[i].natural_h = r.h_mm;
            self.images[i].gen_key = key;
        }
    }

    /// 导入一张图片为图片对象（foreground 或 background）。
    pub fn import_image_file(&mut self, path: &Path, background: bool) -> anyhow::Result<usize> {
        let data = std::fs::read(path)?;
        let opts = crate::io::ImageGenOpts {
            target_h_mm: self.img_target_h,
            step_px: self.img_step,
            threshold: self.img_thr,
            edge_threshold: self.img_edge,
            invert: self.img_invert,
            draw_type: self.img_draw_type,
        };
        let r = crate::io::image_to_polylines(&data, &opts)?;
        let n = r.polylines.len();
        // 默认放在页面左上角附近，避免与文字完全重叠
        let (x, y) = if background { (10.0, 10.0) } else { (30.0, 30.0) };
        let draw = ImgDraw::from_usize(self.img_draw_type);
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
            background,
            out: true,
            base: r.polylines,
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
                background: false,
                out: true,
                base: r.polylines,
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
                background: false,
                out: true,
                base,
                natural_w: (pg.w_mm * scale).max(1.0),
                natural_h: (pg.h_mm * scale).max(1.0),
                gen_key: 0,
            });
            self.sel_image = self.images.len() - 1;
        }
        for i in 0..self.images.len() {
            if self.images[i].bytes.is_empty() {
                self.images[i].gen_key = self.images[i].key();
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
        self.preview = self.collect_polylines(self.use_random);
    }

    /// 重新播种并重建预览——真正产生一次「新的」随机手写变形。
    pub fn rebuild_preview_random(&mut self) {
        self.rand_seed ^= 0x9E3779B97F4A7C15u64;
        self.rand_seed = self.rand_seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.use_random = true;
        self.preview = self.collect_polylines(true);
    }

    /// 生成 G-code 文本（与当前预览使用同一随机种子，保证所见即所得）。
    pub fn generate_gcode(&mut self) -> String {
        let all = self.collect_polylines(self.use_random);
        let gopts = self.gcode_options();
        wrobot_core::generate(&all, &gopts)
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

    /// 扫描本机串口：列出**全部登记端口**并标注能否打开。
    ///
    /// 旧实现只返回「能打开」的口，蓝牙已配对但未建立 SPP 链路时列表为空，
    /// 用户只能瞎猜端口号——这是「连不到 COM」的诱因之一。
    pub fn scan_ports(&mut self) -> usize {
        // 用带类型信息的枚举：蓝牙 / 有线串口线分开，排查建议也就不一样
        let mut entries = wrobot_core::enumerate_ports();
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
            // 按类型分组汇报：一眼看出「有没有串口线」「蓝牙链路建没建」
            let wired = wrobot_core::wired_ports();
            let bt = wrobot_core::bluetooth_ports();
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
            if !self.serial_ports.contains(&self.serial_port) {
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
        self.run_preview = self.collect_polylines(self.use_random);
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
