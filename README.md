# WritingRobot（奎享雕刻 / Kvenjoy 复刻）

> 用 **Rust** 完整复刻「奎享雕刻（Kvenjoy）」写字机器人上位机软件。
> 把文字用**单线矢量字库（`.gfont`）**渲染成笔迹路径，输出 **G-code**（GRBL / Marlin / EBB 三类固件），并支持预览、参数配置与设备连接。

**当前版本：v0.5.0**（2026-10-17） · 构建：Rust 1.99.1（MSVC） · 测试：124 用例全通过、0 警告

---

## 1. 项目简介

奎享雕刻是控制「写字机器人 / 写字机」通过笔在纸上书写文字的上位机软件。原版基于 Java（JavaFX + 自带 JRE），本仓库用 Rust 从零重写，目标是**行为等价、可驱动同一台写字机**，同时去掉对 Java / JRE / 加密 DLL 的依赖。

核心能力：

- **单线字库（`.gfont`）纯 Rust 解析**：内嵌标准 ZIP + DEFLATE（手写解码器，零外部依赖），无需任何密钥或 FFI。
- **十大 UI 模块**：文字 / 表格 / 格子 / 绘图 / 笔记 / 图片 / 设备 / **测试** / 导入导出 / 运行中，统一产出笔迹多段线。
- **统一管线**：所有模块 → `Vec<Polyline>`（页面 mm，y 向下）→ 生成 G-code / SVG / PDF / PNG。
- **三套固件后端**：GRBL / Marlin / EBB，支持 Z 轴与舵机两种落笔方式。
- **设备连接（后台线程，不卡 UI）**：串口（**蓝牙 SPP / CH340 USB-TTL**）/ TCP / Mock 三种传输后端，全部在独立后台线程跑流控，主线程只投命令 / 读状态，**点动 / 回零 / 运行不再卡死**。串口用纯 Win32 API 实现，零外部 crate。
- **图片导入与描边**：前景可打印图 + 背景参考图（浅色、不打印）、多图重叠、画布内拖拽移动、缩放 / 旋转 / 翻转；三种绘制方式 = **线框（Sobel 边缘）/ 素描（影线）/ 高效（连续笔画 + 最优路径）**。
- **图片绘画方式细粒度可调**（v0.5.0）：在原「影线间隔 / 暗部阈值 / 边缘阈值」之外，新增**扫描方向**（横向 / 45° / 纵向 / 135°）、**笔画顺序**（默认 / 最优路径 / 按扫描顺序 / 分块蛇形）、**连笔跨行容差**、**分块边长**。
- **图片裁剪**（v0.5.0）：按原图比例设定左 / 上 / 右 / 下，裁剪后再描线，尺寸与笔迹同步重算。
- **运行前边界自检**（v0.5.0）：生成 G-code 后解析坐标范围与机器幅面比对，超界**弹窗提示并询问是否仍执行**，避免撞机与软限位报警。
- **画布定位辅助**（v0.4.0）：页面外侧**毫米标尺**（顶 / 左，10mm 刻度、50mm 标数值）+ 可调行列数的**浅色虚线网格**，仅定位不打印；另有 XY 方向箭头与文本框虚框。
- **运行中独立页**：实时 G-code 行进度、已用 / 剩余时间、整页黑 / 灰进度图，与编辑页互不干扰。
- **顶部菜单栏（常用功能总入口）**：**文件**（新建 / 打开工程 / 保存 / 另存为 / 导入字库 / 重扫字库，`Ctrl+S`、`Ctrl+O`）、**导出**（G-code .nc / SVG / PDF / PNG，`Ctrl+E`）、**设备**（连接 / 断开 / 自动连接 / 扫描串口 / 诊断端口 / 串口-TCP-模拟 切换）、**运行**（开始绘制 / 暂停 / 继续 / 停止 / 回原点 / 解锁 `$X` / 机械回零）、**视图**（底部日志栏开关 / 界面缩放 / 设置）；右侧常驻连接状态与当前工程名。
- **底部实时日志栏**：窗口最下面一栏常驻，任何页面都能看到设备通信、G-code 下发回显、错误诊断；可折叠、可拖动调高，运行中还显示 `已完成/总行数`。
- **运行控制条**：主界面顶部常驻「**开始绘制** / 暂停 / 继续 / 停止 / 回原点 / 设置 / 清空本页」，一键把当前页面送到写字机。
- **机器测试模块**：解锁(`$X`)、回原点(`$H`/`G28`)、移到 (0,0)、当前点设为零点(`G92`)、读位置、XY 点动、抬落笔、**画测试方框**、发送自定义 G-code。
- **设置对话框**：不常用设置（激光 / 休眠 / 延时 / 坐标系 / 自定义 G-code / 逼真化 / 图片默认参数 / 运行流控 / 机器幅面）收纳其中，主面板保持精简。
- **工程完整存取**：`.wprj` 一键保存 / 载入全部编辑内容（含图片字节、文本块、设备设置）。
- **导入导出**：工程 JSON、Excel、图片、SVG、G-code(.nc)、PNG；另有 DXF / SVG 矢量导入与 PDF 导入。
- **应用图标**（v0.5.0）：自绘图标（蓝→青渐变底板 + 纸张 + 手写波浪线 + 钢笔），编译期嵌入，标题栏与任务栏生效。

---

## 2. 与源程序的关系

| 维度 | 原版 Kvenjoy | 本仓库（Rust 复刻） |
|---|---|---|
| 语言/运行时 | Java + 自带 JRE（32 位） | Rust（原生可执行文件） |
| UI | JavaFX（JOGL/OpenGL） | egui / eframe 0.36 |
| 字库 | `.gfont`（未加密，内嵌 ZIP+DEFLATE） | 同格式，纯 Rust 解析 |
| 加密 | `KvenjoyEncrypt.dll` 仅用于 licene/激活 | 不涉及（字库本就不加密） |
| 输出 | G-code（GRBL/Marlin/EBB） | 同指令集 |
| 表格导入 | Apache POI | `calamine` |
| 图片转路径 | Potrace 自动描线 + 细化(thinning) | 线框（Sobel）+ 影线 + 连续笔画（扫描线模型，可调方向/顺序/连笔容差） |
| 运行前安全 | 未见对应实现 | **边界自检**（G-code 坐标 vs 机器幅面，超界询问） |

> 原版目录 `奎享雕刻/` 作为**资源与对照样本**保留在本仓库根目录：`.gcodeFonts/`（80 个单线字库）、`*.json`（工程存档样例）、`*.pdf`（官方图文教程）、`config.properties`/`default.properties`（参数样本）、`KXSettings.zip`（设置样本）、`potrace-1.15.win32/`、`ch340.EXE`（USB 驱动）、`录字体.docx`（手机录字说明）。

---

## 3. 功能模块

| # | 模块 | 实现要点 | 产物 |
|---|------|----------|------|
| ① | **文字** | 文本框 → `Item` → 字体栈渲染；对齐 / 段首 / 行距 / 字间距（**可为负**）+ 逼真化随机（可按框生效） | 笔迹折线 |
| ② | **表格** | 边框线条（`line` shape）+ 每格文本；tab/逗号/空格分隔，自适应字号（按**真实字身高度**反推）；Excel 导入填充；**显示横行 / 显示竖列**分别开关 + **线条宽度**（>0.35mm 用并行描边加粗） | 折线 + 文字 |
| ③ | **格子** | 空白 / 田字 / 米字 / 回宫四种格型 + 逐格示范字；**示范字「字心 == 格心」**（按字库墨迹框逐字补偿，实测偏差 0.00mm）；**显示格子总开关** + **显示横行 / 显示竖列** + **线条宽度** | 折线 + 文字 |
| ④ | **绘图** | 图片 **线框** / **素描** / **高效（连续笔画）** 三种描边；**DXF / SVG 矢量文件导入**（解析为笔迹、按目标高度缩放并锚定页面左上角，与手绘同落点，可一键清空） | 折线 |
| ⑤ | **笔记** | 多文本块（每个独立字体 / 字高 / 坐标 / 对齐），可整页编排 | 文字 |
| ⑥ | **图片** | 导入前景图（可打印）/ 背景参考图（浅色、不打印）；多图重叠；**画布点击选中 / 拖动移动 / 滚轮缩放**；面板内**缩放滑块 + 放大缩小按钮 + 适应页面 / 原始尺寸**、**X/Y 位置滑块 + 方向键微调（Shift=10mm）+ 页面居中**、旋转 / 镜像 / 重置变换；**每张图片独立 ✕ 删除** + **清除全部图片（二次确认）**；**裁剪**（左/上/右/下 比例滑块 + 重置）；描边参数：影线间隔、暗部阈值、边缘阈值、反相、**扫描方向**、**笔画顺序**、**连笔跨行容差**、**分块边长** | 折线 |
| ⑦ | **设备** | **后台线程架构**（`DevCmd` mpsc + `Arc<Mutex<DevShared>>`）：串口 / TCP / Mock 三后端；连接 / **自动连接** / **自动波特率** / 查询固件 / **点动 / 单步 / 回零**；机器物理幅面 长/宽 设置（越界提示），**全程不阻塞 UI**；**扫描串口**（读注册表列出全部登记端口并标注在线/离线）、**诊断端口**（逐步诊断每步错误码与建议）、**蓝牙链路**（体检 / 绑定 / 可见性观察）、**清空页面内容** | G-code 下发 |
| ⑧ | **测试** | 解锁(`$X`)/软复位、回原点(`$H`)、移到(0,0)、**当前点设为零点(`G92 X0 Y0`)**、读位置(`?`/`M114`)、XY 点动（含对角线）、抬落笔、**画测试方框**、自定义单行 G-code | 机器运动 |
| ⑨ | **导入导出** | 工程 JSON、Excel、图片、SVG、**PDF(矢量)**、G-code(.nc)、PNG 全链路；**完整工程 `.wprj` 一键存取**（图片字节 / 文本块 / 设备设置）；导出时**可自选位置与文件名** | 文件 I/O |
| ⑩ | **运行中** | 实时 G-code 行进度、已用 / 剩余时间、进度条、附近 G-code；整页进度**已完成=黑 / 未完成=浅灰**；冻结时刻的页面与编辑页互不干扰；**超界检测**开关 + 机器幅面提示 | 进度视图 |
| ⑪ | **页面边框** | 对应源程序 `edge`：实线 / 虚线 / 双线，可调页边距 | 折线 |
| ⑫ | **手绘** | 对应源程序 `squiggle`：在页面内按住鼠标拖动直接生成笔迹（带抽稀） | 折线 |
| ⑬ | **字库管理** | 导入 `.gfont` 文件 / 重新扫描字库目录 | 字体栈 |
| ⑭ | **固件高级 / 坐标系** | 激光模式(M3/M5)、自动休眠、落笔/抬笔延时、原点角(四角)、反转/对调、EBB 舵机参数、自定义 G-code——**收纳在「设置」对话框** | G-code |
| ⑮ | **运行控制条** | 主界面顶部常驻：**开始绘制**（含**边界自检**）/ 暂停 / 继续 / 停止 / 回原点 / 设置 / 清空本页 | 一键送机 |
| ⑯ | **画布定位辅助** | 页面外侧**毫米标尺**（顶 / 左；10mm 刻度、50mm 数值）、可调行列数的**浅色虚线网格**、**XY 方向箭头**（蓝 X / 绿 Y）、文本框虚框——**全部只画在画布上，不进 G-code** | 画布图层 |

---

## 4. 架构与目录

```
WritingRobot/
├── 奎享雕刻/                 # 原版安装包（资源/样本/对照，非运行依赖）
├── wrobot/                  # cargo workspace（Rust 复刻代码）
│   ├── Cargo.toml
│   ├── crates/
│   │   ├── wrobot-core/     # 无 UI 核心：文档模型 + 布局 + 字体 + 渲染 + G-code + 设备通信
│   │   │   ├── doc.rs       # 解析/序列化 .json 工程格式
│   │   │   ├── layout.rs    # 文字框、对齐、行距、字体栈回退、随机变形
│   │   │   ├── render.rs    # 渲染为笔迹多段线 + 线段/矩形/网格 + 路径最优化
│   │   │   ├── gcode.rs     # G-code 生成：grbl/marlin/ebb + 坐标变换 + gcode_xy_bounds 边界解析
│   │   │   ├── serial.rs    # 串口：纯 Win32 FFI（CreateFileW/DCB/ReadFile/WriteFile）+ 逐步诊断
│   │   │   ├── bluetooth.rs # 蓝牙链路：bthprops.dll FFI（配对/绑定/SPP/可见性观察）
│   │   │   ├── pdfimport.rs # PDF 导入（纯 Rust）：对象流/多种解码/路径与图像 XObject/ToUnicode
│   │   │   ├── pdf.rs       # 单页矢量 PDF 导出
│   │   │   ├── dxf.rs       # DXF 导入（纯 Rust）
│   │   │   ├── svg_import.rs# SVG 导入（纯 Rust）
│   │   │   ├── svg.rs       # SVG 导出
│   │   │   ├── config.rs    # default.properties 解析（固件/机器参数）
│   │   │   └── font/
│   │   │       ├── gfont.rs # .gfont 解析（ZIP + 字形记录 + 墨迹框度量）
│   │   │       └── inflate.rs# 纯 Rust DEFLATE 解码（零依赖）
│   │   ├── wrobot-cli/      # 命令行：info/gcode/svg/font/config/png/dxf/svgi/ports/wiretest/probe/bt
│   │   └── wrobot-gui/      # egui 界面：十大模块 + 设备 + 导入导出
│   │       ├── main.rs      # 入口；egui 字体注入（CJK 乱码修复）；应用图标装载
│   │       ├── state.rs     # 中央 AppState 与各模块参数→Item/折线；图片/文本块/工程序列化；边界自检
│   │       ├── ui.rs        # 界面布局与参数面板（模块导航 + 画布 + 超界确认弹窗）
│   │       ├── device.rs    # 设备后台线程：DevCmd mpsc + DevShared + DeviceController + Transport(Serial/Tcp/Mock)
│   │       ├── io.rs        # 导入导出 + 图片描边（扫描线模型/裁剪/排序/连笔）+ 工程 .wprj 存取
│   │       ├── preview.rs   # 预览绘制（纸张 + 标尺 + 网格 + 内容 + 运行中进度页）
│   │       ├── fontmgr.rs   # 字库管理（加载/命名/字体栈）
│   │       └── assets/      # icon.png / icon.ico（应用图标，编译期嵌入）
│   ├── tools/
│   │   └── make_icon.py     # 图标生成器（纯标准库超采样光栅化 → PNG/ICO）
│   ├── out/                 # 发布产物（wrobot-gui.exe / wrobot-cli.exe）
│   └── target/              # 构建输出（target/release 为 .bat 启动目标）
└── README.md
```

