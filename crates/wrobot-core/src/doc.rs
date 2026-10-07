//! 工程存档（`.json`）文档模型。
//!
//! 字段与奎享雕刻导出的工程文件一一对应（camelCase）。
//! 未知字段通过 `extra` 保留，保证读-改-写不丢信息。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use crate::error::Result;

/// 顶层工程对象。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Document {
    /// 页面方向：0 纵向，1 横向（待与真机校准）
    #[serde(default)]
    pub orientation: i32,
    /// 页面宽度（单位：厘米）
    #[serde(default)]
    pub width: f64,
    /// 页面高度（单位：厘米，部分工程缺失）
    #[serde(default)]
    pub height: Option<f64>,
    /// 红蓝交叉线（落笔原点）X，工程坐标单位
    #[serde(default)]
    pub axes_pos_x: f64,
    /// 红蓝交叉线（落笔原点）Y，工程坐标单位
    #[serde(default)]
    pub axes_pos_y: f64,
    /// 画布元素
    #[serde(default)]
    pub items: Vec<Item>,
    /// 未识别字段，原样保留
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

/// 画布元素（文字框 / 图片 / 表格 / 线条）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    /// 元素类型：`text` / `image` / `table` / `line`
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub name: Option<String>,
    /// 左上角 X（工程坐标单位，像素 @96dpi）
    #[serde(default)]
    pub x: f64,
    /// 左上角 Y
    #[serde(default)]
    pub y: f64,
    /// 宽度
    #[serde(default)]
    pub width: f64,
    /// 高度
    #[serde(default)]
    pub height: f64,
    /// 旋转（弧度）
    #[serde(default)]
    pub rotation: f64,
    /// 文本内容
    #[serde(default)]
    pub text: Option<String>,
    /// 线条/形状折线路径（页面毫米坐标），用于 表格/格子/绘图 模块。
    /// 每个内层 `Vec<(x,y)>` 是一条连续笔迹（落笔→抬笔）。
    #[serde(default)]
    pub shapes: Option<Vec<Vec<(f64, f64)>>>,
    /// 内嵌 JSON 字符串：`{"charset":"GBK","autoGp":false,"gpNum":1,"firstLineAlign":"LEFT"}`
    #[serde(default)]
    pub content_format: Option<String>,
    /// 排版参数
    #[serde(default)]
    pub page_info: Option<PageInfo>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

/// 文字框排版参数。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PageInfo {
    #[serde(default)]
    pub left_gap: f64,
    #[serde(default)]
    pub right_gap: f64,
    #[serde(default)]
    pub top_gap: f64,
    #[serde(default)]
    pub bottom_gap: f64,
    #[serde(default)]
    pub align_offset_x: f64,
    #[serde(default)]
    pub align_offset_y: f64,
    #[serde(default)]
    pub align_offset_x_max: f64,
    #[serde(default)]
    pub align_offset_y_max: f64,
    #[serde(default)]
    pub base_line: f64,
    /// 水平对齐：`Left` / `Center` / `Right`
    #[serde(default)]
    pub content_align: Option<String>,
    #[serde(default)]
    pub rotation: f64,
    /// 行距
    #[serde(default)]
    pub line_gap: f64,
    /// 行距附加偏移（逼真化）
    #[serde(default)]
    pub line_gap_offset: f64,
    /// 字间距
    #[serde(default)]
    pub space: f64,
    /// 可写区域高度（毫米）
    #[serde(default)]
    pub page_height: f64,
    /// 可写区域宽度（毫米）
    #[serde(default)]
    pub page_width: f64,
    /// 文字块在可写区域内垂直居中（用于表格单元格 / 格子示范字）
    #[serde(default)]
    pub vcenter: bool,
    /// 逐字按「自身墨迹框」居中（用于格子示范字：让每个字都正落在格心）。
    /// 关闭时只按字库的典型墨迹框整体补偿（行文本稳定、不跳动）。
    #[serde(default)]
    pub ink_center: bool,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default)]
    pub line_num: i32,
    /// 字号（基准 100）
    #[serde(default)]
    pub font_size: f64,
    /// 字体栈：按顺序回退 / 混用
    #[serde(default)]
    pub font_path: Vec<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

