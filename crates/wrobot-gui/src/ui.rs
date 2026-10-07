//! 图形界面布局与交互。
//!
//! 结构：左侧「模块导航」+ 中央「页面预览」+ 右侧「参数面板」。

use eframe::egui;
use egui::{ComboBox, RichText, ScrollArea, TextEdit, Ui};
use wrobot_core::render::Polyline;
use wrobot_core::StrokeFont;

use crate::io;
use crate::preview::{draw_canvas, draw_progress_page};
use crate::state::{AppState, GridKind, ImgDraw, PAPER_CUSTOM, PAPER_PRESETS};

const MODULES: &[&str] = &[
    "文字",
    "表格",
    "格子",
    "绘图",
    "笔记",
    "图片",
    "设备",
    "测试",
    "导入导出",
    "运行中",
];
const CONTENT_MODULES: &[&str] = &["文字", "表格", "格子", "绘图", "笔记"];
const ORIGINS: &[&str] = &["左上", "右上", "左下", "右下"];
const BORDER_STYLES: &[&str] = &["实线", "虚线", "双线"];

/// 强调色（蓝灰调，适配明暗两种主题）。
fn accent() -> egui::Color32 {
    egui::Color32::from_rgb(58, 122, 194)
}

/// 次级文字色。
fn muted() -> egui::Color32 {
    egui::Color32::from_gray(140)
}

/// 分区标题。
fn section(ui: &mut Ui, title: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(title).size(14.0).strong().color(accent()));
    ui.separator();
}

/// 顶层界面。
/// 顶部菜单栏：把最常用的功能都放到这里（文件 / 导出 / 设备 / 运行 / 视图）。
fn menu_bar(app: &mut AppState, ui: &mut Ui) {
    egui::MenuBar::new().ui(ui, |ui| {
        // ── 文件 ──
        ui.menu_button("文件", |ui| {
            if ui.button("新建（清空全部内容）").clicked() {
                app.new_project();
                app.project_path = None;
                app.status = "已新建（清空全部内容）".into();
                ui.close();
            }
            if ui.button("打开工程…").clicked() {
                act_open_project(app);
                ui.close();
            }
            if ui
                .button(label_key("保存工程", "Ctrl+S"))
                .clicked()
            {
                act_save_project(app);
                ui.close();
            }
            if ui.button("工程另存为…").clicked() {
                act_save_project_as(app);
                ui.close();
            }
            ui.separator();
            if ui.button("导入字库(.gfont)…").clicked() {
                act_import_font(app);
                ui.close();
            }
            if ui
                .button("导入 PDF 页面…")
                .on_hover_text("选 PDF → 挑页码 → 1:1 原始尺寸导入（矢量描边 / 文字用手写体重排）")
                .clicked()
            {
                if let Some(p) = app.pick_file("PDF", &["pdf"]) {
                    match app.load_pdf_info(&p) {
                        Ok(n) => {
                            app.status = format!("已读取 {} 页，请在「导入导出」页选页码后导入", n);
                            app.tab = 8; // 直接跳到导入导出页
                        }
                        Err(e) => app.status = format!("读取 PDF 失败：{e}"),
                    }
                }
                ui.close();
            }
            if ui.button("重新扫描字库目录").clicked() {
                let reloaded = crate::fontmgr::FontManager::load(std::path::Path::new(&app.font_dir));
                let n = reloaded.fonts.len();
                app.fonts = reloaded;
                app.status = format!("已重新扫描：{} 个字库", n);
                ui.close();
            }
        });

        // ── 导出 ──
        ui.menu_button("导出", |ui| {
            if ui.button("导出 G-code (.nc)…").clicked() {
                act_export(app, Export::Nc);
                ui.close();
            }
            if ui.button("导出 SVG…").clicked() {
                act_export(app, Export::Svg);
                ui.close();
            }
            if ui.button("导出 PDF…").clicked() {
                act_export(app, Export::Pdf);
                ui.close();
            }
            if ui.button("导出 PNG…").clicked() {
                act_export(app, Export::Png);
                ui.close();
            }
        });

        // ── 设备 ──
        ui.menu_button("设备", |ui| {
            let connected = app.dev_connected;
            if ui
                .add_enabled(!connected, egui::Button::new("连接"))
                .clicked()
            {
                app.connect_device();
                ui.close();
            }
            if ui.add_enabled(connected, egui::Button::new("断开")).clicked() {
                app.disconnect_device();
                ui.close();
            }
            if ui.button("自动连接").clicked() {
                app.auto_connect();
                ui.close();
            }
            if ui.button("扫描串口").clicked() {
                let n = app.scan_ports();
                app.status = format!("已扫描：{} 个在线端口", n);
                ui.close();
            }
            if ui.button("自动波特率").on_hover_text("连上但机器没反应时用它：逐个波特率握手，找出机器真正用的那个").clicked() {
                app.auto_baud();
                ui.close();
            }
            if ui.button("诊断端口").clicked() {
                app.diagnose_ports();
                ui.close();
            }
            ui.separator();
            ui.label(RichText::new("连接方式").size(11.0).color(muted()));
            let names = ["串口 / 蓝牙", "TCP", "模拟（不下发，仅回显）"];
            for (i, nm) in names.iter().enumerate() {
                if ui.radio_value(&mut app.dev_kind, i, *nm).changed() {
                    app.status = if i == 2 {
                        "已切到模拟模式：点「设备 → 连接」启动（G-code 会回显到日志，机器不会动）".into()
                    } else {
                        format!("连接方式已切到「{}」，点「连接」生效", nm)
                    };
                }
            }
        });

        // ── 运行 ──
        ui.menu_button("运行", |ui| {
            let snap = app.device.snapshot();
            if ui
                .add_enabled(!snap.running, egui::Button::new("开始绘制"))
                .clicked()
            {
                act_start_draw(app);
                ui.close();
            }
            if snap.running && !snap.paused {
                if ui.button("暂停").clicked() {
                    app.pause_run();
                    ui.close();
                }
            } else if snap.paused {
                if ui.button("继续").clicked() {
                    app.resume_run();
                    ui.close();
                }
            }
            if ui.add_enabled(snap.running, egui::Button::new("停止")).clicked() {
                app.stop_run();
                ui.close();
            }
            ui.separator();
            if ui.button("回原点 (0,0)").clicked() {
                app.go_zero();
                ui.close();
            }
            if ui.button("解锁 $X").clicked() {
                app.unlock_machine();
                ui.close();
            }
            if ui.button("机械回零 $H").clicked() {
                app.home_machine();
                ui.close();
            }
        });

        // ── 视图 ──
        ui.menu_button("视图", |ui| {
            ui.checkbox(&mut app.log_open, "底部实时日志栏");
            ui.separator();
            ui.label(RichText::new("界面缩放").size(11.0).color(muted()));
            ui.add(egui::Slider::new(&mut app.ui_scale, 0.7..=2.0).text("×"));
            if ui.button("恢复 1.0×").clicked() {
                app.ui_scale = 1.0;
            }
            ui.separator();
            if ui.button("设置…").clicked() {
                app.settings_open = true;
                ui.close();
            }
        });

        // 右侧常驻状态
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let (dot, col, txt) = if app.dev_connected {
                if app.dev_kind == 2 {
                    ("●", egui::Color32::from_rgb(90, 150, 210), "模拟模式")
                } else {
                    ("●", egui::Color32::from_rgb(60, 170, 90), "已连接")
                }
            } else {
                ("○", egui::Color32::from_gray(150), "未连接")
            };
            ui.colored_label(col, RichText::new(format!("{} {}", dot, txt)).size(12.0));
            if app.dev_connected && app.dev_kind == 0 {
                ui.weak(format!("{} @ {}", app.serial_port, app.serial_baud));
            }
            if let Some(p) = &app.project_path.clone() {
                ui.weak(p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default());
            }
        });
    });
}

/// 菜单项标签（右侧带快捷键提示）。
fn label_key(text: &str, key: &str) -> String {
    format!("{}\t{}", text, key)
}

/// 导出格式。
enum Export {
    Nc,
    Svg,
    Pdf,
    Png,
}

/// 开始绘制（顶栏与菜单共用）。
fn act_start_draw(app: &mut AppState) {
    let g = app.generate_gcode();
    if g.trim().is_empty() {
        app.status = "当前页面没有可绘制的内容".into();
        return;
    }
    app.gcode_text = g.clone();
    let n = g.lines().filter(|l| !l.trim().is_empty()).count();
    app.start_run(g);
    app.status = format!("已开始传输 {} 行（进度见「运行中」页，传输结果见左下角）", n);
}

/// 保存工程：有路径就直接覆盖，没有就弹另存为。
fn act_save_project(app: &mut AppState) {
    match app.project_path.clone() {
        Some(p) => match io::save_project_full(app, &p) {
            Ok(()) => app.status = format!("工程已保存：{}", p.display()),
            Err(e) => app.status = format!("保存失败：{e}"),
        },
        None => act_save_project_as(app),
    }
}

fn act_save_project_as(app: &mut AppState) {
    if let Some(p) = app.save_file_as("工程", &["wprj", "json"], "我的工程.wprj") {
        match io::save_project_full(app, &p) {
            Ok(()) => {
                app.project_path = Some(p.clone());
                app.status = format!("完整工程已保存：{}", p.display());
            }
            Err(e) => app.status = format!("保存失败：{e}"),
        }
    }
}

fn act_open_project(app: &mut AppState) {
    if let Some(p) = app.pick_file("工程", &["wprj", "json"]) {
        match io::load_project_full(app, &p) {
            Ok(()) => {
                app.project_path = Some(p.clone());
                app.status = format!("完整工程已载入：{}", p.display());
            }
            Err(e) => app.status = format!("载入失败：{e}"),
        }
    }
}

fn act_import_font(app: &mut AppState) {
    if let Some(p) = app.pick_file("字库", &["gfont"]) {
        match wrobot_core::font::GfontFile::open(&p) {
            Ok(f) => {
                let n = f.name().to_string();
                app.fonts.fonts.push(f);
                app.status = format!("已导入字库：{}", n);
            }
            Err(e) => app.status = format!("字库导入失败：{e}"),
        }
    }
}

fn act_export(app: &mut AppState, kind: Export) {
    let (name, exts, file) = match kind {
        Export::Nc => ("G-code", &["nc", "gcode"][..], "output.nc"),
        Export::Svg => ("SVG", &["svg"][..], "output.svg"),
        Export::Pdf => ("PDF", &["pdf"][..], "output.pdf"),
        Export::Png => ("PNG", &["png"][..], "output.png"),
    };
    if let Some(p) = app.save_file_as(name, exts, file) {
        let r = match kind {
            Export::Nc => io::export_gcode(app, &p),
            Export::Svg => io::export_svg(app, &p),
            Export::Pdf => io::export_pdf(app, &p),
            Export::Png => io::export_png(app, &p),
        };
        match r {
            Ok(()) => app.status = format!("{} 已导出：{}", name, p.display()),
            Err(e) => app.status = format!("导出失败：{e}"),
        }
    }
}

/// 底部「实时日志」栏：所有页面都能看到设备通信与操作回显。
fn log_panel(app: &mut AppState, ui: &mut Ui) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("实时日志").strong());
        ui.weak("（设备通信 / 下发回显 / 错误诊断都在这里）");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("清空").clicked() {
                app.device.clear_log();
                app.dev_log.clear();
            }
            if ui.button("隐藏").clicked() {
                app.log_open = false;
            }
            let snap = app.device.snapshot();
            if snap.running {
                ui.colored_label(
                    egui::Color32::from_rgb(60, 170, 90),
                    format!("运行中 {}/{}", snap.done_lines, snap.total_lines),
                );
            }
        });
    });
    // 只显示尾部，避免几万行时每帧全量重排
    let tail: String = if app.dev_log.len() > 6000 {
        let skip = app.dev_log.len() - 6000;
        match app.dev_log.char_indices().nth(skip) {
            Some((i, _)) => format!("…（已省略开头）\n{}", &app.dev_log[i..]),
            None => app.dev_log.clone(),
        }
    } else {
        app.dev_log.clone()
    };
    let mut text = tail;
    ScrollArea::vertical()
        .stick_to_bottom(true)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add(
                TextEdit::multiline(&mut text)
                    .desired_width(f32::MAX)
                    .desired_rows(6)
                    .interactive(false)
                    .font(egui::TextStyle::Monospace),
            );
        });
}