### 统一管线

```
文字/表格/格子/绘图/笔记/图片/线条
        │  (各模块产出 Item 或 Polyline；背景参考图只显示、不入 G-code)
        ▼
  document_to_polylines(doc, stack, opts) → Vec<Polyline>   # 页面 mm，y 向下
        │
        ├──▶ generate(&paths, &GcodeOptions)  → G-code（GRBL/Marlin/EBB）
        │        └──▶ gcode_xy_bounds() → 与机器幅面比对 → 超界弹窗询问
        ├──▶ document_to_svg(&doc, &stack, &opts) → SVG
        └──▶ draw_preview(...) / PNG 栅格化      → 预览图像（运行中另冻结一份 run_preview）
```

文本与线条共用同一条路径，保证预览、G-code、导出三者一致。

---

## 5. 构建过程（详细说明）

### 5.1 环境要求

- **Rust 工具链**：stable，MSVC 目标 `x86_64-pc-windows-msvc`。本机当前为 **rustc / cargo 1.99.1**（2026-10-17 重装，见下方提示）。
- **操作系统**：Windows（egui Glow 后端需要 OpenGL 显示环境；字库目录默认指向 Windows 路径）。
- **MSVC 链接器**：需 Visual Studio 生成工具（本机 VS 18 Community + Windows SDK 10.0.26100）。
- **无需 JRE、无需 .NET**。

> ⚠️ **本机 Rust 曾于 2026-10 被整包卸载**（`~/.cargo` 与 `~/.rustup` 全没了，原为 1.96.0），已重新安装为 1.99.1。
> 重装后 **crates.io 缓存也一并丢失**，因此**不能再用 `--offline` 构建**（除非重新预热依赖）；首次构建需联网。

### 5.2 依赖来源

依赖通过 Cargo 管理，关键 crate 及版本：

| 能力 | crate | 版本 |
|---|---|---|
| GUI | `egui` / `eframe` | 0.36 |
| 文件对话框 | `rfd` | 0.17 |
| Excel 导入 | `calamine` | 0.36 |
| 图像 | `image` | 0.25（`png`/`jpeg`/`gif`/`bmp`） |
| 解压（字库内嵌 ZIP） | **自实现**（`font/inflate.rs`） | — |
| 其它 | `anyhow` / `rand` / `serde` / `serde_json` | 1.x |

> **镜像加速（重要）**：官方源在国内下载极慢（实测约 125KB/s，装一个工具链要一小时）。
> 已改用 **USTC 镜像**（实测 24MB/s）：
> - rustup：`RUSTUP_DIST_SERVER=https://mirrors.ustc.edu.cn/rust-static`
> - crates.io：`~/.cargo/config.toml`
>   ```toml
>   [source.crates-io]
>   replace-with = 'ustc'
>
>   [source.ustc]
>   registry = "sparse+https://mirrors.ustc.edu.cn/crates.io-index/"
>   ```

### 5.3 构建步骤

```powershell
# 0) cargo 可能不在 PATH（rustup 装在用户目录时），先加：
$env:PATH = "C:\Users\<用户名>\.cargo\bin;$env:PATH"
#   Git Bash 下：export PATH="/c/Users/<用户名>/.cargo/bin:$PATH"

cd D:\WritingRobot\wrobot

# 1) Debug 构建（开发调试用，编译快）
cargo build --workspace            # 全部 crate
cargo build -p wrobot-gui          # 仅 GUI
cargo build -p wrobot-cli          # 仅命令行

# 2) Release 构建（实际使用 / 上机，带优化，首次约 2~3 分钟）
cargo build --release --workspace
cargo build --release -p wrobot-gui

# 3) 产物位置
#   wrobot\target\debug\wrobot-gui.exe      （约 30MB）
#   wrobot\target\release\wrobot-gui.exe    （约 18MB，推荐使用）
#   wrobot\target\release\wrobot-cli.exe    （约 1.2MB）
#   wrobot\out\wrobot-gui.exe / wrobot-cli.exe   （发布副本，同步自 release）
```

> **上机请用 Release 版**：双击仓库根目录的 `启动写字机器人.bat` 即以 `--auto` 启动 `target\release\wrobot-gui.exe`（若 release 不存在会自动回退到 debug）。

> **⚠️ 沙箱环境提示**：在受限沙箱里执行时，cargo 写 `target/` 下的 `.o` / `.fingerprint` 会被拦截
> （报 `拒绝访问 / os error 5`）。`cargo check` 一般能通过，但 `cargo test` 与 `cargo build --release`
> 需要放开写盘权限。普通用户在本机终端直接构建不受影响。

> 本环境中对 `cargo` 的输出可用 `run.py` 包装（`python run.py cargo build ... out.txt`）以统一捕获 GBK/UTF-8 输出，普通用户直接 `cargo build` 即可。

### 5.4 egui 0.36 关键 API 注意事项（踩坑记录）

egui / eframe 0.36 与旧版差异较大，本项目已修正：

1. **侧边栏**：`egui::SidePanel` 已移除 → 改用 `egui::Panel::left(id)` / `egui::Panel::right(id)`。
2. **App trait**：`eframe::App` 的方法是 `fn ui(&mut self, ui: &mut Ui, frame: &mut Frame)`，不再是 `update(&mut self, ctx, frame)`。
3. **Panel/CentralPanel::show**：闭包接收 `&mut Ui`，不再接收 `&Context`。
4. **描边**：`Painter::rect_stroke` 第 4 个参数新增 `StrokeKind` → 传入 `egui::StrokeKind::Middle`。
5. **只读文本**：`TextEdit::enabled(false)` 已移除 → 改用 `.interactive(false)`。
6. **Panel 无 `default_width`** → 在面板内用 `ui.set_min_width(300.0)`。
7. **字体注册（乱码修复，见第 6 节）**：`FontDefinitions::font_data` 为 `BTreeMap<String, Arc<FontData>>`，`families` 值为 `Vec<String>`（字体名列表），需在首个帧之前通过 `CreationContext::egui_ctx` 注入。
8. **窗口图标**：`ViewportBuilder::with_icon(IconData{ width, height, rgba })`，rgba 为**非预乘** RGBA；本项目用 `include_bytes!` 嵌入 PNG 再用 `image` 解码。
9. **`.open(&mut open)` 会跨闭包借用**：窗口闭包里不能再改 `open`，需改用独立标志位记录用户选择（本项目的「超界确认弹窗」即如此）。

### 5.5 常见错误

- **中文显示为方块 / 乱码**：egui 默认字体不含 CJK 字形，必须注入系统中文 TTF（见第 6 节）。
- **串口打开失败**：不需要 `serialport` crate——本项目的串口由 `wrobot-core::serial` 用 Win32 FFI 实现。报错时看错误码含义（见 7.2.2）：433=设备未连接、2=端口不存在、5=被占用、31=设备半掉线。
- **`.gfont` 解析为空**：确认字库目录路径（默认候选 `D:\WritingRobot\奎享雕刻\gcodeFonts`），缺失时状态栏会提示「未加载到 .gfont 字库」。
- **构建报 `拒绝访问 (os error 5)`**：写 `target/` 被沙箱/权限拦截，见 5.3 提示。
- **`cargo: command not found`**：rustup 装到用户目录而 PATH 未包含，见 5.3 第 0 步。

---

## 6. 界面乱码问题（已修复）

**根因**：egui 默认字体（Ubuntu-Light 等）不含中文字形，所有 UI 中文标签（模块名、参数名、状态栏）渲染为方块（tofu）。

**修复**：在 `wrobot-gui/src/main.rs` 的 `setup_fonts(ctx)` 中，于首个帧之前向 egui 注入系统中文字体，并置于 `Proportional` 字体族首位：

```rust
fn setup_fonts(ctx: &egui::Context) {
    let candidates = [
        r"C:\Windows\Fonts\simhei.ttf",  // 黑体（优先，纯 TTF）
        r"C:\Windows\Fonts\simkai.ttf",  // 楷体
        r"C:\Windows\Fonts\simfang.ttf", // 仿宋
        r"C:\Windows\Fonts\STXIHEI.TTF", // 华文细黑
        r"C:\Windows\Fonts\msyh.ttc",    // 微软雅黑（TTC，失败则跳过）
        r"C:\Windows\Fonts\simsun.ttc",  // 宋体
    ];
    let found = candidates.iter().find(|p|
        std::path::Path::new(p).is_file() && std::fs::read(p).is_ok());
    let Some(path) = found else {
        eprintln!("[字体] 未找到系统中文字体，UI 中文可能显示为方块");
        return;
    };
    let bytes = std::fs::read(path).expect("读取字体文件");
    eprintln!("[字体] 已加载中文字体：{path}");
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "cn".to_owned(),
        std::sync::Arc::new(egui::FontData::from_owned(bytes)),
    );
    // 中文置顶：优先用中文渲染，缺字形再回退到后续字体
    fonts.families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "cn".to_owned());
    ctx.set_fonts(fonts);
}
```

并在 `eframe::run_native` 的创建闭包中调用：

```rust
Box::new(|cc| {
    setup_fonts(&cc.egui_ctx);
    Ok(Box::new(MyApp::new()))
})
```

> 渲染的内容文字（汉字笔迹）由 `.gfont` 单线矢量字库绘制为折线，**本身不受 egui 字体影响**，不会乱码；乱码仅发生在 egui 界面 chrome 上，现已修复。运行 GUI 时若从终端启动，可在 stderr 看到 `[字体] 已加载中文字体：...` 确认生效。

---

## 7. 运行与测试

### 7.1 运行 GUI

```powershell
# 推荐：Release 版（性能优化，上机使用）
D:\WritingRobot\wrobot\target\release\wrobot-gui.exe

# 或双击仓库根目录的「启动写字机器人.bat」（自动连接蓝牙串口）
# 手动指定端口与波特率：
D:\WritingRobot\wrobot\target\release\wrobot-gui.exe --port COM3 --baud 9600
```

界面结构（自上而下、自左而右）：

| 区域 | 内容 |
|---|---|
| **顶部菜单栏** | 文件 / 导出 / 设备 / 运行 / 视图 五个菜单，右侧常驻连接状态与工程名 |
| 左侧 | 模块导航（文字 / 表格 / 格子 / 绘图 / 笔记 / 图片 / 设备 / 测试 / 导入导出 / 运行中）+ 字库状态 + 并入当前页面 + **实时坐标 / 一键回原点** |
| 中央 | 实时预览（**标尺 / 网格**开关在画布上方工具条；图片页可拖拽移动选中的图；运行中页为进度视图）+ 顶部标题条与运行控制条 |
| 右侧 | 当前模块的参数面板（图片页：变换 / **裁剪** / 描边参数 / **绘画方式** / 默认导入参数） |
| **底部实时日志栏** | 设备通信 / G-code 下发回显 / 错误诊断，可折叠、可拖动调高 |

### 7.2 命令行

```powershell
wrobot-cli info   <工程.json>                       # 查看工程信息
wrobot-cli gcode  <工程.json> -o out.nc [--firmware grbl|marlin|ebb] [--fonts <目录>]
wrobot-cli svg    <工程.json> -o out.svg [--fonts <目录>]
wrobot-cli font   <字体.gfont> [字符...]            # 查看字库字形
wrobot-cli config <default.properties>              # 解析固件参数
wrobot-cli png    [-o out.png] [--fonts <目录>]     # 离线生成预览图（与 GUI 同管线）
wrobot-cli dxf    <文件.dxf> [-o out.nc] [--svg out.svg] [--firmware ...] [--target-h 120]
wrobot-cli svgi   <文件.svg> [-o out.nc] [--svg out.svg] [--firmware ...] [--target-h 120]
wrobot-cli ports                                    # 列出本机串口，区分【蓝牙】与【串口线】
wrobot-cli wiretest [COM3] [--baud 115200] [--loop] # 串口线体检：驱动→线材→对端固件逐层测
wrobot-cli probe  [COM3] [--baud 115200]            # 探测串口上的写字机固件（蓝牙/USB-TTL）
wrobot-cli bt     [list|bind|watch] [--name BtWriter] [--rounds N] [--every N]
```

