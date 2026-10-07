//! 设备通信：把 G-code 下发到写字机。
//!
//! 三种传输后端：
//! - `Serial`：真实串口 / **蓝牙 SPP 虚拟串口（COM3、COM4…）/ CH340 USB-TTL**；
//! - `Tcp`：通过 `std::net::TcpStream` 连接 WiFi/网桥模块；
//! - `Mock`：不真正联网，把要下发的 G-code 回显到日志，用于离线验证整条流水线。
//!
//! **关键设计**：所有通信都在「后台线程」中进行，UI 主线程只通过 channel 投递命令、
//! 通过 `Arc<Mutex<DevShared>>` 读取连接状态/日志/运行进度。这样点动、回零、运行 G-code
//! 都不会阻塞 egui 主线程，彻底解决「点一下电动移动 / 归零 程序就卡死」的问题。

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use wrobot_core::serial::SerialPort;

use crate::state::DevKind;

/// 后台线程日志上限（字符数），超出只保留尾部。
const LOG_CAP: usize = 12000;

/// 投给后台线程的命令。
pub enum DevCmd {
    /// 按指定方式连接（addr 形如 `COM3:115200` / `192.168.4.1:23` / `mock`）。
    Connect(DevKind, String),
    /// 自动扫描串口并握手写字机固件后连接。
    AutoConnect,
    /// 断开。
    Disconnect,
    /// 一次性下发一段 G-code（不计入运行进度，仅回显日志）。
    Send(String),
    /// 查询固件信息（`$I` / `M115`）。
    QueryFirmware,
    /// 开始运行整段 G-code（带行级流控与进度回报）。
    StartRun(String),
    /// 暂停运行。
    PauseRun,
    /// 继续运行。
    ResumeRun,
    /// 停止运行。
    StopRun,
    /// 诊断：逐个端口试开，把每个口的错误码与排查建议写进日志。
    Diagnose,
    /// 在指定端口上自动扫描波特率：逐个波特率握手，挑出能收到**可读回应**的那个。
    /// 「机器连上了但完全没反应」八成是波特率不对——不同写字机出厂可能是
    /// 9600 / 38400 / 115200，猜是猜不出来的，只能逐个试。
    AutoBaud(String),
    /// 串口线体检：对**有线**串口从驱动→线材→对端固件逐层测，报告写进日志。
    ///
    /// 端口能打开 ≠ 线是通的。写测试、回显监听、波特率扫描一次做完，
    /// 专门对付「线插上了、端口也在，但机器完全没反应」。
    WireTest(String, u32),
    /// 蓝牙体检：查适配器、写字机在不在、COM 口绑没绑对设备。
    BtReport(String),
    /// 给已配对的写字机启用 SPP 串口服务（系统自动建传出 COM 口），然后重新扫描端口。
    BtBind(String),
    /// 设定「坐标轮询」：查询指令（`?` / `M114` / `QS`）与间隔毫秒（0=关闭）。
    ///
    /// GRBL 只在收到 `?` 时回报一次坐标，不主动推送；想让左栏坐标实时刷新
    /// 就得定时问一句。关闭时传 0，避免空闲时无谓占用链路。
    SetStatusPoll(String, u64),
    /// 观察写字机的可见性时间线（轮数、间隔秒），判断是不是反复掉线。
    BtWatch(String, usize, u64),
}

/// 后台线程向 UI 回报的共享状态（主线程每帧只读）。
#[derive(Default, Clone)]
pub struct DevShared {
    pub connected: bool,
    pub running: bool,
    pub paused: bool,
    pub total_lines: usize,
    pub done_lines: usize,
    pub elapsed_ms: u64,
    pub last_response: String,
    pub log: String,
    /// 请求 UI 切换到某个 tab（如运行开始时跳到「运行中」页）；UI 处理后置 None。
    pub request_tab: Option<usize>,
    /// 是否等待固件回 `ok` 再发下一行。GRBL/Marlin 建议 true；EBB 等不回 ok 的固件设 false（改为定时下发）。
    pub wait_ok: bool,
    /// 运行前是否先发解锁与单位/绝对坐标指令。GRBL 上电常处于 Alarm 锁定态，不解锁则 G0/G1 全部被忽略。
    pub unlock_first: bool,
    /// 等待 `ok` 的超时（毫秒）。超时即继续下行，避免固件不回包时卡住整段运行。
    pub ok_timeout_ms: u64,
    /// **保活心跳间隔**（毫秒，0=关闭）。
    ///
    /// 蓝牙 SPP 模块普遍有「空闲断链」机制：几十秒到几分钟没有数据流就自己断开
    /// （用户遇到的"蓝牙几分钟就断"就是这个）。定期往链路上发一个空行即可保活。
    pub keepalive_ms: u64,
    /// 是否关闭自动重连（默认 false = **开启**自动重连；取反命名只为兼容 `Default`）。
    pub no_auto_reconnect: bool,
    /// 最近一次链路活动（下发指令/收到回显）的时刻，用于判断该不该发心跳。
    pub last_activity: Option<Instant>,
    /// 已发生的自动重连次数（界面显示，便于确认"是不是老在断"）。
    pub reconnects: usize,
    /// 当前连接的地址（形如 `COM3:115200`），诊断时用来跳过「被自己占用」的端口。
    pub cur_addr: String,
    /// 当前连接**实际**使用的波特率（「自动波特率」扫描后会与界面选择值不同）。
    pub cur_baud: u32,
    /// 最近一次蓝牙体检 / 可见性观察的报告全文（界面单独展示，避免被日志刷走）。
    pub bt_report: String,
    /// 请求 UI 重新扫描一次串口（绑定蓝牙串口后新 COM 口才出现）；UI 处理后置 false。
    pub request_scan: bool,
    /// 运行前是否自动把当前位置设为原点（参考项目 `auto_set_zero`）。
    pub auto_set_zero: bool,
    /// 最近一次 G-code 传输的结果摘要（`✓ 已传输 N 行 / M 字节`，失败则是 `✗ …`）。
    ///
    /// 用户抱怨「代码传到机器上没有任何提示」——状态栏会显示这一行，
    /// 让人知道到底发出去了没有、发了多少。
    pub transfer_status: String,

    // ── 坐标回报 ──────────────────────────────────────────
    /// 最近解析到的坐标：机器物理坐标（MPos）/ 当前工件坐标（WPos）。
    pub pos: Position,
    /// 坐标查询指令：`?`（GRBL）/ `M114`（Marlin）/ `QS`（EBB）。
    pub status_query: String,
    /// 坐标轮询间隔（毫秒，0=关闭）。
    pub status_ms: u64,
    /// 上次发出坐标查询的时刻。
    pub last_status: Option<Instant>,
}

/// 一次坐标回报。
///
/// - `m` = **机器物理坐标**（GRBL 的 `MPos`，相对机械原点，G92 偏置不影响它）；
/// - `w` = **当前工件坐标**（GRBL 的 `WPos`，即 `MPos − WCO`，G92 设过零点后它会变）。
///
/// 界面把两者都列出来，「设当前位置为原点」这类操作有没有生效一眼就能看出来。
#[derive(Default, Clone, Copy, PartialEq)]
pub struct Position {
    pub m: Option<[f64; 3]>,
    pub w: Option<[f64; 3]>,
}

impl Position {
    /// 有没有解析到任何坐标。
    pub fn any(&self) -> bool {
        self.m.is_some() || self.w.is_some()
    }
}

