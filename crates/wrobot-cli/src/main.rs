//! WritingRobot 命令行工具。
//!
//! 用法：
//!   wrobot-cli info <工程.json>
//!   wrobot-cli gcode <工程.json> [-o out.nc] [--firmware grbl|marlin|ebb]
//!                    [--fonts <目录>]
//!   wrobot-cli svg <工程.json> [-o out.svg] [--fonts <目录>]
//!   wrobot-cli font <字体.gfont> [字符...]
//!   wrobot-cli config <default.properties>
//!   wrobot-cli dxf  <文件.dxf> [-o out.nc] [--svg out.svg] [--firmware grbl|marlin|ebb] [--target-h 120]
//!   wrobot-cli svgi <文件.svg> [-o out.nc] [--svg out.svg] [--firmware grbl|marlin|ebb] [--target-h 120]

use std::path::{Path, PathBuf};

use wrobot_core::config::MachineConfig;
use wrobot_core::doc::{Document, Item, PageInfo};
use wrobot_core::font::{BuiltinFont, FontStack, GfontFile, StrokeFont};
use wrobot_core::gcode::GcodeOptions;
use wrobot_core::layout::LayoutOptions;
use wrobot_core::render::{cross_grid, Polyline};
use wrobot_core::{document_to_gcode, document_to_polylines, document_to_svg, Firmware};

const FONT_DIR: &str = r"D:\WritingRobot\奎享雕刻\gcodeFonts";

fn arg(flags: &[&str], name: &str) -> Option<String> {
    let mut it = std::env::args().skip(2);
    while let Some(a) = it.next() {
        if flags.contains(&a.as_str()) {
            return it.next();
        }
    }
    let _ = name;
    None
}

/// 取一个数值型选项（如 `--rounds 12`），解析不出来就用默认值。
fn arg_u64(flag: &str, default: u64) -> u64 {
    let mut it = std::env::args().skip(2);
    while let Some(a) = it.next() {
        if a == flag {
            if let Some(v) = it.next() {
                if let Ok(n) = v.trim().parse::<u64>() {
                    return n.max(1);
                }
            }
        }
    }
    default
}

#[allow(dead_code)]
fn flag(flags: &[&str]) -> bool {
    std::env::args().skip(2).any(|a| flags.contains(&a.as_str()))
}

fn positional(i: usize) -> Option<PathBuf> {
    // 第 1 个位置参数 = 子命令之后的第一个非 - 开头参数
    let mut n = 0;
    for a in std::env::args().skip(2) {
        if a.starts_with('-') {
            continue;
        }
        n += 1;
        if n == i {
            return Some(PathBuf::from(a));
        }
    }
    None
}

/// 加载字体栈：优先从字库目录加载工程引用的真实 `.gfont`，缺失时回退内置字体。
fn stack(doc: Option<&Document>, dir: &Path) -> FontStack {
    let mut fonts: Vec<Box<dyn StrokeFont>> = Vec::new();
    if let Some(d) = doc {
        for name in d.fonts_used() {
            let p = dir.join(&name);
            if !p.exists() {
                eprintln!("  [字体缺失] {}", name);
                continue;
            }
            match GfontFile::open(&p) {
                Ok(f) => {
                    println!("  [字体] {} ({} 字形)", name, f.len());
                    fonts.push(Box::new(f));
                }
                Err(e) => eprintln!("  [字体解析失败] {}：{}", name, e),
            }
        }
    }
    if fonts.is_empty() {
        println!("  [字体] 使用内置替代字体");
        fonts.push(Box::new(BuiltinFont::new()));
    }
    FontStack::new(fonts)
}

fn font_dir() -> PathBuf {
    match arg(&["--fonts", "-F"], "fonts") {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(FONT_DIR),
    }
}

fn main() {
    let cmd = std::env::args().nth(1).unwrap_or_default();
    let rc = match cmd.as_str() {
        "info" => cmd_info(),
        "gcode" => cmd_gcode(),
        "svg" => cmd_svg(),
        "font" => cmd_font(),
        "config" => cmd_config(),
        "png" => cmd_png(),
        "probe" => cmd_probe(),
        "ports" => cmd_ports(),
        "wiretest" => cmd_wiretest(),
        "bt" => cmd_bt(),
        "dxf" => cmd_dxf(),
        "svgi" => cmd_svgi(),
        _ => {
            eprintln!("未知命令：{}\n{}", cmd, USAGE);
            2
        }
    };
    std::process::exit(rc);
}