pub fn show(app: &mut AppState, ui: &mut Ui, _frame: &mut eframe::Frame) {
    // 界面缩放：在显示器原生缩放之上再乘用户设定的倍率（变化时才真正下发，避免每帧重排）。
    let native = ui.ctx().native_pixels_per_point().unwrap_or(1.0);
    let want = (native * app.ui_scale.clamp(0.6, 3.0)).clamp(0.5, 4.0);
    if (ui.ctx().pixels_per_point() - want).abs() > 0.01 {
        ui.ctx().set_pixels_per_point(want);
    }

    // 启动时打印一次画面尺寸，便于排查「窗口太小 / 显示不全」这类问题。
    static LOG_ONCE: std::sync::Once = std::sync::Once::new();
    LOG_ONCE.call_once(|| {
        let (w, h) = ui
            .ctx()
            .input(|i| i.raw.screen_rect)
            .map(|r| (r.width().round() as i32, r.height().round() as i32))
            .unwrap_or((0, 0));
        eprintln!(
            "[窗口] 画面 {}×{} 点 · 缩放 {}×",
            w,
            h,
            ui.ctx().pixels_per_point()
        );
    });

    // 快捷键：Ctrl+S 保存工程 / Ctrl+O 打开工程 / Ctrl+E 导出 G-code
    let (mut k_save, mut k_open, mut k_export) = (false, false, false);
    ui.ctx().input_mut(|i| {
        k_save = i.consume_key(egui::Modifiers::CTRL, egui::Key::S);
        k_open = i.consume_key(egui::Modifiers::CTRL, egui::Key::O);
        k_export = i.consume_key(egui::Modifiers::CTRL, egui::Key::E);
    });
    if k_save {
        act_save_project(app);
    }
    if k_open {
        act_open_project(app);
    }
    if k_export {
        act_export(app, Export::Nc);
    }

    app.rebuild_preview();
    // 页面尺寸可能被改过（PDF 导入 / 滑块 / 载入旧工程），规格名跟着重判一次。
    app.sync_paper();

    // 记住最近停留的内容模块：离开内容页后（如切到「设备」），
    // 页面仍只显示这一个模块，避免五个模块同时渲染互相重叠。
    if app.tab < CONTENT_MODULES.len() {
        app.last_content_tab = app.tab;
    }

    // 每帧从后台设备线程同步连接状态与日志（非阻塞，只读共享状态）。
    app.dev_log = app.device.log_snapshot();
    app.dev_connected = app.device.is_connected();
    // 处理设备线程请求切换 tab（例如点「运行」后自动跳到「运行中」页）。
    if let Some(t) = app.device.take_request_tab() {
        app.tab = t;
    }
    // 绑定蓝牙串口后系统会新建 COM 口，设备线程会请求重新扫描一次。
    if app.device.take_request_scan() {
        app.scan_ports();
    }
    // 同步蓝牙体检 / 可见性观察报告（后台跑完才填，避免 UI 阻塞）。
    let r = app.device.bt_report_snapshot();
    if !r.is_empty() {
        app.bt_report = r;
    }
    // 同步最近一次传输结果（「代码传过去了没有」要有明确回话）
    let ts = app.device.snapshot().transfer_status;
    if !ts.is_empty() {
        app.transfer_status = ts;
    }
    // 坐标轮询开关/固件变了才下发，避免每帧打扰设备线程
    app.apply_status_poll();

    // ── 顶部：菜单栏（最常用的功能都在这里）──────
    egui::Panel::top("menubar").show(ui, |ui| {
        menu_bar(app, ui);
    });

    // ── 底部：实时日志栏（可折叠 / 可拖动调高）────
    // 必须在 CentralPanel 之前注册，中央面板才会给它让出空间。
    let mut log_open = app.log_open;
    egui::Panel::bottom("logbar")
        .resizable(true)
        .default_size(180.0)
        .min_size(60.0)
        .max_size(460.0)
        .show_collapsible(ui, &mut log_open, |ui| {
            log_panel(app, ui);
        });
    app.log_open = log_open;

    // ── 左侧：模块导航 ─────────────────────────────
    // 可拖动调宽（坐标区是等宽数字，窄了会换行），高度不够时整块可滚动，
    // 否则小窗口下「坐标」区会被挤到看不见。
    egui::Panel::left("modules")
        .resizable(true)
        .default_size(196.0)
        .min_size(150.0)
        .max_size(340.0)
        .show(ui, |ui| {
            ScrollArea::vertical()
                .id_salt("left_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("WritingRobot").size(20.0).strong());
                ui.label(RichText::new("写字机器人上位机").size(11.0).color(muted()));
            });
            ui.separator();

            ui.label(RichText::new("功能模块").strong().color(muted()));
            for (i, name) in MODULES.iter().enumerate() {
                let label = RichText::new(format!("  {}", name)).size(14.0);
                let is_cur = app.tab == i;
                let r = ui.selectable_value(&mut app.tab, i, label);
                if is_cur {
                    // 当前项高亮描边（用 painter 画左侧指示条）
                    let rect = r.rect;
                    ui.painter().rect_filled(
                        egui::Rect::from_min_size(
                            rect.min,
                            egui::Vec2::new(3.0, rect.height()),
                        ),
                        1.5,
                        accent(),
                    );
                }
                if r.clicked() {
                    app.status = format!("切换到「{}」模块", name);
                    app.confirm_clear = false;
                }
            }

            ui.separator();
            ui.label(RichText::new("并入当前页面").strong().color(muted()));
            if app.tab < CONTENT_MODULES.len() {
                ui.label(RichText::new("默认只输出当前模块，勾选可叠加").size(10.0).color(muted()));
            } else {
                let cur = CONTENT_MODULES
                    .get(app.last_content_tab)
                    .copied()
                    .unwrap_or("文字");
                ui.label(
                    RichText::new(format!("非内容页：只显示「{}」，勾选可叠加", cur))
                        .size(10.0)
                        .color(muted()),
                );
            }
            for (i, name) in CONTENT_MODULES.iter().enumerate() {
                ui.checkbox(&mut app.module_on[i], *name);
            }
            // 图片是独立导入的元素，此前不受任何开关控制、总被叠加进来。
            // 这里给它一个显式勾选，语义与其它模块一致。
            ui.checkbox(&mut app.images_on, "图片");
            if !app.images.is_empty() && !app.images_active() {
                ui.label(
                    RichText::new("图片未勾选：不写入 G-code（画布上仍可编辑定位）")
                        .size(10.0)
                        .color(muted()),
                );
            }

            ui.separator();
            if app.fonts.is_empty() {
                ui.colored_label(egui::Color32::from_rgb(200, 60, 60), "未加载到字库！");
            } else {
                ui.label(
                    RichText::new(format!("已加载 {} 个单线字库", app.fonts.fonts.len()))
                        .size(12.0),
                );
                ui.label(RichText::new(&app.font_dir).size(10.0).color(muted()));
            }

            // ── 坐标（字库下面、实时日志上面）──────────────────
            coord_panel(app, ui);
                });

            // 状态行固定在底部（不随上面的内容滚动）
            ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                ui.label(RichText::new(&app.status).size(11.0).color(muted()));
                // 传输结果单独一行：让用户明确知道「代码到底发出去了没有」
                if !app.transfer_status.is_empty() {
                    let col = if app.transfer_status.starts_with('✓') {
                        egui::Color32::from_rgb(60, 170, 90)
                    } else {
                        egui::Color32::from_rgb(210, 90, 60)
                    };
                    ui.label(RichText::new(&app.transfer_status).size(11.0).color(col));
                }
                ui.separator();
            });
        });

    // ── 右侧：参数 ────────────────────────────────
    egui::Panel::right("params")
        .resizable(true)
        .show(ui, |ui| {
            ui.set_min_width(310.0);
            ScrollArea::vertical().show(ui, |ui| {
                match app.tab {
                    0 => panel_text(app, ui),
                    1 => panel_table(app, ui),
                    2 => panel_grid(app, ui),
                    3 => panel_draw(app, ui),
                    4 => panel_notes(app, ui),
                    5 => panel_images(app, ui),
                    6 => panel_device(app, ui),
                    7 => panel_test(app, ui),
                    8 => panel_io(app, ui),
                    9 => panel_running(app, ui),
                    _ => {}
                }
                // 公共区段（页面边框 / 固件 / 导出）：内容页与导入导出页直接铺开，
                // 设备 / 测试 / 运行中页本身已经很长，折叠起来免得把本页参数挤到看不见。
                if matches!(app.tab, 0..=5 | 8) {
                    panel_border(app, ui);
                    ui.separator();
                    panel_export(app, ui);
                } else {
                    egui::CollapsingHeader::new("页面边框 / 固件 / 生成导出")
                        .id_salt("common_tail")
                        .default_open(false)
                        .show(ui, |ui| {
                            panel_border(app, ui);
                            ui.separator();
                            panel_export(app, ui);
                        });
                }
            });
        });

    // 设置对话框（收纳不常用设置）
    settings_window(app, ui.ctx().clone());

    // ── 中央：预览 / 运行进度 ─────────────────────
    egui::CentralPanel::default().show(ui, |ui| {
        if app.tab == 9 {
            // 运行中页：显示冻结的整页进度（已完成=黑，未完成=浅灰）。
            let snap = app.device.snapshot();
            let frac = if snap.total_lines > 0 {
                snap.done_lines as f64 / snap.total_lines as f64
            } else {
                0.0
            };
            draw_progress_page(ui, &app.run_preview, frac, app.page_w_mm, app.page_h_mm);
        } else {
            // 标题条 + 纸张 + 运行控制条
            ui.horizontal_wrapped(|ui| {
                let name = MODULES.get(app.tab).copied().unwrap_or("预览");
                ui.label(RichText::new(name).size(17.0).strong());
                ui.label(
                    RichText::new(format!("{} 条笔迹", app.preview.len()))
                        .size(11.0)
                        .color(muted()),
                );
                ui.separator();
                ui.label(RichText::new("页面").size(11.0).color(muted()));
                paper_row(app, ui);
            });
            ui.separator();
            run_control_bar(app, ui);
            ui.separator();

            let freehand = app.tab == 3 && app.freehand_on;
            let image_tab = app.tab == 5;
            // 手绘临时点集用局部变量，避免与整块 `app` 的可变借用冲突。
            let mut acc = std::mem::take(&mut app.freehand);
            let (_map, committed) = draw_canvas(ui, app, freehand, image_tab, &mut acc);
            if committed && acc.len() >= 2 {
                let pts = std::mem::take(&mut acc);
                app.draw_paths.push(Polyline::new(pts));
                app.status = format!("已添加手绘笔迹（共 {} 条）", app.draw_paths.len());
            }
            app.freehand = acc;
        }
    });
}

/// 左栏「坐标」区：机器物理坐标（MPos）/ 当前工件坐标（WPos）/ 页面坐标 → 机器坐标。
///
/// 放在字库下面、实时日志上面：字库是「静态信息」，日志在最底部，
/// 坐标是运行中唯一想一直盯着的东西，插在两者之间最顺眼。
fn coord_panel(app: &mut AppState, ui: &mut Ui) {
    ui.separator();
    ui.label(RichText::new("坐标").strong().color(muted()));

    let snap = app.device.snapshot();
    let p = snap.pos;
    let line = |v: Option<[f64; 3]>| match v {
        Some(a) => format!("X {:>8.2}   Y {:>8.2}   Z {:>7.2}", a[0], a[1], a[2]),
        None => "X        —    Y        —    Z       —".to_string(),
    };
    egui::Grid::new("coord_grid")
        .num_columns(2)
        .spacing([6.0, 2.0])
        .show(ui, |ui| {
            ui.label(RichText::new("机器物理").size(10.0).color(muted()));
            ui.label(
                RichText::new(line(p.m))
                    .size(11.5)
                    .monospace()
                    .color(egui::Color32::from_rgb(40, 110, 240)),
            );
            ui.end_row();
            ui.label(RichText::new("当前坐标").size(10.0).color(muted()));
            ui.label(
                RichText::new(line(p.w))
                    .size(11.5)
                    .monospace()
                    .color(egui::Color32::from_rgb(40, 170, 90)),
            );
            ui.end_row();
        });

    // 鼠标所在页面点 → 机器坐标：反转/对调调得对不对，看这一行最直接
    if let Some((x, y)) = app.cursor_mm {
        let (mx, my) = app.gcode_options().transform(x, y);
        ui.label(
            RichText::new(format!(
                "页面 ({:.1}, {:.1}) → 机器 ({:.2}, {:.2})",
                x, y, mx, my
            ))
            .size(10.5)
            .color(muted()),
        );
    }

    ui.horizontal_wrapped(|ui| {
        if ui.small_button("刷新").on_hover_text("发一次 ? / M114 查询当前位置").clicked() {
            app.read_position();
        }
        let mut poll = app.status_poll_ms > 0;
        if ui
            .checkbox(&mut poll, "自动")
            .on_hover_text("每秒自动查询一次坐标（GRBL 不主动推送，只能定时问）")
            .changed()
        {
            app.status_poll_ms = if poll { 1000 } else { 0 };
        }
    });
    if !app.dev_connected {
        ui.label(RichText::new("未连接：连上机器后才有坐标").size(10.0).color(muted()));
    } else if !p.any() {
        ui.label(RichText::new("暂无坐标回报，点「刷新」试一次").size(10.0).color(muted()));
    }
    ui.weak("「机器物理」是相对机械原点，「当前坐标」受 G92 设零点影响。");
}

