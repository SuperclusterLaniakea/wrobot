//! G-code 生成：GRBL / Marlin / EBB。
//!
//! 落笔 = 沿折线走刀（G1），抬笔 = 快速移动到下一条笔画起点（G0）。

use crate::config::{Firmware, MachineConfig, PenMode};
use crate::render::Polyline;

/// G-code 输出参数。
#[derive(Debug, Clone)]
pub struct GcodeOptions {
    pub firmware: Firmware,
    pub pen_mode: PenMode,
    /// 落笔移动速度（mm/min）
    pub feed_rate: f64,
    /// Z 轴/舵机动作速度
    pub z_speed: f64,
    /// 抬笔位置（mm 或角度或 EBB 参数）
    pub pen_up: f64,
    /// 落笔位置
    pub pen_down: f64,
    /// EBB：毫米→步数
    pub mm_to_steps: f64,
    /// EBB：单段移动耗时（毫秒）
    pub ebb_move_ms: u32,
    pub reverse_x: bool,
    pub reverse_y: bool,
    pub swap_xy: bool,
    pub offset_x: f64,
    pub offset_y: f64,
    /// 页面宽（毫米），>0 时翻转按页面做镜像
    pub page_w: f64,
    /// 页面高（毫米）
    pub page_h: f64,
    pub custom_start: String,
    pub custom_end: String,
    /// 坐标小数位
    pub decimals: usize,
    /// 自定义抬笔指令（覆盖默认）
    pub pen_up_cmd: Option<String>,
    /// 自定义落笔指令（覆盖默认）
    pub pen_down_cmd: Option<String>,
    /// 坐标原点所在角：0 左上 1 右上 2 左下 3 右下
    pub origin_corner: usize,
    /// 激光模式：用 M3/M5 控制光闸代替落笔
    pub laser_mode: bool,
    /// 激光功率（0..1000 或固件定义范围）
    pub laser_power: f64,
    /// 文件完成后自动休眠机器
    pub auto_sleep: bool,
    /// 落笔后暂停（秒）
    pub tool_on_delay: f64,
    /// 抬笔前暂停（秒）
    pub tool_off_delay: f64,
    /// EBB：舵机速度
    pub ebb_servo_speed: f64,
    /// EBB：最大舵机值
    pub ebb_max_pen: f64,
    /// **XY 最小步进（毫米）**：相邻两点间距小于该值的中间点会被合并掉。
    ///
    /// 单线手写字库的笔画点很密（常见 0.01~0.05mm 一段），逐点下发会产生
    /// 海量「移动不到 0.1mm」的 G1 指令：文件又长、机器逐行解析又慢，
    /// 实际绘制速度被指令吞吐拖垮。按 0.1~1mm 抽稀后行数可降一个数量级，
    /// 视觉上（笔尖 0.5mm 左右）几乎看不出差别。
    pub min_step: f64,
    /// **以「当前位置」为原点**：在 G-code 开头输出 `G92 X0 Y0`，
    /// 让后面所有坐标都相对**机器当前所在的那一点**计算。
    ///
    /// 这是写字机的标准做法：把笔移到纸面起点 → 设零点 → 画整页，
    /// 机器只在当前位置附近走一个页面大小，**永远不会按机械绝对值跑到行程外**。
    ///
    /// 关掉的话坐标就是**机器绝对坐标**（`G90` 直接相对机械原点解析），
    /// 页面尺寸一旦逼近/超过机器行程就会软限位报警——即「移动超出限制」。
    pub use_current_origin: bool,
}

