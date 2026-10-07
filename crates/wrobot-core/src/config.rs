//! 机器/固件参数，对应 `default.properties` 与 `config.properties`。

use crate::error::{Result, WError};
use std::collections::HashMap;
use std::path::Path;

/// 写字机固件类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Firmware {
    /// GRBL：Z 轴抬落笔
    Grbl,
    /// Marlin：舵机或 Z 轴
    Marlin,
    /// EBB（EiBotBoard）：SP 指令控笔
    Ebb,
}

impl Firmware {
    pub fn parse(s: &str) -> Firmware {
        match s.to_ascii_lowercase().as_str() {
            "marlin" => Firmware::Marlin,
            "ebb" => Firmware::Ebb,
            _ => Firmware::Grbl,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Firmware::Grbl => "grbl",
            Firmware::Marlin => "marlin",
            Firmware::Ebb => "ebb",
        }
    }
}

/// 抬落笔实现方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PenMode {
    /// 用 Z 轴高度控制
    Z,
    /// 用舵机角度控制
    Servo,
    /// 用专用指令控制（EBB）
    Command,
}

/// 完整机器配置。
#[derive(Debug, Clone)]
pub struct MachineConfig {
    pub firmware: Firmware,
    pub pen_mode: PenMode,
    /// 绘图速度 mm/min
    pub feed_rate: f64,
    /// 点动速度
    pub jog_speed: f64,
    /// 落笔后暂停（秒）
    pub tool_on_delay: f64,
    /// 抬笔前暂停（秒）
    pub tool_off_delay: f64,
    /// 抬笔位置（GRBL 用 mm / 舵机用角度 / EBB 用参数）
    pub pen_up_pos: f64,
    /// 落笔位置
    pub pen_down_pos: f64,
    /// Z 轴速度
    pub z_speed: f64,
    /// EBB：毫米→步数
    pub mm_to_steps: f64,
    /// 坐标系：对调 XY
    pub swap_xy: bool,
    /// 反转 X
    pub reverse_x: bool,
    /// 反转 Y
    pub reverse_y: bool,
    /// 原点位置 0左上 1右上 2左下 3右下
    pub origin: u8,
    /// 结束自动休眠
    pub auto_sleep: bool,
    /// 单步模式
    pub single_step: bool,
    /// 自动设置原点
    pub auto_set_zero: bool,
    /// 开始前自定义 G 代码
    pub custom_start: String,
    /// 结束后自定义 G 代码
    pub custom_end: String,
    /// 未识别的其它键
    pub extra: HashMap<String, String>,
}

impl Default for MachineConfig {
    fn default() -> Self {
        MachineConfig {
            firmware: Firmware::Grbl,
            pen_mode: PenMode::Z,
            feed_rate: 10000.0,
            jog_speed: 10000.0,
            tool_on_delay: 0.0,
            tool_off_delay: 0.0,
            pen_up_pos: 0.0,
            pen_down_pos: 5.0,
            z_speed: 10000.0,
            mm_to_steps: 100.0,
            swap_xy: false,
            reverse_x: false,
            reverse_y: true,
            origin: 0,
            auto_sleep: false,
            single_step: false,
            auto_set_zero: true,
            custom_start: String::new(),
            custom_end: String::new(),
            extra: HashMap::new(),
        }
    }
}

/// 解析 `key=value`（`#` 注释）的属性文本。
pub fn parse_properties(text: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') || t.starts_with("//") {
            continue;
        }
        if let Some((k, v)) = t.split_once('=') {
            m.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    m
}

fn g(m: &HashMap<String, String>, k: &str, d: f64) -> f64 {
    m.get(k).and_then(|v| v.parse::<f64>().ok()).unwrap_or(d)
}
fn b(m: &HashMap<String, String>, k: &str, d: bool) -> bool {
    m.get(k)
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "true" | "1" | "yes"))
        .unwrap_or(d)
}