/// 用系统默认方式打开设置页 / 设备管理器等（cmd start，仅 Windows）。
fn open_system(target: &str) {
    #[cfg(windows)]
    let _ = std::process::Command::new("cmd")
        .args(["/c", "start", "", target])
        .spawn();
    #[cfg(not(windows))]
    let _ = target;
}

/// 字体下拉框。
fn font_combo(ui: &mut Ui, label: &str, sel: &mut String, names: Vec<String>) {
    ComboBox::from_label(label)
        .selected_text(sel.clone())
        .show_ui(ui, |ui| {
            for n in names {
                ui.selectable_value(sel, n.clone(), &n);
            }
        });
}

/// usize 滑块。
fn usize_slider(ui: &mut Ui, label: &str, val: &mut usize, min: i32, max: i32) {
    let mut tmp = *val as i32;
    ui.add(egui::Slider::new(&mut tmp, min..=max).text(label));
    *val = tmp.max(0) as usize;
}

/// 纸张规格选择行：常规规格下拉 + 横/竖切换 + 自定义宽高。
///
/// 以前页面尺寸写死 175×255（哪个标准纸都不是），界面上还只有两个滑块，
/// 用户只能靠猜。这里给常用规格一键切换，尺寸不匹配任何规格时自动标「自定义」。
fn paper_row(app: &mut AppState, ui: &mut Ui) {
    let mut paper = app.paper.clone();
    let mut changed = false;
    egui::ComboBox::from_id_salt("paper_preset")
        .selected_text(app.paper_label())
        .width(168.0)
        .show_ui(ui, |ui| {
            for (nm, w, h) in PAPER_PRESETS {
                changed |= ui
                    .selectable_value(&mut paper, (*nm).to_string(), format!("{nm}  {w:.0}×{h:.0}"))
                    .clicked();
            }
            changed |= ui
                .selectable_value(&mut paper, PAPER_CUSTOM.to_string(), PAPER_CUSTOM)
                .clicked();
        });
    if changed {
        app.set_paper(&paper);
    }
    if ui
        .button(if app.page_w_mm > app.page_h_mm { "⇄ 横" } else { "⇄ 竖" })
        .on_hover_text("横向 / 纵向切换（宽高互换，内容不动）")
        .clicked()
    {
        app.toggle_orientation();
    }
    if app.paper == PAPER_CUSTOM {
        ui.add(egui::DragValue::new(&mut app.page_w_mm).range(20.0..=2000.0).speed(1.0).prefix("宽 "));
        ui.add(egui::DragValue::new(&mut app.page_h_mm).range(20.0..=2000.0).speed(1.0).prefix("高 "));
        ui.weak("mm");
    }
}

/// 顶部运行控制条：**开始绘制** / 暂停 / 继续 / 停止 / 回原点 / 设置 / 清空本页。
fn run_control_bar(app: &mut AppState, ui: &mut Ui) {
    let snap = app.device.snapshot();

    ui.horizontal_wrapped(|ui| {
        let (dot, col) = if app.dev_connected {
            ("●", egui::Color32::from_rgb(60, 170, 90))
        } else {
            ("○", egui::Color32::from_gray(150))
        };
        ui.colored_label(
            col,
            RichText::new(format!(
                "{} {}",
                dot,
                if app.dev_connected { "已连接" } else { "未连接" }
            ))
            .size(13.0),
        );

        // 主按钮：开始绘制
        let start = egui::Button::new(
            RichText::new("开始绘制").size(15.0).strong().color(egui::Color32::WHITE),
        )
        .fill(egui::Color32::from_rgb(46, 160, 90))
        .min_size(egui::Vec2::new(115.0, 30.0));
        if ui.add_enabled(!snap.running, start).clicked() {
            let g = app.generate_gcode();
            if g.trim().is_empty() {
                app.status = "当前页面没有可绘制的内容".into();
            } else {
                app.gcode_text = g.clone();
                app.start_run(g);
                app.status = "已开始绘制（进度见「运行中」页）".into();
            }
        }

        if snap.running && !snap.paused {
            if ui
                .add_sized([66.0, 30.0], egui::Button::new("暂停"))
                .clicked()
            {
                app.pause_run();
                app.status = "已暂停".into();
            }
        } else if snap.paused {
            if ui
                .add_sized([66.0, 30.0], egui::Button::new("继续"))
                .clicked()
            {
                app.resume_run();
                app.status = "已继续".into();
            }
        }

        if ui
            .add_enabled(snap.running, egui::Button::new("停止").min_size(egui::Vec2::new(60.0, 30.0)))
            .clicked()
        {
            app.stop_run();
            app.status = "已停止".into();
        }

        if ui.add_sized([78.0, 30.0], egui::Button::new("回原点")).clicked() {
            app.go_zero();
        }

        if ui.add_sized([60.0, 30.0], egui::Button::new("设置")).clicked() {
            app.settings_open = true;
        }

        // 非内容页清的是「全部内容」，且要二次确认，防止误删整页
        let on_content = app.tab < CONTENT_MODULES.len();
        let label = if on_content { "清空本页" } else { "清空内容" };
        let btn = if app.confirm_clear {
            egui::Button::new(RichText::new("确认清空？").color(egui::Color32::WHITE))
                .fill(egui::Color32::from_rgb(190, 70, 60))
        } else {
            egui::Button::new(label)
        };
        if ui.add_sized([96.0, 30.0], btn).clicked() {
            if app.confirm_clear {
                app.clear_current_module();
            } else {
                app.confirm_clear = true;
                app.status = "再点一次确认清空".into();
            }
        }

        if snap.running {
            ui.label(
                RichText::new(format!("第 {}/{} 行", snap.done_lines, snap.total_lines))
                    .size(12.0)
                    .color(muted()),
            );
        }
    });

    if !app.dev_connected {
        ui.colored_label(
            egui::Color32::from_rgb(200, 120, 40),
            "未连接设备：请到「设备」页连接，或把连接方式选为「模拟」先试跑流程。",
        );
    }
}

/// 设置对话框：收纳不常用设置（固件高级 / 坐标系 / 运行流控 / 逼真化 / 图片默认参数 / 机器幅面）。
fn settings_window(app: &mut AppState, ctx: egui::Context) {
    if !app.settings_open {
        return;
    }
    let mut open = app.settings_open;
    egui::Window::new("设置")
        .open(&mut open)
        .resizable(true)
        .default_size([430.0, 560.0])
        .show(&ctx, |ui| {
            ScrollArea::vertical().show(ui, |ui| {
                section(ui, "界面");
                ui.add(egui::Slider::new(&mut app.ui_scale, 0.7..=2.0).text("界面缩放(×)"));
                ui.weak("窗口里放不下内容 / 字太小时调它；改完立即生效。");
                if ui.button("恢复 1.0×").clicked() {
                    app.ui_scale = 1.0;
                }
                ui.checkbox(&mut app.axes_on, "画布显示 XY 方向箭头（蓝 X / 绿 Y）");
                ui.checkbox(&mut app.text_frame_on, "文字模块显示文本框虚框（不打印）");

                section(ui, "运行流控（机器不动时先看这里）");
                ui.checkbox(
                    &mut app.flow_unlock,
                    "运行前解锁并设 mm/绝对坐标（GRBL 上电为 Alarm 锁定态，不解锁则不动）",
                );
                ui.checkbox(&mut app.flow_wait_ok, "等待固件回 ok 再发下一行");
                let mut timeout = app.flow_ok_timeout as i32;
                ui.add_enabled(
                    app.flow_wait_ok,
                    egui::Slider::new(&mut timeout, 200..=5000).text("等待 ok 超时(ms)"),
                );
                app.flow_ok_timeout = timeout.max(0) as u64;
                if ui.button("应用流控设置").clicked() {
                    app.apply_flow_settings();
                    app.status = "流控设置已应用".into();
                }
                ui.weak("机器完全不动时：保持勾选解锁，并到「测试」页手动发一次 $X 看回显。");

                section(ui, "链路保活（蓝牙几分钟就断时看这里）");
                ui.checkbox(&mut app.auto_reconnect, "断线自动重连（蓝牙/USB 闪断时自动恢复）");
                let mut ka = (app.keepalive_ms / 1000) as i32;
                ui.add(egui::Slider::new(&mut ka, 0..=60).text("保活心跳间隔(秒，0=关闭)"));
                app.keepalive_ms = ka.max(0) as u64 * 1000;
                if ui.button("应用保活设置").clicked() {
                    app.apply_keepalive_settings();
                    app.status = if app.keepalive_ms == 0 {
                        "已关闭保活心跳".into()
                    } else {
                        format!("保活心跳已设为 {} 秒", app.keepalive_ms / 1000)
                    };
                }
                ui.weak("写字机的蓝牙模块空闲几分钟就会自己断链；定期发一个空行即可保持连接。");

                section(ui, "XY 最小步进（G-code 抽稀）");
                ui.add(
                    egui::Slider::new(&mut app.min_step_mm, 0.1..=1.0)
                        .text("最小步进(mm)")
                        .suffix(" mm"),
                );
                ui.weak(
                    "手写字库每段笔画只有百分之几毫米，逐点下发会让 G-code 又长、机器又慢。\
                     小于这个间距的中间点会被合并：0.1 最精细（慢），1.0 最粗（快）。默认 0.2。",
                );
                if ui.button("恢复默认 0.2").clicked() {
                    app.min_step_mm = 0.2;
                }

                section(ui, "机器幅面");
                ui.horizontal(|ui| {
                    ui.add(egui::Slider::new(&mut app.machine_w_mm, 50.0..=600.0).text("宽(mm)"));
                    ui.add(egui::Slider::new(&mut app.machine_h_mm, 50.0..=800.0).text("高(mm)"));
                });
                ui.weak("仅作越界提示，不影响绘制坐标。");

                section(ui, "高级（激光 / 休眠 / 延时）");
                ui.checkbox(&mut app.laser_on, "激光模式（M3/M5 代替落笔）");
                ui.add_enabled(
                    app.laser_on,
                    egui::Slider::new(&mut app.laser_power, 0.0..=1000.0).text("激光功率 S"),
                );
                ui.checkbox(&mut app.auto_sleep, "完成后自动休眠");
                ui.add(egui::Slider::new(&mut app.tool_on_delay, 0.0..=2.0).text("落笔后暂停(s)"));
                ui.add(egui::Slider::new(&mut app.tool_off_delay, 0.0..=2.0).text("抬笔前暂停(s)"));
                ui.collapsing("EBB 舵机", |ui| {
                    ui.add(egui::Slider::new(&mut app.ebb_servo_speed, 0.0..=32767.0).text("舵机速度"));
                    ui.add(egui::Slider::new(&mut app.ebb_max_pen, 0.0..=65535.0).text("最大舵机值"));
                });

                section(ui, "机器参数（与参考项目 default.properties 一致）");
                ui.add(egui::Slider::new(&mut app.feed_rate, 100.0..=30000.0).text("绘图速度(mm/min)"));
                ui.add(egui::Slider::new(&mut app.jog_speed, 100.0..=30000.0).text("点动速度(mm/min)"));
                ui.checkbox(&mut app.auto_set_zero, "运行前自动设原点（G92 X0 Y0）");
                ui.checkbox(&mut app.auto_connect, "启动后自动连接机器");
                ui.checkbox(&mut app.single_step, "单步下发（逐行发送并等待）");

                ui.collapsing("GRBL（给 zOn / zOff / zSpeed）", |ui| {
                    ui.add(egui::Slider::new(&mut app.pen_down, -20.0..=40.0).text("落笔 Z(mm)"));
                    ui.add(egui::Slider::new(&mut app.pen_up, -20.0..=40.0).text("抬笔 Z(mm)"));
                    ui.add(egui::Slider::new(&mut app.z_speed, 100.0..=30000.0).text("Z 轴速度(mm/min)"));
                });
                ui.collapsing("Marlin（给 marlin_pen_* / marlin_z_speed）", |ui| {
                    if app.pen_mode_sel == 1 {
                        ui.add(egui::Slider::new(&mut app.marlin_servo_down, 0.0..=180.0).text("落笔角度"));
                        ui.add(egui::Slider::new(&mut app.marlin_servo_up, 0.0..=180.0).text("抬笔角度"));
                    } else {
                        ui.add(egui::Slider::new(&mut app.marlin_pen_down, -20.0..=40.0).text("落笔 Z(mm)"));
                        ui.add(egui::Slider::new(&mut app.marlin_pen_up, -20.0..=40.0).text("抬笔 Z(mm)"));
                    }
                    ui.add(
                        egui::Slider::new(&mut app.marlin_z_speed, 100.0..=30000.0)
                            .text("Z 轴速度(mm/min)"),
                    );
                });
                ui.collapsing("EBB", |ui| {
                    ui.add(egui::Slider::new(&mut app.ebb_servo_speed, 0.0..=32767.0).text("舵机速度"));
                    ui.add(egui::Slider::new(&mut app.ebb_max_pen, 0.0..=65535.0).text("最大舵机值"));
                    ui.add(egui::Slider::new(&mut app.mm_to_steps, 1.0..=1000.0).text("毫米→步数"));
                    ui.add(
                        egui::Slider::new(&mut app.max_pen_speed, 1000.0..=32767.0)
                            .text("最大抬落笔速度"),
                    );
                });
                ui.weak("抬/落笔参数按固件分别保存，切换固件时不会互相覆盖。");

                section(ui, "坐标系");
                ui.horizontal(|ui| {
                    ui.label("原点角:");
                    for (i, nm) in ORIGINS.iter().enumerate() {
                        ui.selectable_value(&mut app.origin_corner, i, *nm);
                    }
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut app.reverse_x, "反转X");
                    ui.checkbox(&mut app.reverse_y, "反转Y");
                    ui.checkbox(&mut app.swap_xy, "对调XY");
                });

                section(ui, "回到机器原点 / 设当前点为原点");
                if !app.dev_connected {
                    ui.colored_label(
                        egui::Color32::from_rgb(200, 120, 40),
                        "未连接设备：下面的按钮会发出指令，但机器收不到（可先到「设备」页连接）",
                    );
                }
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .button("回到机器原点")
                        .on_hover_text("GRBL 发 $H、Marlin 发 G28：靠限位开关找机械原点")
                        .clicked()
                    {
                        app.home_machine();
                    }
                    if ui
                        .button("移到 (0,0)")
                        .on_hover_text("没有限位开关就用这个：直接快速移动到坐标零点")
                        .clicked()
                    {
                        app.go_zero();
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .button("设当前位置为原点")
                        .on_hover_text("G92 X0 Y0：机器不动，把「现在站着的位置」记为坐标零点")
                        .clicked()
                    {
                        app.set_zero_here();
                    }
                    if ui
                        .button("清除坐标偏置")
                        .on_hover_text("G92.1：撤掉 G92 设的零点，回到机器坐标系")
                        .clicked()
                    {
                        app.clear_zero_offset();
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    if ui.button("读取当前位置").clicked() {
                        app.read_position();
                    }
                    if ui
                        .button("解锁 $X")
                        .on_hover_text("GRBL 上电是 Alarm 锁定态，不解锁所有移动指令都会被忽略")
                        .clicked()
                    {
                        app.unlock_machine();
                    }
                });
                ui.weak("回显都写在窗口最下方的「实时日志」栏里。");

                section(ui, "自定义 G-code");
                ui.label("起始（多条用 ; 分隔）：");
                ui.text_edit_singleline(&mut app.custom_start);
                ui.label("结束：");
                ui.text_edit_singleline(&mut app.custom_end);

                section(ui, "逼真化（随机手写变形）");
                ui.checkbox(&mut app.use_random, "启用随机变形");
                ui.add_enabled(
                    app.use_random,
                    egui::Slider::new(&mut app.rand_x, 0.0..=1.0).text("水平抖动(mm)"),
                );
                ui.add_enabled(
                    app.use_random,
                    egui::Slider::new(&mut app.rand_y, 0.0..=1.0).text("垂直抖动(mm)"),
                );
                ui.add_enabled(
                    app.use_random,
                    egui::Slider::new(&mut app.rand_scale, 0.0..=0.2).text("大小抖动(±比例)"),
                );
                ui.add_enabled(
                    app.use_random,
                    egui::Slider::new(&mut app.rand_head, 0.0..=8.0).text("行首随机(mm)"),
                );

                section(ui, "图片默认导入参数");
                ui.add(egui::Slider::new(&mut app.img_target_h, 30.0..=250.0).text("目标高度(mm)"));
                ui.add(egui::Slider::new(&mut app.img_step, 1..=12).text("影线间隔(px)"));
                ui.add(egui::Slider::new(&mut app.img_thr, 0.1..=0.95).text("暗部阈值(素描)"));
                ui.add(egui::Slider::new(&mut app.img_edge, 0.05..=0.9).text("边缘阈值(线框)"));
                ui.checkbox(&mut app.img_invert, "反相");
                let mut dt = app.img_draw_type;
                ui.horizontal(|ui| {
                    ui.label("默认类型:");
                    for (i, name) in ImgDraw::names().iter().enumerate() {
                        ui.selectable_value(&mut dt, i, *name);
                    }
                });
                app.img_draw_type = dt;

                section(ui, "点动");
                ui.add(egui::Slider::new(&mut app.jog_step, 1.0..=50.0).text("步长(mm)"));
            });
        });
    app.settings_open = open;
}