impl Default for GcodeOptions {
    fn default() -> Self {
        GcodeOptions {
            firmware: Firmware::Grbl,
            pen_mode: PenMode::Z,
            feed_rate: 10000.0,
            z_speed: 10000.0,
            pen_up: 0.0,
            pen_down: 5.0,
            mm_to_steps: 100.0,
            ebb_move_ms: 20,
            reverse_x: false,
            reverse_y: true,
            swap_xy: false,
            offset_x: 0.0,
            offset_y: 0.0,
            page_w: 0.0,
            page_h: 0.0,
            custom_start: String::new(),
            custom_end: String::new(),
            decimals: 3,
            pen_up_cmd: None,
            pen_down_cmd: None,
            origin_corner: 0,
            laser_mode: false,
            laser_power: 1000.0,
            auto_sleep: false,
            tool_on_delay: 0.0,
            tool_off_delay: 0.0,
            ebb_servo_speed: 750.0,
            ebb_max_pen: 30000.0,
            min_step: 0.2,
            use_current_origin: true,
        }
    }
}

/// 参考默认：XY 最小步进 0.2mm（GUI 可调 0.1~1.0mm）。
pub const DEFAULT_MIN_STEP: f64 = 0.2;

impl GcodeOptions {
    /// 由机器配置（default.properties）派生。
    pub fn from_machine(cfg: &MachineConfig) -> GcodeOptions {
        GcodeOptions {
            firmware: cfg.firmware,
            pen_mode: cfg.pen_mode,
            feed_rate: cfg.feed_rate,
            z_speed: cfg.z_speed,
            pen_up: cfg.pen_up_pos,
            pen_down: cfg.pen_down_pos,
            mm_to_steps: cfg.mm_to_steps,
            reverse_x: cfg.reverse_x,
            reverse_y: cfg.reverse_y,
            swap_xy: cfg.swap_xy,
            custom_start: cfg.custom_start.clone(),
            custom_end: cfg.custom_end.clone(),
            ..Default::default()
        }
    }
}

