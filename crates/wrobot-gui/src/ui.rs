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
use crate::theme;

const MODULES: &[&str] = &[
    "文字",
    "表格",
    "练字格",
    "手绘",
    "笔记",
    "图片",
    "设备与调试",
    "机器测试",
    "项目与输出",
    "运行监控",
];
const MODULE_DESC: &[&str] = &[
    "多行文字排版，支持多文本框、逐框字体与随机手写",
    "按行列生成表格，单元格文字自动排布",
    "田字格 / 米字格 / 回宫格，可带示范字",
    "鼠标自由手绘，或导入 DXF / SVG 矢量",
    "页面上任意位置的自由文本块",
    "照片影线描边，可作背景只临摹不打印",
    "连接机器、蓝牙链路诊断、坐标与点动",
    "解锁 / 回零 / 抬落笔 / 测试方框",
    "工程、PDF / 字库导入，固件参数与导出",
    "实时进度、耗时统计与 G-code 跟踪",
];
const CONTENT_MODULES: &[&str] = &["文字", "表格", "练字格", "手绘", "笔记"];

/// 侧栏导航分组：(组名, [(tab, 图标, 名称, 说明)])
const NAV_GROUPS: &[(&str, &[(usize, &str, &str, &str)])] = &[
    (
        "创作",
        &[
            (0, "文", "文字", "多行排版 / 多文本框"),
            (1, "表", "表格", "行列与单元格文字"),
            (2, "格", "练字格", "田字格 / 米字格"),
            (4, "记", "笔记", "自由文本块"),
        ],
    ),
    (
        "图形",
        &[
            (5, "图", "图片", "照片描边 / 背景参照"),
            (3, "绘", "手绘", "鼠标书写 / 矢量导入"),
        ],
    ),
    (
        "设备",
        &[
            (6, "机", "设备与调试", "连接 / 蓝牙 / 坐标"),
            (9, "运", "运行监控", "进度 / 耗时 / G-code"),
        ],
    ),
    (
        "项目",
        &[(8, "出", "项目与输出", "导入 / 固件 / 导出")],
    ),
];
const ORIGINS: &[&str] = &["左上", "右上", "左下", "右下"];
const BORDER_STYLES: &[&str] = &["实线", "虚线", "双线"];

/// 当前主题调色板。
fn pal(ui: &Ui) -> theme::Palette {
    theme::palette(ui.visuals().dark_mode)
}

/// 次级文字色。
fn muted() -> egui::Color32 {
    egui::Color32::from_gray(140)
}

/// 分区标题（沿用旧调用点，内部走统一主题）。
fn section(ui: &mut Ui, title: &str) {
    let p = pal(ui);
    theme::section(ui, &p, title, None);
}

/// 参数栏的通用宽度适配。
///
/// 主题里滑块轨道是 150pt，而侧栏最窄只有 300pt：长中文标签 + 数值 + 轨道
/// 会顶出栏外被裁掉。按当前可用宽度收窄轨道，保证整行都落在栏内。
fn fit_params_width(ui: &mut Ui) {
    ui.spacing_mut().slider_width = (ui.available_width() - 210.0).clamp(60.0, 150.0);
}

/// 秒 → 「1 分 23 秒」。
fn fmt_duration(secs: f64) -> String {
    if !secs.is_finite() || secs <= 0.0 {
        return "—".into();
    }
    let s = secs.round() as u64;
    if s >= 3600 {
        format!("{} 时 {} 分", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{} 分 {} 秒", s / 60, s % 60)
    } else {
        format!("{} 秒", s)
    }
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
                            app.status = format!("已读取 {} 页，请在「项目与输出」页选页码后导入", n);
                            app.tab = 8; // 直接跳到项目与输出页
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
                app.start_port_scan();
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
            let mut dark = app.dark_mode;
            if ui.checkbox(&mut dark, "深色主题").changed() {
                app.dark_mode = dark;
                app.theme_applied = None;
            }
            ui.checkbox(&mut app.log_open, "底部实时日志栏");
            ui.separator();
            ui.checkbox(&mut app.show_left, "显示左侧导航栏");
            ui.checkbox(&mut app.show_right, "显示右侧参数栏");
            ui.weak("隐藏两栏可把画布放到最大；快捷键 Ctrl+\\ 也可切换。");
            ui.separator();
            ui.label(RichText::new("界面缩放").size(11.0).color(muted()));
            ui.add(egui::Slider::new(&mut app.ui_scale, 0.7..=2.0).text("×"));
            if ui.button("恢复 1.0×").clicked() {
                app.ui_scale = 1.0;
            }
            ui.separator();
            if ui.button("适应页面 / 复位缩放").clicked() {
                app.reset_view();
                ui.close();
            }
            if ui.button("快速上手（F1）").clicked() {
                app.help_open = true;
                ui.close();
            }
            if ui.button("设置…").clicked() {
                app.settings_open = true;
                ui.close();
            }
        });

        // ── 关于 ──
        if ui.button("关于").clicked() {
            app.about_open = true;
        }

        // 右侧常驻状态
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {            let (dot, col, txt) = if app.dev_connected {
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
///
/// 先打开「绘制前计算」窗口：由用户选择路径优化方式，后台算好代码量与
/// 预计耗时后，再确认是否下发（超界仍会弹窗二次确认）。
fn act_start_draw(app: &mut AppState) {
    if app.pen_running {
        app.status = "多笔队列进行中：请先完成或取消当前队列".into();
        return;
    }
    app.start_run_plan();
}

/// 边界检查通过（或用户确认）后真正开始运行。
fn finish_start(app: &mut AppState, g: String) {
    let n = g.lines().filter(|l| !l.trim().is_empty()).count();
    // 「绘制前计算」带来的已排序笔迹：直接用，避免重新生成（顺序也保持一致）。
    if let Some(pv) = app.pending_run_paths.take() {
        let tile = app.tile_on.then(|| app.tile_layout()).flatten();
        if let Some(layout) = tile {
            let page = app.tile_page_clamped();
            app.start_run_with_preview(g, pv);
            app.status = format!(
                "已开始传输第 {}/{} 页（{} 行）· 本页画完回到原点后再放下一张纸",
                page + 1,
                layout.count(),
                n
            );
        } else {
            app.start_run_with_preview(g, pv);
            app.status = format!("已开始传输 {} 行（进度见「运行监控」页）", n);
        }
        return;
    }
    if app.tile_on {
        if let Some(layout) = app.tile_layout() {
            let page = app.tile_page_clamped();
            let pv = app.tiled_page_polylines(page);
            app.start_run_with_preview(g, pv);
            app.status = format!(
                "已开始传输第 {}/{} 页（{} 行）· 本页画完回到原点后再放下一张纸",
                page + 1,
                layout.count(),
                n
            );
            return;
        }
    }
    app.start_run(g);
    app.status = format!("已开始传输 {} 行（进度见「运行监控」页，传输结果见左下角）", n);
}

/// 用户确认「绘制前计算」方案后的处理：先做边界自检，再真正开始。
fn confirm_run_plan(app: &mut AppState) {
    let Some(plan) = app.run_plan.take() else {
        return;
    };
    app.show_run_plan = false;
    app.run_plan_busy = false;
    app.run_plan_rx = None;
    if plan.pen_jobs.len() >= 2 {
        // 多笔：先把队列建好（等用户装第一支笔），再决定是否提示越界。
        app.begin_run_plan(plan);
        let g = app.gcode_text.clone();
        if app.bounds_check {
            if let Some(msg) = app.bounds_warning(&g) {
                app.pending_run_gcode = g;
                app.bounds_warn_msg = msg;
                app.show_bounds_warn = true;
                app.status = "⚠ 内容超出机器边界，已暂停：请在弹窗中确认是否仍然执行".into();
            }
        }
        return;
    }
    if app.bounds_check {
        if let Some(msg) = app.bounds_warning(&plan.gcode) {
            app.pending_run_gcode = plan.gcode;
            app.pending_run_paths = Some(plan.paths);
            app.bounds_warn_msg = msg;
            app.show_bounds_warn = true;
            app.status = "⚠ 内容超出机器边界，已暂停：请在弹窗中确认是否仍然执行".into();
            return;
        }
    }
    app.begin_run_plan(plan);
}

/// 「绘制前计算」窗口：确认路径优化、代码量与预计耗时后再开始。
fn run_plan_window(app: &mut AppState, ctx: egui::Context) {
    if !app.show_run_plan {
        return;
    }
    app.poll_run_plan();
    let mut open = true;
    let mut start = false;
    let mut close = false;
    let mut recompute = false;
    let mut order = app.run_plan_order;
    let busy = app.run_plan_busy;
    let plan = app.run_plan.clone();
    let sig_now = app.plan_sig();
    let stale = plan.as_ref().map(|pl| pl.sig) != Some(sig_now);
    egui::Window::new("绘制前计算")
        .open(&mut open)
        .resizable(false)
        .collapsible(false)
        .default_width(480.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(&ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("路径优化");
                egui::ComboBox::from_id_salt("run_plan_order")
                    .selected_text(wrobot_core::PathOrder::from_u8(order).label())
                    .width(250.0)
                    .show_ui(ui, |ui| {
                        for v in 0..=4u8 {
                            if ui
                                .selectable_value(
                                    &mut order,
                                    v,
                                    wrobot_core::PathOrder::from_u8(v).label(),
                                )
                                .changed()
                            {
                                recompute = true;
                            }
                        }
                    });
            });
            ui.weak("只调整绘制顺序与起笔端，不增删任何笔迹。改选后会自动重新计算。");
            ui.separator();

            if busy {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("正在排序路径、抽稀并生成 G-code…");
                });
                return;
            }
            let Some(pl) = plan.as_ref() else {
                ui.label("尚未计算。");
                if ui.button("开始计算").clicked() {
                    recompute = true;
                }
                return;
            };
            let p = pal(ui);

            egui::Grid::new("run_plan_stats")
                .num_columns(2)
                .spacing([18.0, 6.0])
                .show(ui, |ui| {
                    ui.label("笔迹");
                    ui.label(format!("{} 笔 · {} 点", pl.strokes, pl.points));
                    ui.end_row();
                    ui.label("G-code");
                    ui.label(format!(
                        "{} 行 · {:.1} KB",
                        pl.lines,
                        pl.bytes as f64 / 1024.0
                    ));
                    ui.end_row();
                    ui.label("墨线 / 空移");
                    ui.label(format!(
                        "{:.2} m / {:.1} m",
                        pl.ink_mm / 1000.0,
                        pl.travel_mm / 1000.0
                    ));
                    ui.end_row();
                    ui.label("预计耗时");
                    ui.label(
                        RichText::new(fmt_duration(pl.seconds))
                            .strong()
                            .color(p.accent),
                    );
                    ui.end_row();
                });
            if pl.pen_jobs.len() >= 2 {
                ui.label(format!("多笔分层：共 {} 支笔，按顺序换笔绘制。", pl.pen_jobs.len()));
            }
            if let Some(page) = pl.tile_page {
                ui.label(format!("分页绘制：本次只输出第 {} 页。", page + 1));
            }
            ui.weak(
                "估算按快速空移、落笔进给、加减速与每行指令开销计算；实际耗时受机器与纸张影响。",
            );
            if stale {
                ui.colored_label(
                    egui::Color32::from_rgb(200, 90, 30),
                    "内容或参数已改变，方案已过期——请重新计算。",
                );
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !stale,
                        egui::Button::new(RichText::new("开始绘制").strong()),
                    )
                    .clicked()
                {
                    start = true;
                }
                if ui.button("重新计算").clicked() {
                    recompute = true;
                }
                if ui.button("取消").clicked() {
                    close = true;
                }
            });
        });
    if order != app.run_plan_order {
        app.run_plan_order = order;
    }
    if start {
        confirm_run_plan(app);
    } else if recompute {
        app.recompute_run_plan(order);
    } else if close || !open {
        app.close_run_plan();
    }
}

/// 超界确认弹窗：列出超出的方向与数值，用户可选「仍然执行」或「取消」。
fn bounds_warn_window(app: &mut AppState, ctx: egui::Context) {
    if !app.show_bounds_warn {
        return;
    }
    // 先拷出一份开关：`.open()` 要独占借用，闭包里还要用 `app`
    let mut open = app.show_bounds_warn;
    // `.open(&mut open)` 会借用 open，闭包里不能再改它，改用独立标志记录用户选择
    let mut confirmed = false; // 点了「仍然执行」
    let mut cancelled = false; // 点了「取消」
    egui::Window::new("⚠ 内容超出机器边界")
        .open(&mut open)
        .resizable(false)
        .collapsible(false)
        .default_width(420.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(&ctx, |ui| {
            ui.colored_label(
                egui::Color32::from_rgb(200, 90, 30),
                RichText::new(&app.bounds_warn_msg).strong(),
            );
            ui.separator();
            ui.weak(format!(
                "机器幅面设置：{:.0} × {:.0} mm（可在「设置 → 机器幅面」修改）",
                app.machine_w_mm, app.machine_h_mm
            ));
            ui.weak("超界运行可能导致撞机 / 软限位报警 / 皮带丢步，请确认纸张与原点位置。");
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let g = app.pending_run_gcode.clone();
                if ui.button(RichText::new("仍然执行").strong()).clicked() {
                    confirmed = true;
                    app.pending_run_gcode.clear();
                    app.bounds_warn_msg.clear();
                    // 多笔队列已经在等第一支笔：确认越界后不要绕过队列直接跑。
                    if app.pen_running && app.pen_wait {
                        app.status = "已确认越界。装入第一支笔后点「开始这支笔」".into();
                    } else {
                        finish_start(app, g);
                    }
                }
                if ui.button("取消").clicked() {
                    cancelled = true;
                    app.pending_run_gcode.clear();
                    app.pending_run_paths = None;
                    app.bounds_warn_msg.clear();
                    if app.pen_running {
                        app.cancel_pen_jobs();
                    }
                }
            });
        });
    // 收尾：点了按钮、或点了右上角 ×（open 变 false）都关窗；
    // 已确认的保留「已开始传输」状态，其余记为取消。
    if !open || confirmed || cancelled {
        app.show_bounds_warn = false;
        app.pending_run_gcode.clear();
        app.pending_run_paths = None;
        app.bounds_warn_msg.clear();
        if !confirmed {
            app.status = "已取消运行：内容超出机器边界".into();
        }
    }
}