/// 机器测试模块：XY 移动 / 回零 / 定位零点 / 抬落笔 / 画测试方框。
fn panel_test(app: &mut AppState, ui: &mut Ui) {
    section(ui, "⑧ 机器测试");

    ui.horizontal(|ui| {
        let (dot, col) = if app.dev_connected {
            ("●", egui::Color32::from_rgb(60, 170, 90))
        } else {
            ("○", egui::Color32::from_gray(150))
        };
        ui.colored_label(
            col,
            format!("{} {}", dot, if app.dev_connected { "已连接" } else { "未连接" }),
        );
        if !app.dev_connected {
            ui.weak("请先在「设备」页连接");
        }
        let snap = app.device.snapshot();
        if snap.running {
            ui.weak(format!("运行中 {}/{} 行", snap.done_lines, snap.total_lines));
        }
    });

    ui.separator();
    ui.label(RichText::new("解锁 / 复位").strong());
    ui.horizontal(|ui| {
        if ui.button("解锁 $X").clicked() {
            app.unlock_machine();
        }
        if ui.button("软复位").clicked() {
            app.soft_reset();
        }
    });
    ui.weak("GRBL 上电/急停后是 Alarm 锁定态，所有移动指令会被忽略。先点「解锁 $X」，日志看到 ok 才能动。");

    ui.separator();
    ui.label(RichText::new("回零 / 定位零点").strong());
    ui.horizontal(|ui| {
        if ui.button("回原点 $H").clicked() {
            app.home_machine();
        }
        if ui.button("移到 (0,0)").clicked() {
            app.go_zero();
        }
    });
    ui.horizontal(|ui| {
        if ui.button("当前点设为零点").clicked() {
            app.set_zero_here();
        }
        if ui.button("清除坐标偏置").clicked() {
            app.clear_zero_offset();
        }
    });
    ui.weak("「回原点 $H」需要限位开关；没有限位开关的写字机请用「移到 (0,0)」或「当前点设为零点」。");

    ui.separator();
    ui.label(RichText::new("XY 移动（点动）").strong());
    ui.add(egui::Slider::new(&mut app.jog_step, 1.0..=50.0).text("步长(mm)"));
    let s = app.jog_step;
    ui.horizontal(|ui| {
        if ui.button("X −").clicked() {
            send_jog(app, -s, 0.0);
        }
        if ui.button("X +").clicked() {
            send_jog(app, s, 0.0);
        }
        if ui.button("Y −").clicked() {
            send_jog(app, 0.0, -s);
        }
        if ui.button("Y +").clicked() {
            send_jog(app, 0.0, s);
        }
    });
    ui.horizontal(|ui| {
        if ui.button("↖").clicked() {
            send_jog(app, -s, -s);
        }
        if ui.button("↗").clicked() {
            send_jog(app, s, -s);
        }
        if ui.button("↙").clicked() {
            send_jog(app, -s, s);
        }
        if ui.button("↘").clicked() {
            send_jog(app, s, s);
        }
    });

    ui.separator();
    ui.label(RichText::new("笔 / Z").strong());
    ui.horizontal(|ui| {
        if ui.button("抬笔").clicked() {
            app.pen_up_now();
        }
        if ui.button("落笔").clicked() {
            app.pen_down_now();
        }
        if ui.button("读当前位置").clicked() {
            app.read_position();
        }
    });

    ui.separator();
    ui.label(RichText::new("画测试方框（验证方向与比例）").strong());
    ui.horizontal(|ui| {
        ui.add(egui::Slider::new(&mut app.test_frame_w, 10.0..=300.0).text("宽(mm)"));
        ui.add(egui::Slider::new(&mut app.test_frame_h, 10.0..=300.0).text("高(mm)"));
    });
    if ui.button("开始画方框").clicked() {
        let (w, h) = (app.test_frame_w, app.test_frame_h);
        app.draw_test_frame(w, h);
    }
    ui.weak("画出来是矩形说明方向正常；若是镜像/旋转，到设置里调「反转X/反转Y/对调XY」。");

    ui.separator();
    ui.label(RichText::new("发送自定义单行 G-code").strong());
    ui.text_edit_singleline(&mut app.test_custom_gcode);
    if ui.button("发送").clicked() {
        let g = app.test_custom_gcode.clone();
        if !g.trim().is_empty() {
            app.send_gcode(&g);
            app.status = format!("已发送：{}", g);
        }
    }

    ui.separator();
    ui.horizontal(|ui| {
        if ui.button("停止运动").clicked() {
            app.stop_run();
        }
        if ui.button("清空日志").clicked() {
            app.clear_current_module();
        }
    });

    ui.separator();
    ui.label("日志：");
    ui.add(TextEdit::multiline(&mut app.dev_log).desired_rows(10).interactive(false));
}