### 7.2.1 连接写字机（串口 / 蓝牙 / USB-TTL）

串口后端在 `crates/wrobot-core/src/serial.rs`，**直接调用 Win32 API，不依赖 `serialport` crate**，
因此离线也能编译。支持蓝牙 SPP 虚拟串口（`蓝牙链接上的标准串行 (COMx)`）与 CH340/CP210x USB-TTL，
固定 8N1、无流控（GRBL/Marlin 默认配置）。

GUI 操作：切到「⑦ 设备」→ 方式选 **串口/蓝牙** → 点 **扫描串口** → 选端口与波特率 → **连接**（或点 **自动连接** 让程序扫描全部 COM × 常见波特率握手固件）。连上后可点 **查询固件**（GRBL 发 `$I`、Marlin 发 `M115`）把回显打进日志；点动 / 回零按钮现走后台线程，**不再卡死 UI**。
设备页还提供**机器物理幅面 长/宽**设置（默认 200×280mm），用于**运行前边界自检**的比对基准。

命令行探测（比 GUI 更快定位问题）：

```powershell
wrobot-cli probe                       # 自动扫描 COM1..COM32 并逐个握手
wrobot-cli probe COM3 --baud 115200    # 指定端口与波特率
```

判定逻辑：端口能被打开只说明串口链路存在；**收到固件启动横幅或 `$I`/`M115` 的响应**才算真正连上，
输出会显示识别为 GRBL / Marlin / EBB。

### 7.2.2 蓝牙连不上的排查（Windows）

> 更常见的是**有线 USB-TTL（CH340）**：插上后设备管理器出现「USB-SERIAL CH340 (COMx)」，
> 直接选这个口即可，比蓝牙稳得多。蓝牙仅在没有 USB 接口时使用。

蓝牙 SPP 的 COM 口是**按需生效**的：配对后设备管理器会出现端口条目，但只有对端在线并完成
RFCOMM 连接后端口才真正可打开。判定依据：

| 现象 | 含义 |
|---|---|
| `SERIALCOMM` 注册表为空、端口状态 `Unknown` | 只有配对记录，**数据链路未建立** |
| `Bluetooth Device (RFCOMM Protocol TDI)` = `Error` | RFCOMM 连接失败 |
| 打开端口报 **433** | 蓝牙已配对但设备不存在（未开机 / 不在范围 / 未发起 SPP） |
| 打开端口报 **2** | 该 COM 口号当前没有对应设备 |
| 打开端口报 **5** | 端口被其它程序占用（先关闭串口助手、厂商软件，**以及本程序的其它实例**） |
| 打开端口报 **121** | 信号灯超时：蓝牙只配对未连接（写字机通电？Windows 里连上 BtWriter 了吗？） |
| **`SetCommState` 报 31**，且 `WriteFile` 写入 0 字节 | 驱动在、句柄能开，但**设备已半掉线**（USB 松/未通电/驱动挂起）：拔插 USB（换个口）→ 设备管理器禁用再启用 → 换蓝牙 |

处理步骤：

0. 先点「设备」页的 **扫描串口**：本版从注册表 `HKLM\HARDWARE\DEVICEMAP\SERIALCOMM`
   取**全部登记端口**（含蓝牙已配对、但打不开的口），并逐个试开标注 **在线 / 离线**；
   再点 **诊断端口**，日志会把每个口的错误码与排查建议逐条打印出来。
1. 写字机通电，蓝牙模块指示灯处于快闪（等待连接）状态；
2. Windows 设置 → 蓝牙和设备 → 连接 **BtWriter**（若反复失败，删除设备后重新配对）；
3. 控制面板 → 设备和打印机 → 蓝牙设备 → 右键**属性** → **服务** → 勾选 **串行端口 (SPP)** → 确定，
   系统会创建真正可用的传出 COM 口；
4. 重跑 `wrobot-cli probe COM3 --baud 115200`。HC-05/06 模块出厂常为 **9600** 或 38400，115200 不通就换波特率再试。
5. 想看失败到底卡在哪一步，用 **逐步诊断**（GUI「诊断端口」按钮同款输出）：

```powershell
wrobot-cli probe COM3
# 连不上时会打印每一步：CreateFile → GetCommState → SetCommState → Timeouts → Purge → WriteFile → ReadFile
```

判定要点：**`CreateFile` 成功 ≠ 链路通**，关键看 `WriteFile` 有没有真的写出字节、`ReadFile` 有没有回显。
本版对 `SetCommState` 失败做**容错降级**（参数已是目标值时跳过该步），不再因为错误码 31 直接拒绝打开。

诊断输出里这几行最有价值：

| 诊断行 | 含义 |
|---|---|
| `6) WriteFile ✗ 写入 0 字节` | 驱动收下请求但没把数据交给设备 —— 写字机没通电 / USB 半掉线 / 驱动故障（错误码 31 常伴随出现） |
| `6.5) 线路：CTS/DSR/RLSD` | CH340 这类 USB-TTL 通常不接握手线，**全低属正常**；蓝牙 SPP 一般会是高电平 |
| `7) ReadFile ✓ 但内容是一堆 `<XX>`` | **波特率不匹配**（收到的是乱码）→ 用「自动波特率」 |

### 7.2.3 「程序在跑，机器却不动」排查（重要）

按下列顺序逐条排查，「测试」页可逐项验证：

| 现象 | 原因 | 处理 |
|---|---|---|
| 进度在涨、日志有行、机器不动 | ~~G-code 行缺少 `\n` 终止符~~（已修复，见第 10 节 #11） | 升级到本版；旧版固件会一直等待行结束符 |
| 一运行就报软限位 / 提示「移动超出限制」 | G-code 里的 `G90` 坐标被当成**机器绝对坐标**解析，机器要跑到 `X210 Y297`（A4）这类机械绝对值，超出实际行程 | 本版 G-code 开头自带 `G92 X0 Y0`，所有坐标相对**机器当前所在点**计算（把笔移到纸面起点再开始）；确认设置里「运行前自动设原点 `auto_set_zero`」是打开的。导出的 `.nc` 也自带这一句。另：**v0.5.0 起运行前会自动做边界自检**，超界会先弹窗询问 |
| **弹窗提示「内容超出机器边界」** | 内容宽/高超过设备页设置的机器幅面，或绝对坐标模式下落在工作区外 | 确认「设置 → 机器幅面」与实际机器一致；确认纸张与原点位置；确认无误再点「仍然执行」 |
| 发任何指令都没反应、`$X` 也无回显 | **GRBL 处于 Alarm 锁定态**（上电/急停/硬限位触发），此时所有 `G0/G1` 被忽略 | 「测试」页点 **解锁 $X**，日志出现 `ok` 后再动；本版默认「运行前自动解锁」（可在设置里关） |
| 只有移动很慢 / 迟迟不结束 | 每行都在等固件回 `ok`，但固件不回包 | 设置 → 运行流控：关闭「等待固件回 ok」，或把超时调小 |
| 画出来是镜像 / 旋转 90° | 坐标系方向不对 | 「测试」页画一个方框验证，再到设置里勾 反转X / 反转Y / 对调XY |
| 画出来大小不对 | 单位或步进标定问题 | 确认 G-code 首行有 `G21`（毫米）；调整 `mm_to_steps` / 进给 |
| 笔不落 / 只走空行程 | 抬落笔 Z 值或舵机值不对 | 「测试」页用 抬笔/落笔 按钮单独验证 Z 或舵机 |
| 日志里全是 `<F8>` `<FF×4>` 这类十六进制 | **波特率不匹配**（读到的不是文本，是垃圾字节） | 点设备页 **「自动波特率」**，让它逐个波特率握手并自动挑对的那个 |
| 提示 `写入 0 字节` / `device error 0/N` | 驱动收下请求却没把数据交给设备（USB 半掉线 / 流控未就绪 / 未通电） | 看错误提示里附带的**线路状态**；拔插 USB 或换个口，CH340 建议更新官方驱动。程序会自动重试 3 次并尝试重连重发 |
| 连得上、指令也发了，Marlin 板一动不动 | Marlin 上电后步进电机**失能**，`G0/G1` 被忽略 | 本版运行前已自动补发 `M17`（使能步进电机）；手动验证可在「测试」页发一次 `M17` |
| 完全没反应、日志空白 | 根本没连上（蓝牙只是配对，SPP 链路未建立） | 见 7.2.2，先用 `wrobot-cli probe` 确认 |

**推荐的首次上机流程**：

1. 「设备」页连接（串口/蓝牙），状态显示 ● 已连接；
2. 切到「**测试**」页 → 点 **解锁 $X**（日志要看到 `ok`）；
3. 点 **移到 (0,0)** 或 **回原点**，确认机器能走；
4. 用 **XY 点动** 确认 X/Y 方向正确（方向反了到设置里勾反转）；
5. 点 **画测试方框**（默认 100×100mm），验证比例与笔控；
6. 回到编辑页，点顶部 **开始绘制**（如超界会先弹窗确认）。

### 7.2.4 先分清「蓝牙」还是「串口线」，再测这根线

蓝牙和 USB-TTL 串口线的失败原因**完全不是一回事**，程序现在从端口枚举阶段就把它们分开：

```powershell
wrobot-cli ports
```

输出会按类型标注，例如：

```
端口     类型     状态     设备
COM3     串口线   在线     USB-SERIAL CH340 (COM3)  [\Device\USBSER000]
COM4     蓝牙     打不开   蓝牙链接上的标准串行 (COM4)  [\Device\BthModem0]
```

判定依据（三级，依次降级）：SetupAPI 的**枚举器** `BTHENUM`/`USB`/`FTDIBUS`/`ACPI`
→ `SERIALCOMM` 的**设备对象名** → 设备**友好名**关键字。

| 类型 | 端口存在意味着 | 打不开时优先怀疑 |
|---|---|---|
| **串口线**（CH340/CP210x/FTDI） | 驱动已加载、线已插上 | 线只能充电不传数据 / USB 口松 / 驱动故障态（错误码 31） |
| **蓝牙**（BtWriter） | 只代表**配对过**，不代表链路已建立 | 机器没开机 / 只配对未连接 / 没有【传出】COM 口（错误码 433、121） |

测一根**有线串口线**通不通（不指定端口时自动挑第一个「串口线」类型的口）：

```powershell
wrobot-cli wiretest COM3            # 默认 115200
wrobot-cli wiretest COM3 --baud 9600
wrobot-cli wiretest COM3 --loop     # 回环自检：需先把该口 TX/RX 短接
```

体检会依次输出：类型与设备名 → 打开端口 → 线路状态（CTS/DSR/收发队列）→ **写测试**
→ 静默监听开机欢迎语 → 握手 `\n` `$I` `M115` `$X` → 波特率扫描 → 结论。典型结论：

- **端口都打不开** → 先解决驱动/链路（错误码 2=没这个口，5=被占用，31=设备半掉线）；
- **能开但写入 0 字节** → 驱动故障态或 USB 半掉线：拔插换口，仍如此换线/换适配器；
- **能写出去但零回应** → 机器没通电 / TX-RX 接反（对端 TXD 接本机 RXD）/ GND 未共地 / 波特率不对；
- **回环自检收到自己发的字节** → 线材与端口收发都正常，问题在机器端。

GUI 同样可用：设备页「**体检串口线**」按钮（体检会独占端口，开始前自动断开，
结果逐行写进底部实时日志）。

### 7.2.5 蓝牙「几分钟就断」怎么办

根因通常有两层，程序和系统各解决一半：

| 层 | 原因 | 解决办法 |
|---|---|---|
| **写字机端** | 蓝牙模块空闲断链：几十秒~几分钟没有数据流就自己断开 | 软件已内置**保活心跳**（默认 5 秒一个空行），设置 → 链路保活 |
| **Windows 端** | USB 选择性暂停 / 无线适配器省电把蓝牙适配器"睡掉" | 管理员运行根目录 **`优化蓝牙稳定性.bat`**，并在设备管理器蓝牙适配器的「电源管理」里取消"允许计算机关闭此设备" |
| **环境** | 2.4G WiFi 与蓝牙同频段互相干扰（WiFi+蓝牙二合一网卡尤其明显） | 测试时临时关 WiFi，或改用 5G 频段 |
| **链路本身** | 只配对未连接：COM 口存在 ≠ RFCOMM 数据通道已建立 | `wrobot-cli wiretest COMx`：能打开但**写 0 字节**就是这个状态，按输出的 ①~⑤ 处理 |

程序侧还做了：心跳发不出去即判定断链并**自动重连**（累计次数写进状态），
自动连接时**能打开的端口优先**试，写入失败的口立即跳过——不再对着打不开的口空等。

### 7.2.6 蓝牙「设置里看得到设备，但就是连不上」

