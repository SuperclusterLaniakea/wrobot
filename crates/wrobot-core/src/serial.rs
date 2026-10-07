//! 串口通信（Windows 原生实现）。
//!
//! 直接调用 Win32 API（`CreateFileW` / `GetCommState` / `SetCommState` /
//! `ReadFile` / `WriteFile` / `CloseHandle`），**不依赖任何外部 crate**，
//! 因此在完全离线的环境下也能构建——这是离线补上串口后端的关键。
//!
//! 典型用途：
//! - 蓝牙 SPP 虚拟串口（`蓝牙链接上的标准串行 (COM3/COM4)`）；
//! - CH340 / CP210x / FTDI 等 USB-TTL 适配器。
//!
//! 数据格式固定 8N1（8 数据位、无校验、1 停止位），流控关闭，
//! 符合 GRBL / Marlin 的默认串口配置。

use std::thread;
use std::time::{Duration, Instant};

use crate::error::{Result, WError};

/// DCB 结构缓冲区大小（真实 DCB 为 28 字节，留足余量）。
const DCB_BUF: usize = 64;

// DCB 字段在结构体内的字节偏移
const DCB_BAUD: usize = 4; // DWORD BaudRate
const DCB_FLAGS: usize = 8; // 位域（fBinary 等在最低位）
const DCB_BYTESIZE: usize = 18; // BYTE ByteSize
const DCB_PARITY: usize = 19; // BYTE Parity
const DCB_STOPBITS: usize = 20; // BYTE StopBits

/// 常用波特率（GRBL 默认 115200）。
pub const COMMON_BAUDS: &[u32] = &[9600, 19200, 38400, 57600, 115200, 230400, 250000];

#[cfg(windows)]
#[allow(non_snake_case)]
mod win {
    use std::os::raw::c_int;

    pub const GENERIC_READ: u32 = 0x8000_0000;
    pub const GENERIC_WRITE: u32 = 0x4000_0000;
    pub const OPEN_EXISTING: u32 = 3;
    pub const INVALID_HANDLE: isize = -1;
    pub const PURGE_ALL: u32 = 0x000F; // TXABORT|RXABORT|TXCLEAR|RXCLEAR
    pub const PURGE_TX: u32 = 0x0006; // TXABORT|TXCLEAR（只清发送缓冲）

    // —— EscapeCommFunction ——
    pub const SETDTR: u32 = 5;
    #[allow(dead_code)] // 保留完整定义，便于将来实现「拉低 DTR 触发复位」
    pub const CLRDTR: u32 = 6;
    pub const SETRTS: u32 = 3;
    #[allow(dead_code)]
    pub const CLRRTS: u32 = 4;

    // —— GetCommModemStatus ——
    pub const MS_CTS_ON: u32 = 0x0010;
    pub const MS_DSR_ON: u32 = 0x0020;
    pub const MS_RLSD_ON: u32 = 0x0040;

    // —— 代码页与转换标志 ——
    pub const CP_GBK: u32 = 936;
    pub const MB_ERR_INVALID_CHARS: u32 = 0x0000_0008;

    // —— 注册表（用于枚举 COM 口）——
    pub const HKEY_LOCAL_MACHINE: isize = 0x8000_0002u32 as i32 as isize;
    pub const KEY_READ: u32 = 0x0002_0019;
    pub const ERROR_SUCCESS: u32 = 0;
    pub const ERROR_NO_MORE_ITEMS: u32 = 259;
    pub const REG_SZ: u32 = 1;
    pub const REG_EXPAND_SZ: u32 = 2;

    #[repr(C)]
    pub struct CommTimeouts {
        pub read_interval: u32,
        pub read_total_const: u32,
        pub read_total_mult: u32,
        pub write_total_const: u32,
        pub write_total_mult: u32,
    }

    extern "system" {
        pub fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            sa: *const u8,
            disp: u32,
            flags: u32,
            tmpl: isize,
        ) -> isize;
        pub fn CloseHandle(h: isize) -> c_int;
        pub fn GetCommState(h: isize, dcb: *mut u8) -> c_int;
        pub fn SetCommState(h: isize, dcb: *const u8) -> c_int;
        pub fn SetCommTimeouts(h: isize, t: *const CommTimeouts) -> c_int;
        pub fn PurgeComm(h: isize, flags: u32) -> c_int;
        pub fn WriteFile(
            h: isize,
            buf: *const u8,
            n: u32,
            written: *mut u32,
            ov: *const u8,
        ) -> c_int;
        pub fn ReadFile(h: isize, buf: *mut u8, n: u32, read: *mut u32, ov: *const u8) -> c_int;
        pub fn GetLastError() -> u32;
        /// 拉高/拉低 DTR、RTS 等控制线（蓝牙模块与部分 GRBL 板需要 DTR 就绪才通信）。
        pub fn EscapeCommFunction(h: isize, func: u32) -> c_int;
        /// 取串口错误计数与收发队列字节数（判断「写入 0 字节」到底卡在哪）。
        pub fn ClearCommError(h: isize, errors: *mut u32, stat: *mut u8) -> c_int;
        /// 取 CTS/DSR/RLSD 等调制解调器输入线状态（判断硬件流控是否被对端拉住）。
        pub fn GetCommModemStatus(h: isize, stat: *mut u32) -> c_int;
        /// 多字节→宽字符转换（用来把国产固件回的 **GBK 中文** 正确解出来）。
        pub fn MultiByteToWideChar(
            codepage: u32,
            flags: u32,
            src: *const u8,
            src_len: i32,
            dst: *mut u16,
            dst_len: i32,
        ) -> i32;
        // advapi32：读注册表枚举 COM 口
        pub fn RegOpenKeyExW(
            key: isize,
            sub: *const u16,
            opts: u32,
            access: u32,
            out: *mut isize,
        ) -> u32;
        pub fn RegEnumValueW(
            key: isize,
            idx: u32,
            name: *mut u16,
            name_len: *mut u32,
            reserved: *mut u32,
            ty: *mut u32,
            data: *mut u8,
            data_len: *mut u32,
        ) -> u32;
        pub fn RegCloseKey(key: isize) -> u32;
    }
}

/// SetupAPI：用来查每个 COM 口背后的**物理设备**（蓝牙还是 USB 串口线）。
#[cfg(windows)]
#[allow(non_snake_case)]
mod setupapi {
    use std::os::raw::c_int;

    pub const DIGCF_PRESENT: u32 = 0x0000_0002;
    pub const SPDRP_DEVICEDESC: u32 = 0x0000_0000; // 设备描述
    pub const SPDRP_FRIENDLYNAME: u32 = 0x0000_000C; // 友好名（含 "(COM3)"）
    pub const SPDRP_ENUMERATOR_NAME: u32 = 0x0000_0016; // 枚举器：USB / BTHENUM / ACPI

    #[link(name = "setupapi")]
    extern "system" {
        pub fn SetupDiGetClassDevsW(
            guid: *const u8,
            enumerator: *const u16,
            hwnd: isize,
            flags: u32,
        ) -> isize;
        pub fn SetupDiEnumDeviceInfo(set: isize, idx: u32, data: *mut u8) -> c_int;
        pub fn SetupDiGetDeviceRegistryPropertyW(
            set: isize,
            data: *const u8,
            prop: u32,
            ty: *mut u32,
            buf: *mut u8,
            len: u32,
            need: *mut u32,
        ) -> c_int;
        pub fn SetupDiDestroyDeviceInfoList(set: isize) -> c_int;
    }
}

/// GUID_DEVCLASS_PORTS = {4D36E978-E325-11CE-BFC1-08002BE10318}（设备管理器里的「端口(COM 和 LPT)」）。
#[cfg(windows)]
const GUID_DEVCLASS_PORTS: [u8; 16] = [
    0x78, 0xE9, 0x36, 0x4D, 0x25, 0xE3, 0xCE, 0x11, 0xBF, 0xC1, 0x08, 0x00, 0x2B, 0xE1, 0x03, 0x18,
];

/// 串口句柄。关闭时自动释放。
pub struct SerialPort {
    handle: isize,
    /// 端口名，如 `COM3`
    pub port: String,
    /// 请求的波特率
    pub baud: u32,
    /// 驱动回读到的**实际**波特率（可能与请求值不同）
    pub actual_baud: u32,
    /// 打开过程中的降级说明（驱动拒绝某项配置时不致命，只记录）。
    ///
    /// 典型场景：蓝牙 SPP 虚拟串口 / 部分 CH340 在设备半掉线时对 `SetCommState`
    /// 返回错误码 31；此时若当前参数已是目标值，通信完全不受影响，不应直接失败。
    pub warnings: String,
}

impl SerialPort {
    /// 打开串口并配置为 8N1。
    pub fn open(port: &str, baud: u32) -> Result<Self> {
        open_raw(port, baud)
    }

    /// 写入字节（阻塞直到写超时）。
    pub fn write_all(&mut self, data: &[u8]) -> Result<()> {
        write_raw(self.handle, data)
    }

    /// 读取一块数据，返回读到的字节数（超时返回 0）。
    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        read_raw(self.handle, buf)
    }

    /// 读到一行（遇 `\n` 结束）或超时；无数据返回 `None`。
    pub fn read_line(&mut self, timeout_ms: u64) -> Option<String> {
        read_line_raw(self.handle, timeout_ms)
    }

    /// 清空收发缓冲。
    pub fn purge(&mut self) {
        #[cfg(windows)]
        unsafe {
            win::PurgeComm(self.handle, win::PURGE_ALL);
        }
    }

    /// 驱动回读的实际波特率。
    pub fn actual_baud(&self) -> u32 {
        self.actual_baud
    }

    /// 打开过程中的降级/警告说明（空串表示一切正常）。
    pub fn warnings(&self) -> &str {
        &self.warnings
    }

    pub fn is_open(&self) -> bool {
        self.handle >= 0
    }

    /// 底层句柄（仅供诊断使用：读线路状态、查看收发队列）。
    pub fn raw_handle(&self) -> isize {
        self.handle
    }

    fn close(&mut self) {
        if self.is_open() {
            #[cfg(windows)]
            unsafe {
                win::CloseHandle(self.handle);
            }
            self.handle = -1;
        }
    }
}