fn panel_text(app: &mut AppState, ui: &mut Ui) {
    section(ui, "① 文字");
    app.ensure_texts();

    // ── 多个文本框：增删与切换 ───────────────────────────
    // 参考项目的工程存档里一页可以有任意多个文字元素，各自带字体/字号/位置。
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("文本框（{} 个）", app.texts.len())).strong());
        if ui.button("+ 新增").on_hover_text("再加一个文字区域（沿用当前框的参数，位置自动下移）").clicked() {
            app.add_text_box();
            app.status = format!("已新增，共 {} 个文本框", app.texts.len());
        }
        if ui.add_enabled(app.texts.len() > 1, egui::Button::new("删除当前")).clicked() {
            app.remove_text_box();
            app.status = format!("已删除，还剩 {} 个文本框", app.texts.len());
        }
    });
    let nbox = app.texts.len();
    for i in 0..nbox {
        ui.horizontal(|ui| {
            let b = &mut app.texts[i];
            ui.checkbox(&mut b.on, "").on_hover_text("是否输出这个框");
            let sel = app.sel_text == i;
            if ui.selectable_label(sel, b.summary()).clicked() {
                app.sel_text = i;
            }
        });
    }
    ui.separator();

    let names = app.fonts.names();
    let i = app.sel_text.min(nbox - 1);
    let b = &mut app.texts[i];
    ui.label(format!("内容（{}）：", b.name));
    ui.add(TextEdit::multiline(&mut b.text).desired_rows(6));
    font_combo(ui, "字体", &mut b.font, names);
    ui.add(egui::Slider::new(&mut b.size, 20.0..=300.0).text("字号(基准100)"));
    // 行距 / 字间距允许为负：字挤得更紧（极限由排版层兜住，不会倒着排）
    ui.add(egui::Slider::new(&mut b.line_gap, -20.0..=20.0).text("行距(mm，可负)"));
    ui.add(egui::Slider::new(&mut b.space, -10.0..=10.0).text("字间距(mm，可负)"));
    ui.horizontal(|ui| {
        ui.label("对齐:");
        ui.selectable_value(&mut b.align, 0, "左");
        ui.selectable_value(&mut b.align, 1, "中");
        ui.selectable_value(&mut b.align, 2, "右");
    });
    ui.horizontal(|ui| {
        ui.label("段首:");
        ui.selectable_value(&mut b.first, 0, "无");
        ui.selectable_value(&mut b.first, 1, "居中");
        ui.selectable_value(&mut b.first, 2, "缩进");
    });

    section(ui, "本框位置与尺寸(mm)");
    // 滑块上限跟着**当前纸张**走：以前写死 300/400，换成大纸后拖不动，
    // 换成小纸又能把框拖到纸外看不见。
    let (pw, ph) = (app.page_w_mm, app.page_h_mm);
    ui.add(egui::Slider::new(&mut b.x, 0.0..=pw).text("X"));
    ui.add(egui::Slider::new(&mut b.y, 0.0..=ph).text("Y"));
    ui.add(egui::Slider::new(&mut b.w, 10.0..=pw.max(10.0)).text("宽"));
    ui.add(egui::Slider::new(&mut b.h, 10.0..=ph.max(10.0)).text("高"));
    if ui.button("收进页面内").on_hover_text("换纸张后一键把框收回纸面（留 10mm 边距）").clicked() {
        app.clamp_text_box(i);
        app.status = "已把文本框收进页面内".into();
    }
    ui.checkbox(&mut app.text_frame_on, "在画布上显示文本框虚框");
    ui.weak("虚框只是定位参考，不会写进 G-code。");

    section(ui, "逼真化（随机手写变形）");
    ui.checkbox(&mut app.use_random, "启用随机变形");
    ui.add_enabled(
        app.use_random,
        egui::Slider::new(&mut app.rand_x, 0.0..=1.0).text("水平抖动(mm)"),
    );
    ui.add_enabled(
        app.use_random,
        egui::Slider::new(&mut app.rand_y, 0.0..=1.0).text("垂直抖动(mm)"),
    );
    ui.add_enabled(
        app.use_random,
        egui::Slider::new(&mut app.rand_scale, 0.0..=0.2).text("大小抖动(±比例)"),
    );
    ui.add_enabled(
        app.use_random,
        egui::Slider::new(&mut app.rand_head, 0.0..=8.0).text("行首随机(mm)"),
    );

    // 「对哪些文本框生效」：以前一开就整页所有文字都抖，现在可以只挑其中几框
    ui.add_enabled_ui(app.use_random, |ui| {
        ui.separator();
        ui.label(RichText::new("生效范围（只对勾上的文本框生效）").strong());
        let n = app.texts.len();
        for i in 0..n {
            let nm = app.texts[i].name.clone();
            ui.checkbox(&mut app.texts[i].random, nm);
        }
        ui.horizontal(|ui| {
            if ui.button("全选").clicked() {
                for b in app.texts.iter_mut() {
                    b.random = true;
                }
            }
            if ui.button("全不选").clicked() {
                for b in app.texts.iter_mut() {
                    b.random = false;
                }
            }
            if ui.button("只选当前框").clicked() {
                let cur = app.sel_text.min(app.texts.len() - 1);
                for (i, b) in app.texts.iter_mut().enumerate() {
                    b.random = i == cur;
                }
            }
        });
    });
    if ui.add_enabled(app.use_random, egui::Button::new("换一种随机笔迹")).clicked() {
        app.rebuild_preview_random();
        app.status = "已重新随机".into();
    }
}

fn panel_table(app: &mut AppState, ui: &mut Ui) {
    section(ui, "② 表格");
    usize_slider(ui, "行数", &mut app.table_rows, 1, 30);
    usize_slider(ui, "列数", &mut app.table_cols, 1, 20);
    ui.add(egui::Slider::new(&mut app.table_cell, 6.0..=40.0).text("单元格(mm)"));

    section(ui, "表格线条");
    ui.add(egui::Slider::new(&mut app.table_line_width, 0.1..=2.0).text("线条宽度(mm)"));
    ui.weak("笔尖不能真正加粗：>0.35mm 时用多条并行描边实现。");
    ui.horizontal(|ui| {
        ui.checkbox(&mut app.table_show_h, "显示横行（横线）");
        ui.checkbox(&mut app.table_show_v, "显示竖列（竖线）");
    });

    font_combo(ui, "字体", &mut app.table_font, app.fonts.names());
    ui.checkbox(&mut app.table_auto_font, "字号自适应格高");
    ui.add_enabled(
        !app.table_auto_font,
        egui::Slider::new(&mut app.table_font_size, 10.0..=300.0).text("字号(基准100)"),
    );
    ui.label("内容（每行一行，Tab/逗号/空格分隔单元格）：");
    ui.add(TextEdit::multiline(&mut app.table_text).desired_rows(6));
    if ui.button("从 Excel 导入填充").clicked() {
        if let Some(p) = app.pick_file("Excel", &["xlsx", "xls"]) {
            match io::import_excel(app, &p) {
                Ok(n) => app.status = format!("已导入 Excel：{} 行", n),
                Err(e) => app.status = format!("导入失败：{e}"),
            }
        }
    }
}

fn panel_grid(app: &mut AppState, ui: &mut Ui) {
    section(ui, "③ 格子（练字格）");
    ui.horizontal(|ui| {
        ui.label("格型:");
        for (i, name) in GridKind::names().iter().enumerate() {
            ui.selectable_value(&mut app.grid_kind, i, *name);
        }
    });
    usize_slider(ui, "行数", &mut app.grid_rows, 1, 20);
    usize_slider(ui, "列数", &mut app.grid_cols, 1, 20);
    ui.add(egui::Slider::new(&mut app.grid_cell, 6.0..=40.0).text("格尺寸(mm)"));

    // —— 显示 / 隐藏 与线条样式 ——
    section(ui, "显示与线条");
    ui.checkbox(&mut app.grid_show, "显示格子（关闭则线条与示范字都不输出）");
    ui.add_enabled(
        app.grid_show,
        egui::Slider::new(&mut app.grid_line_width, 0.1..=2.0).text("线条宽度(mm)"),
    );
    ui.weak("笔尖不能真正加粗：>0.35mm 时用多条并行描边实现。");
    ui.horizontal(|ui| {
        ui.add_enabled(app.grid_show, egui::Checkbox::new(&mut app.grid_show_h, "显示横行（横线）"));
        ui.add_enabled(app.grid_show, egui::Checkbox::new(&mut app.grid_show_v, "显示竖列（竖线）"));
    });
    ui.weak("米字格的斜叉在横竖都显示时才输出。");

    font_combo(ui, "示范字字体", &mut app.grid_font, app.fonts.names());
    ui.checkbox(&mut app.grid_auto_font, "示范字号自适应格高");
    ui.add_enabled(
        !app.grid_auto_font,
        egui::Slider::new(&mut app.grid_font_size, 10.0..=300.0).text("示范字号"),
    );
    ui.label("示范字（逐格填充，留空只画格）：");
    ui.add(TextEdit::multiline(&mut app.grid_sample).desired_rows(3));
    ui.weak("示范字已按「字心 = 格心」精确居中；字号自适应按真实字身高度计算。");
}

fn panel_draw(app: &mut AppState, ui: &mut Ui) {
    section(ui, "④ 绘图");

    ui.checkbox(&mut app.freehand_on, "手绘模式（在页面内拖动鼠标）");
    if app.freehand_on {
        ui.label(RichText::new("提示：拖动完成即生成一条笔迹").size(10.0).color(muted()));
        if ui.button("清空手绘笔迹").clicked() {
            app.draw_paths.clear();
            app.freehand.clear();
            app.status = "已清空手绘".into();
        }
    }

    ui.separator();
    ui.label(RichText::new("图片描边").strong());
    ui.add(egui::Slider::new(&mut app.draw_target_h, 30.0..=250.0).text("目标高度(mm)"));
    ui.add(egui::Slider::new(&mut app.draw_step, 1.0..=10.0).text("影线间距(px)"));
    ui.add(egui::Slider::new(&mut app.draw_thresh, 0.1..=0.95).text("阈值(越低越黑)"));
    if ui.button("导入图片并生成描边").clicked() {
        if let Some(p) = app.pick_file("图片", &["png", "jpg", "jpeg", "gif", "bmp"]) {
            match app.import_image_file(&p, false) {
                Ok(n) => app.status = format!("已导入图片（{} 条笔迹），可在「图片」页调整", n),
                Err(e) => app.status = format!("导入失败：{e}"),
            }
        }
    }
    ui.label(if app.draw_path.is_empty() {
        "尚未导入图片".to_string()
    } else {
        format!("来源：{}", app.draw_path)
    });

    ui.separator();
    ui.label(RichText::new("矢量文件（CAD / SVG 背景）").strong());
    ui.add(
        egui::Slider::new(&mut app.draw_target_h, 30.0..=250.0).text("目标高度(mm)"),
    );
    ui.weak("导入后等比缩放到该高度、锚定页面左上角（与图片描边一致）。");
    if ui.button("导入 DXF / SVG 矢量文件").clicked() {
        if let Some(p) = app.pick_file("矢量文件", &["dxf", "svg"]) {
            match app.import_vector_file(&p, app.draw_target_h) {
                Ok(n) => app.status = format!("已导入矢量：{n} 条笔迹"),
                Err(e) => app.status = format!("导入失败：{e}"),
            }
        }
    }
}

fn panel_notes(app: &mut AppState, ui: &mut Ui) {
    section(ui, "⑤ 笔记（自由文本块）");
    if ui.button("+ 添加文本块").clicked() {
        app.notes.push(crate::state::NoteBlock {
            text: "新文本".into(),
            font: app.text_font.clone(),
            size_mm: 8.0,
            x: 15.0,
            y: 15.0 + app.notes.len() as f64 * 20.0,
            align: 0,
        });
    }
    let mut remove = None;
    let font_names = app.fonts.names();
    for (i, b) in app.notes.iter_mut().enumerate() {
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("文本块 #{}", i + 1)).strong());
                if ui.button("删除").clicked() {
                    remove = Some(i);
                }
            });
            ui.add(TextEdit::multiline(&mut b.text).desired_rows(2));
            font_combo(ui, "字体", &mut b.font, font_names.clone());
            ui.add(egui::Slider::new(&mut b.size_mm, 3.0..=30.0).text("字高(mm)"));
            ui.horizontal(|ui| {
                ui.add(egui::Slider::new(&mut b.x, 0.0..=200.0).text("X"));
                ui.add(egui::Slider::new(&mut b.y, 0.0..=260.0).text("Y"));
            });
            ui.horizontal(|ui| {
                ui.label("对齐:");
                ui.selectable_value(&mut b.align, 0, "左");
                ui.selectable_value(&mut b.align, 1, "中");
                ui.selectable_value(&mut b.align, 2, "右");
            });
        });
    }
    if let Some(i) = remove {
        app.notes.remove(i);
    }
}