这是**最常见**、也最容易误判的一类。关键要分清三件事，它们互不等价：

| 现象 | 含义 |
|---|---|
| 设置里能搜到 BtWriter | 只说明**模块在广播**：写字机通电正常 |
| 蓝牙列表里显示"已配对" | 只说明**配对密钥存下来了** |
| 真的有数据通道 | 要看底层是否 **connected**，以及 COM 口能不能**写进去** |

程序直接问 Windows 蓝牙栈（`bthprops.dll`），不再靠猜：

```powershell
wrobot-cli bt                                   # 蓝牙体检：适配器 / 设备状态 / COM 口绑没绑对
wrobot-cli bt list                              # 列出周边蓝牙设备 + 蓝牙 COM 口的绑定关系
wrobot-cli bt bind                              # 尝试自动启用 SPP 串口服务（建传出 COM 口）
wrobot-cli bt watch --rounds 6 --every 5        # 30 秒可见性观察，看是不是间歇性掉线
```

GUI 设备页 → **「蓝牙链路」**区块同样有这三个按钮，报告直接显示在面板里。

两个高频陷阱，程序会自动标出来：

1. **空口**：蓝牙 COM 口的设备实例 ID 里 MAC 是全零（`...&000000000000_`），
   它不绑定任何设备——能打开，但 `WriteFile` 写 **0 字节**。看到"未绑定设备"就别选它。
2. **全程可见却一次都没连上**（`bt watch` 每次都是"仅可见"）：
   这不是供电问题也不是信号问题，而是**连接这一步骤失败**。按顺序查：
   ① 它是不是已经被手机/平板连着（这类模块同时只接受一个连接）；
   ② **删掉 BtWriter 的配对重新配一次**（旧密钥对不上就会反复超时，是最常见的原因）；
   ③ 重建【传出】方向的蓝牙串口端口：更多蓝牙选项 → COM 端口 → 添加 → 传出 → 选它。

### 7.3 测试

```powershell
cargo test --workspace
```

- **wrobot-core**：**57 个单元测试**（含 PDF 结构、DXF 解析、SVG 解析、串口诊断/容错、乱码 sanitize 与 GBK 解码、坐标变换/反转一致性、矢量归一化、G-code 原点锚定、笔画路径规划/最优排序、**G-code 坐标边界解析**）+ 12 个集成测试 + 1 个 doctest；
- **wrobot-gui**：**51 个集成测试**（用真实 `.gfont` 覆盖十大模块、边框、手绘、PDF、DXF/SVG 矢量导入、激光、自动休眠/延时、原点角、点动、图片描边与变换数学、模拟后端回显、重连释放旧连接、自动波特率评分、坐标回显解析、标尺/网格布局、**运行前边界自检、图片裁剪、扫描方向、连笔容差、分块蛇形**，以及历次 BUG 回归）；
- **wrobot-cli**：**3 个集成测试**（DXF→G-code、DXF→SVG、SVG→G-code 端到端驱动真实二进制）；
- **合计 124 个用例全部通过、0 失败**，全 workspace **零警告编译**。

> ⚠️ 沙箱环境偶发：`cargo test` 的 **doctest 有时无法编译**，报
> `Failed to spawn rustc.exe: Os 231（所有的管道范例都在使用中）`——Windows 命名管道耗尽，
> 与代码无关（有时同一份代码重跑就通过）。该 doctest 唯一断言是 `gcode.contains("G1")`，
> 已由通过的 `pipeline_with_real_fonts`（同样断言 `contains("G1")`）与
> `min_step_shortens_gcode`（过滤 `G1 X` 行并断言 >0）覆盖证明。

---

## 8. 与源程序（Kvenjoy）功能对比

下表基于原版安装包证据：`KXSettings.zip`（含 `word/grid/line/note/edge/squiggle/dxf/svg/thinning` 等数据文件）、`default.properties`、`config.properties`、二进制 `potrace-1.15.win32`/`ch340.EXE`、`录字体.docx`、以及逆向分析文档。

| 源程序功能 | 证据 | 复刻状态 | 说明 |
|---|---|---|---|
| 文字（word） | `WordData.json` | ✅ 已实现 | 对齐/行距/字间距/字体栈 |
| 笔记（note） | `note.json` | ✅ 已实现 | 自由文本块 |
| 格子（grid） | `grid.json` | ✅ 已实现 | 空白/田字/米字/回宫 + 示范字 |
| 线条/绘图（line） | `line.json` | ✅ 已实现（基础） | 矩形/网格/交叉格 |
| 表格（table） | 图文教程 + POI 依赖 | ✅ 已实现 | 边框 + 每格文字 + Excel 导入 |
| 边框（edge） | `edge.json` | ✅ 已实现 | 实线/虚线/双线 + 页边距 |
| 手绘（squiggle） | `squiggle.json` | ✅ 已实现 | 画布拖动生成笔迹（带抽稀） |
| 图片转矢量（thinning+Potrace） | `thinning.json` + `potrace` | ⚠️ 简化 | 现为**线框（Sobel）+ 影线 + 高效连续笔画**（扫描线模型，可调方向/顺序/连笔容差）；原版为 Potrace 自动描线 |
| 图片导入选项（线框/素描 + 提取参数） | `thinning.json` | ✅ 已实现（**超出**） | 绘制类型 线框/素描/高效，暗部阈值/边缘阈值/反相/影线间隔，**外加扫描方向/笔画顺序/连笔容差/分块边长/裁剪** |
| 图片裁剪 | — | ✅ 已实现（新增） | 按原图比例裁四边后再描线 |
| 背景参考图（浅色、不打印） | 源程序多图层背景 | ✅ 已实现 | 导入背景参考图浅灰显示，不参与 G-code |
| 多图重叠 | 源程序图层 | ✅ 已实现 | 多张前景/背景图可重叠，各自独立变换 |
| 工程中完整存取（含图片/文本块/设备设置） | 模板 `.json` | ✅ 已实现 | `.wprj` 一键保存/载入（图片字节内嵌） |
| 运行中实时进度页 | 源程序运行态 | ✅ 已实现 | 实时 G-code 行进度、已用/剩余时间、整页黑/灰进度图，与编辑页互不干扰 |
| DXF/CAD 导入 | `dxf.json` + kabeja | ✅ 已实现 | 纯 Rust 解析 DXF R12+ ASCII（LINE/LWPOLYLINE/POLYLINE+CIRCLE/ARC/ELLIPSE/SPLINE/INSERT+BLOCK 变换） |
| SVG 导入/导出 | `svg.json` | ✅ 已实现 | 纯 Rust 解析 SVG（path 全指令 / 基本图元 / `<g>` 嵌套变换 / 贝塞尔 / 椭圆弧），与导出双向打通 |
| 字体管理（导入/扫描） | `FontsDialogData.json` | ✅ 已实现 | 导入 `.gfont` + 重新扫描目录 |
| 手机录字（字体录制） | `录字体.docx` | ❌ 缺失 | 需外部 App + 录制格式 |
| 逼真化（随机偏移/变形） | `WordContentFormat.json` | ✅ 已实现 | 随机开关 + 重播种换笔迹（可按文本框生效） |
| 设备串口（CH340/蓝牙 SPP） | `ch340.EXE` + jssc | ✅ 已实现 | `wrobot-core::serial`，纯 Win32 FFI；**后台线程，点动/回零/运行不卡 UI** |
| 抬/落笔、回零 | Grbl.json | ✅ 已实现 | 含落/抬笔、延时、回原点 |
| 机器物理幅面（长/宽）设置 | 源程序设备设置 | ✅ 已实现 | 设备页可调机器长宽，**并用于运行前边界自检** |
| 运行前边界自检 / 超界询问 | — | ✅ 已实现（新增） | 解析 G-code 坐标范围 vs 机器幅面，超界弹窗「仍然执行 / 取消」 |
| 画布标尺与可调网格 | — | ✅ 已实现（新增） | 页面外侧毫米标尺 + 可调行列数虚线网格（不打印） |
| 自动休眠（autoSleep） | `default.properties` | ✅ 已实现 | GRBL `$SLP` / Marlin `M84` |
| 点动（jog）/ 单步（single_step） | `default.properties` | ✅ 已实现 | 四向点动按钮 + 逐行下发（后台线程，不卡死） |
| 机器测试（回零 / 设零点 / 画测试方框） | 源程序调试功能 | ✅ 已实现 | 「测试」模块：解锁、回原点、`G92` 设零点、读位置、抬落笔、测试方框 |
| 主界面「开始绘制」按钮 + 暂停/停止 | 源程序主面板 | ✅ 已实现 | 顶部常驻运行控制条 |
| 设置对话框（不常用设置收纳） | 源程序设置面板 | ✅ 已实现 | 激光/休眠/延时/坐标系/自定义 G-code/逼真化/流控 收纳其中 |
| 激光模式（laser_power） | `default.properties` | ✅ 已实现 | `M3 S<power>` / `M5` |
| 自定义起始/结束 G-code | `custom_start/end_gcode` | ✅ 已实现 | `GcodeOptions` 支持 |
| 坐标系反转 / 原点角（axe_pos）/ 偏移 | `default.properties` | ✅ 已实现 | 反转、对调、偏移、四角原点（箭头与点动同源，不会不一致） |
| 云平台字体/激活（OSS/licene） | `config.properties` + `KEY1.txt` | ❌ 缺失 | 超出核心绘制范围 |
| PDF 导出 | `pdfbox-app` | ✅ 已实现 | 纯 Rust 单页矢量 PDF |
| PDF 导入（指定页） | `pdfbox-app` | ✅ 已实现 | 纯 Rust 解析：矢量描边 / 嵌入图像描边 / **文字用手写体重排**，可选页、默认 1:1 |
| XY 最小步进（G-code 抽稀） | 源程序输出密度 | ✅ 已实现 | 0.1~1.0mm，默认 0.2，行数降一个数量级 |
| 纸张规格（A4/B5/16K…+自定义+横竖） | 源程序页面设置 | ✅ 已实现 | 10 种常规规格一键切换，默认 A4 210×297 |
| 实时坐标显示（机器物理 / 工件 / 页面→机器） | 源程序坐标回显 | ✅ 已实现 | 解析 `MPos`/`WPos`/`WCO` 与 `M114`，左栏常驻，可自动轮询；附**一键回原点** |
| 模板保存（.json） | 工程存档 | ✅ 已实现 | `save_project` |
| 多页自动分页 | 笔记/格子教程 | ⚠️ 部分 | 单页为主 |

**结论**：核心绘制链路（十大模块 → 笔迹 → G-code/SVG/PDF）已完整复刻并通过测试；在源程序能力之外还补齐了**运行前边界自检、图片裁剪、绘画方式细粒度参数、画布标尺与网格、一键回原点、应用图标**等实用功能。主要剩余缺口集中在**图片自动描线（Potrace 轮廓细化）、多页分页、云/激活与手机录字**。

---

## 9. 已知限制与后续工作

1. ~~**设备串口**~~ ✅ 已实现：`wrobot-core::serial` 用纯 Win32 API 实现，离线可编译，GUI 中「串口/蓝牙」为默认连接方式。
2. **图片转路径**：当前为线框（Sobel）+ 影线 + 连续笔画，质量低于原版 Potrace 自动描线；后续接入 `potrace` 或自研轮廓细化算法。
3. ✅ **DXF / SVG 导入**：已实现（纯 Rust，零外部依赖）。
4. **多页自动分页**：笔记/格子目前以单页为主。
5. **逼真化参数**：可继续补全 `lineGapOffset`、`autoGp` 字体轮换等更细的随机项。
6. **真机标定**：`em=500`、`char_mm_at_100=8.0` 为合理默认，需上机微调字号/落笔量使笔画尺寸与原版一致。
7. **云字体 / 激活 / 手机录字**：依赖外部服务与录制 App，超出核心绘制范围。
8. **边界自检的粒度**：目前按「整份 G-code 的坐标范围 vs 机器幅面」判断，不感知机器**当前实际位置**（G92 之后的可用余量）。若笔停在偏离机械原点较远处，仍可能撞限位——后续可读取 `WPos` 参与判断。
9. **图标仅作用于窗口/任务栏**：未做 Windows 资源段嵌入（需要 `winres` 之类的构建期依赖），资源管理器里看 exe 仍是默认图标；如需要可后续补 `build.rs`。

---

## 10. 代码审查修复记录

本轮审查发现并修复的 BUG（均已补测试）：

> 说明：本表在本版整理时把编号**规范化为 1~59 连续编号**（旧版存在缺号与乱序，如 32 缺失、35 排在 33/34 之前）。
> 条目内容保持原样，仅编号调整；若对照更早版本的 README，请以条目标题为准。

