//! 极简 PDF 导出（纯 Rust，无压缩、无外部依赖）。
//!
//! 把笔迹折线写成单页矢量 PDF：每个折线用 `m` 起点 + 若干 `l` 线段 + `S` 描边。
//! 坐标以 PDF 点为单位（1 mm = 72/25.4 pt），页面原点在左下，故 Y 需翻转。

use crate::render::Polyline;

const PT_PER_MM: f64 = 72.0 / 25.4;

/// 把笔迹导出为单页 PDF 字节流。
pub fn to_pdf(paths: &[Polyline], page_w_mm: f64, page_h_mm: f64) -> Vec<u8> {
    let w = page_w_mm * PT_PER_MM;
    let h = page_h_mm * PT_PER_MM;

    // 内容流
    let mut content = String::new();
    content.push_str("0.08 w 0 0 0 RG\n"); // 线宽 / 黑色描边
    for pl in paths {
        let pts = &pl.pts;
        if pts.len() < 2 {
            continue;
        }
        let (x0, y0) = to_pt(pts[0].0, pts[0].1, h);
        content.push_str(&format!("{:.2} {:.2} m\n", x0, y0));
        for p in &pts[1..] {
            let (x, y) = to_pt(p.0, p.1, h);
            content.push_str(&format!("{:.2} {:.2} l\n", x, y));
        }
        content.push_str("S\n");
    }
    let content_bytes = content.as_bytes();

    // 逐个对象写入，同时记录偏移
    let mut out: Vec<u8> = Vec::new();
    let mut offsets: Vec<usize> = Vec::new();

    out.extend_from_slice(b"%PDF-1.4\n");

    offsets.push(out.len());
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    offsets.push(out.len());
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

    offsets.push(out.len());
    out.extend_from_slice(
        format!(
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {:.2} {:.2}] /Contents 4 0 R /Resources << >> >>\nendobj\n",
            w, h
        )
        .as_bytes(),
    );

    offsets.push(out.len());
    out.extend_from_slice(
        format!("4 0 obj\n<< /Length {} >>\nstream\n", content_bytes.len()).as_bytes(),
    );
    out.extend_from_slice(content_bytes);
    out.extend_from_slice(b"\nendstream\nendobj\n");

    // xref 表
    let xref_off = out.len();
    let n = offsets.len() + 1; // +1 为对象 0
    out.extend_from_slice(format!("xref\n0 {}\n", n).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for off in &offsets {
        out.extend_from_slice(format!("{:010} 00000 n \n", off).as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            n, xref_off
        )
        .as_bytes(),
    );

    out
}

/// 页面毫米坐标（左上原点，Y 向下）→ PDF 点坐标（左下原点）。
#[inline]
fn to_pt(x_mm: f64, y_mm: f64, page_h_pt: f64) -> (f64, f64) {
    (x_mm * PT_PER_MM, page_h_pt - y_mm * PT_PER_MM)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::Polyline;

    #[test]
    fn pdf_has_valid_structure() {
        let paths = vec![Polyline::new(vec![(10.0, 10.0), (50.0, 10.0), (50.0, 40.0)])];
        let bytes = to_pdf(&paths, 175.0, 255.0);
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.starts_with("%PDF-1.4"), "缺少 PDF 头");
        assert!(s.contains("/Type /Catalog"), "缺少 Catalog");
        assert!(s.contains("/Type /Page"), "缺少 Page");
        assert!(s.contains("stream"), "缺少内容流");
        assert!(s.trim_end().ends_with("%%EOF"), "缺少 EOF 标记");
        assert!(s.contains(" m\n"), "缺少 moveto");
        assert!(s.contains("S\n"), "缺少 stroke");
        // 尺寸换算：175mm -> 496.06pt
        assert!(s.contains("496.06"), "页面宽度换算不正确: {}", &s[..200]);
    }
}
