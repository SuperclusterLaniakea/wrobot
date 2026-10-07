//! 端到端流水线测试：工程 JSON → 排版 → 渲染 → G-code / SVG。

use std::path::Path;

use wrobot_core::config::MachineConfig;
use wrobot_core::doc::Document;
use wrobot_core::font::{BuiltinFont, FontStack, StrokeFont};
use wrobot_core::gcode::{count_pen_cycles, generate, GcodeOptions};
use wrobot_core::layout::{layout_item, LayoutOptions};
use wrobot_core::render::{bounds_all, render, total_length};
use wrobot_core::{document_to_gcode, document_to_svg};

fn stack() -> FontStack {
    let fonts: Vec<Box<dyn StrokeFont>> = vec![Box::new(BuiltinFont::new())];
    FontStack::new(fonts)
}

fn sample(name: &str) -> Option<std::path::PathBuf> {
    let p = Path::new(r"D:\WritingRobot").join(name);
    if p.exists() {
        Some(p)
    } else {
        None
    }
}

#[test]
fn parses_all_sample_documents() {
    for name in ["教案1.json", "教案2.json", "教案3.json", "思想汇报.json"] {
        let Some(p) = sample(name) else {
            eprintln!("skip missing {name}");
            continue;
        };
        let doc = Document::from_file(&p).expect("parse");
        assert!(!doc.items.is_empty(), "{name} has no items");
        assert!(doc.width > 0.0, "{name} width");
        let texts: Vec<_> = doc.text_items().collect();
        assert!(!texts.is_empty(), "{name} has no text item");
        assert!(doc.char_count() > 0, "{name} has no chars");
    }
}

#[test]
fn round_trip_preserves_items() {
    let Some(p) = sample("教案1.json") else {
        eprintln!("skip");
        return;
    };
    let doc = Document::from_file(&p).unwrap();
    let json = doc.to_json().unwrap();
    let doc2 = Document::from_json(&json).unwrap();
    assert_eq!(doc.items.len(), doc2.items.len());
    assert_eq!(doc.char_count(), doc2.char_count());
    // 字体栈要保持
    assert_eq!(doc.fonts_used(), doc2.fonts_used());
}

#[test]
fn content_format_and_page_info_parsed() {
    let Some(p) = sample("思想汇报.json") else {
        eprintln!("skip");
        return;
    };
    let doc = Document::from_file(&p).unwrap();
    let it = &doc.items[0];
    let cf = it.content_format();
    assert_eq!(cf.charset.as_deref(), Some("GBK"));
    assert_eq!(cf.first_line_align.as_deref(), Some("TAB"));
    let pi = it.page_info.as_ref().expect("pageInfo");
    assert!(pi.page_width > 0.0 && pi.page_height > 0.0);
    assert!(!pi.font_path.is_empty());
}

#[test]
fn pipeline_produces_gcode() {
    let Some(p) = sample("教案1.json") else {
        eprintln!("skip");
        return;
    };
    let doc = Document::from_file(&p).unwrap();
    let st = stack();
    let opts = LayoutOptions::default();
    let g = document_to_gcode(&doc, &st, &opts, &GcodeOptions::default());
    assert!(g.contains("G21"));
    assert!(g.contains("G90"));
    assert!(g.contains("G1"), "应有落笔绘制指令");
    assert!(g.contains("G0"), "应有抬笔快移指令");
    // 抬落笔次数应与笔画数一致
    let (ups, downs) = count_pen_cycles(&g, &GcodeOptions::default());
    assert!(downs > 0);
    assert!(ups >= downs, "每次落笔都要有对应抬笔");
}

#[test]
fn pipeline_produces_svg() {
    let Some(p) = sample("教案3.json") else {
        eprintln!("skip");
        return;
    };
    let doc = Document::from_file(&p).unwrap();
    let svg = document_to_svg(&doc, &stack(), &LayoutOptions::default());
    assert!(svg.starts_with("<svg"));
    assert!(svg.contains("<polyline"));
    assert!(svg.trim_end().ends_with("</svg>"));
}

#[test]
fn layout_respects_page_width_and_wraps() {
    let json = r#"{"orientation":0,"width":17.5,"axesPosX":0,"axesPosY":0,
      "items":[{"type":"text","x":0,"y":0,"width":300,"height":100,"rotation":0,
      "text":"ABCDEFGHIJKLMNOPQRSTUVWXYZ",
      "contentFormat":"{\"charset\":\"GBK\",\"firstLineAlign\":\"LEFT\"}",
      "pageInfo":{"fontSize":100,"lineGap":8,"pageWidth":50,"pageHeight":100,
      "contentAlign":"Left","fontPath":[]}}]}"#;
    let doc = Document::from_json(json).unwrap();
    let lay = layout_item(&doc.items[0], &stack(), &LayoutOptions::default());
    // pageWidth=50mm，字符宽约 8*0.62≈4.96mm → 每行约 10 个字，26 字母应换行
    assert!(lay.lines >= 2, "应发生换行，实际 {} 行", lay.lines);
    assert_eq!(lay.placements.len(), 26);
    let paths = render(&lay.placements, &stack());
    assert!(!paths.is_empty());
    let (x0, _y0, x1, _y1) = bounds_all(&paths);
    assert!(x1 - x0 <= 60.0, "宽度应受 pageWidth 约束: {}", x1 - x0);
}