| # | BUG | 影响 | 修复 |
|---|---|---|---|
| 1 | **G-code 整数被截尾零**：`num(10000, 0)` → `"1"` | **严重**：实际下发 `F1` 而非 `F10000`，机器以极慢速度运行；激光功率 `800` 变 `S8` | `d==0` 时不再去尾零 |
| 2 | 表格单元格切分 `if/else` 两分支都按 `\t` 切 | 逗号/空格分隔的表格无法识别 | 按 tab → 逗号 → 空格 优先级切分 |
| 3 | `rebuild_preview_random` 用固定种子 | 注释称"每次不同"，实则永不变化 | 引入可重播种的 `rand_seed`，重播种后才变化（导出仍可复现） |
| 4 | `compose_doc` 把五大模块全部合成一页 | 各模块默认坐标互相重叠，预览/导出混乱 | 只并入当前模块；提供"并入当前页面"勾选以刻意叠加 |
| 5 | `generate_gcode` / `rebuild_preview_random` 大段重复 | 维护风险 | 抽出统一的 `collect_polylines()` |
| 6 | 预览 `allocate_space(Vec2::ZERO)` | 预览不占布局空间，后续控件会重叠 | 改为按实际区域分配，并取得交互响应 |
| 7 | 表格/格子"自适应字号"分支不可达 | `font_size>0` 恒真，自适应永不生效 | 改为独立"自适应"勾选开关 |
| 8 | `wrobot-cli` 变量 `X`/`Y` 命名告警 | 编译告警 | 改为小写 |
| 9 | **设备点动/回零卡死**：串口 I/O 在 egui 主线程同步执行，UI 直接被阻塞 | **严重**：点「电动移动/归零」后程序页卡死无响应 | 改写为**后台线程架构**：UI 只经 `mpsc` 投命令、读 `Arc<Mutex<DevShared>>`；`device.rs` 的 `thread_main` 阻塞 `recv` + 运行态逐行流控，点动/回零/运行全部非阻塞 |
| 10 | **表格/格子文字跑到格子外**：`render()` 把字形 em 方框**中心**当放置点，而布局期望**左上角** | 表格每格文字溢出到格外、格子示范字不居中 | `render()` 加入 `half` 偏移使放置点成为左上角 + `layout.rs` 垂直居中；表格格锚到左上角、对齐 Center、`vcenter=true`；格子同理。新增回归测试 `table_text_stays_in_cell` / `grid_sample_centered_in_cell` 通过 |
| 11 | **G-code 下发缺少行终止符**：`StartRun` 用 `gcode.lines()` 剥离了 `\n`，`send_line` 直接 `t.send(line)` 发出不带换行的行 | **严重**：GRBL/Marlin 靠 `\n` 判定一行结束，收不到就一直缓冲不执行 → **程序在跑、进度在涨、机器一动不动**；点动 / 回零 / 单步下发同样全部无效 | `send_line` 统一发 `line + "\n"`；`send_gcode_block` 在结尾补 `\n`；日志增加 `→ 行` 回显便于确认 |
| 12 | 未处理 **GRBL Alarm 锁定态**：上电/急停后所有运动指令被固件忽略 | 机器不动且无明显报错 | 新增 `send_unlock()`：运行前自动发 `$X`、`G21`、`G90`；「测试」页提供独立「解锁 $X」按钮；设置里可开关 |
| 13 | 等 `ok` 超时过长（6×500ms）且收到 `error/alarm` 仍继续空等 | 固件不回包时整段运行极慢 | 改为默认 1.2s 超时、收到 `ok`/`error`/`alarm` 立即返回；新增「等待 ok / 超时」流控设置 |
| 14 | **串口枚举只靠"逐个试开"**：`list_serial_ports()` 遍历 COM1..COM32 试开，蓝牙已配对但未建立 SPP 链路的口打不开 → 不在列表里 | **连不到设备**：设备页下拉里看不到蓝牙 COM 口，用户只能瞎猜端口号 | 新增 `registered_ports()`：直接读注册表 `HKLM\HARDWARE\DEVICEMAP\SERIALCOMM` 取**全部登记端口**（含离线口）；`scan_ports_detailed()` 逐个试开并标注 **在线/离线**；设备页下拉与状态列表都显示在线状态；新增「诊断端口」按钮逐口打印错误码与建议；`err_hint` 补 121（信号灯超时＝只配对未连接） |
| 15 | 串口读超时设为 `read_total_const=1000`，`ReadFile` 无数据也会阻塞满 1 秒；`read_line` 遇到 0 字节直接 `break` | 扫描「多端口 × 多波特率」时慢得像卡死 | 改为 `read_interval=MAXDWORD, total=0` 的**非阻塞读** + `read_line` 自己轮询到 deadline；写超时降到 1s |
| 16 | 打开串口后 `PurgeComm(PURGE_ALL)` 清空接收缓冲，丢掉开机欢迎语 `Grbl 1.1h ['$' for help]` | 自动连接识别不到固件 | 改为只清发送缓冲（`PURGE_TX`），保留欢迎语；`probe_port` 依次发 `\n`、`$I`、`M115` 并读取，按两轮波特率（先 115200/9600，再全量）扫描 |
| 17 | **非内容页把五大模块全部并入页面**：`module_active()` 在 `tab >= MODULE_COUNT` 时无条件返回 `true` | 设备 / 测试 / 导入导出页上，文字+表格+格子+手绘+笔记 五个模块的默认坐标互相压在一起**全部重叠**，且左侧「并入当前页面」勾选框取消也无效 | 新增 `last_content_tab`（记住最近停留的内容模块）：非内容页只并入该模块 + 显式勾选的模块；左侧面板在非内容页提示"只显示「X」"（该机制后在 #59 被进一步收紧） |
| 18 | 设备页缺少一键清除；窄面板里两个下拉 / 两个滑块横向并排 | 控件互相挤压重叠；无法清空页面内容 | 设备页下拉与机器幅面滑块改**竖排各占一行**；新增「清空页面内容」按钮（二次确认防误删）；顶栏按钮按当前页显示「清空本页 / 清空内容」，非内容页走 `clear_all_content()` 且需二次确认 |
| 19 | **格子里文字中心与格子中心对不齐**：各字库字形原点不统一——`.gfont` 原点在**基线**上（实测 CJK 墨迹 y ∈ [-0.8em, +0.3em]），内置字体原点在 em 框**左上角**；旧代码一律把原点当字框左上角 | **严重**：示范字整体偏上约 0.45 倍字高、并溢出到格线上方，怎么看都不居中 | 新增字体「典型墨迹框」`InkMetrics`（加载字库时**按 CJK 各字形墨迹框四边取中位数**，抗个别超框字形干扰），渲染层据此把墨迹中心对齐到字框中心；格子/单元格再打开 `PageInfo::ink_center` 做**逐字墨迹居中**。实测单字偏差由 3.6mm 降到 **0.00mm**。回归测试 `grid_sample_centered_in_cell` 收紧到 ±0.6mm |
| 20 | **格子缺「线条宽度 / 显示横行 / 显示竖行 / 显示隐藏」** | 只能整片画框，无法只练横排或竖排，也不能调线宽 | 新增 `grid_show`（总开关）/ `grid_show_h` / `grid_show_v` / `grid_line_width`；行列线改为**整行/整列一条线**（消除相邻格共享边被描两遍）；线宽 >0.35mm 时用 `thicken_lines()` 生成多条并行描边实现加粗；表格同步补齐。新增测试 `grid_rows_cols_toggle` / `grid_line_width_thickens` / `grid_show_toggle_outputs_nothing` |
| 21 | **图片页缺「清除已导入图片 / 缩放 / 移动位置」** | 只能靠画布拖一个图，缩放与位移无可感控件，也无法快速清图 | 面板补齐：缩放滑块 + ±按钮 + 适应页面 / 原始尺寸（**围绕中心缩放**，位置不漂）；X/Y 位置滑块 + 方向键微调（Shift=10mm）+ 页面居中；旋转 / 镜像 / 重置变换；每张图独立 ✕ 删除 + **清除全部图片（二次确认）**。画布补：**点击选中**（取最上层，含旋转/镜像命中）、**滚轮缩放**；选中框改按旋转后四角绘制。变换数学抽到 `ImageObject::to_page/from_page/hit`，渲染与命中测试共用一套，新增测试 `image_transform_scale_and_hit` |
| 22 | **错误码 31 导致永远连不上**：`open_raw` 无条件调用 `SetCommState`，一旦失败（设备半掉线 / 蓝牙虚拟串口 / 部分 CH340）就整端口拒绝打开 | **严重**：明明端口能开、参数也已经是 115200 8N1，却在打开阶段被判失败，用户只能看到一句「错误码 31」无从下手 | ① 打开前先比对当前 DCB，**参数已是目标值时跳过 `SetCommState`**；② 仍失败则**降级继续**（`SetCommState`/`SetCommTimeouts`/`PurgeComm` 都只记 warning 不致命），并把「请求/实际波特率」回读写进日志；③ `CreateFile` 失败按 3 次 ×350ms 重试（USB/蓝牙枚举抖动）；④ `GetCommState` 失败时改用内置 8N1 DCB 兜底；⑤ 新增 `diag_port_steps()` 逐步诊断（CreateFile→GetCommState→SetCommState→Timeouts→Purge→WriteFile→ReadFile），GUI「诊断端口」与 `wrobot-cli probe` 共用；⑥ `err_hint` 补 31/6/22/23/87 的可执行建议；⑦ `WriteFile` 写入 0 字节判定为失败并提示设备掉线。新增测试 `diag_steps_never_panics` / `open_failure_has_hint` |
| 23 | **启动窗口过小、内容显示不全**：`NativeOptions` 用 `..Default::default()`，`ViewportBuilder` 的 `inner_size` 为 `None`，且 `persist_window` 默认为 `true` 会记住旧的（小）尺寸 | 三栏布局被挤成一团，画布与参数栏都放不下 | 显式 `with_inner_size([1600,1000])` + `with_min_inner_size([1180,720])` + `with_maximized(true)`；`persist_window=false` 避免沿用历史小尺寸；启动时自动扫一次串口（设备页一打开就有端口状态）；设置对话框新增**界面缩放**滑块（0.7×–2.0×，在显示器原生缩放之上叠加，变化时才下发 `set_pixels_per_point`），高分屏/小屏都能自己调 |
| 24 | **模拟（Mock）后端"不好使"**：`send_gcode_block` 只写一句「已下发 N 行 G-code」，**完全不回显发了什么**；Mock 的 `send` 也不做行数统计、无任何延迟 | 选了模拟模式却什么也看不到，无法确认指令是否正确；「运行中」进度一闪而过 | ① `send_gcode_block` 改为**逐行回显**要下发的 G-code（最多 40 行，超出标注省略）+ 「本次共下发 N 行」；② `MockTransport` 统计已下发行数、像真机一样逐行回 `ok`、每行 12ms 延迟让进度可见；③ 连接时日志明确标出「模拟模式已启动（不下发真实指令）」，`notes()` 回报已下发行数；④ 菜单切换连接方式时给出下一步提示。新增测试 `mock_backend_echoes_gcode` |
| 25 | **重复连接报「被占用」，其实是被自己占用**：`DevCmd::Connect` 直接新建 transport 去开同一端口，而**旧 transport 仍握着句柄**（只有替换时才释放），于是第二次连接必报「错误码 5 被占用」 | 用户再点一次「连接」就失败，且提示指向"其它程序占用"，实际是本程序自己 | ① `Connect` 先断开并释放上一个 transport 再连（日志提示「已释放上一个连接」）；② `AutoConnect` 探测前同样先释放；③ `DevShared::cur_addr` 记录当前地址，「诊断端口」跳过已连接的口并说明「重复打开会误报错误码 5」；④ `scan_ports()` 把本程序已连的口直接标为在线；⑤ `err_hint(5)` 补上"先点断开再连"。新增测试 `reconnect_releases_previous_transport` |
| 26 | **常用功能没有统一入口、日志藏在设备页里**：保存 / 导出 / 连接 / 运行分散在右侧各个页面，设备日志只在「设备」页底部能看到 | 换个页面就看不到机器回显，排查问题时来回切页；常用操作要翻面板 | ① 新增**顶部菜单栏**：文件（新建 / 打开 / 保存 / 另存为 / 导入字库，`Ctrl+S` `Ctrl+O`）/ 导出（.nc / SVG / PDF / PNG，`Ctrl+E`）/ 设备（连接 / 断开 / 自动连接 / 扫描 / 诊断 / 连接方式）/ 运行（开始 / 暂停 / 继续 / 停止 / 回原点 / 解锁 / 回零）/ 视图（日志栏 / 界面缩放 / 设置），右侧常驻连接状态与工程名；② 新增**底部实时日志栏**（窗口最下面一栏，任何页面可见，可折叠、可拖动调高，只渲染尾部 6000 字符、`stick_to_bottom` 自动滚到底，运行中显示 `已行/总行`）；③ 记住工程路径 `project_path`，`Ctrl+S` 直接覆盖保存不再弹对话框 |
| 27 | **实时日志全是乱码**：串口读到的字节直接 `String::from_utf8_lossy` 转成字符串，非 UTF-8 字节全变成 ` `（U+FFFD）；波特率不匹配时读到的全是垃圾字节 | 日志一片黑菱形问号，除了让人迷惑什么也看不出来，更无法据此判断「波特率不对」 | 新增 `sanitize_bytes()`：可打印 ASCII 与合法 UTF-8（中文）原样保留，其余字节显示为 `<F8>` 这样的十六进制，连续 4 个以上相同字节折叠为 `<FF×4>` 防刷屏；`garbage_ratio()` 统计不可读占比（含 ` ` 与 `<XX>` 标记），`read_line` 一律走 sanitize。附带收益：日志里出现大量 `<XX>` 就是**波特率不匹配的直接证据**。新增测试 `sanitize_turns_garbage_into_hex` / `sanitize_keeps_readable_text` / `garbage_ratio_detects_mismatch` |
| 28 | **`WriteFile` 返回成功却写入 0 字节（"device error 0/31"）**：DCB 的**流控位与 DTR/RTS 模式没有纳入"参数是否已是目标值"的判断**——驱动默认常把 `fDtrControl`/`fRtsControl` 设成硬件握手或 `fOutxCtsFlow=1`，此时代码认为"参数一致"直接跳过 `SetCommState`，对端不拉高 CTS/DSR 时 `WriteFile` 就会一直等握手、超时后写 0 字节 | 表现最隐蔽的一类故障："连上了、也发了、进度在涨"，但机器一动不动，且只给一句 `写入失败（已写 0/31 字节）`，无从下手 | ① 新增 `desired_flags()`：强制关闭硬件/软件流控，并把 DTR/RTS 固定为**使能（高电平）**；`already` 判断**必须**包含这些位，不一致就一定重新 `SetCommState`；② 打开后无条件 `EscapeCommFunction(SETDTR/SETRTS)` 拉高控制线 + `ClearCommError` 清旧错误；③ `write_raw` 改为**最多 3 轮重试**（每轮清错误、重拉 DTR/RTS、续写剩余字节），彻底写不进去时把线路状态（CTS/DSR/收发队列/错误标志）一并写进错误提示；④ 诊断新增「流控：…」与「6.5) 线路状态」两行，并说明 CH340 不接握手线时全低属正常 |
| 29 | **连上了但机器一声不吭，波特率靠猜**：出厂波特率可能是 9600 / 38400 / 115200 中的任意一个，程序只用界面选的那个值去握手 | 波特率不对时机器完全无响应，日志还刷一堆乱码，用户只能手动一个个试 | 新增 **「自动波特率」**（设备页与「设备」菜单）：逐个波特率打开端口 → 读开机欢迎语 → 发 `\n` `?` `$I` `M115` 唤醒 → 用 `score_response()` 评分（认出固件名 +100、可读度高加分、乱码率 >50% 扣 40），自动挑出最佳波特率并重连，全程把每个波特率的评分与回显写进日志；`DevShared::cur_baud` 回报实际波特率。新增测试 `score_prefers_real_firmware_over_garbage` |
| 30 | **Marlin 固件上电后步进电机失能，G0/G1 被直接忽略**：运行前只发 `$X` / `G21` / `G90` | Marlin 板子"程序跑完机器一动不动" | `send_unlock()` 增加 `M17 ; 使能步进电机(Marlin)`（GRBL 只会回一个 error，不影响后续）。另：`send_line` 发送失败时若地址是 COM 口，会**自动断开重连一次并重发**（USB 半掉线/蓝牙闪断的自愈），日志写明「已重连，指令已补发」；连接成功后立即读开机欢迎语（GRBL 会主动打 `Grbl 1.1h ['$' for help]`），并检测乱码率给出「波特率不匹配」提示。新增测试 `send_without_connection_logs_instead_of_panicking` |
| 31 | **蓝牙与串口线混在同一个端口列表里，分不清哪根是哪根**：`registered_ports()` 只取 COM 名，蓝牙 SPP 与 USB-TTL 看起来完全一样；而"蓝牙只配对未连接"和"串口线没插好"的现象相同（端口都打不开），排查步骤却完全不是一回事 | 用户只能对着一串 COM 口号逐个试；用蓝牙的流程去修串口线（或反过来），越修越乱 | 新增 `PortKind`（蓝牙 / 串口线 / 主板串口 / 未识别）与 `PortEntry` / `enumerate_ports()`：**用 SetupAPI 枚举「端口(COM 和 LPT)」类**取设备友好名与**枚举器**（`BTHENUM`→蓝牙、`USB`/`FTDIBUS`→串口线、`ACPI`→主板），再用注册表 `SERIALCOMM` 的**设备对象名**（`\Device\BthModem0` vs `\Device\USBSER000`）兜底；`wired_ports()` / `bluetooth_ports()` 分组，每种类型各给一套 `tips()`。GUI：端口下拉与列表显示 `COM3 [串口线] USB-SERIAL CH340 (COM3)`，扫描日志按「串口线 N 个、蓝牙 N 个」汇报，蓝牙离线口单独提示"只配对未连接"，没有串口线时明确告警；CLI 新增 `wrobot-cli ports` |
| 32 | **自动连接先试打不开的口**：`candidate_ports()` 按口号顺序枚举，蓝牙口成对出现时往往先撞上"只配对未连接"的那个，每次都要等满超时 | 自动连接转半天，最后还是连不上 | `candidate_ports()` 改为**能打开的排前面**（`scan_ports_detailed()` 先分 online/offline 再拼接）；`probe_port()` 写入失败**立即放弃该口**（原来忽略写错误继续空等 3×350ms）；蓝牙口"能开但写 0 字节"时给出专门结论（**端口存在 ≠ 链路已建立**）。另：写超时 1000→1600ms（蓝牙 SPP 建链比有线慢），写入重试改为递增退避 250/600ms |
| 33 | **日志里的中文回应是乱码**：`sanitize_bytes()` 只认 UTF-8 与可打印 ASCII，而国产固件回的**中文几乎全是 GBK**（如 `就绪` = `BE CD D0 F7`）——GBK 字节不是合法 UTF-8，于是全被拆成 `<BE><CD>` | 机器明明回了中文，日志却是一串十六进制，看着像"乱码/波特率不对"，把排查带偏 | 新增 `decode_text()`：整段先按 UTF-8 严格解码，失败再用 Win32 `MultiByteToWideChar(936, MB_ERR_INVALID_CHARS)` 按 **GBK** 解码；**只有整段通过校验且可读占比 ≥70% 才采用**——波特率不对产生的随机字节几乎不可能整段通过 GBK 校验，仍会显示为十六进制，不会被误判成中文。新增测试 `gbk_chinese_is_decoded_not_hex` / `ascii_reply_is_untouched` |
| 34 | **蓝牙几分钟就断、连不稳**：空闲时链路上没有任何数据流，写字机那端的蓝牙模块（HC-05/06 一类）普遍有**空闲断链**机制 | 连上几分钟就掉线，只能手动重连，长图根本跑不完 | ① 新增**保活心跳**：后台线程空闲时每 500ms 检查一次，距上次活动超过间隔（默认 **5 秒**）就发一个空行（GRBL 回 `ok`、Marlin 不回也不报错），让链路始终有数据流；② 心跳发不出去即判定断链，**自动重开端口重连**并累计次数（状态栏可见"第 N 次重连"）；③ 设置 → **链路保活**：可调心跳间隔（0=关闭）与自动重连开关；④ 仓库根目录 `优化蓝牙稳定性.bat`：关闭 **USB 选择性暂停**与**无线适配器省电**——蓝牙断链的另外一半原因在 Windows 省电机制（需管理员运行）。新增测试 `keepalive_defaults_to_five_seconds` |
| 35 | **「设置里看得到 BtWriter，但连不上；它一会儿看得到一会儿看不到」**：程序此前完全没碰 Windows 蓝牙栈，只能靠 COM 口号猜——而「能搜到设备」「已配对」「真有数据通道」是三件互不等价的事；蓝牙 COM 口还可能是**没绑任何设备**的空口（设备实例 ID 里 MAC 全零），能打开却写 0 字节 | 用户把问题归到驱动/供电/信号上反复折腾，方向完全错了；实测 COM4 是空口、COM5 绑了 BtWriter 但打开报 121（底层从未连接） | 新增 `crates/wrobot-core/src/bluetooth.rs`（FFI 调 bthprops.dll）：`bt_devices()` 查已配对/已认证/已连接与最后见到时间；`bt_com_bindings()` 用 SetupAPI 读设备实例 ID 抠出**对端 MAC**，判定 COM 口到底绑没绑设备；`bt_set_spp()` 尝试自动启用 SPP 服务；`bt_watch()` 周期性 inquiry 输出可见性时间线并统计**状态翻转次数**。`bt_report()` 按状态组合给针对性结论——尤其「全程可见却一次都没连上」这一组合明确否掉供电与信号，直指「被手机占用 / 配对密钥失效 / 需重建传出口」。CLI：`wrobot-cli bt` / `bt list` / `bt bind` / `bt watch`；GUI 设备页新增「蓝牙链路」区块（体检 / 绑定蓝牙串口 / 观察可见性 / 打开蓝牙 COM 端口设置）。**踩坑**：`BLUETOOTH_ADDRESS` 是含 `ULONGLONG` 的 union，**必须 `#[repr(C, align(8))]`**，否则 `BLUETOOTH_DEVICE_INFO` 只有 556 字节（应为 560），全部蓝牙 API 静默失败——已加测试 `struct_sizes_match_win32` 钉死 |
| 36 | **实时日志 / 代码窗口里中文是乱码（方块）**：只给 egui 的 `Proportional` 字体家族注入了中文字体，而日志栏用的是 `TextStyle::Monospace`——该家族仍是 egui 默认字体，一个 CJK 字形都没有 | 界面其它地方中文正常，唯独日志和等宽区域显示成一串方块，看着像编码问题 | `setup_fonts()` 同时向 `FontFamily::Monospace` **追加**（push，不是插到首位）中文字体：ASCII 仍走等宽字体保持对齐，只有中文才回退到中文字体 |
| 37 | **没勾选「并入当前页面」，其它模块的内容（尤其图片）照样出现**：`collect_polylines()` 里前景图片是**无条件**叠加的，完全不经过 `module_active()`；而图片根本不在左侧勾选列表里，用户想关也关不掉 | 默认只输出当前模块的规则形同虚设，页面上混着别处导入的图片 | 新增 `images_on` 开关（`#[serde(default)]`，旧存档兼容）与 `images_active()`：勾选或停在图片页时才并入；左侧「并入当前页面」增加「图片」勾选框，未勾选时给出提示。背景参考图不参与输出，仍始终以浅色显示。新增测试 `images_not_merged_unless_enabled` |
| 38 | **代码传到机器上没有任何回话**：下发 G-code 只往日志里写内容，状态栏还是上一次的旧信息 | 点完「开始绘制 / 发送 G-code」不知道到底发出去了没有、发了多少 | `DevShared` 新增 `transfer_status`：下发前写「正在下发 N 行」、成功后写「✓ 已传输 N 行 / M 字节（x.xs）」、失败写「✗ 下发失败」并附排查建议、未连接时明确写「**没有发出去**」；左下角状态栏单独一行彩色显示，「开始绘制」/「发送当前 G-code」的状态文案也带上行数。日志同步增加开始/完成两行与耗时 |
| 39 | **报错只显示 `device error`，看不出是哪儿的问题**：`WError::Device` 的 Display 是英文裸前缀；`open_raw()` 的初始错误还是「未知错误」 | 用户只看到 "device error"，不知道下一步该干什么 | Display 改为「设备错误：{0}」（具体原因与 `err_hint()` 建议由调用方补在里面）；`open_raw()` 初值改为「打开 {port} 失败（未重试、未取得错误码）」 |
| 40 | **文字模块只能有一个文本框**：工程存档里一页本可以有任意多个文字元素，各自带字体/字号/位置，而程序把整页文字塞在一个 `String` 里 | 做不了多段落、多区域排版，跟参考工程对不上 | 新增 `TextBox`（名称/文字/字体/字号/行距/字距/对齐/段首/位置尺寸/开关）与 `texts: Vec<TextBox>` + `sel_text`；`ensure_texts()` 把旧的单框字段自动迁移成第一个框；`build_text_items()` 逐框输出（关闭或无内容的框跳过）；UI 文字页支持**增删、切换、单独开关**每个框，并带本框位置尺寸调节；打开工程 JSON 时每个文字元素生成一个框；「清空」只清当前框。新增测试 `text_module_supports_multiple_boxes` |
| 41 | **机器参数与参考项目不一致，且抬落笔参数不分固件**：GRBL/Marlin 共用一套 `pen_up/pen_down/z_speed`，切一次固件就得手改数值；还缺 `jog_speed`、`auto_set_zero`、`auto_connect`、`mm_to_steps`、`max_pen_speed` | 参数与 `奎享雕刻/default.properties` 对不上，换固件容易出错 | 按固件分别保存：GRBL 用 zOn/zOff/zSpeed（5/0/10000），Marlin 用 `marlin_pen_down_mm=2` / `marlin_pen_up_mm=0` / `marlin_z_speed=20000`（舵机模式用 50/0 角度），`gcode_options()` 按当前固件选取；补齐 `jog_speed=10000`（点动不再占用绘图速度）、`mm_to_steps=100`、`max_pen_speed=32767`、`auto_set_zero=true`（运行前发 G92 X0 Y0）、`auto_connect=true`（启动自动连接）。设置页新增「机器参数」分组，按 GRBL / Marlin / EBB 折叠显示。新增测试 `machine_defaults_match_reference_project` 与 `jog_uses_jog_speed_not_feed_rate` |
| 42 | **XY 运动方向给的空间太小：G-code 又密又长、机器绘制极慢**：单线手写字库每段笔画只有 0.01~0.05mm，逐点下发会产生海量「移动不到 0.1mm」的 G1 指令，文件大、固件逐行解析把速度全吃掉 | 一页字几千行指令，画一页要等很久（瓶颈在指令吞吐，不在机械速度） | `GcodeOptions` 新增 `min_step`（默认 **0.2mm**，界面可调 **0.1~1.0**），`generate()` 前先用 `decimate_polylines()` 抽稀：保留首点/末点，间距小于步进的中间点合并掉（笔尖 0.5mm，0.2mm 的合并肉眼无差别）；设置页新增「XY 最小步进」分区并给出默认值按钮。新增 `gcode::tests`（默认 0.2 / 抽稀保端点 / 行数大幅下降）与 GUI 测试 `xy_min_step_default_and_effect` |
| 43 | **设置里没有「回到机器原点 / 设当前位置为原点」**：这些指令此前只躺在「测试」页，用户进设置找不到 | 调机时要来回切页，容易漏掉解锁这一步 | 设置对话框新增「回到机器原点 / 设当前点为原点」分区：回到机器原点（`$H`/`G28`）、移到 (0,0)（无限位开关时用）、设当前位置为原点（`G92 X0 Y0`）、清除坐标偏置（`G92.1`）、读取当前位置（`?`/`M114`）、解锁 `$X`；未连接时给出橙色提示，回显统一写进底部实时日志 |
| 44 | **画布上看不出 X / Y 往哪边走**：反转 X/反转 Y/对调 XY 一开，画出来的字是镜像的还是旋转的全靠猜 | 方向搞反时只能反复试画，浪费纸张和时间 | 画布左上角新增 **XY 方向箭头**（**蓝 = X、绿 = Y**，带箭头与字母标注）：方向按 `reverse_x / reverse_y / swap_xy` 实时换算（对调后 X 沿页面 Y 方向指），可在设置里关闭（`axes_on`，默认开） |
| 45 | **点一下「图片」模块，所有图片就被并进输出**：`images_active()` 写成 `images_on \|\| tab == 5`，只要停在图片页就整包并入——包括别的页留下的、根本不想打印的图 | 明明没勾选，页面上却多出一堆图；生成的 G-code 里混着无关笔迹 | `images_active()` 改为**只看显式勾选**（停在图片页不再自动并入）；再给每张图加 `out` 开关，**模块开关 + 本图开关都开才输出**；图片页新增「并入当前页面」勾选与逐图「输出」勾选，画布上未勾选的图用灰色框/浅色笔迹表示「看得见但不打印」（仍可拖动缩放）。新增测试 `images_not_merged_on_image_tab_until_checked` |
| 46 | **文本框没有可视边界**：多框排版时只能靠坐标数字想象框在哪，重叠了也看不出来 | 定位文本框全靠试，调整 X/Y/W/H 没有参照 | 画布新增**虚拟文本框**（虚线矩形 + 框名，当前框蓝色高亮、关闭的框标「不输出」），**不参与打印**；开关 `text_frame_on`（默认开）在文字页与设置页均可切换。新增测试 `canvas_overlays_default_on_and_never_printed` 钉死「辅助层只画在画布上，不进 G-code」 |
| 47 | **行距 / 字间距不能为负**：滑块下限写死 0，想让字挤紧一点做不到 | 排版只能往松了调，密排（抄写、小字）做不出来 | 滑块放开到 行距 **-20~20mm**、字间距 **-10~10mm**；排版层加兜底：行步进下限 `size×0.15`、单字前进宽度下限 `size×0.05`，负值可以把行/字挤紧但**不会倒着排**。新增测试 `negative_line_gap_and_letter_space` |
| 48 | **随机变形一开就对所有文字生效**：`collect_polylines()` 给所有文字元素套同一份 `RandomSpec`，想只让其中一段像手写、其余保持工整做不到 | 整页一起抖，标题/正文无法区别对待 | `TextBox` 新增 `random` 字段，`build_text_tagged()` 为每个元素带上「本框是否允许随机」标记，只有勾上的框才喂 `rng`；文字页「逼真化」下新增**生效范围**勾选列表 + 全选/全不选/只选当前框。新增测试 `random_applies_only_to_selected_text_boxes` |
| 49 | **没有 PDF 导入（源程序用 PDFBox）**：只能导出 PDF，反过来读不进来 | 手上的字帖/讲义 PDF 用不了，只能先截图再当图片导入 | 新增 `crates/wrobot-core/src/pdfimport.rs`（**纯 Rust、零新增依赖**，复用字库模块的 DEFLATE 解压器）：对象表扫描 + **对象流（ObjStm）**展开、`FlateDecode`（含 PNG 预测器）/ `DCTDecode` / `ASCIIHexDecode` / `LZWDecode`；内容流解释路径算子（`m/l/c/v/y/h/re` + `S/s/f/B/b/n`，贝塞尔按设备尺度自动展平）、**跳过整页白底矩形与近白色描边**、图像 XObject（灰/RGB/CMYK/索引色，1/8 位深，按 CTM 求摆放矩形）、文本算子带 **ToUnicode** 解码（`bfchar`/`bfrange`）。导入策略：矢量路径直接描边、嵌入图像走影线/轮廓、**文字按原位置用手写字体重新排版**（这才是写字机该干的事）。界面在「导入导出」页与「文件」菜单：选 PDF → 挑页码 → **默认 1:1 原始尺寸**（可关掉改成目标高度）；导入结果进「图片」页，默认不并入输出。加密 PDF 明确报错。新增核心测试 `imports_real_pdf`、`roundtrip_export_import`、`skips_white_background_rect` 与 GUI 测试 `pdf_import_selected_page_one_to_one` |
| 50 | **页面显示的尺寸不对、也选不了纸张**：页面尺寸在代码里写死成 **175×255**（既不是 A4 也不是 B5），界面上只有两个滑块（50~400 / 50~500）可以瞎调，没有任何常规纸张可选 | 用户不知道自己在往多大的纸上排版；换纸只能手动量着填数字 | 新增 `PAPER_PRESETS`（A4/A5/A3/B4/B5/16K/32K/Letter/Legal/明信片）+ `PAPER_CUSTOM`：**默认改为标准 A4 210×297**；`AppState::paper` 记规格名（旧存档 `#[serde(default)]` 兼容），`set_paper()` 套用尺寸并**沿用当前横竖取向**、`toggle_orientation()` 一键横竖、`sync_paper()` 按宽高反判规格名（手动改过就显示「自定义」）。界面：**中央顶栏**与「导入导出」页都有纸张下拉 + 横竖按钮 + 自定义宽高微调；默认文本框同步放大到 180×260 以填满 A4。新增测试 `paper_preset_switch_custom_and_orientation` |
| 51 | **看不到机器现在在哪**：设备回显里的坐标（`MPos`/`WPos`、`M114` 的 `X/Y/Z`）只当普通文本进日志，界面上没有任何地方显示坐标 | 判断「回原点成功没有」「G92 设零点生效没有」「是不是跑到纸外了」只能翻日志猜 | ① `device.rs` 新增 `Position` 与 `parse_position()`：解析 GRBL `<Idle\|MPos:..\|WPos:..\|WCO:..>`（只有 `MPos`+`WCO` 时自己算 `WPos = MPos − WCO`）、Marlin `M114`（`Count` 之后的**步数不参与解析**）；回显与定时查询都写入 `DevShared::pos`。② 新增 `DevCmd::SetStatusPoll(q, ms)` + `status_tick()`：空闲时按间隔自动问一句（GRBL 只在收到 `?` 才回报），运行中绝不插队。③ 左栏新增**坐标**区：机器物理坐标（蓝）、当前工件坐标（绿）、**鼠标所在的页面坐标 → 机器坐标**实时换算，带「刷新」与「自动」开关；v0.4.0 起再加**一键回原点**按钮。新增测试 `parses_grbl_status_positions` / `parses_marlin_m114_and_ignores_count` / `non_position_lines_parse_to_empty` / `coordinate_panel_defaults_and_page_to_machine_mapping` |
| 52 | **对调 XY 后整页被平移（真 BUG），且箭头 / 点动各算一套方向**：`transform()` 先 `swap_xy` 交换 x/y，但镜像仍一律用 `page_w`/`page_h` —— 对调后「机器 X」装的是页面的 Y 值（取值 0..page_h），拿 `page_w` 去减就会把整页平移 `page_h − page_w` 毫米（A4 上是 **87mm**），字直接跑到纸外；另外画布蓝绿箭头按 `reverse_*` 另算一套符号、点动则**完全不做换算** | 开了「对调 XY」字就画到纸外面；开了「反转 X」按「X +」机器往左走，跟画布上的蓝箭头正好相反 | ① `transform()` 的镜像基准跟着对调走（`swap` 时用 `page_h/page_w`），并新增 `axis_dirs()` 由 transform **反推**机器轴在页面上的单位方向（`M⁻¹ = Mᵀ`），箭头与点动都改用它，从根本上杜绝两套算法不一致；② `jog_gcode()` 把页面位移投影到机器轴；③ `draw_axes()` 不再自己算符号，直接调 `axis_dirs()`。新增核心测试 `reverse_maps_page_into_page_bounds`（8 种开关组合下整页必须落在 0..w/0..h 内）、`swap_xy_uses_swapped_mirror_reference`、`axis_dirs_match_transform`、`mirroring_twice_is_identity`，GUI 测试 `jog_follows_reverse_and_swap_like_the_arrows` |
| 53 | **页面布局不合理**：① 左栏 `resizable(false)` 且没有滚动，窗口一矮「并入当前页面 / 字库 / 坐标」就被底部的状态行挤掉；② 文本框 X/Y/宽/高滑块范围写死 0~300 / 0~400 / 10~320 / 10~420，换成大纸拖不动、换成小纸又能拖到纸外；③ 右栏底部「页面边框 + 固件/生成/导出」在**任何** tab 都整块铺开，设备 / 测试 / 运行中页本就很长，本页参数被挤到看不见 | 小窗口下左栏内容丢失；换纸后文本框定位失控；长页面要滚很久才够到本页参数 | ① 左栏改 `resizable(true)`（默认 196px，150~340 可调）+ 上半部分包 `ScrollArea`，状态行仍固定底部；② 文字框四个滑块的**上限改为当前纸张宽高**，并新增「收进页面内」按钮（`clamp_text_box()`，留 10mm 边距一键归位）；③ 右栏公共区段在内容页（0~5）与导入导出页照旧铺开，在设备 / 测试 / 运行中页收进默认折叠的 `CollapsingHeader` |
| 54 | **DXF / SVG 矢量文件导入缺失**：绘图只能靠图片描边或手绘，参考程序的 `dxf.json`/`svg.json` 矢量背景导入完全没有对应实现 | 无法把 CAD 图 / SVG 矢量直接作为书写背景，对照原版缺一块实打实的功能 | 新增 `wrobot-core::dxf`（纯 Rust 解析 DXF R12+ ASCII 全部常见图元 + BLOCK/INSERT 变换）与 `wrobot-core::svg_import`（path 全指令 / 基本图元 / `<g>` 变换 / 贝塞尔 / 椭圆弧）；统一 `render::fit_to_page` 归一化到页面 mm（y 向下、原点左上），DXF 翻转 Y 轴以纠正其 Y-up 朝向；GUI「绘图」面板加「导入 DXF / SVG 矢量文件」按钮 + 目标高度滑块；CLI 新增 `dxf`/`svgi` 子命令（→ G-code / SVG）；全部补测试（core 11 个 + cli 3 个）。关键坑：`Xf` 默认构造若 `derive(Default)` 会把 `sx/sy` 置 0 导致所有点塌到原点，改为手写 `impl Default` 返回单位变换 |
| 55 | **生成的 G-code 按「机器原点」算，导致移动超出限制**：`generate()` 只输出 `G21`+`G90`+页面局部坐标，**G-code 里没有任何原点锚定**。这些 `G90` 绝对坐标相对谁解析完全取决于机器当时有没有 G92 偏置——只有运行前序列 `send_unlock()` 里那句 `G92 X0 Y0` 才让它相对"当前位置"；一旦**导出成 `.nc` 文件**、用「发送当前 G-code」、关掉 `auto_set_zero`、或 `unlock_first=false` 跳过准备序列，坐标就被解析成**机器绝对坐标**，机器要跑到 `X210 Y297`（A4）这类绝对值 → 超过机器行程 → 软限位报警（移动超出限制） | **严重**：换台机器/换个起始位置就报警不动；导出的 `.nc` 拿到别的发送器上跑必然超程 | 让 G-code **自带原点锚定**：`GcodeOptions` 新增 `use_current_origin`（默认 `true`），`generate()` 在 `G90` 之后输出 `G92 X0 Y0 ; 以当前位置为原点（后续坐标相对该点）`（**EBB 跳过**，它不认 G92、本就用 SM 步进指令）；结尾注释按是否锚定区分「回到页面零点（当前原点）」/「回机器原点」，避免误解成回机械原点。GUI 把既有 `auto_set_zero` 接到该字段，`gcode_options()` 自动带上。新增测试 `gcode_anchors_at_current_origin`（G92 必须在第一个移动之前）、`anchored_coords_stay_within_page`、`ebb_gets_no_g92`、`use_current_origin_off_keeps_machine_absolute`，GUI 测试 `gcode_origin_follows_auto_set_zero` |
| 56 | **「测试方框」也按机器原点跑（BUG #55 同类）**：审计发现 `draw_test_frame()` 生成的 `G21/G90` 绝对坐标依赖外部 `send_unlock()` 注入的 `G92`；而 `send_unlock` 的 G92 仅在 `unlock_first && auto_set_zero` 都为真时才发出。一旦关掉这两个开关之一，测试方框就按**机器绝对坐标**跑 → 同样超出行程 | 与 #55 同一类：测试方框画出纸外 / 软限位报警 | 把测试方框的 G-code 生成抽成纯函数 `test_frame_gcode()` 并**自带 `G92 X0 Y0` 锚定**（EBB 跳过），不再依赖运行前序列的外部时序；`draw_test_frame()` 改为调用它。新增 GUI 测试 `test_frame_anchors_current_origin`（GRBL 自带 G92 且在首个移动之前、EBB 不含 G92） |
| 57 | **图片描线太慢：逐行扫描把每个墨段切成一条独立笔画**：逐行取出连续墨段后，每段各自生成一条两点折线；而 `generate()` 对**每条折线**都要「抬笔 → 快速空移 → 落笔」，于是几百行 = 几百次抬落笔，且每行都是左→右画完再跳回下一行最左端，空移≈整幅画宽 | 画一张图耗时是实际书写长度的数倍，大图尤其明显 | 新增 **「高效（连续笔画）」** 绘制方式（`draw_type == 2`）：① **行间串联**——相邻两行墨段若横向范围覆盖了上一条线的收笔位置，就**笔不离纸**顺下去画，并从离笔尖更近的那一端入笔，天然形成左右往复的**蛇形长笔画**；② **最优路径排序**（核心新增 `render::optimize_path_order` / `path_travel`）——最近邻构造 + 相邻交换 2-opt 精修，**允许笔画反向**，输出时按「就近起笔」定向，保证 G-code 实走距离就是算出来的距离。实测（120×120 纯黑方块 / 60mm / 行距 4px）：**笔画 30 条 → 1 条，空移 1741mm → 0mm，总行程 3541mm → 1858mm（降 47%）**，且墨迹内容不丢、互不相连的色块不会被硬连。新增 core 测试 5 个（`path_travel_measures_as_drawn` / `optimize_reverses_next_row_into_serpentine` / `optimized_travel_is_shorter` / `reordering_preserves_strokes` / `optimize_handles_many_row_strokes`）+ GUI 测试 2 个（`continuous_draw_mode_cuts_pen_lifts` / `continuous_mode_does_not_bridge_separate_shapes`） |
| 58 | **切换编辑模式后，未勾选叠加的其他模块内容仍残留在页面上**：`module_active()` 在非内容页（图片 / 设备 / 测试 / 导入导出 / 运行中）会靠 `last_content_tab` 自动带入「最近停留过的内容模块」，于是切到图片 / 设备页后，上一个模块的笔迹仍然显示在画布上，即便用户**没勾选**任何「并入当前页面」叠加框 | 图片页里还压着上一层文字、设备页里堆着上一个模块的内容，干扰定位也看不清当前到底在编什么 | 重写 `module_active()`：① 显式勾选 `module_on` 的模块始终并入；② 仅当**停留在本内容编辑页**时自动并入当前页模块；③ **非内容页不再自动带入** `last_content_tab`——必须显式勾选叠加才显示。同步删除已无用的 `last_content_tab` 字段（`serde` 对旧存档多出的该字段会直接忽略，加载兼容）。左栏提示文案改为「非内容页：默认只显示已勾选叠加的模块」。新增 GUI 测试 `switching_to_noncontent_tab_clears_unoverlaid_content` |
| 59 | **开启标尺时预览左侧出现大片空白**：标尺带从「可用区边缘」一直填充到「页面边缘」（`avail.min → page_rect.min.x`）；页面在预览框里居中时，两者之间原本就有几百像素的居中留白，于是被整片填成灰底 | 一开标尺左侧就多出一大片灰底，版面失衡，看起来像「预览没居中」 | **根因在标尺带宽度跟着间距走**，不在页面对齐方式（先前改成左上对齐只是掩盖症状）。改为：标尺是**紧贴页面边缘、厚度固定**（`RULER_PX = 30`）的带子，长度与页面对齐，**不依赖可用区到页面的间距**；居中留白不再被填色，只是普通背景，开/关标尺的版面一致。页面仍保持**居中**（左右留白对称），边距放大到「带厚 + 6px ≈ 36px」保证标签放得下且不溢出；窗口过小时（<80px）退化为无标尺居中型。新增测试 `ruler_layout_centers_page_in_box`（断言居中、左右留白对称、左留白 ≥ 边距、`RULER_PX ≤ margin`） |