/// 导入图片的「尺寸 / 精细度」确认窗口。
///
/// 以前选完文件就直接按默认高度生成，大图被静默压到固定长边，用户拿到的是
/// 简化过的结果。现在把原图尺寸、成品毫米尺寸和实际工作分辨率摆在一起，
/// 由用户选择目标高度与精细度后再生成。
fn import_window(app: &mut AppState, ctx: egui::Context) {
    let Some(p) = app.pending_import.as_ref() else {
        return;
    };
    let background = p.background;
    let src_w = p.src_w;
    let src_h = p.src_h;
    let file_name = p
        .path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| p.path.display().to_string());
    let mut target_h = p.target_h;
    let mut max_side = p.max_side;
    let page_w = app.machine_w_mm;
    let page_h = app.machine_h_mm;
    let draw_type = app.img_draw_type;
    let size_cap = matches!(draw_type, 3 | 4); // 中心线 / 描边固定封顶

    let long = src_w.max(src_h).max(1);
    let work_side = if max_side == 0 {
        long
    } else {
        max_side.min(long)
    };
    let k = work_side as f64 / long as f64;
    let work_w = (src_w as f64 * k).round().max(1.0) as u32;
    let work_h = (src_h as f64 * k).round().max(1.0) as u32;
    let out_w = target_h * src_w as f64 / src_h.max(1) as f64;

    let mut open = true;
    let mut confirmed = false;
    let mut cancelled = false;
    egui::Window::new(if background {
        "导入背景参考图"
    } else {
        "导入前景图片"
    })
    .open(&mut open)
    .resizable(false)
    .collapsible(false)
    .default_width(430.0)
    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
    .show(&ctx, |ui| {
        ui.weak(&file_name);
        ui.label(format!("原图：{} × {} px", src_w, src_h));
        ui.separator();

        ui.label(RichText::new("尺寸（成品高度）").strong());
        ui.horizontal_wrapped(|ui| {
            for h in [100.0, 150.0, 200.0, 250.0, 300.0] {
                if ui.selectable_label((target_h - h).abs() < 0.5, format!("{h:.0}mm")).clicked() {
                    target_h = h;
                }
            }
            if page_w > 20.0 && page_h > 20.0 {
                // 按原图比例取「宽高都不超过纸面留边」的最大高度，避免竖图
                // 选「纸面高」后左右溢出。
                let fit_h = ((page_w - 20.0) * src_h as f64 / src_w.max(1) as f64)
                    .min(page_h - 20.0)
                    .max(20.0);
                if ui
                    .selectable_label((target_h - fit_h).abs() < 0.5, "适应纸面")
                    .on_hover_text(format!(
                        "机器幅面 {page_w:.0}×{page_h:.0}mm，四周留 10mm 边"
                    ))
                    .clicked()
                {
                    target_h = fit_h;
                }
            }
        });
        ui.add(egui::Slider::new(&mut target_h, 30.0..=400.0).suffix(" mm"));
        ui.weak(format!(
            "成品尺寸约 {:.1} × {:.1} mm（宽 × 高）",
            out_w, target_h
        ));
        if out_w > page_w + 1e-6 || target_h > page_h + 1e-6 {
            ui.colored_label(
                egui::Color32::from_rgb(200, 120, 30),
                format!(
                    "超出机器幅面 {:.0} × {:.0} mm：导入后可在画布上缩放，或启用分页绘制",
                    page_w, page_h
                ),
            );
        }
        ui.separator();

        ui.label(RichText::new("精细度（工作分辨率）").strong());
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut max_side, 0, "原图");
            for v in [3600u32, 3000, 2400, 1800] {
                ui.selectable_value(&mut max_side, v, format!("{v}px"));
            }
        });
        ui.weak(format!(
            "算法实际处理 {work_w} × {work_h} px（{}）",
            if max_side == 0 || work_side == long {
                "原分辨率".to_string()
            } else {
                format!("长边压到 {work_side}px")
            }
        ));
        if max_side != 0 && max_side < long {
            ui.weak(format!(
                "原图长边 {long}px，当前会丢掉一部分像素细节；追细节可选「原图」。"
            ));
        }
        if size_cap {
            ui.weak("中心线 / 描边为保证界面响应，最高按 1600px 处理。");
        }
        if max_side == 0 && long > 5000 {
            ui.colored_label(
                egui::Color32::from_rgb(200, 120, 30),
                "原图很大，生成可能需要较久；卡住时可选 3600px。",
            );
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button(RichText::new("导入并生成").strong()).clicked() {
                confirmed = true;
            }
            if ui.button("取消").clicked() {
                cancelled = true;
            }
        });
    });

    if confirmed {
        if let Some(p) = app.pending_import.as_mut() {
            p.target_h = target_h;
            p.max_side = max_side;
        }
        match app.confirm_import() {
            Ok(n) => {
                app.status = if background {
                    format!("已导入背景参考图（{} 条笔迹，浅色显示不打印）", n)
                } else {
                    format!("已导入前景图片（{} 条笔迹）", n)
                }
            }
            Err(e) => app.status = format!("导入失败：{e}"),
        }
    } else if cancelled || !open {
        app.pending_import = None;
    }
}

/// 多笔绘制：每支笔开始前的换笔确认窗口。
///
/// **不能自动连发下一支**：设备线程的「运行结束」只代表最后一行指令已发出，
/// 笔架可能还在减速；换笔时手会伸到机器里。所以这里必须由人确认。
fn pen_swap_window(app: &mut AppState, ctx: egui::Context) {
    if !app.pen_wait || !app.pen_running {
        return;
    }
    let Some(job) = app.pen_jobs.get(app.pen_job_idx) else {
        app.cancel_pen_jobs();
        return;
    };
    let idx = app.pen_job_idx;
    let total = app.pen_jobs.len();
    let name = job.name.clone();
    let pen = job.pen;
    let paths = job.paths.len();
    let ink_mm = wrobot_core::total_length(&job.paths);
    let spec = app
        .active_pens()
        .get(pen)
        .map(|p| (p.width_mm, p.darkness, p.color, p.offset_mm));
    let mut open = true;
    let mut start = false;
    let mut cancel = false;
    egui::Window::new("换笔")
        .open(&mut open)
        .resizable(false)
        .collapsible(false)
        .default_width(420.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(&ctx, |ui| {
            ui.heading(format!("第 {}/{} 支笔：{name}", idx + 1, total));
            if let Some((w, d, c, off)) = spec {
                ui.horizontal(|ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::Vec2::splat(18.0), egui::Sense::hover());
                    ui.painter().rect_filled(
                        rect,
                        3.0,
                        egui::Color32::from_rgb(c[0], c[1], c[2]),
                    );
                    ui.label(format!(
                        "线宽 {w:.2}mm · 可压深 {:.0}% · {paths} 根笔迹 · 墨线 {ink_mm:.0}mm",
                        d * 100.0
                    ));
                });
                if off[0] != 0.0 || off[1] != 0.0 {
                    ui.weak(format!("套准偏移：X {:.2}mm / Y {:.2}mm", off[0], off[1]));
                }
            }
            ui.separator();
            ui.label(RichText::new("装入这支笔，并确认：").strong());
            ui.label("1. 机器已完全停稳（进度 100% 只代表指令发送完毕，笔架可能还在走）");
            ui.label("2. 笔尖已落到页面原点附近、抬笔高度正常");
            ui.label("3. 旧笔已取下，新笔夹紧");
            ui.add_space(4.0);
            ui.weak(
                "每支笔的 G-code 都以当前位置为零点重新锚定，换笔不需要重新对刀；\
                 若落点有系统性偏差，可在「图片 → 多笔分层」里调该笔的套准偏移。",
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .add(egui::Button::new(RichText::new("开始这支笔").strong()))
                    .clicked()
                {
                    start = true;
                }
                if ui.button("取消剩余笔").clicked() {
                    cancel = true;
                }
            });
        });
    if start {
        app.start_pen_job();
    } else if cancel || !open {
        app.cancel_pen_jobs();
    }
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
            Ok(()) => {
                // 多笔导出会在 export_gcode 里写好更具体的状态（含分笔文件数），别覆盖掉。
                if !(matches!(kind, Export::Nc) && app.status.contains("分笔")) {
                    app.status = format!("{} 已导出：{}", name, p.display());
                }
            }
            Err(e) => app.status = format!("导出失败：{e}"),
        }
    }
}

/// 底部「实时日志」栏：所有页面都能看到设备通信与操作回显。
fn log_panel(app: &mut AppState, ui: &mut Ui) -> bool {
    let mut hide = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new("实时日志").strong());
        ui.weak("（设备通信 / 下发回显 / 错误诊断都在这里）");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("清空").clicked() {
                app.device.clear_log();
                app.dev_log.clear();
            }
            if ui.button("隐藏").clicked() {
                hide = true;
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
    hide
}

pub fn show(app: &mut AppState, ui: &mut Ui, _frame: &mut eframe::Frame) {
    // 界面缩放：在显示器原生缩放之上再乘用户设定的倍率（变化时才真正下发，避免每帧重排）。
    let native = ui.ctx().native_pixels_per_point().unwrap_or(1.0);
    let want = (native * app.ui_scale.clamp(0.6, 3.0)).clamp(0.5, 4.0);
    if (ui.ctx().pixels_per_point() - want).abs() > 0.01 {
        ui.ctx().set_pixels_per_point(want);
    }

    // 主题只在切换时重设一次（每帧 set_style 会让滚动条/动画状态被重置）。
    if app.theme_applied != Some(app.dark_mode) {
        theme::apply(ui.ctx(), app.dark_mode);
        app.theme_applied = Some(app.dark_mode);
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

    // 快捷键：Ctrl+S 保存工程 / Ctrl+O 打开工程 / Ctrl+E 导出 G-code / F1 帮助
    // Ctrl+\ 显示/隐藏左右侧栏（画布最大化）
    let (mut k_save, mut k_open, mut k_export, mut k_help, mut k_side) =
        (false, false, false, false, false);
    ui.ctx().input_mut(|i| {
        k_save = i.consume_key(egui::Modifiers::CTRL, egui::Key::S);
        k_open = i.consume_key(egui::Modifiers::CTRL, egui::Key::O);
        k_export = i.consume_key(egui::Modifiers::CTRL, egui::Key::E);
        k_help = i.consume_key(egui::Modifiers::NONE, egui::Key::F1);
        k_side = i.consume_key(egui::Modifiers::CTRL, egui::Key::Backslash);
    });
    if k_side {
        let show = !(app.show_left && app.show_right);
        app.show_left = show;
        app.show_right = show;
        app.status = if show {
            "已显示左右侧栏".into()
        } else {
            "已隐藏左右侧栏（Ctrl+\\ 恢复）".into()
        };
    }
    if k_save {
        act_save_project(app);
    }
    if k_open {
        act_open_project(app);
    }
    if k_export {
        act_export(app, Export::Nc);
    }
    if k_help {
        app.help_open = true;
    }

    app.rebuild_preview();
    // 页面尺寸可能被改过（PDF 导入 / 滑块 / 载入旧工程），规格名跟着重判一次。
    app.sync_paper();

    // 每帧从后台设备线程同步连接状态与日志（非阻塞，只读共享状态）。
    app.dev_log = app.device.log_snapshot();
    app.dev_connected = app.device.is_connected();
    app.poll_pen_queue(app.device.snapshot().running);
    // 处理设备线程请求切换 tab（例如点「运行」后自动跳到「运行中」页）。
    if let Some(t) = app.device.take_request_tab() {
        app.tab = t;
    }
    // 绑定蓝牙串口后系统会新建 COM 口，设备线程会请求重新扫描一次。
    if app.device.take_request_scan() {
        app.start_port_scan();
    }
    // 后台扫描完成后取回结果（扫描本身不占用 UI 线程）。
    app.poll_port_scan();
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

    let p = pal(ui);

    // ── 顶部：菜单栏 ────────────────────────────
    egui::Panel::top("menubar")
        .frame(
            egui::Frame::new()
                .fill(p.panel)
                .inner_margin(egui::Margin::symmetric(10, 4)),
        )
        .show(ui, |ui| {
            menu_bar(app, ui);
        });

    // ── 底部：实时日志栏（可折叠 / 可拖动调高）────
    // 先注册日志栏，再注册操作栏，操作栏就会叠在日志栏上方。
    let mut log_open = app.log_open;
    let mut log_hide = false;
    egui::Panel::bottom("logbar")
        .resizable(true)
        .default_size(170.0)
        .min_size(60.0)
        .max_size(460.0)
        .show_collapsible(ui, &mut log_open, |ui| {
            log_hide = log_panel(app, ui);
        });
    // 「隐藏」按钮在本帧内部改动，不能直接用帧首拷贝的 log_open 覆盖回去。
    if log_hide {
        log_open = false;
    }
    app.log_open = log_open;

    // ── 底部：全局操作条（连接状态 / 运行控制 / 统计）──
    egui::Panel::bottom("actionbar")
        .frame(
            egui::Frame::new()
                .fill(p.panel)
                .stroke(egui::Stroke::new(1.0, p.stroke))
                .inner_margin(egui::Margin::symmetric(12, 7)),
        )
        .show(ui, |ui| {
            action_bar(app, ui, &p);
        });

    // ── 工作区标题 / 视图工具条（整窗宽，避免被左右侧栏挤成两行）──
    if app.tab != 9 {
        egui::Panel::top("workspacebar")
            .frame(
                egui::Frame::new()
                    .fill(p.panel)
                    .inner_margin(egui::Margin::symmetric(12, 5)),
            )
            .show(ui, |ui| {
                workspace_header(app, ui, &p);
            });
    }

    // ── 左侧：分组导航 + 输出图层 + 坐标（视图菜单可隐藏）──
    if app.show_left {
        egui::Panel::left("modules")
            .resizable(true)
            .default_size(220.0)
            .min_size(190.0)
            .max_size(340.0)
            .frame(
                egui::Frame::new()
                    .fill(p.panel)
                    .inner_margin(egui::Margin::symmetric(8, 6)),
            )
            .show(ui, |ui| {
                left_sidebar(app, ui, &p);
            });
    }

    // ── 右侧：当前模块参数（视图菜单可隐藏）───────
    if app.show_right {
        egui::Panel::right("params")
            .resizable(true)
            .default_size(340.0)
            .min_size(300.0)
            // 上限要给死，否则长按钮/滑块会把栏位一路撑大、挤压预览；
            // 但图片页的多笔笔组和长标签滑块确实需要更宽的栏位，
            // 400 不够用时用户又拖不过去，所以留到 520。
            .max_size(520.0)
            .frame(
                egui::Frame::new()
                    .fill(p.panel)
                    .inner_margin(egui::Margin::symmetric(0, 0)),
            )
            .show(ui, |ui| {
                ScrollArea::vertical()
                    .id_salt("params_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        egui::Frame::new()
                            .inner_margin(egui::Margin::symmetric(12, 8))
                            .show(ui, |ui| {
                                // 主题的滑块轨道宽 150pt，侧栏最窄 300pt：长中文标签 +
                                // 数值 + 轨道会顶出栏外被裁掉。按当前栏宽收窄轨道，
                                // 保证整行（标签、数值、滑块）都完整显示。
                                fit_params_width(ui);
                                match app.tab {
                                    0 => panel_text(app, ui),
                                    1 => panel_table(app, ui),
                                    2 => panel_grid(app, ui),
                                    3 => panel_draw(app, ui),
                                    4 => panel_notes(app, ui),
                                    5 => panel_images(app, ui),
                                    6 | 7 => panel_device(app, ui),
                                    8 => panel_io(app, ui),
                                    9 => panel_running(app, ui),
                                    _ => {}
                                }
                                // 手写风格是全体内容模块的公共参数，放一个入口即可。
                                if app.tab <= 4 || app.tab == 5 {
                                    panel_style(app, ui);
                                }
                            });
                    });
            });
    }

    // 设置 / 帮助 / 关于 / 越界确认 / 绘制前计算
    settings_window(app, ui.ctx().clone());
    help_window(app, ui.ctx().clone());
    about_window(app, ui.ctx().clone());
    bounds_warn_window(app, ui.ctx().clone());
    run_plan_window(app, ui.ctx().clone());
    import_window(app, ui.ctx().clone());
    pen_swap_window(app, ui.ctx().clone());

    // ── 中央：工作区 ─────────────────────────────
    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(p.bg)
                .inner_margin(egui::Margin::symmetric(12, 8)),
        )
        .show(ui, |ui| {
            if app.tab == 9 {
                running_header(app, ui, &p);
                let snap = app.device.snapshot();
                let frac = if snap.total_lines > 0 {
                    snap.done_lines as f64 / snap.total_lines as f64
                } else {
                    0.0
                };
                draw_progress_page(
                    ui,
                    &app.run_preview,
                    frac,
                    app.page_w_mm,
                    app.page_h_mm,
                    app.grid_overlay,
                    app.grid_overlay_cols,
                    app.grid_overlay_rows,
                );
            } else {
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

/// 当前实际参与输出的图层数（内容模块 + 图片）。
fn active_layer_count(app: &AppState) -> usize {
    (0..CONTENT_MODULES.len())
        .filter(|i| app.module_active(*i))
        .count()
        + usize::from(app.images_active() && !app.images.is_empty())
}

/// 左侧导航：分组模块，输出图层常驻最底部。
fn left_sidebar(app: &mut AppState, ui: &mut Ui, p: &theme::Palette) {
    // 停靠在模块列表下方、底部连接状态上方。
    egui::Panel::bottom("output_layers_dock")
        .resizable(false)
        .frame(
            egui::Frame::new()
                .fill(p.card)
                .stroke(egui::Stroke::new(1.0, p.accent.gamma_multiply(0.45)))
                .corner_radius(egui::CornerRadius::same(9))
                .inner_margin(egui::Margin::symmetric(8, 6)),
        )
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            output_layers_body(app, ui, p);
        });
    ui.add_space(2.0);

    ScrollArea::vertical()
        .id_salt("left_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (gi, (group, items)) in NAV_GROUPS.iter().enumerate() {
                if gi > 0 {
                    ui.add_space(6.0);
                }
                ui.label(
                    RichText::new(*group)
                        .size(10.0)
                        .strong()
                        .color(p.muted),
                );
                for (tab, icon, name, hint) in items.iter() {
                    let selected = app.tab == *tab;
                    if theme::nav_item(ui, p, icon, name, hint, selected).clicked() {
                        app.tab = *tab;
                        app.confirm_clear = false;
                        app.status = format!("切换到「{name}」");
                    }
                }
            }
        });
}