const USAGE: &str = r#"用法：
  wrobot-cli info <工程.json>
  wrobot-cli gcode <工程.json> [-o out.nc] [--firmware grbl|marlin|ebb] [--fonts <目录>]
  wrobot-cli svg <工程.json> [-o out.svg] [--fonts <目录>]
  wrobot-cli font <字体.gfont> [字符...]
  wrobot-cli config <default.properties>
  wrobot-cli png [-o out.png] [--fonts <目录>]   生成默认样板文档的笔迹预览图
  wrobot-cli dxf  <文件.dxf> [-o out.nc] [--svg out.svg] [--firmware grbl|marlin|ebb] [--target-h 120]
  wrobot-cli svgi <文件.svg> [-o out.nc] [--svg out.svg] [--firmware grbl|marlin|ebb] [--target-h 120]
                                      导入 DXF / SVG 矢量文件，解析为笔迹后转 G-code 或 SVG
  wrobot-cli ports                                列出本机串口（区分【蓝牙】与【串口线】）
  wrobot-cli wiretest [端口] [--baud 115200] [--loop]
                                                  串口线体检：从驱动→线材→对端固件逐层测
  wrobot-cli probe [端口] [--baud 115200]       探测串口（蓝牙/USB-TTL）上是否有写字机固件

probe 说明：
  不指定端口时，自动扫描 COM1..COM32 并逐个尝试握手。
  判定依据：收到固件启动横幅（Grbl / Marlin / EBB）或对 $I、M115 的响应。

wiretest 说明：
  专测**有线串口线**。不指定端口时自动挑第一个「串口线」类型的口。
  --loop：额外做回环自检，需先把该口的 TX/RX 短接（能收到自己发的字节=线材完好）。

bt 说明（蓝牙链路，支持 --name <设备名>，默认 BtWriter）：
  wrobot-cli bt              蓝牙体检：适配器 / 设备状态 / COM 口绑没绑对
  wrobot-cli bt list         主动查询周边蓝牙设备，并列出蓝牙 COM 口的绑定关系
  wrobot-cli bt bind         给已配对的写字机启用 SPP 串口服务（系统会自动建传出 COM 口）
  wrobot-cli bt watch        观察可见性时间线（默认 12 轮 × 5 秒），判断是不是反复掉线
                             --rounds N 轮数   --every N 每轮间隔秒数"#;

fn cmd_info() -> i32 {
    let Some(p) = positional(1) else {
        eprintln!("缺少工程文件路径");
        return 2;
    };
    let doc = match Document::from_file(&p) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("解析失败：{e}");
            return 1;
        }
    };
    println!("工程：{}", p.display());
    println!("  页面：{:.2} x {:.2} cm (方向 {})",
        doc.width, doc.height.unwrap_or(0.0), doc.orientation);
    println!("  落笔原点：({:.2}, {:.2})", doc.axes_pos_x, doc.axes_pos_y);
    println!("  元素数：{}（文字 {}）", doc.items.len(), doc.text_items().count());
    println!("  字符数：{}", doc.char_count());
    println!("  使用字体：");
    for f in doc.fonts_used() {
        println!("    - {}", f);
    }
    for it in doc.items.iter() {
        if !it.is_text() {
            continue;
        }
        let pi = it.page_info.as_ref();
        println!(
            "  [{}] 位置({:.1},{:.1}) 尺寸({:.1}x{:.1}) 字号{:.0} 行距{:.1} 对齐{} 段首{}",
            it.name.clone().unwrap_or_default(),
            it.x, it.y, it.width, it.height,
            pi.map(|p| p.font_size).unwrap_or(100.0),
            pi.map(|p| p.line_gap).unwrap_or(0.0),
            format!("{:?}", it.align()),
            format!("{:?}", it.first_line()),
        );
    }
    0
}

fn cmd_gcode() -> i32 {
    let Some(p) = positional(1) else {
        eprintln!("缺少工程文件路径");
        return 2;
    };
    let doc = match Document::from_file(&p) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("解析失败：{e}");
            return 1;
        }
    };
    let fw = Firmware::parse(&arg(&["--firmware", "-f"], "firmware").unwrap_or_else(|| "grbl".into()));
    let out = arg(&["-o", "--out"], "out");

    let mut gopts = GcodeOptions::default();
    let cfgp = Path::new(r"D:\WritingRobot\奎享雕刻\default.properties");
    if cfgp.exists() {
        if let Ok(mut mc) = MachineConfig::from_file(cfgp) {
            mc.firmware = fw;
            gopts = GcodeOptions::from_machine(&mc);
        }
    }
    gopts.firmware = fw;
    if let Some(h) = doc.height {
        gopts.page_h = h * 10.0; // cm → mm
    }
    gopts.page_w = doc.width * 10.0;

    let g = document_to_gcode(&doc, &stack(Some(&doc), &font_dir()), &LayoutOptions::default(), &gopts);
    match out {
        Some(o) => {
            if let Err(e) = std::fs::write(&o, &g) {
                eprintln!("写入失败：{e}");
                return 1;
            }
            println!("已生成 G-code（{} 固件）：{}", fw.as_str(), o);
        }
        None => println!("{}", g),
    }
    0
}