fn panel_device(app: &mut AppState, ui: &mut Ui) {
    section(ui, "⑦ 设备");

    // ── 连接状态横幅：把「现在到底卡在哪一步」直接写在最上面 ──
    {
        let snap = app.device.snapshot();
        let (txt, col) = if app.dev_connected {
            let baud = if snap.cur_baud > 0 {
                format!(" @ {}", snap.cur_baud)
            } else {
                String::new()
            };
            (
                format!("● 已连接 {}{}（到「测试」页发条指令验证机器动不动）", snap.cur_addr, baud),
                egui::Color32::from_rgb(60, 170, 90),
            )
        } else if app.serial_ports_all.is_empty() {
            (
                "✗ 还没扫描到任何 COM 口：先点「扫描串口」；若列表为空，说明机器没连上电脑（有线=插 USB，蓝牙=配对并连上 BtWriter）".to_string(),
                egui::Color32::from_rgb(210, 70, 60),
            )
        } else if app.serial_port.is_empty() {
            (
                "○ 未选择端口：先在上面挑一个，再点「连接」".to_string(),
                egui::Color32::from_gray(170),
            )
        } else {
            (
                format!("○ 未连接：选中的是 {}，点「连接」；连不上就点「自动波特率」或「诊断端口」", app.serial_port),
                egui::Color32::from_rgb(210, 150, 40),
            )
        };
        ui.label(RichText::new(txt).color(col).strong());
    }

    ui.horizontal(|ui| {
        ui.label("方式:");
        ui.selectable_value(&mut app.dev_kind, 0, "串口/蓝牙");
        ui.selectable_value(&mut app.dev_kind, 1, "TCP");
        ui.selectable_value(&mut app.dev_kind, 2, "模拟");
    });

    if app.dev_kind == 0 {
        // —— 串口（蓝牙 SPP / USB-TTL）——
        // 窄面板里横向塞两个下拉框会互相挤压重叠，这里统一改成竖排。
        ui.label("端口:");
        let cur = app.serial_port.clone();
        egui::ComboBox::from_id_salt("ser_port")
            .width(280.0)
            .selected_text(if cur.is_empty() { "（未选择）".to_string() } else { cur })
            .show_ui(ui, |ui| {
                // 未扫描时给出登记名单或常见候选；扫描后标注在线/离线。
                // 扫描过则显示【类型 · 设备名】，蓝牙与串口线一眼分得开。
                if app.port_labels.is_empty() {
                    let mut list: Vec<(String, bool)> = app.serial_ports_all.clone();
                    if list.is_empty() {
                        list = (1..=8).map(|i| (format!("COM{}", i), false)).collect();
                    }
                    for (p, ok) in list {
                        let txt = if ok {
                            format!("{}（在线）", p)
                        } else {
                            format!("{}（离线/未连接）", p)
                        };
                        ui.selectable_value(&mut app.serial_port, p.clone(), txt);
                    }
                } else {
                    for (p, label, ok) in app.port_labels.clone() {
                        let txt = format!("{}{}", label, if ok { " ●在线" } else { " ○离线" });
                        ui.selectable_value(&mut app.serial_port, p.clone(), txt);
                    }
                }
            });

        ui.label("波特率:");
        let bauds = wrobot_core::COMMON_BAUDS;
        egui::ComboBox::from_id_salt("ser_baud")
            .width(280.0)
            .selected_text(format!("{}", app.serial_baud))
            .show_ui(ui, |ui| {
                for b in bauds {
                    ui.selectable_value(&mut app.serial_baud, *b, format!("{}", b));
                }
            });

        ui.horizontal_wrapped(|ui| {
            if ui.button("扫描串口").clicked() {
                let cur = app.serial_port.clone();
                app.scan_ports();
                if app.serial_ports.contains(&cur) || !cur.is_empty() {
                    app.serial_port = cur;
                }
            }
            if ui.button("自动连接").clicked() {
                app.auto_connect();
            }
            if ui.button("自动波特率").on_hover_text("逐个波特率握手，找出机器真正用的那个（连上但没反应时首选这个）").clicked() {
                app.auto_baud();
            }
            if ui.button("诊断端口").clicked() {
                app.diagnose_ports();
            }
            if ui.button("体检串口线").on_hover_text("对选中的有线串口逐层测：驱动→写测试→回显监听→波特率扫描，结果写在底部日志里").clicked() {
                app.wire_test();
            }
        });
        ui.horizontal_wrapped(|ui| {
            if ui.button("打开蓝牙设置").clicked() {
                open_system("ms-settings:bluetooth");
            }
            if ui.button("打开设备管理器").clicked() {
                open_system("devmgmt.msc");
            }
            if ui.button("试 9600（换波特率）").clicked() {
                app.serial_baud = 9600;
                app.status = "波特率已切到 9600，再点一次「连接」".into();
            }
        });

        if app.serial_ports_all.is_empty() {
            ui.weak("点「扫描串口」列出本机全部 COM 口（含蓝牙已配对但离线的）");
        } else {
            ui.label(RichText::new("本机端口（蓝牙与串口线分开显示）：").strong());
            for (p, label, ok) in app.port_labels.clone() {
                let (mark, col) = if ok {
                    ("●", egui::Color32::from_rgb(60, 170, 90))
                } else {
                    ("○", egui::Color32::from_gray(140))
                };
                let hint = if ok {
                    String::from("可打开")
                } else if label.starts_with("蓝牙") {
                    String::from("打不开：蓝牙多半只配对未连接，或机器没开机")
                } else {
                    String::from("打不开：设备未插好/驱动故障，拔插 USB 后重扫")
                };
                ui.colored_label(col, format!("{} {}  {}", mark, label, hint));
                let _ = p;
            }
            if !app.port_labels.iter().any(|(_, l, _)| l.starts_with("串口线"))
                && !app.port_labels.is_empty()
            {
                ui.colored_label(
                    egui::Color32::from_rgb(210, 150, 40),
                    "⚠ 没有检测到串口线（USB-TTL）：插上适配器后点「扫描」，带 USB/SERIAL 字样的就是它",
                );
            }
        }


    // ── 蓝牙专区 ───────────────────────────────────────────────
    // 「设置里看得到设备」≠「有能用的串口」。这里直接问蓝牙栈：
    // 写字机在不在、装没装 SPP、COM 口绑没绑对，并且能一键绑定。
    egui::CollapsingHeader::new("蓝牙链路（看得到设备但仍连不上，先看这里）")
        .default_open(true)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("写字机蓝牙名：");
                ui.add(
                    egui::TextEdit::singleline(&mut app.bt_name)
                        .desired_width(140.0)
                        .hint_text("BtWriter"),
                );
            });
            ui.horizontal_wrapped(|ui| {
                if ui
                    .button("蓝牙体检")
                    .on_hover_text("查适配器、写字机在不在、SPP 装没装、COM 口绑没绑对设备")
                    .clicked()
                {
                    app.bt_report();
                }
                if ui
                    .button("绑定蓝牙串口")
                    .on_hover_text(
                        "给已配对的写字机启用 SPP 服务，让系统建出真正绑到它的传出 COM 口",
                    )
                    .clicked()
                {
                    app.bt_bind();
                }
                if ui
                    .button("观察可见性")
                    .on_hover_text("周期性查询，看它是不是一会儿看得到一会儿看不到")
                    .clicked()
                {
                    app.bt_watch();
                }
            });
                ui.horizontal(|ui| {
                    ui.label("轮数");
                    ui.add(egui::DragValue::new(&mut app.bt_watch_rounds).range(2..=60));
                    ui.label("间隔秒");
                    ui.add(egui::DragValue::new(&mut app.bt_watch_every).range(2..=60));
                });
                ui.horizontal_wrapped(|ui| {
                    if ui.button("打开蓝牙 COM 端口设置").on_hover_text(
                        "手工建【传出】方向的蓝牙串口：添加 → 传出 → 选 BtWriter → 确定",
                    ).clicked() {
                        open_system("bthprops.cpl");
                    }
                    if ui.button("打开蓝牙设置").clicked() {
                        open_system("ms-settings:bluetooth");
                    }
                });

            if !app.bt_report.is_empty() {
                ui.separator();
                egui::ScrollArea::vertical()
                    .id_salt("bt_report_scroll")
                    .max_height(260.0)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        ui.monospace(&app.bt_report);
                    });
            }
        });

        // 连不上时的排查清单（默认折叠，不占版面）
        egui::CollapsingHeader::new("连不上机器？点开排查")
            .default_open(false)
            .show(ui, |ui| {
                ui.label(RichText::new("按这个顺序做，八成能解决：").strong());
                for (i, s) in [
                    "写字机通电了吗？（风扇/指示灯）",
                    "有线：拔掉 USB 重插，最好换个 USB 口；看设备管理器有没有「USB-SERIAL CH340 (COMx)」",
                    "蓝牙：Windows 蓝牙里连上 BtWriter；再在「蓝牙设置→更多蓝牙选项→COM 端口」确认有【传出】端口",
                    "点「扫描串口」→ 选在线的口 → 点「连接」",
                    "连上了但机器不动：点「自动波特率」——波特率不对时机器一声不吭，日志里还会刷一堆 <F8> 之类的十六进制乱码",
                    "还不行就点「诊断端口」，看日志里 WriteFile/ReadFile 两行：能开但不通 = 链路没建立",
                    "到「测试」页点「解锁 $X」：有 ← 回显说明通了，没回显说明线没通",
                ]
                .iter()
                .enumerate()
                {
                    ui.label(format!("{}. {}", i + 1, s));
                }
                ui.separator();
                ui.label(RichText::new("错误码速查").strong());
                for (code, tip) in [
                    ("31", "设备不工作：拔插 USB / 设备管理器禁用再启用 / 写字机通电 / 蓝牙重新配对"),
                    ("5", "端口被占用：关掉串口助手、厂商软件、本程序的其它实例"),
                    ("121/433", "蓝牙只配对未连接：Windows 里连上 BtWriter，机器要开机"),
                    ("2", "端口不存在：换 COM 口号，或重新添加蓝牙传出端口"),
                    ("1167/1117", "设备未连接 / I/O 失败：数据线松或设备掉线"),
                ] {
                    ui.label(RichText::new(format!("{} → {}", code, tip)).size(11.0).color(muted()));
                }
            });
    } else if app.dev_kind == 1 {
        ui.horizontal(|ui| {
            ui.label("地址:");
            ui.text_edit_singleline(&mut app.dev_addr);
        });
    }

    ui.horizontal(|ui| {
        if ui.button("连接").clicked() {
            app.connect_device();
        }
        if ui.button("断开").clicked() {
            app.disconnect_device();
        }
        if app.dev_connected && ui.button("查询固件").clicked() {
            app.query_firmware();
        }
    });
    let (dot, col) = if app.dev_connected {
        ("●", egui::Color32::from_rgb(60, 170, 90))
    } else {
        ("○", egui::Color32::from_gray(150))
    };
    ui.horizontal(|ui| {
        ui.colored_label(
            col,
            format!(
                "{} {}",
                dot,
                if app.dev_connected {
                    "已连接"
                } else {
                    "未连接"
                }
            ),
        );
        if app.dev_connected && app.dev_kind == 0 {
            ui.weak(format!("{} @ {}", app.serial_port, app.serial_baud));
        }
    });

    ui.separator();
    ui.checkbox(&mut app.single_step, "单步下发（逐行发送）");
    if ui.button("发送当前 G-code").clicked() {
        let g = app.generate_gcode();
        app.gcode_text = g.clone();
        let n = g.lines().filter(|l| !l.trim().is_empty()).count();
        if app.single_step {
            for line in g.lines() {
                if !line.trim().is_empty() {
                    app.send_gcode(line);
                }
            }
            app.status = format!("已逐行下发 {} 行（结果见左下角传输状态）", n);
        } else {
            app.send_gcode(&g);
            app.status = format!("已下发 G-code {} 行（结果见左下角传输状态）", n);
        }
    }

    // 机器幅面（电动移动 / 归零的边界参考）
    // 两个滑块并排会互相挤压到重叠，改为竖排各占一行。
    ui.separator();
    ui.label(RichText::new("机器幅面（绘图区域）").strong());
    ui.add(egui::Slider::new(&mut app.machine_w_mm, 50.0..=600.0).text("宽(mm)"));
    ui.add(egui::Slider::new(&mut app.machine_h_mm, 50.0..=800.0).text("高(mm)"));
    ui.weak("电动移动 / 回零将以此为边界提示，避免越界碰撞");

    // 运行控制（开始绘制 / 暂停 / 停止 / 回原点）
    ui.separator();
    ui.label(RichText::new("运行控制").strong());
    let snap = app.device.snapshot();
    ui.horizontal(|ui| {
        let start = egui::Button::new(
            RichText::new("开始绘制").strong().color(egui::Color32::WHITE),
        )
        .fill(egui::Color32::from_rgb(46, 160, 90))
        .min_size(egui::Vec2::new(95.0, 28.0));
        if ui.add_enabled(!snap.running, start).clicked() {
            let g = app.generate_gcode();
            if g.trim().is_empty() {
                app.status = "当前页面没有可绘制的内容".into();
            } else {
                app.gcode_text = g.clone();
                app.start_run(g);
                app.status = "已开始绘制".into();
            }
        }
        if snap.running && !snap.paused {
            if ui.add_sized([58.0, 28.0], egui::Button::new("暂停")).clicked() {
                app.pause_run();
            }
        } else if snap.paused {
            if ui.add_sized([58.0, 28.0], egui::Button::new("继续")).clicked() {
                app.resume_run();
            }
        }
        if ui
            .add_enabled(snap.running, egui::Button::new("停止").min_size(egui::Vec2::new(52.0, 28.0)))
            .clicked()
        {
            app.stop_run();
        }
    });
    ui.horizontal(|ui| {
        if ui.button("回原点").clicked() {
            app.go_zero();
        }
        if ui.button("解锁 $X").clicked() {
            app.unlock_machine();
        }
        if ui.button("移到 (0,0)").clicked() {
            app.go_zero();
        }
    });

    // 点动
    ui.separator();
    ui.label(RichText::new("点动").strong());
    ui.add(egui::Slider::new(&mut app.jog_step, 1.0..=50.0).text("步长(mm)"));
    let s = app.jog_step;
    ui.horizontal(|ui| {
        if ui.button("X−").clicked() {
            send_jog(app, -s, 0.0);
        }
        if ui.button("X+").clicked() {
            send_jog(app, s, 0.0);
        }
        if ui.button("Y−").clicked() {
            send_jog(app, 0.0, -s);
        }
        if ui.button("Y+").clicked() {
            send_jog(app, 0.0, s);
        }
    });

    ui.separator();
    ui.horizontal(|ui| {
        ui.label("日志：");
        if ui.button("清空日志").clicked() {
            app.device.clear_log();
            app.dev_log.clear();
            app.status = "已清空设备日志".into();
        }
        let btn = if app.log_open { "隐藏底部日志栏" } else { "显示底部日志栏" };
        if ui.button(btn).clicked() {
            app.log_open = !app.log_open;
        }
    });
    ui.weak("完整日志在窗口最下方的「实时日志」栏（可拖动调高，任何页面都可见）。");

    // 页面内容的一键清除（设备页也能清，不必切回编辑页）
    ui.separator();
    let clear_btn = if app.confirm_clear {
        egui::Button::new(RichText::new("确认清空全部内容？").color(egui::Color32::WHITE))
            .fill(egui::Color32::from_rgb(190, 70, 60))
            .min_size(egui::Vec2::new(200.0, 28.0))
    } else {
        egui::Button::new("清空页面内容（文字/表格/格子/手绘/笔记/图片）")
            .min_size(egui::Vec2::new(200.0, 28.0))
    };
    if ui.add(clear_btn).clicked() {
        if app.confirm_clear {
            app.clear_all_content();
        } else {
            app.confirm_clear = true;
            app.status = "再点一次确认清空全部内容".into();
        }
    }
    ui.weak("当前页面显示的是「最近停留的内容模块」，可在左侧「并入当前页面」勾选叠加。");
}