/// 输出图层开关组：常驻左侧栏底部，默认仅输出当前页。
fn output_layers_body(app: &mut AppState, ui: &mut Ui, p: &theme::Palette) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("输出图层")
                .size(12.0)
                .strong()
                .color(p.text),
        )
        .on_hover_text("默认只输出当前页，勾选可叠加输出");
        theme::pill(
            ui,
            &format!("{} 层", active_layer_count(app)),
            p.accent,
            p.accent_soft,
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .small_button("当前")
                .on_hover_text("恢复默认：只输出当前所在页面")
                .clicked()
            {
                for (i, m) in app.module_on.iter_mut().enumerate() {
                    *m = app.tab == i;
                }
                app.images_on = app.tab == 5;
            }
            if ui
                .small_button("全部")
                .on_hover_text("勾选所有图层，全部叠加输出")
                .clicked()
            {
                for m in app.module_on.iter_mut() {
                    *m = true;
                }
                app.images_on = true;
            }
        });
    });
    ui.spacing_mut().item_spacing = egui::Vec2::new(4.0, 1.0);
    egui::Grid::new("layer_grid")
        .num_columns(3)
        .spacing([4.0, 1.0])
        .show(ui, |ui| {
            for (i, name) in CONTENT_MODULES.iter().enumerate() {
                // 当前页始终参与输出，因此显示为选中且不可取消；
                // 其它模块的勾选才真正对应 `module_on` 叠加开关。
                let current = app.tab == i;
                let mut on = app.module_active(i);
                if current {
                    ui.add_enabled(false, egui::Checkbox::new(&mut on, *name))
                        .on_disabled_hover_text("当前页默认输出，无需勾选");
                } else {
                    let resp = ui.checkbox(&mut on, *name);
                    if resp.changed() {
                        app.module_on[i] = on;
                    }
                    resp.on_hover_text("叠加到同一页输出");
                }
                if i % 3 == 2 {
                    ui.end_row();
                }
            }
            ui.checkbox(&mut app.images_on, "图片")
                .on_hover_text("把图片模块叠加到同一页");
            ui.end_row();
        });
    if !app.images.is_empty() && !app.images_on {
        ui.label(
            RichText::new("图片未勾选：不会写入 G-code")
                .size(9.0)
                .color(p.warning),
        );
    }
}

/// 全局操作条：左侧运行控制，中间进度，右侧统计。
fn action_bar(app: &mut AppState, ui: &mut Ui, p: &theme::Palette) {
    let snap = app.device.snapshot();
    ui.horizontal(|ui| {
        // 连接状态
        let (col, txt) = if app.dev_connected {
            (
                if app.dev_kind == 2 { p.accent } else { p.success },
                if app.dev_kind == 2 { "模拟" } else { "已连接" },
            )
        } else {
            (p.muted, "未连接")
        };
        let (rect, resp) = ui.allocate_exact_size(egui::Vec2::splat(10.0), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), 4.0, col);
        ui.label(RichText::new(txt).size(11.5).color(col));
        if ui
            .add(theme::soft_button(p, "连接"))
            .on_hover_text("跳到设备页连接机器")
            .clicked()
        {
            app.tab = 6;
        }
        let _ = resp;

        ui.add_space(6.0);

        if !snap.running {
            let tile_layout = app.tile_on.then(|| app.tile_layout()).flatten();
            // 分页时先选页再绘制：页码直接决定「开始绘制」下发哪一页。
            if let Some(layout) = &tile_layout {
                let count = layout.count().max(1);
                let mut page = app.tile_page_clamped();
                egui::ComboBox::from_id_salt("action_tile_page")
                    .selected_text(format!("第 {}/{} 页", page + 1, count))
                    .width(104.0)
                    .show_ui(ui, |ui| {
                        ui.set_max_height(260.0);
                        for i in 0..count {
                            ui.selectable_value(&mut page, i, format!("第 {} 页", i + 1));
                        }
                    });
                if page != app.tile_page {
                    app.tile_page = page;
                    app.status = format!("已选择第 {}/{} 页", page + 1, count);
                }
            }
            let start_label = match &tile_layout {
                Some(_) => format!("绘制第 {} 页", app.tile_page_clamped() + 1),
                None => "开始绘制".to_string(),
            };
            if ui
                .add(theme::primary_button(p, &start_label))
                .on_hover_text(if tile_layout.is_some() {
                    "先计算当前分页的方案（代码量与预计耗时），确认后再下发（含四角定位标记）"
                } else {
                    "先计算代码量、路径优化与预计耗时，确认后再下发（含边界自检）"
                })
                .clicked()
            {
                act_start_draw(app);
            }
        } else if snap.paused {
            if ui
                .add(theme::soft_button(p, "继续绘制"))
                .clicked()
            {
                app.resume_run();
                app.status = "已继续".into();
            }
        } else if ui.add(theme::soft_button(p, "暂停")).clicked() {
            app.pause_run();
            app.status = "已暂停".into();
        }

        if ui
            .add_enabled(
                snap.running,
                theme::danger_button(p, "停止", false),
            )
            .clicked()
        {
            app.stop_run();
            app.status = "已停止".into();
        }

        // 一键四角定位：不依赖分页 / 图片，任何页签都能直接下发。
        if ui
            .add_enabled(
                !snap.running,
                theme::soft_button(p, "四角定位"),
            )
            .on_hover_text(
                "在当前定位框（默认 200×280mm）四角各画一个 1cm L 标记，完成后回到原点",
            )
            .clicked()
        {
            app.draw_corner_marks();
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(4.0);

        if snap.running {
            let frac = if snap.total_lines > 0 {
                snap.done_lines as f64 / snap.total_lines as f64
            } else {
                0.0
            };
            ui.add(
                egui::ProgressBar::new(frac as f32)
                    .desired_width(220.0)
                    .text(format!("{:.0}%", frac * 100.0)),
            );
            ui.label(
                RichText::new(format!("第 {} / {} 行", snap.done_lines, snap.total_lines))
                    .size(11.0)
                    .color(p.muted),
            );
            let remain = if snap.done_lines > 0 {
                (snap.elapsed_ms as f64 / 1000.0) / snap.done_lines as f64
                    * (snap.total_lines.saturating_sub(snap.done_lines)) as f64
            } else {
                app.estimated_seconds()
            };
            ui.label(
                RichText::new(format!(
                    "剩余 {}",
                    fmt_duration(remain)
                ))
                .size(11.0)
                .color(p.text),
            );
        } else {
            ui.label(
                RichText::new(format!(
                    "{} 笔 · 墨线 {:.2} m · 空移 {:.1} m · 预计 {}",
                    app.stats.strokes,
                    app.stats.ink_mm / 1000.0,
                    app.stats.travel_mm / 1000.0,
                    fmt_duration(app.estimated_seconds())
                ))
                .size(11.0)
                .color(p.muted),
            );
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if !app.transfer_status.is_empty() {
                let ok = app.transfer_status.starts_with('✓');
                theme::pill(
                    ui,
                    if ok { "已下发" } else { "下发异常" },
                    if ok { p.success } else { p.danger },
                    p.card,
                );
            }
        });
    });
}

/// 工作区标题栏：模块信息 + 定位辅助 + 视图比例 + 纸张，**压成一行**把高度留给预览。
fn workspace_header(app: &mut AppState, ui: &mut Ui, p: &theme::Palette) {
    let icon = match app.tab {
        0 => "文",
        1 => "表",
        2 => "格",
        3 => "绘",
        4 => "记",
        5 => "图",
        6 => "机",
        7 => "测",
        8 => "出",
        _ => "页",
    };
    let name = MODULES.get(app.tab).copied().unwrap_or("预览");
    let desc = MODULE_DESC.get(app.tab).copied().unwrap_or("");

    ui.horizontal(|ui| {
        let (tile, _) = ui.allocate_exact_size(egui::Vec2::splat(24.0), egui::Sense::hover());
        ui.painter()
            .rect_filled(tile, egui::CornerRadius::same(6), p.accent);
        ui.painter().text(
            tile.center(),
            egui::Align2::CENTER_CENTER,
            icon,
            egui::FontId::proportional(13.0),
            egui::Color32::WHITE,
        );
        ui.label(RichText::new(name).size(15.0).strong().color(p.text))
            .on_hover_text(desc);
        theme::pill(ui, &format!("{} 笔", app.stats.strokes), p.muted, p.card);
        if app.images_on {
            theme::pill(ui, "图片叠加中", p.success, p.card);
        }

        ui.separator();
        ui.checkbox(&mut app.ruler_on, "标尺");
        ui.checkbox(&mut app.grid_overlay, "网格")
            .on_hover_text("显示按列/行等分的虚线参考网格（仅定位，不打印）");
        if app.grid_overlay {
            ui.label(RichText::new("列").size(10.0).color(p.muted));
            ui.add(
                egui::DragValue::new(&mut app.grid_overlay_cols)
                    .range(1..=60)
                    .clamp_existing_to_range(true)
                    .speed(1),
            );
            ui.label(RichText::new("行").size(10.0).color(p.muted));
            ui.add(
                egui::DragValue::new(&mut app.grid_overlay_rows)
                    .range(1..=80)
                    .clamp_existing_to_range(true)
                    .speed(1),
            );
        }
        if app.tab <= 4 || app.tab == 5 {
            ui.separator();
            let label = if app.confirm_clear {
                "确认清空？"
            } else {
                "清空本页"
            };
            let btn = if app.confirm_clear {
                theme::danger_button(p, label, true)
            } else {
                theme::soft_button(p, label)
            };
            if ui.add(btn).clicked() {
                if app.confirm_clear {
                    app.clear_current_module();
                    app.confirm_clear = false;
                } else {
                    app.confirm_clear = true;
                    app.status = "再点一次确认清空本页".into();
                }
            }
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // 纸张 / 方向
            paper_row(app, ui);
            ui.separator();
            // 笔尖粗细：只影响预览显示，让屏幕上的线宽和纸上一致
            ui.add(
                egui::DragValue::new(&mut app.preview_pen_mm)
                    .range(0.1..=1.6)
                    .speed(0.05)
                    .fixed_decimals(2)
                    .prefix("笔 ")
                    .suffix(" mm"),
            )
            .on_hover_text(
                "笔尖实际粗细，只影响预览显示，不改 G-code。\n0.3 ≈ 针管笔 / 0.5 ≈ 中性笔 / 0.7~1.0 ≈ 记号笔。",
            );
            ui.separator();
            // 视图比例：适应 / + / % / −
            if ui
                .add(theme::soft_button(p, "适应"))
                .on_hover_text("恢复整页适应窗口（也支持 Ctrl+滚轮缩放 / 空格 + 拖动平移）")
                .clicked()
            {
                app.reset_view();
            }
            if ui.small_button("+").on_hover_text("放大画布").clicked() {
                app.zoom_by(1.25);
            }
            ui.label(
                RichText::new(format!("{:.0}%", app.canvas_zoom * 100.0))
                    .size(11.0)
                    .monospace()
                    .color(p.text),
            );
            if ui.small_button("−").on_hover_text("缩小画布").clicked() {
                app.zoom_by(1.0 / 1.25);
            }
            if let Some((x, y)) = app.cursor_mm {
                ui.label(
                    RichText::new(format!("{:.1}, {:.1} mm", x, y))
                        .size(10.0)
                        .monospace()
                        .color(p.muted),
                )
                .on_hover_text("鼠标在页面上的位置");
            }
        });
    });
    ui.add_space(2.0);
    theme::divider(ui, p);
}

/// 运行监控页标题栏。
fn running_header(app: &mut AppState, ui: &mut Ui, p: &theme::Palette) {
    let snap = app.device.snapshot();
    ui.horizontal(|ui| {
        ui.label(RichText::new("运行监控").size(17.0).strong().color(p.text));
        let state = if snap.running {
            if snap.paused {
                "已暂停"
            } else {
                "运行中"
            }
        } else {
            "空闲"
        };
        theme::pill(
            ui,
            state,
            if snap.running { p.success } else { p.muted },
            p.card,
        );
        ui.label(
            RichText::new(format!(
                "{} / {} 行 · 已用 {}",
                snap.done_lines,
                snap.total_lines,
                fmt_duration(snap.elapsed_ms as f64 / 1000.0)
            ))
            .size(11.0)
            .color(p.muted),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add(theme::soft_button(p, "返回编辑"))
                .clicked()
            {
                app.tab = 0;
            }
        });
    });
    ui.add_space(4.0);
    theme::divider(ui, p);
}