/// 从一行固件回显里解析坐标。
///
/// - GRBL：` <Idle|MPos:1.000,2.000,3.000|FS:0,0|WCO:0.000,0.000,0.000>`
///   （有的固件只给 `MPos` + `WCO`，此时 `WPos = MPos − WCO` 自己算）
/// - Marlin `M114`：`X:1.00 Y:2.00 Z:3.00 E:0.00 Count X:100 Y:200 Z:300`
///   （`Count` 后面是步数，不能当坐标，解析时先截掉）
pub fn parse_position(line: &str) -> Position {
    let mut p = Position::default();
    p.m = axis_list(line, "MPos:");
    p.w = axis_list(line, "WPos:");
    if p.w.is_none() {
        if let (Some(m), Some(wco)) = (p.m, axis_list(line, "WCO:")) {
            p.w = Some([m[0] - wco[0], m[1] - wco[1], m[2] - wco[2]]);
        }
    }
    if p.m.is_none() && p.w.is_none() {
        // Marlin：只有一组坐标，按「工件坐标」展示（M114 报的是逻辑位置）
        p.w = marlin_xyz(line);
    }
    p
}

/// 取 `key` 后面 `a,b,c` 形式的三个数（GRBL 的 `MPos:` / `WPos:` / `WCO:`）。
fn axis_list(line: &str, key: &str) -> Option<[f64; 3]> {
    let at = line.find(key)? + key.len();
    let rest = &line[at..];
    let end = rest.find(['|', '>', '\r', '\n']).unwrap_or(rest.len());
    let mut it = rest[..end].split(',').map(|s| s.trim().parse::<f64>().ok());
    let a = it.next()??;
    let b = it.next()??;
    let c = it.next()??;
    Some([a, b, c])
}

/// Marlin `M114`：`X:1.00 Y:2.00 Z:3.00 …`（`Count` 之后的步数不参与解析）。
fn marlin_xyz(line: &str) -> Option<[f64; 3]> {
    let up = line.to_uppercase();
    let cut = up.find("COUNT").unwrap_or(line.len());
    let head = &line[..cut];
    let mut out = [0.0_f64; 3];
    let mut got = 0usize;
    for (i, ax) in ["X", "Y", "Z"].iter().enumerate() {
        let key = format!("{ax}:");
        let Some(at) = head.find(&key) else { continue };
        let rest = &head[at + key.len()..];
        let end = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))
            .unwrap_or(rest.len());
        if let Ok(v) = rest[..end].parse::<f64>() {
            out[i] = v;
            got += 1;
        }
    }
    if got == 3 {
        Some(out)
    } else {
        None
    }
}

/// 待下发内容的字节数（UTF-8 长度，用于传输摘要）。

/// 后台线程持有的传输后端。
pub trait Transport: Send {
    fn connect(&mut self, addr: &str) -> anyhow::Result<()>;
    fn disconnect(&mut self);
    fn send(&mut self, data: &str) -> anyhow::Result<()>;
    fn read_line(&mut self, timeout_ms: u64) -> Option<String>;
    fn is_connected(&self) -> bool;
    /// 连接过程中的降级说明（如驱动拒绝设置波特率）。默认无。
    fn notes(&self) -> String {
        String::new()
    }
}

/// TCP 后端（WiFi/网桥）。
struct TcpTransport {
    stream: Option<TcpStream>,
}
impl TcpTransport {
    fn new() -> Self {
        TcpTransport { stream: None }
    }
}
impl Transport for TcpTransport {
    fn connect(&mut self, addr: &str) -> anyhow::Result<()> {
        let s = TcpStream::connect(addr)?;
        s.set_read_timeout(Some(Duration::from_millis(200)))?;
        s.set_write_timeout(Some(Duration::from_millis(2000)))?;
        self.stream = Some(s);
        Ok(())
    }
    fn disconnect(&mut self) {
        self.stream = None;
    }
    fn send(&mut self, data: &str) -> anyhow::Result<()> {
        if let Some(s) = self.stream.as_mut() {
            s.write_all(data.as_bytes())?;
            s.flush()?;
        }
        Ok(())
    }
    fn read_line(&mut self, _timeout_ms: u64) -> Option<String> {
        if let Some(s) = self.stream.as_mut() {
            let mut buf = [0u8; 256];
            if let Ok(n) = s.read(&mut buf) {
                if n > 0 {
                    return Some(String::from_utf8_lossy(&buf[..n]).trim().to_string());
                }
            }
        }
        None
    }
    fn is_connected(&self) -> bool {
        self.stream.is_some()
    }
}

/// 串口后端（蓝牙 SPP / USB-TTL）。
struct SerialTransport {
    port: Option<SerialPort>,
    notes: String,
}
impl SerialTransport {
    fn new() -> Self {
        SerialTransport {
            port: None,
            notes: String::new(),
        }
    }
    fn parse(addr: &str) -> (String, u32) {
        match addr.split_once(':') {
            Some((p, b)) => (p.trim().to_string(), b.trim().parse::<u32>().unwrap_or(115200)),
            None => (addr.trim().to_string(), 115200),
        }
    }
}
impl Transport for SerialTransport {
    fn connect(&mut self, addr: &str) -> anyhow::Result<()> {
        let (p, b) = Self::parse(addr);
        let port = SerialPort::open(&p, b)?;
        self.notes = port.warnings().to_string();
        if port.actual_baud() != 0 && port.actual_baud() != b {
            self.notes.push_str(&format!(
                "驱动实际波特率 {}（请求 {}）；",
                port.actual_baud(),
                b
            ));
        }
        self.port = Some(port);
        Ok(())
    }
    fn disconnect(&mut self) {
        self.port = None;
    }
    fn send(&mut self, data: &str) -> anyhow::Result<()> {
        match self.port.as_mut() {
            Some(p) => p.write_all(data.as_bytes()).map_err(|e| anyhow::anyhow!(e.to_string())),
            None => Ok(()),
        }
    }
    fn read_line(&mut self, _timeout_ms: u64) -> Option<String> {
        self.port.as_mut()?.read_line(400)
    }
    fn is_connected(&self) -> bool {
        self.port.is_some()
    }
    fn notes(&self) -> String {
        self.notes.clone()
    }
}

/// 离线模拟后端：不下发真实指令，**把所有该发的 G-code 完整回显到日志**，
/// 并像真机一样逐行回 `ok`，用于在没有机器时验证整条流水线。
struct MockTransport {
    connected: bool,
    sent_lines: usize,
}
impl Transport for MockTransport {
    fn connect(&mut self, _addr: &str) -> anyhow::Result<()> {
        self.connected = true;
        self.sent_lines = 0;
        Ok(())
    }
    fn disconnect(&mut self) {
        self.connected = false;
    }
    fn send(&mut self, data: &str) -> anyhow::Result<()> {
        self.sent_lines += data.lines().filter(|l| !l.trim().is_empty()).count();
        // 稍微延迟，让「运行中」页的进度条能看得见（真机有运动时间）
        thread::sleep(Duration::from_millis(12));
        Ok(())
    }
    fn read_line(&mut self, _timeout_ms: u64) -> Option<String> {
        if self.connected {
            Some("ok".to_string())
        } else {
            None
        }
    }
    fn is_connected(&self) -> bool {
        self.connected
    }
    fn notes(&self) -> String {
        format!("模拟模式：已下发 {} 行（不会真的动机器）", self.sent_lines)
    }
}

/// 设备控制器：持有一个后台线程。
pub struct DeviceController {
    cmd_tx: Sender<DevCmd>,
    pub shared: Arc<Mutex<DevShared>>,
}

impl Default for DeviceController {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceController {
    pub fn new() -> Self {
        let (tx, rx) = channel::<DevCmd>();
        let shared = Arc::new(Mutex::new(DevShared {
            wait_ok: true,
            unlock_first: true,
            ok_timeout_ms: 1200,
            // 蓝牙/串口默认 5 秒一次保活心跳：写字机的蓝牙模块空闲几分钟就断链，
            // 持续有数据流就能稳住。设为 0 可关闭。
            keepalive_ms: 5000,
            ..Default::default()
        }));
        let sh = shared.clone();
        thread::spawn(move || thread_main(rx, sh));
        DeviceController {
            cmd_tx: tx,
            shared,
        }
    }

    /// 投递一条命令（非阻塞，立即返回）。
    pub fn send(&self, cmd: DevCmd) {
        let _ = self.cmd_tx.send(cmd);
    }

    /// 当前是否连接（读取共享状态）。
    pub fn is_connected(&self) -> bool {
        self.shared.lock().map(|s| s.connected).unwrap_or(false)
    }