fn cmd_svg() -> i32 {
    let Some(p) = positional(1) else {
        eprintln!("缺少工程文件路径");
        return 2;
    };
    let doc = match Document::from_file(&p) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("解析失败：{e}");
            return 1;
        }
    };
    let svg = document_to_svg(&doc, &stack(Some(&doc), &font_dir()), &LayoutOptions::default());
    match arg(&["-o", "--out"], "out") {
        Some(o) => {
            if let Err(e) = std::fs::write(&o, &svg) {
                eprintln!("写入失败：{e}");
                return 1;
            }
            println!("已生成 SVG：{}", o);
        }
        None => println!("{}", svg),
    }
    0
}

/// 导入矢量文件（DXF / SVG）并转为 G-code（默认）或 SVG（`--svg`）。
///
/// 把参考图的 CAD / 矢量背景直接喂给写字机：
///   wrobot-cli dxf  logo.dxf  -o logo.nc   [--firmware grbl] [--target-h 120]
///   wrobot-cli svgi logo.svg  --svg logo_traced.svg
/// 不指定 `-o`/`--svg` 时把 G-code 打到 stdout。
fn vector_import(kind: &str) -> i32 {
    let Some(p) = positional(1) else {
        eprintln!("缺少矢量文件路径（{}）", kind.to_uppercase());
        return 2;
    };
    let data = match std::fs::read(&p) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("读取失败：{e}");
            return 1;
        }
    };
    let target_h: f64 = arg(&["--target-h", "-H"], "target-h")
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|v| *v > 0.0)
        .unwrap_or(120.0);
    let paths = match kind {
        "dxf" => match wrobot_core::import_dxf(&data, target_h) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("DXF 解析失败：{e}");
                return 1;
            }
        },
        _ => match wrobot_core::import_svg(&data, target_h) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("SVG 解析失败：{e}");
                return 1;
            }
        },
    };
    println!(
        "已解析 {}：{} 条笔迹（目标高度 {:.0}mm）",
        p.display(),
        paths.len(),
        target_h
    );

    // SVG 输出优先（把矢量背景导出成可再编辑的 SVG）
    if let Some(sp) = arg(&["--svg"], "svg") {
        let svg = wrobot_core::to_svg(&paths, 5.0);
        if let Err(e) = std::fs::write(&sp, svg) {
            eprintln!("写入 SVG 失败：{e}");
            return 1;
        }
        println!("已生成 SVG：{}", sp);
        return 0;
    }

    // 否则生成 G-code（默认打到 stdout，或 -o 写到文件）
    let fw = Firmware::parse(
        &arg(&["--firmware", "-f"], "firmware").unwrap_or_else(|| "grbl".into()),
    );
    let mut gopts = GcodeOptions::default();
    let cfgp = Path::new(r"D:\WritingRobot\奎享雕刻\default.properties");
    if cfgp.exists() {
        if let Ok(mut mc) = MachineConfig::from_file(cfgp) {
            mc.firmware = fw;
            gopts = GcodeOptions::from_machine(&mc);
        }
    }
    gopts.firmware = fw;
    gopts.page_w = target_h.max(1.0);
    gopts.page_h = target_h.max(1.0);
    let g = wrobot_core::generate(&paths, &gopts);
    match arg(&["-o", "--out"], "out") {
        Some(o) => {
            if let Err(e) = std::fs::write(&o, &g) {
                eprintln!("写入失败：{e}");
                return 1;
            }
            println!("已生成 G-code（{} 固件）：{}", fw.as_str(), o);
        }
        None => println!("{}", g),
    }
    0
}

fn cmd_dxf() -> i32 {
    vector_import("dxf")
}

fn cmd_svgi() -> i32 {
    vector_import("svg")
}