/// 实时坐标：机器物理坐标（MPos）/ 当前工件坐标（WPos）/ 页面坐标 → 机器坐标。
///
/// 放在字库下面、实时日志上面：字库是「静态信息」，日志在最底部，
/// 坐标是运行中唯一想一直盯着的东西，插在两者之间最顺眼。
fn coord_panel(app: &mut AppState, ui: &mut Ui) {
    let snap = app.device.snapshot();
    let p = snap.pos;
    // 等宽坐标串不能太长：三行并排会撑宽参数栏，把中间画布挤小。
    let line = |v: Option<[f64; 3]>| match v {
        Some(a) => format!("X{:>7.2}  Y{:>7.2}  Z{:>6.2}", a[0], a[1], a[2]),
        None => "X  —      Y  —      Z  —".to_string(),
    };
    egui::Grid::new("coord_grid")
        .num_columns(2)
        .spacing([6.0, 2.0])
        .show(ui, |ui| {
            ui.label(RichText::new("机器物理").size(10.0).color(muted()));
            ui.label(
                RichText::new(line(p.m))
                    .size(11.0)
                    .monospace()
                    .color(egui::Color32::from_rgb(40, 110, 240)),
            );
            ui.end_row();
            ui.label(RichText::new("当前坐标").size(10.0).color(muted()));
            ui.label(
                RichText::new(line(p.w))
                    .size(11.0)
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
        let mut poll = app.status_poll_ms > 0;
        if ui
            .checkbox(&mut poll, "自动刷新坐标")
            .on_hover_text("每秒自动查询一次坐标（GRBL 不主动推送，只能定时问）")
            .changed()
        {
            app.status_poll_ms = if poll { 1000 } else { 0 };
        }
    });

    if !app.dev_connected {
        ui.label(RichText::new("未连接：连上机器后才有坐标").size(10.0).color(muted()));
    } else if !p.any() {
        ui.label(RichText::new("暂无坐标回报，点下方「读取当前位置」查询一次").size(10.0).color(muted()));
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
                let mut dark = app.dark_mode;
                if ui.checkbox(&mut dark, "深色主题").changed() {
                    app.dark_mode = dark;
                    app.theme_applied = None;
                }
                ui.add(egui::Slider::new(&mut app.ui_scale, 0.7..=2.0).text("界面缩放(×)"));
                ui.weak("窗口里放不下内容 / 字太小时调它；改完立即生效。");
                if ui.button("恢复 1.0×").clicked() {
                    app.ui_scale = 1.0;
                }
                ui.checkbox(&mut app.axes_on, "画布显示 XY 方向箭头（蓝 X / 绿 Y）");
                ui.checkbox(&mut app.text_frame_on, "显示文本框虚框（不打印）");
                if ui.button("打开快速上手 / 快捷键").clicked() {
                    app.help_open = true;
                }

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
                ui.weak("机器完全不动时：保持勾选解锁，并到「设备与调试 → 机器调试」手动发一次 $X 看回显。");

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

                section(ui, "耗时估算（开始绘制前给出的预计时间）");
                ui.add(
                    egui::Slider::new(&mut app.rapid_rate, 500.0..=30000.0)
                        .text("空移快速(mm/min)"),
                );
                ui.add(
                    egui::Slider::new(&mut app.accel_mm_s2, 50.0..=5000.0)
                        .text("加速度(mm/s²)"),
                );
                ui.add(egui::Slider::new(&mut app.line_ms, 0.0..=10.0).text("每行指令(ms)"));
                ui.weak(
                    "空移速度与加速度取机器实际值；每行指令开销约等于「一行代码的固件处理时间」。\
                     估时按加减速曲线逐段累计，短笔触多、指令密时会更准。",
                );

                section(ui, "机器幅面");
                ui.horizontal(|ui| {
                    ui.add(egui::Slider::new(&mut app.machine_w_mm, 50.0..=600.0).text("宽(mm)"));
                    ui.add(egui::Slider::new(&mut app.machine_h_mm, 50.0..=800.0).text("高(mm)"));
                });
                ui.weak("仅作越界提示，不影响绘制坐标。");

                section(ui, "启动与发送");
                ui.checkbox(&mut app.auto_connect, "启动后自动连接机器");
                ui.checkbox(&mut app.auto_set_zero, "运行前自动设原点（G92 X0 Y0）");
                ui.checkbox(&mut app.single_step, "单步下发（逐行发送并等待回显）");
                ui.checkbox(&mut app.bounds_check, "开始前做机器幅面边界检查");
                ui.weak("固件、抬落笔、速度、坐标系等生成参数在「项目与输出」页统一设置。");
            });
        });
    app.settings_open = open;
}

/// 内容模块共用的「手写风格」：随机抖动参数与生效范围。
fn panel_style(app: &mut AppState, ui: &mut Ui) {
    let p = pal(ui);
    ui.add_space(8.0);
    theme::collapsible_card(ui, &p, "手写风格 · 随机变形", false, |ui| {
        ui.checkbox(&mut app.use_random, "启用随机变形（更接近真人手写）");
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

        if app.tab == 0 && !app.texts.is_empty() {
            ui.add_enabled_ui(app.use_random, |ui| {
                theme::divider(ui, &p);
                ui.label(RichText::new("生效范围（只对勾上的文本框）").size(10.5).color(p.muted));
                let n = app.texts.len();
                for i in 0..n {
                    let nm = app.texts[i].name.clone();
                    ui.checkbox(&mut app.texts[i].random, nm);
                }
                ui.horizontal(|ui| {
                    if ui.small_button("全选").clicked() {
                        for b in app.texts.iter_mut() {
                            b.random = true;
                        }
                    }
                    if ui.small_button("全不选").clicked() {
                        for b in app.texts.iter_mut() {
                            b.random = false;
                        }
                    }
                    if ui.small_button("只选当前").clicked() {
                        let cur = app.sel_text.min(n.saturating_sub(1));
                        for (i, b) in app.texts.iter_mut().enumerate() {
                            b.random = i == cur;
                        }
                    }
                });
            });
        }

        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(app.use_random, theme::soft_button(&p, "换一种笔迹"))
                .clicked()
            {
                app.rebuild_preview_random();
                app.status = "已重新随机".into();
            }
            if ui
                .add_enabled(app.use_random, theme::soft_button(&p, "复位参数"))
                .clicked()
            {
                app.rand_x = 0.0;
                app.rand_y = 0.0;
                app.rand_scale = 0.0;
                app.rand_head = 0.0;
                app.status = "随机参数已复位".into();
            }
        });
    });
}

/// 关于窗口：产品介绍、版本与运行环境。
fn about_window(app: &mut AppState, ctx: egui::Context) {
    if !app.about_open {
        return;
    }
    let p = theme::palette(app.dark_mode);
    let mut open = app.about_open;
    let mut close = false;

    egui::Window::new("关于 WritingRobot")
        .open(&mut open)
        .resizable(false)
        .collapsible(false)
        .default_width(480.0)
        .show(&ctx, |ui| {
            ui.set_max_width(480.0);
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(52.0), egui::Sense::hover());
                let painter = ui.painter();
                painter.rect_filled(rect, egui::CornerRadius::same(14), p.accent);
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "写",
                    egui::FontId::proportional(26.0),
                    egui::Color32::WHITE,
                );
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("WritingRobot").size(22.0).strong().color(p.text));
                        theme::pill(
                            ui,
                            &format!("v{}", env!("CARGO_PKG_VERSION")),
                            p.accent,
                            p.accent_soft,
                        );
                    });
                    ui.label(
                        RichText::new("写字机器人上位机 · 从排版到落笔的一站式控制台")
                            .size(11.0)
                            .color(p.muted),
                    );
                });
            });

            ui.add_space(8.0);
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "WritingRobot 把文字排版、表格、练字格、手绘、笔记和图片线稿集中到一个所见即所得的页面里，\
                         自动生成 G-code，并通过 USB 串口、蓝牙或 TCP 直接驱动写字机落笔。\
                         无需安装驱动，断线可自动重连，也提供模拟模式用于完整试跑流程。",
                    )
                    .size(12.5)
                    .color(p.text),
                )
                .wrap(),
            );

            ui.add_space(4.0);
            theme::section(ui, &p, "主要能力", None);
            egui::Grid::new("about_features")
                .num_columns(2)
                .spacing([16.0, 5.0])
                .show(ui, |ui| {
                    for (k, v) in [
                        ("创作", "文字 / 表格 / 练字格 / 手绘 / 笔记，多图层叠加输出"),
                        ("图片", "8 种线稿模式（含素描原稿），前景可打印、背景仅作临摹参考"),
                        ("设备", "串口 / 蓝牙 / TCP / 模拟，后台扫描与蓝牙链路体检"),
                        ("输出", "GRBL / Marlin / EBB，支持 G-code / SVG / PDF / PNG"),
                    ] {
                        ui.label(RichText::new(k).size(11.5).strong().color(p.accent));
                        ui.add(egui::Label::new(RichText::new(v).size(11.5)).wrap());
                        ui.end_row();
                    }
                });

            ui.add_space(6.0);
            theme::divider(ui, &p);
            egui::Grid::new("about_build")
                .num_columns(2)
                .spacing([20.0, 3.0])
                .show(ui, |ui| {
                    for (k, v) in [
                        (
                            "版本",
                            format!(
                                "v{} · {}版",
                                env!("CARGO_PKG_VERSION"),
                                if cfg!(debug_assertions) { "开发" } else { "正式" }
                            ),
                        ),
                        (
                            "运行平台",
                            format!(
                                "{} {}",
                                std::env::consts::OS,
                                std::env::consts::ARCH
                            ),
                        ),
                        ("界面框架", "egui / eframe · OpenGL (Glow)".to_string()),
                    ] {
                        ui.label(RichText::new(k).size(10.5).color(p.muted));
                        ui.label(RichText::new(v).size(10.5).color(p.text));
                        ui.end_row();
                    }
                });

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.add(theme::soft_button(&p, "快速上手")).clicked() {
                    app.help_open = true;
                    close = true;
                }
                if ui.add(theme::primary_button(&p, "关闭")).clicked() {
                    close = true;
                }
            });
        });

    app.about_open = open && !close;
}

/// 快速上手窗口：把分散在各页的操作顺序讲清楚。
fn help_window(app: &mut AppState, ctx: egui::Context) {
    if !app.help_open {
        return;
    }
    let p = theme::palette(app.dark_mode);
    let mut open = app.help_open;
    egui::Window::new("快速上手")
        .open(&mut open)
        .resizable(true)
        .default_size([520.0, 560.0])
        .show(&ctx, |ui| {
            ScrollArea::vertical().show(ui, |ui| {
                theme::section(ui, &p, "1. 选模块，勾图层", Some("左侧导航按创作 / 图形 / 设备 / 项目分组"));
                ui.label("· 「创作」「图形」里选一个模块编辑内容。");
                ui.label("· 左侧栏「输出图层」决定哪些模块叠加到同一页并写入 G-code。");
                ui.label("· 默认只输出当前页；点「全部」让文字、表格、图片等一起叠加，点「当前」恢复默认。");

                theme::section(ui, &p, "2. 连接机器", Some("有线 USB 或蓝牙串口都可以"));
                ui.label("· 打开「设备与调试」→ 扫描串口 → 选在线端口 → 连接。");
                ui.label("· 连上但机器不动：先「自动波特率」，再解锁 $X。");
                ui.label("· 没有机器：连接方式选「模拟」，可以完整试跑流程。");

                theme::section(ui, &p, "3. 调整与预览", Some("所见即所得"));
                ui.label("· 右侧参数栏只显示当前模块的参数；纸张、缩放、标尺在画布上方。");
                ui.label("· Ctrl + 滚轮缩放画布，中键或空格 + 左键拖动平移。");
                ui.label("· 右栏底部「手写风格」可开启随机抖动。");

                theme::section(ui, &p, "4. 输出与运行", None);
                ui.label("· 底部操作条「开始绘制」会先做幅面边界检查，再下发 G-code。");
                ui.label("· 「项目与输出」页集中了固件、速度、抬落笔、坐标系、导入与导出。");
                ui.label("· 运行进度、耗时和剩余时间显示在底部操作条与「运行监控」页。");

                theme::section(ui, &p, "快捷键", None);
                egui::Grid::new("help_keys")
                    .num_columns(2)
                    .spacing([24.0, 4.0])
                    .show(ui, |ui| {
                        for (k, v) in [
                            ("Ctrl + S", "保存工程"),
                            ("Ctrl + O", "打开工程"),
                            ("Ctrl + E", "导出 G-code"),
                            ("Ctrl + 滚轮", "缩放画布"),
                            ("F1", "打开本窗口"),
                        ] {
                            ui.label(RichText::new(k).monospace().color(p.accent));
                            ui.label(v);
                            ui.end_row();
                        }
                    });

                theme::section(ui, &p, "常见问题", None);
                ui.label("· 中文变方块：未加载 .gfont 字库，到「项目与输出 → 导入」重新扫描。");
                ui.label("· 超界报警：在「项目与输出 → 坐标系与幅面」核对机器行程，或调整纸张/缩放。");
                ui.label("· 蓝牙几分钟断一次：设置里开启保活心跳与断线自动重连。");

                ui.add_space(10.0);
                ui.separator();
                ui.label(
                    RichText::new(format!(
                        "WritingRobot v{} · {} 构建 · {}",
                        env!("CARGO_PKG_VERSION"),
                        if cfg!(debug_assertions) { "调试" } else { "发布" },
                        if cfg!(target_os = "windows") { "Windows x64" } else { std::env::consts::OS },
                    ))
                    .size(10.0)
                    .color(p.muted),
                );
            });
        });
    app.help_open = open;
}

fn panel_text(app: &mut AppState, ui: &mut Ui) {
    section(ui, "文字内容");
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
    ui.weak("虚框只是定位参考，不会写进 G-code。手写抖动在右栏底部「手写风格」里调。");
}

