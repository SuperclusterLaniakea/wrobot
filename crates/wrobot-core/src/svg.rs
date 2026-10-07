//! SVG 预览导出：用于校验排版结果（与 G-code 同源）。

use crate::render::{bounds_all, Polyline};

/// 生成 SVG 字符串。
pub fn to_svg(paths: &[Polyline], margin: f64) -> String {
    let (x0, y0, x1, y1) = bounds_all(paths);
    let w = (x1 - x0 + margin * 2.0).max(1.0);
    let h = (y1 - y0 + margin * 2.0).max(1.0);
    let mut s = String::new();
    s.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{:.2}mm\" height=\"{:.2}mm\" viewBox=\"{:.2} {:.2} {:.2} {:.2}\">\n",
        w, h, x0 - margin, y0 - margin, w, h
    ));
    s.push_str("<rect width=\"100%\" height=\"100%\" fill=\"white\"/>\n");
    s.push_str("<g fill=\"none\" stroke=\"#111\" stroke-width=\"0.35\" stroke-linecap=\"round\" stroke-linejoin=\"round\">\n");
    for p in paths {
        if p.pts.len() < 2 {
            continue;
        }
        s.push_str("<polyline points=\"");
        let parts: Vec<String> = p
            .pts
            .iter()
            .map(|(x, y)| format!("{:.2},{:.2}", x, y))
            .collect();
        s.push_str(&parts.join(" "));
        s.push_str("\"/>\n");
    }
    s.push_str("</g>\n</svg>\n");
    s
}