fn cmd_font() -> i32 {
    let Some(p) = positional(1) else {
        eprintln!("缺少字体文件路径");
        return 2;
    };
    match GfontFile::open(&p) {
        Ok(f) => {
            println!("字库：{}", p.display());
            println!("  版本头：{}", {
                let b = std::fs::read(&p).unwrap_or_default();
                if b.len() >= 8 {
                    format!(
                        "{} meta_len={}",
                        u32::from_be_bytes([b[0], b[1], b[2], b[3]]),
                        u32::from_be_bytes([b[4], b[5], b[6], b[7]])
                    )
                } else {
                    "过短".into()
                }
            });
            println!("  字形数：{}", f.len());
            println!("  em：{:.0} 字库单位", f.em());
            // 抽样统计笔画数
            let mut total_strokes = 0usize;
            let mut total_pts = 0usize;
            for g in f.glyphs().values() {
                total_strokes += g.strokes.len();
                total_pts += g.point_count();
            }
            println!(
                "  平均笔画：{:.2}  平均点数：{:.2}",
                total_strokes as f64 / f.len() as f64,
                total_pts as f64 / f.len() as f64
            );
            // 指定字符打印 ASCII 渲染
            let chars: Vec<char> = std::env::args()
                .skip(2)
                .filter(|a| !a.starts_with('-'))
                .skip(1)
                .flat_map(|s| s.chars().collect::<Vec<char>>())
                .collect();
            for ch in &chars {
                match f.glyph(*ch) {
                    Some(g) => {
                        println!("\n  —— {} (U+{:04X}) 笔画 {} ——" , ch, *ch as u32, g.strokes.len());
                        for s in &g.strokes {
                            let pts: Vec<String> = s
                                .points
                                .iter()
                                .map(|(x, y)| format!("({:.1},{:.1})", x, y))
                                .collect();
                            println!("    {}", pts.join(" "));
                        }
                    }
                    None => println!("\n  —— {} 无字形 ——", ch),
                }
            }
            0
        }
        Err(e) => {
            eprintln!("解析失败：{e}");
            1
        }
    }
}

fn cmd_config() -> i32 {
    let Some(p) = positional(1) else {
        eprintln!("缺少配置文件路径");
        return 2;
    };
    match MachineConfig::from_file(&p) {
        Ok(c) => {
            println!("固件：{}", c.firmware.as_str());
            println!("抬落笔方式：{:?}", c.pen_mode);
            println!("feed_rate：{}", c.feed_rate);
            println!("抬笔位置：{} 落笔位置：{}", c.pen_up_pos, c.pen_down_pos);
            println!("坐标系：swap_xy={} reverse_x={} reverse_y={} origin={}",
                c.swap_xy, c.reverse_x, c.reverse_y, c.origin);
            println!("其它键：{} 个", c.extra.len());
            0
        }
        Err(e) => {
            eprintln!("解析失败：{e}");
            1
        }
    }
}

/// 取字库目录里第一个 `.gfont` 的字体名（用于样板文档的 font_path）。
fn first_font_name(dir: &Path) -> Option<String> {
    let rd = std::fs::read_dir(dir).ok()?;
    for e in rd.flatten() {
        let p = e.path();
        let is_gfont = p
            .extension()
            .map(|x| x.eq_ignore_ascii_case("gfont"))
            .unwrap_or(false);
        if is_gfont {
            if let Ok(g) = GfontFile::open(&p) {
                return Some(g.name().to_string());
            }
        }
    }
    None
}

/// 依据工程引用的字体名构建字体栈（与 GUI 的 build_stack 一致），缺失时回退内置字体。
fn stack_png(dir: &Path, doc: &Document) -> FontStack {
    let mut names: Vec<String> = Vec::new();
    for it in &doc.items {
        if let Some(pi) = &it.page_info {
            for f in &pi.font_path {
                if !names.iter().any(|n| n == f) {
                    names.push(f.clone());
                }
            }
        }
    }
    let mut fonts: Vec<Box<dyn StrokeFont>> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            let is_gfont = p
                .extension()
                .map(|x| x.eq_ignore_ascii_case("gfont"))
                .unwrap_or(false);
            if is_gfont {
                if let Ok(g) = GfontFile::open(&p) {
                    let nm = g.name();
                    if names.iter().any(|n| n == &nm) {
                        fonts.push(Box::new(g));
                    }
                }
            }
        }
    }
    fonts.push(Box::new(BuiltinFont::new()));
    FontStack::new(fonts)
}