fn panel_table(app: &mut AppState, ui: &mut Ui) {
    section(ui, "表格");
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
    section(ui, "练字格");
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
    let p = pal(ui);
    section(ui, "鼠标手绘");
    theme::card(ui, &p, |ui| {
        ui.checkbox(&mut app.freehand_on, "启用手绘（在纸张范围内按住拖动）");
        ui.label(
            RichText::new("拖动松手即生成一条笔迹；画布支持 Ctrl+滚轮缩放，方便写小字。")
                .size(10.0)
                .color(p.muted),
        );
        ui.horizontal(|ui| {
            theme::pill(
                ui,
                &format!("{} 条笔迹", app.draw_paths.len()),
                if app.draw_paths.is_empty() { p.muted } else { p.accent },
                p.card_hover,
            );
            if ui
                .add_enabled(!app.draw_paths.is_empty(), theme::soft_button(&p, "撤销最后一条"))
                .clicked()
            {
                app.draw_paths.pop();
                app.status = "已撤销最后一条手绘".into();
            }
            if ui
                .add_enabled(!app.draw_paths.is_empty(), theme::soft_button(&p, "清空手绘"))
                .clicked()
            {
                app.draw_paths.clear();
                app.freehand.clear();
                app.status = "已清空手绘".into();
            }
        });
    });

    ui.add_space(6.0);
    section(ui, "导入矢量文件");
    theme::card(ui, &p, |ui| {
        ui.label(
            RichText::new("支持 DXF / SVG：等比缩放到目标高度，锚定页面左上角。")
                .size(10.0)
                .color(p.muted),
        );
        ui.add(egui::Slider::new(&mut app.draw_target_h, 20.0..=280.0).text("目标高度(mm)"));
        if ui.add(theme::soft_button(&p, "选择 DXF / SVG 文件…")).clicked() {
            if let Some(path) = app.pick_file("矢量文件", &["dxf", "svg"]) {
                match app.import_vector_file(&path, app.draw_target_h) {
                    Ok(n) => app.status = format!("已导入矢量：{n} 条笔迹"),
                    Err(e) => app.status = format!("导入失败：{e}"),
                }
            }
        }
        if !app.draw_path.is_empty() {
            ui.label(
                RichText::new(format!("最近导入：{}", app.draw_path))
                    .size(9.5)
                    .color(p.muted),
            );
        }
    });

    ui.add_space(6.0);
    theme::notice(
        ui,
        &p,
        "图片描边（照片转影线）属于「图片」模块：那里可以调描边类型、旋转、裁剪，并单独控制每张图是否输出。",
        p.accent,
    );
}

fn panel_notes(app: &mut AppState, ui: &mut Ui) {
    section(ui, "笔记");
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
            // 两个滑块横排会超出窄参数栏，改成竖排避免把面板撑宽。
            ui.add(egui::Slider::new(&mut b.x, 0.0..=200.0).text("X"));
            ui.add(egui::Slider::new(&mut b.y, 0.0..=260.0).text("Y"));
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
    section(ui, "设备连接");

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
                format!("● 已连接 {}{}（可在下方「机器调试」发指令验证）", snap.cur_addr, baud),
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
        ui.add(egui::Label::new(RichText::new(txt).color(col).strong()).wrap());
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
        let combo_w = (ui.available_width() - 4.0).clamp(160.0, 280.0);
        egui::ComboBox::from_id_salt("ser_port")
            .width(combo_w)
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
            .width(combo_w)
            .selected_text(format!("{}", app.serial_baud))
            .show_ui(ui, |ui| {
                for b in bauds {
                    ui.selectable_value(&mut app.serial_baud, *b, format!("{}", b));
                }
            });

        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(!app.port_scan_running(), egui::Button::new("扫描串口"))
                .on_hover_text("后台扫描，不会卡住界面；离线蓝牙口可能需要等几秒")
                .clicked()
            {
                app.start_port_scan();
            }
            if app.port_scan_running() {
                ui.spinner();
                ui.label(RichText::new("扫描中…").size(11.0).color(muted()));
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
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("{} {}  {}", mark, label, hint)).color(col),
                    )
                    .wrap(),
                );
                let _ = p;
            }
            if !app.port_labels.iter().any(|(_, l, _)| l.starts_with("串口线"))
                && !app.port_labels.is_empty()
            {
                ui.add(
                    egui::Label::new(
                        RichText::new("⚠ 没有检测到串口线（USB-TTL）：插上适配器后点「扫描」，带 USB/SERIAL 字样的就是它")
                            .color(egui::Color32::from_rgb(210, 150, 40)),
                    )
                    .wrap(),
                );
            }
        }


    // ── 蓝牙专区 ───────────────────────────────────────────────
    // 「设置里看得到设备」≠「有能用的串口」。这里直接问蓝牙栈：
    // 写字机在不在、装没装 SPP、COM 口绑没绑对，并且能一键绑定。
    egui::CollapsingHeader::new("蓝牙链路（连不上先看这里）")
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
                    "展开下方「机器调试」点「解锁 $X」：有 ← 回显说明通了，没回显说明线没通",
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

    // 机器调试：原先独立的「测试」页整体并入这里，避免两处重复一套点动/回零按钮。
    ui.separator();
    let p = pal(ui);
    theme::collapsible_card(ui, &p, "机器调试（坐标 / 点动 / 回零）", true, |ui| {
        machine_debug(app, ui);
    });

    ui.separator();
    ui.horizontal(|ui| {
        if ui.add(theme::soft_button(&p, "清空日志")).clicked() {
            app.device.clear_log();
            app.dev_log.clear();
            app.status = "已清空设备日志".into();
        }
        let btn = if app.log_open { "隐藏底部日志栏" } else { "显示底部日志栏" };
        if ui.add(theme::soft_button(&p, btn)).clicked() {
            app.log_open = !app.log_open;
        }
    });
    ui.weak("完整日志在窗口最下方的「实时日志」栏；运行控制固定在底部操作条，任何页面都能用。");
}

/// 机器调试区：实时坐标 / 解锁 / 回零 / 点动 / 抬落笔 / 测试方框 / 单行 G-code。
fn machine_debug(app: &mut AppState, ui: &mut Ui) {
    let p = pal(ui);
    ui.label(RichText::new("实时坐标").size(12.0).strong().color(p.text));
    coord_panel(app, ui);
    theme::divider(ui, &p);
    ui.horizontal_wrapped(|ui| {
        if ui.add(theme::soft_button(&p, "解锁 $X")).clicked() {
            app.unlock_machine();
        }
        if ui.add(theme::soft_button(&p, "软复位")).clicked() {
            app.soft_reset();
        }
        if ui.add(theme::soft_button(&p, "回机械原点 $H")).clicked() {
            app.home_machine();
        }
        if ui.add(theme::soft_button(&p, "移到 (0,0)")).clicked() {
            app.go_zero();
        }
    });
    ui.weak("GRBL 上电 / 急停后处于 Alarm 锁定态，先「解锁 $X」，日志看到 ok 才能移动。");

    theme::divider(ui, &p);
    ui.label(RichText::new("零点").size(12.0).strong().color(p.text));
    ui.horizontal_wrapped(|ui| {
        if ui.add(theme::soft_button(&p, "当前点设为零点")).clicked() {
            app.set_zero_here();
        }
        if ui.add(theme::soft_button(&p, "清除坐标偏置")).clicked() {
            app.clear_zero_offset();
        }
        if ui.add(theme::soft_button(&p, "读取当前位置")).clicked() {
            app.read_position();
        }
    });
    ui.weak("没有限位开关的写字机用「移到 (0,0)」或「当前点设为零点」，不要点 $H。");

    theme::divider(ui, &p);
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("点动").size(12.0).strong().color(p.text));
        ui.add(egui::Slider::new(&mut app.jog_step, 1.0..=50.0).text("步长(mm)"));
    });
    let s = app.jog_step;
    ui.horizontal(|ui| {
        if ui.button("↖").clicked() {
            send_jog(app, -s, -s);
        }
        if ui.button("Y−").clicked() {
            send_jog(app, 0.0, -s);
        }
        if ui.button("↗").clicked() {
            send_jog(app, s, -s);
        }
    });
    ui.horizontal(|ui| {
        if ui.button("X−").clicked() {
            send_jog(app, -s, 0.0);
        }
        if ui.button("⌂").clicked() {
            app.go_zero();
        }
        if ui.button("X+").clicked() {
            send_jog(app, s, 0.0);
        }
    });
    ui.horizontal(|ui| {
        if ui.button("↙").clicked() {
            send_jog(app, -s, s);
        }
        if ui.button("Y+").clicked() {
            send_jog(app, 0.0, s);
        }
        if ui.button("↘").clicked() {
            send_jog(app, s, s);
        }
    });

    theme::divider(ui, &p);
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("笔 / Z").size(12.0).strong().color(p.text));
        if ui.add(theme::soft_button(&p, "抬笔")).clicked() {
            app.pen_up_now();
        }
        if ui.add(theme::soft_button(&p, "落笔")).clicked() {
            app.pen_down_now();
        }
    });

    theme::divider(ui, &p);
    ui.label(RichText::new("测试方框").size(12.0).strong().color(p.text));
    // 两个滑块并排会超出参数栏宽度，竖排各占一行。
    ui.add(egui::Slider::new(&mut app.test_frame_w, 10.0..=300.0).text("宽(mm)"));
    ui.add(egui::Slider::new(&mut app.test_frame_h, 10.0..=300.0).text("高(mm)"));
    if ui.add(theme::soft_button(&p, "开始画方框")).clicked() {
        let (w, h) = (app.test_frame_w, app.test_frame_h);
        app.draw_test_frame(w, h);
    }
    ui.weak("画出来是矩形说明方向正常；镜像 / 旋转请在「项目与输出 → 坐标系」里调。");

    theme::divider(ui, &p);
    ui.label(RichText::new("四角定位标记").size(12.0).strong().color(p.text));
    // 两个滑块并排会超出参数栏宽度，竖排各占一行。
    ui.add(egui::Slider::new(&mut app.corner_w_mm, 20.0..=600.0).text("定位框宽(mm)"));
    ui.add(egui::Slider::new(&mut app.corner_h_mm, 20.0..=800.0).text("定位框高(mm)"));
    let (ew, eh) = app.corner_frame_size();
    ui.weak(format!(
        "实际绘制 {:.0}×{:.0}mm（超出纸张/机器幅面会自动收窄）。四角各一个 1cm L（尖端朝角点、开口朝内），完成后回到原点。",
        ew, eh
    ));
    let running = app.device.snapshot().running;
    if ui
        .add_enabled(
            !running,
            theme::soft_button(&p, "一键绘制四角 L 形"),
        )
        .on_disabled_hover_text("运行中不能重新下发定位标记")
        .clicked()
    {
        app.draw_corner_marks();
    }

    theme::divider(ui, &p);
    ui.label(RichText::new("发送单行 G-code").size(12.0).strong().color(p.text));
    ui.horizontal(|ui| {
        ui.add(TextEdit::singleline(&mut app.test_custom_gcode).desired_width(180.0));
        if ui.add(theme::soft_button(&p, "发送")).clicked() {
            let g = app.test_custom_gcode.clone();
            if !g.trim().is_empty() {
                app.send_gcode(&g);
                app.status = format!("已发送：{}", g);
            }
        }
    });
}

fn send_jog(app: &mut AppState, dx: f64, dy: f64) {
    let g = app.jog_gcode(dx, dy);
    app.send_gcode(&g);
    app.status = format!("点动 ({}, {})", dx, dy);
}

