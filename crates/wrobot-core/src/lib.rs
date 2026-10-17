//! WritingRobot 核心引擎（Rust 重写）。
//!
//! 流水线：工程 `.json` → 排版 → 笔画渲染 → G-code / SVG。
//!
//! ```
//! use wrobot_core::doc::Document;
//! use wrobot_core::font::{BuiltinFont, FontStack, StrokeFont};
//! use wrobot_core::layout::{layout_item, LayoutOptions};
//! use wrobot_core::render::render;
//! use wrobot_core::gcode::{generate, GcodeOptions};
//!
//! let json = r#"{"orientation":0,"width":17.5,"axesPosX":0,"axesPosY":0,
//!   "items":[{"type":"text","x":0,"y":0,"width":200,"height":50,"rotation":0,
//!   "text":"HELLO","contentFormat":"{\"charset\":\"GBK\",\"firstLineAlign\":\"LEFT\"}",
//!   "pageInfo":{"fontSize":100,"lineGap":8,"pageWidth":100,"pageHeight":30,
//!   "contentAlign":"Left","fontPath":[]}}]}"#;
//! let doc = Document::from_json(json).unwrap();
//! let fonts: Vec<Box<dyn StrokeFont>> = vec![Box::new(BuiltinFont::new())];
//! let stack = FontStack::new(fonts);
//! let opts = LayoutOptions::default();
//! let lay = layout_item(&doc.items[0], &stack, &opts);
//! let paths = render(&lay.placements, &stack);
//! assert!(!paths.is_empty());
//! let gcode = generate(&paths, &GcodeOptions::default());
//! assert!(gcode.contains("G1"));
//! ```

pub mod bluetooth;
pub mod config;
pub mod doc;
pub mod dxf;
pub mod error;
pub mod font;
pub mod gcode;
pub mod layout;
pub mod pdf;
pub mod pdfimport;
pub mod render;
pub mod serial;
pub mod svg;
pub mod svg_import;

pub use config::{MachineConfig, Firmware, PenMode};
pub use error::{Result, WError};
pub use font::{BuiltinFont, FontStack, StrokeFont};
pub use gcode::{
    decimate, decimate_polylines, generate, gcode_xy_bounds, GcodeOptions, DEFAULT_MIN_STEP,
};
pub use pdf::to_pdf;
pub use pdfimport::{
    import_page, page_count as pdf_page_count, PdfError, PdfImage, PdfImageData, PdfImportOpts,
    PdfPage, PdfRect, PdfTextRun,
};
pub use layout::{layout_document, layout_item, LayoutOptions, Placement};
pub use render::{
    bounds_all, cross_grid, fit_to_page, grid, optimize_path_order, optimize_path_order_from,
    path_travel, rect, segment, total_length, Polyline,
};
pub use svg::to_svg;
pub use dxf::{import_dxf, DxfError};
pub use svg_import::{import_svg, SvgError};
pub use bluetooth::{
    bt_com_bindings, bt_devices, bt_find, bt_radios, bt_report, bt_set_spp, bt_watch, BtComBinding,
    BtDevice,
};
pub use serial::{
    bluetooth_ports, diag_port_steps, enumerate_ports, err_hint, garbage_ratio,
    list_serial_ports, registered_ports, sanitize_bytes, scan_ports_detailed, try_open_report,
    wired_ports, wire_test, PortEntry, PortKind, SerialPort, COMMON_BAUDS,
};

use doc::Document;

/// 把整个工程展开为统一的笔尖路径（页面毫米坐标）。
///
/// - 文字元素：排版 → 渲染为笔画折线；
/// - 线条/形状/图片元素：直接取其 `shapes` 字段中的绝对毫米坐标折线。
///
/// 这是 `document_to_gcode` / `document_to_svg` 的共享底层，也是 GUI
/// 各模块（表格/格子/绘图/笔记）统一的产物形态——最终都汇聚为 `Vec<Polyline>`。
pub fn document_to_polylines(
    doc: &Document,
    stack: &FontStack,
    layout_opts: &LayoutOptions,
) -> Vec<Polyline> {
    let mut all: Vec<Polyline> = Vec::new();
    for it in &doc.items {
        if it.is_text() {
            let lay = layout_item(it, stack, layout_opts);
            all.extend(render::render(&lay.placements, stack));
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
    all
}

/// 一次性把工程转成 G-code。
pub fn document_to_gcode(
    doc: &Document,
    stack: &FontStack,
    layout_opts: &LayoutOptions,
    gcode_opts: &GcodeOptions,
) -> String {
    let all = document_to_polylines(doc, stack, layout_opts);
    generate(&all, gcode_opts)
}

/// 一次性把工程转成 SVG 预览。
pub fn document_to_svg(
    doc: &Document,
    stack: &FontStack,
    layout_opts: &LayoutOptions,
) -> String {
    let all = document_to_polylines(doc, stack, layout_opts);
    to_svg(&all, 5.0)
}

/// 一次性把工程转成单页矢量 PDF（页面尺寸以毫米给出）。
pub fn document_to_pdf(
    doc: &Document,
    stack: &FontStack,
    layout_opts: &LayoutOptions,
    page_w_mm: f64,
    page_h_mm: f64,
) -> Vec<u8> {
    let all = document_to_polylines(doc, stack, layout_opts);
    to_pdf(&all, page_w_mm, page_h_mm)
}
