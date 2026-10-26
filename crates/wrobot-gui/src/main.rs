//! WritingRobot 图形界面入口。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod device;
mod fontmgr;
mod io;
mod preview;
mod sketch;
mod state;
mod theme;
mod ui;

use std::path::PathBuf;

use eframe::egui;

use state::AppState;

/// 探测字库目录（安装目录下的 gcodeFonts）。
fn find_font_dir() -> Option<PathBuf> {
    let candidates = [
        r"D:\WritingRobot\奎享雕刻\gcodeFonts",
        r"D:\WritingRobot\wrobot\gcodeFonts",
        "gcodeFonts",
        "../奎享雕刻/gcodeFonts",
    ];
    for c in candidates {
        let p = PathBuf::from(c);
        if p.is_dir() {
            return Some(p);
        }
    }
    None
}

struct MyApp {
    state: AppState,
}

impl MyApp {
    fn new() -> Self {
        let dir = find_font_dir();
        let dir_str = dir
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "<未找到字库目录>".into());
        let fonts = match &dir {
            Some(d) => fontmgr::FontManager::load(d),
            None => fontmgr::FontManager { fonts: vec![] },
        };
        let mut state = AppState::new(fonts, dir_str);
        if state.fonts.is_empty() {
            state.status = "警告：未加载到 .gfont 字库，中文将显示为占位框".into();
        }
        state.rebuild_preview();
        MyApp { state }
    }
}

impl eframe::App for MyApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        ui::show(&mut self.state, ui, frame);
    }
}

/// 向 egui 注入系统中文字体，解决界面中文显示为方块（乱码）的问题。
/// egui 默认字体不含 CJK 字形，必须在首个帧之前通过 Context 注册。
fn setup_fonts(ctx: &egui::Context) {
    let candidates = [
        r"C:\Windows\Fonts\simhei.ttf", // 黑体（优先，纯 TTF，解析最稳）
        r"C:\Windows\Fonts\simkai.ttf", // 楷体
        r"C:\Windows\Fonts\simfang.ttf", // 仿宋
        r"C:\Windows\Fonts\STXIHEI.TTF", // 华文细黑
        r"C:\Windows\Fonts\msyh.ttc",   // 微软雅黑（TTC，解析失败则跳过）
        r"C:\Windows\Fonts\simsun.ttc", // 宋体
    ];
    let found = candidates
        .iter()
        .find(|p| std::path::Path::new(p).is_file() && std::fs::read(p).is_ok());
    let Some(path) = found else {
        eprintln!("[字体] 未找到系统中文字体，UI 中文可能显示为方块");
        return;
    };
    let bytes = std::fs::read(path).expect("读取字体文件");
    eprintln!("[字体] 已加载中文字体：{path}");
    let mut fonts = egui::FontDefinitions::default();
    // egui 0.36：font_data 值为 Arc<FontData>，families 值为字体名(String)列表。
    fonts.font_data.insert(
        "cn".to_owned(),
        std::sync::Arc::new(egui::FontData::from_owned(bytes)),
    );
    // 把中文放在 Proportional 家族首位：优先用中文渲染，缺字形再回退到后续字体。
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "cn".to_owned());
    // 日志栏 / 代码窗口用的是等宽样式（Monospace 家族）。只给 Proportional 注入的话，
    // 这些区域里一个 CJK 字形都没有，中文会显示成方块——必须也挂上，
    // 但**追加在末尾**（不是插到首位）：ASCII 仍走等宽字体，只有中文才回退过来。
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .push("cn".to_owned());
    ctx.set_fonts(fonts);
}

/// 载入应用图标（编译期把 `assets/icon.png` 嵌进可执行文件，运行时解码为 RGBA）。
///
/// 图标由 `tools/make_icon.py` 生成（纯标准库光栅化：蓝→青渐变圆角底板 +
/// 纸张 + 手写波浪线 + 钢笔）。同时影响标题栏与任务栏。
fn load_icon() -> Option<egui::IconData> {
    let png = include_bytes!("../assets/icon.png");
    let img = image::load_from_memory(png).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    Some(egui::IconData {
        width: w,
        height: h,
        rgba: img.into_raw(),
    })
}

fn main() -> eframe::Result {
    // 命令行：--port COM3 [--baud 115200] 直接连接；--auto 自动搜索并连接
    let args: Vec<String> = std::env::args().collect();
    let mut port: Option<String> = None;
    let mut baud: u32 = 115200;
    let mut auto = false;
    let mut i = 1usize;
    while i < args.len() {
        match args[i].as_str() {
            "--port" | "-p" => {
                if let Some(v) = args.get(i + 1) {
                    port = Some(v.clone());
                }
                i += 2;
            }
            "--baud" | "-b" => {
                if let Some(v) = args.get(i + 1) {
                    if let Ok(b) = v.parse() {
                        baud = b;
                    }
                }
                i += 2;
            }
            "--auto" | "-a" => {
                auto = true;
                i += 1;
            }
            _ => i += 1,
        }
    }

    // 窗口：给一个足够容纳「左侧导航 + 画布 + 右侧参数栏」的初始尺寸并最大化启动。
    //
    // 之前用 `..Default::default()`，ViewportBuilder 的 inner_size 是 None，
    // winit 会按系统默认值创建（在高分屏/多显示器上常常只有很小一块），
    // 三栏布局被挤成一团、内容显示不全。
    // 同时关掉窗口状态持久化：程序迭代中布局会变，旧的“小窗口”尺寸会被记住并一直复用。
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1600.0, 1000.0])
        .with_min_inner_size([1180.0, 720.0])
        .with_maximized(true)
        .with_app_id("wrobot-gui")
        .with_title("WritingRobot · 写字机器人上位机");
    // 应用图标（标题栏 + 任务栏）：嵌入的 PNG 解码失败则退回系统默认图标
    if let Some(icon) = load_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        persist_window: false,
        centered: true,
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "WritingRobot · 写字机器人上位机",
        options,
        Box::new(move |cc| {
            setup_fonts(&cc.egui_ctx);
            let mut app = MyApp::new();
            // 后台扫一次端口（只 CreateFile 试探，不重试），这样设备页一打开就能看到
            // 每个口的在线状态。扫描必须放后台：离线蓝牙口的 CreateFile 可能卡十几秒。
            app.state.start_port_scan();
            if let Some(p) = &port {
                app.state.serial_port = p.clone();
                app.state.serial_baud = baud;
                app.state.connect_device();
            } else if auto {
                app.state.auto_connect();
            } else if app.state.auto_connect {
                // 参考项目 auto_connect=true：启动就自己找机器，不用每次手动点。
                // 等后台端口扫描结束再连，避免扫描与连接同时试开同一个口。
                app.state.auto_connect_after_scan = true;
            }
            Ok(Box::new(app))
        }),
    )
}

#[cfg(test)]
mod tests {
    use crate::fontmgr::FontManager;
    use crate::state::AppState;
    use wrobot_core::Firmware;

    const FONT_DIR: &str = r"D:\WritingRobot\奎享雕刻\gcodeFonts";

    #[test]
    fn pipeline_with_real_fonts() {
        let dir = std::path::Path::new(FONT_DIR);
        if !dir.is_dir() {
            eprintln!("跳过：字库目录不存在");
            return;
        }
        let fm = FontManager::load(dir);
        assert!(!fm.is_empty(), "应加载到真实 .gfont 字库");
        let mut app = AppState::new(fm, FONT_DIR.into());

        // ① 文字
        app.rebuild_preview();
        assert!(!app.preview.is_empty(), "文字预览应有笔画");
        let g = app.generate_gcode();
        assert!(g.contains("G1"), "G-code 应含落笔移动 G1");

        // ② 表格
        app.tab = 1;
        app.table_text = "甲\t乙\t丙\n1\t2\t3".into();
        app.table_rows = 2;
        app.table_cols = 3;
        app.rebuild_preview();
        assert!(!app.preview.is_empty(), "表格预览应有笔画");

        // ③ 格子（含示范字）
        app.tab = 2;
        app.grid_sample = "永和九年岁在癸丑".into();
        app.rebuild_preview();
        assert!(!app.preview.is_empty(), "格子预览应有笔画");

        // ④ 绘图（无图片时应为空，但不应崩溃）
        app.tab = 3;
        app.rebuild_preview();

        // ⑤ 笔记
        app.tab = 4;
        app.notes.push(crate::state::NoteBlock {
            text: "测试".into(),
            font: app.text_font.clone(),
            size_mm: 8.0,
            x: 10.0,
            y: 10.0,
            align: 0,
        });
        app.rebuild_preview();
        assert!(!app.preview.is_empty(), "笔记预览应有笔画");

        // ⑥/⑦ 设备连接（模拟）+ 导出字符串
        let _ = app.generate_svg();
    }

    #[test]
    fn gcode_three_firmwares() {
        let dir = std::path::Path::new(FONT_DIR);
        if !dir.is_dir() {
            return;
        }
        let fm = FontManager::load(dir);
        let mut app = AppState::new(fm, FONT_DIR.into());
        app.texts[0].text = "中".into();
        for fw in 0..=2 {
            app.firmware_sel = fw;
            let g = app.generate_gcode();
            assert!(g.contains("G21"), "固件 {} 输出应含 G21", fw);
        }
    }

    fn make_app() -> Option<AppState> {
        let dir = std::path::Path::new(FONT_DIR);
        if !dir.is_dir() {
            return None;
        }
        let fm = FontManager::load(dir);
        if fm.is_empty() {
            return None;
        }
        Some(AppState::new(fm, FONT_DIR.into()))
    }

    /// 造一张指定尺寸的前景图片（基础笔迹为 0,0→w,h 的对角线）。
    fn push_test_image(app: &mut AppState, w: f64, h: f64) {
        app.images.push(crate::state::ImageObject {
            path: "tile-test.png".into(),
            bytes: Vec::new(),
            x: 0.0,
            y: 0.0,
            scale: 1.0,
            rotation: 0.0,
            flip_h: false,
            flip_v: false,
            draw: crate::state::ImgDraw::Wire,
            step_px: 2,
            threshold: 0.5,
            edge_threshold: 0.5,
            invert: false,
            crop: (0.0, 0.0, 1.0, 1.0),
            scan_angle: 0,
            merge_gap_px: 8,
            order_mode: 0,
            block_px: 40,
            sketch_pressure: 2,
            sketch_jitter: 0.35,
            sketch_pen: 0.5,
            hatch_levels: 6,
            hatch_angle: 35.0,
            region_block_mm: 2.5,
            stroke_len_mm: 3.2,
            stroke_follow: 2,
            multi_pen: false,
            background: false,
            out: true,
            base: vec![wrobot_core::Polyline::new(vec![(0.0, 0.0), (w, h)])],
            base_pens: vec![],
            natural_w: w,
            natural_h: h,
            gen_key: 0,
        });
        app.images_on = true;
    }

    /// 四角 L 标记：每个角两条独立短臂，每条都从角点起笔（原点角绝不缺笔）；
    /// 原点那一角臂长加倍并多一道 45° 斜线，好让人一眼认出 (0,0)。
    #[test]
    fn corner_marks_are_1cm_l_at_page_corners() {
        // 原点在左下（页面坐标 (0,280)）
        let marks = crate::state::corner_mark_polylines(200.0, 280.0, 10.0, (false, true));
        assert_eq!(marks.len(), 9, "4 角 × 2 臂 + 原点斜线");
        let corners = [(0.0, 0.0), (200.0, 0.0), (200.0, 280.0), (0.0, 280.0)];
        for c in corners {
            let n = marks.iter().filter(|m| m.pts[0] == c).count();
            let want = if c == (0.0, 280.0) { 3 } else { 2 };
            assert_eq!(n, want, "角点 {c:?} 应以该点为起点");
        }
        for m in &marks {
            let (a, b) = (m.pts[0], m.pts[1]);
            let len = ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt();
            let is_origin_arm = a == (0.0, 280.0) && (len - 20.0).abs() < 1e-9;
            assert!(
                (len - 10.0).abs() < 1e-9 || is_origin_arm,
                "普通臂 10mm、原点角臂 20mm，实际 {len}（{a:?}→{b:?}）"
            );
        }
        // 臂一律朝纸内：左上角的横臂向 +X，竖臂向 +Y（页面 y 向下）
        assert_eq!(marks[0].pts, vec![(0.0, 0.0), (10.0, 0.0)]);
        assert_eq!(marks[1].pts, vec![(0.0, 0.0), (0.0, 10.0)]);
    }

    /// 定位框默认 200×280，并夹到页面 / 机器幅面内。
    #[test]
    fn corner_frame_defaults_and_clamps() {
        let Some(mut app) = make_app() else { return };
        assert_eq!(app.corner_frame_size(), (200.0, 280.0), "默认定位框 200×280");
        app.corner_w_mm = 400.0;
        app.corner_h_mm = 500.0;
        assert_eq!(
            app.corner_frame_size(),
            (200.0, 297.0),
            "超出的设定值必须夹到 A4 / 机器幅面"
        );
        app.machine_h_mm = 240.0;
        assert_eq!(app.corner_frame_size(), (200.0, 240.0), "不能超过机器幅面");
        app.page_w_mm = 80.0;
        assert_eq!(app.corner_frame_size(), (80.0, 240.0), "不能超过纸张");
    }

    /// 四角 G-code（原点在纸面左下角）：原点角的两条长臂都从机器 (0,0) 落笔，
    /// 远端臂落在 Y=h，结尾回 (0,0)。
    #[test]
    fn corner_marks_gcode_starts_at_origin() {
        let Some(app) = make_app() else { return };
        let (pw, ph) = (210.0, 297.0);
        let (w, h) = (200.0, 280.0);
        // 与 draw_corner_marks 一致：框贴在纸的左下角
        let paths: Vec<wrobot_core::Polyline> =
            crate::state::corner_mark_polylines(w, h, 10.0, (false, true))
                .into_iter()
                .map(|p| {
                    wrobot_core::Polyline::new(
                        p.pts.iter().map(|&(x, y)| (x, y + (ph - h))).collect(),
                    )
                })
                .collect();
        let mut o = app.gcode_options();
        o.page_w = pw;
        o.page_h = ph;
        o.use_current_origin = true;
        o.custom_start.clear();
        o.custom_end.clear();
        let g = wrobot_core::generate(&paths, &o);
        // 原点角的横臂：机器 (0,0) → (20,0)
        let i = g.find("G1 X20 Y0 ").expect("原点角横臂应有 20mm");
        assert!(g[..i].contains("G0 X0 Y0"), "横臂必须先到原点：\n{}", &g[..i]);
        let j = g.find("G1 X0 Y20 ").expect("原点角竖臂应有 20mm");
        assert!(g[..j].rfind("G0 X0 Y0").is_some(), "竖臂也必须从原点起笔");
        // 远端（页面左上）那一角落在机器 Y = 框高
        assert!(
            g.contains("G1 X10 Y280 ") || g.contains("Y280"),
            "页面左上角应落在机器 Y=280：\n{g}"
        );
        assert!(
            g.trim_end().ends_with("G0 X0 Y0 ; 回到页面零点（当前原点）"),
            "四角画完必须回原点：\n{}",
            g.lines().rev().take(3).collect::<Vec<_>>().join("\n")
        );
    }

    /// 原点位置与真实坐标变换必须一致：`origin_page_point()` 说在哪，`transform`
    /// 就得把那个点映射到机器 (0,0)。两个设置（原点角 / 反转）任一改动都要成立，
    /// 否则就是「选了左下却画到左上」这类 bug 的温床。
    #[test]
    fn origin_page_point_matches_transform() {
        let Some(mut app) = make_app() else { return };
        for oc in 0..4usize {
            for rx in [false, true] {
                for ry in [false, true] {
                    app.origin_corner = oc;
                    app.reverse_x = rx;
                    app.reverse_y = ry;
                    app.swap_xy = false;
                    let o = app.gcode_options();
                    let (px, py) = app.origin_page_point();
                    let (mx, my) = o.transform(px, py);
                    assert!(
                        mx.abs() < 1e-6 && my.abs() < 1e-6,
                        "origin_corner={oc} rx={rx} ry={ry}：说原点在页面 ({px},{py})，\
                         实际映射到机器 ({mx},{my})"
                    );
                }
            }
        }
    }

    /// 定位框必须贴住原点所在的角：默认（0,0 在左下）框就贴在纸的左下角，
    /// 且镜像基准用纸张尺寸——这样四角标记和正文才对得上。
    #[test]
    fn corner_frame_sits_on_the_origin_corner() {
        let Some(mut app) = make_app() else { return };
        app.page_w_mm = 210.0;
        app.page_h_mm = 297.0;
        app.origin_corner = 0;
        app.reverse_y = true;
        let (px, py) = app.origin_page_point();
        assert_eq!((px, py), (0.0, 297.0), "默认原点应在纸的左下角");
        let (_, h) = app.corner_frame_size();
        assert_eq!(297.0 - h, 17.0, "A4 与默认框高差 17mm，正是旧版错位的量");
    }

    /// 大图分页：按机器可绘制幅面切块，页数/原点正确。
    #[test]
    fn tile_layout_splits_large_image() {
        let Some(mut app) = make_app() else { return };
        app.page_w_mm = 210.0;
        app.page_h_mm = 297.0;
        app.machine_w_mm = 200.0;
        app.machine_h_mm = 300.0;
        push_test_image(&mut app, 500.0, 650.0);

        let layout = app.tile_layout().expect("有输出图片就应有分页布局");
        assert_eq!((layout.width_mm, layout.height_mm), (200.0, 280.0));
        assert_eq!(
            (layout.cols, layout.rows),
            (3, 3),
            "500×650 应按单页 200×280 切 3×3"
        );
        assert_eq!(layout.count(), 9);
        assert_eq!(layout.origin(0), Some((0.0, 0.0)));
        assert_eq!(layout.origin(1), Some((200.0, 0.0)));
        assert_eq!(layout.origin(3), Some((0.0, 280.0)));
        assert_eq!(layout.origin(8), Some((400.0, 560.0)));
        assert_eq!(layout.origin(9), None);
    }

    /// 分页裁剪：每页笔迹平移到页内，且附带四角定位标记。
    #[test]
    fn tiled_page_clips_and_translates_image() {
        let Some(mut app) = make_app() else { return };
        app.page_w_mm = 210.0;
        app.page_h_mm = 297.0;
        app.machine_w_mm = 200.0;
        app.machine_h_mm = 300.0;
        push_test_image(&mut app, 500.0, 650.0);

        let p0 = app.tiled_page_polylines(0);
        assert_eq!(p0.len(), 1 + 9, "一条裁剪段 + 九条角标（4 角 × 2 臂 + 原点斜线）");
        let line = &p0[0].pts;
        assert!((line[0].0).abs() < 1e-9 && (line[0].1).abs() < 1e-9);
        let end = *line.last().unwrap();
        assert!(
            (end.0 - 200.0).abs() < 1e-6 && (end.1 - 260.0).abs() < 1e-6,
            "对角线应在右边界 (200,260) 处裁断，实际 {end:?}"
        );
        assert!(
            line.iter().all(|(x, y)| (0.0..=200.0).contains(x) && (0.0..=280.0).contains(y)),
            "裁剪后所有点都必须落在单页范围内"
        );

        // 第二页：同一对角线被平移到局部 0..0 起点附近，且不与第一页重叠。
        let p1 = app.tiled_page_polylines(1);
        let e1 = *p1[0].pts.first().unwrap();
        assert!(
            (e1.0 - 0.0).abs() < 1e-6 && (e1.1 - 260.0).abs() < 1e-6,
            "第二页应从 (200,260) 的局部 (0,260) 继续，实际 {e1:?}"
        );
        // 画布页内预览用的是「仅内容」接口，角标单独用橙色绘制。
        assert_eq!(
            app.tiled_page_content_polylines(1).len(),
            1,
            "内容接口应只返回图片笔迹，不含四角标记"
        );

        app.tile_marks = false;
        assert_eq!(app.tiled_page_polylines(0).len(), 1, "关掉角标后只剩笔迹");
    }

    /// 缩略图条要展示每一页：一次性缓存必须覆盖全部分页且与按页重算一致。
    #[test]
    fn tile_cache_fills_every_page_for_thumbnails() {
        let Some(mut app) = make_app() else { return };
        app.page_w_mm = 210.0;
        app.page_h_mm = 297.0;
        app.machine_w_mm = 200.0;
        app.machine_h_mm = 300.0;
        push_test_image(&mut app, 500.0, 650.0);

        app.fill_tile_cache();
        let count = app.tile_layout().unwrap().count();
        assert_eq!(app.tile_cache_pages.len(), count);
        let mut nonempty = 0;
        for page in 0..count {
            let cached = app.tile_cache_pages[page]
                .as_ref()
                .unwrap_or_else(|| panic!("第 {page} 页缩略图内容未缓存"));
            let direct = app.tiled_page_content_polylines(page);
            assert_eq!(cached.len(), direct.len(), "第 {page} 页缓存应与直接计算一致");
            if !cached.is_empty() {
                nonempty += 1;
            }
        }
        assert!(nonempty >= 2, "对角线应经过多页，实际只有 {nonempty} 页有内容");
    }