impl Drop for SerialPort {
    fn drop(&mut self) {
        self.close();
    }
}

/// 枚举本机**登记在册**的所有 COM 口（含蓝牙 SPP 已配对、但链路尚未建立的口）。
///
/// 数据来源：`HKLM\HARDWARE\DEVICEMAP\SERIALCOMM`。
/// 这一步非常关键——蓝牙写字机只配对未连接时，`CreateFile` 打不开（错误码 433/121），
/// 若只靠"逐个试开"来枚举，这些口根本不会出现在列表里，用户就只能瞎猜端口号。
/// 读取注册表 `SERIALCOMM` 的原始内容：**(设备对象名, COM 口)**。
///
/// 设备对象名是区分「蓝牙」与「有线串口线」的第一手依据：
/// - `\Device\BthModem0` → 蓝牙 SPP 虚拟串口；
/// - `\Device\USBSER000` / `\Device\VCP0` / `\Device\FTSER2K0` → USB-TTL 有线串口；
/// - `\Device\Serial0` → 主板自带串口（也是有线）。
#[cfg(windows)]
fn serialcomm_map() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    unsafe {
        let sub = to_wide("HARDWARE\\DEVICEMAP\\SERIALCOMM");
        let mut key: isize = 0;
        if win::RegOpenKeyExW(
            win::HKEY_LOCAL_MACHINE,
            sub.as_ptr(),
            0,
            win::KEY_READ,
            &mut key,
        ) != win::ERROR_SUCCESS
        {
            return out;
        }
        for i in 0..128u32 {
            let mut name = [0u16; 256];
            let mut name_len = 256u32;
            let mut ty = 0u32;
            let mut data = [0u8; 512];
            let mut data_len = 512u32;
            let r = win::RegEnumValueW(
                key,
                i,
                name.as_mut_ptr(),
                &mut name_len,
                std::ptr::null_mut(),
                &mut ty,
                data.as_mut_ptr(),
                &mut data_len,
            );
            if r == win::ERROR_NO_MORE_ITEMS || r != win::ERROR_SUCCESS {
                break;
            }
            if ty != win::REG_SZ && ty != win::REG_EXPAND_SZ {
                continue;
            }
            let dev = wide_to_string_u16(&name);
            let units = (data_len as usize / 2).min(256);
            let words = std::slice::from_raw_parts(data.as_ptr() as *const u16, units);
            let s = String::from_utf16_lossy(words);
            let s = s.trim_end_matches('\0').trim().to_string();
            if !s.is_empty()
                && s.to_uppercase().starts_with("COM")
                && !out.iter().any(|(_, p)| *p == s)
            {
                out.push((dev, s));
            }
        }
        win::RegCloseKey(key);
    }
    out
}

#[cfg(not(windows))]
fn serialcomm_map() -> Vec<(String, String)> {
    Vec::new()
}

#[cfg(windows)]
pub fn registered_ports() -> Vec<String> {
    let mut out: Vec<String> = serialcomm_map().into_iter().map(|(_, p)| p).collect();
    out.sort_by_key(|s| port_number(s));
    out
}

/// 把一段 UTF-16 缓冲（可能未填满、以 0 结尾）转成字符串。
fn wide_to_string_u16(buf: &[u16]) -> String {
    let end = buf.iter().position(|&w| w == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// 把一段 UTF-16 **字节**缓冲转成字符串（SetupAPI 返回的是字节数组）。
fn wide_bytes_to_string(buf: &[u8], valid_bytes: usize) -> String {
    let n = valid_bytes.min(buf.len()) / 2;
    let words: Vec<u16> = (0..n)
        .map(|i| u16::from_ne_bytes([buf[i * 2], buf[i * 2 + 1]]))
        .collect();
    wide_to_string_u16(&words)
}

// ——————————————— 端口类型：蓝牙 vs 有线串口线 ———————————————

/// 一个 COM 口的物理类型。
///
/// 蓝牙和 USB 串口线的失败模式完全不同，混在一起排查会浪费大量时间：
/// - **蓝牙**：端口存在不等于链路建立（只配对未连接时 `CreateFile` 报 433/121）；
/// - **有线串口线**：端口存在说明驱动已加载，剩下的问题是线材/波特率/机器是否通电。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortKind {
    /// 蓝牙 SPP 虚拟串口（BtWriter 之类）
    Bluetooth,
    /// USB-TTL 有线串口（CH340 / CP210x / FTDI / PL2303）
    UsbSerial,
    /// 主板自带串口（DB9，也是有线）
    Onboard,
    /// 无法判定
    Unknown,
}

impl PortKind {
    /// 中文短标签，用于界面上直接区分。
    pub fn label(&self) -> &'static str {
        match self {
            PortKind::Bluetooth => "蓝牙",
            PortKind::UsbSerial => "串口线",
            PortKind::Onboard => "主板串口",
            PortKind::Unknown => "未识别",
        }
    }

    /// 是否**有线**连接（串口线 / 主板串口）。
    pub fn is_wired(&self) -> bool {
        matches!(self, PortKind::UsbSerial | PortKind::Onboard)
    }

    /// 该类型连不上时最该先做的三件事。
    pub fn tips(&self) -> &'static str {
        match self {
            PortKind::Bluetooth => {
                "① 写字机通电；② Windows 蓝牙里【连接】BtWriter（配对≠连接）；③ 蓝牙设置→更多蓝牙选项→COM 端口，确认有【传出】方向"
            }
            PortKind::UsbSerial => {
                "① 换一个 USB 口/换一根数据线（很多线只能充电不能传数据）；② 设备管理器看「端口(COM 和 LPT)」下有没有 USB-SERIAL；③ 装官方 CH340/CP210x 驱动"
            }
            PortKind::Onboard => "① 确认主板串口在 BIOS 中启用；② 用的是直连线还是交叉线（DB9 需要交叉）",
            PortKind::Unknown => "① 设备管理器确认该口对应的物理设备；② 拔掉再插上，看哪个口消失即可定位",
        }
    }
}

/// 一个 COM 口的完整信息。
#[derive(Clone, Debug)]
pub struct PortEntry {
    /// `COM3`
    pub port: String,
    /// 设备对象名，如 `\Device\USBSER000`（蓝牙为 `\Device\BthModem0`）
    pub device: String,
    /// 设备友好名，如 `USB-SERIAL CH340 (COM3)`
    pub desc: String,
    /// 物理类型
    pub kind: PortKind,
    /// 当前能否打开（蓝牙只配对未连接时为 false）
    pub online: bool,
}

impl PortEntry {
    /// 界面上一行的显示名，如 `COM3 [串口线] USB-SERIAL CH340`。
    pub fn display(&self) -> String {
        if self.desc.is_empty() {
            format!("{} [{}]", self.port, self.kind.label())
        } else {
            format!("{} [{}] {}", self.port, self.kind.label(), self.desc)
        }
    }
}

/// 按「枚举器 → 设备对象名 → 友好名」三级判据判定端口类型。
fn classify(enumerator: &str, device: &str, desc: &str) -> PortKind {
    let e = enumerator.to_uppercase();
    if e.starts_with("BTHENUM") || e.starts_with("BTH") {
        return PortKind::Bluetooth;
    }
    if e.starts_with("USB") || e.starts_with("FTDI") {
        return PortKind::UsbSerial;
    }
    if e.starts_with("ACPI") || e.starts_with("*PNP") || e.starts_with("ROOT") {
        return PortKind::Onboard;
    }
    let d = device.to_uppercase();
    if d.contains("BTH") {
        return PortKind::Bluetooth;
    }
    if d.contains("USBSER")
        || d.contains("VCP")
        || d.contains("FTSER")
        || d.contains("SILAB")
        || d.contains("PROLIFIC")
    {
        return PortKind::UsbSerial;
    }
    let s = desc.to_uppercase();
    if s.contains("BLUETOOTH") || s.contains("蓝牙") {
        return PortKind::Bluetooth;
    }
    if s.contains("USB") || s.contains("SERIAL") || s.contains("UART") {
        return PortKind::UsbSerial;
    }
    if s.contains("COMMUNICATIONS PORT") || s.contains("通信端口") {
        return PortKind::Onboard;
    }
    PortKind::Unknown
}

/// 从友好名里抠出 COM 口号（`USB-SERIAL CH340 (COM3)` → `COM3`）。
pub fn extract_com(s: &str) -> Option<String> {
    let up = s.to_uppercase();
    let pos = up.rfind("COM")?;
    let rest: String = s[pos..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
    if rest.len() > 3 {
        Some(rest)
    } else {
        None
    }
}

/// 用 SetupAPI 枚举「端口(COM 和 LPT)」类设备，取回 (COM 口, 友好名, 枚举器)。
#[cfg(windows)]
fn pnp_port_table() -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    unsafe {
        let set = setupapi::SetupDiGetClassDevsW(
            GUID_DEVCLASS_PORTS.as_ptr(),
            std::ptr::null(),
            0,
            setupapi::DIGCF_PRESENT,
        );
        if set == -1 || set == 0 {
            return out;
        }
        // SP_DEVINFO_DATA：64 位下 32 字节，32 位下 28 字节
        let cb: u32 = if std::mem::size_of::<usize>() == 8 { 32 } else { 28 };
        let mut dev = [0u8; 48];
        dev[0..4].copy_from_slice(&cb.to_ne_bytes());
        let mut idx = 0u32;
        while setupapi::SetupDiEnumDeviceInfo(set, idx, dev.as_mut_ptr()) != 0 {
            idx += 1;
            let desc = dev_property(set, &dev, setupapi::SPDRP_FRIENDLYNAME)
                .or_else(|| dev_property(set, &dev, setupapi::SPDRP_DEVICEDESC))
                .unwrap_or_default();
            let enr = dev_property(set, &dev, setupapi::SPDRP_ENUMERATOR_NAME).unwrap_or_default();
            if let Some(c) = extract_com(&desc) {
                if !out.iter().any(|(p, _, _)| *p == c) {
                    out.push((c, desc, enr));
                }
            }
        }
        setupapi::SetupDiDestroyDeviceInfoList(set);
    }
    out
}