impl MachineConfig {
    /// 从属性集合构建（对应 default.properties 的键名）。
    pub fn from_props(m: HashMap<String, String>) -> MachineConfig {
        let mut c = MachineConfig::default();
        c.firmware = Firmware::parse(m.get("fixed_machine_type").map(|s| s.as_str()).unwrap_or("grbl"));
        // grb1 / marlin / ebb
        match m
            .get("fixed_machine_type")
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default()
            .as_str()
        {
            s if s.starts_with("marlin") => c.firmware = Firmware::Marlin,
            s if s.starts_with("ebb") => c.firmware = Firmware::Ebb,
            _ => c.firmware = Firmware::Grbl,
        }
        c.feed_rate = g(&m, "feed_rate", c.feed_rate);
        c.jog_speed = g(&m, "jog_speed", c.jog_speed);
        c.tool_on_delay = g(&m, "tool_on_delay", c.tool_on_delay);
        c.tool_off_delay = g(&m, "tool_off_delay", c.tool_off_delay);
        c.z_speed = g(&m, "zSpeed", g(&m, "marlin_z_speed", c.z_speed));
        c.mm_to_steps = g(&m, "mm_to_steps", c.mm_to_steps);
        c.auto_sleep = b(&m, "autoSleep", c.auto_sleep);
        c.single_step = b(&m, "single_step_command", c.single_step);
        c.auto_set_zero = b(&m, "auto_set_zero", c.auto_set_zero);
        c.swap_xy = b(&m, "axe_rev_xy", c.swap_xy);
        c.reverse_x = b(&m, "axe_reverse_x", c.reverse_x);
        c.reverse_y = b(&m, "axe_reverse_y", c.reverse_y);
        c.origin = m
            .get("axe_pos")
            .and_then(|s| s.parse::<u8>().ok())
            .unwrap_or(c.origin);
        c.custom_start = m.get("custom_start_gcode").cloned().unwrap_or_default();
        c.custom_end = m.get("custom_end_gcode").cloned().unwrap_or_default();

        match c.firmware {
            Firmware::Grbl => {
                c.pen_mode = if m.get("grbl_type").map(|s| s.to_ascii_lowercase()).unwrap_or_default() == "servo" {
                    PenMode::Servo
                } else {
                    PenMode::Z
                };
                c.pen_down_pos = g(&m, "zOn", 5.0);
                c.pen_up_pos = g(&m, "zOff", 0.0);
                if c.pen_mode == PenMode::Servo {
                    c.pen_down_pos = g(&m, "pen_down_pos", 18000.0);
                    c.pen_up_pos = g(&m, "pen_up_pos", 12000.0);
                }
            }
            Firmware::Marlin => {
                c.pen_mode = if m.get("marlin_type").map(|s| s.to_ascii_lowercase()).unwrap_or_default() == "servo" {
                    PenMode::Servo
                } else {
                    PenMode::Z
                };
                if c.pen_mode == PenMode::Servo {
                    c.pen_up_pos = g(&m, "marlin_pen_up", 50.0);
                    c.pen_down_pos = g(&m, "marlin_pen_down", 0.0);
                } else {
                    c.pen_up_pos = g(&m, "marlin_pen_up_mm", 0.0);
                    c.pen_down_pos = g(&m, "marlin_pen_down_mm", 2.0);
                }
            }
            Firmware::Ebb => {
                c.pen_mode = PenMode::Command;
                c.pen_up_pos = g(&m, "pen_up_pos", 12000.0);
                c.pen_down_pos = g(&m, "pen_down_pos", 18000.0);
            }
        }
        c.extra = m;
        c
    }

    /// 从 default.properties 文件加载。
    pub fn from_file(path: &Path) -> Result<MachineConfig> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| WError::Config(format!("read {}: {}", path.display(), e)))?;
        Ok(MachineConfig::from_props(parse_properties(&text)))
    }
}