/// 把笔迹折线栅格化为 PNG（白底黑线）。
fn rasterize(
    paths: &[Polyline],
    page_w_mm: f64,
    page_h_mm: f64,
    out: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let ppm = 6.0; // 像素 / 毫米
    let w = (page_w_mm * ppm).ceil() as u32;
    let h = (page_h_mm * ppm).ceil() as u32;
    let mut buf = vec![255u8; (w * h * 4) as usize];
    let set = |buf: &mut [u8], x: i32, y: i32| {
        if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
            return;
        }
        let i = ((y as u32 * w + x as u32) * 4) as usize;
        buf[i] = 20;
        buf[i + 1] = 20;
        buf[i + 2] = 20;
        buf[i + 3] = 255;
    };
    for pl in paths {
        for seg in pl.pts.windows(2) {
            let (x0, y0) = (seg[0].0 * ppm, seg[0].1 * ppm);
            let (x1, y1) = (seg[1].0 * ppm, seg[1].1 * ppm);
            let dx = x1 - x0;
            let dy = y1 - y0;
            let steps = (dx.abs().max(dy.abs())).ceil() as i32 + 1;
            for s in 0..=steps {
                let t = s as f64 / steps as f64;
                set(
                    &mut buf,
                    (x0 + dx * t).round() as i32,
                    (y0 + dy * t).round() as i32,
                );
            }
        }
    }
    image::save_buffer(out, &buf, w, h, image::ColorType::Rgba8)?;
    Ok(())
}

/// 生成一份「样板文档」的预览 PNG：标题 + 段落 + 表格 + 米字格示范字 + 笔记。
fn cmd_png() -> i32 {
    let dir = font_dir();
    let stem = match first_font_name(&dir) {
        Some(n) => {
            println!("已选用字体：{}", n);
            n
        }
        None => {
            eprintln!("未在 {} 找到 .gfont 字库，将使用内置替代字体", dir.display());
            "Builtin".into()
        }
    };

    let page_w = 175.0;
    let page_h = 255.0;
    let mut items: Vec<Item> = Vec::new();

    // 标题 + 段落（文字模块）
    items.push(Item {
        r#type: "text".into(),
        x: 12.0,
        y: 12.0,
        width: 150.0,
        height: 70.0,
        text: Some("兰亭集序（预览）\n永和九年岁在癸丑，暮春之初，会于会稽山阴之兰亭。".into()),
        content_format: Some("{}".into()),
        page_info: Some(PageInfo {
            line_gap: 4.0,
            space: 0.0,
            content_align: Some("left".into()),
            page_width: 150.0,
            page_height: 70.0,
            font_size: 100.0,
            font_path: vec![stem.clone()],
            ..Default::default()
        }),
        ..Default::default()
    });

    // 表格边框（线条形状）
    let (tx, ty, tcw, tch, tcols, trows) = (15.0, 95.0, 18.0, 12.0, 3usize, 3usize);
    let mut lines: Vec<Vec<(f64, f64)>> = Vec::new();
    for i in 0..=tcols {
        let gx = tx + tcw * i as f64;
        lines.push(vec![(gx, ty), (gx, ty + tch * trows as f64)]);
    }
    for j in 0..=trows {
        let gy = ty + tch * j as f64;
        lines.push(vec![(tx, gy), (tx + tcw * tcols as f64, gy)]);
    }
    items.push(Item {
        r#type: "line".into(),
        shapes: Some(lines),
        ..Default::default()
    });
    // 表格文字
    let cells = ["张三\t88\t92", "李四\t76\t85", "王五\t95\t89"];
    for (r, row) in cells.iter().enumerate() {
        for (c, cell) in row.split('\t').enumerate() {
            items.push(Item {
                r#type: "text".into(),
                x: tx + tcw * c as f64 + 1.0,
                y: ty + tch * r as f64 + 1.0,
                width: tcw - 2.0,
                height: tch,
                text: Some(cell.to_string()),
                content_format: Some("{}".into()),
                page_info: Some(PageInfo {
                    font_path: vec![stem.clone()],
                    font_size: 100.0,
                    page_width: tcw - 2.0,
                    page_height: tch,
                    ..Default::default()
                }),
                ..Default::default()
            });
        }
    }

    // 米字格 + 示范字（格子模块）
    let (gx0, gy0, gc, gn) = (15.0, 140.0, 40.0, 3usize);
    let mut glines: Vec<Vec<(f64, f64)>> = Vec::new();
    for r in 0..gn {
        for c in 0..gn {
            let (x, y) = (gx0 + gc * c as f64, gy0 + gc * r as f64);
            glines.extend(cross_grid(x, y, gc, gc).into_iter().map(|p| p.pts));
        }
    }
    items.push(Item {
        r#type: "line".into(),
        shapes: Some(glines),
        ..Default::default()
    });
    let sample = "永字八法天地玄黄宇宙洪荒";
    for (i, ch0) in sample.chars().enumerate() {
        let r = i / gn;
        let c = i % gn;
        if r >= gn {
            break;
        }
        items.push(Item {
            r#type: "text".into(),
            x: gx0 + gc * c as f64 + 4.0,
            y: gy0 + gc * r as f64 + 4.0,
            width: gc - 8.0,
            height: gc,
            text: Some(ch0.to_string()),
            content_format: Some("{}".into()),
            page_info: Some(PageInfo {
                font_path: vec![stem.clone()],
                font_size: 100.0,
                page_width: gc - 8.0,
                page_height: gc,
                ..Default::default()
            }),
            ..Default::default()
        });
    }

    // 笔记模块
    items.push(Item {
        r#type: "text".into(),
        x: 15.0,
        y: 230.0,
        width: 150.0,
        height: 20.0,
        text: Some("笔记：临摹示范 · 供参考".into()),
        content_format: Some("{}".into()),
        page_info: Some(PageInfo {
            font_path: vec![stem.clone()],
            font_size: 100.0,
            page_width: 150.0,
            page_height: 20.0,
            ..Default::default()
        }),
        ..Default::default()
    });

    let doc = Document {
        orientation: 0,
        width: page_w / 10.0,
        height: Some(page_h / 10.0),
        axes_pos_x: 0.0,
        axes_pos_y: 0.0,
        items,
        ..Default::default()
    };

    let opts = LayoutOptions {
        px_per_mm: 1.0,
        char_mm_at_100: 8.0,
        tab_indent_chars: 2.0,
        random: Default::default(),
    };
    let paths = document_to_polylines(&doc, &stack_png(&dir, &doc), &opts);
    let out = arg(&["-o", "--out"], "out")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"D:\WritingRobot\wrobot\out\preview.png"));
    match rasterize(&paths, page_w, page_h, &out) {
        Ok(()) => {
            println!("已生成预览 PNG：{}（{} 条笔迹）", out.display(), paths.len());
            0
        }
        Err(e) => {
            eprintln!("生成失败：{e}");
            1
        }
    }
}