    /// 分页 G-code 只含单页内容、不超机器幅面，且结尾回到当前页原点。
    #[test]
    fn tile_gcode_is_per_page_and_returns_to_origin() {
        let Some(mut app) = make_app() else { return };
        app.page_w_mm = 210.0;
        app.page_h_mm = 297.0;
        app.machine_w_mm = 200.0;
        app.machine_h_mm = 300.0;
        push_test_image(&mut app, 500.0, 650.0);
        app.texts[0].text = "不应出现在分页输出里".into();
        app.tile_on = true;
        app.tile_page = 0;
        app.tile_marks = true;

        let g = app.generate_gcode();
        assert!(g.contains("大纸张分页"), "应标注分页信息：\n{g}");
        assert!(g.contains("第 1/9 页"), "当前页信息不对：\n{g}");
        assert!(g.contains("G92 X0 Y0"), "每页都必须以当前位置为零点");

        let (min_x, min_y, max_x, max_y) =
            wrobot_core::gcode_xy_bounds(&g).expect("GRBL 输出应能解析 XY 范围");
        assert!(max_x - min_x <= 200.0 + 1e-6, "单页宽度不能超过 200mm");
        assert!(max_y - min_y <= 280.0 + 1e-6, "单页高度不能超过 280mm");
        assert!(
            app.bounds_warning(&g).is_none(),
            "分页输出不应触发超界警告"
        );
        assert!(
            g.trim_end().ends_with("G0 X0 Y0 ; 回到页面零点（当前原点）"),
            "分页画完必须回到原点：\n{}",
            g.lines().rev().take(3).collect::<Vec<_>>().join("\n")
        );
        let g1 = g.lines().filter(|l| l.starts_with("G1 ")).count();
        assert_eq!(
            g1, 20,
            "单页 = 10 段描绘 + 10 次落笔（Z 轴也是 G1），实际 {g1} 段"
        );
    }

    /// BUG 修复：非 tab 分隔（逗号/空格）的表格内容现在能正确切分。
    #[test]
    fn table_splits_comma_and_space() {
        let Some(mut app) = make_app() else { return };
        app.tab = 1;
        app.table_cols = 3;
        app.table_rows = 2;
        app.table_text = "甲,乙,丙\n1 2 3".into();
        let items = app.build_table_items();
        // 1 个边框 line 元素 + 6 个单元格文本元素
        assert_eq!(items.len(), 1 + 6, "逗号/空格分隔应切出 6 个单元格，实际 {}", items.len());
    }

    /// BUG 修复：只并入当前模块（避免多模块坐标重叠）。
    #[test]
    fn compose_only_active_module() {
        let Some(mut app) = make_app() else { return };
        app.tab = 0;
        assert!(app.module_active(0), "当前 tab 模块应激活");
        assert!(!app.module_active(1), "非当前模块默认不并入");

        let n_only = app.compose_doc().items.len();
        app.module_on[1] = true;
        let n_with = app.compose_doc().items.len();
        assert!(n_with > n_only, "显式开启后应叠加表格元素");
    }

    /// BUG #59 修复：切到非内容页（图片/设备/…）后，未勾选叠加的内容模块不应再自动残留。
    ///
    /// 旧实现靠 `last_content_tab` 在非内容页自动带入上一个内容模块，
    /// 导致切到图片/设备页后旧笔迹仍留在画布上，干扰定位、也让人误以为没切换成功。
    #[test]
    fn switching_to_noncontent_tab_clears_unoverlaid_content() {
        let Some(mut app) = make_app() else { return };
        // 在文字页应有内容
        app.tab = 0;
        assert!(app.texts[0].text.len() > 0, "默认应有一个含文字的文本框");
        app.rebuild_preview();
        assert!(!app.preview.is_empty(), "文字页应有笔迹");
        assert!(app.module_active(0), "当前文字页应激活文字模块");

        // 切到「图片」页（非内容页）且未勾选任何叠加
        for i in 0..app.module_on.len() {
            app.module_on[i] = false;
        }
        app.images_on = false;
        app.tab = 5;
        app.rebuild_preview();

        // 旧实现靠 last_content_tab 自动带入文字模块 → 这里必须不再带入
        assert!(
            !app.module_active(0),
            "非内容页未勾选叠加时，文字模块不应激活"
        );
        assert!(
            app.collect_polylines(false).is_empty(),
            "非内容页且未勾选叠加时，页面不应残留任何内容"
        );

        // 显式勾选叠加后又应出现
        app.module_on[0] = true;
        app.rebuild_preview();
        assert!(
            app.module_active(0),
            "显式勾选叠加后文字模块应激活"
        );
        assert!(
            !app.preview.is_empty(),
            "勾选叠加后文字笔迹应重新出现"
        );
    }

    /// 视图辅助默认值：标尺默认开、自定义网格默认关。
    #[test]
    fn view_ruler_grid_defaults() {
        let Some(app) = make_app() else { return };
        assert!(app.ruler_on, "标尺默认开启");
        assert!(!app.grid_overlay, "自定义网格默认关闭");
        assert_eq!(app.grid_overlay_cols, 10);
        assert_eq!(app.grid_overlay_rows, 14);
    }

    /// BUG 修复：重新随机应真正改变笔迹（旧实现固定种子永不变化）。
    #[test]
    fn random_reseed_changes_output() {
        let Some(mut app) = make_app() else { return };
        app.tab = 0;
        app.texts[0].text = "中文测试".into();
        app.use_random = true;
        app.rand_x = 0.8;
        app.rand_y = 0.8;
        app.rebuild_preview();
        let a = app.preview.first().and_then(|p| p.pts.first()).copied();
        app.rebuild_preview_random();
        let b = app.preview.first().and_then(|p| p.pts.first()).copied();
        let (Some((ax, ay)), Some((bx, by))) = (a, b) else {
            panic!("随机预览不应为空");
        };
        assert!(
            (ax - bx).abs() > 1e-9 || (ay - by).abs() > 1e-9,
            "重新随机后笔迹应发生变化"
        );
    }

    /// 新增：页面边框（edge）模块。
    #[test]
    fn border_module_generates_lines() {
        let Some(mut app) = make_app() else { return };
        app.tab = 0;
        app.texts[0].text = String::new(); // 无文字内容
        app.border_on = false;
        app.rebuild_preview();
        assert!(app.preview.is_empty(), "无内容无边框时应为空");

        app.border_on = true;
        app.rebuild_preview();
        assert!(!app.preview.is_empty(), "开启边框后应产生笔迹");

        app.border_style = 1; // 虚线
        app.rebuild_preview();
        assert!(!app.preview.is_empty(), "虚线边框应产生笔迹");
    }

