//! WritingRobot 图形界面入口。

mod device;
mod fontmgr;
mod io;
mod preview;
mod state;
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
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        persist_window: false,
        centered: true,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1600.0, 1000.0])
            .with_min_inner_size([1180.0, 720.0])
            .with_maximized(true)
            .with_app_id("wrobot-gui")
            .with_title("WritingRobot · 写字机器人上位机"),
        ..Default::default()
    };
    eframe::run_native(
        "WritingRobot · 写字机器人上位机",
        options,
        Box::new(move |cc| {
            setup_fonts(&cc.egui_ctx);
            let mut app = MyApp::new();
            app.state.tab = 6; // 直接切到「设备」页
            // 启动时扫一次端口（只 CreateFile 试探，不重试，几十毫秒内完成），
            // 这样设备页一打开就能看到每个口的在线状态，而不是空列表要手动点扫描。
            app.state.scan_ports();
            if let Some(p) = &port {
                app.state.serial_port = p.clone();
                app.state.serial_baud = baud;
                app.state.connect_device();
            } else if auto {
                app.state.auto_connect();
            } else if app.state.auto_connect {
                // 参考项目 auto_connect=true：启动就自己找机器，不用每次手动点
                app.state.auto_connect();
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
            background: false,
            out: true,
            base: Vec::new(),
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
            background: false,
            out: true,
            base: vec![wrobot_core::Polyline::new(vec![(0.0, 0.0), (10.0, 10.0)])],
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
            },
        );
        assert_eq!(
            r.polylines.len(),
            2,
            "左右两块应各自串成一条笔画，实际 {} 条",
            r.polylines.len()
        );
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
            background: false,
            out: true,
            base: vec![wrobot_core::Polyline::new(vec![(0.0, 0.0), (10.0, 10.0)])],
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
}