---

## 11. 版本说明（Changelog）

### v0.5.0（2026-10-17）
- **【功能】运行前边界自检**：生成 G-code 后解析其 XY 坐标范围（新增 `gcode_xy_bounds`），与「设置 → 机器幅面」比对。内容宽/高超过机器行程，或（关闭「以当前位置为原点」时）绝对坐标落到工作区域外，都会**弹窗列出超出方向与数值**，由用户选择「仍然执行 / 取消」；「运行中」页可一键开关（默认开启）。避免撞机与软限位报警。
- **【功能】图片裁剪**：图片面板新增「裁剪」——按**原图比例**滑块设定左/上/右/下，裁剪后目标高度与笔迹尺寸随之重算；一键「重置裁剪」。旧工程兼容（字段带 `serde` 默认）。
- **【功能】绘画方式参数扩展**（原有影线间隔/暗部阈值/边缘阈值之外）：
  - **扫描方向**：横向 → / 对角 45° ↘ / 纵向 ↓ / 对角 135° ↗ —— 影线不再只能横着走，斜纹/竖纹质感一键切换（影线生成统一改为「扫描线」模型，见 `io::scan_lines`）；
  - **笔画顺序**：默认 / 最优路径（最近邻+2-opt）/ 按扫描顺序 / **分块蛇形**（按 `分块边长` 把画面切方格、块内最优、块间像犁地一样蛇形走）；
  - **连笔跨行容差（px）**（连续笔画模式）：0 = 只接紧挨着的段，调大可把隔得较远的段也串成长笔画（抬笔更少、连接线更多），取代原先写死的「行内覆盖即续接」。
  - 以上均为**每张图片独立参数**，同时提供默认值（作用于后续导入）。