/// 探测串口：打开端口 → 读固件启动横幅 → 握手 `$I` / `M115` → 判定固件是否在线。
///
/// 这是验证「蓝牙是否已连上写字机」最快的手段：
/// 端口能被打开只说明蓝牙 SPP 链路存在，收到固件响应才说明对端真的是写字机。
fn cmd_probe() -> i32 {
    use wrobot_core::SerialPort;

    // 自己解析：`positional()` 会把 `--baud` 的值误当成端口名
    let argv: Vec<String> = std::env::args().skip(2).collect();
    let mut port: Option<String> = None;
    let mut baud: u32 = 115200;
    let mut i = 0usize;
    while i < argv.len() {
        let a = argv[i].as_str();
        if a == "--baud" || a == "-b" {
            if let Some(v) = argv.get(i + 1) {
                if let Ok(b) = v.parse() {
                    baud = b;
                }
            }
            i += 2;
            continue;
        }
        if a.starts_with('-') {
            i += 1;
            continue;
        }
        if port.is_none() {
            port = Some(a.to_string());
        }
        i += 1;
    }

    let ports: Vec<String> = match port {
        Some(p) => vec![p],
        None => {
            println!("扫描本机串口…");
            let v = wrobot_core::list_serial_ports();
            if v.is_empty() {
                println!("✗ 未发现任何可用串口。");
                println!("  蓝牙设备需先配对，并在「蓝牙设置 → 更多蓝牙选项 → COM 端口」添加【传出】端口。");
                return 1;
            }
            println!("发现端口：{}", v.join(", "));
            v
        }
    };

    let mut hits = 0usize;
    for port in &ports {
        println!("\n— {} @ {} —", port, baud);
        let mut sp = match SerialPort::open(port, baud) {
            Ok(p) => p,
            Err(e) => {
                println!("  ✗ 打开失败：{e}");
                continue;
            }
        };
        if !sp.warnings().is_empty() {
            println!("  ⚠ {}", sp.warnings());
        }
        if sp.actual_baud() != baud {
            println!("  ⚠ 驱动实际波特率 {}（请求 {}）", sp.actual_baud(), baud);
        }
        println!("  ✓ 端口已打开（蓝牙/串口链路存在）");

        // 1) 启动横幅：连接或复位后固件会主动打印一串信息
        let mut banner = Vec::new();
        for _ in 0..5 {
            if let Some(l) = sp.read_line(500) {
                banner.push(l);
            }
        }
        print_lines("启动横幅", &banner);

        // 2) 主动握手：GRBL 的 $I、Marlin 的 M115
        let mut replies = Vec::new();
        for cmd in ["\r\n", "$I\n", "M115\n"] {
            if sp.write_all(cmd.as_bytes()).is_err() {
                break;
            }
            for _ in 0..3 {
                if let Some(l) = sp.read_line(500) {
                    replies.push(l);
                }
            }
        }
        print_lines("握手响应", &replies);

        let all = format!("{} {}", banner.join(" "), replies.join(" "));
        // 必须先关掉句柄，否则紧接着的逐步诊断会因为「本程序自己占用」误报错误码 5
        drop(sp);
        let kind = if all.contains("Grbl") || all.contains("grbl") {
            "GRBL"
        } else if all.contains("Marlin") || all.contains("marlin") {
            "Marlin"
        } else if all.contains("EBB") {
            "EBB"
        } else if all.to_lowercase().contains("ok") {
            "未知固件（收到 ok 响应）"
        } else {
            ""
        };

        if kind.is_empty() {
            println!("  ✗ 未收到固件响应：端口可打开，但对端没有写字机固件（或波特率不匹配）");
            // 给出逐步诊断，把失败定位到具体环节（错误码 31 尤其需要）
            println!("{}", wrobot_core::diag_port_steps(port, baud));
        } else {
            println!("  ✓ 识别为 {kind} —— 蓝牙链路正常，可以下发 G-code");
            hits += 1;
        }
    }

    if hits == 0 {
        println!("\n结论：未发现在线的写字机固件。");
    } else {
        println!("\n结论：{hits} 个端口上有固件响应。");
    }
    0
}