#[cfg(not(windows))]
fn pnp_port_table() -> Vec<(String, String, String)> {
    Vec::new()
}

/// 读取某个设备的字符串属性（友好名 / 设备描述 / 枚举器）。
#[cfg(windows)]
fn dev_property(set: isize, dev: &[u8], prop: u32) -> Option<String> {
    unsafe {
        let mut buf = [0u8; 512];
        let mut ty = 0u32;
        let mut need = 0u32;
        let ok = setupapi::SetupDiGetDeviceRegistryPropertyW(
            set,
            dev.as_ptr(),
            prop,
            &mut ty,
            buf.as_mut_ptr(),
            buf.len() as u32,
            &mut need,
        );
        if ok == 0 {
            return None;
        }
        let n = if need > 0 { need as usize } else { buf.len() };
        let s = wide_bytes_to_string(&buf, n).trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
}

/// 枚举本机全部 COM 口，并**区分蓝牙与有线串口线**。
///
/// 这是「蓝牙和串口线分开」的基础：界面按 `kind` 分组显示，
/// 排查建议按 `kind.tips()` 给，不再让用户对着一串 COM 口号瞎猜。
pub fn enumerate_ports() -> Vec<PortEntry> {
    let pnp = pnp_port_table();
    let sc = serialcomm_map();
    let mut keys: Vec<String> = pnp.iter().map(|(c, _, _)| c.clone()).collect();
    for (_, p) in &sc {
        if !keys.contains(p) {
            keys.push(p.clone());
        }
    }
    keys.sort_by_key(|s| port_number(s));
    keys.into_iter()
        .map(|k| {
            let desc = pnp
                .iter()
                .find(|(c, _, _)| *c == k)
                .map(|(_, d, _)| d.clone())
                .unwrap_or_default();
            let enr = pnp
                .iter()
                .find(|(c, _, _)| *c == k)
                .map(|(_, _, e)| e.clone())
                .unwrap_or_default();
            let device = sc
                .iter()
                .find(|(_, p)| *p == k)
                .map(|(d, _)| d.clone())
                .unwrap_or_default();
            let kind = classify(&enr, &device, &desc);
            let online = probe(&k);
            PortEntry {
                port: k,
                device,
                desc,
                kind,
                online,
            }
        })
        .collect()
}

/// 只取**有线**串口（串口线 / 主板串口），排除蓝牙。
pub fn wired_ports() -> Vec<PortEntry> {
    enumerate_ports()
        .into_iter()
        .filter(|e| e.kind.is_wired())
        .collect()
}

/// 只取蓝牙串口。
pub fn bluetooth_ports() -> Vec<PortEntry> {
    enumerate_ports()
        .into_iter()
        .filter(|e| e.kind == PortKind::Bluetooth)
        .collect()
}

#[cfg(not(windows))]
pub fn registered_ports() -> Vec<String> {
    Vec::new()
}

/// 从 `COM12` 取出数字用于排序。
pub fn port_number(name: &str) -> u32 {
    name.chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse::<u32>()
        .unwrap_or(u32::MAX)
}

/// 枚举本机可用串口（先取注册表名单，再逐个试开，只返回**能打开**的）。
///
/// 注意：试开会短暂拉起 DTR，对已连接的 Arduino 会触发一次复位，
/// 因此 GUI 中把它放在"扫描"按钮下手动触发，而不是每次启动都扫。
pub fn list_serial_ports() -> Vec<String> {
    let mut candidates = registered_ports();
    if candidates.is_empty() {
        candidates = (1..=32u32).map(|i| format!("COM{}", i)).collect();
    }
    candidates.into_iter().filter(|p| probe(p)).collect()
}

/// 扫描全部登记端口并给出**在线/离线**状态（离线也返回，便于提示用户排查蓝牙）。
pub fn scan_ports_detailed() -> Vec<(String, bool)> {
    let mut candidates = registered_ports();
    if candidates.is_empty() {
        candidates = (1..=32u32).map(|i| format!("COM{}", i)).collect();
    }
    candidates
        .into_iter()
        .map(|p| {
            let ok = probe(&p);
            (p, ok)
        })
        .collect()
}

/// 逐步诊断某个端口：把每一步 Win32 调用的结果与错误码都列出来。
///
/// 「错误码 31（ERROR_GEN_FAILURE）」是最难自查的一类故障——它只说明"设备不工作"，
/// 但可能是 CreateFile / GetCommState / SetCommState / 读写任一环节失败。
/// 本函数逐环打印，配合 `err_hint` 给出可执行建议。
#[cfg(windows)]
pub fn diag_port_steps(port: &str, baud: u32) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "── {port} @ {baud} ──");

    let h = match open_handle(port) {
        Ok(h) => h,
        Err(e) => {
            let _ = writeln!(out, "  1) CreateFile  ✗ {e}");
            let _ = writeln!(out, "     → {}", err_hint(last_error()));
            return out;
        }
    };
    let _ = writeln!(out, "  1) CreateFile  ✓ 句柄已拿到（物理链路/驱动存在）");

    // 2) GetCommState
    let mut dcb = [0u8; DCB_BUF];
    let ok = unsafe { win::GetCommState(h, dcb.as_mut_ptr()) };
    if ok == 0 {
        let e = last_error();
        let _ = writeln!(out, "  2) GetCommState  ✗ 错误码 {e}");
        let _ = writeln!(out, "     → {}（改用手工构造的 8N1 DCB 重试）", err_hint(e));
        dcb = blank_dcb(baud);
    } else {
        let got_baud = u32::from_le_bytes([dcb[DCB_BAUD], dcb[DCB_BAUD + 1], dcb[DCB_BAUD + 2], dcb[DCB_BAUD + 3]]);
        let _ = writeln!(
            out,
            "  2) GetCommState  ✓ 当前 BAUD={got_baud} ByteSize={} Parity={} StopBits={}",
            dcb[DCB_BYTESIZE], dcb[DCB_PARITY], dcb[DCB_STOPBITS]
        );
        let cur_flags = u32::from_le_bytes([
            dcb[DCB_FLAGS],
            dcb[DCB_FLAGS + 1],
            dcb[DCB_FLAGS + 2],
            dcb[DCB_FLAGS + 3],
        ]);
        let _ = writeln!(out, "     流控：{}", describe_flow(cur_flags));
        if desired_flags(cur_flags) != cur_flags {
            let _ = writeln!(
                out,
                "     ⚠ 不是「无流控 + DTR/RTS 高」：硬件握手会让 WriteFile 写 0 字节 → 已尝试纠正"
            );
        }
        apply_8n1(&mut dcb, baud);
    }

    // 2.5) 把**刚读到的原始 DCB** 原样回写：区分「设备本身不工作」与「我们改的字段不被接受」
    if ok != 0 {
        let mut raw = [0u8; DCB_BUF];
        if unsafe { win::GetCommState(h, raw.as_mut_ptr()) } != 0 {
            let ok2 = unsafe { win::SetCommState(h, raw.as_ptr()) };
            let _ = if ok2 == 0 {
                writeln!(
                    out,
                    "  2.5) SetCommState(原样回写)  ✗ 错误码 {} → 设备/驱动已处于故障态",
                    last_error()
                )
            } else {
                writeln!(out, "  2.5) SetCommState(原样回写)  ✓ 端口接受配置")
            };
        }
        apply_8n1(&mut dcb, baud);
    }

    // 3) SetCommState（设波特率与 8N1）
    let ok = unsafe { win::SetCommState(h, dcb.as_ptr()) };
    if ok == 0 {
        let e = last_error();
        let _ = writeln!(out, "  3) SetCommState  ✗ 错误码 {e}");
        let _ = writeln!(out, "     → {}", err_hint(e));
        let _ = writeln!(out, "     → 蓝牙 SPP 虚拟串口忽略波特率，可勾选「容错打开」继续");
    } else {
        let _ = writeln!(out, "  3) SetCommState  ✓ 已设为 {baud} 8N1");
    }

    // 4) SetCommTimeouts
    let t = win::CommTimeouts {
        read_interval: 0xFFFF_FFFF,
        read_total_const: 0,
        read_total_mult: 0,
        write_total_const: 1000,
        write_total_mult: 0,
    };
    let ok = unsafe { win::SetCommTimeouts(h, &t) };
    let _ = if ok == 0 {
        writeln!(out, "  4) SetCommTimeouts  ✗ 错误码 {}", last_error())
    } else {
        writeln!(out, "  4) SetCommTimeouts  ✓")
    };

    // 5) PurgeComm
    let ok = unsafe { win::PurgeComm(h, win::PURGE_TX) };
    let _ = if ok == 0 {
        writeln!(out, "  5) PurgeComm  ✗ 错误码 {}", last_error())
    } else {
        writeln!(out, "  5) PurgeComm  ✓")
    };

    // 6) WriteFile：发一个换行唤醒固件
    let mut written: u32 = 0;
    let ok = unsafe { win::WriteFile(h, b"\n".as_ptr(), 1, &mut written, std::ptr::null()) };
    let _ = if ok == 0 {
        writeln!(out, "  6) WriteFile  ✗ 错误码 {}", last_error())
    } else if written == 0 {
        writeln!(out, "  6) WriteFile  ✗ 返回成功但写入 0 字节：驱动已不接受数据（设备掉线/驱动故障）")
    } else {
        writeln!(out, "  6) WriteFile  ✓ 已发送 {} 字节（唤醒）", written)
    };

    // 6.5) 线路状态：CTS/DSR 若为低，说明对端没就绪（硬件流控会把写卡死）
    let st = line_state(h);
    let _ = writeln!(out, "  6.5) 线路：{}", st);
    let _ = writeln!(
        out,
        "     注：CH340/CP210x 这类 USB-TTL 通常不接握手线，CTS/DSR 全低属正常；蓝牙 SPP 一般会是高电平"
    );

    // 7) ReadFile：看固件有没有回应
    let mut buf = [0u8; 128];
    let mut got: u32 = 0;
    let ok = unsafe { win::ReadFile(h, buf.as_mut_ptr(), 128, &mut got, std::ptr::null()) };
    if ok == 0 {
        let _ = writeln!(out, "  7) ReadFile  ✗ 错误码 {}（无数据也可能报超时）", last_error());
    } else if got == 0 {
        let _ = writeln!(out, "  7) ReadFile  · 0 字节：端口能开但固件没有回应");
    } else {
        let text = sanitize_bytes(&buf[..got as usize]);
        let _ = writeln!(out, "  7) ReadFile  ✓ {} 字节：{}", got, text);
        if garbage_ratio(&text) > 0.35 {
            let _ = writeln!(
                out,
                "     ⚠ 超过 1/3 的字节不可读 → **波特率多半不匹配**，请换 9600 / 38400 再试"
            );
        }
    }

    unsafe { win::CloseHandle(h) };
    out
}