/// `contentFormat` 内嵌对象。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ContentFormat {
    /// 字符集，中文工程为 `GBK`
    #[serde(default)]
    pub charset: Option<String>,
    /// 是否自动选字（同一字多字形轮换，逼真化）
    #[serde(default)]
    pub auto_gp: bool,
    /// 字形组数量
    #[serde(default)]
    pub gp_num: i32,
    /// 段首对齐：`LEFT` / `CENTER` / `TAB`
    #[serde(default)]
    pub first_line_align: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

/// 水平对齐方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

impl Align {
    pub fn parse(s: Option<&str>) -> Align {
        match s.unwrap_or("Left").to_ascii_lowercase().as_str() {
            "center" | "centre" => Align::Center,
            "right" => Align::Right,
            _ => Align::Left,
        }
    }
}

/// 段首处理方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstLine {
    /// 不缩进
    Left,
    /// 首行居中（常用于标题）
    Center,
    /// 首行缩进（中文段落，两个字符宽）
    Tab,
}

impl FirstLine {
    pub fn parse(s: Option<&str>) -> FirstLine {
        match s.unwrap_or("LEFT").to_ascii_uppercase().as_str() {
            "CENTER" | "CENTRE" => FirstLine::Center,
            "TAB" => FirstLine::Tab,
            _ => FirstLine::Left,
        }
    }
}

impl Item {
    /// 解析内嵌的 `contentFormat`。
    pub fn content_format(&self) -> ContentFormat {
        match &self.content_format {
            Some(s) => serde_json::from_str(s).unwrap_or_default(),
            None => ContentFormat::default(),
        }
    }

    /// 段首处理方式。
    pub fn first_line(&self) -> FirstLine {
        FirstLine::parse(self.content_format().first_line_align.as_deref())
    }

    /// 水平对齐方式。
    pub fn align(&self) -> Align {
        Align::parse(self.page_info.as_ref().and_then(|p| p.content_align.as_deref()))
    }

    pub fn text(&self) -> &str {
        self.text.as_deref().unwrap_or("")
    }

    pub fn is_text(&self) -> bool {
        self.r#type == "text"
    }

    /// 是否为线条/形状元素（直接以绝对毫米坐标描述笔迹）。
    pub fn is_shape(&self) -> bool {
        matches!(self.r#type.as_str(), "line" | "shape" | "image")
            && self.shapes.as_ref().map(|s| !s.is_empty()).unwrap_or(false)
    }
}

impl Document {
    /// 从 JSON 文本解析。
    pub fn from_json(s: &str) -> Result<Document> {
        Ok(serde_json::from_str(s)?)
    }

    /// 从文件解析（UTF-8，失败时回退 GBK）。
    pub fn from_file(path: &std::path::Path) -> Result<Document> {
        let bytes = std::fs::read(path)?;
        match String::from_utf8(bytes) {
            Ok(s) => Self::from_json(&s),
            Err(e) => {
                // 回退：按 GBK 解码
                let bytes = e.into_bytes();
                let (s, _, _) = encoding_gbk_decode(&bytes);
                Self::from_json(&s)
            }
        }
    }

    /// 序列化为 JSON（紧凑）。
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    /// 序列化为 JSON（可读）。
    pub fn to_json_pretty(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// 仅文字元素。
    pub fn text_items(&self) -> impl Iterator<Item = &Item> {
        self.items.iter().filter(|i| i.is_text())
    }

    /// 工程用到的全部字体文件名（去重、保序）。
    pub fn fonts_used(&self) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for it in &self.items {
            if let Some(pi) = &it.page_info {
                for f in &pi.font_path {
                    if !v.contains(f) {
                        v.push(f.clone());
                    }
                }
            }
        }
        v
    }

    /// 统计字符总数（不含换行）。
    pub fn char_count(&self) -> usize {
        self.text_items()
            .map(|i| i.text().chars().filter(|c| *c != '\n').count())
            .sum()
    }
}

/// 极简 GBK 解码（仅用于兜底；中文工程实际为 UTF-8）。
fn encoding_gbk_decode(bytes: &[u8]) -> (String, usize, usize) {
    // 使用标准库无法直接解 GBK，这里做 lossy 的 Latin-1 兜底，
    // 实际情况中工程文件为 UTF-8，不会走到这里。
    let s: String = bytes.iter().map(|&b| b as char).collect();
    (s, 0, 0)
}