fn send_jog(app: &mut AppState, dx: f64, dy: f64) {
    let g = app.jog_gcode(dx, dy);
    app.send_gcode(&g);
    app.status = format!("点动 ({}, {})", dx, dy);
}

fn panel_io(app: &mut AppState, ui: &mut Ui) {
    section(ui, "⑨ 导入 / 导出");
    ui.label(RichText::new("纸张 / 页面尺寸").strong());
    paper_row(app, ui);
    ui.horizontal(|ui| {
        ui.add(egui::Slider::new(&mut app.page_w_mm, 20.0..=600.0).text("宽"));
        ui.add(egui::Slider::new(&mut app.page_h_mm, 20.0..=800.0).text("高"));
    });
    ui.weak("改宽高后会自动判定规格名（不再是标准尺寸就显示为「自定义」）。");
    ui.separator();
    ui.horizontal(|ui| {
        if ui.button("新建（清空全部内容）").clicked() {
            app.new_project();
        }
    });
    ui.weak("清空全部编辑内容，保留字库与设备连接。");
    ui.separator();
    if ui.button("打开工程 JSON").clicked() {
        if let Some(p) = app.pick_file("JSON", &["json"]) {
            match io::open_project(&p) {
                Ok(doc) => {
                    // 工程里的每个文字元素 → 一个独立的文本框，不再挤成一大段
                    let items: Vec<String> = doc
                        .text_items()
                        .map(|it| it.text().to_string())
                        .filter(|s| !s.trim().is_empty())
                        .collect();
                    app.texts.clear();
                    if items.is_empty() {
                        app.ensure_texts();
                        app.status = "已载入工程（没有文字内容）".into();
                    } else {
                        for (k, t) in items.into_iter().enumerate() {
                            let mut b = app.texts.first().cloned().unwrap_or_default();
                            b.name = format!("文本框 {}", k + 1);
                            b.text = t;
                            b.on = true;
                            app.texts.push(b);
                        }
                        app.sel_text = 0;
                        app.status = format!("已载入工程（{} 个文本框）", app.texts.len());
                    }
                }
                Err(e) => app.status = format!("打开失败：{e}"),
            }
        }
    }
    if ui.button("保存工程 JSON").clicked() {
        if let Some(p) = app.save_file_as("JSON", &["json"], "工程.json") {
            match io::save_project(app, &p) {
                Ok(()) => app.status = format!("工程已保存：{}", p.display()),
                Err(e) => app.status = format!("保存失败：{e}"),
            }
        }
    }

    // 完整工程（含图片 / 文本块 / 设备设置等全部编辑内容）
    section(ui, "完整工程（含图片与全部设置）");
    ui.label(RichText::new("保存/载入整个编辑工程（.wprj），下次可直接继续").size(11.0).color(muted()));
    ui.horizontal(|ui| {
        if ui.button("保存完整工程").clicked() {
            if let Some(p) = app.save_file_as("工程", &["wprj", "json"], "我的工程.wprj") {
                match io::save_project_full(app, &p) {
                    Ok(()) => app.status = format!("完整工程已保存：{}", p.display()),
                    Err(e) => app.status = format!("保存失败：{e}"),
                }
            }
        }
        if ui.button("打开完整工程").clicked() {
            if let Some(p) = app.pick_file("工程", &["wprj", "json"]) {
                match io::load_project_full(app, &p) {
                    Ok(()) => app.status = format!("完整工程已载入：{}", p.display()),
                    Err(e) => app.status = format!("载入失败：{e}"),
                }
            }
        }
    });

    // PDF 导入：可选页，默认 1:1 原始尺寸
    section(ui, "导入 PDF（可指定页）");
    ui.horizontal(|ui| {
        if ui.button("选择 PDF…").clicked() {
            if let Some(p) = app.pick_file("PDF", &["pdf"]) {
                match app.load_pdf_info(&p) {
                    Ok(n) => app.status = format!("已读取：{} 页（选好页码后点「导入该页」）", n),
                    Err(e) => app.status = format!("读取 PDF 失败：{e}"),
                }
            }
        }
        if app.pdf_pages > 0 {
            ui.weak(format!("共 {} 页", app.pdf_pages));
        }
    });
    if app.pdf_pages > 0 {
        ui.weak(&app.pdf_path);
        ui.horizontal(|ui| {
            ui.label("页码：");
            ui.add(egui::DragValue::new(&mut app.pdf_page).range(1..=app.pdf_pages.max(1)));
            ui.label(format!("/ {}", app.pdf_pages));
        });
        ui.checkbox(&mut app.pdf_1to1, "按原始尺寸 1:1 导入");
        ui.add_enabled(
            !app.pdf_1to1,
            egui::Slider::new(&mut app.img_target_h, 30.0..=250.0).text("目标高度(mm)"),
        );
        if ui
            .button(format!("导入第 {} 页", app.pdf_page))
            .on_hover_text(
                "矢量图形直接描边；文字按原位置用当前手写字体重排；嵌入图片走影线/轮廓描边",
            )
            .clicked()
        {
            let path = std::path::PathBuf::from(&app.pdf_path);
            let page = app.pdf_page.max(1);
            match app.import_pdf_page(&path, page) {
                Ok((n_vec, n_img)) => {
                    let mut msg = format!("已导入 PDF 第 {} 页", page);
                    if n_img > 0 {
                        msg.push_str(&format!("：{} 张图片", n_img));
                    }
                    if n_vec > 0 {
                        msg.push_str(&format!("、{} 条矢量/文字笔迹", n_vec));
                    }
                    if n_img == 0 && n_vec == 0 {
                        msg.push_str("，但这一页没有可绘制内容（可能是纯空白页）");
                    } else {
                        msg.push_str("。到「图片」页调整，并勾选「并入当前页面」才会输出。");
                    }
                    app.status = msg;
                }
                Err(e) => app.status = format!("PDF 导入失败：{e}"),
            }
        }
        ui.weak("导入结果会放进「图片」页，默认不并入输出，需要时再勾选。");
    }

    // 字库管理
    section(ui, "字库管理");
    ui.label(RichText::new(format!("当前 {} 个 .gfont 字库", app.fonts.fonts.len())).size(11.0));
    if ui.button("导入字库文件(.gfont)").clicked() {
        if let Some(p) = app.pick_file("字库", &["gfont"]) {
            match wrobot_core::font::GfontFile::open(&p) {
                Ok(f) => {
                    let n = f.name().to_string();
                    app.fonts.fonts.push(f);
                    app.status = format!("已导入字库：{}", n);
                }
                Err(e) => app.status = format!("字库导入失败：{e}"),
            }
        }
    }
    if ui.button("重新扫描字库目录").clicked() {
        let reloaded = crate::fontmgr::FontManager::load(std::path::Path::new(&app.font_dir));
        let n = reloaded.fonts.len();
        app.fonts = reloaded;
        app.status = format!("已重新扫描：{} 个字库", n);
    }
}