fn print_lines(title: &str, lines: &[String]) {
    if lines.is_empty() {
        println!("  {title}：（无）");
        return;
    }
    println!("  {title}：");
    for l in lines {
        println!("    | {l}");
    }
}

/// `wrobot-cli ports`：列出本机串口，并**区分蓝牙与有线串口线**。
///
/// 蓝牙和串口线的失败原因完全不是一回事，混在一张清单里排查会绕远路：
/// 蓝牙口存在只代表「配对过」，不代表链路已建立；串口线存在则代表驱动已加载。
fn cmd_ports() -> i32 {
    use wrobot_core::PortKind;

    println!("扫描本机串口…");
    let all = wrobot_core::enumerate_ports();
    if all.is_empty() {
        println!("✗ 本机没有任何 COM 口（注册表 SERIALCOMM 为空、设备管理器也没有串口设备）。");
        println!("  → 想用【串口线】：插上 USB-TTL（CH340/CP210x/FTDI），等设备管理器");
        println!("    「端口(COM 和 LPT)」下出现「USB-SERIAL CH340 (COMx)」再回来扫；");
        println!("  → 想用【蓝牙】：先在 Windows 蓝牙里配对并【连接】写字机，");
        println!("    再到「蓝牙设置 → 更多蓝牙选项 → COM 端口」添加【传出】方向端口。");
        return 1;
    }

    println!("\n{:<7} {:<7} {:<7} {}", "端口", "类型", "状态", "设备");
    println!("{}", "-".repeat(72));
    for e in &all {
        println!(
            "{:<7} {:<7} {:<7} {}{}",
            e.port,
            e.kind.label(),
            if e.online { "在线" } else { "打不开" },
            e.desc,
            if e.device.is_empty() {
                String::new()
            } else {
                format!("  [{}]", e.device)
            }
        );
    }

    let wired: Vec<&wrobot_core::PortEntry> = all.iter().filter(|e| e.kind.is_wired()).collect();
    let bt: Vec<&wrobot_core::PortEntry> = all
        .iter()
        .filter(|e| e.kind == PortKind::Bluetooth)
        .collect();

    println!("\n── 有线串口线（{} 个）──", wired.len());
    if wired.is_empty() {
        println!("  无。想测线就先插上 USB-TTL 适配器。");
    } else {
        for e in &wired {
            println!(
                "  {}{} → 用 `wrobot-cli wiretest {}` 体检",
                e.port,
                if e.online { "（可打开）" } else { "（打不开）" },
                e.port
            );
        }
    }

    println!("\n── 蓝牙串口（{} 个）──", bt.len());
    if bt.is_empty() {
        println!("  无。蓝牙需先配对【并连接】写字机，且要有【传出】COM 口。");
    } else {
        for e in &bt {
            println!(
                "  {}{}  {}",
                e.port,
                if e.online {
                    "（可打开 = SPP 链路已建立）"
                } else {
                    "（打不开 = 只配对未连接，或机器没开机）"
                },
                e.desc
            );
        }
        println!("  → {}", PortKind::Bluetooth.tips());
    }
    0
}