fn panel_io(app: &mut AppState, ui: &mut Ui) {
    let p = pal(ui);
    section(ui, "页面与边框");

    theme::card(ui, &p, |ui| {
        paper_row(app, ui);
        ui.weak(format!(
            "当前页面 {:.0} × {:.0} mm · 内容 {} 笔",
            app.page_w_mm, app.page_h_mm, app.stats.strokes
        ));
        egui::CollapsingHeader::new("页面边框")
            .id_salt("border_card")
            .show(ui, |ui| {
                panel_border(app, ui);
            });
    });

    ui.add_space(6.0);
    section(ui, "工程");
    theme::card(ui, &p, |ui| {
        ui.label(
            RichText::new("完整工程保存页面内容、图片、文本块与全部参数；字库和设备连接不写入文件。")
                .size(10.0)
                .color(p.muted),
        );
        ui.horizontal_wrapped(|ui| {
            if ui.add(theme::soft_button(&p, "新建")).clicked() {
                app.new_project();
                app.project_path = None;
                app.status = "已新建空白工程".into();
            }
            if ui.add(theme::soft_button(&p, "打开工程…")).clicked() {
                act_open_project(app);
            }
            if ui.add(theme::primary_button(&p, "保存工程")).clicked() {
                act_save_project(app);
            }
            if ui.add(theme::soft_button(&p, "另存为…")).clicked() {
                act_save_project_as(app);
            }
        });
        if let Some(path) = &app.project_path {
            ui.add(
                egui::Label::new(
                    RichText::new(format!("当前工程：{}", path.display()))
                        .size(9.5)
                        .color(p.muted),
                )
                .wrap(),
            );
        }
    });

    ui.add_space(6.0);
    section(ui, "导入");
    theme::card(ui, &p, |ui| {
        // PDF
        ui.horizontal_wrapped(|ui| {
            if ui.add(theme::soft_button(&p, "选择 PDF…")).clicked() {
                if let Some(path) = app.pick_file("PDF", &["pdf"]) {
                    match app.load_pdf_info(&path) {
                        Ok(n) => {
                            app.status = format!("已读取 {} 页，选好页码后导入", n);
                        }
                        Err(e) => app.status = format!("读取 PDF 失败：{e}"),
                    }
                }
            }
            if app.pdf_pages > 0 {
                theme::pill(
                    ui,
                    &format!("{} 页", app.pdf_pages),
                    p.accent,
                    p.card_hover,
                );
            }
        });
        if app.pdf_pages > 0 {
            ui.add(
                egui::Label::new(RichText::new(&app.pdf_path).size(9.5).color(p.muted)).wrap(),
            );
            ui.horizontal(|ui| {
                ui.label("页码");
                ui.add(egui::DragValue::new(&mut app.pdf_page).range(1..=app.pdf_pages.max(1)));
                ui.label(format!("/ {}", app.pdf_pages));
                ui.checkbox(&mut app.pdf_1to1, "1:1 原始尺寸");
            });
            ui.add_enabled(
                !app.pdf_1to1,
                egui::Slider::new(&mut app.img_target_h, 30.0..=250.0).text("目标高度(mm)"),
            );
            if ui
                .add(theme::soft_button(&p, &format!("导入第 {} 页", app.pdf_page)))
                .on_hover_text("矢量描边；文字按原位置重排；嵌入图片走描边提取")
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
                            msg.push_str("，但这一页没有可绘制内容");
                        } else {
                            msg.push_str("。到「图片」页调整，并在左侧勾选「图片」输出。");
                        }
                        app.status = msg;
                    }
                    Err(e) => app.status = format!("PDF 导入失败：{e}"),
                }
            }
        }

        theme::divider(ui, &p);

        // 字库
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("单线字库").size(12.0).strong().color(p.text));
            theme::pill(
                ui,
                &format!("{} 个", app.fonts.fonts.len()),
                if app.fonts.is_empty() { p.danger } else { p.success },
                p.card_hover,
            );
        });
        ui.add(egui::Label::new(RichText::new(&app.font_dir).size(9.5).color(p.muted)).wrap());
        ui.horizontal_wrapped(|ui| {
            if ui.add(theme::soft_button(&p, "导入 .gfont…")).clicked() {
                act_import_font(app);
            }
            if ui.add(theme::soft_button(&p, "重新扫描")).clicked() {
                let reloaded =
                    crate::fontmgr::FontManager::load(std::path::Path::new(&app.font_dir));
                let n = reloaded.fonts.len();
                app.fonts = reloaded;
                app.status = format!("已重新扫描：{} 个字库", n);
            }
        });
    });

    ui.add_space(6.0);
    section(ui, "生成设置");
    theme::card(ui, &p, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new("固件").size(11.0).color(p.muted));
            ui.selectable_value(&mut app.firmware_sel, 0, "GRBL");
            ui.selectable_value(&mut app.firmware_sel, 1, "Marlin");
            ui.selectable_value(&mut app.firmware_sel, 2, "EBB");
        });
        if app.firmware_sel != 2 {
            ui.horizontal(|ui| {
                ui.label(RichText::new("笔控").size(11.0).color(p.muted));
                ui.selectable_value(&mut app.pen_mode_sel, 0, "Z 轴");
                ui.selectable_value(&mut app.pen_mode_sel, 1, "舵机");
            });
        }
        ui.add(egui::Slider::new(&mut app.feed_rate, 500.0..=30000.0).text("进给(mm/min)"));
        ui.add(egui::Slider::new(&mut app.z_speed, 100.0..=30000.0).text("Z 轴速度(mm/min)"));
        if app.firmware_sel != 1 {
            ui.add(egui::Slider::new(&mut app.pen_up, -20.0..=40.0).text("抬笔 Z"));
            ui.add(egui::Slider::new(&mut app.pen_down, -20.0..=40.0).text("落笔 Z"));
        }
        ui.checkbox(&mut app.bounds_check, "开始前检查机器幅面（超界先确认）");
        ui.checkbox(&mut app.auto_sleep, "完成后自动休眠");
        ui.add(egui::Slider::new(&mut app.tool_on_delay, 0.0..=2.0).text("落笔后暂停(s)"));
        ui.add(egui::Slider::new(&mut app.tool_off_delay, 0.0..=2.0).text("抬笔前暂停(s)"));

        egui::CollapsingHeader::new("坐标系与幅面")
            .id_salt("gen_coords")
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("原点角");
                    for (i, nm) in ORIGINS.iter().enumerate() {
                        ui.selectable_value(&mut app.origin_corner, i, *nm);
                    }
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut app.reverse_x, "反转 X");
                    ui.checkbox(&mut app.reverse_y, "反转 Y");
                    ui.checkbox(&mut app.swap_xy, "对调 XY");
                });
                // 真实原点读数：两个设置都能镜像，叠加后才是结果，必须算出来给人看
                let (ox, oy) = app.origin_page_point();
                let at = match (ox > app.page_w_mm * 0.5, oy > app.page_h_mm * 0.5) {
                    (false, false) => "左上",
                    (true, false) => "右上",
                    (false, true) => "左下",
                    (true, true) => "右下",
                };
                let ok = at == "左下";
                ui.colored_label(
                    if ok {
                        p.muted
                    } else {
                        egui::Color32::from_rgb(200, 120, 40)
                    },
                    format!(
                        "0,0 实际落在纸面{at}（{:.0}, {:.0}）{}",
                        ox,
                        oy,
                        if ok { "" } else { "  ← 不是左下，正稿会整体偏移/翻转" }
                    ),
                );
                if !ok
                    && ui
                        .add(theme::soft_button(&p, "设为左下原点"))
                        .on_hover_text(
                            "把原点角设成左下、并清掉反转 X/Y。四角定位与正文都以这个点为基准。",
                        )
                        .clicked()
                {
                    app.origin_corner = 2;
                    app.reverse_x = false;
                    app.reverse_y = false;
                    app.status = "原点已设为纸面左下角 (0,0)".into();
                }
                ui.add(egui::Slider::new(&mut app.machine_w_mm, 50.0..=600.0).text("机器宽(mm)"));
                ui.add(egui::Slider::new(&mut app.machine_h_mm, 50.0..=800.0).text("机器高(mm)"));
                ui.add(egui::Slider::new(&mut app.jog_speed, 100.0..=30000.0).text("点动速度(mm/min)"));
                ui.weak("幅面只用于越界提示，不会改变绘制坐标。");
            });

        egui::CollapsingHeader::new("抬落笔细节")
            .id_salt("gen_pen")
            .show(ui, |ui| {
                if app.firmware_sel == 1 {
                    if app.pen_mode_sel == 1 {
                        ui.add(egui::Slider::new(&mut app.marlin_servo_down, 0.0..=180.0).text("落笔角度"));
                        ui.add(egui::Slider::new(&mut app.marlin_servo_up, 0.0..=180.0).text("抬笔角度"));
                    } else {
                        ui.add(egui::Slider::new(&mut app.marlin_pen_down, -20.0..=40.0).text("落笔 Z(mm)"));
                        ui.add(egui::Slider::new(&mut app.marlin_pen_up, -20.0..=40.0).text("抬笔 Z(mm)"));
                    }
                    ui.add(egui::Slider::new(&mut app.marlin_z_speed, 100.0..=30000.0).text("Z 轴速度"));
                } else if app.firmware_sel == 2 {
                    ui.add(egui::Slider::new(&mut app.mm_to_steps, 1.0..=1000.0).text("毫米→步数"));
                    ui.add(egui::Slider::new(&mut app.ebb_servo_speed, 0.0..=32767.0).text("舵机速度"));
                    ui.add(egui::Slider::new(&mut app.ebb_max_pen, 0.0..=65535.0).text("最大舵机值"));
                    ui.add(egui::Slider::new(&mut app.max_pen_speed, 1000.0..=32767.0).text("最大抬落笔速度"));
                } else {
                    ui.weak("GRBL 使用上方的抬笔 / 落笔 Z 值。");
                }
                ui.checkbox(&mut app.laser_on, "激光模式（M3/M5 代替落笔）");
                ui.add_enabled(
                    app.laser_on,
                    egui::Slider::new(&mut app.laser_power, 0.0..=1000.0).text("激光功率 S"),
                );
            });

        egui::CollapsingHeader::new("自定义 G-code")
            .id_salt("gen_custom")
            .show(ui, |ui| {
                ui.label("起始代码（多条用 ; 分隔）");
                ui.add(TextEdit::multiline(&mut app.custom_start).desired_rows(2));
                ui.label("结束代码");
                ui.add(TextEdit::multiline(&mut app.custom_end).desired_rows(2));
            });

        ui.add(
            egui::Slider::new(&mut app.min_step_mm, 0.1..=1.0)
                .text("G-code 最小步进(mm)"),
        );
        ui.weak("越小越贴近原字（G-code 更长），默认 0.2mm。");
    });

    ui.add_space(6.0);
    section(ui, "导出");
    theme::card(ui, &p, |ui| {
        ui.horizontal_wrapped(|ui| {
            if ui.add(theme::primary_button(&p, "生成并预览 G-code")).clicked() {
                app.gcode_text = app.generate_gcode();
                app.status = "已生成 G-code".into();
            }
            if ui.add(theme::soft_button(&p, "导出 .nc")).clicked() {
                act_export(app, Export::Nc);
            }
            if ui.add(theme::soft_button(&p, "导出 SVG")).clicked() {
                act_export(app, Export::Svg);
            }
            if ui.add(theme::soft_button(&p, "导出 PDF")).clicked() {
                act_export(app, Export::Pdf);
            }
            if ui.add(theme::soft_button(&p, "导出 PNG")).clicked() {
                act_export(app, Export::Png);
            }
        });
        if !app.gcode_text.is_empty() {
            ui.horizontal(|ui| {
                theme::pill(
                    ui,
                    &format!("{} 行", app.gcode_text.lines().count()),
                    p.accent,
                    p.card_hover,
                );
                if app.bounds_check {
                    if let Some(msg) = app.bounds_warning(&app.gcode_text) {
                        theme::pill(ui, "超出幅面", p.danger, p.card_hover);
                        let _ = msg;
                    } else {
                        theme::pill(ui, "幅面正常", p.success, p.card_hover);
                    }
                }
            });
            egui::CollapsingHeader::new("G-code 预览（前 60 行）")
                .id_salt("gcode_preview")
                .default_open(true)
                .show(ui, |ui| {
                    let mut head: String =
                        app.gcode_text.lines().take(60).collect::<Vec<_>>().join("\n");
                    ui.add(
                        TextEdit::multiline(&mut head)
                            .desired_rows(10)
                            .interactive(false)
                            .font(egui::TextStyle::Monospace),
                    );
                });
        } else {
            ui.weak("点击「生成并预览 G-code」查看实际下发的代码。");
        }
    });
}