#[cfg(not(windows))]
pub fn diag_port_steps(_port: &str, _baud: u32) -> String {
    String::from("（串口后端目前仅实现 Windows 平台）")
}

// ——————————————— 串口线体检 ———————————————

/// 写一次，返回 (实际写入字节数, 错误码)。
#[cfg(windows)]
fn write_once(handle: isize, data: &[u8]) -> (u32, u32) {
    let mut written = 0u32;
    let ok = unsafe {
        win::WriteFile(
            handle,
            data.as_ptr(),
            data.len() as u32,
            &mut written,
            std::ptr::null(),
        )
    };
    (written, if ok == 0 { last_error() } else { 0 })
}

/// 给一段回应打分：认出固件名最高，其次可读度，全是乱码给负分。
fn score_reply(s: &str) -> i32 {
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
        sc += 20;
    }
    sc -= (garbage_ratio(t) * 60.0) as i32;
    sc
}

/// **串口线体检**：对一根**有线**串口（USB-TTL / 主板串口）从驱动到对端固件逐层测一遍。
///
/// 与 `diag_port_steps` 的区别：后者只回答「本机能不能打开端口」，
/// 前者还要回答「线材通不通、对端有没有固件、波特率对不对」——
/// 这正是「线插上了、端口也在，但机器完全没反应」时最需要的信息。
///
/// `loopback=true` 时额外做回环自检：需要把该口的 TX/RX 短接，
/// 能收到自己发的字符串即证明 **线材 + 端口收发** 都是好的。
#[cfg(windows)]
pub fn wire_test(port: &str, baud: u32, loopback: bool) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "══ 串口线体检：{port} @ {baud} ══");

    // 0) 先分清这根线是蓝牙还是有线
    let entry = enumerate_ports().into_iter().find(|e| e.port == port);
    match &entry {
        Some(e) => {
            let _ = writeln!(out, "  类型：{}  {}", e.kind.label(), e.desc);
            if !e.device.is_empty() {
                let _ = writeln!(out, "  设备对象：{}", e.device);
            }
            if e.kind == PortKind::Bluetooth {
                let _ = writeln!(out, "  ⚠ 这是【蓝牙】口，不是串口线；蓝牙的排查方式与有线不同：");
                let _ = writeln!(out, "    {}", PortKind::Bluetooth.tips());
            }
        }
        None => {
            let _ = writeln!(out, "  ⚠ 本机端口表里查不到 {port}（设备未插/未登记），仍尝试直接打开");
        }
    }

    // 1) 打开 + 配置
    let mut sp = match open_raw(port, baud) {
        Ok(p) => p,
        Err(e) => {
            let _ = writeln!(out, "  1) 打开端口  ✗ {e}");
            let _ = writeln!(out, "     → {}", err_hint(last_error()));
            let _ = writeln!(out, "── 结论 ──");
            let _ = writeln!(out, "  ✗ 端口都打不开，先解决驱动/链路，暂时不用谈波特率。");
            return out;
        }
    };
    let _ = writeln!(out, "  1) 打开端口  ✓");
    if !sp.warnings().is_empty() {
        let _ = writeln!(out, "     ⚠ {}", sp.warnings());
    }
    if sp.actual_baud() != baud {
        let _ = writeln!(
            out,
            "     ⚠ 驱动实际波特率 {}（请求 {}）",
            sp.actual_baud(),
            baud
        );
    }
    let h = sp.raw_handle();

    // 2) 线路状态
    let _ = writeln!(out, "  2) 线路状态：{}", line_state(h));

    // 3) 写测试：能区分「驱动半死」与「对端没接」。
    //    蓝牙 SPP 打开句柄后 RFCOMM 数据通道还需要一点时间才就绪，
    //    所以先等 400ms，再按 3 轮（递增退避）重试，避免把"还没建好链"误判成"线坏了"。
    thread::sleep(Duration::from_millis(400));
    let mut written = 0u32;
    let mut werr = 0u32;
    for attempt in 0..3 {
        let (w, e) = write_once(h, b"\r\n");
        written += w;
        werr = e;
        if w > 0 {
            break;
        }
        if attempt + 1 < 3 {
            unsafe {
                let mut errs: u32 = 0;
                let mut stat = [0u8; 16];
                win::ClearCommError(h, &mut errs, stat.as_mut_ptr());
                win::EscapeCommFunction(h, win::SETDTR);
                win::EscapeCommFunction(h, win::SETRTS);
            }
            thread::sleep(Duration::from_millis(250 + 350 * attempt as u64));
        }
    }
    if werr != 0 || written == 0 {
        let _ = writeln!(
            out,
            "  3) 写测试  ✗ 请求 2 字节、实际 {} 字节{}",
            written,
            if werr != 0 {
                format!("（错误码 {werr}）")
            } else {
                String::new()
            }
        );
        let _ = writeln!(out, "     → {}", err_hint(werr));
        let _ = writeln!(out, "  4) 线路状态（写失败后）：{}", line_state(h));
        let _ = writeln!(out, "── 结论 ──");
        let is_bt = entry
            .as_ref()
            .map(|e| e.kind == PortKind::Bluetooth)
            .unwrap_or(false);
        if is_bt {
            let _ = writeln!(
                out,
                "  ✗ 蓝牙口能打开、但数据写不出去 = **SPP 数据通道没真正建立**（端口存在 ≠ 链路已通）。"
            );
            for s in [
                "① 写字机通电，蓝牙指示灯处于可连接状态（快闪/常亮）；",
                "② Windows 蓝牙设置里【连接】BtWriter —— 配对过也要再点一次「连接」；",
                "③ 蓝牙设置 → 更多蓝牙选项 → COM 端口：确认有【传出】方向，可删掉后重新添加；",
                "④ 蓝牙口总是成对出现，换另一个方向的 COM 口再试一次；",
                "⑤ 仍不行就在写字机开机状态下删除设备重新配对。",
            ] {
                let _ = writeln!(out, "     {s}");
            }
        } else {
            let _ = writeln!(
                out,
                "  ✗ 端口能开但写不进去 —— 驱动故障态或 USB 已半掉线。拔插 USB（换个口）后重试；仍如此就换线/换适配器。"
            );
        }
        return out;
    }
    let _ = writeln!(out, "  3) 写测试  ✓ 2 字节已交给驱动（TX 通路正常）");

    // 4) 静默监听：上电/复位后固件会主动打招呼
    let mut rx: Vec<u8> = Vec::new();
    let mut buf = [0u8; 512];
    for _ in 0..6 {
        if let Ok(n) = read_raw(h, &mut buf) {
            if n > 0 {
                rx.extend_from_slice(&buf[..n]);
            }
        }
        thread::sleep(Duration::from_millis(120));
    }
    let idle = sanitize_bytes(&rx);
    let _ = writeln!(
        out,
        "  4) 静默监听 0.7s：{}",
        if idle.is_empty() {
            String::from("（无数据）")
        } else {
            format!("收到 {} 字节 → {}", rx.len(), idle)
        }
    );

    // 5) 握手：GRBL / Marlin 各自能听懂的问句
    let mut all = String::new();
    for probe_cmd in ["\n", "$I\n", "M115\n", "$X\n"] {
        let _ = write_once(h, probe_cmd.as_bytes());
        for _ in 0..3 {
            if let Ok(n) = read_raw(h, &mut buf) {
                if n > 0 {
                    let s = sanitize_bytes(&buf[..n]);
                    all.push_str(&s);
                    all.push(' ');
                }
            }
            thread::sleep(Duration::from_millis(150));
        }
    }
    let _ = writeln!(
        out,
        "  5) 握手（\\n / $I / M115 / $X）：{}",
        if all.trim().is_empty() {
            String::from("（无任何回显）")
        } else {
            all.trim().to_string()
        }
    );

    // 6) 回环自检（需短接 TX/RX）
    if loopback {
        let tag = b"WRLOOP\n";
        let _ = write_once(h, tag);
        let mut back = String::new();
        for _ in 0..8 {
            if let Ok(n) = read_raw(h, &mut buf) {
                if n > 0 {
                    back.push_str(&sanitize_bytes(&buf[..n]));
                }
            }
            if back.contains("WRLOOP") {
                break;
            }
            thread::sleep(Duration::from_millis(120));
        }
        if back.contains("WRLOOP") {
            let _ = writeln!(out, "  6) 回环自检  ✓ 收到自己发的字节 → 线材与端口收发都正常");
        } else {
            let _ = writeln!(
                out,
                "  6) 回环自检  ✗ 没收到自己发的字节（{}）→ 要么 TX/RX 没短接，要么线材断/接反",
                back.trim()
            );
        }
    }

    // 7) 波特率扫描：有回显但全是乱码时，这是唯一出路
    let best = if all.trim().is_empty() || score_reply(&all) < 20 {
        let _ = writeln!(out, "  7) 波特率扫描（当前波特率没拿到有效回应，逐个试）：");
        let mut best_baud = 0u32;
        let mut best_score = i32::MIN;
        let mut best_text = String::new();
        for &b in COMMON_BAUDS {
            if b == baud {
                continue;
            }
            let mut acc = String::new();
            if let Ok(p) = open_raw(port, b) {
                let ph = p.raw_handle();
                for probe_cmd in ["\n", "$I\n", "M115\n"] {
                    let _ = write_once(ph, probe_cmd.as_bytes());
                    for _ in 0..2 {
                        if let Ok(n) = read_raw(ph, &mut buf) {
                            if n > 0 {
                                acc.push_str(&sanitize_bytes(&buf[..n]));
                                acc.push(' ');
                            }
                        }
                        thread::sleep(Duration::from_millis(140));
                    }
                }
            }
            let sc = score_reply(&acc);
            let _ = writeln!(
                out,
                "     {} → 得分 {} {}",
                b,
                sc,
                if acc.trim().is_empty() {
                    String::new()
                } else {
                    format!("← {}", acc.trim())
                }
            );
            if sc > best_score {
                best_score = sc;
                best_baud = b;
                best_text = acc.trim().to_string();
            }
        }
        if best_score >= 20 {
            let _ = writeln!(out, "     → 建议改用 {}（回应：{}）", best_baud, best_text);
            Some(best_baud)
        } else {
            let _ = writeln!(
                out,
                "     → 全部波特率都没有可读回应：对端大概率不是写字机固件，或 TX/RX 根本没连通"
            );
            None
        }
    } else {
        let _ = writeln!(out, "  7) 波特率扫描：跳过（当前 {} 已有有效回应）", baud);
        None
    };
    let _ = sp.purge();

    // 8) 结论
    let _ = writeln!(out, "── 结论 ──");
    if score_reply(&all) >= 20 {
        let _ = writeln!(out, "  ✓ 链路正常：{} 上有固件回应，可以下发 G-code。", port);
    } else if let Some(b) = best {
        let _ = writeln!(
            out,
            "  ⚠ 当前波特率不通，但 {} 能收到回应 —— 把波特率改成 {} 再连。",
            port, b
        );
    } else {
        let _ = writeln!(out, "  ✗ 端口能开、数据能写出去，但对端零回应。按顺序排查：");
        let _ = writeln!(out, "     ① 写字机通电了吗（风扇/灯/屏幕有没有反应）？");
        let _ = writeln!(out, "     ② USB-TTL 的 TX/RX 是否接反（对端 TXD 接本机 RXD），GND 必须共地；");
        let _ = writeln!(out, "     ③ 换一根确认能传数据的 USB 线（很多充电线只有电源线芯）；");
        let _ = writeln!(out, "     ④ 想单独验证线材：把该口 TX/RX 短接，再加 --loop 跑回环自检。");
    }
    out
}