/// 数字格式化：`d>0` 时去掉多余的小数尾零；`d==0` 时按整数输出（不可去零，
/// 否则 10000 会被截成 1）。
fn num(v: f64, d: usize) -> String {
    if d == 0 {
        return format!("{:.0}", v);
    }
    let s = format!("{:.*}", d, v);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

impl GcodeOptions {
    /// 页面坐标 → 机器坐标。
    pub fn transform(&self, x: f64, y: f64) -> (f64, f64) {
        let (mut x, mut y) = if self.swap_xy { (y, x) } else { (x, y) };
        // 镜像基准必须**跟着对调走**：对调 XY 之后「机器 X」装的是页面的 Y 值
        // （取值 0..page_h），此时若还拿 page_w 做镜像，整页会被平移
        // `page_h - page_w` 毫米——页面宽高不等时字就跑到纸外面去了。
        let (rx, ry) = if self.swap_xy {
            (self.page_h, self.page_w)
        } else {
            (self.page_w, self.page_h)
        };
        // 原点角：1/3 → 镜像 X；2/3 → 镜像 Y（与 reverse_* 叠加）
        if self.origin_corner & 1 != 0 {
            x = if rx > 0.0 { rx - x } else { -x };
        }
        if self.origin_corner & 2 != 0 {
            y = if ry > 0.0 { ry - y } else { -y };
        }
        if self.reverse_x {
            x = if rx > 0.0 { rx - x } else { -x };
        }
        if self.reverse_y {
            y = if ry > 0.0 { ry - y } else { -y };
        }
        (x + self.offset_x, y + self.offset_y)
    }

    /// **机器轴在页面上的指向**（供画布蓝/绿箭头与点动按钮使用）。
    ///
    /// 返回 `(机器X方向, 机器Y方向)`，每个方向都是页面坐标系里的单位向量
    /// （x 向右为正、y 向下为正）。例如 `(1,0)` 表示「机器 X 正方向 = 页面向右」。
    ///
    /// 这样箭头画出来的指向就一定等于实际下发 G-code 的移动方向，
    /// 不会出现「箭头指这边、机器走那边」。
    pub fn axis_dirs(&self) -> ((f64, f64), (f64, f64)) {
        // 页面 +X / +Y 各走 1mm，看机器坐标往哪边变（M = 页面→机器 的方向矩阵）。
        let a = self.transform(0.0, 0.0);
        let bx = self.transform(1.0, 0.0);
        let by = self.transform(0.0, 1.0);
        let dx = (sgn(bx.0 - a.0), sgn(bx.1 - a.1));
        let dy = (sgn(by.0 - a.0), sgn(by.1 - a.1));
        // M 是带符号的置换矩阵，故 M⁻¹ = Mᵀ：
        // 机器 X 在页面上的方向取 M 的第 0 **行**，机器 Y 取第 1 行。
        ((dx.0, dy.0), (dx.1, dy.1))
    }

    fn pen_up_cmd(&self) -> String {
        if let Some(c) = &self.pen_up_cmd {
            return c.clone();
        }
        // 激光模式：M5 关光（EBB 无激光支持）
        if self.laser_mode && self.firmware != Firmware::Ebb {
            return "M5".to_string();
        }
        match (self.firmware, self.pen_mode) {
            (Firmware::Ebb, _) => "SP,1".to_string(), // 请按 EBB 固件手册确认方向
            (_, PenMode::Servo) => format!("M280 P0 S{}", num(self.pen_up, 0)),
            _ => format!("G0 Z{}", num(self.pen_up, 3)),
        }
    }

    fn pen_down_cmd(&self) -> String {
        if let Some(c) = &self.pen_down_cmd {
            return c.clone();
        }
        // 激光模式：M3 S<power> 开光
        if self.laser_mode && self.firmware != Firmware::Ebb {
            return format!("M3 S{}", num(self.laser_power, 0));
        }
        match (self.firmware, self.pen_mode) {
            (Firmware::Ebb, _) => "SP,0".to_string(),
            (_, PenMode::Servo) => format!("M280 P0 S{}", num(self.pen_down, 0)),
            _ => format!("G1 Z{} F{}", num(self.pen_down, 3), num(self.z_speed, 0)),
        }
    }

    fn move_fast(&self, x: f64, y: f64) -> String {
        match self.firmware {
            Firmware::Ebb => {
                let sx = (x * self.mm_to_steps).round() as i64;
                let sy = (y * self.mm_to_steps).round() as i64;
                format!("SM,{},{},{}", self.ebb_move_ms, sx, sy)
            }
            _ => format!("G0 X{} Y{}", num(x, self.decimals), num(y, self.decimals)),
        }
    }

    fn move_draw(&self, x: f64, y: f64) -> String {
        match self.firmware {
            Firmware::Ebb => {
                let sx = (x * self.mm_to_steps).round() as i64;
                let sy = (y * self.mm_to_steps).round() as i64;
                format!("SM,{},{},{}", self.ebb_move_ms, sx, sy)
            }
            _ => format!(
                "G1 X{} Y{} F{}",
                num(x, self.decimals),
                num(y, self.decimals),
                num(self.feed_rate, 0)
            ),
        }
    }
}

/// 方向符号：`0 → 0`（`f64::signum` 会把 0 也当成 +1，箭头会多出一个假分量）。
fn sgn(v: f64) -> f64 {
    if v.abs() < 1e-9 {
        0.0
    } else if v > 0.0 {
        1.0
    } else {
        -1.0
    }
}

/// 按「最小步进」抽稀一条折线：去掉与上一个保留点间距小于 `min_step` 的中间点。
///
/// - 首点必留（抬笔后要先走到笔画起点）；
/// - 末点必留（笔画终点不能缩短，否则字形收尾缺一截）；
/// - `min_step <= 0` 或点数 < 3 时原样返回。
pub fn decimate(pts: &[(f64, f64)], min_step: f64) -> Vec<(f64, f64)> {
    if min_step <= 0.0 || pts.len() < 3 {
        return pts.to_vec();
    }
    let m2 = min_step * min_step;
    let mut out: Vec<(f64, f64)> = Vec::with_capacity(pts.len());
    out.push(pts[0]);
    for p in &pts[1..pts.len() - 1] {
        let (lx, ly) = *out.last().unwrap();
        let dx = p.0 - lx;
        let dy = p.1 - ly;
        if dx * dx + dy * dy >= m2 {
            out.push(*p);
        }
    }
    let last = *pts.last().unwrap();
    let (lx, ly) = *out.last().unwrap();
    if (last.0 - lx).abs() > f64::EPSILON || (last.1 - ly).abs() > f64::EPSILON {
        out.push(last);
    }
    out
}

/// 对整批折线抽稀（见 [`decimate`]），丢弃抽稀后不足两点的笔画。
pub fn decimate_polylines(paths: &[Polyline], min_step: f64) -> Vec<Polyline> {
    if min_step <= 0.0 {
        return paths.to_vec();
    }
    paths
        .iter()
        .filter_map(|p| {
            let pts = decimate(&p.pts, min_step);
            if pts.len() >= 2 {
                Some(Polyline::new(pts))
            } else {
                None
            }
        })
        .collect()
}

/// 生成完整 G-code。
pub fn generate(paths: &[Polyline], opts: &GcodeOptions) -> String {
    let mut o = String::new();
    o.push_str("; WritingRobot (Rust rewrite) - 单线仿手写\n");
    o.push_str(&format!("; firmware: {}\n", opts.firmware.as_str()));
    o.push_str("G21 ; mm\n");
    o.push_str("G90 ; absolute\n");
    // 以「机器当前位置」为原点：后面的页面坐标全部相对这一点计算，
    // 机器只在当前位置附近走一个页面大小，不会按机械绝对值跑到行程外。
    // （EBB 不认 G92，且它本来就用 SM 步进指令，跳过）
    let anchored = opts.use_current_origin && opts.firmware != Firmware::Ebb;
    if anchored {
        o.push_str("G92 X0 Y0 ; 以当前位置为原点（后续坐标相对该点）\n");
    }
    if !opts.custom_start.trim().is_empty() {
        for c in opts.custom_start.split(';') {
            let c = c.trim();
            if !c.is_empty() {
                o.push_str(c);
                o.push('\n');
            }
        }
    }
    o.push_str(&opts.pen_up_cmd());
    o.push('\n');

    let mut pen_down = false;
    // 先按 XY 最小步进抽稀：密集的微移动指令是「G-code 又长、绘制又慢」的主因
    let paths = decimate_polylines(paths, opts.min_step);
    for p in &paths {
        if p.pts.len() < 2 {
            continue;
        }
        let (x0, y0) = opts.transform(p.pts[0].0, p.pts[0].1);
        if pen_down {
            push_pen_up(&mut o, opts);
        }
        o.push_str(&opts.move_fast(x0, y0));
        o.push('\n');
        push_pen_down(&mut o, opts);
        pen_down = true;
        for pt in &p.pts[1..] {
            let (x, y) = opts.transform(pt.0, pt.1);
            o.push_str(&opts.move_draw(x, y));
            o.push('\n');
        }
    }
    if pen_down {
        push_pen_up(&mut o, opts);
    }
    if !opts.custom_end.trim().is_empty() {
        for c in opts.custom_end.split(';') {
            let c = c.trim();
            if !c.is_empty() {
                o.push_str(c);
                o.push('\n');
            }
        }
    }
    // 结尾回到原点：锚定过就是「页面零点（= 机器当前位置）」，没锚定才是机械原点。
    if anchored {
        o.push_str("G0 X0 Y0 ; 回到页面零点（当前原点）\n");
    } else {
        o.push_str("G0 X0 Y0 ; 回机器原点\n");
    }
    if opts.auto_sleep {
        match opts.firmware {
            Firmware::Grbl => o.push_str("$SLP ; 自动休眠\n"),
            Firmware::Marlin => o.push_str("M84 ; 关闭电机\n"),
            Firmware::Ebb => {}
        }
    }
    o
}

/// 输出「抬笔前暂停 + 抬笔指令」。
fn push_pen_up(o: &mut String, opts: &GcodeOptions) {
    if opts.tool_off_delay > 0.0 && opts.firmware != Firmware::Ebb {
        o.push_str(&format!("G4 P{}\n", num(opts.tool_off_delay, 3)));
    }
    o.push_str(&opts.pen_up_cmd());
    o.push('\n');
}

/// 输出「落笔指令 + 落笔后暂停」。
fn push_pen_down(o: &mut String, opts: &GcodeOptions) {
    o.push_str(&opts.pen_down_cmd());
    o.push('\n');
    if opts.tool_on_delay > 0.0 && opts.firmware != Firmware::Ebb {
        o.push_str(&format!("G4 P{}\n", num(opts.tool_on_delay, 3)));
    }
}

/// 统计 G-code 中的落笔/抬笔次数。
pub fn count_pen_cycles(gcode: &str, opts: &GcodeOptions) -> (usize, usize) {
    let up = opts.pen_up_cmd();
    let down = opts.pen_down_cmd();
    let mut u = 0;
    let mut d = 0;
    for line in gcode.lines() {
        let t = line.trim();
        if t == up {
            u += 1;
        } else if t == down {
            d += 1;
        }
    }
    (u, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 默认最小步进 0.2mm（GUI 可调 0.1~1.0）。
    #[test]
    fn default_min_step_is_point_two() {
        assert_eq!(GcodeOptions::default().min_step, 0.2);
        assert_eq!(DEFAULT_MIN_STEP, 0.2);
    }

    /// 抽稀：间距小于 min_step 的中间点被合并，首末点必须保留。
    #[test]
    fn decimate_keeps_ends_and_drops_micro_moves() {
        let pts: Vec<(f64, f64)> = (0..=10).map(|i| (i as f64 * 0.1, 0.0)).collect();
        let d = decimate(&pts, 0.2);
        assert_eq!(d.first().copied(), Some((0.0, 0.0)), "首点必留");
        assert_eq!(d.last().copied(), Some((1.0, 0.0)), "末点必留");
        assert!(d.len() < pts.len(), "应抽掉一些点：{} -> {}", pts.len(), d.len());
        // min_step<=0 时原样返回
        assert_eq!(decimate(&pts, 0.0).len(), pts.len());
        // 点数 < 3 原样返回
        assert_eq!(decimate(&[(0.0, 0.0), (0.05, 0.0)], 1.0).len(), 2);
    }

    /// G-code 行数：开启最小步进后应明显变少（这是「绘制太慢」的直接原因）。
    #[test]
    fn min_step_shortens_gcode() {
        // 一条 0.01mm 一段的密集折线：共 500 段
        let pts: Vec<(f64, f64)> = (0..=500).map(|i| (i as f64 * 0.01, 0.0)).collect();
        let paths = vec![Polyline::new(pts)];
        let mut o = GcodeOptions::default();
        o.min_step = 0.0;
        let n_full = generate(&paths, &o).lines().filter(|l| l.starts_with("G1 X")).count();
        o.min_step = 0.2;
        let n_thin = generate(&paths, &o).lines().filter(|l| l.starts_with("G1 X")).count();
        assert!(n_thin > 0 && n_thin < n_full / 2, "抽稀后行数应大幅减少：{n_full} -> {n_thin}");
    }

    fn opts(rev_x: bool, rev_y: bool, swap: bool) -> GcodeOptions {
        GcodeOptions {
            reverse_x: rev_x,
            reverse_y: rev_y,
            swap_xy: swap,
            page_w: 210.0,
            page_h: 297.0,
            min_step: 0.0,
            ..Default::default()
        }
    }

    /// 反转后整页必须**仍在纸面内**（0..宽 / 0..高），不能只翻个方向就算完。
    #[test]
    fn reverse_maps_page_into_page_bounds() {
        for &swap in &[false, true] {
            for &rx in &[false, true] {
                for &ry in &[false, true] {
                    let o = opts(rx, ry, swap);
                    // 页面宽高（对调后机器轴的范围要跟着换）
                    let (w, h) = if swap { (297.0, 210.0) } else { (210.0, 297.0) };
                    for &(px, py) in &[(0.0, 0.0), (210.0, 0.0), (0.0, 297.0), (210.0, 297.0)] {
                        let (mx, my) = o.transform(px, py);
                        assert!(
                            mx >= -1e-6 && mx <= w + 1e-6 && my >= -1e-6 && my <= h + 1e-6,
                            "swap={swap} rx={rx} ry={ry}: 页面({px},{py}) → 机器({mx},{my}) 超出 0..{w}/0..{h}"
                        );
                    }
                }
            }
        }
    }

    /// 上一版的真实 BUG：对调 XY 后镜像基准仍用 `page_w`/`page_h`，
    /// 页面宽高不等时整页被平移了 `page_h - page_w` 毫米（字跑到纸外）。
    #[test]
    fn swap_xy_uses_swapped_mirror_reference() {
        let o = opts(true, false, true);
        // 页面右上角 (210, 0)：对调后机器 X 装的是页面 Y=0 → 反转 X 得 297；
        // 机器 Y 装的是页面 X=210，没反转 Y 所以还是 210。
        let (mx, my) = o.transform(210.0, 0.0);
        assert!((mx - 297.0).abs() < 1e-6, "机器 X 应为 297，实际 {mx}");
        assert!((my - 210.0).abs() < 1e-6, "机器 Y 应为 210，实际 {my}");
        // 若镜像基准没跟着对调（旧 BUG），机器 X 会变成 210-0=210 —— 整页平移 87mm。
    }

    /// 画布箭头方向必须等于 transform 的实际走向，否则「箭头指这边、机器走那边」。
    ///
    /// 约定：箭头画的是**机器轴在页面上的指向**（页面 +x 向右、+y 向下）。
    #[test]
    fn axis_dirs_match_transform() {
        let cases = [
            (false, false, false),
            (true, false, false),
            (false, true, false),
            (true, true, false),
            (false, false, true),
            (true, false, true),
            (false, true, true),
            (true, true, true),
        ];
        for &(rx, ry, swap) in &cases {
            let o = opts(rx, ry, swap);
            let sx = if rx { -1.0 } else { 1.0 };
            let sy = if ry { -1.0 } else { 1.0 };
            let (xd, yd) = o.axis_dirs();
            if swap {
                // 对调后：机器 X 沿页面的 Y 轴走，机器 Y 沿页面的 X 轴走
                assert_eq!(xd, (0.0, sx), "swap 后 X 箭头错 rx={rx}");
                assert_eq!(yd, (sy, 0.0), "swap 后 Y 箭头错 ry={ry}");
            } else {
                assert_eq!(xd, (sx, 0.0), "X 箭头错 rx={rx}");
                assert_eq!(yd, (0.0, sy), "Y 箭头错 ry={ry}");
            }
            // 两个箭头必须互相垂直（一个沿页面 X、一个沿页面 Y），不能同向重叠
            assert!(
                (xd.0 * yd.0 + xd.1 * yd.1).abs() < 1e-9,
                "XY 箭头应互相垂直：{xd:?} {yd:?}"
            );
        }
    }

    /// 抽出 G-code 里所有 G0/G1 的 X/Y 数值（跳过只动 Z 的抬落笔行）。
    fn xy_values(g: &str) -> Vec<(f64, f64)> {
        let mut out = Vec::new();
        for l in g.lines() {
            let t = l.trim();
            if !(t.starts_with("G0 ") || t.starts_with("G1 ")) {
                continue;
            }
            let mut x = None;
            let mut y = None;
            for tok in t.split_whitespace() {
                if let Some(v) = tok.strip_prefix('X') {
                    x = v.parse::<f64>().ok();
                } else if let Some(v) = tok.strip_prefix('Y') {
                    y = v.parse::<f64>().ok();
                }
            }
            if let (Some(x), Some(y)) = (x, y) {
                out.push((x, y));
            }
        }
        out
    }

    /// BUG #56：G-code 必须以「当前设置的原点」为基准输出 `G92 X0 Y0`，
    /// 否则 `G90` 坐标会被当机器绝对值解析 → 超出机器行程 → 软限位报警。
    #[test]
    fn gcode_anchors_at_current_origin() {
        let paths = vec![Polyline::new(vec![(0.0, 0.0), (100.0, 200.0)])];
        let o = GcodeOptions {
            page_w: 210.0,
            page_h: 297.0,
            min_step: 0.0,
            ..Default::default()
        };
        assert!(o.use_current_origin, "默认应以当前位置为原点");
        let g = generate(&paths, &o);
        assert!(g.contains("G92 X0 Y0"), "默认应输出 G92 锚定当前位置为原点：\n{g}");
        // G92 必须在任何 XY 移动之前，否则第一笔就已经按机器绝对值跑了
        let i92 = g.find("G92 X0 Y0").expect("应含 G92");
        let i_move = g.find("G0 X").or_else(|| g.find("G1 X")).expect("应含移动指令");
        assert!(i92 < i_move, "G92 必须出现在第一个移动指令之前");
    }

    /// 锚定后所有坐标都落在页面范围内（只在当前位置附近走一个页面大小），
    /// 不会出现远超页面的机器绝对值——这是「不超出限制」的直接保证。
    #[test]
    fn anchored_coords_stay_within_page() {
        let paths = vec![Polyline::new(vec![
            (0.0, 0.0),
            (210.0, 0.0),
            (210.0, 297.0),
            (0.0, 297.0),
        ])];
        let o = GcodeOptions {
            page_w: 210.0,
            page_h: 297.0,
            min_step: 0.0,
            ..Default::default()
        };
        let g = generate(&paths, &o);
        let xy = xy_values(&g);
        assert!(!xy.is_empty(), "应有 XY 移动指令");
        for (x, y) in xy {
            assert!(x >= -1e-6 && x <= 210.0 + 1e-6, "X 超出页面范围：{x}");
            assert!(y >= -1e-6 && y <= 297.0 + 1e-6, "Y 超出页面范围：{y}");
        }
    }

    /// EBB 不认 G92（它用 SM 步进指令），不能给它发。
    #[test]
    fn ebb_gets_no_g92() {
        let paths = vec![Polyline::new(vec![(0.0, 0.0), (50.0, 50.0)])];
        let o = GcodeOptions {
            firmware: Firmware::Ebb,
            page_w: 210.0,
            page_h: 297.0,
            min_step: 0.0,
            ..Default::default()
        };
        let g = generate(&paths, &o);
        assert!(!g.contains("G92"), "EBB 不该收到 G92：\n{g}");
    }

    /// 显式关掉时保持原来的机器绝对坐标行为，不破坏既有用法。
    #[test]
    fn use_current_origin_off_keeps_machine_absolute() {
        let paths = vec![Polyline::new(vec![(0.0, 0.0), (50.0, 50.0)])];
        let o = GcodeOptions {
            use_current_origin: false,
            page_w: 210.0,
            page_h: 297.0,
            min_step: 0.0,
            ..Default::default()
        };
        let g = generate(&paths, &o);
        assert!(!g.contains("G92 X0 Y0"), "关掉后不该有 G92：\n{g}");
        assert!(g.contains("回机器原点"), "未锚定时结尾应标注回机器原点");
    }

    /// 把同一组镜像开关连续作用两次必须回到原值（镜像是对合的），
    /// 保证用户来回拨开关不会累积位移。
    #[test]
    fn mirroring_twice_is_identity() {
        for &(rx, ry) in &[(true, false), (false, true), (true, true)] {
            let o = opts(rx, ry, false);
            let (ax, ay) = o.transform(30.0, 40.0);
            let (bx, by) = o.transform(ax, ay);
            assert!(
                (bx - 30.0).abs() < 1e-6 && (by - 40.0).abs() < 1e-6,
                "镜像两次应回到原值：({bx},{by})，rx={rx} ry={ry}"
            );
        }
    }
}