#[test]
fn deterministic_layout_without_rng() {
    let json = r#"{"items":[{"type":"text","x":10,"y":20,"width":300,"height":100,
      "rotation":0,"text":"HELLO WORLD",
      "contentFormat":"{\"firstLineAlign\":\"LEFT\"}",
      "pageInfo":{"fontSize":100,"lineGap":8,"pageWidth":200,"pageHeight":100,
      "contentAlign":"Left","fontPath":[]}}]}"#;
    let doc = Document::from_json(json).unwrap();
    let st = stack();
    let a = layout_item(&doc.items[0], &st, &LayoutOptions::default());
    let b = layout_item(&doc.items[0], &st, &LayoutOptions::default());
    assert_eq!(a.placements.len(), b.placements.len());
    for (p, q) in a.placements.iter().zip(b.placements.iter()) {
        assert_eq!(p.ch, q.ch);
        assert!((p.x - q.x).abs() < 1e-9);
        assert!((p.y - q.y).abs() < 1e-9);
    }
}

#[test]
fn reverse_y_flips_coordinates() {
    let mut o = GcodeOptions::default();
    o.reverse_y = true;
    o.page_h = 100.0;
    let (_, y) = o.transform(10.0, 20.0);
    assert!((y - 80.0).abs() < 1e-9, "y 应翻转为 80, 实际 {}", y);
    o.reverse_y = false;
    let (_, y2) = o.transform(10.0, 20.0);
    assert!((y2 - 20.0).abs() < 1e-9);
}

#[test]
fn three_firmwares_emit_pen_commands() {
    let json = r#"{"items":[{"type":"text","x":0,"y":0,"width":100,"height":40,
      "rotation":0,"text":"AB",
      "contentFormat":"{\"firstLineAlign\":\"LEFT\"}",
      "pageInfo":{"fontSize":100,"lineGap":8,"pageWidth":100,"pageHeight":40,
      "contentAlign":"Left","fontPath":[]}}]}"#;
    let doc = Document::from_json(json).unwrap();
    let st = stack();
    let lay = layout_item(&doc.items[0], &st, &LayoutOptions::default());
    let paths = render(&lay.placements, &st);
    assert!(!paths.is_empty());

    for fw in ["grbl", "marlin", "ebb"] {
        let cfgp = Path::new(r"D:\WritingRobot\奎享雕刻\default.properties");
        let o = if cfgp.exists() {
            let mut mc = MachineConfig::from_file(cfgp).unwrap();
            mc.firmware = wrobot_core::Firmware::parse(fw);
            GcodeOptions::from_machine(&mc)
        } else {
            let mut o = GcodeOptions::default();
            o.firmware = wrobot_core::Firmware::parse(fw);
            o
        };
        let g = generate(&paths, &o);
        assert!(!g.is_empty(), "{fw} 输出为空");
        let (ups, downs) = count_pen_cycles(&g, &o);
        assert!(downs > 0, "{fw} 没有落笔指令");
        assert!(ups > 0, "{fw} 没有抬笔指令");
    }
}

#[test]
fn parses_real_machine_config() {
    let p = Path::new(r"D:\WritingRobot\奎享雕刻\default.properties");
    if !p.exists() {
        eprintln!("skip");
        return;
    }
    let cfg = MachineConfig::from_file(p).unwrap();
    assert!(cfg.feed_rate > 0.0);
    assert!(cfg.reverse_y, "default.properties 里 axe_reverse_y=true");
    assert_eq!(cfg.origin, 0);
}

#[test]
fn font_stack_falls_back() {
    // 两个字体：第一个缺字（用 dummy），第二个提供
    struct OnlyA;
    impl StrokeFont for OnlyA {
        fn name(&self) -> &str {
            "only-a"
        }
        fn em(&self) -> f32 {
            100.0
        }
        fn glyph(&self, ch: char) -> Option<wrobot_core::font::Glyph> {
            if ch == 'A' {
                Some(wrobot_core::font::Glyph::new(
                    vec![wrobot_core::font::Stroke::new(vec![(0.0, 0.0), (10.0, 10.0)])],
                    62.0,
                ))
            } else {
                None
            }
        }
    }
    let fonts: Vec<Box<dyn StrokeFont>> =
        vec![Box::new(OnlyA), Box::new(BuiltinFont::new())];
    let st = FontStack::new(fonts);
    let (_, i) = st.glyph('A', None).unwrap();
    assert_eq!(i, 0, "'A' 应由第一个字体提供");
    let (_, i2) = st.glyph('B', None).unwrap();
    assert_eq!(i2, 1, "'B' 应回退到第二个字体");
}

#[test]
fn stroke_length_and_bounds() {
    let p = wrobot_core::render::Polyline::new(vec![(0.0, 0.0), (3.0, 4.0)]);
    assert!((p.length() - 5.0).abs() < 1e-9);
    let (x0, y0, x1, y1) = p.bounds();
    assert_eq!((x0, y0, x1, y1), (0.0, 0.0, 3.0, 4.0));
    let paths = vec![p.clone(), p];
    assert!((total_length(&paths) - 10.0).abs() < 1e-9);
}