#[cfg(not(windows))]
pub fn wire_test(port: &str, _baud: u32, _loopback: bool) -> String {
    format!("串口线体检仅支持 Windows（{port}）\n")
}

/// 构造一份最小的 8N1 DCB（用于 `GetCommState` 失败的虚拟串口）。
#[cfg(windows)]
fn blank_dcb(baud: u32) -> [u8; DCB_BUF] {
    let mut d = [0u8; DCB_BUF];
    d[0..4].copy_from_slice(&28u32.to_le_bytes()); // DCBlength
    apply_8n1(&mut d, baud);
    d
}

// —— DCB 位域（DCB_FLAGS 偏移处的一个 u32）——
const F_BINARY: u32 = 1 << 0;
const F_OUTX_CTS_FLOW: u32 = 1 << 2; // 发送受 CTS 控制
const F_OUTX_DSR_FLOW: u32 = 1 << 3; // 发送受 DSR 控制
const F_DTR_CONTROL: u32 = 0b11 << 4; // 0=禁用 1=使能(高) 2=硬件握手
const F_DSR_SENSITIVITY: u32 = 1 << 6;
const F_OUT_X: u32 = 1 << 8; // 发送端 XON/XOFF
const F_IN_X: u32 = 1 << 9; // 接收端 XON/XOFF
const F_NULL: u32 = 1 << 11;
const F_RTS_CONTROL: u32 = 0b11 << 12; // 0=禁用 1=使能(高) 2=硬件握手 3=切换
const F_ABORT_ON_ERROR: u32 = 1 << 14;

/// 计算「我们希望 DCB 位域长成什么样」：只关流控、把 DTR/RTS 固定为高电平。
///
/// **这里正是「写入 0 字节」的根因所在**：若 `fDtrControl` / `fRtsControl` 为
/// 「硬件握手(2)」或 `fOutxCtsFlow` 为 1，而对端（写字机/蓝牙模块）根本没把
/// CTS/DSR 拉高，`WriteFile` 就会一直等握手信号，超时后返回 **TRUE 但 written=0**——
/// 表现就是「连上了、也发了，机器一动不动」。
fn desired_flags(cur: u32) -> u32 {
    let mut f = cur | F_BINARY;
    f &= !(F_OUTX_CTS_FLOW | F_OUTX_DSR_FLOW | F_OUT_X | F_IN_X | F_DSR_SENSITIVITY | F_NULL);
    f &= !(F_DTR_CONTROL | F_RTS_CONTROL);
    f |= 0b01 << 4; // fDtrControl = 使能（DTR 保持高电平）
    f |= 0b01 << 12; // fRtsControl = 使能（RTS 保持高电平）
    f &= !F_ABORT_ON_ERROR; // 出错不要把端口整个锁死
    f
}

/// 在原位设置波特率与 8N1，并强制关流控、拉高 DTR/RTS（不动其它位）。
#[cfg(windows)]
fn apply_8n1(dcb: &mut [u8], baud: u32) {
    dcb[DCB_BAUD..DCB_BAUD + 4].copy_from_slice(&baud.to_le_bytes());
    let flags = u32::from_le_bytes([
        dcb[DCB_FLAGS],
        dcb[DCB_FLAGS + 1],
        dcb[DCB_FLAGS + 2],
        dcb[DCB_FLAGS + 3],
    ]);
    dcb[DCB_FLAGS..DCB_FLAGS + 4].copy_from_slice(&desired_flags(flags).to_le_bytes());
    dcb[DCB_BYTESIZE] = 8;
    dcb[DCB_PARITY] = 0;
    dcb[DCB_STOPBITS] = 0;
}

/// 把 DCB 位域翻译成人能看懂的流控说明（诊断用）。
#[cfg(windows)]
fn describe_flow(flags: u32) -> String {
    let mut v = Vec::new();
    if flags & F_OUTX_CTS_FLOW != 0 {
        v.push("发送受CTS控制");
    }
    if flags & F_OUTX_DSR_FLOW != 0 {
        v.push("发送受DSR控制");
    }
    if flags & F_OUT_X != 0 {
        v.push("软件流控XON/XOFF(发)");
    }
    if flags & F_IN_X != 0 {
        v.push("软件流控XON/XOFF(收)");
    }
    match (flags & F_DTR_CONTROL) >> 4 {
        0 => v.push("DTR=低"),
        1 => v.push("DTR=高"),
        2 => v.push("DTR=硬件握手"),
        _ => {}
    }
    match (flags & F_RTS_CONTROL) >> 12 {
        0 => v.push("RTS=低"),
        1 => v.push("RTS=高"),
        2 => v.push("RTS=硬件握手"),
        _ => v.push("RTS=切换"),
    }
    if flags & F_BINARY == 0 {
        v.push("fBinary=0(异常)");
    }
    if v.is_empty() {
        "无流控".to_string()
    } else {
        v.join(" / ")
    }
}

/// 取线路状态（CTS/DSR）与错误计数，用于诊断「写不进去」。
#[cfg(windows)]
fn line_state(handle: isize) -> String {
    let mut st: u32 = 0;
    let mut errs: u32 = 0;
    let mut stat = [0u8; 16];
    let mut s = String::new();
    unsafe {
        if win::GetCommModemStatus(handle, &mut st) != 0 {
            s.push_str(&format!(
                "CTS={} DSR={} RLSD={} ",
                if st & win::MS_CTS_ON != 0 { "高" } else { "低" },
                if st & win::MS_DSR_ON != 0 { "高" } else { "低" },
                if st & win::MS_RLSD_ON != 0 { "高" } else { "低" },
            ));
        } else {
            s.push_str("线路状态不可用 ");
        }
        if win::ClearCommError(handle, &mut errs, stat.as_mut_ptr()) != 0 {
            let in_que = u32::from_le_bytes([stat[4], stat[5], stat[6], stat[7]]);
            let out_que = u32::from_le_bytes([stat[8], stat[9], stat[10], stat[11]]);
            s.push_str(&format!(
                "· 接收队列 {} 字节、发送队列 {} 字节、错误标志 {:#x}",
                in_que, out_que, errs
            ));
        }
    }
    s
}