/// 图片页：导入（前景可打印 / 背景参考）、多选、删除/清空（二次确认）、
/// 缩放 / 移动 / 居中 / 适应页面等完整变换。
fn panel_images(app: &mut AppState, ui: &mut Ui) {
    section(ui, "⑥ 图片");
    ui.label("导入图片：");
    ui.horizontal(|ui| {
        if ui.button("导入前景图片（可打印）").clicked() {
            if let Some(p) = app.pick_file("图片", &["png", "jpg", "jpeg", "gif", "bmp"]) {
                match app.import_image_file(&p, false) {
                    Ok(n) => app.status = format!("已导入前景图片（{} 条笔迹）", n),
                    Err(e) => app.status = format!("导入失败：{e}"),
                }
            }
        }
        if ui.button("导入背景参考图").clicked() {
            if let Some(p) = app.pick_file("图片", &["png", "jpg", "jpeg", "gif", "bmp"]) {
                match app.import_image_file(&p, true) {
                    Ok(n) => app.status = format!("已导入背景参考图（{} 条笔迹，浅色显示不打印）", n),
                    Err(e) => app.status = format!("导入失败：{e}"),
                }
            }
        }
    });
    ui.weak("画布操作：点击选图 · 按住拖动移动 · 滚轮缩放当前选中图片");

    // 输出开关：模块级 + 每张图各自一个。两张都开才会写进 G-code。
    ui.separator();
    ui.checkbox(&mut app.images_on, "并入当前页面（写入 G-code）");
    if !app.images_on {
        ui.colored_label(
            egui::Color32::from_rgb(200, 120, 40),
            "默认不并入：上面的开关不开，任何图片都不会写进 G-code（画布上仍可编辑）。",
        );
    } else {
        ui.weak("已开启：只有下面勾了「输出」的那几张才会真正输出。");
    }

    // 图片列表（支持多张重叠）
    ui.separator();
    let n = app.images.len();
    if n == 0 {
        ui.weak("尚未导入任何图片");
    } else {
        ui.label(format!("共 {} 张图片（点击选中后在下方调参）：", n));
        // 每张一行：选中名 + 类型 + 独立删除按钮，避免只靠一个「删除选中」误删
        let mut del: Option<usize> = None;
        for i in 0..n {
            ui.horizontal(|ui| {
                let bg = app.images[i].background;
                let label = format!("#{} {}", i + 1, if bg { "背景" } else { "前景" });
                if ui.selectable_label(app.sel_image == i, label).clicked() {
                    app.sel_image = i;
                }
                if !bg {
                    // 每张图自己的输出开关：只有勾上的才并入
                    ui.checkbox(&mut app.images[i].out, "输出")
                        .on_hover_text("勾上才会写进 G-code（模块总开关也要开）");
                }
                if ui.small_button("✕").on_hover_text("删除这张图片").clicked() {
                    del = Some(i);
                }
            });
        }
        if let Some(i) = del {
            let name = app.images[i].path.clone();
            app.remove_image(i);
            app.status = format!("已删除图片：{}", name);
        }

        // 一键清除全部（二次确认）
        ui.separator();
        let btn = if app.confirm_clear {
            egui::Button::new(
                RichText::new("确认清除全部图片？").color(egui::Color32::WHITE),
            )
            .fill(egui::Color32::from_rgb(190, 70, 60))
            .min_size(egui::Vec2::new(200.0, 28.0))
        } else {
            egui::Button::new("清除全部图片")
                .min_size(egui::Vec2::new(200.0, 28.0))
        };
        if ui.add(btn).clicked() {
            if app.confirm_clear {
                app.clear_images();
                app.confirm_clear = false;
                app.status = "已清除全部图片".into();
            } else {
                app.confirm_clear = true;
                app.status = "再点一次确认清除全部图片".into();
            }
        }
    }

    // 选中图片的变换与描边参数
    let sel = app.sel_image;
    if sel < app.images.len() {
        ui.separator();
        section(ui, &format!("选中图片 #{} 的变换", sel + 1));

        // —— 缩放 ——
        ui.label(RichText::new("缩放").strong());
        let cur_scale = app.images[sel].scale;
        ui.add(egui::Slider::new(&mut app.images[sel].scale, 0.05..=8.0).text("缩放(×)"));
        // 滑块直接改 scale 会让左上角锚点漂移，这里换算回「中心不动」
        let sc = app.images[sel].scale;
        if (sc - cur_scale).abs() > 1e-9 {
            app.set_image_scale(sel, sc);
        }
        ui.horizontal(|ui| {
            if ui.button("− 缩小").clicked() {
                app.scale_image(sel, 1.0 / 1.1);
            }
            if ui.button("+ 放大").clicked() {
                app.scale_image(sel, 1.1);
            }
            if ui.button("适应页面").clicked() {
                app.fit_image_to_page(sel);
                app.status = "已缩放至适应页面".into();
            }
            if ui.button("原始尺寸").clicked() {
                app.set_image_scale(sel, 1.0);
                app.status = "已恢复原始尺寸".into();
            }
        });

        // —— 位置 ——
        ui.separator();
        ui.label(RichText::new("位置（左上角，毫米）").strong());
        let (mut px, mut py) = {
            let im = &app.images[sel];
            (im.x, im.y)
        };
        ui.add(egui::Slider::new(&mut px, -200.0..=400.0).text("X(mm)"));
        ui.add(egui::Slider::new(&mut py, -200.0..=500.0).text("Y(mm)"));
        app.set_image_pos(sel, px, py);
        let step = if ui.input(|i| i.modifiers.shift) { 10.0 } else { 1.0 };
        ui.horizontal(|ui| {
            if ui.button("←").clicked() {
                app.nudge_image(sel, -step, 0.0);
            }
            if ui.button("→").clicked() {
                app.nudge_image(sel, step, 0.0);
            }
            if ui.button("↑").clicked() {
                app.nudge_image(sel, 0.0, -step);
            }
            if ui.button("↓").clicked() {
                app.nudge_image(sel, 0.0, step);
            }
            if ui.button("页面居中").clicked() {
                app.center_image(sel);
                app.status = "已居中".into();
            }
        });
        ui.weak(format!("按住 Shift 点击方向键 = 每次 10mm（当前 {}mm）", step));

        // —— 旋转 / 镜像 ——
        ui.separator();
        ui.add(egui::Slider::new(&mut app.images[sel].rotation, -180.0..=180.0).text("旋转(度)"));
        ui.horizontal(|ui| {
            ui.checkbox(&mut app.images[sel].flip_h, "水平翻转");
            ui.checkbox(&mut app.images[sel].flip_v, "垂直翻转");
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut app.images[sel].background, "作为背景参考（只临摹不打印）");
            if !app.images[sel].background {
                ui.checkbox(&mut app.images[sel].out, "并入输出");
            }
        });
        ui.horizontal(|ui| {
            if ui.button("顺时针 90°").clicked() {
                app.images[sel].rotation += 90.0;
            }
            if ui.button("重置变换").clicked() {
                app.reset_image_transform(sel);
                app.status = "已重置变换并居中".into();
            }
        });

        // —— 描边提取参数 ——
        ui.separator();
        ui.label(RichText::new("描边提取参数").strong());
        let mut dt = app.images[sel].draw as usize;
        ui.horizontal(|ui| {
            ui.label("绘制类型:");
            for (i, name) in ImgDraw::names().iter().enumerate() {
                ui.selectable_value(&mut dt, i, *name);
            }
        });
        app.images[sel].draw = ImgDraw::from_usize(dt);
        ui.add(egui::Slider::new(&mut app.images[sel].step_px, 1..=12).text("影线间隔(px)"));
        ui.add(egui::Slider::new(&mut app.images[sel].threshold, 0.1..=0.95).text("暗部阈值(素描)"));
        ui.add(egui::Slider::new(&mut app.images[sel].edge_threshold, 0.05..=0.9).text("边缘阈值(线框)"));
        ui.checkbox(&mut app.images[sel].invert, "反相（黑变白）");

        // 参数变化需要重算基础笔迹
        app.regenerate_image(sel);

        let (ix, iy, nw, nh, strokes) = {
            let im = &app.images[sel];
            (im.x, im.y, im.natural_w * im.scale, im.natural_h * im.scale, im.base.len())
        };
        ui.weak(format!(
            "位置 ({:.1},{:.1}) · 输出尺寸 {:.1}×{:.1} mm · {} 条笔迹",
            ix, iy, nw, nh, strokes
        ));
    }

    // 默认导入参数（作用于后续导入）
    ui.separator();
    section(ui, "默认导入参数");
    ui.add(egui::Slider::new(&mut app.img_target_h, 30.0..=250.0).text("目标高度(mm)"));
    ui.add(egui::Slider::new(&mut app.img_step, 1..=12).text("影线间隔(px)"));
    ui.add(egui::Slider::new(&mut app.img_thr, 0.1..=0.95).text("暗部阈值(素描)"));
    ui.add(egui::Slider::new(&mut app.img_edge, 0.05..=0.9).text("边缘阈值(线框)"));
    ui.checkbox(&mut app.img_invert, "反相");
    let mut dt = app.img_draw_type;
    ui.horizontal(|ui| {
        ui.label("默认类型:");
        for (i, name) in ImgDraw::names().iter().enumerate() {
            ui.selectable_value(&mut dt, i, *name);
        }
    });
        app.img_draw_type = dt;
        if dt == 2 {
            ui.weak("连续笔画：把相邻影线串起来画，抬笔次数最少、绘制最快；行间距建议大于 XY 最小步进。");
        }
        ui.weak("以上为默认值；单张图片可在上方单独覆盖");
}

/// 运行中页：实时进度（当前行/总行、已用/剩余时间、进度页）与控制。
fn panel_running(app: &mut AppState, ui: &mut Ui) {
    section(ui, "⑩ 运行中");
    let snap = app.device.snapshot();

    ui.horizontal(|ui| {
        ui.label(
            RichText::new(if app.dev_connected { "● 已连接" } else { "○ 未连接" })
                .color(if app.dev_connected {
                    egui::Color32::from_rgb(60, 170, 90)
                } else {
                    egui::Color32::from_gray(150)
                }),
        );
        let state = if snap.running {
            if snap.paused {
                "已暂停"
            } else {
                "运行中"
            }
        } else {
            "空闲"
        };
        ui.label(format!("状态：{state}"));
    });
    ui.label(format!(
        "G-code 进度：第 {} / {} 行",
        snap.done_lines, snap.total_lines
    ));

    let elapsed = snap.elapsed_ms as f64 / 1000.0;
    let remain = if snap.total_lines > 0 && snap.done_lines > 0 && snap.running {
        elapsed / snap.done_lines as f64 * (snap.total_lines - snap.done_lines) as f64
    } else {
        0.0
    };
    ui.label(format!("已用时间：{:.1}s", elapsed));
    ui.label(format!("预计剩余：{:.1}s", remain));

    let frac = if snap.total_lines > 0 {
        snap.done_lines as f64 / snap.total_lines as f64
    } else {
        0.0
    };
    ui.add(egui::ProgressBar::new(frac as f32).text(format!("{:.0}%", frac * 100.0)));

    ui.separator();
    ui.horizontal(|ui| {
        if ui.button("开始运行").clicked() {
            let g = app.generate_gcode();
            app.gcode_text = g.clone();
            app.start_run(g);
            app.status = "已启动运行（见左侧「运行中」页）".into();
        }
        if snap.running && !snap.paused {
            if ui.button("暂停").clicked() {
                app.pause_run();
            }
        } else if snap.paused {
            if ui.button("继续").clicked() {
                app.resume_run();
            }
        }
        if ui.button("停止").clicked() {
            app.stop_run();
        }
    });
    ui.weak("运行中可切换到其他页继续编辑，绘制进度互不干扰");

    ui.separator();
    ui.label("当前 G-code（运行位置附近）：");
    if app.gcode_text.is_empty() {
        ui.weak("（点击「开始运行」后显示）");
    } else {
        let lines: Vec<&str> = app.gcode_text.lines().collect();
        let cur = snap.done_lines.min(lines.len());
        let start = cur.saturating_sub(8);
        let end = (cur + 8).min(lines.len());
        let mut view = String::new();
        for (i, l) in lines.iter().enumerate().take(end).skip(start) {
            let mark = if i + 1 == cur { "▶ " } else { "  " };
            view.push_str(&format!("{}{:>4}: {}\n", mark, i + 1, l));
        }
        ui.add(TextEdit::multiline(&mut view).desired_rows(9).interactive(false));
    }
}

/// 页面边框（对应源程序 edge 模块）。
fn panel_border(app: &mut AppState, ui: &mut Ui) {
    section(ui, "页面边框");
    ui.checkbox(&mut app.border_on, "绘制边框");
    ui.add_enabled(
        app.border_on,
        egui::Slider::new(&mut app.border_margin, 0.0..=40.0).text("页边距(mm)"),
    );
    ui.horizontal(|ui| {
        ui.label("样式:");
        for (i, nm) in BORDER_STYLES.iter().enumerate() {
            ui.selectable_value(&mut app.border_style, i, *nm);
        }
    });
}

fn panel_export(app: &mut AppState, ui: &mut Ui) {
    section(ui, "固件 / 生成");
    ui.horizontal(|ui| {
        ui.label("固件:");
        ui.selectable_value(&mut app.firmware_sel, 0, "GRBL");
        ui.selectable_value(&mut app.firmware_sel, 1, "Marlin");
        ui.selectable_value(&mut app.firmware_sel, 2, "EBB");
    });
    ui.horizontal(|ui| {
        ui.label("笔控:");
        ui.selectable_value(&mut app.pen_mode_sel, 0, "Z轴");
        ui.selectable_value(&mut app.pen_mode_sel, 1, "舵机");
    });
    ui.add(egui::Slider::new(&mut app.feed_rate, 500.0..=30000.0).text("进给(mm/min)"));
    ui.add(egui::Slider::new(&mut app.z_speed, 100.0..=30000.0).text("Z轴速度"));
    ui.add(egui::Slider::new(&mut app.pen_up, -10.0..=20.0).text("抬笔"));
    ui.add(egui::Slider::new(&mut app.pen_down, -10.0..=20.0).text("落笔"));

    // 不常用设置已移到「设置」对话框（激光/休眠/延时/坐标系/自定义 G-code/逼真化等）
    ui.horizontal(|ui| {
        if ui.button("⚙ 更多设置").clicked() {
            app.settings_open = true;
        }
        ui.weak("（激光、坐标系、自定义 G-code 等）");
    });

    if ui.button("生成 G-code").clicked() {
        app.gcode_text = app.generate_gcode();
        app.status = "已生成 G-code".into();
    }
    ui.separator();
    ui.label(RichText::new("导出（可自选位置与文件名）").strong());
    if ui.button("导出 .nc (G-code)").clicked() {
        if let Some(p) = app.save_file_as("G-code", &["nc", "gcode"], "output.nc") {
            match io::export_gcode(app, &p) {
                Ok(()) => app.status = format!("G-code 已导出：{}", p.display()),
                Err(e) => app.status = format!("导出失败：{e}"),
            }
        }
    }
    if ui.button("导出 SVG").clicked() {
        if let Some(p) = app.save_file_as("SVG", &["svg"], "output.svg") {
            match io::export_svg(app, &p) {
                Ok(()) => app.status = format!("SVG 已导出：{}", p.display()),
                Err(e) => app.status = format!("导出失败：{e}"),
            }
        }
    }
    if ui.button("导出 PDF（矢量）").clicked() {
        if let Some(p) = app.save_file_as("PDF", &["pdf"], "output.pdf") {
            match io::export_pdf(app, &p) {
                Ok(()) => app.status = format!("PDF 已导出：{}", p.display()),
                Err(e) => app.status = format!("导出失败：{e}"),
            }
        }
    }
    if ui.button("导出 PNG").clicked() {
        if let Some(p) = app.save_file_as("PNG", &["png"], "output.png") {
            match io::export_png(app, &p) {
                Ok(()) => app.status = format!("PNG 已导出：{}", p.display()),
                Err(e) => app.status = format!("导出失败：{e}"),
            }
        }
    }
    ui.separator();
    ui.label("G-code 预览（前 30 行）：");
    if app.gcode_text.is_empty() {
        ui.label(RichText::new("（点击「生成 G-code」）").color(muted()));
    } else {
        let mut head: String = app.gcode_text.lines().take(30).collect::<Vec<_>>().join("\n");
        ui.add(TextEdit::multiline(&mut head).desired_rows(8).interactive(false));
    }
}