    /// 新增：PDF 矢量导出。
    #[test]
    fn pdf_export_is_valid() {
        let Some(mut app) = make_app() else { return };
        app.tab = 0;
        let bytes = app.generate_pdf();
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.starts_with("%PDF-1.4"), "缺少 PDF 文件头");
        assert!(s.contains("/Type /Page"), "缺少页面对象");
        assert!(s.trim_end().ends_with("%%EOF"), "缺少 EOF 标记");
    }

    /// 新增：激光模式用 M3/M5 代替 Z 轴落笔。
    #[test]
    fn laser_mode_emits_m3_m5() {
        let Some(mut app) = make_app() else { return };
        app.tab = 0;
        app.texts[0].text = "一".into();
        app.laser_on = true;
        app.laser_power = 800.0;
        let g = app.generate_gcode();
        assert!(g.contains("M3 S800"), "激光模式应输出 M3 S800，实际:\n{}", g);
        assert!(g.contains("M5"), "激光模式应输出 M5 关光");
        assert!(!g.contains("G1 Z"), "激光模式不应再有 Z 轴落笔");
    }

    /// 回归 BUG：整数格式化曾被截掉尾零，导致 `F10000` 输出成 `F1`（机器会以极慢速度运行）。
    #[test]
    fn feed_rate_not_truncated() {
        let Some(mut app) = make_app() else { return };
        app.tab = 0;
        app.texts[0].text = "一".into();
        app.feed_rate = 10000.0;
        app.z_speed = 10000.0;
        let g = app.generate_gcode();
        let head: String = g.chars().take(400).collect();
        assert!(g.contains("F10000"), "进给速率应保持 10000，实际输出:\n{}", head);
        assert!(!g.contains("F1\n"), "不应出现被截断的 F1");
    }

    /// 新增：自动休眠与落笔/抬笔延时。
    #[test]
    fn auto_sleep_and_delays() {
        let Some(mut app) = make_app() else { return };
        app.tab = 0;
        app.texts[0].text = "一".into();
        app.auto_sleep = true;
        app.tool_on_delay = 0.5;
        app.tool_off_delay = 0.25;
        let g = app.generate_gcode();
        assert!(g.contains("$SLP"), "自动休眠应输出 $SLP");
        assert!(g.contains("G4 P0.5"), "落笔后暂停应输出 G4 P0.5");
        assert!(g.contains("G4 P0.25"), "抬笔前暂停应输出 G4 P0.25");
    }

    /// 新增：原点角镜像（0 左上 / 3 右下）。
    #[test]
    fn origin_corner_mirrors_coordinates() {
        let mut o = wrobot_core::GcodeOptions::default();
        o.page_w = 100.0;
        o.page_h = 100.0;
        o.reverse_x = false;
        o.reverse_y = false;
        o.swap_xy = false;
        o.origin_corner = 0;
        let (x, y) = o.transform(10.0, 20.0);
        assert!((x - 10.0).abs() < 1e-9 && (y - 20.0).abs() < 1e-9, "左上原点不镜像");

        o.origin_corner = 3; // 右下：X、Y 均镜像
        let (x, y) = o.transform(10.0, 20.0);
        assert!((x - 90.0).abs() < 1e-9, "右下原点 X 应镜像为 90，实际 {}", x);
        assert!((y - 80.0).abs() < 1e-9, "右下原点 Y 应镜像为 80，实际 {}", y);
    }

    /// 新增：点动 G-code 三种固件均可生成。
    #[test]
    fn jog_gcode_all_firmwares() {
        let Some(app) = make_app() else { return };
        for fw in 0..=2 {
            let mut a = AppState::new(
                FontManager::load(std::path::Path::new(FONT_DIR)),
                FONT_DIR.into(),
            );
            a.firmware_sel = fw;
            let g = a.jog_gcode(10.0, -5.0);
            assert!(!g.is_empty(), "固件 {} 点动指令不应为空", fw);
        }
        let _ = app;
    }

    /// BUG 修复：表格文字应落在单元格范围内，不得溢出到表格之外（item 6）。
    #[test]
    fn table_text_stays_in_cell() {
        let Some(mut app) = make_app() else { return };
        app.tab = 1;
        app.table_rows = 3;
        app.table_cols = 3;
        app.table_cell = 14.0;
        app.table_x = 20.0;
        app.table_y = 20.0;
        app.table_auto_font = true;
        app.table_text = "甲\t乙\t丙\n1\t2\t3\n中\t文\t字".into();
        app.rebuild_preview();
        let (x0, y0, x1, y1) = wrobot_core::render::bounds_all(&app.preview);
        let tw = app.table_cell * app.table_cols as f64;
        let th = app.table_cell * app.table_rows as f64;
        assert!(x0 >= app.table_x - 1e-6, "文字左界 {:.2} 不应小于表格左 {:.2}", x0, app.table_x);
        assert!(y0 >= app.table_y - 1e-6, "文字上界 {:.2} 不应小于表格上 {:.2}", y0, app.table_y);
        assert!(x1 <= app.table_x + tw + 1e-6, "文字右界 {:.2} 不应超出表格右 {:.2}", x1, app.table_x + tw);
        assert!(y1 <= app.table_y + th + 1e-6, "文字下界 {:.2} 不应超出表格下 {:.2}", y1, app.table_y + th);
    }

    /// BUG 修复：格子示范字应与格中心**精确重合**（字心 == 格心）。
    ///
    /// 旧实现把字形原点（`.gfont` 在基线上）当成字框左上角，字整体偏上约 0.45 倍字高；
    /// 现在按字库墨迹框逐字补偿，偏差应为 0。
    /// 测试里关掉横竖线条，让预览只剩示范字，排除网格线把包围盒撑成整格的干扰。
    #[test]
    fn grid_sample_centered_in_cell() {
        let Some(mut app) = make_app() else { return };
        app.tab = 2;
        app.grid_kind = 0; // 空白格
        app.grid_rows = 1;
        app.grid_cols = 1;
        app.grid_cell = 18.0;
        app.grid_x = 10.0;
        app.grid_y = 10.0;
        app.grid_show_h = false; // 去掉线条，只留示范字
        app.grid_show_v = false;
        app.grid_auto_font = true;
        app.grid_sample = "永".into();
        app.rebuild_preview();
        assert!(!app.preview.is_empty(), "应只输出示范字笔迹");
        let (x0, y0, x1, y1) = wrobot_core::render::bounds_all(&app.preview);
        assert!(x0 >= 10.0 && x1 <= 28.0, "示范字 x [{x0:.2},{x1:.2}] 应落在格内");
        assert!(y0 >= 10.0 && y1 <= 28.0, "示范字 y [{y0:.2},{y1:.2}] 应落在格内");
        let cx = (x0 + x1) / 2.0;
        let cy = (y0 + y1) / 2.0;
        assert!((cx - 19.0).abs() < 0.6, "示范字中心 x {cx:.2} 应落在格心 19");
        assert!((cy - 19.0).abs() < 0.6, "示范字中心 y {cy:.2} 应落在格心 19");
    }

    /// 新增：格子「显示 / 隐藏」总开关。
    #[test]
    fn grid_show_toggle_outputs_nothing() {
        let Some(mut app) = make_app() else { return };
        app.tab = 2;
        app.grid_sample = "永".into();
        app.grid_show = true;
        app.rebuild_preview();
        let on = app.preview.len();
        assert!(on > 0, "开启时应有笔迹");
        app.grid_show = false;
        app.rebuild_preview();
        assert!(app.preview.is_empty(), "关闭后不应再输出任何格子内容");
    }

    /// 新增：横行 / 竖行可分别显示隐藏。
    #[test]
    fn grid_rows_cols_toggle() {
        let Some(mut app) = make_app() else { return };
        app.tab = 2;
        app.grid_kind = 0; // 空白格：只有行列线，无格型辅助线
        app.grid_sample = String::new();
        app.grid_rows = 4;
        app.grid_cols = 3;

        app.grid_show_h = true;
        app.grid_show_v = true;
        let both = app.build_grid_items();
        let n_both: usize = both.iter().filter_map(|i| i.shapes.as_ref().map(|s| s.len())).sum();

        app.grid_show_h = false;
        let only_v = app.build_grid_items();
        let n_v: usize = only_v.iter().filter_map(|i| i.shapes.as_ref().map(|s| s.len())).sum();

        app.grid_show_h = true;
        app.grid_show_v = false;
        let only_h = app.build_grid_items();
        let n_h: usize = only_h.iter().filter_map(|i| i.shapes.as_ref().map(|s| s.len())).sum();
        // 4 行 → 5 条横线；3 列 → 4 条竖线
        assert_eq!(n_both, 5 + 4, "横竖全开应为 9 条，实际 {n_both}");
        assert_eq!(n_v, 4, "只显示竖行应为 4 条，实际 {n_v}");
        assert_eq!(n_h, 5, "只显示横行应为 5 条，实际 {n_h}");
    }

    /// 新增：线条宽度 > 0.35mm 时用多条并行描边实现加粗。
    #[test]
    fn grid_line_width_thickens() {
        let Some(mut app) = make_app() else { return };
        app.tab = 2;
        app.grid_kind = 0;
        app.grid_sample = String::new();
        app.grid_rows = 2;
        app.grid_cols = 2;
        app.grid_line_width = 0.35;
        let thin: usize = app
            .build_grid_items()
            .iter()
            .filter_map(|i| i.shapes.as_ref().map(|s| s.len()))
            .sum();
        app.grid_line_width = 1.4;
        let thick: usize = app
            .build_grid_items()
            .iter()
            .filter_map(|i| i.shapes.as_ref().map(|s| s.len()))
            .sum();
        assert_eq!(thin, 3 + 3, "细线应为 6 条，实际 {thin}");
        assert!(thick > thin, "加粗后应为多条并行描边，实际 {thick} <= {thin}");
        // 加粗后线束应仍落在原线附近（偏移量不超过线宽）
        let shapes = app.build_grid_items();
        let lines = shapes[0].shapes.clone().unwrap_or_default();
        let (x0, _y0, _x1, _y1) = wrobot_core::render::bounds_all(
            &lines.iter().cloned().map(wrobot_core::render::Polyline::new).collect::<Vec<_>>(),
        );
        assert!((x0 - app.grid_x).abs() <= 1.5, "加粗偏移不应超过线宽，实际左界 {x0:.2}");
    }

    /// 新增：图片变换——缩放保持中心不动，命中测试与逆变换自洽。
    #[test]
    fn image_transform_scale_and_hit() {
        let Some(mut app) = make_app() else { return };
        // 手工放一张 40×20mm 的「图片」（用空笔迹即可，只验证变换数学）
        app.images.push(crate::state::ImageObject {
            path: "test".into(),
            bytes: Vec::new(),
            x: 10.0,
            y: 20.0,
            scale: 1.0,
            rotation: 0.0,
            flip_h: false,
            flip_v: false,
            draw: crate::state::ImgDraw::Wire,
            step_px: 3,
            threshold: 0.5,
            edge_threshold: 0.25,
            invert: false,
            crop: (0.0, 0.0, 1.0, 1.0),
            scan_angle: 0,
            merge_gap_px: 8,
            order_mode: 0,
            block_px: 40,
            sketch_pressure: 2,
            sketch_jitter: 0.35,
            sketch_pen: 0.5,
            hatch_levels: 6,
            hatch_angle: 35.0,
            region_block_mm: 2.5,
            stroke_len_mm: 3.2,
            stroke_follow: 2,
            multi_pen: false,
            background: false,
            out: true,
            base: Vec::new(),
            base_pens: vec![],
            natural_w: 40.0,
            natural_h: 20.0,
            gen_key: 0,
        });
        let i = 0;
        let (cx0, cy0) = (10.0 + 20.0, 20.0 + 10.0); // 缩放前中心
        app.scale_image(i, 2.0);
        let (cx1, cy1, scale_now, before_x) = {
            let im = &app.images[i];
            (
                im.x + im.natural_w * im.scale * 0.5,
                im.y + im.natural_h * im.scale * 0.5,
                im.scale,
                im.x,
            )
        };
        assert!((scale_now - 2.0).abs() < 1e-9, "缩放应生效");
        assert!((cx1 - cx0).abs() < 1e-9 && (cy1 - cy0).abs() < 1e-9, "缩放应保持中心不动");

        // 命中测试：中心命中、远离则不命中
        assert!(app.image_hit(i, cx1, cy1), "中心应命中");
        assert!(!app.image_hit(i, cx1 + 200.0, cy1), "远处不应命中");

        // 逆变换往返一致（含旋转与镜像）
        app.images[i].rotation = 37.0;
        app.images[i].flip_h = true;
        let p = app.images[i].to_page(5.0, 7.0);
        let q = app.images[i].from_page(p.0, p.1);
        assert!((q.0 - 5.0).abs() < 1e-6 && (q.1 - 7.0).abs() < 1e-6, "逆变换应还原: {q:?}");

        // 位置微调与清除
        app.nudge_image(i, 3.0, -4.0);
        assert!((app.images[i].x - (before_x + 3.0)).abs() < 1e-9, "微调应改变位置");
        app.clear_images();
        assert!(app.images.is_empty(), "清除全部图片应清空");
        assert_eq!(app.sel_image, 0, "清除后选中应回落 0");
    }

    /// 模拟（Mock）后端必须真的能用：连接后下发的 G-code 要完整回显到日志。
    #[test]
    fn mock_backend_echoes_gcode() {
        let Some(mut app) = make_app() else { return };
        app.dev_kind = 2; // 模拟
        app.connect_device();
        std::thread::sleep(std::time::Duration::from_millis(250));
        assert!(app.device.is_connected(), "模拟模式应处于已连接状态");

        app.send_gcode("G1 X10 Y10");
        std::thread::sleep(std::time::Duration::from_millis(400));
        let log = app.device.log_snapshot();
        assert!(log.contains("模拟模式"), "日志应标明模拟模式：{log}");
        assert!(log.contains("G1 X10 Y10"), "日志应回显下发的 G-code：{log}");
    }

    /// 重复连接不应「被自己占用」：连接前必须先释放上一个 transport。
    #[test]
    fn reconnect_releases_previous_transport() {
        let Some(mut app) = make_app() else { return };
        app.dev_kind = 2;
        app.connect_device();
        std::thread::sleep(std::time::Duration::from_millis(200));
        app.connect_device(); // 再连一次
        std::thread::sleep(std::time::Duration::from_millis(300));
        let log = app.device.log_snapshot();
        assert!(
            log.contains("已释放上一个连接"),
            "重复连接应先释放旧连接：{log}"
        );
        assert!(app.device.is_connected(), "重连后应仍保持连接");
    }

    /// 文字模块支持多个文本框：默认 1 个，可增删；每个框独立输出。
    #[test]
    fn text_module_supports_multiple_boxes() {
        let Some(mut app) = make_app() else { return };
        app.ensure_texts();
        assert_eq!(app.texts.len(), 1, "初始应为 1 个文本框");

        app.texts[0].text = "第一段".to_string();
        app.add_text_box();
        assert_eq!(app.texts.len(), 2);
        assert_eq!(app.sel_text, 1, "新增后应自动选中新框");
        assert!(app.texts[1].text.is_empty(), "新框内容应为空");
        assert!(app.texts[1].y > app.texts[0].y, "新框位置应下移，避免完全重叠");

        app.texts[1].text = "第二段".to_string();
        let items = app.build_text_items();
        assert_eq!(items.len(), 2, "两个有内容的框应输出两个元素");

        // 关掉一个框就不输出它，但内容还留着
        app.texts[0].on = false;
        assert_eq!(app.build_text_items().len(), 1, "关闭的框不应输出");

        app.remove_text_box();
        assert_eq!(app.texts.len(), 1, "删除后应只剩一个");
        assert_eq!(app.sel_text, 0);
    }

    /// 未勾选「图片」时，前景图片不得被并入页面（BUG：以前无条件叠加）。
    #[test]
    fn images_not_merged_unless_enabled() {
        let Some(mut app) = make_app() else { return };
        app.tab = 0;
        app.images_on = false;
        assert!(!app.images_active(), "停在文字页且未勾选时不应并入图片");

        // 造一张有笔迹的前景图片
        let mut img = crate::state::ImageObject {
            path: "t.png".into(),
            bytes: Vec::new(),
            x: 0.0,
            y: 0.0,
            scale: 1.0,
            rotation: 0.0,
            flip_h: false,
            flip_v: false,
            draw: crate::state::ImgDraw::Wire,
            step_px: 2,
            threshold: 0.5,
            edge_threshold: 0.5,
            invert: false,
            crop: (0.0, 0.0, 1.0, 1.0),
            scan_angle: 0,
            merge_gap_px: 8,
            order_mode: 0,
            block_px: 40,
            sketch_pressure: 2,
            sketch_jitter: 0.35,
            sketch_pen: 0.5,
            hatch_levels: 6,
            hatch_angle: 35.0,
            region_block_mm: 2.5,
            stroke_len_mm: 3.2,
            stroke_follow: 2,
            multi_pen: false,
            background: false,
            out: true,
            base: vec![wrobot_core::Polyline::new(vec![(0.0, 0.0), (10.0, 10.0)])],
            base_pens: vec![],
            natural_w: 10.0,
            natural_h: 10.0,
            gen_key: 0,
        };
        let _ = &mut img;
        app.images.push(img);
        app.texts[0].text = String::new();

        let n_off = app.collect_polylines(false).len();
        app.images_on = true;
        let n_on = app.collect_polylines(false).len();
        assert_eq!(n_off, 0, "未勾选时图片笔迹不应出现");
        assert_eq!(n_on, 1, "勾选后应出现图片笔迹");
    }

    /// 机器参数与参考项目 `default.properties` 一致。
    #[test]
    fn machine_defaults_match_reference_project() {
        let Some(app) = make_app() else { return };
        assert_eq!(app.feed_rate, 10000.0, "feed_rate=10000");
        assert_eq!(app.jog_speed, 10000.0, "jog_speed=10000");
        assert_eq!(app.machine_w_mm, 200.0, "默认绘制宽度 200mm");
        assert_eq!(app.machine_h_mm, 300.0, "默认绘制长度 300mm");
        assert_eq!(app.mm_to_steps, 100.0, "mm_to_steps=100");
        assert_eq!(app.max_pen_speed, 32767.0, "max_pen_speed=32767");
        assert_eq!(app.ebb_servo_speed, 750.0, "servo_speed=750");
        assert_eq!(app.ebb_max_pen, 30000.0, "max_pen_value=30000");
        assert_eq!(app.pen_up, 0.0, "grbl zOff=0");
        assert_eq!(app.pen_down, 5.0, "grbl zOn=5");
        assert_eq!(app.z_speed, 10000.0, "zSpeed=10000");
        assert_eq!(app.marlin_pen_up, 0.0, "marlin_pen_up_mm=0");
        assert_eq!(app.marlin_pen_down, 2.0, "marlin_pen_down_mm=2");
        assert_eq!(app.marlin_z_speed, 20000.0, "marlin_z_speed=20000");
        assert_eq!(app.marlin_servo_up, 50.0, "marlin_pen_up=50");
        assert_eq!(app.marlin_servo_down, 0.0, "marlin_pen_down=0");
        assert_eq!(app.laser_power, 1000.0, "laser_power=1000");
        assert_eq!(app.origin_corner, 0, "axe_pos=0");
        assert!(!app.reverse_x && app.reverse_y, "axe_reverse_x=false / y=true");
        assert!(!app.swap_xy, "axe_rev_xy=false");
        assert!(app.auto_set_zero, "auto_set_zero=true");
        assert!(app.auto_connect, "auto_connect=true");
        assert!(!app.auto_sleep, "autoSleep=false");
        assert!(!app.single_step, "single_step_command=false");
    }

    /// 点动速度用的是 jog_speed，不占用绘图速度。
    #[test]
    fn jog_uses_jog_speed_not_feed_rate() {
        let Some(mut app) = make_app() else { return };
        app.firmware_sel = 0;
        app.feed_rate = 3000.0;
        app.jog_speed = 10000.0;
        let g = app.jog_gcode(10.0, 0.0);
        assert!(g.contains("F10000"), "点动应带 jog_speed：{g}");
        assert!(!g.contains("F3000"), "不应混用绘图速度：{g}");
    }

    // ── 本轮新增（BUG #43~#50）────────────────────────────

    /// ① XY 最小步进：默认 0.2mm，开启后 G-code 行数明显下降。
    #[test]
    fn xy_min_step_default_and_effect() {
        let Some(mut app) = make_app() else { return };
        app.tab = 0;
        app.texts[0].text = "永和九年岁在癸丑暮春之初会于会稽山阴之兰亭".into();
        assert_eq!(app.min_step_mm, 0.2, "默认最小步进应为 0.2mm");
        assert_eq!(app.gcode_options().min_step, 0.2);

        app.min_step_mm = 0.0; // 关闭抽稀
        let n_off = app.generate_gcode().lines().filter(|l| l.starts_with("G1 X")).count();
        app.min_step_mm = 0.2;
        let n_on = app.generate_gcode().lines().filter(|l| l.starts_with("G1 X")).count();
        assert!(n_on > 0, "抽稀后仍应有落笔移动");
        assert!(
            n_on < n_off,
            "开启最小步进后 G-code 行数应减少：{} -> {}",
            n_off,
            n_on
        );
    }

    /// BUG #56：G-code 必须以「当前设置的原点」计算（自带 `G92 X0 Y0`），
    /// 否则 `G90` 坐标会被当机器绝对值解析 → 超出机器行程 → 软限位报警。
    #[test]
    fn gcode_origin_follows_auto_set_zero() {
        let Some(mut app) = make_app() else { return };
        app.tab = 0;
        app.texts[0].text = "永".into();
        assert!(app.auto_set_zero, "默认 auto_set_zero=true（以当前位置为原点）");
        assert!(
            app.gcode_options().use_current_origin,
            "auto_set_zero 应接到 use_current_origin"
        );

        let g = app.generate_gcode();
        assert!(g.contains("G92 X0 Y0"), "生成的 G-code 应自带原点锚定：\n{g}");
        assert!(
            g.contains("回到页面零点"),
            "锚定后结尾应回页面零点而非机械原点：\n{}",
            g.lines().last().unwrap_or("")
        );

        // 关掉开关 → 退回机器绝对坐标（保留既有行为，不强制）
        app.auto_set_zero = false;
        assert!(!app.gcode_options().use_current_origin);
        let g2 = app.generate_gcode();
        assert!(!g2.contains("G92 X0 Y0"), "关掉后不该再输出 G92");
    }

    /// BUG #56 同类：测试方框 G-code 也必须以「当前位置」为原点（自带 G92），
    /// 否则关掉「运行前自动设原点」后，测试方框会按机器绝对坐标跑 → 超出行程。
    #[test]
    fn test_frame_anchors_current_origin() {
        // GRBL：必须自带 G92 锚定，且 G92 在任何移动指令之前
        let g = AppState::test_frame_gcode(50.0, 30.0, 5.0, 0.0, 1000.0, Firmware::Grbl);
        assert!(g.contains("G92 X0 Y0"), "测试方框 G-code 应自带原点锚定：\n{g}");
        assert!(g.contains("以当前位置为原点"), "锚定注释应说明是当前原点");
        let i92 = g.find("G92 X0 Y0").expect("应含 G92");
        let i_move = g.find("G0 X").or_else(|| g.find("G1 X")).expect("应含移动指令");
        assert!(i92 < i_move, "G92 必须出现在第一个移动指令之前");

        // EBB 不认 G92，不应输出
        let ge = AppState::test_frame_gcode(50.0, 30.0, 5.0, 0.0, 1000.0, Firmware::Ebb);
        assert!(!ge.contains("G92"), "EBB 测试方框不该含 G92：\n{ge}");
    }

    /// 图片「高效（连续笔画）」：把相邻行的墨段串成长笔画，抬落笔次数应大幅下降，
    /// 空移几乎归零，同时不能少画任何一笔。
    #[test]
    fn continuous_draw_mode_cuts_pen_lifts() {
        use wrobot_core::Polyline;
        // 120×120 纯黑方块，缩放到 60mm 高 → 0.5 mm/px，行距 step=4 → 30 行
        let (w, h) = (120u32, 120u32);
        let gray = image::GrayImage::from_fn(w, h, |_, _| image::Luma([0u8]));
        let base = crate::io::ImageGenOpts {
            target_h_mm: 60.0,
            step_px: 4,
            threshold: 0.5,
            edge_threshold: 0.5,
            invert: false,
            draw_type: 1, // 素描：逐行扫描（旧方式）
            crop: (0.0, 0.0, 1.0, 1.0),
            scan_angle: 0,
            merge_gap_px: 8,
            order_mode: 0,
            block_px: 40,
            sketch_pressure: 2,
            sketch_jitter: 0.35,
            sketch_pen: 0.5,
            hatch_levels: 6,
            hatch_angle: 35.0,
            ..Default::default()
        };
        let rowish = crate::io::gray_to_polylines(&gray, &base);
        let cont = crate::io::gray_to_polylines(
            &gray,
            &crate::io::ImageGenOpts {
                draw_type: 2,
                ..base
            },
        );

        assert_eq!(rowish.polylines.len(), 30, "旧方式每行一条独立横线");
        assert!(
            cont.polylines.len() <= 2,
            "整块洋黑应串成 1~2 条连续笔画，实际 {} 条",
            cont.polylines.len()
        );

        // 空移（抬笔后白白跑掉的距离）：旧方式每行都要跳回最左端
        let idle = |ps: &[Polyline]| {
            wrobot_core::path_travel(ps, (0.0, 0.0)) - wrobot_core::total_length(ps)
        };
        assert!(
            idle(&cont.polylines) < 1.0,
            "连续笔画几乎不该有空移：{}mm",
            idle(&cont.polylines)
        );
        assert!(
            idle(&rowish.polylines) > 100.0,
            "逐行方式空移应很可观：{}mm",
            idle(&rowish.polylines)
        );

        // 墨迹守恒：串起来只多了行间连接，绝不能少画
        let ink_row = wrobot_core::total_length(&rowish.polylines);
        let ink_cont = wrobot_core::total_length(&cont.polylines);
        assert!(
            ink_cont > ink_row,
            "不能少画：连续={ink_cont} 逐行={ink_row}"
        );
        assert!(
            ink_cont < ink_row * 1.1,
            "连接器不该让墨迹暴涨：连续={ink_cont} 逐行={ink_row}"
        );
    }

    /// 左右两个互不相连的色块不能被跨空隙硬连——横向不重叠就该各成一条笔画。
    #[test]
    fn continuous_mode_does_not_bridge_separate_shapes() {
        let (w, h) = (100u32, 60u32);
        // 左 0..40 与右 60..100 黑，中间 20px 是白的
        let gray = image::GrayImage::from_fn(w, h, |x, _y| {
            let v = if x < 40 || x >= 60 { 0u8 } else { 255u8 };
            image::Luma([v])
        });
        let r = crate::io::gray_to_polylines(
            &gray,
            &crate::io::ImageGenOpts {
                target_h_mm: 60.0,
                step_px: 5,
                threshold: 0.5,
                edge_threshold: 0.5,
                invert: false,
                draw_type: 2,
                crop: (0.0, 0.0, 1.0, 1.0),
                scan_angle: 0,
                merge_gap_px: 8,
                order_mode: 0,
                block_px: 40,
                sketch_pressure: 2,
                sketch_jitter: 0.35,
                sketch_pen: 0.5,
                hatch_levels: 6,
                hatch_angle: 35.0,
                ..Default::default()
            },
        );
        assert_eq!(
            r.polylines.len(),
            2,
            "左右两块应各自串成一条笔画，实际 {} 条",
            r.polylines.len()
        );
    }

    /// ① 边界自检：内容宽超过机器行程宽时要提醒，缩到幅面内就不再提醒。
    #[test]
    fn bounds_check_warns_when_content_exceeds_machine_area() {
        let Some(mut app) = make_app() else { return };
        assert!(app.bounds_check, "超界检测应默认开启");
        app.texts[0].text = "测试".into();
        app.rebuild_preview();
        let g = app.generate_gcode();
        assert!(
            app.bounds_warning(&g).is_none(),
            "默认 A4 页面应在默认机器幅面 (200×280) 之内"
        );
        // 把机器行程压窄到比内容小 → 必须报警
        let (x0, _y0, x1, _y1) = wrobot_core::render::bounds_all(&app.preview);
        app.machine_w_mm = (x1 - x0).ceil() - 1.0;
        let warn = app
            .bounds_warning(&g)
            .expect("内容宽超过机器行程宽应报警");
        assert!(warn.contains("机器行程宽"), "提示应说明是宽度超界：{warn}");
        // 关掉检测开关只影响是否弹窗，计算本身照常
        app.bounds_check = false;
        assert!(
            app.bounds_warning(&g).is_some(),
            "关掉开关只是不弹窗，检测函数本身仍应能算出超界"
        );
    }

    /// ② 裁剪：只保留原图一半时，输出宽度应减半且笔迹全部落在裁剪区域内。
    #[test]
    fn crop_limits_output_extent() {
        // 左半黑右半白（200×100），不裁剪 → 宽=高（target 50mm、比例 2:1 → 100×50mm）
        let (w, h) = (200u32, 100u32);
        let gray = image::GrayImage::from_fn(w, h, |x, _| {
            image::Luma([if x < w / 2 { 0u8 } else { 255u8 }])
        });
        let base = crate::io::ImageGenOpts {
            target_h_mm: 50.0,
            step_px: 10,
            threshold: 0.5,
            edge_threshold: 0.5,
            invert: false,
            draw_type: 1,
            crop: (0.0, 0.0, 1.0, 1.0),
            scan_angle: 0,
            merge_gap_px: 8,
            order_mode: 0,
            block_px: 40,
            sketch_pressure: 2,
            sketch_jitter: 0.35,
            sketch_pen: 0.5,
            hatch_levels: 6,
            hatch_angle: 35.0,
            ..Default::default()
        };
        let full = crate::io::gray_to_polylines(&gray, &base);
        assert!((full.w_mm - 100.0).abs() < 0.1, "整图宽应=100mm，实际 {}", full.w_mm);

        // 裁掉右半（右=0.5）→ 宽减半
        let cropped = crate::io::gray_to_polylines(
            &gray,
            &crate::io::ImageGenOpts {
                crop: (0.0, 0.0, 0.5, 1.0),
                ..base
            },
        );
        assert!(
            (cropped.w_mm - 50.0).abs() < 0.2,
            "裁剪后宽应≈50mm，实际 {}",
            cropped.w_mm
        );
        let (_x0, _y0, x1, _y1) = wrobot_core::render::bounds_all(&cropped.polylines);
        assert!(x1 <= 50.5, "笔迹应全落在裁剪区内：x1={x1}");
        assert!(!cropped.polylines.is_empty(), "裁剪区里有墨就应有笔迹");
    }

    /// ③ 扫描方向：纵向扫描出来的影线应是「竖线」（y 跨度远大于 x 跨度）。
    #[test]
    fn scan_angle_vertical_draws_columns() {
        let (w, h) = (100u32, 200u32);
        let gray = image::GrayImage::from_pixel(w, h, image::Luma([0u8]));
        let r = crate::io::gray_to_polylines(
            &gray,
            &crate::io::ImageGenOpts {
                target_h_mm: 100.0,
                step_px: 10,
                threshold: 0.5,
                edge_threshold: 0.5,
                invert: false,
                draw_type: 1,
                crop: (0.0, 0.0, 1.0, 1.0),
                scan_angle: 2, // 纵向
                merge_gap_px: 8,
                order_mode: 0,
                block_px: 40,
                sketch_pressure: 2,
                sketch_jitter: 0.35,
                sketch_pen: 0.5,
                hatch_levels: 6,
                hatch_angle: 35.0,
                ..Default::default()
            },
        );
        assert!(!r.polylines.is_empty(), "纵向扫描应有笔迹");
        let (x0, y0, x1, y1) = wrobot_core::render::bounds_all(&r.polylines);
        assert!(
            y1 - y0 > (x1 - x0) * 1.5,
            "纵向扫描的笔画应以竖向为主：y 跨 {} vs x 跨 {}",
            y1 - y0,
            x1 - x0
        );
    }

    /// ④ 连笔容差：容差小 → 跨不过空隙（笔画多）；容差大 → 串成长笔画（笔画少）。
    #[test]
    fn merge_gap_controls_chaining() {
        // 两条窄墨带左右交替出现（行 0/2 只画左带 x∈[35,45)，行 1/3 只画右带 x∈[55,65)），
        // 想串成一条长笔画必须逐行跨过带间空隙。
        let (w, h) = (100u32, 40u32);
        let gray = image::GrayImage::from_fn(w, h, |x, y| {
            let left_band = (y / 10) % 2 == 0;
            let ink = (left_band && (35..45).contains(&x)) || (!left_band && (55..65).contains(&x));
            image::Luma([if ink { 0u8 } else { 255u8 }])
        });
        let mk = |gap: u32| crate::io::ImageGenOpts {
            target_h_mm: 40.0,
            step_px: 10,
            threshold: 0.5,
            edge_threshold: 0.5,
            invert: false,
            draw_type: 2,
            crop: (0.0, 0.0, 1.0, 1.0),
            scan_angle: 0,
            merge_gap_px: gap,
            order_mode: 2, // 按扫描顺序，排除最优路径重排的干扰
            block_px: 40,
            sketch_pressure: 2,
            sketch_jitter: 0.35,
            sketch_pen: 0.5,
            hatch_levels: 6,
            hatch_angle: 35.0,
            ..Default::default()
        };
        let tight = crate::io::gray_to_polylines(&gray, &mk(0));
        let loose = crate::io::gray_to_polylines(&gray, &mk(30));
        // 行距 10px + 空隙 20px → 跨行 22.4px：小容差接不上（4 条），大容差串成 1 条
        assert_eq!(
            tight.polylines.len(),
            4,
            "小容差跨不过空隙：{} 条",
            tight.polylines.len()
        );
        assert_eq!(
            loose.polylines.len(),
            1,
            "大容差应串成一条长笔画：{} 条",
            loose.polylines.len()
        );
    }

    /// ⑤ 分块蛇形：只重排不增减笔画，且顺序按块聚集（相邻块的笔迹位置相邻）。
    #[test]
    fn block_serpentine_keeps_all_strokes() {
        let (w, h) = (120u32, 120u32);
        let gray = image::GrayImage::from_fn(w, h, |x, y| {
            // 三条竖黑条
            let v = if x % 40 < 10 { 0u8 } else { 255u8 };
            let _ = y;
            image::Luma([v])
        });
        let r = crate::io::gray_to_polylines(
            &gray,
            &crate::io::ImageGenOpts {
                target_h_mm: 60.0,
                step_px: 10,
                threshold: 0.5,
                edge_threshold: 0.5,
                invert: false,
                draw_type: 1,
                crop: (0.0, 0.0, 1.0, 1.0),
                scan_angle: 0,
                merge_gap_px: 8,
                order_mode: 3, // 分块蛇形
                block_px: 40,
                sketch_pressure: 2,
                sketch_jitter: 0.35,
                sketch_pen: 0.5,
                hatch_levels: 6,
                hatch_angle: 35.0,
                ..Default::default()
            },
        );
        assert_eq!(r.polylines.len(), 36, "12 行 × 3 条竖带 = 36 段，只重排不该增减");
    }

    fn img_opts(draw_type: usize) -> crate::io::ImageGenOpts {
        crate::io::ImageGenOpts {
            target_h_mm: 60.0,
            step_px: 6,
            threshold: 0.5,
            edge_threshold: 0.5,
            invert: false,
            draw_type,
            crop: (0.0, 0.0, 1.0, 1.0),
            scan_angle: 0,
            merge_gap_px: 8,
            order_mode: 2,
            block_px: 40,
            sketch_pressure: 2,
            sketch_jitter: 0.35,
            sketch_pen: 0.5,
            hatch_levels: 6,
            hatch_angle: 35.0,
            ..Default::default()
        }
    }

    /// 中心线（骨架）：粗圆环应细化成一条近似圆形的单线，
    /// 不能留下整块黑色填充，也不能跑到环外。
    #[test]
    fn line_art_centerline_traces_skeleton() {
        let (w, h) = (120u32, 120u32);
        let gray = image::GrayImage::from_fn(w, h, |x, y| {
            let dx = x as f64 - 60.0;
            let dy = y as f64 - 60.0;
            let d = (dx * dx + dy * dy).sqrt();
            image::Luma([if (30.0..=48.0).contains(&d) { 0u8 } else { 255u8 }])
        });
        let r = crate::io::gray_to_polylines(&gray, &img_opts(3));
        assert!(!r.polylines.is_empty(), "骨架模式应生成笔迹");
        assert!(
            r.polylines.len() <= 6,
            "圆环骨架应是少数几条连续线，实际 {} 条",
            r.polylines.len()
        );
        let mut min_d = f64::INFINITY;
        let mut max_d = 0.0f64;
        for pl in &r.polylines {
            for &(x, y) in &pl.pts {
                let d = ((x - 30.0).powi(2) + (y - 30.0).powi(2)).sqrt();
                min_d = min_d.min(d);
                max_d = max_d.max(d);
            }
        }
        // 环内径 30px=15mm、外径 48px=24mm，骨架应落在两者之间（允许少量毛刺）
        assert!(min_d > 10.0, "骨架不应跑进环心：最近 {min_d}mm");
        assert!(max_d < 28.0, "骨架不应跑到环外：最远 {max_d}mm");
    }

    /// 描边（等值轮廓）：圆环应只画出内外两条闭合轮廓，不填充内部。
    #[test]
    fn line_art_contour_outlines_shape() {
        let (w, h) = (120u32, 120u32);
        let gray = image::GrayImage::from_fn(w, h, |x, y| {
            let dx = x as f64 - 60.0;
            let dy = y as f64 - 60.0;
            let d = (dx * dx + dy * dy).sqrt();
            image::Luma([if (30.0..=48.0).contains(&d) { 0u8 } else { 255u8 }])
        });
        let r = crate::io::gray_to_polylines(&gray, &img_opts(4));
        assert!(!r.polylines.is_empty(), "描边模式应生成笔迹");
        assert!(
            r.polylines.len() <= 4,
            "圆环应只有内外两条轮廓，实际 {} 条",
            r.polylines.len()
        );
        for pl in &r.polylines {
            let first = *pl.pts.first().unwrap();
            let last = *pl.pts.last().unwrap();
            assert!(
                (first.0 - last.0).abs() < 1e-6 && (first.1 - last.1).abs() < 1e-6,
                "轮廓应闭合成环：{:?} → {:?}",
                first,
                last
            );
            for &(x, y) in &pl.pts {
                let d = ((x - 30.0).powi(2) + (y - 30.0).powi(2)).sqrt();
                assert!(
                    (12.0..=26.0).contains(&d),
                    "轮廓应落在圆环边缘（15/24mm）附近，实际 {d}mm"
                );
            }
        }
    }

    /// 交叉排线：暗部在横向影线之外还要有垂直方向的第二层排线。
    #[test]
    fn line_art_crosshatch_adds_second_direction() {
        let (w, h) = (120u32, 120u32);
        // 左黑右白的灰度渐变
        let gray = image::GrayImage::from_fn(w, h, |x, _| {
            image::Luma([(x * 255 / (w - 1)) as u8])
        });
        let base = img_opts(1);
        let sketch = crate::io::gray_to_polylines(&gray, &base);
        let cross = crate::io::gray_to_polylines(
            &gray,
            &crate::io::ImageGenOpts {
                draw_type: 5,
                threshold: 0.7,
                ..base
            },
        );
        assert!(
            cross.polylines.len() > sketch.polylines.len(),
            "暗部应多出垂直排线：交叉 {} 条 vs 素描 {} 条",
            cross.polylines.len(),
            sketch.polylines.len()
        );
        let (mut horizontal, mut vertical) = (0, 0);
        for pl in &cross.polylines {
            if pl.pts.len() < 2 {
                continue;
            }
            let (dx, dy) = (
                (pl.pts[1].0 - pl.pts[0].0).abs(),
                (pl.pts[1].1 - pl.pts[0].1).abs(),
            );
            if dx > dy {
                horizontal += 1;
            } else if dy > dx {
                vertical += 1;
            }
        }
        assert!(horizontal > 0 && vertical > 0, "应同时含横线与竖线");
    }

    /// 网点（半调）：白区不落点，黑区网点密集且每次生成结果一致。
    #[test]
    fn line_art_halftone_dots_follow_darkness() {
        let (w, h) = (120u32, 120u32);
        let gray = image::GrayImage::from_fn(w, h, |x, _| {
            image::Luma([if x < w / 2 { 0u8 } else { 255u8 }])
        });
        let opts = img_opts(6);
        let r = crate::io::gray_to_polylines(&gray, &opts);
        assert!(
            r.polylines.len() > 100,
            "黑半边应生成密集网点，实际 {} 个",
            r.polylines.len()
        );
        for pl in &r.polylines {
            assert!(pl.pts.len() >= 5, "每个网点应是闭合多边形");
            for &(x, _) in &pl.pts {
                assert!(x < 30.5, "白半边不应有网点：x={x}mm");
            }
        }
        let again = crate::io::gray_to_polylines(&gray, &opts);
        assert_eq!(again.polylines.len(), r.polylines.len(), "半调结果应可复现");
        assert_eq!(again.polylines[0].pts, r.polylines[0].pts);
    }

    /// 新增模式不能破坏旧存档：0/1/2 的序号语义保持不变。
    #[test]
    fn line_art_draw_type_indices_are_stable() {
        use crate::state::ImgDraw;
        assert_eq!(ImgDraw::names().len(), 11);
        for i in 0..ImgDraw::names().len() {
            assert_eq!(ImgDraw::from_usize(i).name(), ImgDraw::names()[i]);
        }
        assert_eq!(ImgDraw::from_usize(0), ImgDraw::Wire);
        assert_eq!(ImgDraw::from_usize(1), ImgDraw::Sketch);
        assert_eq!(ImgDraw::from_usize(2), ImgDraw::Continuous);
        assert_eq!(ImgDraw::from_usize(7), ImgDraw::SketchOriginal);
        assert_eq!(ImgDraw::from_usize(8), ImgDraw::SketchHatch);
        assert_eq!(ImgDraw::from_usize(9), ImgDraw::RegionRedraw);
        assert_eq!(ImgDraw::from_usize(10), ImgDraw::StrokeSketch);
        assert_eq!(ImgDraw::from_usize(99), ImgDraw::Sketch);
    }

    // ——— 调子排线通路（色调稿：铅笔素描、照片、成片的暗部） ———

    /// 笔迹在某个矩形里的覆盖率（墨面积 / 矩形面积）。
    ///
    /// 沿折线**逐段细步采样**、只看落在框内的那部分长度。早先是「线段中点落在
    /// 框内就把整段长度算进去」，在长直笔画上会离谱地虚高——一条 40mm 的长线
    /// 只要中点蹭到框边，整段 40mm 都算成框里的墨，实测把纯色块内部的覆盖率
    /// 抬到 0.246，而框内其实只有 3 个点。细步采样量的是「框里到底有多少墨」，
    /// 跟折线怎么抽稀无关。
    fn cover_in_box(
        polys: &[wrobot_core::render::Polyline],
        x0: f64,
        y0: f64,
        x1: f64,
        y1: f64,
        pen_mm: f64,
    ) -> f64 {
        let mut len = 0.0f64;
        for p in polys {
            for s in p.pts.windows(2) {
                let (ax, ay) = s[0];
                let (bx, by) = s[1];
                let dx = bx - ax;
                let dy = by - ay;
                let seg = (dx * dx + dy * dy).sqrt();
                let steps = (seg / 0.35).ceil().max(1.0) as usize;
                let dt = 1.0 / steps as f64;
                for k in 0..steps {
                    let t = (k as f64 + 0.5) * dt;
                    let px = ax + dx * t;
                    let py = ay + dy * t;
                    if px < x0 || px > x1 || py < y0 || py > y1 {
                        continue;
                    }
                    len += seg * dt;
                }
            }
        }
        len * pen_mm / ((x1 - x0) * (y1 - y0)).max(1e-9)
    }

    /// 纯白纸上不该有任何笔迹：素描的亮部就是留白。
    #[test]
    fn sketch_tonal_blank_paper_stays_empty() {
        crate::sketch::force_path(2);
        let gray = image::GrayImage::from_pixel(200, 200, image::Luma([255]));
        let mut o = img_opts(7);
        o.threshold = 0.7;
        let r = crate::io::gray_to_polylines(&gray, &o);
        let cov = cover_in_box(&r.polylines, 0.0, 0.0, r.w_mm, r.h_mm, 0.5);
        assert!(cov < 0.005, "白纸落墨了：覆盖率 {cov:.3}");
    }

    /// 核心语义：**原稿上真有的线要描出来**。
    /// 造一张「一组平行细线」的图——这正是一张铅笔稿最典型的样子，
    /// 提取出的笔迹数量应与原线一致、方向应贴着原线、长度应基本贯通。
    #[test]
    fn sketch_tonal_extracts_parallel_lines() {
        crate::sketch::force_path(2);
        let (w, h) = (240u32, 200u32);
        let rows = [30u32, 50, 70, 90, 110, 130, 150, 170];
        let gray = image::GrayImage::from_fn(w, h, |x, y| {
            let on = rows.contains(&y) && (16..224).contains(&x);
            image::Luma([if on { 96 } else { 244 }])
        });
        let mut o = img_opts(7);
        o.threshold = 0.7;
        o.sketch_pen = 0.5;
        let r = crate::io::gray_to_polylines(&gray, &o);
        let ppm = r.w_mm / w as f64;
        let n = r.polylines.len();
        assert!(n >= 6, "8 条线只认出了 {n} 条");
        // 方向：每条的外接框应明显「横长竖短」
        let mut horiz = 0usize;
        for p in &r.polylines {
            if p.pts.len() < 2 {
                continue;
            }
            let (mut x0, mut x1, mut y0, mut y1) = (1e9f64, -1e9f64, 1e9f64, -1e9f64);
            for &(x, y) in &p.pts {
                x0 = x0.min(x);
                x1 = x1.max(x);
                y0 = y0.min(y);
                y1 = y1.max(y);
            }
            if x1 - x0 > 3.0 * (y1 - y0).max(0.02) {
                horiz += 1;
            }
        }
        assert!(horiz * 2 > n, "多数笔迹应是水平走向，实际 {horiz}/{n}");
        // 长度：横线原长 208px ≈ 104mm 的一半以上要画出来
        let long = r
            .polylines
            .iter()
            .filter(|p| {
                let x0 = p.pts.iter().map(|q| q.0).fold(1e9f64, f64::min);
                let x1 = p.pts.iter().map(|q| q.0).fold(-1e9f64, f64::max);
                (x1 - x0) > 208.0 * ppm * 0.45
            })
            .count();
        assert!(long >= 4, "只有 {long} 条线贯穿近半幅（共 {n} 条）");
        let _ = ppm;
    }

    /// 纯色块里**没有线**，就不该在里面编线：只描出它的边界。
    /// 这条守的是「提取真实笔触」这个前提——一旦退化成"按明暗补排线"，
    /// 块内部就会被填满，整幅画变成另一张图。
    #[test]
    fn sketch_tonal_flat_block_only_gets_its_outline() {
        crate::sketch::force_path(2);
        let (w, h) = (200u32, 200u32);
        let gray = image::GrayImage::from_fn(w, h, |x, y| {
            let d = ((x as f32 - 100.0).powi(2) + (y as f32 - 100.0).powi(2)).sqrt();
            let t = ((d - 48.0) / 12.0).clamp(0.0, 1.0);
            image::Luma([(70.0 + (242.0 - 70.0) * t) as u8])
        });
        let mut o = img_opts(7);
        o.threshold = 0.7;
        o.sketch_pen = 0.5;
        let r = crate::io::gray_to_polylines(&gray, &o);
        let (wmm, hmm) = (r.w_mm, r.h_mm);
        let (cxm, cym) = (wmm * 0.5, hmm * 0.5);
        // 圆内核心：本来就没有线，应当几乎空白
        let inner = cover_in_box(
            &r.polylines,
            cxm - wmm * 0.10,
            cym - hmm * 0.10,
            cxm + wmm * 0.10,
            cym + hmm * 0.10,
            0.5,
        );
        // 边界一圈：应当有笔
        let ring = cover_in_box(
            &r.polylines,
            cxm - wmm * 0.28,
            cym - hmm * 0.28,
            cxm + wmm * 0.28,
            cym + hmm * 0.28,
            0.5,
        );
        let corner = cover_in_box(
            &r.polylines,
            wmm * 0.96,
            hmm * 0.02,
            wmm * 0.99,
            hmm * 0.2,
            0.5,
        );
        if inner >= 0.10 {
            let ppm2 = 200.0 / wmm; // px per mm
            eprintln!("DBG 共 {} 条，wmm={wmm:.1} hmm={hmm:.1}", r.polylines.len());
            for (k, p) in r.polylines.iter().enumerate() {
                let (mut ax, mut bx, mut ay, mut by) = (1e9f64, -1e9f64, 1e9f64, -1e9f64);
                let mut ln = 0.0f64;
                for s in p.pts.windows(2) {
                    ln += ((s[1].0 - s[0].0).powi(2) + (s[1].1 - s[0].1).powi(2)).sqrt();
                }
                for q in &p.pts {
                    ax = ax.min(q.0);
                    bx = bx.max(q.0);
                    ay = ay.min(q.1);
                    by = by.max(q.1);
                }
                let f = |v: f64, c: f64| (v - c) * ppm2;
                let ib = cover_in_box(
                    std::slice::from_ref(p),
                    cxm - wmm * 0.10,
                    cym - hmm * 0.10,
                    cxm + wmm * 0.10,
                    cym + hmm * 0.10,
                    0.5,
                );
                eprintln!(
                    "DBG #{k} n={} len={:.1}px box x[{:.0},{:.0}] y[{:.0},{:.0}] inner={ib:.4}",
                    p.pts.len(),
                    ln * ppm2,
                    f(ax, cxm),
                    f(bx, cxm),
                    f(ay, cym),
                    f(by, cym),
                );
            }
        }
        assert!(inner < 0.10, "纯色块内部被编了线：{inner:.3}");
        assert!(ring > 0.02, "块边界没描出来：{ring:.3}");
        assert!(corner < 0.05, "纸白处不该落墨：{corner:.3}");
    }

    /// 识别与笔宽无关：换笔宽只改「纸上怎么排」，不该改变「认出几根线」。
    #[test]
    fn sketch_tonal_ink_is_pen_independent() {
        crate::sketch::force_path(2);
        let (w, h) = (200u32, 160u32);
        let gray = image::GrayImage::from_fn(w, h, |x, y| {
            let on = y % 16 == 8 && (10..190).contains(&x);
            image::Luma([if on { 110 } else { 242 }])
        });
        let n_for = |pen: f64| {
            let mut o = img_opts(7);
            o.threshold = 0.7;
            o.sketch_pen = pen;
            crate::io::gray_to_polylines(&gray, &o).polylines.len()
        };
        let a = n_for(0.3);
        let b = n_for(0.6);
        assert!(a >= 6 && b >= 6, "细笔 {a} 条、粗笔 {b} 条，都太少");
        let k = a as f64 / b as f64;
        assert!(
            (0.6..1.67).contains(&k),
            "笔宽不该改变识别到的线条数：0.3mm → {a} 条，0.6mm → {b} 条"
        );
    }

    /// 素描原稿：沿原稿线条描线而非铺影线；暗部叠描更多，空白处保持空白。
    #[test]
    fn sketch_original_follows_strokes_and_pressure() {
        // 一张 120×120 的「铅笔稿」：一条深色斜线 + 一条浅灰横线。
        let (w, h) = (120u32, 120u32);
        let gray = image::GrayImage::from_fn(w, h, |x, y| {
            let dark = (x as i32 - y as i32).abs() <= 1;
            let light = y == 90 && (20..100).contains(&x);
            let v = if dark {
                20u8
            } else if light {
                150u8
            } else {
                255u8
            };
            image::Luma([v])
        });

        let mut opts = img_opts(7);
        opts.threshold = 0.75; // 深线和浅线都算墨
        opts.sketch_jitter = 0.0;
        opts.sketch_pressure = 1;
        opts.order_mode = 2; // 保持扫描顺序，便于比较遍数
        let one = crate::io::gray_to_polylines(&gray, &opts);
        assert!(!one.polylines.is_empty(), "原稿有线条就应描出笔迹");

        opts.sketch_pressure = 3;
        let many = crate::io::gray_to_polylines(&gray, &opts);
        assert!(
            many.polylines.len() > one.polylines.len(),
            "叠描遍数增加后暗部应多描几遍：{} → {}",
            one.polylines.len(),
            many.polylines.len()
        );

        // 白纸区域不能凭空生成线条（高光天然留白）。
        // 允许 1px 的光晕：笔有宽度，墨带边缘那一遍的线心本就压在墨迹边界上。
        for pl in &many.polylines {
            for &(x, y) in &pl.pts {
                let px = (((x / many.w_mm) * w as f64) as i32).clamp(0, w as i32 - 1);
                let py = (((y / many.h_mm) * h as f64) as i32).clamp(0, h as i32 - 1);
                let mut near_ink = false;
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        let (qx, qy) = (
                            (px + dx).clamp(0, w as i32 - 1) as u32,
                            (py + dy).clamp(0, h as i32 - 1) as u32,
                        );
                        if gray.get_pixel(qx, qy).0[0] < 200 {
                            near_ink = true;
                        }
                    }
                }
                assert!(near_ink, "白纸区域不应落笔：({px},{py})");
            }
        }

        // 纯白图：没有任何线条。
        let blank = image::GrayImage::from_pixel(40, 40, image::Luma([255u8]));
        opts.sketch_pressure = 1;
        let r = crate::io::gray_to_polylines(&blank, &opts);
        assert!(r.polylines.is_empty(), "空白稿不应产生笔迹");
    }

    /// 逐笔识别：十字交叉处两条笔迹都要保持连续，不能在结点处断成四截。
    #[test]
    fn sketch_original_keeps_strokes_whole_through_crossing() {
        let (w, h) = (160u32, 160u32);
        let gray = image::GrayImage::from_fn(w, h, |x, y| {
            let horiz = (79..=81).contains(&y) && (18..=141).contains(&x);
            let vert = (79..=81).contains(&x) && (18..=141).contains(&y);
            image::Luma([if horiz || vert { 20u8 } else { 255u8 }])
        });
        let mut opts = img_opts(7);
        opts.threshold = 0.6;
        opts.sketch_jitter = 0.0;
        opts.sketch_pressure = 1;
        opts.order_mode = 2;
        let r = crate::io::gray_to_polylines(&gray, &opts);
        let sc = r.h_mm / h as f64;
        let px = |pl: &wrobot_core::Polyline| -> Vec<(f64, f64)> {
            pl.pts.iter().map(|&(x, y)| (x / sc, y / sc)).collect()
        };
        let (mut horiz_ok, mut vert_ok) = (false, false);
        for pl in &r.polylines {
            let pts = px(pl);
            let xmin = pts.iter().map(|p| p.0).fold(f64::MAX, f64::min);
            let xmax = pts.iter().map(|p| p.0).fold(f64::MIN, f64::max);
            let ymin = pts.iter().map(|p| p.1).fold(f64::MAX, f64::min);
            let ymax = pts.iter().map(|p| p.1).fold(f64::MIN, f64::max);
            if xmin < 30.0 && xmax > 130.0 && pts.iter().all(|p| (p.1 - 80.0).abs() <= 3.0) {
                horiz_ok = true;
            }
            if ymin < 30.0 && ymax > 130.0 && pts.iter().all(|p| (p.0 - 80.0).abs() <= 3.0) {
                vert_ok = true;
            }
        }
        assert!(horiz_ok, "横向线应在交叉点处续笔成一条：{:?}", r.polylines.len());
        assert!(vert_ok, "纵向线应在交叉点处续笔成一条：{:?}", r.polylines.len());
    }

    /// 断笔接续：同一方向、只差几个像素的小断口应接成一条笔迹。
    #[test]
    fn sketch_original_bridges_small_break() {
        let (w, h) = (300u32, 300u32);
        let gray = image::GrayImage::from_fn(w, h, |x, y| {
            let band = (148..=152).contains(&y);
            let on = band && ((40..=147).contains(&x) || (152..=250).contains(&x));
            let faint = band && (148..=151).contains(&x);
            let v = if on {
                20u8
            } else if faint {
                215u8
            } else {
                255u8
            };
            image::Luma([v])
        });
        let mut opts = img_opts(7);
        opts.threshold = 0.6;
        opts.sketch_jitter = 0.0;
        opts.sketch_pressure = 1;
        opts.order_mode = 2;
        let r = crate::io::gray_to_polylines(&gray, &opts);
        let sc = r.h_mm / h as f64;
        let mut bridged = false;
        for pl in &r.polylines {
            let xs: Vec<f64> = pl.pts.iter().map(|p| p.0 / sc).collect();
            let span = xs.iter().fold(f64::MIN, |a, &b| a.max(b))
                - xs.iter().fold(f64::MAX, |a, &b| a.min(b));
            if span > 180.0 {
                bridged = true;
            }
        }
        assert!(bridged, "4px 小断口应接成一条长笔迹");
    }

    /// 1px 针孔（浅灰残留，不是白纸）应在细化前补上，避免骨架天生就是断的。
    #[test]
    fn sketch_original_seals_one_pixel_hole() {
        let (w, h) = (280u32, 80u32);
        let gray = image::GrayImage::from_fn(w, h, |x, y| {
            let band = (38..=40).contains(&y) && (20..=260).contains(&x);
            let hole = band && x == 140;
            let v = if hole {
                225u8
            } else if band {
                25u8
            } else {
                255u8
            };
            image::Luma([v])
        });
        let mut opts = img_opts(7);
        opts.threshold = 0.72;
        opts.sketch_jitter = 0.0;
        opts.sketch_pressure = 1;
        opts.order_mode = 2;
        let r = crate::io::gray_to_polylines(&gray, &opts);
        let sc = r.h_mm / h as f64;
        let span = |pl: &wrobot_core::Polyline| {
            let xs: Vec<f64> = pl.pts.iter().map(|p| p.0 / sc).collect();
            xs.iter().fold(f64::MIN, |a, &b| a.max(b))
                - xs.iter().fold(f64::MAX, |a, &b| a.min(b))
        };
        assert!(
            r.polylines.iter().any(|p| span(p) > 200.0),
            "1px 浅灰针孔不应把一根线切成两段"
        );
    }

    /// 网格交叉处的笔迹必须各走各的：横向线在交叉点穿过竖向线继续走，
    /// 不能拐进相邻的竖线里，把排线织成「人字纹」。
    #[test]
    fn sketch_original_grid_crossings_do_not_zigzag() {
        let (w, h) = (140u32, 140u32);
        let gray = image::GrayImage::from_fn(w, h, |x, y| {
            let vert = (0..4).any(|k| (20 + k * 30..=21 + k * 30).contains(&x));
            let horiz = (0..4).any(|k| (20 + k * 30..=21 + k * 30).contains(&y));
            let ink = (vert && (10..=130).contains(&y)) || (horiz && (10..=130).contains(&x));
            image::Luma([if ink { 30u8 } else { 255u8 }])
        });
        let mut opts = img_opts(7);
        opts.threshold = 0.7;
        opts.sketch_jitter = 0.0;
        opts.sketch_pressure = 1;
        opts.order_mode = 2;
        let r = crate::io::gray_to_polylines(&gray, &opts);
        let sc = r.h_mm / h as f64;
        let mut plain = 0;
        for pl in &r.polylines {
            let pts: Vec<(f64, f64)> = pl.pts.iter().map(|&(x, y)| (x / sc, y / sc)).collect();
            let (xmin, xmax) = pts.iter().fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p.0), b.max(p.0)));
            let (ymin, ymax) = pts.iter().fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p.1), b.max(p.1)));
            let (dw, dh) = (xmax - xmin, ymax - ymin);
            // 横线/竖线：一个方向很长，另一个方向只有一个线宽
            if (dw > 80.0 && dh < 6.0) || (dh > 80.0 && dw < 6.0) {
                plain += 1;
            }
        }
        assert!(plain >= 7, "8 根线里应几乎都是直通线，实际只有 {plain} 根（其余被织成折线）");
    }

    /// 还原出来的线必须落在原稿那条墨上：这是「拟真」的底线，
    /// 位置飘了的线再漂亮也不是原稿。
    #[test]
    fn sketch_original_lands_on_the_ink() {
        let (w, h) = (200u32, 120u32);
        // 一条 3px 粗的横线 + 一条 3px 粗的竖线，位置都已知
        let gray = image::GrayImage::from_fn(w, h, |x, y| {
            let horiz = (78..=80).contains(&y) && (30..=170).contains(&x);
            let vert = (138..=140).contains(&x) && (25..=95).contains(&y);
            image::Luma([if horiz || vert { 30u8 } else { 255u8 }])
        });
        let mut opts = img_opts(7);
        opts.threshold = 0.7;
        opts.sketch_jitter = 0.0;
        opts.sketch_pressure = 1;
        opts.order_mode = 2;
        let r = crate::io::gray_to_polylines(&gray, &opts);
        let sc = r.h_mm / h as f64;
        let mut best_h = f64::MAX;
        let mut best_v = f64::MAX;
        for pl in &r.polylines {
            let pts: Vec<(f64, f64)> = pl.pts.iter().map(|&(x, y)| (x / sc, y / sc)).collect();
            let (xmin, xmax) = pts
                .iter()
                .fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p.0), b.max(p.0)));
            let (ymin, ymax) = pts
                .iter()
                .fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p.1), b.max(p.1)));
            if xmax - xmin > 80.0 {
                for p in &pts {
                    best_h = best_h.min((p.1 - 79.0).abs());
                }
            }
            if ymax - ymin > 40.0 {
                for p in &pts {
                    best_v = best_v.min((p.0 - 139.0).abs());
                }
            }
        }
        assert!(best_h <= 1.5, "横线偏离原墨 {best_h:.2}px");
        assert!(best_v <= 1.5, "竖线偏离原墨 {best_v:.2}px");
    }

    /// 真正的白纸断口不能接：留白是高光，接上就成了凭空多出的线。
    #[test]
    fn sketch_original_keeps_paper_break() {
        let (w, h) = (280u32, 80u32);
        let gray = image::GrayImage::from_fn(w, h, |x, y| {
            let band = (38..=40).contains(&y);
            let ink = band && ((20..=138).contains(&x) || (142..=260).contains(&x));
            image::Luma([if ink { 25u8 } else { 255u8 }])
        });
        let mut opts = img_opts(7);
        opts.threshold = 0.72;
        opts.sketch_jitter = 0.0;
        opts.sketch_pressure = 1;
        opts.order_mode = 2;
        let r = crate::io::gray_to_polylines(&gray, &opts);
        let sc = r.h_mm / h as f64;
        let span = |pl: &wrobot_core::Polyline| {
            let xs: Vec<f64> = pl.pts.iter().map(|p| p.0 / sc).collect();
            xs.iter().fold(f64::MIN, |a, &b| a.max(b))
                - xs.iter().fold(f64::MAX, |a, &b| a.min(b))
        };
        assert!(
            r.polylines.iter().all(|p| span(p) < 200.0),
            "3px 纯白断口是留白，不应被接成长线"
        );
    }

    /// 分层描绘：浅色起稿线先画、暗部后画；暗部重描更多。
    #[test]
    fn sketch_original_draws_light_layer_first() {
        let (w, h) = (300u32, 140u32);
        let gray = image::GrayImage::from_fn(w, h, |x, y| {
            let light = (25..=27).contains(&y) && (20..=280).contains(&x);
            let dark = (105..=107).contains(&y) && (20..=280).contains(&x);
            let v = if dark {
                30u8
            } else if light {
                170u8
            } else {
                255u8
            };
            image::Luma([v])
        });
        let mut opts = img_opts(7);
        opts.threshold = 0.8;
        opts.sketch_jitter = 0.2;
        opts.sketch_pressure = 3;
        opts.order_mode = 0;
        let r = crate::io::gray_to_polylines(&gray, &opts);
        assert!(!r.polylines.is_empty());
        let sc = r.h_mm / h as f64;
        let mean_y = |pl: &wrobot_core::Polyline| -> f64 {
            pl.pts.iter().map(|p| p.1 / sc).sum::<f64>() / pl.pts.len() as f64
        };
        assert!(
            mean_y(&r.polylines[0]) < 70.0,
            "浅色起稿线应先画，实际第一条在 y={:.1}px",
            mean_y(&r.polylines[0])
        );
        let light_n = r.polylines.iter().filter(|p| mean_y(p) < 70.0).count();
        let dark_n = r.polylines.iter().filter(|p| mean_y(p) >= 70.0).count();
        assert!(dark_n >= 2, "暗部应有重描：{dark_n} 条");
        assert!(dark_n > light_n, "暗部重描应多于浅色：{dark_n} vs {light_n}");
    }

    // ——— 素描调子：灰度分层排线 ———

    /// 三段等宽色块（白 / 中灰 / 深灰）：排线密度必须随调子递增，白纸留白。
    #[test]
    fn sketch_hatch_density_follows_tone() {
        let (w, h) = (240u32, 160u32);
        let gray = image::GrayImage::from_fn(w, h, |x, _| {
            image::Luma([if x < 80 {
                255
            } else if x < 160 {
                170
            } else {
                55
            }])
        });
        let mut o = img_opts(8);
        o.hatch_levels = 6;
        o.hatch_angle = 35.0;
        o.step_px = 6;
        o.threshold = 0.5;
        o.edge_threshold = 0.9; // 少提轮廓，避免块边界干扰内部统计
        o.sketch_jitter = 0.0;
        o.sketch_pen = 0.5;
        o.order_mode = 2;
        let r = crate::io::gray_to_polylines(&gray, &o);
        assert!(!r.polylines.is_empty(), "调子排线应生成笔迹");
        let (wmm, hmm) = (r.w_mm, r.h_mm);
        let cov = |x0: f64, x1: f64| {
            cover_in_box(
                &r.polylines,
                x0 * wmm,
                0.2 * hmm,
                x1 * wmm,
                0.8 * hmm,
                0.5,
            )
        };
        let white = cov(0.03, 0.30);
        let mid = cov(0.36, 0.63);
        let dark = cov(0.70, 0.97);
        assert!(white < 0.008, "亮部应留白，实际覆盖率 {white:.4}");
        assert!(mid > 0.015, "中间调应有一层排线，实际 {mid:.4}");
        assert!(
            dark > mid * 1.25,
            "暗部排线应明显更密：暗 {dark:.4} vs 中 {mid:.4}"
        );
    }

    /// 暗部叠的是交叉排线：同一块暗部里应出现多个明显不同的走向。
    #[test]
    fn sketch_hatch_dark_area_crosses_directions() {
        let (w, h) = (220u32, 180u32);
        // 中间一块深色，四周留白（留白让调子归一化有纸白基准）
        let gray = image::GrayImage::from_fn(w, h, |x, y| {
            let inside = (30..190).contains(&x) && (25..155).contains(&y);
            image::Luma([if inside { 55u8 } else { 250u8 }])
        });
        let mut o = img_opts(8);
        o.hatch_levels = 6;
        o.hatch_angle = 20.0;
        o.step_px = 6;
        o.threshold = 0.5;
        o.edge_threshold = 0.9;
        o.sketch_jitter = 0.0;
        o.sketch_pen = 0.5;
        o.order_mode = 2;
        let r = crate::io::gray_to_polylines(&gray, &o);
        let (wmm, hmm) = (r.w_mm, r.h_mm);
        let (x0, x1, y0, y1) = (0.3 * wmm, 0.7 * wmm, 0.3 * hmm, 0.7 * hmm);
        let mut bins = [0.0f64; 6];
        let mut total = 0.0f64;
        for pl in &r.polylines {
            for s in pl.pts.windows(2) {
                let (ax, ay) = s[0];
                let (bx, by) = s[1];
                let mx = (ax + bx) * 0.5;
                let my = (ay + by) * 0.5;
                if mx < x0 || mx > x1 || my < y0 || my > y1 {
                    continue;
                }
                let len = ((bx - ax).powi(2) + (by - ay).powi(2)).sqrt();
                if len < 1e-6 {
                    continue;
                }
                let mut th = (by - ay).atan2(bx - ax);
                if th < 0.0 {
                    th += std::f64::consts::PI;
                }
                if th >= std::f64::consts::PI {
                    th -= std::f64::consts::PI;
                }
                let b = (th / (std::f64::consts::PI / 6.0)).floor() as usize;
                bins[b.min(5)] += len;
                total += len;
            }
        }
        assert!(total > 10.0, "暗部内部没有排线：{total:.1}mm");
        let used = bins.iter().filter(|&&v| v > total * 0.08).count();
        assert!(
            used >= 2,
            "暗部应叠出两组交叉排线，实际 {used} 个（{:?}）",
            bins.map(|v| (v / total * 100.0).round())
        );
    }

    /// 多笔：亮部只由最浅的笔出线；暗部一定轮到最深的笔；笔迹标签按笔序单调递进。
    #[test]
    fn sketch_hatch_multi_pen_allocates_tone_bands() {
        let (w, h) = (240u32, 160u32);
        // 白 / 中灰 / 深灰 三段：浅笔管中间调，深笔管暗部，白纸留白。
        let gray = image::GrayImage::from_fn(w, h, |x, _| {
            image::Luma([if x < 80 {
                255
            } else if x < 160 {
                150
            } else {
                35
            }])
        });
        let mut o = img_opts(8);
        o.multi_pen = true;
        o.pen_count = 4;
        o.pens = [
            crate::sketch::HatchPen { width_mm: 0.3, darkness: 0.45 },
            crate::sketch::HatchPen { width_mm: 0.5, darkness: 0.65 },
            crate::sketch::HatchPen { width_mm: 0.8, darkness: 0.82 },
            crate::sketch::HatchPen { width_mm: 1.0, darkness: 0.95 },
        ];
        o.hatch_levels = 8;
        o.hatch_angle = 35.0;
        o.step_px = 6;
        o.threshold = 0.5;
        o.edge_threshold = 0.9;
        o.sketch_jitter = 0.0;
        o.order_mode = 2;
        let r = crate::io::gray_to_polylines(&gray, &o);
        assert_eq!(r.pens.len(), r.polylines.len(), "每根笔迹都要有笔标签");
        assert!(!r.polylines.is_empty(), "多笔排线应生成笔迹");

        let (wmm, _hmm) = (r.w_mm, r.h_mm);
        let pens_in_box = |x0: f64, x1: f64| {
            let mut v = std::collections::BTreeSet::new();
            for (pl, &pen) in r.polylines.iter().zip(r.pens.iter()) {
                if pl.pts.iter().any(|p| p.0 >= x0 * wmm && p.0 <= x1 * wmm) {
                    v.insert(pen);
                }
            }
            v
        };
        let light = pens_in_box(0.03, 0.30);
        let mid = pens_in_box(0.36, 0.63);
        let dark = pens_in_box(0.70, 0.97);
        assert!(light.is_empty(), "亮部应留白，实际出现笔 {light:?}");
        assert!(mid.contains(&0), "中间调应由最浅的笔起稿：{mid:?}");
        assert!(
            dark.contains(&3),
            "最暗的区域必须轮到最深那支笔：{dark:?}"
        );
        assert!(
            light.union(&mid).all(|&p| p <= 2),
            "最浅的带不应把最深笔浪费在亮部"
        );
        let sorted = r.pens.windows(2).all(|s| s[0] <= s[1]);
        assert!(sorted, "笔迹应按 浅 → 深 输出：{:?}", r.pens);
    }

    /// 关掉多笔时，无论笔组里放了几支笔，输出都只属于第 0 支。
    #[test]
    fn sketch_hatch_single_pen_ignores_pen_set() {
        let (w, h) = (160u32, 120u32);
        let gray = image::GrayImage::from_fn(w, h, |x, _| image::Luma([if x < 80 { 255 } else { 40 }]));
        let mut o = img_opts(8);
        o.pen_count = 4;
        o.pens = [
            crate::sketch::HatchPen { width_mm: 0.3, darkness: 0.45 },
            crate::sketch::HatchPen { width_mm: 0.5, darkness: 0.65 },
            crate::sketch::HatchPen { width_mm: 0.8, darkness: 0.82 },
            crate::sketch::HatchPen { width_mm: 1.0, darkness: 0.95 },
        ];
        o.multi_pen = false;
        o.edge_threshold = 0.9;
        o.sketch_jitter = 0.0;
        let r = crate::io::gray_to_polylines(&gray, &o);
        assert!(!r.polylines.is_empty());
        assert!(r.pens.iter().all(|&p| p == 0), "单笔模式不应出现其它笔标签");
    }

    /// 多笔最要紧的是「线来自原稿」：一张由深浅不同、彼此分离的横线组成的稿子，
    /// 重建出的采样点大部分必须落在原稿的墨上，而不是在纸白处凭空拟合。
    #[test]
    fn sketch_hatch_multi_pen_traces_source_ink() {
        let (w, h) = (360u32, 300u32);
        // 48 条 200px 长的横线，行距 6px、线宽 2px，明暗从浅灰一路到深灰。
        let gray = image::GrayImage::from_fn(w, h, |x, y| {
            if !(20..340).contains(&x) {
                return image::Luma([245u8]);
            }
            let row = y / 6;
            let on = y % 6 < 2 && row < 48;
            let tone = 220u8.saturating_sub((row / 4) as u8 * 10);
            image::Luma([if on { tone } else { 245 }])
        });
        let mut o = img_opts(8);
        o.multi_pen = true;
        o.pen_count = 4;
        o.pens = [
            crate::sketch::HatchPen { width_mm: 0.3, darkness: 0.45 },
            crate::sketch::HatchPen { width_mm: 0.5, darkness: 0.65 },
            crate::sketch::HatchPen { width_mm: 0.8, darkness: 0.82 },
            crate::sketch::HatchPen { width_mm: 1.0, darkness: 0.95 },
        ];
        o.threshold = 0.5;
        o.edge_threshold = 0.9;
        o.sketch_jitter = 0.0;
        o.order_mode = 2;
        let r = crate::io::gray_to_polylines(&gray, &o);
        assert!(!r.polylines.is_empty(), "多笔应生成笔迹");
        assert_eq!(r.pens.len(), r.polylines.len());

        // on-ink：沿折线细步采样，每个采样点在原稿 3×3 邻域里找得到墨
        // （直线被抽稀成两三个拐点，只看顶点会严重低估采样量）。
        let scale = r.w_mm / w as f64;
        let mut hit = 0usize;
        let mut tot = 0usize;
        for pl in &r.polylines {
            for s in pl.pts.windows(2) {
                let (ax, ay) = s[0];
                let (bx, by) = s[1];
                let seg = ((bx - ax).powi(2) + (by - ay).powi(2)).sqrt();
                let steps = (seg / 0.35).ceil().max(1.0) as usize;
                for k in 0..steps {
                    let t = (k as f64 + 0.5) / steps as f64;
                    let px = ((ax + (bx - ax) * t) / scale).round() as i32;
                    let py = ((ay + (by - ay) * t) / scale).round() as i32;
                    if !(1..w as i32 - 1).contains(&px) || !(1..h as i32 - 1).contains(&py) {
                        continue;
                    }
                    let mut ink = false;
                    'find: for dy in -1..=1 {
                        for dx in -1..=1 {
                            if gray.get_pixel((px + dx) as u32, (py + dy) as u32).0[0] < 200 {
                                ink = true;
                                break 'find;
                            }
                        }
                    }
                    tot += 1;
                    hit += ink as usize;
                }
            }
        }
        assert!(tot > 200, "采样点太少：{tot}");
        let ratio = hit as f64 / tot as f64;
        assert!(
            ratio > 0.75,
            "多笔笔迹应贴在原稿墨上，实际 on-ink {:.2}（{hit}/{tot}）",
            ratio
        );

        let mut used = [0usize; 4];
        for &p in &r.pens {
            used[(p as usize).min(3)] += 1;
        }
        assert!(
            used.iter().filter(|&&n| n > 0).count() >= 3,
            "深浅不同的原稿线应分给至少三支笔：{used:?}"
        );
        let sorted = r.pens.windows(2).all(|s| s[0] <= s[1]);
        assert!(sorted, "笔迹应按 浅 → 深 输出：{:?}", r.pens);
    }

    /// 分区重绘：覆盖率必须跟着深度层走——亮部留白、暗部叠得最密，
    /// 并且每个深度层绑定不同的笔，输出按 浅 → 深 排列。
    #[test]
    fn region_redraw_density_follows_depth_layers() {
        let (w, h) = (240u32, 160u32);
        // 白 / 中灰 / 近黑 三段
        let gray = image::GrayImage::from_fn(w, h, |x, _| {
            image::Luma([if x < 80 {
                255
            } else if x < 160 {
                150
            } else {
                12
            }])
        });
        let mut o = img_opts(9);
        o.multi_pen = true;
        o.pen_count = 4;
        o.pens = [
            crate::sketch::HatchPen { width_mm: 0.3, darkness: 0.45 },
            crate::sketch::HatchPen { width_mm: 0.5, darkness: 0.65 },
            crate::sketch::HatchPen { width_mm: 0.8, darkness: 0.82 },
            crate::sketch::HatchPen { width_mm: 1.0, darkness: 0.95 },
        ];
        o.hatch_levels = 6;
        o.region_block_mm = 3.0;
        o.hatch_angle = 35.0;
        o.threshold = 0.5;
        o.edge_threshold = 0.9;
        o.sketch_jitter = 0.0;
        o.order_mode = 2;
        let r = crate::io::gray_to_polylines(&gray, &o);
        assert_eq!(r.pens.len(), r.polylines.len());
        assert!(!r.polylines.is_empty(), "分区重绘应生成笔迹");
        let (wmm, hmm) = (r.w_mm, r.h_mm);
        let cov = |x0: f64, x1: f64| {
            cover_in_box(
                &r.polylines,
                x0 * wmm,
                0.2 * hmm,
                x1 * wmm,
                0.8 * hmm,
                0.5,
            )
        };
        let white = cov(0.03, 0.30);
        let mid = cov(0.36, 0.63);
        let dark = cov(0.70, 0.97);
        assert!(white < 0.01, "亮部应留白，实际覆盖率 {white:.4}");
        assert!(mid > 0.05, "中间调应有排线，实际 {mid:.4}");
        assert!(
            dark > mid * 1.15,
            "暗部多层叠加应明显更密：暗 {dark:.4} vs 中 {mid:.4}"
        );

        let pens_in_box = |x0: f64, x1: f64| {
            let mut v = std::collections::BTreeSet::new();
            for (pl, &pen) in r.polylines.iter().zip(r.pens.iter()) {
                if pl.pts.iter().any(|p| p.0 >= x0 * wmm && p.0 <= x1 * wmm) {
                    v.insert(pen);
                }
            }
            v
        };
        assert!(pens_in_box(0.03, 0.30).is_empty(), "亮部不应落笔");
        let mid_pens = pens_in_box(0.36, 0.63);
        let dark_pens = pens_in_box(0.70, 0.97);
        assert!(mid_pens.contains(&0), "中间调应由最浅的笔起稿：{mid_pens:?}");
        assert!(
            dark_pens.contains(&3),
            "最暗处必须轮到最深那支笔：{dark_pens:?}"
        );
        let sorted = r.pens.windows(2).all(|s| s[0] <= s[1]);
        assert!(sorted, "分区重绘应按 浅 → 深 逐层输出：{:?}", r.pens);
    }

    /// 分区重绘按原稿走向排线：一片 135°（x+y 恒定）斜纹，笔迹也应主要沿
    /// 同一走向，而不是不管原稿一律按扫描方向横排。
    #[test]
    fn region_redraw_follows_source_stroke_direction() {
        let (w, h) = (200u32, 200u32);
        let gray = image::GrayImage::from_fn(w, h, |x, y| {
            let on = (x + y) % 8 < 3;
            image::Luma([if on { 55u8 } else { 250u8 }])
        });
        let mut o = img_opts(9);
        o.hatch_levels = 5;
        o.region_block_mm = 2.0;
        o.hatch_angle = 0.0;
        o.threshold = 0.5;
        o.edge_threshold = 0.9;
        o.sketch_jitter = 0.0;
        o.order_mode = 2;
        let r = crate::io::gray_to_polylines(&gray, &o);
        assert!(!r.polylines.is_empty());
        let (wmm, hmm) = (r.w_mm, r.h_mm);
        let (x0, x1, y0, y1) = (0.2 * wmm, 0.8 * wmm, 0.2 * hmm, 0.8 * hmm);
        let mut along = 0.0f64;
        let mut total = 0.0f64;
        for pl in &r.polylines {
            for s in pl.pts.windows(2) {
                let (ax, ay) = s[0];
                let (bx, by) = s[1];
                let mx = (ax + bx) * 0.5;
                let my = (ay + by) * 0.5;
                if mx < x0 || mx > x1 || my < y0 || my > y1 {
                    continue;
                }
                let len = ((bx - ax).powi(2) + (by - ay).powi(2)).sqrt();
                if len < 1e-6 {
                    continue;
                }
                let mut th = (by - ay).atan2(bx - ax).to_degrees();
                if th < 0.0 {
                    th += 180.0;
                }
                total += len;
                if (th - 135.0).abs() <= 25.0 {
                    along += len;
                }
            }
        }
        assert!(total > 20.0, "内部没有排线：{total:.1}mm");
        let frac = along / total;
        assert!(
            frac > 0.45,
            "排线应主要顺着原稿斜纹（135°），实际只有 {:.0}% 沿纹（共 {total:.0}mm）",
            frac * 100.0
        );
    }

    /// 素描笔触：密度必须由调子决定——亮部留白、中间调稀、暗部密，
    /// 而不是全图统一线距。
    #[test]
    fn stroke_sketch_density_follows_tone() {
        let (w, h) = (240u32, 160u32);
        let gray = image::GrayImage::from_fn(w, h, |x, _| {
            image::Luma([if x < 80 {
                255
            } else if x < 160 {
                150
            } else {
                20
            }])
        });
        let mut o = img_opts(10);
        o.stroke_len_mm = 3.0;
        o.stroke_follow = 2;
        o.hatch_levels = 5;
        o.threshold = 0.5;
        o.sketch_jitter = 0.0;
        o.sketch_pen = 0.5;
        o.order_mode = 2;
        let r = crate::io::gray_to_polylines(&gray, &o);
        assert!(!r.polylines.is_empty(), "素描笔触应生成笔迹");
        let (wmm, hmm) = (r.w_mm, r.h_mm);
        let cov = |x0: f64, x1: f64| {
            cover_in_box(
                &r.polylines,
                x0 * wmm,
                0.2 * hmm,
                x1 * wmm,
                0.8 * hmm,
                0.5,
            )
        };
        let white = cov(0.03, 0.30);
        let mid = cov(0.36, 0.63);
        let dark = cov(0.70, 0.97);
        assert!(white < 0.004, "亮部应留白，实际覆盖率 {white:.4}");
        assert!(mid > 0.02, "中间调应有稀疏笔触，实际 {mid:.4}");
        assert!(
            dark > mid * 1.2,
            "暗部必须明显更密：暗 {dark:.4} vs 中 {mid:.4}"
        );
    }

    /// 素描笔触除结构线外应是 1~5mm 的短直线：不能退化成两种坏形状——
    /// 一根贯穿画面的长排线，或者抖动过度的小曲线。
    #[test]
    fn stroke_sketch_uses_short_straight_segments() {
        let (w, h) = (200u32, 160u32);
        // 大片纸白上放一块中灰：让「纸白」按面积胜出，中灰才有调子。
        let gray = image::GrayImage::from_fn(w, h, |x, y| {
            let on = (60..180).contains(&x) && (20..140).contains(&y);
            image::Luma([if on { 120u8 } else { 252u8 }])
        });
        let mut o = img_opts(10);
        o.stroke_len_mm = 3.0;
        o.stroke_follow = 2;
        o.hatch_levels = 4;
        o.threshold = 0.5;
        o.sketch_jitter = 0.15;
        o.sketch_pen = 0.5;
        o.order_mode = 1;
        let r = crate::io::gray_to_polylines(&gray, &o);
        assert!(!r.polylines.is_empty());
        let (mut short, mut straight, mut total) = (0usize, 0usize, 0usize);
        for pl in &r.polylines {
            if pl.pts.len() < 2 || pl.pts[0].0 < 0.4 * r.w_mm {
                continue;
            }
            total += 1;
            let mut len = 0.0;
            for s in pl.pts.windows(2) {
                len += ((s[1].0 - s[0].0).powi(2) + (s[1].1 - s[0].1).powi(2)).sqrt();
            }
            if (1.2..=6.5).contains(&len) {
                short += 1;
            }
            // 到首尾弦的最大垂距：短直线的偏离应远小于自身长度。
            let (a, b) = (pl.pts[0], *pl.pts.last().unwrap());
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let chord = (dx * dx + dy * dy).sqrt().max(1e-6);
            let mut dev = 0.0f64;
            for &p in &pl.pts {
                let d = ((p.0 - a.0) * dy - (p.1 - a.1) * dx).abs() / chord;
                dev = dev.max(d);
            }
            if dev <= 0.12 * len + 0.25 {
                straight += 1;
            }
        }
        assert!(total >= 20, "中灰区应有足够笔触，实际 {total}");
        assert!(
            short * 100 >= total * 70,
            "至少 70% 笔触应是 1.2~6.5mm 短段，实际 {}/{total}",
            short
        );
        assert!(
            straight * 100 >= total * 75,
            "至少 75% 笔触应是短直线，实际 {}/{total}",
            straight
        );
    }

    /// 「跟随轮廓」用等照度线切向：一片 135° 斜纹里，笔触应顺着斜纹走。
    #[test]
    fn stroke_sketch_follows_contour_direction() {
        let (w, h) = (200u32, 200u32);
        let gray = image::GrayImage::from_fn(w, h, |x, y| {
            let on = (x + y) % 8 < 3;
            image::Luma([if on { 55u8 } else { 250u8 }])
        });
        let mut o = img_opts(10);
        o.stroke_len_mm = 3.0;
        o.stroke_follow = 0;
        o.hatch_levels = 3;
        o.threshold = 0.5;
        o.edge_threshold = 0.9;
        o.sketch_jitter = 0.0;
        o.order_mode = 2;
        let r = crate::io::gray_to_polylines(&gray, &o);
        assert!(!r.polylines.is_empty());
        let (wmm, hmm) = (r.w_mm, r.h_mm);
        let (x0, x1, y0, y1) = (0.2 * wmm, 0.8 * wmm, 0.2 * hmm, 0.8 * hmm);
        let mut along = 0.0f64;
        let mut total = 0.0f64;
        for pl in &r.polylines {
            for s in pl.pts.windows(2) {
                let (ax, ay) = s[0];
                let (bx, by) = s[1];
                let (mx, my) = ((ax + bx) * 0.5, (ay + by) * 0.5);
                if mx < x0 || mx > x1 || my < y0 || my > y1 {
                    continue;
                }
                let len = ((bx - ax).powi(2) + (by - ay).powi(2)).sqrt();
                if len < 1e-6 {
                    continue;
                }
                let mut th = (by - ay).atan2(bx - ax).to_degrees();
                if th < 0.0 {
                    th += 180.0;
                }
                total += len;
                if (th - 135.0).abs() <= 25.0 {
                    along += len;
                }
            }
        }
        assert!(total > 20.0, "内部没有笔触：{total:.1}mm");
        let frac = along / total;
        assert!(
            frac > 0.5,
            "笔触应顺着原稿斜纹（135°），实际只有 {:.0}%（共 {total:.0}mm）",
            frac * 100.0
        );
    }

    /// 多笔素描笔触：浅笔铺中间调、深笔压暗部，笔标签按浅 → 深输出。
    #[test]
    fn stroke_sketch_multi_pen_allocates_tone_bands() {
        let (w, h) = (240u32, 160u32);
        let gray = image::GrayImage::from_fn(w, h, |x, _| {
            image::Luma([if x < 80 {
                255
            } else if x < 160 {
                150
            } else {
                15
            }])
        });
        let mut o = img_opts(10);
        o.multi_pen = true;
        o.pen_count = 4;
        o.pens = [
            crate::sketch::HatchPen { width_mm: 0.3, darkness: 0.45 },
            crate::sketch::HatchPen { width_mm: 0.5, darkness: 0.65 },
            crate::sketch::HatchPen { width_mm: 0.8, darkness: 0.82 },
            crate::sketch::HatchPen { width_mm: 1.0, darkness: 0.95 },
        ];
        o.stroke_len_mm = 3.0;
        o.hatch_levels = 5;
        o.threshold = 0.5;
        o.sketch_jitter = 0.0;
        o.order_mode = 2;
        let r = crate::io::gray_to_polylines(&gray, &o);
        assert_eq!(r.pens.len(), r.polylines.len(), "每根笔迹都要有笔标签");
        assert!(!r.polylines.is_empty(), "多笔素描笔触应生成笔迹");
        let wmm = r.w_mm;
        let pens_in_box = |x0: f64, x1: f64| {
            let mut v = std::collections::BTreeSet::new();
            for (pl, &pen) in r.polylines.iter().zip(r.pens.iter()) {
                if pl.pts.iter().any(|p| p.0 >= x0 * wmm && p.0 <= x1 * wmm) {
                    v.insert(pen);
                }
            }
            v
        };
        assert!(pens_in_box(0.03, 0.30).is_empty(), "亮部应留白");
        let mid = pens_in_box(0.36, 0.63);
        let dark = pens_in_box(0.70, 0.97);
        assert!(mid.contains(&0), "中间调应由最浅的笔起稿：{mid:?}");
        assert!(dark.contains(&3), "最暗处必须轮到最深那支笔：{dark:?}");
        let sorted = r.pens.windows(2).all(|s| s[0] <= s[1]);
        assert!(sorted, "笔迹应按 浅 → 深 分笔输出：{:?}", r.pens);
    }

    /// 造一张已带笔标签的测试图片：三条横线分别属于笔 0 / 2 / 0。
    fn push_tagged_image(app: &mut AppState) {
        push_test_image(app, 40.0, 40.0);
        let img = app.images.last_mut().expect("刚推入的图片");
        img.base = vec![
            wrobot_core::Polyline::new(vec![(10.0, 10.0), (20.0, 10.0)]),
            wrobot_core::Polyline::new(vec![(10.0, 20.0), (20.0, 20.0)]),
            wrobot_core::Polyline::new(vec![(10.0, 30.0), (20.0, 30.0)]),
        ];
        img.base_pens = vec![0, 2, 0];
        img.multi_pen = true;
    }

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    /// 多笔任务：按笔拆成独立 G-code，文字归主笔，每份都能独立重新认原点。
    #[test]
    fn multi_pen_jobs_split_by_pen_and_reanchor() {
        let Some(mut app) = make_app() else { return };
        app.tab = 0;
        app.texts[0].text = "永".into();
        push_tagged_image(&mut app);

        let jobs = app.build_pen_jobs();
        assert_eq!(jobs.len(), 3, "三支笔应拆成三份任务");
        assert_eq!(
            jobs.iter().map(|j| j.pen).collect::<Vec<_>>(),
            vec![0, 1, 2],
            "任务按笔序（浅 → 深）排列"
        );
        assert_eq!(jobs[0].paths.len(), 2, "笔 0 画两条横线");
        assert_eq!(jobs[2].paths.len(), 1, "笔 2 画一条横线");
        assert!(!jobs[1].paths.is_empty(), "文字应由主笔出线");
        assert_eq!(jobs[1].pen, app.main_pen_idx(), "文字/边框必须归主笔");
        assert!(jobs[1].name.contains("2B"), "默认主笔是 2B：{}", jobs[1].name);

        for j in &jobs {
            assert!(
                j.gcode.contains("G92 X0 Y0"),
                "每份任务都要重新认原点：{}",
                j.gcode
            );
            assert!(
                j.gcode.contains("G0 X0 Y0"),
                "每份任务结束时回到页面零点：{}",
                j.gcode
            );
        }
    }

    /// 套准偏移只作用于对应那支笔，并且预览标签与实际输出一一对应。
    #[test]
    fn multi_pen_offsets_shift_only_their_own_pen() {
        let Some(mut app) = make_app() else { return };
        app.tab = 0;
        app.texts[0].text = String::new();
        push_tagged_image(&mut app);
        app.pens[0].offset_mm = [1.5, -0.8];
        app.pens[2].offset_mm = [-0.5, 2.0];

        let img = app.images[0].clone();
        let expect0 = img.to_page(10.0, 10.0);
        let expect2 = img.to_page(10.0, 20.0);
        let tagged = app.collect_tagged_polylines(false);
        let p0 = tagged
            .iter()
            .find(|(_, t)| *t == 0)
            .expect("应有笔 0 的笔迹")
            .0
            .pts[0];
        let p2 = tagged
            .iter()
            .find(|(_, t)| *t == 2)
            .expect("应有笔 2 的笔迹")
            .0
            .pts[0];
        assert!(
            approx(p0.0, expect0.0 + 1.5) && approx(p0.1, expect0.1 - 0.8),
            "笔 0 应叠加 (1.5,-0.8)：实际 {p0:?}，期望 ({:.3},{:.3})",
            expect0.0 + 1.5,
            expect0.1 - 0.8
        );
        assert!(
            approx(p2.0, expect2.0 - 0.5) && approx(p2.1, expect2.1 + 2.0),
            "笔 2 应叠加 (-0.5,2.0)：实际 {p2:?}，期望 ({:.3},{:.3})",
            expect2.0 - 0.5,
            expect2.1 + 2.0
        );

        app.rebuild_preview();
        assert_eq!(
            app.preview.len(),
            app.preview_pens.len(),
            "预览笔迹与笔标签必须一一对应"
        );
        assert!(app.preview_pens.contains(&0) && app.preview_pens.contains(&2));
    }

    /// 换笔队列：上一支停稳才推进，刚下发的 1.5s 宽限期不能被误判为画完。
    #[test]
    fn multi_pen_queue_advances_only_after_machine_stops() {
        let Some(mut app) = make_app() else { return };
        app.tab = 0;
        app.texts[0].text = "永".into();
        push_tagged_image(&mut app);

        assert!(app.start_pen_jobs());
        assert!(app.pen_running && app.pen_wait && !app.pen_active);
        assert_eq!(app.pen_jobs.len(), 3);

        // 第一支下发后停稳 → 进入换笔等待。
        app.pen_wait = false;
        app.pen_active = true;
        app.pen_seen_running = true;
        app.poll_pen_queue(false);
        assert_eq!(app.pen_job_idx, 1);
        assert!(app.pen_wait, "停稳后应提示换笔");
        assert!(!app.pen_active);

        // 第二支刚下发、后台线程尚未置 running：不能算完成。
        app.pen_wait = false;
        app.pen_active = true;
        app.pen_seen_running = false;
        app.pen_dispatched_at = Some(std::time::Instant::now());
        app.poll_pen_queue(false);
        assert_eq!(app.pen_job_idx, 1, "宽限期内不得推进");

        // 超过宽限期且确实没在跑 → 推进。
        app.pen_dispatched_at = Some(
            std::time::Instant::now() - std::time::Duration::from_secs(2),
        );
        app.poll_pen_queue(false);
        assert_eq!(app.pen_job_idx, 2);

        // 最后一支画完 → 队列关闭。
        app.pen_wait = false;
        app.pen_active = true;
        app.pen_seen_running = false;
        app.pen_dispatched_at = Some(
            std::time::Instant::now() - std::time::Duration::from_secs(2),
        );
        app.poll_pen_queue(false);
        assert!(!app.pen_running, "所有笔完成后队列应关闭");
        assert!(app.pen_jobs.is_empty());

        // 取消：清空剩余任务，不再提示换笔。
        assert!(app.start_pen_jobs());
        app.cancel_pen_jobs();
        assert!(!app.pen_running && !app.pen_wait && app.pen_jobs.is_empty());
    }

    /// 导出多笔：写合并版的同时，按文件名标出每支笔，换笔后可直接单独运行。
    #[test]
    fn export_multipen_writes_one_file_per_pen() {
        let Some(mut app) = make_app() else { return };
        app.tab = 0;
        app.texts[0].text = String::new();
        push_tagged_image(&mut app);

        let dir = std::env::temp_dir().join(format!("wrobot-multipen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let main = dir.join("job.nc");
        crate::io::export_gcode(&mut app, &main).unwrap();
        assert!(main.exists(), "应写出合并版");
        assert!(
            dir.join("job-pen1-HB_0_20.nc").exists(),
            "应写出第 1 支笔的分笔文件"
        );
        assert!(
            dir.join("job-pen2-4B_0_32.nc").exists(),
            "应写出第 2 支笔的分笔文件"
        );
        assert!(app.status.contains("分笔"), "状态栏应提示已导出分笔文件");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 分页多笔：每页每支笔仍是独立、带锚点的任务。
    #[test]
    fn multi_pen_tiled_jobs_keep_per_page_anchor() {
        let Some(mut app) = make_app() else { return };
        app.tab = 0;
        app.texts[0].text = String::new();
        push_tagged_image(&mut app);
        app.tile_on = true;
        app.tile_marks = false;

        let jobs = app.build_pen_jobs();
        assert_eq!(
            jobs.iter().map(|j| j.pen).collect::<Vec<_>>(),
            vec![0, 2],
            "分页里只应出现图片实际用到的两支笔"
        );
        for j in &jobs {
            assert!(j.gcode.contains("大纸张分页"), "分页任务应带页码：{}", j.gcode);
            assert!(j.gcode.contains("G92 X0 Y0"), "分页任务也要重新认原点");
            assert!(j.gcode.contains("G0 X0 Y0"), "分页任务结束回到页面零点");
        }
    }

    /// ⑥ 行距 / 字间距可以为负：把行与字挤紧，但不能把排列搞反。
    #[test]
    fn negative_line_gap_and_letter_space() {
        let Some(mut app) = make_app() else { return };
        app.tab = 0;
        app.texts[0].text = "一二三\n四五六\n七八九".into();

        app.texts[0].line_gap = 8.0;
        app.texts[0].space = 4.0;
        app.rebuild_preview();
        let (x0, y0, x1, y1) = wrobot_core::render::bounds_all(&app.preview);
        assert!(!app.preview.is_empty(), "正常行距应有笔迹");

        app.texts[0].line_gap = -6.0;
        app.texts[0].space = -3.0;
        app.rebuild_preview();
        let (tx0, ty0, tx1, ty1) = wrobot_core::render::bounds_all(&app.preview);
        assert!(!app.preview.is_empty(), "负行距下仍应有笔迹");
        assert!(ty1 - ty0 > 0.0, "文字块高度必须为正，实际 {} ", ty1 - ty0);
        assert!(ty1 - ty0 < y1 - y0, "负行距应把行压得更紧：{} < {}", ty1 - ty0, y1 - y0);
        assert!(tx1 - tx0 < x1 - x0, "负字间距应把字挤得更紧：{} < {}", tx1 - tx0, x1 - x0);
    }

    /// ④ 图片：停在图片页也不再自动并入；每张图还有自己的「输出」开关。
    #[test]
    fn images_not_merged_on_image_tab_until_checked() {
        let Some(mut app) = make_app() else { return };
        app.texts[0].text = String::new();
        app.images.push(crate::state::ImageObject {
            path: "t.png".into(),
            bytes: Vec::new(),
            x: 0.0,
            y: 0.0,
            scale: 1.0,
            rotation: 0.0,
            flip_h: false,
            flip_v: false,
            draw: crate::state::ImgDraw::Wire,
            step_px: 2,
            threshold: 0.5,
            edge_threshold: 0.5,
            invert: false,
            crop: (0.0, 0.0, 1.0, 1.0),
            scan_angle: 0,
            merge_gap_px: 8,
            order_mode: 0,
            block_px: 40,
            sketch_pressure: 2,
            sketch_jitter: 0.35,
            sketch_pen: 0.5,
            hatch_levels: 6,
            hatch_angle: 35.0,
            region_block_mm: 2.5,
            stroke_len_mm: 3.2,
            stroke_follow: 2,
            multi_pen: false,
            background: false,
            out: true,
            base: vec![wrobot_core::Polyline::new(vec![(0.0, 0.0), (10.0, 10.0)])],
            base_pens: vec![],
            natural_w: 10.0,
            natural_h: 10.0,
            gen_key: 0,
        });

        // 停在图片页（tab 5）且未勾选：不得并入（BUG：以前一点图片模块就并进来）
        app.tab = 5;
        app.images_on = false;
        assert!(!app.images_active(), "停在图片页也不应自动并入");
        assert_eq!(
            app.collect_polylines(false).len(),
            0,
            "未勾选时 G-code 里不应有图片笔迹"
        );

        // 勾模块开关 → 并入
        app.images_on = true;
        assert_eq!(app.collect_polylines(false).len(), 1, "勾选模块后应并入");

        // 单张图取消「输出」→ 又不并入（只有选择的才并入）
        app.images[0].out = false;
        assert_eq!(
            app.collect_polylines(false).len(),
            0,
            "取消单张图的输出后不应并入"
        );
        assert!(!app.image_outputs(0));
    }

    /// ⑦ 随机变形只对勾上的文本框生效。
    #[test]
    fn random_applies_only_to_selected_text_boxes() {
        let Some(mut app) = make_app() else { return };
        app.tab = 0;
        app.texts[0].text = "第一段不抖".into();
        app.texts[0].random = false;
        app.add_text_box();
        app.texts[1].text = "第二段要抖".into();
        app.texts[1].random = true;
        app.use_random = true;
        app.rand_x = 1.0;
        app.rand_y = 1.0;
        app.rand_head = 3.0;

        // 只留「不抖」的框：随机开与关应完全一致
        app.texts[1].on = false;
        let a = app.collect_polylines(true);
        let b = app.collect_polylines(false);
        assert_eq!(a.len(), b.len(), "未勾随机的框不应改变笔迹条数");
        let same = a
            .iter()
            .zip(b.iter())
            .all(|(p, q)| p.pts == q.pts);
        assert!(same, "未勾随机的文本框笔迹不应发生任何抖动");

        // 只留「要抖」的框：随机开与关应不同
        app.texts[0].on = false;
        app.texts[1].on = true;
        let c = app.collect_polylines(true);
        let d = app.collect_polylines(false);
        assert_eq!(c.len(), d.len(), "抖动不应改变笔迹条数");
        let diff = c
            .iter()
            .zip(d.iter())
            .any(|(p, q)| p.pts != q.pts);
        assert!(diff, "勾了随机的文本框笔迹应当被抖动");
    }

    /// ③⑤ 画布辅助层默认开启，且不参与打印。
    #[test]
    fn canvas_overlays_default_on_and_never_printed() {
        let Some(mut app) = make_app() else { return };
        assert!(app.axes_on, "默认应显示 XY 方向箭头");
        assert!(app.text_frame_on, "默认应显示文本框虚框");
        // 它们只影响画布绘制，不应改变任何输出
        app.tab = 0;
        let before = app.collect_polylines(false).len();
        app.axes_on = false;
        app.text_frame_on = false;
        let after = app.collect_polylines(false).len();
        assert_eq!(before, after, "箭头/虚框必须只画在画布上，不能进 G-code");
    }

    /// ⑧ PDF 导入：可指定页，默认 1:1 原始尺寸，且默认不并入输出。
    #[test]
    fn pdf_import_selected_page_one_to_one() {
        let Some(mut app) = make_app() else { return };
        let path = std::path::Path::new(r"D:\WritingRobot\4.教案本操作图文版.pdf");
        if !path.exists() {
            eprintln!("跳过：找不到示例 PDF");
            return;
        }
        let n = app.load_pdf_info(path).expect("应能读出页数");
        assert!(n > 5, "示例 PDF 应有几十页，实际 {}", n);
        assert_eq!(app.pdf_page, 1, "默认选中第 1 页");
        assert!(app.pdf_1to1, "默认按 1:1 原始尺寸导入");

        let before = app.images.len();
        let (n_vec, n_img) = app
            .import_pdf_page(path, 2)
            .expect("第 2 页应能导入");
        assert!(n_vec > 0 || n_img > 0, "这一页应有可绘制内容");
        assert!(app.images.len() > before, "应新增图片/矢量对象");
        assert!(!app.images_active(), "导入后默认不并入输出");

        if n_vec > 0 {
            // 1:1：对象尺寸就是 PDF 页面的物理尺寸（毫米）
            let last = app.images.last().unwrap();
            assert!(
                last.natural_w > 100.0 && last.natural_h > 100.0,
                "1:1 导入尺寸应接近页面物理尺寸，实际 {:.1}×{:.1}",
                last.natural_w,
                last.natural_h
            );
        }
    }

    /// ① 页面尺寸：默认标准 A4，可一键切常规规格，可横竖，可自定义。
    ///
    /// 旧版写死 175×255 —— 哪个标准纸都不是，界面上还只有两个滑块靠猜。
    #[test]
    fn paper_preset_switch_custom_and_orientation() {
        let Some(mut app) = make_app() else { return };
        assert_eq!(app.paper, "A4", "默认应为 A4");
        assert!(
            (app.page_w_mm - 210.0).abs() < 1e-6 && (app.page_h_mm - 297.0).abs() < 1e-6,
            "A4 应为 210×297，实际 {}×{}",
            app.page_w_mm,
            app.page_h_mm
        );

        app.set_paper("A5");
        assert!(
            (app.page_w_mm - 148.0).abs() < 1e-6 && (app.page_h_mm - 210.0).abs() < 1e-6,
            "A5 应为 148×210，实际 {}×{}",
            app.page_w_mm,
            app.page_h_mm
        );

        // 横竖切换：宽高互换，规格名不变
        app.toggle_orientation();
        assert!(
            (app.page_w_mm - 210.0).abs() < 1e-6 && (app.page_h_mm - 148.0).abs() < 1e-6,
            "横放 A5 应为 210×148，实际 {}×{}",
            app.page_w_mm,
            app.page_h_mm
        );
        app.sync_paper();
        assert_eq!(app.paper, "A5", "横放也应识别成 A5");

        // 每个预设都能套用（先摆回纵向，预设默认按纵向写入）
        if app.page_w_mm > app.page_h_mm {
            app.toggle_orientation();
        }
        for (nm, w, h) in crate::state::PAPER_PRESETS {
            app.set_paper(nm);
            assert!(
                (app.page_w_mm - w).abs() < 1e-6 && (app.page_h_mm - h).abs() < 1e-6,
                "{nm} 应为 {w}×{h}，实际 {}×{}",
                app.page_w_mm,
                app.page_h_mm
            );
        }

        // 手动改成非标准尺寸 → 自动标「自定义」
        app.page_w_mm = 333.0;
        app.page_h_mm = 111.0;
        app.sync_paper();
        assert_eq!(app.paper, crate::state::PAPER_CUSTOM);
        assert!(
            app.paper_label().contains("333×111"),
            "标题应显示真实尺寸：{}",
            app.paper_label()
        );
    }

    /// ② 左栏坐标：默认不轮询（省链路），页面坐标 → 机器坐标随反转镜像。
    #[test]
    fn coordinate_panel_defaults_and_page_to_machine_mapping() {
        let Some(mut app) = make_app() else { return };
        assert_eq!(app.status_poll_ms, 0, "坐标轮询默认关闭，避免空闲时占用链路");
        assert!(app.cursor_mm.is_none(), "鼠标不在画布上时没有页面坐标");

        app.reverse_x = false;
        app.reverse_y = false;
        app.swap_xy = false;
        let a = app.gcode_options().transform(10.0, 20.0);
        app.reverse_x = true;
        let b = app.gcode_options().transform(10.0, 20.0);
        assert!(
            (a.0 + b.0 - app.page_w_mm).abs() < 1e-6,
            "反转 X 后两点应关于页面中线对称：{:?} vs {:?}",
            a,
            b
        );
        assert!((a.1 - b.1).abs() < 1e-6, "反转 X 不该改变机器 Y：{:?} vs {:?}", a, b);
    }

    /// ③ 点动方向必须跟随「反转 X/Y / 对调 XY」，且与画布箭头指向同一个方向。
    ///
    /// 旧实现直接用页面位移当机器位移，开了反转后按「X +」机器往左走，
    /// 跟画布上蓝色 X 箭头正好相反。
    fn jog_xy(g: &str) -> (f64, f64) {
        let after_x = g.split_once('X').expect("应含 X").1;
        let (xs, rest) = after_x.split_once(' ').expect("X 后应有空格");
        let ys = rest.split_once('Y').expect("应含 Y").1;
        let yv = ys.split_whitespace().next().expect("Y 后应有数值");
        (xs.parse().unwrap(), yv.parse().unwrap())
    }

    #[test]
    fn jog_follows_reverse_and_swap_like_the_arrows() {
        let Some(mut app) = make_app() else { return };
        app.firmware_sel = 0; // GRBL
        let cases = [
            (false, false, false),
            (true, false, false),
            (false, true, false),
            (true, true, false),
            (false, false, true),
            (true, true, true),
        ];
        for &(rx, ry, sw) in &cases {
            app.reverse_x = rx;
            app.reverse_y = ry;
            app.swap_xy = sw;
            let (xd, yd) = app.gcode_options().axis_dirs();

            // 沿蓝色 X 箭头方向点动 10mm → 机器 X 必须 +10，机器 Y 必须 0
            let (mx, my) = jog_xy(&app.jog_gcode(xd.0 * 10.0, xd.1 * 10.0));
            assert!(
                (mx - 10.0).abs() < 1e-6 && my.abs() < 1e-6,
                "沿 X 箭头点动应只走机器 X +10，实际 ({mx}, {my})；rx={rx} ry={ry} swap={sw}"
            );
            // 沿绿色 Y 箭头方向点动 10mm → 机器 Y 必须 +10，机器 X 必须 0
            let (mx, my) = jog_xy(&app.jog_gcode(yd.0 * 10.0, yd.1 * 10.0));
            assert!(
                mx.abs() < 1e-6 && (my - 10.0).abs() < 1e-6,
                "沿 Y 箭头点动应只走机器 Y +10，实际 ({mx}, {my})；rx={rx} ry={ry} swap={sw}"
            );
            // 按钮上写的是「页面方向」：未反转时按 X+ 机器就是 +X
            if !rx && !ry && !sw {
                let (mx, my) = jog_xy(&app.jog_gcode(10.0, 0.0));
                assert!((mx - 10.0).abs() < 1e-6 && my.abs() < 1e-6);
            }
            if rx && !ry && !sw {
                let (mx, _my) = jog_xy(&app.jog_gcode(10.0, 0.0));
                assert!((mx + 10.0).abs() < 1e-6, "反转 X 后按 X+ 机器应走 −X，实际 {mx}");
            }
            if !rx && !ry && sw {
                let (mx, my) = jog_xy(&app.jog_gcode(10.0, 0.0));
                assert!(
                    mx.abs() < 1e-6 && (my - 10.0).abs() < 1e-6,
                    "对调 XY 后按 X+ 应走机器 Y +10，实际 ({mx}, {my})"
                );
            }
        }
    }

    /// 视觉验收工具（默认 ignore）：把一张铅笔稿按「素描原稿」模式重画，
    /// 输出「原稿 | 重建」并排对照图，用于人工判断拟真度。
    #[test]
    #[ignore]
    fn dump_sketch_preview() {
        let root = std::path::Path::new(r"D:\WritingRobot\wrobot\tools");
        let name = std::env::var("SKETCH_IN").unwrap_or_else(|_| "sketch_input.png".to_string());
        let p0 = std::path::PathBuf::from(&name);
        // cargo test 的工作目录是 crate 根，图片常放在工作区根或 tools/，三处依次找
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf())
            .unwrap_or_default();
        let path = if p0.is_absolute() || p0.exists() {
            p0
        } else if repo.join(&name).exists() {
            repo.join(&name)
        } else {
            root.join(&name)
        };
        let src = match std::fs::read(&path) {
            Ok(v) => v,
            Err(_) => return,
        };
        let mut gray = image::load_from_memory(&src).unwrap().to_luma8();
        // 和 `image_to_polylines` 一样先做色阶归一化（纸不是纯白时这是前提）
        crate::io::stretch_levels(&mut gray);
        let (w, h) = gray.dimensions();
        // SKETCH_DRAW=1 素描（影线）/ 5 交叉排线 / 7 素描原稿：同一张图横向对比
        let draw_type: usize = std::env::var("SKETCH_DRAW")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(7);
        let mut opts = img_opts(draw_type);
        opts.target_h_mm = std::env::var("SKETCH_H")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(120.0);
        opts.threshold = 0.72;
        opts.order_mode = 0;
        opts.sketch_pressure = 3;
        opts.sketch_jitter = 0.12;
        if let Ok(v) = std::env::var("SKETCH_THR") {
            opts.threshold = v.parse().unwrap_or(opts.threshold);
        }
        if let Ok(v) = std::env::var("SKETCH_PRESS") {
            opts.sketch_pressure = v.parse().unwrap_or(opts.sketch_pressure);
        }
        if let Ok(v) = std::env::var("SKETCH_JIT") {
            opts.sketch_jitter = v.parse().unwrap_or(opts.sketch_jitter);
        }
        if let Ok(v) = std::env::var("SKETCH_PENW") {
            opts.sketch_pen = v.parse().unwrap_or(opts.sketch_pen);
        }
        if let Ok(v) = std::env::var("SKETCH_EDGE") {
            opts.edge_threshold = v.parse().unwrap_or(opts.edge_threshold);
        }
        if let Ok(v) = std::env::var("SKETCH_LEVELS") {
            opts.hatch_levels = v.parse().unwrap_or(opts.hatch_levels);
        }
        if let Ok(v) = std::env::var("SKETCH_BLOCK") {
            opts.region_block_mm = v.parse().unwrap_or(opts.region_block_mm);
        }
        if let Ok(v) = std::env::var("SKETCH_LEN") {
            opts.stroke_len_mm = v.parse().unwrap_or(opts.stroke_len_mm);
        }
        if let Ok(v) = std::env::var("SKETCH_FOLLOW") {
            opts.stroke_follow = v.parse().unwrap_or(opts.stroke_follow);
        }
        // SKETCH_MULTI=1：按默认铅笔笔组走多笔分层，右半边按每支笔的
        // 真实线宽与深浅着色，用来核对「浅笔铺中间调、深笔压暗部」。
        let multi = std::env::var("SKETCH_MULTI").is_ok();
        let pens = crate::state::default_pens();
        if multi {
            opts.multi_pen = true;
            opts.pen_count = pens.len() as u8;
            for (i, p) in pens.iter().take(4).enumerate() {
                opts.pens[i] = p.to_hatch();
            }
        }
        let t0 = std::time::Instant::now();
        let r = crate::io::gray_to_polylines(&gray, &opts);
        eprintln!(
            "[timing] 输入 {}×{} → 笔迹 {} 条，耗时 {:.0} ms",
            w,
            h,
            r.polylines.len(),
            t0.elapsed().as_secs_f64() * 1000.0
        );
        // SKETCH_MODE=skel：右半边改画骨架，用来判断是骨架断了还是后续阶段丢了笔迹
        let debug_skel = std::env::var("SKETCH_MODE").ok().as_deref() == Some("skel");
        let skel = if debug_skel {
            let mut ink = vec![false; (w * h) as usize];
            for y in 0..h {
                for x in 0..w {
                    ink[(y * w + x) as usize] =
                        (gray.get_pixel(x, y).0[0] as f64) < opts.threshold * 255.0;
                }
            }
            Some(crate::io::thin_ink(&ink, w, h))
        } else {
            None
        };
        let base_ppm = 1.0 / (opts.target_h_mm / h as f64); // px / mm
        // 取景框（原稿像素坐标）与放大倍数，便于逐块检查细节
        let z: f64 = std::env::var("SKETCH_ZOOM")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1.0);
        let (ox, oy, bw, bh) = match std::env::var("SKETCH_BOX") {
            Ok(v) => {
                let n: Vec<f64> = v
                    .split(',')
                    .filter_map(|s| s.trim().parse().ok())
                    .collect();
                if n.len() == 4 {
                    (n[0], n[1], n[2] - n[0], n[3] - n[1])
                } else {
                    (0.0, 0.0, w as f64, h as f64)
                }
            }
            Err(_) => (0.0, 0.0, w as f64, h as f64),
        };
        let ow = (bw * z).round().max(1.0) as u32;
        let oh = (bh * z).round().max(1.0) as u32;
        let mut buf = vec![255u8; (ow * 2 * oh * 4) as usize];
        let overlay = std::env::var("SKETCH_MODE").ok().as_deref() == Some("overlay");
        // 左半边先铺原稿（最近邻采样）
        for y in 0..oh {
            for x in 0..ow {
                let sx = ((ox + x as f64 / z) as u32).min(w - 1);
                let sy = ((oy + y as f64 / z) as u32).min(h - 1);
                let v = gray.get_pixel(sx, sy).0[0];
                let i = ((y * ow * 2 + x) * 4) as usize;
                buf[i] = v;
                buf[i + 1] = v;
                buf[i + 2] = v;
                buf[i + 3] = 255;
            }
        }
        let mut put = |x: i32, y: i32, v: u8| {
            if x < 0 || y < 0 || x >= (ow * 2) as i32 || y >= oh as i32 {
                return;
            }
            let i = ((y as u32 * ow * 2 + x as u32) * 4) as usize;
            if overlay && x < ow as i32 {
                // 叠加模式：原稿灰度底上画红线
                buf[i] = 220;
                buf[i + 1] = 30;
                buf[i + 2] = 30;
                buf[i + 3] = 255;
                return;
            }
            let cur = buf[i];
            if v < cur {
                buf[i] = v;
                buf[i + 1] = v;
                buf[i + 2] = v;
            }
            buf[i + 3] = 255;
        };
        // 预览笔宽：SKETCH_PEN_AUTO=1 时按真实笔宽折算成显示像素。
        // 固定 1~2px 的预览会骗人——缩放越大，画出来的线相对越细，
        // 看着稀疏其实纸上已经糊满。
        let pen: i32 = if std::env::var("SKETCH_PEN_AUTO").is_ok() {
            let d = opts.sketch_pen * base_ppm * z;
            (d.round() as i32).max(1)
        } else {
            std::env::var("SKETCH_PEN")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1)
        };
        let mut dot = |x: i32, y: i32, v: u8, pd: i32| {
            if pd <= 1 {
                put(x, y, v);
                return;
            }
            let pr = (pd - 1) as f64 / 2.0;
            for dy in -(pd / 2)..=(pd / 2) {
                for dx in -(pd / 2)..=(pd / 2) {
                    if ((dx * dx + dy * dy) as f64).sqrt() <= pr + 0.35 {
                        put(x + dx, y + dy, v);
                    }
                }
            }
        };
        if multi {
            let mut n = [0usize; 4];
            let mut mm = [0.0f64; 4];
            for (pl, &tag) in r.polylines.iter().zip(r.pens.iter()) {
                let t = (tag as usize).min(3);
                n[t] += 1;
                for s in pl.pts.windows(2) {
                    mm[t] += ((s[1].0 - s[0].0).powi(2) + (s[1].1 - s[0].1).powi(2)).sqrt();
                }
            }
            for (i, p) in pens.iter().enumerate() {
                eprintln!(
                    "[multi] 笔 {}「{}」{:.2}mm · 深度 {:.0}% · {} 条 / {:.0}mm",
                    i + 1,
                    p.name,
                    p.width_mm,
                    p.darkness * 100.0,
                    n[i],
                    mm[i]
                );
            }
        }
        let dx_off = if overlay { 0.0 } else { ow as f64 };
        for (k, pl) in r.polylines.iter().enumerate() {
            let (pd, tone) = if multi {
                let tag = (r.pens.get(k).copied().unwrap_or(0) as usize).min(pens.len() - 1);
                let p = &pens[tag];
                let d = (p.width_mm * base_ppm * z).round() as i32;
                (d.max(1), p.color[0])
            } else {
                (pen, 40)
            };
            for seg in pl.pts.windows(2) {
                let (x0, y0) = (
                    (seg[0].0 * base_ppm - ox) * z + dx_off,
                    (seg[0].1 * base_ppm - oy) * z,
                );
                let (x1, y1) = (
                    (seg[1].0 * base_ppm - ox) * z + dx_off,
                    (seg[1].1 * base_ppm - oy) * z,
                );
                let dx = x1 - x0;
                let dy = y1 - y0;
                let steps = dx.abs().max(dy.abs()).ceil() as i32 + 1;
                for s in 0..=steps {
                    let t = s as f64 / steps as f64;
                    dot(
                        (x0 + dx * t).round() as i32,
                        (y0 + dy * t).round() as i32,
                        tone,
                        pd,
                    );
                }
            }
        }
        // 调试：算法内部看到的调子图（深 = 调子高）。用来核对「起笔门槛」
        // 落在哪儿：脸上如果已经是一片灰，排线当然会铺满整张脸。
        if std::env::var("SKETCH_MODE").ok().as_deref() == Some("ftone") {
            let t = crate::sketch::field_tone(&gray, w, h, opts.target_h_mm / h as f64, opts.invert);
            for y in 0..oh {
                for x in 0..ow {
                    let sx = ((ox + x as f64 / z) as u32).min(w - 1);
                    let sy = ((oy + y as f64 / z) as u32).min(h - 1);
                    let v = t[(sy * w + sx) as usize];
                    let i = ((y * ow * 2 + x + ow) * 4) as usize;
                    buf[i] = v;
                    buf[i + 1] = v;
                    buf[i + 2] = v;
                    buf[i + 3] = 255;
                }
            }
        }
        // 调试：只看「哪些像素被判定成墨」，用来区分是「没认出墨」还是「没追踪好」。
        if std::env::var("SKETCH_MODE").ok().as_deref() == Some("mask") {
            let m = crate::sketch::ink_mask(&gray, w, h, opts.threshold, opts.invert);
            for y in 0..oh {
                for x in 0..ow {
                    let sx = ((ox + x as f64 / z) as u32).min(w - 1);
                    let sy = ((oy + y as f64 / z) as u32).min(h - 1);
                    let v = if m[(sy * w + sx) as usize] { 0u8 } else { 255u8 };
                    let i = ((y * ow * 2 + x + ow) * 4) as usize;
                    buf[i] = v;
                    buf[i + 1] = v;
                    buf[i + 2] = v;
                }
            }
        }
        // 调试：背景掩码（黑 = 被判成背景 → 不排线）。
        if std::env::var("SKETCH_MODE").ok().as_deref() == Some("bgmask") {
            let m = crate::sketch::bg_mask_map(&gray, w, h, opts.target_h_mm / h as f64, opts.invert);
            for y in 0..oh {
                for x in 0..ow {
                    let sx = ((ox + x as f64 / z) as u32).min(w - 1);
                    let sy = ((oy + y as f64 / z) as u32).min(h - 1);
                    let v = if m[(sy * w + sx) as usize] { 0u8 } else { 255u8 };
                    let i = ((y * ow * 2 + x + ow) * 4) as usize;
                    buf[i] = v;
                    buf[i + 1] = v;
                    buf[i + 2] = v;
                }
            }
        }
        // 调试：只画「追踪出来的中心线」，按长度着色，用来判断追踪本身对不对。
        // 调试：方向场。看一眼就知道「排线为什么乱」——走向一致就干净，
        // 每格各指一个方向就必然织成网。
        if std::env::var("SKETCH_MODE").ok().as_deref() == Some("field") {
            let ticks = crate::sketch::field_ticks(
                &gray,
                w,
                h,
                opts.target_h_mm / h as f64,
                opts.invert,
                7.0,
            );
            for (tx, ty, a, c) in ticks {
                let sx = ((tx - ox) * z) as i32 + ow as i32;
                let sy = ((ty - oy) * z) as i32;
                if sx < ow as i32 || sy < 0 || sx >= (ow * 2) as i32 || sy >= oh as i32 {
                    continue;
                }
                let l = 3.0 * z;
                let (dx, dy) = (a.cos() * l, a.sin() * l);
                // 一致性低的地方画浅灰，一眼看出哪里在瞎猜
                let v = if c > 0.3 { 20u8 } else { 190u8 };
                for s in -6..=6 {
                    let t = s as f64 / 6.0;
                    let x = (sx as f64 + dx * t).round() as i32;
                    let y = (sy as f64 + dy * t).round() as i32;
                    if x < ow as i32 || y < 0 || x >= (ow * 2) as i32 || y >= oh as i32 {
                        continue;
                    }
                    let i = ((y as u32 * ow * 2 + x as u32) * 4) as usize;
                    buf[i] = v;
                    buf[i + 1] = v;
                    buf[i + 2] = v;
                    buf[i + 3] = 255;
                }
            }
        }
        if std::env::var("SKETCH_MODE").ok().as_deref() == Some("strokes") {
            let scale_mm = opts.target_h_mm / h as f64;
            let tr = crate::sketch::trace_strokes(
                &gray,
                w,
                h,
                scale_mm,
                opts.threshold,
                opts.invert,
            );
            for y in 0..oh {
                for x in ow..(ow * 2) {
                    let i = ((y * ow * 2 + x) * 4) as usize;
                    buf[i] = 255;
                    buf[i + 1] = 255;
                    buf[i + 2] = 255;
                }
            }
            let mut order: Vec<usize> = (0..tr.len()).collect();
            order.sort_by(|&a, &b| {
                tr[b]
                    .len_mm
                    .partial_cmp(&tr[a].len_mm)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            for &si in order.iter() {
                // 长 = 暖色，短 = 冷色，一眼能看出「哪条是真正的一笔」。
                let v = (tr[si].len_mm / 80.0).clamp(0.0, 1.0);
                let (rr, gg, bb) = (
                    (40.0 + 210.0 * v) as u8,
                    (60.0 + 60.0 * (1.0 - (2.0 * v - 1.0).abs())) as u8,
                    (200.0 - 170.0 * v) as u8,
                );
                for w2 in tr[si].pts.windows(2) {
                    // trace_strokes 给的是像素坐标，不要再乘 base_ppm。
                    let (x0, y0) = ((w2[0].0 - ox) * z + ow as f64, (w2[0].1 - oy) * z);
                    let (x1, y1) = ((w2[1].0 - ox) * z + ow as f64, (w2[1].1 - oy) * z);
                    let dx = x1 - x0;
                    let dy = y1 - y0;
                    let steps = dx.abs().max(dy.abs()).ceil() as i32 + 1;
                    for st in 0..=steps {
                        let t = st as f64 / steps as f64;
                        let px = (x0 + dx * t).round() as i32;
                        let py = (y0 + dy * t).round() as i32;
                        if px < 0 || py < 0 || px >= (ow * 2) as i32 || py >= oh as i32 {
                            continue;
                        }
                        let i = ((py as u32 * ow * 2 + px as u32) * 4) as usize;
                        buf[i] = rr;
                        buf[i + 1] = gg;
                        buf[i + 2] = bb;
                    }
                }
            }
            for (k, &si) in order.iter().take(10).enumerate() {
                let t = &tr[si];
                let a = t.pts[0];
                let b = *t.pts.last().unwrap();
                eprintln!(
                    "长[{k}] {:.1}mm 宽{:.2}px 墨{:.2} 起({:.0},{:.0}) 终({:.0},{:.0}) 点{}",
                    t.len_mm,
                    t.width_px,
                    t.tone,
                    a.0,
                    a.1,
                    b.0,
                    b.1,
                    t.pts.len()
                );
            }
            eprintln!("追踪总数 {}", tr.len());
        }
        if let Some(sk) = &skel {
            // 右半边重画骨架：先清空再点像素
            for y in 0..oh {
                for x in ow..(ow * 2) {
                    let i = ((y * ow * 2 + x) * 4) as usize;
                    buf[i] = 255;
                    buf[i + 1] = 255;
                    buf[i + 2] = 255;
                }
            }
            for y in 0..oh {
                for x in 0..ow {
                    let sx = ((ox + x as f64 / z) as u32).min(w - 1);
                    let sy = ((oy + y as f64 / z) as u32).min(h - 1);
                    if sk[(sy * w + sx) as usize] {
                        let i = ((y * ow * 2 + x + ow) * 4) as usize;
                        buf[i] = 30;
                        buf[i + 1] = 30;
                        buf[i + 2] = 30;
                    }
                }
            }
        }
        // 左半边放原稿（最近邻采样）
        if !overlay {
            for y in 0..oh {
                for x in 0..ow {
                    let sx = ((ox + x as f64 / z) as u32).min(w - 1);
                    let sy = ((oy + y as f64 / z) as u32).min(h - 1);
                    let v = gray.get_pixel(sx, sy).0[0];
                    let i = ((y * ow * 2 + x) * 4) as usize;
                    buf[i] = v;
                    buf[i + 1] = v;
                    buf[i + 2] = v;
                    buf[i + 3] = 255;
                }
            }
        }
        // 量化指标。只看「墨有没有被覆盖」是不够的：一堆乱弯的曲线覆盖率也很高。
        // 所以同时量「落笔有没有落在墨上」「每条笔迹直不直」「笔尖朝向与原稿线条差多少」。
        let mask = crate::sketch::ink_mask(&gray, w, h, opts.threshold, opts.invert);
        let stats = {
            let n = (w * h) as usize;
            let mut hit = vec![false; n];
            for pl in &r.polylines {
                for seg in pl.pts.windows(2) {
                    let (x0, y0) = (seg[0].0 * base_ppm, seg[0].1 * base_ppm);
                    let (x1, y1) = (seg[1].0 * base_ppm, seg[1].1 * base_ppm);
                    let dx = x1 - x0;
                    let dy = y1 - y0;
                    let steps = dx.abs().max(dy.abs()).ceil() as i32 + 1;
                    for s in 0..=steps {
                        let t = s as f64 / steps as f64;
                        let px = (x0 + dx * t).round() as i32;
                        let py = (y0 + dy * t).round() as i32;
                        if px >= 0 && py >= 0 && px < w as i32 && py < h as i32 {
                            hit[(py as u32 * w + px as u32) as usize] = true;
                        }
                    }
                }
            }
            let dist = crate::io::chamfer_distance(&hit, w, h); // 墨 → 笔迹
            let mut ink_px = 0usize;
            let mut c1 = 0usize;
            let mut c2 = 0usize;
            let mut c3 = 0usize;
            let mut dark_px = 0usize;
            let mut dark_c2 = 0usize;
            for y in 0..h {
                for x in 0..w {
                    let i = (y * w + x) as usize;
                    if !mask[i] {
                        continue;
                    }
                    ink_px += 1;
                    let d = dist[i];
                    if d <= 1.0 {
                        c1 += 1;
                    }
                    if d <= 2.0 {
                        c2 += 1;
                    }
                    if d <= 3.0 {
                        c3 += 1;
                    }
                    if gray.get_pixel(x, y).0[0] < 110 {
                        dark_px += 1;
                        if d <= 2.0 {
                            dark_c2 += 1;
                        }
                    }
                }
            }
            // 笔迹 → 墨：落笔点离最近的墨有多远
            let back = crate::io::chamfer_distance(&mask, w, h);
            let mut drawn = 0usize;
            let mut on_ink = 0usize;
            let mut on_ink3 = 0usize;
            for pl in &r.polylines {
                for pt in &pl.pts {
                    let px = (pt.0 * base_ppm).round() as i32;
                    let py = (pt.1 * base_ppm).round() as i32;
                    if px < 0 || py < 0 || px >= w as i32 || py >= h as i32 {
                        continue;
                    }
                    drawn += 1;
                    let d = back[(py as u32 * w + px as u32) as usize];
                    if d <= 2.0 {
                        on_ink += 1;
                    }
                    if d <= 3.0 {
                        on_ink3 += 1;
                    }
                }
            }
            // 直线度：笔迹到「首尾连线」的中位 RMS（排线本来就是直线）
            let mut straight: Vec<f64> = Vec::new();
            // 方向一致性：落笔方向 vs 该处原稿墨的走向
            let mut dirs: Vec<f64> = Vec::new();
            for pl in &r.polylines {
                let pts: Vec<(f64, f64)> = pl
                    .pts
                    .iter()
                    .map(|&(x, y)| (x * base_ppm, y * base_ppm))
                    .collect();
                if pts.len() >= 6 {
                    let (a, b) = (pts[0], *pts.last().unwrap());
                    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                    let l = (dx * dx + dy * dy).sqrt();
                    if l > 6.0 {
                        let (nx, ny) = (-dy / l, dx / l);
                        let mut acc = 0.0f64;
                        let mut cnt = 0.0f64;
                        for p in &pts[1..pts.len() - 1] {
                            let d = (p.0 - a.0) * nx + (p.1 - a.1) * ny;
                            acc += d * d;
                            cnt += 1.0;
                        }
                        straight.push((acc / cnt.max(1.0)).sqrt());
                    }
                }
                if pts.len() >= 8 {
                    for k in (2..pts.len() - 2).step_by(4) {
                        let (p, q) = (pts[k - 2], pts[k + 2]);
                        let (dx, dy) = (q.0 - p.0, q.1 - p.1);
                        let l = (dx * dx + dy * dy).sqrt();
                        if l < 2.0 {
                            continue;
                        }
                        // 7×7 窗口里原稿墨的主方向（PCA）
                        let cx = (pts[k].0 as i32).clamp(0, w as i32 - 1);
                        let cy = (pts[k].1 as i32).clamp(0, h as i32 - 1);
                        let (mut sxx, mut sxy, mut syy, mut cnt) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
                        for yy in -3..=3i32 {
                            for xx in -3..=3i32 {
                                let (qx, qy) = (cx + xx, cy + yy);
                                if qx < 0 || qy < 0 || qx >= w as i32 || qy >= h as i32 {
                                    continue;
                                }
                                if !mask[(qy as u32 * w + qx as u32) as usize] {
                                    continue;
                                }
                                let (rx, ry) = (xx as f64, yy as f64);
                                sxx += rx * rx;
                                sxy += rx * ry;
                                syy += ry * ry;
                                cnt += 1.0;
                            }
                        }
                        if cnt < 6.0 {
                            continue;
                        }
                        // 点云的主方向（最大方差方向）就是线条走向，不需要再转 90°。
                        let th = 0.5 * (2.0 * sxy).atan2(sxx - syy);
                        let mut d = (th.cos(), th.sin());
                        if d.0 * dx + d.1 * dy < 0.0 {
                            d = (-d.0, -d.1);
                        }
                        let mut c = (d.0 * dx + d.1 * dy) / l;
                        c = c.clamp(-1.0, 1.0);
                        dirs.push(c.abs().acos().to_degrees());
                    }
                }
            }
            let pct = |a: usize, b: usize| {
                if b == 0 {
                    100.0
                } else {
                    a as f64 * 100.0 / b as f64
                }
            };
            let med = |mut v: Vec<f64>| -> f64 {
                if v.is_empty() {
                    return 0.0;
                }
                v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                v[v.len() / 2]
            };
            format!(
                "召回 1px/{:.0}% 2px/{:.0}% 3px/{:.0}%（深线 2px/{:.0}%）；落笔压墨 2px/{:.0}% 3px/{:.0}%；\
                 直线度中位 {:.2}px；方向差中位 {:.1}°",
                pct(c1, ink_px),
                pct(c2, ink_px),
                pct(c3, ink_px),
                pct(dark_c2, dark_px),
                pct(on_ink, drawn),
                pct(on_ink3, drawn),
                med(straight),
                med(dirs)
            )
        };
        // 明暗保真：素描的明暗是「这一块有多少墨」，不是「这里有没有线」。
        // 只盯召回率会奖励「全都画」，必须同时看三档调子的墨量比。
        // 换算必须按每根笔迹自己的线宽与深度：多笔模式下拿统一的 0.5mm
        // 去算，会把 8B 的 1mm 黑线当成 0.5mm 中灰，暗部永远显得「刚好」。
        let single_pen = opts.sketch_pen.clamp(0.15, 1.6);
        let tone = crate::sketch::tone_map(
            &gray,
            w,
            h,
            opts.invert,
            opts.threshold,
            opts.target_h_mm / h as f64,
        );
        let ncell = tone.cw * tone.ch;
        let mut out_ink = vec![0.0f64; ncell];
        for (k, pl) in r.polylines.iter().enumerate() {
            let (w_mm, depth) = if multi {
                let tag = (r.pens.get(k).copied().unwrap_or(0) as usize).min(pens.len() - 1);
                (pens[tag].width_mm, pens[tag].darkness as f64)
            } else {
                (single_pen, 1.0)
            };
            for seg in pl.pts.windows(2) {
                let (a, b) = (seg[0], seg[1]);
                let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
                let cx = (((a.0 + b.0) * 0.5) / tone.cell_mm) as isize;
                let cy = (((a.1 + b.1) * 0.5) / tone.cell_mm) as isize;
                if cx < 0 || cy < 0 || cx >= tone.cw as isize || cy >= tone.ch as isize {
                    continue;
                }
                out_ink[cy as usize * tone.cw + cx as usize] += len * w_mm * depth;
            }
        }
        let area = tone.cell_mm * tone.cell_mm;
        let (mut sx, mut sy, mut sxx, mut syy, mut sxy) = (0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64);
        // 三档：亮 / 中间调 / 暗。看的是「这一档实际画了多少墨 / 原稿有多少」。
        let mut band = [(0.0f64, 0.0f64); 3];
        for i in 0..ncell {
            let src = tone.cells[i] as f64 * area;
            let o = out_ink[i];
            sx += src;
            sy += o;
            sxx += src * src;
            syy += o * o;
            sxy += src * o;
            let k = if tone.cells[i] < 0.25 {
                0
            } else if tone.cells[i] < 0.6 {
                1
            } else {
                2
            };
            band[k].0 += src;
            band[k].1 += o;
        }
        let n = ncell as f64;
        let cov_r = {
            let num = sxy - sx * sy / n;
            let den = ((sxx - sx * sx / n) * (syy - sy * sy / n)).sqrt();
            if den > 1e-9 {
                num / den
            } else {
                0.0
            }
        };
        let ratio = |b: (f64, f64)| if b.0 > 1e-6 { b.1 / b.0 } else { 0.0 };
        if std::env::var("SKETCH_DEBUG").is_ok() {
            let mut t = tone.cells.clone();
            t.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let q = |f: f64| t[(((t.len() - 1) as f64) * f) as usize];
            eprintln!(
                "  调子 {}x{} 格 {:.1}mm；p10/{:.2} p50/{:.2} p90/{:.2} max/{:.2}",
                tone.cw,
                tone.ch,
                tone.cell_mm,
                q(0.10),
                q(0.50),
                q(0.90),
                t[t.len() - 1]
            );
            for k in 0..3 {
                let cells = (0..ncell)
                    .filter(|&i| {
                        let v = tone.cells[i];
                        if k == 0 {
                            v < 0.25
                        } else if k == 1 {
                            v >= 0.25 && v < 0.6
                        } else {
                            v >= 0.6
                        }
                    })
                    .count();
                let hit = (0..ncell)
                    .filter(|&i| {
                        let v = tone.cells[i];
                        let inb = if k == 0 {
                            v < 0.25
                        } else if k == 1 {
                            v >= 0.25 && v < 0.6
                        } else {
                            v >= 0.6
                        };
                        inb && out_ink[i] > 1e-6
                    })
                    .count();
                eprintln!(
                    "  档{}（{}）：格 {} 有笔迹 {}；原稿 {:.0} 输出 {:.0} 比 {:.2}",
                    k,
                    ["亮", "中", "暗"][k],
                    cells,
                    hit,
                    band[k].0,
                    band[k].1,
                    ratio(band[k])
                );
            }
        }
        if std::env::var("SKETCH_DEBUG").is_ok() {
            let tr = crate::sketch::trace_strokes(
                &gray,
                w,
                h,
                opts.target_h_mm / h as f64,
                opts.threshold,
                opts.invert,
            );
            let mut v: Vec<&crate::sketch::Traced> = tr.iter().collect();
            v.sort_by(|a, b| b.len_mm.partial_cmp(&a.len_mm).unwrap());
            eprintln!("  最长 6 条（长度/带宽/墨色）：");
            for t in v.iter().take(6) {
                eprintln!(
                    "    {:.1}mm  {:.2}mm  {:.2}",
                    t.len_mm,
                    t.width_px * opts.target_h_mm / h as f64,
                    t.tone
                );
            }
            let mut wv: Vec<f64> = tr
                .iter()
                .map(|t| t.width_px * opts.target_h_mm / h as f64)
                .collect();
            wv.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let wq = |f: f64| wv[(((wv.len() - 1) as f64) * f) as usize];
            eprintln!(
                "  带宽分位 p25/{:.2} p50/{:.2} p75/{:.2} p95/{:.2}mm",
                wq(0.25),
                wq(0.50),
                wq(0.75),
                wq(0.95)
            );
        }
        let tone_stats = format!(
            "；明暗比 总/{:.2} 亮/{:.2} 中/{:.2} 暗/{:.2} 相关r {:.2}",
            if sx > 1e-6 { sy / sx } else { 0.0 },
            ratio(band[0]),
            ratio(band[1]),
            ratio(band[2]),
            cov_r
        );
        // SKETCH_MODE=tone：三行「原稿调子 / 输出墨量 / 差值」，肉眼核对明暗结构。
        if std::env::var("SKETCH_MODE").ok().as_deref() == Some("tone") {
            let s: u32 = 16;
            let (tw, th) = (tone.cw as u32 * s, tone.ch as u32 * s);
            let mut timg = image::RgbImage::from_pixel(tw, th * 3, image::Rgb([255, 255, 255]));
            for cy in 0..tone.ch {
                for cx in 0..tone.cw {
                    let i = cy * tone.cw + cx;
                    let src = tone.cells[i] as f64;
                    let o = (out_ink[i] / area).clamp(0.0, 1.0);
                    let diff = (o - src).clamp(-1.0, 1.0);
                    let paint = |img: &mut image::RgbImage, row: u32, rgb: [u8; 3]| {
                        for y in 0..s {
                            for x in 0..s {
                                img.put_pixel(
                                    cx as u32 * s + x,
                                    row + cy as u32 * s + y,
                                    image::Rgb(rgb),
                                );
                            }
                        }
                    };
                    let g = |v: f64| (255.0 - v.clamp(0.0, 1.0) * 255.0) as u8;
                    paint(&mut timg, 0, [g(src), g(src), g(src)]);
                    paint(&mut timg, th, [g(o), g(o), g(o)]);
                    // 蓝 = 画少了，红 = 画多了
                    let d = if diff < 0.0 {
                        [80, 80, (80.0 + -diff * 175.0) as u8]
                    } else {
                        [(80.0 + diff * 175.0) as u8, 80, 80]
                    };
                    paint(&mut timg, th * 2, d);
                }
            }
            let p = root.join("sketch_tone.png");
            timg.save(&p).ok();
            println!("调子图 → {}", p.display());
        }
        let cov = format!("{stats}{tone_stats}");
        let name = std::env::var("SKETCH_OUT").unwrap_or_else(|_| "sketch_verify.png".into());
        let out = root.join(name);
        image::save_buffer(&out, &buf, ow * 2, oh, image::ColorType::Rgba8).unwrap();
        println!(
            "笔迹 {} 条，总长 {:.0}mm，空移 {:.0}mm；{cov} → {}",
            r.polylines.len(),
            r.polylines.iter().map(|p| p.length()).sum::<f64>(),
            wrobot_core::render::path_travel(&r.polylines, (0.0, 0.0)),
            out.display()
        );
    }
}