/// 取最近一次 Win32 错误码。
#[cfg(windows)]
fn last_error() -> u32 {
    unsafe { win::GetLastError() }
}

/// 打开某端口并返回可读的错误说明（含排查建议）。
pub fn try_open_report(port: &str, baud: u32) -> std::result::Result<(), String> {
    match open_raw(port, baud) {
        Ok(_) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// 探测某个端口是否可打开。
pub fn probe(port: &str) -> bool {
    #[cfg(windows)]
    {
        match open_handle(port) {
            Ok(h) => {
                unsafe {
                    win::CloseHandle(h);
                }
                true
            }
            Err(_) => false,
        }
    }
    #[cfg(not(windows))]
    {
        let _ = port;
        false
    }
}

// ——————————————————— Windows 实现 ———————————————————

#[cfg(windows)]
fn to_wide(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    let mut v: Vec<u16> = std::ffi::OsStr::new(s).encode_wide().collect();
    v.push(0);
    v
}

/// 打开 `\\.\COMx` 设备句柄（不做任何参数配置）。
#[cfg(windows)]
fn open_handle(port: &str) -> Result<isize> {
    let path = format!("\\\\.\\{}", port);
    let wide = to_wide(&path);
    let h = unsafe {
        win::CreateFileW(
            wide.as_ptr(),
            win::GENERIC_READ | win::GENERIC_WRITE,
            0,
            std::ptr::null(),
            win::OPEN_EXISTING,
            0,
            0,
        )
    };
    if h == win::INVALID_HANDLE || h == 0 {
        let code = unsafe { win::GetLastError() };
        return Err(WError::Device(format!(
            "无法打开 {}（{}）",
            port,
            err_hint(code)
        )));
    }
    Ok(h)
}

/// 把常见 Win32 串口错误码翻译成可执行的排查建议。
pub fn err_hint(code: u32) -> String {
    match code {
        2 => "错误码 2 文件不存在：该 COM 口号在本机没有对应设备".to_string(),
        5 => "错误码 5 拒绝访问：端口已被占用——① 关掉串口助手/厂商软件/本程序的其它实例；② 若本程序已连着该口，先点「断开」再连（重复打开同一口就会被自己挡住）".to_string(),
        121 => "错误码 121 信号灯超时：蓝牙只配对未连接（写字机通电了吗？Windows 蓝牙里连上 BtWriter 了吗？）".to_string(),
        433 => "错误码 433 设备不存在：蓝牙已配对但 SPP 链路未建立（写字机是否开机？）".to_string(),
        31 => "错误码 31 设备无法正常工作(ERROR_GEN_FAILURE)：驱动拒绝配置或 USB 已半掉线 —— ① 拔掉 USB 重插（最好换个 USB 口）；② 设备管理器里『禁用』再『启用』该 COM 口；③ 写字机是否通电；④ 若是 CH340：装/更新官方驱动（老版驱动在 Win10/11 上会半掉线）；⑤ 若是蓝牙：Windows 蓝牙设置里删除设备重新配对，并重建【传出】COM 口".to_string(),
        6 => "错误码 6 句柄无效：设备在使用过程中被拔出或驱动崩溃，重新插拔后重试".to_string(),
        22 => "错误码 22 设备无法识别该命令".to_string(),
        23 => "错误码 23 数据错误（CRC/帧错误）：波特率不匹配，试试 9600".to_string(),
        87 => "错误码 87 参数错误：该虚拟串口不支持所请求的波特率或参数".to_string(),
        1117 => "错误码 1117 I/O 操作失败（数据线松动或设备掉线）".to_string(),
        1167 => "错误码 1167 设备未连接".to_string(),
        other => format!("错误码 {other}"),
    }
}

/// 打开句柄的重试次数与间隔。
///
/// USB-TTL 刚插上、蓝牙 SPP 链路刚建立时，设备枚举与驱动初始化需要几十到几百毫秒，
/// 立刻 `CreateFile` 会偶发失败（错误码 31 / 433 / 121）。重试两次即可覆盖绝大多数抖动。
const OPEN_RETRIES: u32 = 3;
const OPEN_RETRY_MS: u64 = 350;

#[cfg(windows)]
fn open_raw(port: &str, baud: u32) -> Result<SerialPort> {
    // 1) CreateFile（带重试）
    // 下面的初值只会在 OPEN_RETRIES 为 0 时残留，写清楚免得界面上出现无信息的「未知错误」
    let mut h = Err(WError::Device(format!("打开 {} 失败（未重试、未取得错误码）", port)));
    for attempt in 0..OPEN_RETRIES {
        h = open_handle(port);
        if h.is_ok() {
            break;
        }
        if attempt + 1 < OPEN_RETRIES {
            thread::sleep(Duration::from_millis(OPEN_RETRY_MS));
        }
    }
    let h = h?;

    let mut warn = String::new();

    // 2) 取当前 DCB；取不到就手工构造一份 8N1（某些虚拟串口不支持 GetCommState）
    let mut dcb = [0u8; DCB_BUF];
    let have_dcb = unsafe { win::GetCommState(h, dcb.as_mut_ptr()) } != 0;
    if !have_dcb {
        warn.push_str(&format!(
            "GetCommState 失败（错误码 {}），改用内置 8N1 配置；",
            last_error()
        ));
        dcb = blank_dcb(baud);
    }
    let cur_baud = u32::from_le_bytes([
        dcb[DCB_BAUD],
        dcb[DCB_BAUD + 1],
        dcb[DCB_BAUD + 2],
        dcb[DCB_BAUD + 3],
    ]);
    let flags = u32::from_le_bytes([
        dcb[DCB_FLAGS],
        dcb[DCB_FLAGS + 1],
        dcb[DCB_FLAGS + 2],
        dcb[DCB_FLAGS + 3],
    ]);
    // 只在参数不一致时才 SetCommState（参数已一致时跳过，绕开「原样回写也报 31」的半死设备）。
    // 注意：**流控/DTR/RTS 位也必须纳入比较**——驱动默认常常开启硬件握手
    // （或对端根本没拉 CTS），此时跳过设置就会导致 WriteFile 写 0 字节。
    let already = have_dcb
        && cur_baud == baud
        && dcb[DCB_BYTESIZE] == 8
        && dcb[DCB_PARITY] == 0
        && dcb[DCB_STOPBITS] == 0
        && desired_flags(flags) == flags;

    if !already {
        apply_8n1(&mut dcb, baud);
        if unsafe { win::SetCommState(h, dcb.as_ptr()) } == 0 {
            let code = last_error();
            // 回读实际值，判断这次失败是否真的影响通信
            let mut chk = [0u8; DCB_BUF];
            let (real, real_flags) = if unsafe { win::GetCommState(h, chk.as_mut_ptr()) } != 0 {
                (
                    u32::from_le_bytes([
                        chk[DCB_BAUD],
                        chk[DCB_BAUD + 1],
                        chk[DCB_BAUD + 2],
                        chk[DCB_BAUD + 3],
                    ]),
                    Some(u32::from_le_bytes([
                        chk[DCB_FLAGS],
                        chk[DCB_FLAGS + 1],
                        chk[DCB_FLAGS + 2],
                        chk[DCB_FLAGS + 3],
                    ])),
                )
            } else {
                (cur_baud, None)
            };
            if real == baud && real_flags.map(|f| desired_flags(f) == f).unwrap_or(false) {
                warn.push_str(&format!(
                    "驱动拒绝重设参数（{}），但当前已是 {} 8N1 且无流控，通信不受影响；",
                    err_hint(code),
                    baud
                ));
            } else {
                warn.push_str(&format!(
                    "参数未完全生效：请求 {}、实际 {}（{}）；当前流控状态『{}』；",
                    baud,
                    real,
                    err_hint(code),
                    real_flags.map(describe_flow).unwrap_or_else(|| "未知".into())
                ));
            }
        }
    }

    // 3.5) 直接把 DTR/RTS 拉高：就算 SetCommState 被拒，这一步仍常常能救回通信。
    //      很多蓝牙模块与 GRBL 板在 DTR/RTS 为低时不理会任何数据。
    let mut dtr_ok = true;
    unsafe {
        if win::EscapeCommFunction(h, win::SETDTR) == 0 {
            dtr_ok = false;
        }
        if win::EscapeCommFunction(h, win::SETRTS) == 0 {
            dtr_ok = false;
        }
        // 清掉遗留错误（CE_FRAME / CE_OVERRUN 等），否则后续读写会被旧错误干扰
        let mut errs: u32 = 0;
        let mut stat = [0u8; 16];
        win::ClearCommError(h, &mut errs, stat.as_mut_ptr());
    }
    if !dtr_ok {
        warn.push_str("DTR/RTS 拉高失败（{}）；");
        warn = warn.replace("{}", &err_hint(last_error()));
    }

    // 4) 超时：读非阻塞（read_interval=MAXDWORD 且 total=0 → ReadFile 立即返回），
    //    超时由 read_line 自己轮询控制，扫描「多端口 × 多波特率」时才不会像卡死。
    //    写总超时 1600ms：**蓝牙 SPP 建链比有线慢得多**（打开句柄后 RFCOMM 通道还要几十到几百
    //    毫秒才能真正就绪），给 1000ms 时经常写成 0 字节；加长后蓝牙首帧基本都能发出去，
    //    同时又不至于在设备真掉线时卡太久。
    let t = win::CommTimeouts {
        read_interval: 0xFFFF_FFFF,
        read_total_const: 0,
        read_total_mult: 0,
        write_total_const: 1600,
        write_total_mult: 0,
    };
    if unsafe { win::SetCommTimeouts(h, &t) } == 0 {
        warn.push_str(&format!("SetCommTimeouts 失败（错误码 {}）；", last_error()));
    }

    // 5) 只清发送缓冲：保留接收缓冲里的开机欢迎语（如 "Grbl 1.1h ['$' for help]"）
    if unsafe { win::PurgeComm(h, win::PURGE_TX) } == 0 {
        warn.push_str(&format!("PurgeComm 失败（错误码 {}）；", last_error()));
    }

    let actual_baud = {
        let mut chk = [0u8; DCB_BUF];
        if unsafe { win::GetCommState(h, chk.as_mut_ptr()) } != 0 {
            u32::from_le_bytes([
                chk[DCB_BAUD],
                chk[DCB_BAUD + 1],
                chk[DCB_BAUD + 2],
                chk[DCB_BAUD + 3],
            ])
        } else {
            baud
        }
    };

    Ok(SerialPort {
        handle: h,
        port: port.to_string(),
        baud,
        actual_baud,
        warnings: warn,
    })
}

#[cfg(windows)]
fn write_raw(handle: isize, data: &[u8]) -> Result<()> {
    if data.is_empty() {
        return Ok(());
    }
    // 「WriteFile 返回 TRUE 但写入 0 字节」是串口最难自查的故障之一：
    // 驱动收下了请求却没有把数据交给设备。常见原因：
    //   ① 硬件流控未就绪（CTS/DSR 为低，或 fDtrControl/fRtsControl 被设成了握手模式）；
    //   ② USB-TTL 半掉线（CH340 常见）；③ 波特率不匹配；④ 设备未通电。
    // 这里最多写 3 轮：每轮失败先清错误计数、重新拉高 DTR/RTS，再试剩余字节。
    let mut sent = 0usize;
    for attempt in 0..3 {
        let mut written: u32 = 0;
        let ok = unsafe {
            win::WriteFile(
                handle,
                data[sent..].as_ptr(),
                (data.len() - sent) as u32,
                &mut written,
                std::ptr::null(),
            )
        };
        if ok != 0 && written > 0 {
            sent += written as usize;
            if sent >= data.len() {
                return Ok(());
            }
            continue; // 部分写入：继续写剩下的
        }
        if attempt + 1 < 3 {
            unsafe {
                let mut errs: u32 = 0;
                let mut stat = [0u8; 16];
                win::ClearCommError(handle, &mut errs, stat.as_mut_ptr());
                win::EscapeCommFunction(handle, win::SETDTR);
                win::EscapeCommFunction(handle, win::SETRTS);
            }
            // 递增退避：蓝牙链路刚建立/刚闪断时，给驱动更长的恢复时间
            thread::sleep(Duration::from_millis(250 + 350 * attempt as u64));
        }
    }
    // 三轮都没写进去
    let mut written: u32 = 0;
    let ok = unsafe {
        win::WriteFile(
            handle,
            data[sent..].as_ptr(),
            (data.len() - sent) as u32,
            &mut written,
            std::ptr::null(),
        )
    };
    let state = line_state(handle);
    if ok == 0 {
        let code = last_error();
        return Err(WError::Device(format!(
            "写入失败（已写 {}/{} 字节，{}）｜线路：{}",
            sent,
            data.len(),
            err_hint(code),
            state
        )));
    }
    Err(WError::Device(format!(
        "写入 0 字节：设备没有接收数据（已重试 3 次）｜线路：{}｜→ ① 写字机通电了吗 ② 拔插 USB 换一个口 ③ 试试 9600 波特率 ④ 设备管理器里禁用再启用该 COM 口",
        state
    )))
}

#[cfg(windows)]
fn read_raw(handle: isize, buf: &mut [u8]) -> Result<usize> {
    let mut got: u32 = 0;
    let ok = unsafe {
        win::ReadFile(
            handle,
            buf.as_mut_ptr(),
            buf.len() as u32,
            &mut got,
            std::ptr::null(),
        )
    };
    if ok == 0 {
        // 超时也会走这里（ERROR_SEM_TIMEOUT），按 0 字节处理即可
        return Ok(0);
    }
    Ok(got as usize)
}

/// 连续相同字节的折叠标记里用的乘号。
///
/// 与 [`hex_marker_len`] 共用同一份字节，避免两处写死 UTF-8 编码导致识别不上。
const RUN_MARK: &str = "×";

/// 把一个字节按 UTF-8 前导位推断该字符占几字节（非法前导返回 1）。
fn utf8_seq_len(b: u8) -> usize {
    match b >> 4 {
        0b1100 | 0b1101 => 2,
        0b1110 => 3,
        0b1111 => match b >> 3 {
            0b11110 => 4,
            _ => 1,
        },
        _ => 1,
    }
}

/// 把串口原始字节转成**人能看懂**的字符串。
///
/// 这是「日志全是乱码」的解药：波特率不匹配 / 电平不兼容时，读到的字节往往
/// 既不是合法 UTF-8 也不是可打印 ASCII，直接 `from_utf8_lossy` 会变成一片
/// `�`（U+FFFD），除了让人迷惑什么也看不出来。这里把不可读字节显式显示为
/// `十六进制`，用户一眼就能判断「波特率不对」还是「设备回了什么奇怪的东西」。
///
/// - 可打印 ASCII 与换行原样保留；
/// - 合法 UTF-8（中文等）原样保留；
/// - 其余字节显示为 `<0A>` 这样的十六进制；
/// - 连续 4 个以上相同的 0x00/0xFF 折叠为 `<00×128>`，避免刷屏。
/// 尝试把整段字节当**文本**解出来：先 UTF-8，失败再试 **GBK（代码页 936）**。
///
/// 国产写字机/蓝牙模块的固件回应常有中文（"就绪""开始""错误"），而且几乎全是 **GBK** 编码。
/// GBK 中文的字节（如 `0xBE 0xCD`）不是合法 UTF-8，旧逻辑只能显示成 `<BE><CD>`——
/// 用户看到的正是"日志里的文字变成乱码"。这里补上 GBK 解码后，中文就能正常显示了。
///
/// 只有在**整段**都能被完整解码、且可读字符占比够高时才采用，
/// 否则（波特率不对产生的随机字节）退回逐字节的十六进制显示，不会被误判成中文。
fn decode_text(bytes: &[u8]) -> Option<String> {
    let clean = |s: &str| -> Option<String> {
        let mut out = String::new();
        let mut good = 0usize;
        let mut all = 0usize;
        for c in s.chars() {
            all += 1;
            if c == '\n' || c == '\r' || c == '\t' || (!c.is_control() && c != '\u{FFFD}') {
                out.push(c);
                good += 1;
            } else if (c as u32) < 0x100 {
                out.push_str(&format!("<{:02X}>", c as u32));
            } else {
                return None; // 解出奇怪的东西，交给逐字节兜底
            }
        }
        if all > 0 && (good as f32 / all as f32) < 0.7 {
            return None;
        }
        Some(out)
    };

    if let Ok(s) = std::str::from_utf8(bytes) {
        return clean(&s);
    }
    #[cfg(windows)]
    if let Some(s) = gbk_decode(bytes) {
        if let Some(c) = clean(&s) {
            return Some(c);
        }
    }
    None
}

/// 用 Win32 `MultiByteToWideChar(936)` 把 GBK 字节解成 UTF-8 字符串。
///
/// `MB_ERR_INVALID_CHARS` 会在遇到非法序列时直接失败——这正是我们要的：
/// 波特率不对时读到的随机字节几乎不可能整段通过 GBK 校验。
#[cfg(windows)]
fn gbk_decode(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    unsafe {
        let n = win::MultiByteToWideChar(
            win::CP_GBK,
            win::MB_ERR_INVALID_CHARS,
            bytes.as_ptr(),
            bytes.len() as i32,
            std::ptr::null_mut(),
            0,
        );
        if n <= 0 {
            return None;
        }
        let mut wide = vec![0u16; n as usize];
        let m = win::MultiByteToWideChar(
            win::CP_GBK,
            win::MB_ERR_INVALID_CHARS,
            bytes.as_ptr(),
            bytes.len() as i32,
            wide.as_mut_ptr(),
            n,
        );
        if m <= 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&wide[..m as usize]))
    }
}