/// `wrobot-cli wiretest [端口]`：串口线体检。
fn cmd_wiretest() -> i32 {
    let argv: Vec<String> = std::env::args().skip(2).collect();
    let mut port: Option<String> = None;
    let mut baud: u32 = 115200;
    let mut loopback = false;
    let mut i = 0usize;
    while i < argv.len() {
        match argv[i].as_str() {
            "--baud" | "-b" => {
                if let Some(v) = argv.get(i + 1) {
                    if let Ok(b) = v.parse() {
                        baud = b;
                    }
                }
                i += 2;
            }
            "--loop" | "--loopback" => {
                loopback = true;
                i += 1;
            }
            a if a.starts_with('-') => i += 1,
            a => {
                if port.is_none() {
                    port = Some(a.to_string());
                }
                i += 1;
            }
        }
    }

    let port = match port {
        Some(p) => p,
        None => {
            let wired = wrobot_core::wired_ports();
            if wired.is_empty() {
                let bt = wrobot_core::bluetooth_ports();
                println!("✗ 本机没有检测到任何【有线串口线】。");
                if bt.is_empty() {
                    println!("  一个 COM 口都没有：先插上 USB-TTL，或配对蓝牙写字机。");
                } else {
                    println!("  只有蓝牙口（{}），它们是无线链路，不适用串口线体检：", bt.iter().map(|e| e.port.clone()).collect::<Vec<_>>().join(", "));
                    println!("  → {}", wrobot_core::PortKind::Bluetooth.tips());
                }
                println!("  插好线后用 `wrobot-cli ports` 确认类型，再跑 wiretest。");
                return 1;
            }
            println!("未指定端口，自动选用第一根串口线：{}", wired[0].display());
            wired[0].port.clone()
        }
    };

    print!("{}", wrobot_core::wire_test(&port, baud, loopback));
    0
}

/// `bt`：蓝牙链路诊断。`bt` = 体检，`bt watch` = 观察可见性，`bt bind` = 建串口。
fn cmd_bt() -> i32 {
    let sub = std::env::args().nth(2).unwrap_or_default();
    let name = arg(&["--name", "-n"], "").unwrap_or_else(|| "BtWriter".to_string());
    match sub.as_str() {
        // 观察它到底是不是「一会儿看得到一会儿看不到」，以及掉线的节奏
        "watch" => {
            let rounds: usize = arg_u64("--rounds", 12) as usize;
            let interval = arg_u64("--every", 5);
            println!(
                "观察「{}」的可见性（{} 轮 × {} 秒）。这会做主动蓝牙查询，请稍候…\n",
                name, rounds, interval
            );
            print!("{}", wrobot_core::bt_watch(&name, rounds, interval));
            0
        }
        // 给已配对的写字机装 SPP 服务 → 系统自动建出绑定它的传出 COM 口
        "bind" => match wrobot_core::bt_set_spp(&name, true) {
            Ok(msg) => {
                println!("✓ {}", msg);
                println!("  稍等 2 秒后用 `wrobot-cli ports` 看新出现的 COM 口。");
                0
            }
            Err(e) => {
                println!("✗ {}", e);
                1
            }
        },
        // 列出所有蓝牙设备与 COM 口绑定情况
        "list" => {
            println!("主动查询周边蓝牙设备（约 5 秒）…\n");
            for d in wrobot_core::bt_devices(true, 4) {
                println!("  {}", d.status_line());
            }
            println!("\n蓝牙 COM 口绑定：");
            let bs = wrobot_core::bt_com_bindings();
            if bs.is_empty() {
                println!("  （无）");
            }
            for b in bs {
                println!(
                    "  {}  {}",
                    b.port,
                    if b.bound() {
                        format!("绑定设备 {}", b.addr)
                    } else {
                        "⚠ 空口：没绑任何设备，打开能写 0 字节（别选它）".to_string()
                    }
                );
            }
            0
        }
        // 默认：完整体检报告
        _ => {
            print!("{}", wrobot_core::bt_report(&name));
            0
        }
    }
}