- **【功能】应用图标**：新增蓝→青渐变底板 + 纸张 + 手写波浪线 + 钢笔的应用图标（标题栏/任务栏生效）。图标由 `tools/make_icon.py`（纯标准库超采样光栅化）生成，PNG 编译期嵌入（`assets/icon.png`），并输出多尺寸 `assets/icon.ico`。
- **【环境】**：本机 Rust 工具链曾被卸载，本次重新安装（rustc/cargo 1.99.1，MSVC），并配置 crates.io 镜像加速。
- 测试：**合计 124 个用例全部通过、0 警告**（cli 3 + 核心单元 57 + 核心集成 12 + GUI 51 + doctest 1）。核心新增 `gcode_xy_bounds` 2 个用例；GUI 新增边界自检、裁剪、纵向扫描、连笔容差、分块蛇形 5 个回归用例。

### v0.4.1（2026-10-12）
- **【修复 #59】开启标尺时预览左侧出现大片空白**：**真正根因**是标尺带从「可用区边缘」一直填充到「页面边缘」（`avail.min → page_rect.min.x`）。页面在预览框里居中时，这两者之间原本就有几百像素的居中留白，于是被整片填成灰底 → 就是看到的「左侧大片空白」（左上对齐只是掩盖了症状，不是根因）。
  - 改为：标尺是**紧贴页面边缘、厚度固定**（`RULER_PX = 30`）的带子，长度与页面对齐，**不依赖可用区到页面的间距**；居中留白不再被填色，只是普通背景，开/关标尺的版面一致。
  - 页面仍保持**居中**（左右留白对称），边距放大到「标尺带厚 + 6px ≈ 36px」，保证标尺带与数值标签放得下且不溢出可用区；窗口过小时（<80px）退化为无标尺居中型。
  - 新增回归测试 `ruler_layout_centers_page_in_box`：断言页面居中、左右留白对称、左留白 ≥ 边距、且 `RULER_PX ≤ margin`（带子放得进预留边距）。