/// 图片页：导入（前景可打印 / 背景参考）、多选、删除/清空（二次确认）、
/// 缩放 / 移动 / 居中 / 适应页面等完整变换。
fn panel_images(app: &mut AppState, ui: &mut Ui) {
    const ANGLES: [&str; 4] = ["横向 →", "对角 45° ↘", "纵向 ↓", "对角 135° ↗"];
    const ORDERS: [&str; 4] = ["默认", "最优路径", "按扫描顺序", "分块蛇形"];
    let p = pal(ui);
    // 窄面板下所有行都必须能换行，否则内容会撑破右侧栏并被裁切。
    // 下拉框本身也要按行宽收窄，避免标签 + 190pt 下拉框刚好顶出栏外。
    let combo_w = (ui.available_width() - 100.0).clamp(140.0, 250.0);
    section(ui, "图片");
    ui.label("导入图片：");
    ui.horizontal_wrapped(|ui| {
        if ui.button("导入前景图片（可打印）").clicked() {
            if let Some(p) = app.pick_file("图片", &["png", "jpg", "jpeg", "gif", "bmp"]) {
                app.begin_import(&p, false);
            }
        }
        if ui.button("导入背景参考图").clicked() {
            if let Some(p) = app.pick_file("图片", &["png", "jpg", "jpeg", "gif", "bmp"]) {
                app.begin_import(&p, true);
            }
        }
    });
    ui.weak("选好文件后可确认成品尺寸与精细度；不会再把大图静默压小。");
    ui.weak("画布操作：点击选图 · 按住拖动移动 · 滚轮缩放当前选中图片");

    // 模块级输出开关统一放在左侧栏「输出图层」，避免两个地方各有一个开关。
    ui.horizontal_wrapped(|ui| {
        theme::pill(
            ui,
            if app.images_on { "图片图层：输出" } else { "图片图层：仅参考" },
            if app.images_on { p.success } else { p.muted },
            p.card_hover,
        );
        ui.label(
            RichText::new("总开关在左侧栏「输出图层 → 图片」")
                .size(9.5)
                .color(p.muted),
        );
    });

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

    // 大纸张分页：图片大于单页幅面时切成多页，逐页定位绘制。
    ui.separator();
    section(ui, "大纸张分页绘制");
    ui.add_enabled_ui(!app.images.is_empty(), |ui| {
        ui.checkbox(&mut app.tile_on, "启用分页（大图分多页绘制）")
            .on_hover_text("按单页可绘制幅面切分前景图片，一次只画一页；页面按角标拼接");
    });
    if app.tile_on {
        if let Some(layout) = app.tile_layout() {
            let count = layout.count();
            let cur = app.tile_page.min(count.saturating_sub(1));
            app.tile_page = cur;
            let (min_x, min_y, max_x, max_y) = app
                .output_image_bounds()
                .unwrap_or((0.0, 0.0, 0.0, 0.0));
            ui.label(
                RichText::new(format!(
                    "图片范围 {:.0}×{:.0}mm · 单页 {:.0}×{:.0}mm · {} 列 × {} 行 · 共 {} 页",
                    max_x - min_x,
                    max_y - min_y,
                    layout.width_mm,
                    layout.height_mm,
                    layout.cols,
                    layout.rows,
                    count
                ))
                .size(10.0)
                .color(p.muted),
            );
            ui.checkbox(&mut app.tile_marks, "每页绘制 1cm 四角定位标记")
                .on_hover_text("四个页角各画一个 L 形，尖端朝页角、开口朝内；画完回到原点");
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(cur > 0, egui::Button::new("◀ 上一页"))
                    .clicked()
                {
                    app.tile_page = cur - 1;
                }
                ui.label(
                    RichText::new(format!("第 {} / {} 页", cur + 1, count))
                        .strong()
                        .color(p.text),
                );
                if ui
                    .add_enabled(cur + 1 < count, egui::Button::new("下一页 ▶"))
                    .clicked()
                {
                    app.tile_page = cur + 1;
                }
            });
            ui.weak(
                "把笔移到本页左上角后点「开始绘制」；本页画完自动回原点，换纸后再画下一页。",
            );
        } else {
            ui.colored_label(
                p.warning,
                "还没有可输出的图片：请勾选左侧「输出图层 → 图片」，并给图片勾上「输出」。",
            );
        }
        ui.weak("分页模式只输出图片；文字/表格等模块请在关闭分页后单独绘制。");
    }

    // 选中图片的变换与描边参数
    let sel = app.sel_image;
    if sel < app.images.len() {
        ui.separator();
        section(ui, &format!("选中图片 #{} 的变换", sel + 1));

        // —— 缩放 ——
        ui.label(RichText::new("缩放").strong());
        let cur_scale = app.images[sel].scale;
        // 范围要和 `scale_image` 的 0.05..=20 一致：否则滚轮放大到 8 以上，
        // 滑块每帧会把值夹回 8，图片就「怎么滚都不再变大」。
        ui.add(egui::Slider::new(&mut app.images[sel].scale, 0.05..=20.0).text("缩放(×)"));
        // 滑块直接改 scale 会让左上角锚点漂移，这里换算回「中心不动」
        let sc = app.images[sel].scale;
        if (sc - cur_scale).abs() > 1e-9 {
            app.set_image_scale(sel, sc);
        }
        ui.horizontal_wrapped(|ui| {
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
        // 不要再用固定 ±几百的滑块：页面最大可设到 2000mm，分页大图的位置也会
        // 远超 400/500。滑块每帧都会把越界值夹回范围，画布上就会出现「拖到一半
        // 被吸回去、永远拖不到正确位置」。用 DragValue 按页面尺寸给足范围，并且
        // 只在用户真正改动时写回。
        let (im_x, im_y) = {
            let im = &app.images[sel];
            (im.x, im.y)
        };
        let mut px = im_x;
        let mut py = im_y;
        let span = (app.page_w_mm.max(app.page_h_mm) * 2.0).max(600.0);
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::DragValue::new(&mut px)
                    .range(-span..=span)
                    .clamp_existing_to_range(false)
                    .speed(0.5)
                    .prefix("X ")
                    .suffix(" mm"),
            );
            ui.add(
                egui::DragValue::new(&mut py)
                    .range(-span..=span)
                    .clamp_existing_to_range(false)
                    .speed(0.5)
                    .prefix("Y ")
                    .suffix(" mm"),
            );
        });
        if (px - im_x).abs() > 1e-9 || (py - im_y).abs() > 1e-9 {
            app.set_image_pos(sel, px, py);
        }
        let step = if ui.input(|i| i.modifiers.shift) { 10.0 } else { 1.0 };
        ui.horizontal_wrapped(|ui| {
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
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut app.images[sel].flip_h, "水平翻转");
            ui.checkbox(&mut app.images[sel].flip_v, "垂直翻转");
        });
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut app.images[sel].background, "作为背景参考（只临摹不打印）");
            if !app.images[sel].background {
                ui.checkbox(&mut app.images[sel].out, "并入输出");
            }
        });
        ui.horizontal_wrapped(|ui| {
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
        let mut mode_sel = app.images[sel].draw.normalized();
        ui.horizontal_wrapped(|ui| {
            ui.label("绘制类型:");
            ComboBox::from_id_salt("img_draw_sel")
                .selected_text(mode_sel.name())
                .width(combo_w)
                .show_ui(ui, |ui| {
                    for m in ImgDraw::active() {
                        ui.selectable_value(&mut mode_sel, *m, m.name());
                    }
                });
        });
        app.images[sel].draw = mode_sel;
        ui.weak(mode_sel.hint());
        let mode = app.images[sel].draw;
        let uses_step = matches!(
            mode,
            ImgDraw::Wire
                | ImgDraw::Sketch
                | ImgDraw::Continuous
                | ImgDraw::CrossHatch
                | ImgDraw::Halftone
        );
        ui.add_enabled(
            uses_step,
            egui::Slider::new(&mut app.images[sel].step_px, 1..=12)
                .text(if mode == ImgDraw::Halftone { "网点间隔(px)" } else { "影线间隔(px)" }),
        );
        ui.add_enabled(
            mode != ImgDraw::Wire,
            egui::Slider::new(&mut app.images[sel].threshold, 0.1..=0.95)
                .text(if mode == ImgDraw::StrokeSketch { "明暗对比" } else { "暗部阈值" }),
        );
        ui.add_enabled(
            matches!(mode, ImgDraw::Wire | ImgDraw::StrokeSketch),
            egui::Slider::new(&mut app.images[sel].edge_threshold, 0.05..=0.9)
                .text(if mode == ImgDraw::StrokeSketch { "轮廓阈值" } else { "边缘阈值(线框)" }),
        );
        ui.checkbox(&mut app.images[sel].invert, "反相（黑变白）");
        if mode == ImgDraw::StrokeSketch {
            let mut style = app.images[sel].sketch_style;
            ui.horizontal_wrapped(|ui| {
                ui.label("笔触风格:");
                ComboBox::from_id_salt("img_sketch_style")
                    .selected_text(if style == 1 {
                        "随形短笔触（逐遍补墨）"
                    } else {
                        "交叉排线基底"
                    })
                    .width(combo_w)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut style, 1, "随形短笔触（逐遍补墨）")
                            .on_hover_text(
                                "0.20 的落笔流水线：量出每个小块还差多少墨，用 1~5mm 短笔触\
                                 沿轮廓/墨脊去补，一遍不够下一遍接着补，直到目标墨量。",
                            );
                        ui.selectable_value(&mut style, 0, "交叉排线基底（等距平行线）")
                            .on_hover_text(
                                "0.25 的落笔流水线：全图两族平行线打底，按笔宽分档加密，\
                                 线条更规整、耗时更短。",
                            );
                    });
            });
            app.images[sel].sketch_style = style;
            if style == 1 {
                ui.add(
                    egui::Slider::new(&mut app.images[sel].hatch_levels, 2..=8)
                        .text("补墨遍数"),
                )
                .on_hover_text(
                    "最多补几遍。每一遍都重新量「目标墨 − 当前墨」，只补还差的缺口；\
                     遍数越多，暗部越压实，耗时也越长。",
                );
                ui.add(
                    egui::Slider::new(&mut app.images[sel].stroke_len_mm, 1.0..=8.0)
                        .text("短笔触长度(mm)"),
                );
                ui.horizontal_wrapped(|ui| {
                    ui.label("走向参考:");
                    const FOLLOW_NAMES: [&str; 4] = [
                        "跟随轮廓",
                        "跟随原稿笔触",
                        "混合（轮廓+笔触）",
                        "交叉明暗调子",
                    ];
                    ComboBox::from_id_salt("img_stroke_follow")
                        .selected_text(FOLLOW_NAMES[usize::from(app.images[sel].stroke_follow).min(3)])
                        .width(combo_w)
                        .show_ui(ui, |ui| {
                            for (i, name) in FOLLOW_NAMES.iter().enumerate() {
                                ui.selectable_value(
                                    &mut app.images[sel].stroke_follow,
                                    i as u8,
                                    *name,
                                )
                                .on_hover_text(match i {
                                    0 => "笔触沿轮廓切线走，形体边界最清楚。",
                                    1 => "笔触沿原稿里已有的笔触方向走，最贴近原作。",
                                    2 => "前两者按置信度混合，平滑处跟轮廓、有笔触处跟原稿。",
                                    _ => "浅调子只走一个方向，越暗越叠加交叉方向（45°/90°/135°），\
                                          画面对比更丰富，不再是一边倒的单向排线。",
                                });
                            }
                        });
                });
                ui.add(
                    egui::Slider::new(&mut app.images[sel].sketch_jitter, 0.0..=0.8)
                        .text("手抖幅度(mm)"),
                );
                ui.add(
                    egui::Slider::new(&mut app.images[sel].hatch_angle, 0.0..=180.0)
                        .text("交叉基准角(°)"),
                );
                ui.weak(
                    "逐遍补残留墨：先把每个小块的目标墨量与当前覆盖做差，差得多就排得密，\
                     差得少就留白；笔触用 1~5mm 的短直线，方向跟着轮廓走，暗部再按固定角差\
                     叠交叉遍。原稿里的真实细线（发丝、睫毛、衣褶）由细节层单独描一遍。\
                     这是 0.20 的落笔方式，版面更「脏」也更像手绘。",
                );
            } else {
                ui.add(
                    egui::Slider::new(&mut app.images[sel].hatch_levels, 2..=8)
                        .text("排线层数（HB→8B）"),
                );
                ui.add(
                    egui::Slider::new(&mut app.images[sel].hatch_angle, 0.0..=180.0)
                        .text("排线基准角(°)"),
                );
                ui.add(
                    egui::Slider::new(&mut app.images[sel].sketch_jitter, 0.0..=0.8)
                        .text("手抖幅度(mm)"),
                );
                ui.weak(
                    "以交叉排线为底：全图只用「基准角 / 基准角+90°」两族平行线，行距由笔宽决定。\
                     调子越高、缺口越大，同一族线排得越密；每两层换一支更深的笔（HB→2B→4B→8B），\
                     所以暗部是几族平行线的叠加，而不是一坨涂黑。原稿里真实存在的细线（发丝、\
                     睫毛、衣褶）由细节层单独描一遍，压在排线之上。层数越多，最深的头发/瞳孔\
                     才压得下去；亮部没有缺墨的空间，自然留白。",
                );
            }
            if !app.images[sel].multi_pen {
                ui.add(
                    egui::Slider::new(&mut app.images[sel].sketch_pen, 0.2..=1.2)
                        .text("画笔线宽(mm)"),
                );
            }
            ui.separator();
            ui.checkbox(
                &mut app.images[sel].multi_pen,
                "多笔分层（换笔绘制 HB → 2B → 4B → 8B）",
            )
            .on_hover_text(
                "把「还差多少墨」按每支笔能压出的最深色分带：浅笔先补浅的一档，深笔接着补剩下的。\
                 笔越深，同一处需要的交叉遍数越少；绘制时仍会逐支提示换笔。",
            );
            if app.images[sel].multi_pen {
                let n = app.active_pens().len();
                if n < 2 {
                    ui.colored_label(
                        p.warning,
                        "多笔分层至少需要两支启用中的笔：请在下方「多笔分层 · 笔组」里勾选。",
                    );
                } else {
                    ui.weak(format!(
                        "当前 {n} 支笔参与明暗分带（亮 → 暗）；浅笔负责浅灰，深笔负责剩下的黑场。"
                    ));
                }
            }
        }

        if mode == ImgDraw::CrossHatch {
            ui.separator();
            ui.checkbox(
                &mut app.images[sel].multi_pen,
                "按明暗多笔分层（浅笔铺底，深笔补暗部）",
            )
            .on_hover_text(
                "第 1 支（最浅）画全部排线；之后每支更深的笔只在更暗的区域补一层交叉排线，\
                 阈值逐层 ×0.72，方向依次换成 90° / 45° / 135°。亮部只由浅笔铺一遍，\
                 深色块则由多支笔叠出层次，不会一上来就糊黑。",
            );
            if app.images[sel].multi_pen {
                let n = app.active_pens().len();
                if n < 2 {
                    ui.colored_label(
                        p.warning,
                        "多笔分层至少需要两支启用中的笔：请在下方「多笔分层 · 笔组」里勾选。",
                    );
                } else {
                    ui.weak(format!(
                        "当前 {n} 支笔：第 1 支铺全部排线，其余 {} 支依次补更暗的交叉层。",
                        n - 1
                    ));
                }
            } else {
                ui.add(
                    egui::Slider::new(&mut app.images[sel].sketch_pen, 0.2..=1.2)
                        .text("画笔线宽(mm)"),
                );
            }
        }
        ui.separator();
        ui.label(RichText::new("绘画方式").strong());
        let uses_scan = matches!(
            mode,
            ImgDraw::Wire
                | ImgDraw::Sketch
                | ImgDraw::Continuous
                | ImgDraw::CrossHatch
                | ImgDraw::StrokeSketch
        );
        ui.add_enabled_ui(uses_scan, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label("扫描方向:");
                ComboBox::from_id_salt("img_scan_angle")
                    .selected_text(ANGLES[usize::from(app.images[sel].scan_angle).min(3)])
                    .width(combo_w)
                    .show_ui(ui, |ui| {
                        for (i, name) in ANGLES.iter().enumerate() {
                            ui.selectable_value(
                                &mut app.images[sel].scan_angle,
                                i as u8,
                                *name,
                            );
                        }
                    });
            });
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("笔画顺序:");
            ComboBox::from_id_salt("img_order_mode")
                .selected_text(ORDERS[usize::from(app.images[sel].order_mode).min(3)])
                .width(combo_w)
                .show_ui(ui, |ui| {
                    for (i, name) in ORDERS.iter().enumerate() {
                        ui.selectable_value(
                            &mut app.images[sel].order_mode,
                            i as u8,
                            *name,
                        );
                    }
                });
        });
        ui.add_enabled(
            mode == ImgDraw::Continuous,
            egui::Slider::new(&mut app.images[sel].merge_gap_px, 0..=40)
                .text("连笔跨行容差(px)"),
        );
        ui.add_enabled(
            app.images[sel].order_mode == 3,
            egui::Slider::new(&mut app.images[sel].block_px, 16..=120).text("分块边长(px)"),
        );
        if mode == ImgDraw::StrokeSketch {
            match app.images[sel].order_mode {
                1 => ui.weak("全部笔触就近串联：空移最少，但打乱「先浅后深、先铺后压」的层次。"),
                2 => ui.weak("按扫描顺序成排推进：同一片区域一路画过去，最接近人手排线。"),
                3 => ui.weak("分块蛇形：块内就近、块间来回，适合大幅面分批绘制。"),
                _ => ui.weak(
                    "按明暗分带逐层画：浅笔先铺、深笔后压，每一遍内就近串联；暗部补不够的墨会在下一次交叉层里继续补。",
                ),
            }
        } else {
            match app.images[sel].order_mode {
                1 => ui.weak("最优路径：最近邻 + 2-opt 重排并允许笔画反向，空移最短。"),
                2 => ui.weak("按扫描顺序：保持从上到下（或当前扫描方向）的原始顺序。"),
                3 => ui.weak("分块蛇形：切成方格逐块「犁地」，块内最优、块间蛇形，路线规整。"),
                _ => ui.weak("默认：素描/线框按扫描顺序，高效连续笔画自动做最优路径。"),
            }
        };
        if !uses_scan {
            ui.weak("中心线 / 描边 / 网点不使用扫描方向；线条走向由原稿决定。");
        } else if mode == ImgDraw::StrokeSketch {
            ui.weak("扫描方向叠加到「无明确走向」区域的基准角上；有轮廓/原稿走向的地方优先跟随画面。");
        }

        // —— 裁剪 ——
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("裁剪").strong());
            if ui.button("重置裁剪").clicked() {
                app.images[sel].crop = (0.0, 0.0, 1.0, 1.0);
            }
            if app.images[sel].is_cropped() {
                ui.colored_label(
                    egui::Color32::from_rgb(200, 120, 40),
                    "已裁剪（比例基于原图）",
                );
            }
        });
        let crop = &mut app.images[sel].crop;
        ui.add(egui::Slider::new(&mut crop.0, 0.0..=0.95).text("左"));
        ui.add(egui::Slider::new(&mut crop.1, 0.0..=0.95).text("上"));
        ui.add(egui::Slider::new(&mut crop.2, 0.05..=1.0).text("右"));
        ui.add(egui::Slider::new(&mut crop.3, 0.05..=1.0).text("下"));
        let (cl, ct, cr, cb) = app.images[sel].crop;
        if cr - cl < 0.02 || cb - ct < 0.02 {
            ui.colored_label(egui::Color32::from_rgb(200, 60, 60), "裁剪区域太小（宽/高不足 2%）");
        }

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
        if app.img_regen_at.is_some() {
            // 防抖期间给出明确反馈，否则用户会觉得「调了没反应」
            ui.colored_label(
                egui::Color32::from_rgb(200, 140, 40),
                "参数已改，松开后重新计算笔迹…",
            );
        }
    }

    // —— 多笔分层：真实的笔组 ——
    // 笔宽不是「限制」而是参数：0.25mm 针管笔铺浅灰，1.0mm 8B 压暗部。
    ui.separator();
    section(ui, "多笔分层 · 笔组");
    ui.weak(
        "按「亮 → 暗」排列。每支笔的线宽决定一根线盖多宽，深度决定它满覆盖时能压多黑；\
         多笔排线按深度把明暗带分给各笔，所以 HB 不用硬排到死黑，黑场交给 8B。",
    );
    ui.horizontal_wrapped(|ui| {
        if ui.button("铅笔组 HB/2B/4B/8B").clicked() {
            app.pens = crate::state::default_pens();
            app.main_pen = 1;
            app.status = "已套用铅笔笔组".into();
        }
        if ui.button("针管笔 0.25–1.0").clicked() {
            app.pens = crate::state::needle_pens();
            app.main_pen = 1;
            app.status = "已套用针管笔笔组".into();
        }
    });
    let active_names: Vec<String> = app.active_pens().iter().map(|p| p.name.clone()).collect();
    ui.horizontal_wrapped(|ui| {
        ui.label("文字 / 边框 / 四角标记用笔:");
        egui::ComboBox::from_id_salt("main_pen_sel")
            .selected_text(
                active_names
                    .get(app.main_pen_idx())
                    .cloned()
                    .unwrap_or_else(|| "—".to_string()),
            )
            .width((combo_w - 40.0).max(100.0))
            .show_ui(ui, |ui| {
                for (i, n) in active_names.iter().enumerate() {
                    ui.selectable_value(&mut app.main_pen, i, n);
                }
            });
    });
    if !active_names.is_empty() && app.main_pen >= active_names.len() {
        app.main_pen = active_names.len() - 1;
    }
    let n_pens = app.pens.len();
    let mut remove_pen: Option<usize> = None;
    for i in 0..n_pens {
        let pen = &mut app.pens[i];
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut pen.on, "")
                .on_hover_text("取消勾选后，这支笔的明暗带会分给其他笔");
            ui.color_edit_button_srgb(&mut pen.color)
                .on_hover_text("预览与分笔标签用的笔色");
            ui.add(
                egui::TextEdit::singleline(&mut pen.name)
                    .desired_width(78.0)
                    .hint_text("笔名"),
            );
            ui.add(
                egui::DragValue::new(&mut pen.width_mm)
                    .range(0.15..=1.6)
                    .speed(0.05)
                    .suffix("mm"),
            );
        });
        ui.horizontal_wrapped(|ui| {
            ui.add(egui::Slider::new(&mut pen.darkness, 0.2..=1.0).text("深度"));
            ui.add(
                egui::DragValue::new(&mut pen.offset_mm[0])
                    .range(-5.0..=5.0)
                    .speed(0.05)
                    .prefix("X ")
                    .suffix("mm"),
            )
            .on_hover_text("换笔后的套准偏移：这一层整体平移，补偿笔尖位置差");
            ui.add(
                egui::DragValue::new(&mut pen.offset_mm[1])
                    .range(-5.0..=5.0)
                    .speed(0.05)
                    .prefix("Y ")
                    .suffix("mm"),
            );
            if n_pens > 1 && ui.small_button("✕").on_hover_text("移除这支笔").clicked() {
                remove_pen = Some(i);
            }
        });
    }
    if let Some(i) = remove_pen {
        app.pens.remove(i);
        app.status = "已移除一支笔".into();
    }
    if app.pens.len() < 4 && ui.button("+ 添加一支笔").clicked() {
        let w = app.pens.last().map(|p| p.width_mm).unwrap_or(0.5) + 0.15;
        let d = app.pens.last().map(|p| p.darkness).unwrap_or(0.6) + 0.1;
        app.pens.push(crate::state::PenSpec {
            name: format!("笔 {}", app.pens.len() + 1),
            width_mm: w.clamp(0.15, 1.6),
            darkness: d.clamp(0.2, 1.0),
            color: [40, 40, 40],
            offset_mm: [0.0, 0.0],
            on: true,
        });
    }
    if app.multi_pen_active() {
        let mut seen = [false; 4];
        for &t in &app.preview_pens {
            if t != crate::state::NON_PEN && (t as usize) < 4 {
                seen[t as usize] = true;
            }
        }
        let used = seen.iter().filter(|b| **b).count().max(1);
        ui.colored_label(
            p.accent,
            format!("★ 当前会分 {used} 支笔绘制（点「开始绘制」后逐支提示换笔）"),
        );
        if app.pen_running {
            ui.weak(format!(
                "队列进行中：第 {}/{} 支",
                (app.pen_job_idx + 1).min(app.pen_jobs.len().max(1)),
                app.pen_jobs.len().max(1)
            ));
        }
    }
    // 笔宽 / 深度 / 启用状态变了要重算多笔明暗带（函数内部有防抖与指纹判断）。
    if sel < app.images.len() {
        app.regenerate_image(sel);
    }

    // 默认导入参数（作用于后续导入）：不常用，默认折叠，和「手写风格」一致。
    ui.add_space(8.0);
    theme::collapsible_card(ui, &p, "默认导入参数", false, |ui| {
        ui.add(egui::Slider::new(&mut app.img_target_h, 30.0..=400.0).text("目标高度(mm)"));
        ui.add(egui::Slider::new(&mut app.img_step, 1..=12).text("影线/网点间隔(px)"));
        ui.add(egui::Slider::new(&mut app.img_thr, 0.1..=0.95).text("暗部阈值"));
        ui.add(egui::Slider::new(&mut app.img_edge, 0.05..=0.9).text("边缘阈值(线框)"));
        ui.checkbox(&mut app.img_invert, "反相");
        let mut mode_sel = ImgDraw::from_usize(app.img_draw_type);
        ui.horizontal_wrapped(|ui| {
            ui.label("默认类型:");
            ComboBox::from_id_salt("img_default_draw")
                .selected_text(mode_sel.name())
                .width(combo_w)
                .show_ui(ui, |ui| {
                    for m in ImgDraw::active() {
                        ui.selectable_value(&mut mode_sel, *m, m.name());
                    }
                });
        });
        app.img_draw_type = mode_sel as usize;
        ui.weak(mode_sel.hint());
        ui.horizontal_wrapped(|ui| {
            ui.label("默认扫描方向:");
            ComboBox::from_id_salt("img_default_scan")
                .selected_text(ANGLES[usize::from(app.img_scan_angle).min(3)])
                .width(combo_w)
                .show_ui(ui, |ui| {
                    for (i, name) in ANGLES.iter().enumerate() {
                        ui.selectable_value(&mut app.img_scan_angle, i as u8, *name);
                    }
                });
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("默认笔画顺序:");
            ComboBox::from_id_salt("img_default_order")
                .selected_text(ORDERS[usize::from(app.img_order_mode).min(3)])
                .width(combo_w)
                .show_ui(ui, |ui| {
                    for (i, name) in ORDERS.iter().enumerate() {
                        ui.selectable_value(&mut app.img_order_mode, i as u8, *name);
                    }
                });
        });
        ui.add(egui::Slider::new(&mut app.img_merge_gap, 0..=40).text("连笔跨行容差(px)"));
        ui.add(egui::Slider::new(&mut app.img_block_px, 16..=120).text("分块边长(px)"));
        let mut style = app.img_sketch_style;
        ui.horizontal_wrapped(|ui| {
            ui.label("默认笔触风格:");
            ComboBox::from_id_salt("img_default_style")
                .selected_text(if style == 1 {
                    "随形短笔触（逐遍补墨）"
                } else {
                    "交叉排线基底"
                })
                .width(combo_w)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut style, 1, "随形短笔触（逐遍补墨）");
                    ui.selectable_value(&mut style, 0, "交叉排线基底");
                });
        });
        app.set_sketch_style(style);
        ui.horizontal_wrapped(|ui| {
            ui.label("默认精细度:");
            ComboBox::from_id_salt("img_default_max_side")
                .selected_text(if app.img_max_side == 0 {
                    "原图".to_string()
                } else {
                    format!("{}px", app.img_max_side)
                })
                .width(combo_w)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut app.img_max_side, 0, "原图（不缩放）");
                    for v in [3600u32, 3000, 2400, 1800] {
                        ui.selectable_value(&mut app.img_max_side, v, format!("长边 {v}px"));
                    }
                });
        });
        ui.add(egui::Slider::new(&mut app.img_sketch_jitter, 0.0..=0.8).text("笔触手抖幅度(mm)"));
        ui.add(egui::Slider::new(&mut app.img_sketch_pen, 0.2..=1.2).text("笔触线宽(mm)"));
        ui.add(
            egui::Slider::new(&mut app.img_hatch_levels, 2..=8).text(if app.img_sketch_style == 1 {
                "补墨遍数"
            } else {
                "排线层数（HB→8B）"
            }),
        );
        ui.add(egui::Slider::new(&mut app.img_hatch_angle, 0.0..=180.0).text("排线基准角(°)"));
        ui.add(egui::Slider::new(&mut app.img_stroke_len_mm, 1.0..=8.0).text("短笔触长度(mm)"));
        ui.weak("以上为默认值；单张图片可在上方单独覆盖");
    });
}