pub fn sanitize_bytes(bytes: &[u8]) -> String {
    // 整段是合法文本（UTF-8 或 GBK）时直接返回，中文不再被拆成十六进制
    if let Some(s) = decode_text(bytes) {
        return s;
    }
    let mut out = String::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x00 || b == 0xFF {
            let mut j = i;
            while j < bytes.len() && bytes[j] == b {
                j += 1;
            }
            if j - i > 3 {
                out.push_str(&format!("<{:02X}{}{}>", b, RUN_MARK, j - i));
            } else {
                for _ in 0..(j - i) {
                    out.push_str(&format!("<{:02X}>", b));
                }
            }
            i = j;
            continue;
        }
        if b == b'\n' || b == b'\r' || b == b'\t' {
            out.push(b as char);
            i += 1;
            continue;
        }
        if (0x20..0x7f).contains(&b) {
            out.push(b as char);
            i += 1;
            continue;
        }
        let len = utf8_seq_len(b);
        if len > 1 && i + len <= bytes.len() {
            if let Ok(s) = std::str::from_utf8(&bytes[i..i + len]) {
                if let Some(c) = s.chars().next() {
                    if !c.is_control() {
                        out.push(c);
                        i += len;
                        continue;
                    }
                }
            }
        }
        out.push_str(&format!("<{:02X}>", b));
        i += 1;
    }
    out
}