    /// 取一份日志快照（用于初始加载等）。
    pub fn log_snapshot(&self) -> String {
        self.shared.lock().map(|s| s.log.clone()).unwrap_or_default()
    }

    /// 取一份共享状态快照（用于运行进度、连接状态等显示）。
    pub fn snapshot(&self) -> DevShared {
        self.shared.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// 取走「请求切换 tab」的请求（处理后置空，避免反复跳转）。
    pub fn take_request_tab(&self) -> Option<usize> {
        self.shared.lock().ok().and_then(|mut s| s.request_tab.take())
    }

    /// 取走「请求重新扫描串口」（绑定蓝牙串口后新口才出现，UI 收到就扫一次）。
    pub fn take_request_scan(&self) -> bool {
        self.shared
            .lock()
            .ok()
            .map(|mut s| std::mem::replace(&mut s.request_scan, false))
            .unwrap_or(false)
    }

    /// 取最近一次蓝牙体检 / 可见性观察报告。
    pub fn bt_report_snapshot(&self) -> String {
        self.shared.lock().map(|s| s.bt_report.clone()).unwrap_or_default()
    }

    /// 在指定端口上自动扫描波特率（后台线程执行，不阻塞 UI）。
    pub fn auto_baud(&self, port: String) {
        self.send(DevCmd::AutoBaud(port));
    }

    /// 设置运行流控：是否等待 `ok`、运行前是否解锁、`ok` 超时(ms)。
    pub fn set_flow(&self, wait_ok: bool, unlock_first: bool, ok_timeout_ms: u64) {
        if let Ok(mut s) = self.shared.lock() {
            s.wait_ok = wait_ok;
            s.unlock_first = unlock_first;
            s.ok_timeout_ms = ok_timeout_ms;
        }
    }

    /// 设置运行前是否自动设原点（G92 X0 Y0）。
    pub fn set_auto_zero(&self, on: bool) {
        if let Ok(mut s) = self.shared.lock() {
            s.auto_set_zero = on;
        }
    }

    /// 设置链路保活：心跳间隔(ms，0=关闭) 与 是否自动重连。
    pub fn set_keepalive(&self, interval_ms: u64, auto_reconnect: bool) {
        if let Ok(mut s) = self.shared.lock() {
            s.keepalive_ms = interval_ms;
            s.no_auto_reconnect = !auto_reconnect;
        }
    }

    /// 清空日志。
    pub fn clear_log(&self) {
        if let Ok(mut s) = self.shared.lock() {
            s.log.clear();
        }
    }
}

/// 在日志末尾追加并裁剪。
fn append_log(log: &mut String, line: &str) {
    log.push_str(line);
    if !line.ends_with('\n') {
        log.push('\n');
    }
    if log.len() > LOG_CAP {
        let drop = log.len() - LOG_CAP;
        if let Some(pos) = log.char_indices().nth(drop) {
            *log = log[pos.0..].to_string();
        }
    }
}

/// 后台线程主循环。
fn thread_main(rx: Receiver<DevCmd>, shared: Arc<Mutex<DevShared>>) {
    let mut transport: Option<Box<dyn Transport>> = None;
    let mut run_lines: Vec<String> = Vec::new();
    let mut run_idx: usize = 0;
    let mut run_start: Option<Instant> = None;

    loop {
        // —— 运行态：逐行流控 + 监听控制命令 ——
        let running = shared.lock().map(|s| s.running).unwrap_or(false);
        if running {
            // 非阻塞地消费暂停/停止/继续等控制命令
            while let Ok(cmd) = rx.try_recv() {
                match cmd {
                    DevCmd::PauseRun => {
                        if let Ok(mut s) = shared.lock() {
                            s.paused = true;
                        }
                    }
                    DevCmd::ResumeRun => {
                        if let Ok(mut s) = shared.lock() {
                            s.paused = false;
                        }
                    }
                    DevCmd::StopRun => {
                        if let Ok(mut s) = shared.lock() {
                            s.running = false;
                            s.paused = false;
                        }
                        run_lines.clear();
                    }
                    DevCmd::Disconnect => {
                        if let Some(t) = transport.as_mut() {
                            t.disconnect();
                        }
                        transport = None;
                        if let Ok(mut s) = shared.lock() {
                            s.connected = false;
                            s.running = false;
                        }
                    }
                    _ => {}
                }
            }

            let paused = shared.lock().map(|s| s.paused).unwrap_or(false);
            if paused {
                thread::sleep(Duration::from_millis(60));
                continue;
            }

            if run_idx < run_lines.len() {
                let line = run_lines[run_idx].clone();
                if !line.trim().is_empty() {
                    send_line(&mut transport, &line, &shared);
                }
                run_idx += 1;
                if let Ok(mut s) = shared.lock() {
                    s.done_lines = run_idx;
                    s.elapsed_ms = run_start.map(|t| t.elapsed().as_millis() as u64).unwrap_or(0);
                    if run_idx >= run_lines.len() {
                        s.running = false;
                        append_log(&mut s.log, "● 运行结束");
                    }
                }
            } else {
                if let Ok(mut s) = shared.lock() {
                    s.running = false;
                }
            }
            thread::sleep(Duration::from_millis(if run_lines.get(run_idx.wrapping_sub(1)).map(|l| l.trim().starts_with('G')).unwrap_or(false) { 0 } else { 8 }));
            continue;
        }

        // —— 非运行态：等待命令（**带超时**，好让空闲时能定时发保活心跳）——
        let cmd = match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(c) => Some(c),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break, // 通道关闭，线程退出
        };
        let Some(cmd) = cmd else {
            keepalive_tick(&mut transport, &shared);
            status_tick(&mut transport, &shared);
            continue;
        };
        match cmd {
            DevCmd::Connect(kind, addr) => {
                // 关键：先释放上一个连接再开新的。
                // 同一个 COM 口不能被打开两次——用户再点一次「连接」时，
                // 旧 transport 仍握着句柄，新连接就会被自己挡在门外，报「错误码 5 被占用」。
                if transport.is_some() {
                    if let Some(t) = transport.as_mut() {
                        t.disconnect();
                    }
                    transport = None;
                    if let Ok(mut s) = shared.lock() {
                        s.connected = false;
                        s.cur_addr.clear();
                        append_log(&mut s.log, "● 已释放上一个连接，重新连接…\n");
                    }
                }
                let mut t: Box<dyn Transport> = match kind {
                    DevKind::Tcp => Box::new(TcpTransport::new()),
                    DevKind::Mock => Box::new(MockTransport { connected: false, sent_lines: 0 }),
                    DevKind::Serial => Box::new(SerialTransport::new()),
                };
                match t.connect(&addr) {
                    Ok(()) => {
                        let ok = t.is_connected();
                        let notes = t.notes();
                        transport = Some(t);
                        if let Ok(mut s) = shared.lock() {
                            s.connected = ok;
                            s.cur_addr = if ok { addr.clone() } else { String::new() };
                            s.cur_baud = if ok { SerialTransport::parse(&addr).1 } else { 0 };
                            let head = match kind {
                                DevKind::Mock => "● 模拟模式已启动（不下发真实指令，G-code 会完整回显到日志）\n",
                                _ => "",
                            };
                            if !head.is_empty() {
                                append_log(&mut s.log, head);
                            }
                            append_log(&mut s.log, &format!("● 已连接 {} ({:?})\n", addr, kind));
                            if !notes.is_empty() {
                                append_log(&mut s.log, &format!("⚠ {}\n", notes));
                            }
                            if !ok {
                                append_log(&mut s.log, "⚠ 后端报告未连接：请点「诊断端口」看每步错误码\n");
                            }
                        }
                        // 连接后立刻读一次：GRBL 上电会主动打出
                        // "Grbl 1.1h ['$' for help]"——这是「链路真的通了」最有力的证据。
                        if ok && !matches!(kind, DevKind::Mock) {
                            let mut got = String::new();
                            for _ in 0..4 {
                                if let Some(l) = transport.as_mut().and_then(|t| t.read_line(350)) {
                                    got.push_str(&l);
                                    got.push(' ');
                                }
                            }
                            if let Ok(mut s) = shared.lock() {
                                let g = got.trim();
                                if g.is_empty() {
                                    append_log(&mut s.log, "· 端口已打开，但没收到开机欢迎语（部分固件不主动打招呼）——到「测试」页发一条指令看有没有 ← 回显\n");
                                } else {
                                    append_log(&mut s.log, &format!("← 收到：{}\n", g));
                                    let ratio = wrobot_core::garbage_ratio(g);
                                    if ratio > 0.35 {
                                        append_log(
                                            &mut s.log,
                                            &format!("⚠ 其中 {:.0}% 的字节不可读 → **波特率不匹配**（收到的是乱码）。点「自动波特率」让它自己找。\n", ratio * 100.0),
                                        );
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        if let Ok(mut s) = shared.lock() {
                            s.connected = false;
                            append_log(&mut s.log, &format!("✗ 连接失败：{}\n", e));
                        }
                    }
                }
            }
            DevCmd::AutoConnect => {
                auto_connect(&mut transport, &shared);
            }
            DevCmd::Diagnose => {
                diagnose(&shared);
            }
            DevCmd::AutoBaud(port) => {
                auto_baud(&port, &mut transport, &shared);
            }
            DevCmd::WireTest(port, baud) => {
                wire_test_cmd(&port, baud, &mut transport, &shared);
            }
            DevCmd::BtReport(name) => {
                if let Ok(mut s) = shared.lock() {
                    append_log(&mut s.log, &format!("{}\n", wrobot_core::bt_report(&name)));
                    s.bt_report = wrobot_core::bt_report(&name);
                }
            }
            DevCmd::BtBind(name) => {
                match wrobot_core::bt_set_spp(&name, true) {
                    Ok(msg) => {
                        if let Ok(mut s) = shared.lock() {
                            append_log(&mut s.log, &format!("✓ {}\n", msg));
                        }
                        // 给系统一点时间把 COM 口建出来，再让 UI 重新扫描端口表
                        thread::sleep(Duration::from_millis(2000));
                        if let Ok(mut s) = shared.lock() {
                            s.request_scan = true;
                        }
                    }
                    Err(e) => {
                        if let Ok(mut s) = shared.lock() {
                            append_log(&mut s.log, &format!("✗ {}\n", e));
                        }
                    }
                }
            }
            DevCmd::BtWatch(name, rounds, interval) => {
                let report = wrobot_core::bt_watch(&name, rounds, interval);
                if let Ok(mut s) = shared.lock() {
                    s.bt_report = report.clone();
                    append_log(&mut s.log, &format!("{}\n", report));
                }
            }
            DevCmd::SetStatusPoll(query, ms) => {
                if let Ok(mut s) = shared.lock() {
                    s.status_query = query;
                    s.status_ms = ms;
                    s.last_status = None;
                }
            }
            DevCmd::Disconnect => {
                if let Some(t) = transport.as_mut() {
                    t.disconnect();
                }
                transport = None;
                if let Ok(mut s) = shared.lock() {
                    s.connected = false;
                    s.cur_addr.clear();
                    s.cur_baud = 0;
                    s.pos = Position::default();
                    append_log(&mut s.log, "● 已断开\n");
                }
            }
            DevCmd::Send(gcode) => {
                send_gcode_block(transport.as_mut(), &gcode, &shared);
            }
            DevCmd::QueryFirmware => {
                query_firmware(transport.as_mut(), &shared);
            }
            DevCmd::StartRun(gcode) => {
                run_lines = gcode
                    .lines()
                    .map(|l| l.trim_end().to_string())
                    .filter(|l| !l.is_empty())
                    .collect();
                run_idx = 0;
                run_start = Some(Instant::now());
                // 运行前解锁 / 设单位与坐标模式（GRBL 上电常为 Alarm 锁定态，不解锁则不动）
                let unlock = shared.lock().map(|s| s.unlock_first).unwrap_or(true);
                if unlock {
                    send_unlock(&mut transport, &shared);
                }
                if let Ok(mut s) = shared.lock() {
                    s.total_lines = run_lines.len();
                    s.done_lines = 0;
                    s.elapsed_ms = 0;
                    s.running = true;
                    s.paused = false;
                    s.request_tab = Some(9); // 跳到「运行中」页
                    append_log(&mut s.log, &format!("● 开始运行（{} 行）\n", run_lines.len()));
                }
            }
            DevCmd::PauseRun | DevCmd::ResumeRun | DevCmd::StopRun => {
                // 非运行态收到这些命令，忽略
            }
        }
    }
}

/// **保活心跳 + 断链自动重连**（后台线程空闲时每 500ms 调一次）。
///
/// 为什么需要：写字机那端的蓝牙 SPP 模块普遍有「空闲断链」机制——几十秒到几分钟
/// 没有数据流就自己断开，用户看到的就是「蓝牙几分钟就断了，跟电脑连不稳」。
/// 定期往链路上发一个空行（GRBL 会回一个 `ok`，Marlin 不回也不报错）就能一直保持住。
/// 若心跳发不出去，说明链路已经断了，立刻自动重连，并把次数记进状态供界面显示。
fn keepalive_tick(transport: &mut Option<Box<dyn Transport>>, shared: &Arc<Mutex<DevShared>>) {
    let (ms, auto_re, connected, addr, last) = match shared.lock() {
        Ok(s) => (
            s.keepalive_ms,
            !s.no_auto_reconnect,
            s.connected,
            s.cur_addr.clone(),
            s.last_activity,
        ),
        Err(_) => return,
    };
    if ms == 0 || !connected || addr.is_empty() {
        return;
    }
    let due = match last {
        Some(t) => t.elapsed() >= Duration::from_millis(ms),
        None => true,
    };
    if !due {
        return;
    }
    if let Ok(mut s) = shared.lock() {
        s.last_activity = Some(Instant::now());
    }

    let mut alive = false;
    if let Some(t) = transport.as_mut() {
        match t.send("\n") {
            Ok(()) => {
                alive = true;
                // 心跳回显不刷屏：只有非空且不是 ok 的回应才记一笔
                if let Some(l) = t.read_line(150) {
                    let l = l.trim();
                    if !l.is_empty() && !l.eq_ignore_ascii_case("ok") {
                        if let Ok(mut s) = shared.lock() {
                            append_log(&mut s.log, &format!("← （心跳回应）{}\n", l));
                        }
                    }
                }
            }
            Err(e) => {
                if let Ok(mut s) = shared.lock() {
                    append_log(&mut s.log, &format!("✗ 心跳发送失败：{}\n", e));
                }
            }
        }
    }
    if alive {
        return;
    }
    if !auto_re {
        if let Ok(mut s) = shared.lock() {
            append_log(&mut s.log, "⚠ 链路已断开（自动重连已关闭），请手动点「连接」\n");
        }
        return;
    }
    if let Ok(mut s) = shared.lock() {
        append_log(&mut s.log, "● 链路中断，正在自动重连…\n");
    }
    if reconnect_serial(transport, &addr, shared) {
        if let Ok(mut s) = shared.lock() {
            s.reconnects += 1;
            let n = s.reconnects;
            append_log(&mut s.log, &format!("✓ 已自动重连 {}（第 {} 次）\n", addr, n));
        }
    } else if let Ok(mut s) = shared.lock() {
        append_log(
            &mut s.log,
            &format!(
                "✗ 自动重连 {} 失败：写字机通电了吗？蓝牙还在范围内吗？\n",
                addr
            ),
        );
    }
}

/// 定时问一句坐标（GRBL 不主动推送，只有收到 `?` 才回报一次）。
///
/// 只在「已连接 + 空闲 + 开了轮询」时发，运行中绝不插队，免得打断流控。
fn status_tick(transport: &mut Option<Box<dyn Transport>>, shared: &Arc<Mutex<DevShared>>) {
    let (ms, connected, running, q, last) = match shared.lock() {
        Ok(s) => (
            s.status_ms,
            s.connected,
            s.running,
            s.status_query.clone(),
            s.last_status,
        ),
        Err(_) => return,
    };
    if ms == 0 || !connected || running || q.is_empty() {
        return;
    }
    let due = match last {
        Some(t) => t.elapsed() >= Duration::from_millis(ms),
        None => true,
    };
    if !due {
        return;
    }
    if let Ok(mut s) = shared.lock() {
        s.last_status = Some(Instant::now());
    }
    let Some(t) = transport.as_mut() else {
        return;
    };
    if t.send(&format!("{}\n", q)).is_err() {
        return;
    }
    // 状态回报一般紧跟在查询后面，读几行把它捞出来（不要再打进日志刷屏）
    for _ in 0..3 {
        match t.read_line(150) {
            Some(r) if !r.trim().is_empty() => {
                let pos = parse_position(&r);
                if let Ok(mut s) = shared.lock() {
                    if pos.any() {
                        s.pos = pos;
                    }
                    s.last_response = r;
                    s.last_activity = Some(Instant::now());
                }
                if pos.any() {
                    break;
                }
            }
            Some(_) => {}
            None => break,
        }
    }
}

/// 重开串口连接（沿用原地址与波特率），成功返回 true。
fn reconnect_serial(
    transport: &mut Option<Box<dyn Transport>>,
    addr: &str,
    shared: &Arc<Mutex<DevShared>>,
) -> bool {
    if let Some(t) = transport.as_mut() {
        t.disconnect();
    }
    *transport = None;
    if let Ok(mut s) = shared.lock() {
        s.connected = false;
    }
    thread::sleep(Duration::from_millis(400));
    let mut nt = SerialTransport::new();
    match nt.connect(addr) {
        Ok(()) => {
            let notes = nt.notes();
            let ok = nt.is_connected();
            *transport = Some(Box::new(nt));
            if let Ok(mut s) = shared.lock() {
                s.connected = ok;
                if ok {
                    s.cur_addr = addr.to_string();
                    s.cur_baud = SerialTransport::parse(addr).1;
                    s.last_activity = Some(Instant::now());
                }
                if !notes.is_empty() {
                    append_log(&mut s.log, &format!("⚠ {}\n", notes));
                }
            }
            ok
        }
        Err(e) => {
            if let Ok(mut s) = shared.lock() {
                append_log(&mut s.log, &format!("✗ 重连失败：{}\n", e));
            }
            false
        }
    }
}

/// 发送一行并等待 `ok`（带超时），把回显写入日志。
///
/// **关键**：G-code 行必须以 `\n`（或 `\r\n`）结尾，GRBL/Marlin 才认为这一行结束并执行。
/// 之前直接发送不带换行的行，固件会一直把字节缓存在行缓冲里等待终止符，
/// 表现为「程序在跑、进度在涨，但机器一动不动」。
///
/// 发送失败（典型是「写入 0 字节」）时会自动**断开重连一次再重发**——
/// USB-TTL 半掉线 / 蓝牙链路闪断时，重开句柄往往就能恢复，省得用户手动点。
fn send_line(transport: &mut Option<Box<dyn Transport>>, line: &str, shared: &Arc<Mutex<DevShared>>) {
    let line = line.trim_end();
    if line.is_empty() {
        return;
    }
    let payload = format!("{}\n", line);
    let addr = shared.lock().map(|s| s.cur_addr.clone()).unwrap_or_default();
    let mut sent = false;
    if let Some(t) = transport.as_mut() {
        match t.send(&payload) {
            Ok(()) => {
                sent = true;
                if let Ok(mut s) = shared.lock() {
                    append_log(&mut s.log, &format!("→ {}\n", line));
                    s.last_activity = Some(Instant::now());
                }
            }
            Err(e) => {
                if let Ok(mut s) = shared.lock() {
                    append_log(&mut s.log, &format!("✗ 发送失败：{}\n", e));
                }
            }
        }
    } else {
        if let Ok(mut s) = shared.lock() {
            append_log(&mut s.log, &format!("（未连接）跳过：{}\n", line));
        }
        return;
    }

    // 失败自愈：重开一次端口再发（仅限串口，且必须知道地址）
    if !sent && !addr.is_empty() && addr.to_uppercase().contains("COM") {
        if let Ok(mut s) = shared.lock() {
            append_log(&mut s.log, "● 尝试自动重连并重发…\n");
        }
        if let Some(t) = transport.as_mut() {
            t.disconnect();
        }
        *transport = None;
        thread::sleep(Duration::from_millis(300));
        let mut nt = SerialTransport::new();
        match nt.connect(&addr) {
            Ok(()) => {
                let notes = nt.notes();
                *transport = Some(Box::new(nt));
                if let Some(t) = transport.as_mut() {
                    match t.send(&payload) {
                        Ok(()) => {
                            sent = true;
                            if let Ok(mut s) = shared.lock() {
                                s.connected = true;
                                append_log(&mut s.log, "✓ 已重连，指令已补发\n");
                                append_log(&mut s.log, &format!("→ {}\n", line));
                                if !notes.is_empty() {
                                    append_log(&mut s.log, &format!("⚠ {}\n", notes));
                                }
                            }
                        }
                        Err(e) => {
                            if let Ok(mut s) = shared.lock() {
                                s.connected = false;
                                append_log(&mut s.log, &format!("✗ 重连后仍发送失败：{}\n", e));
                            }
                        }
                    }
                }
            }
            Err(e) => {
                if let Ok(mut s) = shared.lock() {
                    s.connected = false;
                    append_log(&mut s.log, &format!("✗ 重连失败：{}\n", e));
                }
            }
        }
    }
    if !sent {
        return;
    }

    let t = match transport.as_mut() {
        Some(t) => t,
        None => return,
    };

    let (wait_ok, timeout) = shared
        .lock()
        .map(|s| (s.wait_ok, s.ok_timeout_ms))
        .unwrap_or((true, 1200));
    if !wait_ok {
        // 不等待 ok：按固定间隔下发
        thread::sleep(Duration::from_millis(15));
        return;
    }

    // 等待固件回 `ok`（默认最多约 1.2 秒，超时继续，避免整段卡死）
    let deadline = Instant::now() + Duration::from_millis(timeout.max(200));
    while Instant::now() < deadline {
        let left = (deadline - Instant::now()).as_millis().min(400).max(50) as u64;
        match t.read_line(left) {
            Some(r) if !r.trim().is_empty() => {
                // 状态回报（`<Idle|MPos:…|WPos:…>` / `X:… Y:… Z:…`）顺手解析成坐标，
                // 左栏的「坐标」区就能实时显示机器在哪。
                let pos = parse_position(&r);
                if let Ok(mut s) = shared.lock() {
                    if pos.any() {
                        s.pos = pos;
                    }
                    append_log(&mut s.log, &format!("← {}\n", r));
                    s.last_response = r.clone();
                }
                if r.contains("ok") || r.contains("OK") {
                    return;
                }
                // 报错信息（如 error:9）也立即返回，让 UI 能看到
                if r.to_lowercase().contains("error") || r.to_lowercase().contains("alarm") {
                    return;
                }
            }
            Some(_) => {}
            None => break, // 读超时，退出等待
        }
    }
}

/// 运行前的准备指令：解锁 + 毫米 + 绝对坐标。
///
/// GRBL 上电/复位后常处于 `ALARM` 锁定态，此时所有 G0/G1 都会被忽略，
/// 必须先发 `$X` 解锁，否则「程序跑完了机器也没动」。
fn send_unlock(transport: &mut Option<Box<dyn Transport>>, shared: &Arc<Mutex<DevShared>>) {
    // `M17` 是给 Marlin 用的：Marlin 上电后步进电机默认是**失能**状态，
    // 不发 M17 的话 G0/G1 会被直接忽略——「程序跑完了机器一动不动」的经典原因之一。
    // GRBL 遇到它只会回一个 error（不影响后续指令），所以无条件发是安全的。
    for pre in [
        "$X ; 解锁(解除Alarm锁定)",
        "G21 ; 毫米",
        "G90 ; 绝对坐标",
        "M17 ; 使能步进电机(Marlin)",
    ] {
        send_line(transport, pre, shared);
    }
    // 参考项目 auto_set_zero=true：开机默认把当前位置设为原点，否则机器会按上次的
    // 机械坐标跑，第一笔就可能撞到限位。
    let set_zero = shared.lock().map(|s| s.auto_set_zero).unwrap_or(true);
    if set_zero {
        send_line(transport, "G92 X0 Y0 ; 设当前位置为原点", shared);
    }
}

/// 一次性下发（不计入进度）：写全部行，读少量回传。
fn send_gcode_block(transport: Option<&mut Box<dyn Transport>>, gcode: &str, shared: &Arc<Mutex<DevShared>>) {
    let Some(t) = transport else {
        if let Ok(mut s) = shared.lock() {
            append_log(&mut s.log, "（未连接）**没有发出去**，仅记录待下发内容：\n");
            for l in gcode.lines().take(6) {
                append_log(&mut s.log, &format!("   {}\n", l));
            }
            s.transfer_status = "✗ 未连接：G-code 没有发出去，请先点「连接」".to_string();
        }
        return;
    };
    let lines: Vec<&str> = gcode.lines().filter(|l| !l.trim().is_empty()).collect();
    let n = lines.len();
    // 单行指令（点动/回零/单步）不带换行，必须补上，否则固件不执行
    let payload = if gcode.ends_with('\n') {
        gcode.to_string()
    } else {
        format!("{}\n", gcode)
    };
    let bytes = payload.len();
    let started = Instant::now();
    if let Ok(mut s) = shared.lock() {
        s.transfer_status = format!("正在下发 {} 行…", n);
        append_log(&mut s.log, &format!("● 开始下发 {} 行（{} 字节）\n", n, bytes));
    }
    if let Err(e) = t.send(&payload) {
        if let Ok(mut s) = shared.lock() {
            append_log(&mut s.log, &format!("✗ 发送失败：{}\n", e));
            append_log(
                &mut s.log,
                "  → 链路断了：点「连接」重连一次；仍失败就点「诊断端口」看错误码。\n",
            );
            s.transfer_status = format!("✗ 下发失败（0/{} 行）", n);
        }
        return;
    }
    // 逐行回显到底发了什么——以前只写一句「已下发 N 行」，
    // 模拟模式下什么都看不到，用户根本无法确认指令对不对。
    if let Ok(mut s) = shared.lock() {
        const MAX_SHOW: usize = 40;
        for l in lines.iter().take(MAX_SHOW) {
            append_log(&mut s.log, &format!("→ {}\n", l));
        }
        if n > MAX_SHOW {
            append_log(&mut s.log, &format!("→ …（其余 {} 行略）\n", n - MAX_SHOW));
        }
        let secs = started.elapsed().as_secs_f32();
        append_log(
            &mut s.log,
            &format!("✓ 已下发 {} 行 / {} 字节，耗时 {:.1}s\n", n, bytes, secs),
        );
        s.transfer_status = format!("✓ 已传输 {} 行 / {} 字节（{:.1}s）", n, bytes, secs);
    }
    for _ in 0..3 {
        if let Some(r) = t.read_line(300) {
            if !r.is_empty() {
                if let Ok(mut s) = shared.lock() {
                    append_log(&mut s.log, &format!("← {}\n", r));
                }
            }
        }
    }
}

/// 查询固件信息。
fn query_firmware(transport: Option<&mut Box<dyn Transport>>, shared: &Arc<Mutex<DevShared>>) {
    let Some(t) = transport else {
        if let Ok(mut s) = shared.lock() {
            append_log(&mut s.log, "（未连接）无法查询固件\n");
        }
        return;
    };
    // GRBL 用 `$I`，Marlin 用 `M115`
    let cmds = ["$I", "M115"];
    for c in cmds {
        let _ = t.send(&format!("{}\n", c));
        if let Ok(mut s) = shared.lock() {
            append_log(&mut s.log, &format!("→ {}\n", c));
        }
        for _ in 0..4 {
            if let Some(r) = t.read_line(400) {
                if !r.is_empty() {
                    if let Ok(mut s) = shared.lock() {
                        append_log(&mut s.log, &format!("← {}\n", r));
                        s.last_response = r.clone();
                    }
                }
            }
        }
    }
}

/// 候选端口：**能打开的排在最前面**，打不开的兜底放后面。
///
/// 蓝牙串口常常成对出现（一个传出、一个传入），传不出去的那个打不开。
/// 若按口号顺序先试它，每次都要等满超时，用户感觉就是"扫半天没反应"。
/// 先试能打开的口，命中率与速度都明显更好。
fn candidate_ports() -> Vec<String> {
    let mut detailed = wrobot_core::scan_ports_detailed();
    if detailed.is_empty() {
        detailed = (1..=32u32).map(|i| (format!("COM{}", i), false)).collect();
    }
    let mut online: Vec<String> = Vec::new();
    let mut offline: Vec<String> = Vec::new();
    for (p, ok) in detailed {
        if ok {
            online.push(p);
        } else {
            offline.push(p);
        }
    }
    online.extend(offline);
    online
}

/// 两轮波特率：先试最常见的两种，全部失败再做全量扫描（避免用户干等）。
const FAST_BAUDS: &[u32] = &[115200, 9600];

/// 自动扫描串口并握手写字机固件后连接（在后台线程执行，不阻塞 UI）。
fn auto_connect(transport: &mut Option<Box<dyn Transport>>, shared: &Arc<Mutex<DevShared>>) {
    // 先释放已有连接：否则探测当前已占用的端口时会被自己挡住（错误码 5）
    if transport.is_some() {
        if let Some(t) = transport.as_mut() {
            t.disconnect();
        }
        *transport = None;
        if let Ok(mut s) = shared.lock() {
            s.connected = false;
            s.cur_addr.clear();
        }
    }
    let ports = candidate_ports();
    if ports.is_empty() {
        if let Ok(mut s) = shared.lock() {
            append_log(&mut s.log, "✗ 自动连接：本机没有任何登记在册的串口\n");
            append_log(&mut s.log, "  请确认：写字机已通电 → Windows 蓝牙里已连接 BtWriter → 已勾选“串行端口(SPP)”服务\n");
        }
        return;
    }
    if let Ok(mut s) = shared.lock() {
        append_log(&mut s.log, &format!("● 自动连接：候选端口 {}\n", ports.join(", ")));
    }

    // 第 1 轮：常见波特率；第 2 轮：全量波特率
    for (round, bauds) in [(1, FAST_BAUDS), (2, wrobot_core::COMMON_BAUDS)].iter() {
        for p in &ports {
            for &b in *bauds {
                if *round == 2 && FAST_BAUDS.contains(&b) {
                    continue; // 第 1 轮已试过
                }
                match probe_port(p, b) {
                    Ok(kind) if !kind.is_empty() => {
                        let mut t = SerialTransport::new();
                        match t.connect(&format!("{}:{}", p, b)) {
                            Ok(()) => {
                                let notes = t.notes();
                                *transport = Some(Box::new(t));
                                if let Ok(mut s) = shared.lock() {
                                    s.connected = true;
                                    s.cur_addr = format!("{}:{}", p, b);
                                    s.cur_baud = b;
                                    append_log(&mut s.log, &format!("✓ {} @ {} → {}\n", p, b, kind));
                                    if !notes.is_empty() {
                                        append_log(&mut s.log, &format!("⚠ {}\n", notes));
                                    }
                                }
                            }
                            Err(e) => {
                                if let Ok(mut s) = shared.lock() {
                                    append_log(&mut s.log, &format!("✗ 重开 {} 失败：{}\n", p, e));
                                }
                            }
                        }
                        return;
                    }
                    _ => {}
                }
            }
        }
    }

    // 握手失败：退而连上第一个「能打开」的口，让用户用测试页手动排查
    for p in &ports {
        for &b in FAST_BAUDS {
            let mut t = SerialTransport::new();
            if t.connect(&format!("{}:{}", p, b)).is_ok() {
                let notes = t.notes();
                *transport = Some(Box::new(t));
                if let Ok(mut s) = shared.lock() {
                    s.connected = true;
                    s.cur_addr = format!("{}:{}", p, b);
                    s.cur_baud = b;
                    append_log(&mut s.log, &format!("⚠ 未收到固件响应，退而连接 {} @ {}\n", p, b));
                    if !notes.is_empty() {
                        append_log(&mut s.log, &format!("⚠ {}\n", notes));
                    }
                    append_log(&mut s.log, "  请到「测试」页点「解锁 $X」/「读位置」看有没有 ← 回显；没有回显说明链路没通。\n");
                    append_log(&mut s.log, "  链路不通时优先怀疑：写字机没通电 / USB 线松 / 蓝牙只配对未连接。\n");
                }
                return;
            }
        }
    }

    if let Ok(mut s) = shared.lock() {
        append_log(&mut s.log, "✗ 自动连接失败：没有任何端口可以打开\n");
        append_log(&mut s.log, "  点「诊断端口」查看每个口的错误码；蓝牙常见为 433/121（只配对未连接）。\n");
    }
}

/// 试探某个「端口 + 波特率」上是否有写字机固件，返回识别结果（空串=没反应）。
fn probe_port(port: &str, baud: u32) -> anyhow::Result<&'static str> {
    let mut sp = SerialPort::open(port, baud)?;
    let mut all = String::new();
    let mut drain = |sp: &mut SerialPort, ms: u64| {
        for _ in 0..4 {
            if let Some(l) = sp.read_line(ms) {
                all.push_str(&l);
                all.push(' ');
            }
        }
    };
    // 开机欢迎语（GRBL 复位后约 1 秒内打出 "Grbl 1.1h ['$' for help]"）
    drain(&mut sp, 300);
    // 唤醒 + 两种固件查询各来一次。
    // 写不进去就立刻放弃这个口：此时再等也只是干耗（典型是蓝牙 SPP 端口在、链路没建）。
    for probe in [b"\n".to_vec(), b"$I\n".to_vec(), b"M115\n".to_vec()] {
        if let Err(e) = sp.write_all(&probe) {
            return Err(anyhow::anyhow!(
                "{} 上写入失败（{}）——端口能打开但数据发不出去，链路未真正建立",
                port,
                e
            ));
        }
        drain(&mut sp, 350);
    }
    Ok(detect_firmware(&all))
}

/// 给一段固件回应打分：认出固件名最高，其次是「可读度高」。
///
/// 全是十六进制乱码（`<F8>` 之类）说明波特率不对，会给负分。
fn score_response(s: &str) -> i32 {
    let t = s.trim();
    if t.is_empty() {
        return 0;
    }
    let low = t.to_lowercase();
    let mut sc = 0;
    if low.contains("grbl") {
        sc += 100;
    }
    if low.contains("marlin") {
        sc += 80;
    }
    if low.contains("ebb") {
        sc += 60;
    }
    if low.contains("ok") {
        sc += 30;
    }
    if low.contains("ver") || low.contains('$') {
        sc += 15;
    }
    let g = wrobot_core::garbage_ratio(t);
    sc += ((1.0 - g) * 25.0) as i32;
    if g > 0.5 {
        sc -= 40; // 多半是乱码 = 波特率不对
    }
    sc
}

/// 自动波特率：在指定端口上逐个波特率握手，挑出能收到**可读回应**的那个并连上。
///
/// 「连上了但机器完全没反应」最常见的原因就是波特率不对——写字机出厂可能是
/// 9600 / 38400 / 115200 中的任意一个，靠猜是不行的，只能逐个试。
fn auto_baud(port: &str, transport: &mut Option<Box<dyn Transport>>, shared: &Arc<Mutex<DevShared>>) {
    // 先释放当前连接：否则自己占着端口，试开必然报「错误码 5 被占用」
    if transport.is_some() {
        if let Some(t) = transport.as_mut() {
            t.disconnect();
        }
        *transport = None;
        if let Ok(mut s) = shared.lock() {
            s.connected = false;
            s.cur_addr.clear();
        }
    }
    if let Ok(mut s) = shared.lock() {
        append_log(&mut s.log, &format!("● 自动波特率扫描 {} …\n", port));
    }

    let mut best: (u32, i32, String) = (115200, i32::MIN, String::new());
    for &b in wrobot_core::COMMON_BAUDS {
        let mut sp = match SerialPort::open(port, b) {
            Ok(s) => s,
            Err(e) => {
                if let Ok(mut s) = shared.lock() {
                    append_log(&mut s.log, &format!("  {} ✗ {}\n", b, e));
                }
                continue;
            }
        };
        let mut all = String::new();
        // 开机欢迎语（GRBL 上电约 1 秒内会打 "Grbl 1.1h ['$' for help]"）
        for _ in 0..3 {
            if let Some(l) = sp.read_line(250) {
                all.push_str(&l);
                all.push(' ');
            }
        }
        // 唤醒 + GRBL 状态查询 + 两种固件查询
        for probe in ["\n", "?\n", "$I\n", "M115\n"] {
            let _ = sp.write_all(probe.as_bytes());
            for _ in 0..3 {
                if let Some(l) = sp.read_line(200) {
                    all.push_str(&l);
                    all.push(' ');
                }
            }
        }
        drop(sp); // 必须显式关闭，否则下一个波特率会被自己挡住
        let sc = score_response(&all);
        let brief: String = all.chars().take(90).collect();
        if let Ok(mut s) = shared.lock() {
            append_log(&mut s.log, &format!("  {} → 评分 {}：{}\n", b, sc, brief));
        }
        if sc > best.1 {
            best = (b, sc, all);
        }
    }

    let (baud, sc, sample) = best;
    if let Ok(mut s) = shared.lock() {
        if sc >= 30 {
            append_log(&mut s.log, &format!("✓ {} 上 {} 有明确回应：{}\n", port, baud, sample.chars().take(120).collect::<String>()));
        } else if sc > 0 {
            append_log(&mut s.log, &format!("⚠ {} 上 {} 只有微弱回应，可能仍不匹配\n", port, baud));
        } else {
            append_log(
                &mut s.log,
                "✗ 所有波特率都没有任何回应 → 优先怀疑：写字机没通电 / 数据线松 / 驱动故障（点「诊断端口」看 WriteFile 那一步）\n",
            );
        }
    }

    // 用选出的波特率重新连接
    let mut t = SerialTransport::new();
    match t.connect(&format!("{}:{}", port, baud)) {
        Ok(()) => {
            let notes = t.notes();
            *transport = Some(Box::new(t));
            if let Ok(mut s) = shared.lock() {
                s.connected = true;
                s.cur_addr = format!("{}:{}", port, baud);
                s.cur_baud = baud;
                append_log(&mut s.log, &format!("● 已按 {} 连接 {}\n", baud, port));
                if !notes.is_empty() {
                    append_log(&mut s.log, &format!("⚠ {}\n", notes));
                }
            }
        }
        Err(e) => {
            if let Ok(mut s) = shared.lock() {
                append_log(&mut s.log, &format!("✗ 按 {} 重连失败：{}\n", baud, e));
            }
        }
    }
}

/// 诊断：逐个端口逐步试开，输出每一步的错误码与排查建议。
fn diagnose(shared: &Arc<Mutex<DevShared>>) {
    let mut log = String::new();
    log.push_str("● 端口诊断开始\n");
    let reg = wrobot_core::registered_ports();
    let cur = shared.lock().map(|s| s.cur_addr.clone()).unwrap_or_default();
    if reg.is_empty() {
        log.push_str("  ✗ 注册表 HKLM\\HARDWARE\\DEVICEMAP\\SERIALCOMM 为空：本机没有任何 COM 口\n");
        log.push_str("    → 有线：插上 USB-TTL（CH340），等设备管理器出现「USB-SERIAL CH340 (COMx)」\n");
        log.push_str("    → 蓝牙：先配对 BtWriter，再在「蓝牙设置 → 更多蓝牙选项 → COM 端口」添加【传出】端口\n");
    } else {
        log.push_str(&format!("  已登记端口：{}\n", reg.join(", ")));
        for p in &reg {
            // 本程序已连着这个口：再打开一次必然报「错误码 5 被占用」——其实就是被自己占用
            if !cur.is_empty() && cur.starts_with(p.as_str()) {
                log.push_str(&format!(
                    "  · {} 本程序已连接（跳过探测：重复打开会误报「错误码 5 被占用」）\n",
                    p
                ));
                continue;
            }
            for &b in wrobot_core::COMMON_BAUDS.iter().rev().take(1) {
                log.push_str(&wrobot_core::diag_port_steps(p, b));
            }
        }
    }
    log.push_str("  注：若某端口已被本程序连接，诊断它会显示「错误码 5」（自己占用），先断开再诊断更准。\n");
    log.push_str("  ── 错误码速查 ──\n");
    log.push_str("    31 = 设备不工作：拔插 USB / 设备管理器禁用再启用 / 写字机是否通电 / 蓝牙重新配对\n");
    log.push_str("    5  = 端口被占用：关掉串口助手、厂商软件、以及本程序的其它实例\n");
    log.push_str("    121/433 = 蓝牙只配对未连接：Windows 蓝牙里连上 BtWriter，且机器要开机\n");
    log.push_str("    2  = 端口不存在：换一个 COM 口号，或重新添加蓝牙传出端口\n");
    log.push_str("● 诊断结束（能打开≠能通信，关键看 WriteFile/ReadFile 那两行）\n");
    if let Ok(mut s) = shared.lock() {
        append_log(&mut s.log, &log);
    }
}

/// 串口线体检（后台线程）：释放端口 → 跑 core 的体检 → 报告逐行进日志。
fn wire_test_cmd(
    port: &str,
    baud: u32,
    transport: &mut Option<Box<dyn Transport>>,
    shared: &Arc<Mutex<DevShared>>,
) {
    // 体检必须独占端口：否则会被自己挡住（错误码 5 拒绝访问）
    if transport.is_some() {
        if let Some(t) = transport.as_mut() {
            t.disconnect();
        }
        *transport = None;
        if let Ok(mut s) = shared.lock() {
            s.connected = false;
            s.cur_addr.clear();
            s.cur_baud = 0;
            append_log(&mut s.log, "● 体检前先断开连接（体检需要独占端口）\n");
        }
    }
    if let Ok(mut s) = shared.lock() {
        append_log(&mut s.log, &format!("● 开始体检 {} @ {}\n", port, baud));
    }
    let report = wrobot_core::wire_test(port, baud, false);
    if let Ok(mut s) = shared.lock() {
        for line in report.lines() {
            append_log(&mut s.log, &format!("{}\n", line));
        }
    }
}

/// 从固件回显中识别固件类型。
fn detect_firmware(s: &str) -> &'static str {
    if s.contains("Grbl") || s.contains("grbl") {
        "GRBL"
    } else if s.contains("Marlin") || s.contains("marlin") {
        "Marlin"
    } else if s.contains("EBB") {
        "EBB"
    } else if s.contains("VER") || s.contains("ver:") {
        "未知固件（有版本响应）"
    } else if s.to_lowercase().contains("ok") {
        "未知固件（有 ok 响应）"
    } else {
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 「自动波特率」的评分必须能把「正常固件回应」和「乱码」区分开，
    /// 否则会挑到一个全是乱码的波特率，用户依旧连不上。
    #[test]
    fn score_prefers_real_firmware_over_garbage() {
        let grbl = score_response("Grbl 1.1h ['$' for help]");
        let marlin = score_response("Marlin 2.0 ok");
        let ok_only = score_response("ok");
        let garbage = score_response("<F8><FF×4><C3><80><FE>");
        let empty = score_response("");
        println!("grbl={grbl} marlin={marlin} ok={ok_only} garbage={garbage} empty={empty}");
        assert!(grbl > 90, "GRBL 欢迎语应得高分，实际 {grbl}");
        assert!(marlin > 60, "Marlin 应得高分，实际 {marlin}");
        assert!(ok_only > 0, "单纯 ok 也应算有效回应，实际 {ok_only}");
        assert!(grbl > garbage, "正常回应必须高于乱码：{grbl} vs {garbage}");
        assert!(garbage < 20, "乱码不该被当成有效波特率，实际 {garbage}");
        assert_eq!(empty, 0, "无回应应为 0 分");
    }

    /// 「蓝牙几分钟就断」的对策必须默认生效：保活心跳 5 秒、自动重连开启。
    #[test]
    fn keepalive_defaults_to_five_seconds() {
        let d = DeviceController::new();
        let s = d.snapshot();
        assert_eq!(s.keepalive_ms, 5000, "默认应有 5 秒心跳，实际 {}", s.keepalive_ms);
        assert!(!s.no_auto_reconnect, "自动重连应默认开启");
        d.set_keepalive(0, false);
        let s2 = d.snapshot();
        assert_eq!(s2.keepalive_ms, 0);
        assert!(s2.no_auto_reconnect);
    }

    /// 坐标解析：GRBL 的 `MPos`/`WPos`，只给 `MPos`+`WCO` 时要能自己算出 `WPos`。
    #[test]
    fn parses_grbl_status_positions() {
        let p = parse_position("<Idle|MPos:10.000,20.000,0.000|FS:0,0|WCO:1.000,2.000,0.000>");
        assert_eq!(p.m, Some([10.0, 20.0, 0.0]), "机器物理坐标错：{:?}", p.m);
        assert_eq!(p.w, Some([9.0, 18.0, 0.0]), "WPos 应为 MPos−WCO：{:?}", p.w);

        let q = parse_position("<Run|MPos:1.500,2.500,3.500|FS:500,0>");
        assert_eq!(q.m, Some([1.5, 2.5, 3.5]));
        assert_eq!(q.w, None, "没有 WCO 时不该编造 WPos");
        assert!(q.any());

        // 两个都有时以固件给的为准
        let r = parse_position("<Idle|MPos:10,10,0|WPos:3.000,4.000,0.000|WCO:7,6,0>");
        assert_eq!(r.w, Some([3.0, 4.0, 0.0]));
    }

    /// Marlin 的 `M114`：只给一组逻辑坐标；`Count` 后面的步数不能当成坐标。
    #[test]
    fn parses_marlin_m114_and_ignores_count() {
        let p = parse_position("X:12.34 Y:-5.60 Z:0.00 E:0.00 Count X:1234 Y:-560 Z:0");
        assert_eq!(p.w, Some([12.34, -5.6, 0.0]), "工件坐标错：{:?}", p.w);
        assert_eq!(p.m, None, "Marlin M114 不给物理坐标");
    }

    /// 普通回显（ok / 报错）不该被误判成坐标。
    #[test]
    fn non_position_lines_parse_to_empty() {
        for s in ["ok", "error:9", "Grbl 1.1h ['$' for help]", ""] {
            assert!(!parse_position(s).any(), "不该解析出坐标：{s:?}");
        }
    }

    /// 未连接时下发指令不能 panic，且要把指令记进日志（用户能看到「本该发什么」）。
    #[test]
    fn send_without_connection_logs_instead_of_panicking() {
        let shared = Arc::new(Mutex::new(DevShared::default()));
        let mut transport: Option<Box<dyn Transport>> = None;
        send_line(&mut transport, "G0 X10 Y10", &shared);
        let s = shared.lock().unwrap();
        assert!(s.log.contains("G0 X10 Y10"), "日志应记录跳过的指令：{}", s.log);
        assert!(s.log.contains("未连接"), "应明确提示未连接：{}", s.log);
    }
}