/// 运行中页：实时进度（当前行/总行、已用/剩余时间、进度页）与控制。
fn panel_running(app: &mut AppState, ui: &mut Ui) {
    let p = pal(ui);
    section(ui, "运行详情");
    let snap = app.device.snapshot();

    if !snap.running {
        theme::notice(
            ui,
            &p,
            "当前空闲：点底部操作条的「开始绘制」下发任务；运行中可自由切换到其它页继续编辑。",
            p.accent,
        );
    }

    theme::card(ui, &p, |ui| {
        theme::status_dot(
            ui,
            if app.dev_connected { p.success } else { p.muted },
            if app.dev_connected { "设备已连接" } else { "设备未连接（可在设备页连接或切模拟）" },
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
        ui.horizontal(|ui| {
            ui.label(RichText::new("状态").size(11.0).color(p.muted));
            ui.label(RichText::new(state).size(13.0).strong().color(p.text));
        });
        let frac = if snap.total_lines > 0 {
            snap.done_lines as f64 / snap.total_lines as f64
        } else {
            0.0
        };
        ui.add(
            egui::ProgressBar::new(frac as f32)
                .desired_width(f32::INFINITY)
                .text(format!("{:.0}%  ·  {} / {} 行", frac * 100.0, snap.done_lines, snap.total_lines)),
        );
        let elapsed = snap.elapsed_ms as f64 / 1000.0;
        let remain = if snap.total_lines > 0 && snap.done_lines > 0 && snap.running {
            elapsed / snap.done_lines as f64 * (snap.total_lines - snap.done_lines) as f64
        } else {
            app.estimated_seconds()
        };
        ui.horizontal(|ui| {
            theme::stat(ui, &p, "已用", &fmt_duration(elapsed), p.text);
            theme::stat(ui, &p, "预计剩余", &fmt_duration(remain), p.accent);
            theme::stat(ui, &p, "笔迹", &format!("{}", app.stats.strokes), p.text);
        });
    });

    ui.add_space(6.0);
    section(ui, "任务统计");
    theme::card(ui, &p, |ui| {
        ui.horizontal_wrapped(|ui| {
            theme::stat(ui, &p, "墨线长度", &format!("{:.2} m", app.stats.ink_mm / 1000.0), p.text);
            theme::stat(ui, &p, "空移行程", &format!("{:.1} m", app.stats.travel_mm / 1000.0), p.warning);
            theme::stat(ui, &p, "总点数", &format!("{}", app.stats.points), p.muted);
            theme::stat(ui, &p, "预计耗时", &fmt_duration(app.estimated_seconds()), p.accent);
        });
        ui.weak("估算含快速空移、加减速与每行指令开销；开始绘制前的计算窗口会给出按实际 G-code 逐行累计的精确值。");
    });

    ui.add_space(6.0);
    section(ui, "G-code 跟踪");
    theme::card(ui, &p, |ui| {
        if app.gcode_text.is_empty() {
            ui.weak("尚未生成 G-code：先点底部「开始绘制」，或到「项目与输出」页生成预览。");
        } else {
            let lines: Vec<&str> = app.gcode_text.lines().collect();
            let cur = snap.done_lines.min(lines.len());
            let start = cur.saturating_sub(10);
            let end = (cur + 12).min(lines.len());
            let mut view = String::new();
            for (i, l) in lines.iter().enumerate().take(end).skip(start) {
                let mark = if i + 1 == cur { "▶ " } else { "  " };
                view.push_str(&format!("{}{:>4}: {}\n", mark, i + 1, l));
            }
            ui.add(
                TextEdit::multiline(&mut view)
                    .desired_rows(12)
                    .interactive(false)
                    .font(egui::TextStyle::Monospace),
            );
        }
    });
}

/// 页面边框（对应源程序 edge 模块）；外层已有「页面边框」折叠标题，这里不再重复。
fn panel_border(app: &mut AppState, ui: &mut Ui) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ImageObject;

    fn app_with_image() -> AppState {
        // 空字库也能构造 AppState：这里只测布局，不生成真实笔迹。
        let mut app = AppState::new(crate::fontmgr::FontManager { fonts: vec![] }, String::new());
        app.tab = 5;
        // 分页打开时会多出整页导航与提示行，一并纳入窄栏回归。
        app.tile_on = true;
        app.images.push(ImageObject {
            path: "layout.png".into(),
            bytes: Vec::new(),
            x: 30.0,
            y: 30.0,
            scale: 1.0,
            rotation: 0.0,
            flip_h: false,
            flip_v: false,
            draw: ImgDraw::StrokeSketch,
            step_px: 3,
            threshold: 0.55,
            edge_threshold: 0.25,
            invert: false,
            crop: (0.0, 0.0, 1.0, 1.0),
            scan_angle: 0,
            merge_gap_px: 8,
            order_mode: 0,
            block_px: 40,
            sketch_pressure: 3,
            sketch_jitter: 0.12,
            sketch_pen: 0.5,
            hatch_levels: 6,
            hatch_angle: 35.0,
            region_block_mm: 2.5,
            stroke_len_mm: 3.2,
            stroke_follow: 2,
            sketch_style: 1,
            max_side_px: 3600,
            multi_pen: true,
            background: false,
            out: true,
            base: vec![Polyline::new(vec![(0.0, 0.0), (40.0, 40.0)])],
            base_pens: vec![0],
            natural_w: 40.0,
            natural_h: 40.0,
            gen_key: 0,
        });
        app
    }

    /// 在指定宽度的“参数栏”里渲染一次图片页，返回绘制结果。
    fn render_image_panel(app: &mut AppState, width: f32) -> egui::FullOutput {
        use egui::{pos2, vec2, Rect};

        let ctx = egui::Context::default();
        crate::setup_fonts(&ctx);
        crate::theme::apply(&ctx, false);
        let panel = Rect::from_min_size(pos2(0.0, 0.0), vec2(width, 2000.0));
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(900.0, 2000.0))),
                ..Default::default()
            },
            |ui| {
                ui.set_width(width);
                ui.set_clip_rect(panel);
                fit_params_width(ui);
                panel_images(app, ui);
            },
        );
        // 测试里没有渲染后端，纹理增量必须手动清掉，否则 Drop 时会 panic。
        output.textures_delta.clear();
        output
    }

    /// 右侧参数栏最窄时（300pt 减去内边距与滚动条），图片页的文字不能越过
    /// 栏边界——越过就会被栏位裁掉。回归：多笔笔组和长标签滑块曾把内容顶出
    /// 右侧栏，用户看到的就是「显示不完整」。
    #[test]
    fn image_panel_fits_narrow_sidebar() {
        let mut app = app_with_image();
        let width = 262.0;
        let output = render_image_panel(&mut app, width);

        let mut worst = 0.0_f32;
        for cs in &output.shapes {
            let r = cs.shape.visual_bounding_rect();
            if r.max.x > width + 2.0 {
                worst = worst.max(r.max.x);
            }
        }
        assert!(
            worst <= width + 2.0,
            "图片参数栏有内容被裁到栏外，最右到 {worst:.1}pt（栏宽 {width:.1}pt）"
        );
    }

    /// 位置不能被右侧栏「夹回去」：大页面上把图拖到 x>400 / y>500 后，
    /// 参数栏每帧重绘不能把坐标写回旧范围，否则画布上永远拖不到目标位置。
    #[test]
    fn image_position_is_not_clamped_by_sidebar() {
        let mut app = app_with_image();
        app.page_w_mm = 1000.0;
        app.page_h_mm = 1000.0;
        app.images[0].x = 850.0;
        app.images[0].y = -120.0;

        let _ = render_image_panel(&mut app, 340.0);

        assert!(
            (app.images[0].x - 850.0).abs() < 1e-9,
            "x 被参数栏夹回：{:.1}",
            app.images[0].x
        );
        assert!(
            (app.images[0].y + 120.0).abs() < 1e-9,
            "y 被参数栏夹回：{:.1}",
            app.images[0].y
        );
    }
}