- 测试：**116 个用例通过、0 警告**（cli 3 + 核心单元 55 + 核心集成 12 + GUI 46）。doctest 因 Windows 命名管道耗尽（`Os 231`「所有的管道范例都在使用中」）无法启动 `rustc`，属环境限制、与代码无关。

### v0.4.0（2026-10-12）
- **【功能】页面标尺**：中央画布**外侧**（顶部 + 左侧）新增毫米标尺，每 10mm 一个刻度、每 50mm 标一个数值，一眼读出「这一列 / 这一行在页面多少毫米处」，定位更准。标尺默认开启，可在画布上方工具条一键关掉。
- **【功能】可调虚线网格**：新增「网格」开关，可单独调节**列数 / 行数**，以浅蓝色虚线绘制等分参考线（仅用于定位、**不打印**），覆盖在原有 10mm 淡网格之上，方便把内容对齐到任意等分格。状态字段 `ruler_on` / `grid_overlay` / `grid_overlay_cols` / `grid_overlay_rows`（带 `serde` 默认，旧存档兼容）。
- **【功能】左下角一键回原点**：左栏实时坐标区下方新增蓝色「↺ 回原点」按钮，一键发 `G0 X0 Y0` 把笔移到坐标零点（0,0），运行中想随时把笔退回家不必切到顶部运行条。
- **【修复 #58】切换编辑模式清除未叠加内容**：重写 `module_active()`——非内容页（图片 / 设备 / 测试 / 导入导出 / 运行中）**不再自动带入**上一个内容模块，必须显式勾选「并入当前页面」才显示；删去已无用的 `last_content_tab` 字段（旧存档加载兼容）。修复「切到图片/设备页后旧笔迹还压在画布上」的残留问题。
- 测试：GUI 新增 2 个回归用例（`switching_to_noncontent_tab_clears_unoverlaid_content` / `view_ruler_grid_defaults`），**合计 116 个用例全部通过，0 警告**。

### v0.3.0（2026-09-30）
- **【性能】图片新增「高效（连续笔画）」绘制方式**：原来逐行扫描把每个墨段切成一条独立笔画，每条都要「抬笔→空移→落笔」，且每行都跳回最左端。新方式把相邻行的墨段串成**蛇形长笔画**（笔不离纸），再用 `render::optimize_path_order`（最近邻 + 2-opt，允许笔画反向）做**最优路径排序**。
- **实测收益**（120×120 纯黑方块、目标高 60mm、行距 4px）：**笔画 30 条 → 1 条，空移 1741mm → 0mm，总行程 3541mm → 1858mm（降 47%）**；墨迹内容不丢，互不相连的色块不会被跨空隙硬连。
- 描线类型下拉现为三项：`线框（轮廓）` / `素描（影线）` / `高效（连续笔画）`，图片导入默认参数与单张图片均可单独选择。
- **注意**：连续笔画依赖「行间距离为有效位移」，行间距（由「影线间隔」决定）建议大于设置的 XY 最小步进，否则会被 `decimate` 抽稀掉。
- 核心新增 `path_travel`（按 G-code 实际走法丈量行程）、`optimize_path_order` / `optimize_path_order_from`。

### v0.2.0（2026-09-30）
- **【功能】DXF / SVG 矢量文件导入**：绘图模块支持把 CAD（.dxf）与矢量（.svg）作为书写背景；新增 `wrobot-core::dxf` 与 `wrobot-core::svg_import`（纯 Rust、零外部依赖），统一 `render::fit_to_page` 归一化到页面 mm（y 向下、原点左上，DXF 翻转 Y 轴）。GUI「绘图」面板加导入按钮 + 目标高度滑块，CLI 新增 `dxf`/`svgi` 子命令。
- **【修复 #55】G-code 以「当前原点」计算**：`generate()` 现自带 `G92 X0 Y0` 锚定，导出的 `.nc` / 关掉「运行前自动设原点」/ 关掉「运行前解锁」都不会再按机器绝对坐标跑（避免超出行程软限位报警）。EBB 固件跳过 G92。
- **【修复 #56】「测试方框」同样锚定当前原点**：抽成纯函数 `test_frame_gcode()` 自带锚定，不再依赖运行前序列。
- **【审计结论】** 全代码库机械原点 / 当前原点相关的同类隐患至此已清零。
- 测试套件：**合计 107 个用例全部通过，0 警告**。

### v0.1.0
- 初始复刻版本：九大功能模块（笔记/文字/表格/格子/绘图/字体管理/逼真化/设备连接/模板）、GRBL/Marlin/EBB 三类固件、串口（含蓝牙 SPP）/TCP/Mock 三后端、PDF 导入、纸张预设、坐标显示、XY 对调/反转一致性修复、图片描边与变换、SVG 导出等。

---

## 12. 免责声明

本仓库仅用于**学习与研究 Rust 工程化与写字机控制原理**，字库与样本来自用户自有/已授权的原版安装包。请勿用于任何侵犯第三方权益的用途。