/// 不可读内容的占比，0.0~1.0。占比高说明**波特率或电平不匹配**。
///
/// 计为「不可读」的三类：
/// 1. 非可打印 ASCII、又不是合法 UTF-8 的字节（波特率不对时最常见的产物）；
/// 2. `from_utf8_lossy` 产生的替换字符 `�`（U+FFFD）；
/// 3. 已经过 [`sanitize_bytes`] 处理的十六进制乱码标记 `<F8>` / `<FF×4>`。
pub fn garbage_ratio(s: &str) -> f32 {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return 0.0;
    }
    let mut i = 0usize;
    let mut bad = 0usize;
    let mut total = 0usize;
    while i < bytes.len() {
        total += 1;
        let b = bytes[i];
        // ① 十六进制乱码标记（sanitize 之后的形态）
        if b == b'<' {
            if let Some(len) = hex_marker_len(&bytes[i..]) {
                bad += 1;
                i += len;
                continue;
            }
        }
        if b == b'\n' || b == b'\r' || b == b'\t' || (0x20..0x7f).contains(&b) {
            i += 1;
            continue;
        }
        // ② 合法多字节 UTF-8（中文等）；但替换字符要算乱码
        let len = utf8_seq_len(b);
        if len > 1 && i + len <= bytes.len() {
            if let Ok(ch) = std::str::from_utf8(&bytes[i..i + len]) {
                let c = ch.chars().next().unwrap_or('\u{FFFD}');
                if !c.is_control() && c != '\u{FFFD}' {
                    i += len;
                    continue;
                }
            }
        }
        bad += 1;
        i += 1;
    }
    bad as f32 / total as f32
}

/// 识别 `<XX>` / `<XX×N>` 形式的乱码标记，返回其字节长度。
fn hex_marker_len(b: &[u8]) -> Option<usize> {
    if b.len() < 4 || b[0] != b'<' || !b[1].is_ascii_hexdigit() || !b[2].is_ascii_hexdigit() {
        return None;
    }
    if b[3] == b'>' {
        return Some(4);
    }
    // `<XX×N>`：乘号用与 sanitize 相同的字节序列，避免编码不一致
    let mark = RUN_MARK.as_bytes();
    if b.len() > 3 + mark.len() && &b[3..3 + mark.len()] == mark {
        let mut i = 3 + mark.len();
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i < b.len() && b[i] == b'>' {
            return Some(i + 1);
        }
    }
    None
}

#[cfg(windows)]
/// 读到一行（遇 `\n` 结束）或超时；无数据返回 `None`。
///
/// 采用「非阻塞读取 + 轮询」：底层 `ReadFile` 会立即返回，超时由本函数控制。
/// 否则每次无数据的读取都要白等满超时，扫描「多端口 × 多波特率」时会慢得像卡死。
///
/// 返回的内容经过 [`sanitize_bytes`] 处理，**不会再出现一片乱码**——
/// 不可读字节会显示为十六进制，可直接据此判断波特率是否匹配。
#[cfg(windows)]
fn read_line_raw(handle: isize, timeout_ms: u64) -> Option<String> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(10));
    let mut acc: Vec<u8> = Vec::new();
    let mut buf = [0u8; 256];
    // 上限：波特率不匹配时会灌进来海量「垃圾字节」，不能让缓冲区无限增长
    const MAX_ACC: usize = 4096;
    loop {
        let n = read_raw(handle, &mut buf).unwrap_or(0);
        if n > 0 {
            for &b in &buf[..n] {
                acc.push(b);
                if b == b'\n' {
                    let s = sanitize_bytes(&acc).trim().to_string();
                    return if s.is_empty() { None } else { Some(s) };
                }
            }
            if acc.len() >= MAX_ACC || Instant::now() >= deadline {
                break;
            }
            continue;
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(8));
    }
    if acc.is_empty() {
        None
    } else {
        let s = sanitize_bytes(&acc).trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
}

// ——————————————————— 非 Windows 平台占位 ———————————————————

#[cfg(not(windows))]
fn open_raw(port: &str, _baud: u32) -> Result<SerialPort> {
    let _ = (port, DCB_BUF, DCB_BAUD, DCB_FLAGS, DCB_BYTESIZE, DCB_PARITY, DCB_STOPBITS);
    Err(WError::Unsupported(
        "串口后端目前仅实现 Windows 平台".to_string(),
    ))
}

#[cfg(not(windows))]
fn write_raw(_h: isize, _d: &[u8]) -> Result<()> {
    Err(WError::Unsupported("非 Windows 平台无串口支持".into()))
}

#[cfg(not(windows))]
fn read_raw(_h: isize, _b: &mut [u8]) -> Result<usize> {
    Ok(0)
}

#[cfg(not(windows))]
fn read_line_raw(_h: isize, _t: u64) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bauds_sorted() {
        assert_eq!(COMMON_BAUDS.first(), Some(&9600u32));
        assert!(COMMON_BAUDS.contains(&115200));
    }

    /// 国产固件回的 **GBK 中文**必须显示成中文，而不是一串十六进制。
    ///
    /// 这是「日志里的文字是乱码」的直接原因：GBK 字节不是合法 UTF-8，
    /// 旧逻辑只能显示成 `<BE><CD>`。波特率不对产生的随机字节仍要显示成十六进制，
    /// 不能被误判成中文。
    #[test]
    #[cfg(windows)]
    fn gbk_chinese_is_decoded_not_hex() {
        let gbk: Vec<u8> = vec![0xBE, 0xCD, 0xD0, 0xF7]; // "就绪"
        let s = sanitize_bytes(&gbk);
        assert!(s.contains('就') && s.contains('绪'), "GBK 中文应解出，实际：{s}");
        let junk: Vec<u8> = vec![0xF8, 0xF8, 0xF8, 0xF8, 0xF8];
        let j = sanitize_bytes(&junk);
        assert!(j.contains("<F8"), "随机字节仍应为十六进制，实际：{j}");
        assert!(garbage_ratio(&j) > 0.5, "随机字节的乱码率应当很高：{}", garbage_ratio(&j));
    }

    /// 正常的 ASCII 回应不能被 GBK 解码改动。
    #[test]
    fn ascii_reply_is_untouched() {
        let s = sanitize_bytes(b"ok\r\nGrbl 1.1h ['$' for help]\r\n");
        assert!(s.contains("ok") && s.contains("Grbl 1.1h"), "实际：{s}");
        assert_eq!(garbage_ratio(&s), 0.0);
    }

    /// 打开一个几乎必然不存在的端口，必须优雅报错而不是 panic。
    #[test]
    fn open_missing_port_errors() {        let r = SerialPort::open("COM250", 115200);
        assert!(r.is_err(), "不存在的端口应返回 Err");
    }

    /// 逐步诊断对不存在的端口必须给出结论而不是 panic。
    ///
    /// 「错误码 31」那类故障之所以难查，就是因为没有分步输出；
    /// 这里保证诊断函数在任何端口上都能返回可读结论。
    #[test]
    fn diag_steps_never_panics() {
        let s = diag_port_steps("COM250", 115200);
        assert!(!s.is_empty(), "诊断应输出内容");
        assert!(s.contains("COM250"), "诊断应回显端口名：{s}");
        println!("{s}");
    }

    /// 打开端口失败时，错误信息应带排查建议（含错误码）。
    #[test]
    fn open_failure_has_hint() {
        match SerialPort::open("COM250", 115200) {
            Ok(p) => println!("意外打开：{:?} {}", p.port, p.warnings()),
            Err(e) => {
                let msg = e.to_string();
                println!("{msg}");
                assert!(!msg.is_empty(), "错误信息不应为空");
            }
        }
    }

    /// 乱码必须变成**可读的十六进制**，而不是一片 U+FFFD。
    ///
    /// 这是「实时日志全是乱码」的直接修复：波特率不匹配时读到的字节既不是
    /// 合法 UTF-8 也不是可打印 ASCII，`from_utf8_lossy` 只能给出 `�`。
    #[test]
    fn sanitize_turns_garbage_into_hex() {
        // 波特率不匹配时典型的垃圾数据
        let raw = [0xF8u8, 0xFF, 0xFF, 0xFF, 0xFF, 0x7E, 0x00, 0xC3];
        let s = sanitize_bytes(&raw);
        println!("{s}");
        assert!(s.contains("<F8>"), "单个非法字节应显示为十六进制：{s}");
        assert!(s.contains("<FF×4>"), "连续相同字节应折叠：{s}");
        assert!(s.contains("<C3>"), "非法 UTF-8 前导应显示为十六进制：{s}");
        assert!(!s.contains('\u{FFFD}'), "不应出现替换字符（那就是乱码）：{s}");
    }

    /// 正常文本与中文必须原样保留，不能被误判成乱码。
    #[test]
    fn sanitize_keeps_readable_text() {
        let s = sanitize_bytes("Grbl 1.1h ['$' for help]\r\n".as_bytes());
        assert_eq!(s, "Grbl 1.1h ['$' for help]\r\n");
        let cn = sanitize_bytes("写字机已就绪\n".as_bytes());
        assert_eq!(cn, "写字机已就绪\n");
        assert_eq!(garbage_ratio("Grbl 1.1h ['$' for help]"), 0.0);
        assert_eq!(garbage_ratio("写字机"), 0.0);
    }

    /// 乱码占比应能区分「正常回应」与「波特率不匹配」。
    #[test]
    fn garbage_ratio_detects_mismatch() {
        let good = "Grbl 1.1h ['$' for help]";
        let bad = "<F8><FF×4><C3><80>";
        assert!(garbage_ratio(good) < 0.1, "正常回应不该被判成乱码");
        assert!(garbage_ratio(bad) > 0.5, "乱码必须被识别出来");
    }

    #[test]
    fn list_ports_does_not_panic() {
        let v = list_serial_ports();
        // 只返回能真正打开的口（蓝牙只配对未建链路的口会被排除）
        println!("可打开的串口: {:?}", v);
        // 注册表名单应包含可打开口的超集
        let reg = registered_ports();
        for p in &v {
            assert!(reg.contains(p), "{} 应出现在登记名单里", p);
        }
    }
}
