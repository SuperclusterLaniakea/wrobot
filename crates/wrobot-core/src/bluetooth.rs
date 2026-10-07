//! Windows **经典蓝牙（BR/EDR）**原生访问。
//!
//! 为什么需要它：写字机走的是蓝牙 SPP（虚拟串口），而「设置里能看到设备」和
//! 「SPP 通道真的建起来」是两件事。本模块直接问系统蓝牙栈三件事：
//!   1. 这台写字机现在**在不在**（已记住 / 已认证 / 已连接 / 最后一次见到是什么时候）；
//!   2. 它身上**有没有装 SPP 服务**，对应哪个 COM 口；
//!   3. 没有就**替用户装上**（`BluetoothSetServiceState`），系统会自动建出传出 COM 口，
//!      免得用户去控制面板里手动点「添加 → 传出」。
//!
//! 另外提供可见性观察器 `bt_watch()`：周期性 inquiry，把「看得到 / 看不到」
//! 的时刻记成时间线，用来区分「模块反复重启（供电问题）」和「只是不广播」。

#[cfg(windows)]
use std::os::raw::c_int;

// ───────────────────────────── FFI 声明 ─────────────────────────────

#[cfg(windows)]
#[allow(non_snake_case, dead_code)]
mod ffi {
    use super::c_int;

    /// 蓝牙设备名最大长度（微软定义）
    pub const BLUETOOTH_MAX_NAME_SIZE: usize = 248;
    pub const BLUETOOTH_SERVICE_DISABLE: u32 = 0;
    pub const BLUETOOTH_SERVICE_ENABLE: u32 = 1;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct SystemTime {
        pub year: u16,
        pub month: u16,
        pub day_of_week: u16,
        pub day: u16,
        pub hour: u16,
        pub minute: u16,
        pub second: u16,
        pub milliseconds: u16,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Guid {
        pub data1: u32,
        pub data2: u16,
        pub data3: u16,
        pub data4: [u8; 8],
    }

    /// `BTH_ADDR`：`union { BTH_ADDR ullLong; UCHAR rgBytes[6]; }`。
    ///
    /// 它是 union 里带 `ULONGLONG`，所以**对齐是 8**——不写 align(8) 的话
    /// 整个 `BLUETOOTH_DEVICE_INFO` 会少 4 字节（556 vs 560），蓝牙 API 全部失败。
    #[repr(C, align(8))]
    #[derive(Clone, Copy, Default)]
    pub struct BluetoothAddress {
        pub bytes: [u8; 8],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct BluetoothRadioInfo {
        pub size: u32,
        pub address: BluetoothAddress,
        pub name: [u16; BLUETOOTH_MAX_NAME_SIZE],
        pub class_of_device: u32,
        pub lmp_subversion: u16,
        pub manufacturer: u8,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct BluetoothDeviceInfo {
        pub size: u32,
        pub address: BluetoothAddress,
        pub class_of_device: u32,
        pub connected: i32,
        pub remembered: i32,
        pub authenticated: i32,
        pub last_seen: SystemTime,
        pub last_used: SystemTime,
        pub name: [u16; BLUETOOTH_MAX_NAME_SIZE],
    }

    #[repr(C)]
    pub struct BluetoothFindRadioParams {
        pub size: u32,
    }

    #[repr(C)]
    pub struct BluetoothDeviceSearchParams {
        pub size: u32,
        pub return_authenticated: i32,
        pub return_remembered: i32,
        pub return_unknown: i32,
        pub return_connected: i32,
        pub issue_inquiry: i32,
        pub timeout_multiplier: u8,
        pub radio: isize, // HANDLE，repr(C) 会自动补上对齐 padding
    }

    #[link(name = "bthprops")]
    extern "system" {
        pub fn BluetoothFindFirstRadio(
            params: *const BluetoothFindRadioParams,
            radio: *mut isize,
        ) -> isize;
        pub fn BluetoothFindNextRadio(find: isize, radio: *mut isize) -> c_int;
        pub fn BluetoothFindRadioClose(find: isize) -> c_int;
        pub fn BluetoothGetRadioInfo(radio: isize, info: *mut BluetoothRadioInfo) -> u32;

        pub fn BluetoothFindFirstDevice(
            params: *const BluetoothDeviceSearchParams,
            dev: *mut BluetoothDeviceInfo,
        ) -> isize;
        pub fn BluetoothFindNextDevice(find: isize, dev: *mut BluetoothDeviceInfo) -> c_int;
        pub fn BluetoothFindDeviceClose(find: isize) -> c_int;
        pub fn BluetoothGetDeviceInfo(radio: isize, dev: *mut BluetoothDeviceInfo) -> u32;

        pub fn BluetoothSetServiceState(
            radio: isize,
            dev: *const BluetoothDeviceInfo,
            service: *const Guid,
            flags: u32,
        ) -> u32;
        pub fn BluetoothEnumerateInstalledServices(
            radio: isize,
            dev: *const BluetoothDeviceInfo,
            count: *mut u32,
            guids: *mut Guid,
        ) -> u32;
    }
}

/// SPP（串口）服务类 UUID：`00001101-0000-1000-8000-00805F9B34FB`
#[cfg(windows)]
const GUID_SPP: ffi::Guid = ffi::Guid {
    data1: 0x0000_1101,
    data2: 0x0000,
    data3: 0x1000,
    data4: [0x80, 0x00, 0x00, 0x80, 0x5F, 0x9B, 0x34, 0xFB],
};

// ───────────────────────────── 数据结构 ─────────────────────────────

/// 一台蓝牙设备的状态快照。
#[derive(Debug, Clone, Default)]
pub struct BtDevice {
    /// 设备名（如 `BtWriter`）
    pub name: String,
    /// MAC，12 位大写十六进制
    pub addr: String,
    /// 当前是否**已连接**（注意：已连接 ≠ SPP 通道已建立）
    pub connected: bool,
    /// 是否已被系统记住（配对过就有）
    pub remembered: bool,
    /// 是否已认证（配对完成）
    pub authenticated: bool,
    /// 设备类型编码
    pub class_of_device: u32,
    /// 最后一次被看到的时间（inquiry 结果）；从未见过则为 None
    pub last_seen: Option<String>,
    /// 最后一次被使用（连过）的时间
    pub last_used: Option<String>,
    /// 已安装的 SPP 服务数量（用于判断系统有没有给它开过串口）
    pub spp_installed: Option<bool>,
}

impl BtDevice {
    /// 人类可读的一行状态。
    pub fn status_line(&self) -> String {
        let mut s = format!("{}（{}）", self.name, self.addr);
        let mut tags: Vec<&str> = Vec::new();
        if self.remembered {
            tags.push("已配对");
        }
        if self.authenticated {
            tags.push("已认证");
        }
        if self.connected {
            tags.push("已连接");
        }
        if !tags.is_empty() {
            s.push_str(&format!(" [{}]", tags.join(" ")));
        }
        if let Some(t) = &self.last_seen {
            s.push_str(&format!(" 最后见到 {}", t));
        }
        s
    }

    /// 地址是否为空（未取到）。
    pub fn has_addr(&self) -> bool {
        !self.addr.is_empty() && self.addr != "000000000000"
    }
}

/// 蓝牙 COM 口与设备的绑定关系。
#[derive(Debug, Clone)]
pub struct BtComBinding {
    /// COM 口名
    pub port: String,
    /// 绑定的对端 MAC；`000000000000` 表示**未绑定任何设备**的通用口
    pub addr: String,
    /// true = **绑到了具体设备**（实例 ID 里带对端 MAC）；
    /// false = 地址全零的空口：谁也不属于，能打开但写进去 0 字节。
    pub outgoing: bool,
}

impl BtComBinding {
    /// 这个口是否真的绑到了某台设备上。
    pub fn bound(&self) -> bool {
        !self.addr.is_empty() && self.addr != "000000000000"
    }
}

// ───────────────────────────── 基础能力 ─────────────────────────────

/// 本机有几个蓝牙适配器（无线电）。
#[cfg(windows)]
pub fn bt_radios() -> Vec<String> {
    use ffi::*;
    let mut out = Vec::new();
    unsafe {
        let params = BluetoothFindRadioParams {
            size: std::mem::size_of::<BluetoothFindRadioParams>() as u32,
        };
        let mut h: isize = 0;
        let find = BluetoothFindFirstRadio(&params, &mut h);
        if find == 0 {
            return out;
        }
        loop {
            if h != 0 {
                let mut info: BluetoothRadioInfo = std::mem::zeroed();
                info.size = std::mem::size_of::<BluetoothRadioInfo>() as u32;
                if BluetoothGetRadioInfo(h, &mut info) == 0 {
                    let name = wide_to_string(&info.name);
                    let addr = addr_to_hex(&info.address);
                    out.push(format!("{}{}", name, if addr.is_empty() { String::new() } else { format!("（{}）", addr) }));
                } else {
                    out.push("(未知适配器)".to_string());
                }
                // 用完后关闭句柄，避免占着无线电
                let _ = winapi_close_handle(h);
            }
            let mut next: isize = 0;
            if BluetoothFindNextRadio(find, &mut next) == 0 {
                break;
            }
            h = next;
        }
        BluetoothFindRadioClose(find);
    }
    out
}

#[cfg(not(windows))]
pub fn bt_radios() -> Vec<String> {
    Vec::new()
}

#[cfg(windows)]
fn winapi_close_handle(h: isize) -> i32 {
    extern "system" {
        fn CloseHandle(h: isize) -> i32;
    }
    unsafe { CloseHandle(h) }
}

/// 枚举蓝牙设备。
///
/// - `inquiry = false`：只列系统**已记住**的（快，几十毫秒，不打扰链路）；
/// - `inquiry = true` ：额外做一次主动查询，能发现**未配对但可发现**的设备（慢，约 1.3×multiplier 秒）。
#[cfg(windows)]
pub fn bt_devices(inquiry: bool, timeout_multiplier: u8) -> Vec<BtDevice> {
    use ffi::*;
    let mut out: Vec<BtDevice> = Vec::new();
    unsafe {
        let mut h: isize = 0;
        let params = BluetoothFindRadioParams {
            size: std::mem::size_of::<BluetoothFindRadioParams>() as u32,
        };
        let find_radio = BluetoothFindFirstRadio(&params, &mut h);
        if find_radio == 0 {
            return out;
        }
        loop {
            if h != 0 {
                collect_on_radio(h, inquiry, timeout_multiplier, &mut out);
                let _ = winapi_close_handle(h);
            }
            let mut next: isize = 0;
            if BluetoothFindNextRadio(find_radio, &mut next) == 0 {
                break;
            }
            h = next;
        }
        BluetoothFindRadioClose(find_radio);
    }
    out
}

#[cfg(not(windows))]
pub fn bt_devices(_inquiry: bool, _mult: u8) -> Vec<BtDevice> {
    Vec::new()
}

#[cfg(windows)]
fn collect_on_radio(radio: isize, inquiry: bool, mult: u8, out: &mut Vec<BtDevice>) {
    use ffi::*;
    unsafe {
        let sp = BluetoothDeviceSearchParams {
            size: std::mem::size_of::<BluetoothDeviceSearchParams>() as u32,
            return_authenticated: 1,
            return_remembered: 1,
            return_unknown: 1,
            return_connected: 1,
            issue_inquiry: if inquiry { 1 } else { 0 },
            timeout_multiplier: mult.max(1),
            radio,
        };
        let mut info: BluetoothDeviceInfo = std::mem::zeroed();
        info.size = std::mem::size_of::<BluetoothDeviceInfo>() as u32;
        let find = BluetoothFindFirstDevice(&sp, &mut info);
        if find == 0 {
            return;
        }
        loop {
            let d = to_device(&info, radio);
            // 同一设备可能被多个无线电重复报出
            if !out.iter().any(|x| x.addr == d.addr && !d.addr.is_empty()) {
                out.push(d);
            }
            info.size = std::mem::size_of::<BluetoothDeviceInfo>() as u32;
            if BluetoothFindNextDevice(find, &mut info) == 0 {
                break;
            }
        }
        BluetoothFindDeviceClose(find);
    }
}

#[cfg(windows)]
fn to_device(info: &ffi::BluetoothDeviceInfo, radio: isize) -> BtDevice {
    let mut d = BtDevice {
        name: wide_to_string(&info.name),
        addr: addr_to_hex(&info.address),
        connected: info.connected != 0,
        remembered: info.remembered != 0,
        authenticated: info.authenticated != 0,
        class_of_device: info.class_of_device,
        last_seen: sys_time_to_string(&info.last_seen),
        last_used: sys_time_to_string(&info.last_used),
        spp_installed: None,
    };
    if d.name.is_empty() {
        d.name = format!("<无名 {}>", d.addr);
    }
    // 顺带查一下这台设备装没装 SPP 服务
    if radio != 0 {
        unsafe {
            let mut n: u32 = 32;
            let mut guids: [ffi::Guid; 32] = std::mem::zeroed();
            let r = ffi::BluetoothEnumerateInstalledServices(
                radio,
                info,
                &mut n,
                guids.as_mut_ptr(),
            );
            if r == 0 {
                let mut has = false;
                for i in 0..n.min(32) as usize {
                    let g = &guids[i];
                    if g.data1 == GUID_SPP.data1
                        && g.data2 == GUID_SPP.data2
                        && g.data3 == GUID_SPP.data3
                        && g.data4 == GUID_SPP.data4
                    {
                        has = true;
                    }
                }
                d.spp_installed = Some(has);
            }
        }
    }
    d
}

/// 按名字找设备（大小写不敏感、支持包含匹配）。先在已配对里找，找不到再主动查询一次。
pub fn bt_find(name: &str) -> Option<BtDevice> {
    let key = name.trim().to_lowercase();
    if key.is_empty() {
        return None;
    }
    let remembered = bt_devices(false, 1);
    let hit = pick(&remembered, &key);
    if hit.is_some() {
        return hit;
    }
    let seen = bt_devices(true, 4);
    pick(&seen, &key)
}

fn pick(list: &[BtDevice], key: &str) -> Option<BtDevice> {
    list.iter()
        .find(|d| d.name.to_lowercase() == key)
        .or_else(|| list.iter().find(|d| d.name.to_lowercase().contains(key)))
        .cloned()
}

/// 为一个已配对设备启用（或停用）SPP 串口服务。
///
/// 启用成功后 Windows 会自动建出一个**绑定了该设备**的传出 COM 口，
/// 这正是「设置里点连接」做的事——现在程序自己就能做。
#[cfg(windows)]
pub fn bt_set_spp(name: &str, enable: bool) -> Result<String, String> {
    use ffi::*;
    let dev = bt_find(name).ok_or_else(|| format!("找不到蓝牙设备「{}」", name))?;
    if !dev.authenticated && !dev.remembered {
        return Err(format!(
            "「{}」还没配对。请先在 Windows 蓝牙设置里完成配对，再回来绑定串口。",
            dev.name
        ));
    }
    unsafe {
        let mut h: isize = 0;
        let params = BluetoothFindRadioParams {
            size: std::mem::size_of::<BluetoothFindRadioParams>() as u32,
        };
        let find_radio = BluetoothFindFirstRadio(&params, &mut h);
        if find_radio == 0 {
            return Err("本机没有可用的蓝牙适配器".to_string());
        }
        let mut result: Result<String, String> =
            Err("没有在任何无线电上找到该设备".to_string());
        loop {
            if h != 0 {
                let mut info: BluetoothDeviceInfo = std::mem::zeroed();
                info.size = std::mem::size_of::<BluetoothDeviceInfo>() as u32;
                info.address = hex_to_addr(&dev.addr);
                if BluetoothGetDeviceInfo(h, &mut info) == 0 {
                    let flag = if enable {
                        BLUETOOTH_SERVICE_ENABLE
                    } else {
                        BLUETOOTH_SERVICE_DISABLE
                    };
                    let rc = BluetoothSetServiceState(h, &info, &GUID_SPP, flag);
                    if rc == 0 {
                        result = Ok(if enable {
                            format!(
                                "已为「{}」启用蓝牙串口服务，系统正在建 COM 口（稍等 2 秒后扫描串口）。",
                                dev.name
                            )
                        } else {
                            format!("已停用「{}」的蓝牙串口服务。", dev.name)
                        });
                    } else {
                        result = Err(format!(
                            "程序没能自动建口（错误码 {}）。系统多半已经给它装过一次串口服务了，\n\
                             ✦ 手动做法（30 秒搞定）：Windows 设置 → 蓝牙和其他设备 → 更多蓝牙选项\n\
                             \x20 → 「COM 端口」页 → 添加 → 传出 → 选「{}」→ 确定。\n\
                             \x20 建好后回到程序点「扫描串口」，新口会带上它的设备地址。",
                            rc,
                            dev.name
                        ));
                    }
                }
                let _ = winapi_close_handle(h);
            }
            let mut next: isize = 0;
            if BluetoothFindNextRadio(find_radio, &mut next) == 0 {
                break;
            }
            h = next;
        }
        BluetoothFindRadioClose(find_radio);
        result
    }
}

#[cfg(not(windows))]
pub fn bt_set_spp(_name: &str, _enable: bool) -> Result<String, String> {
    Err("蓝牙管理目前仅支持 Windows".to_string())
}

/// 枚举「蓝牙链接上的标准串行」COM 口，以及它们各自绑到了哪台设备。
///
/// 关键用途：识别那种**没绑设备**的通用传出口（实例 ID 里 MAC 全零）——
/// 这种口能打开但写 0 字节，是「连得上却没反应」的经典陷阱。
pub fn bt_com_bindings() -> Vec<BtComBinding> {
    #[cfg(windows)]
    {
        bt_com_bindings_win()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

#[cfg(windows)]
fn bt_com_bindings_win() -> Vec<BtComBinding> {
    use std::os::raw::c_int;
    #[repr(C)]
    struct SpDevInfoData {
        cb_size: u32,
        class_guid: [u8; 16],
        dev_inst: u32,
        reserved: usize,
    }
    #[link(name = "setupapi")]
    extern "system" {
        fn SetupDiGetClassDevsW(
            guid: *const u8,
            enumerator: *const u16,
            hwnd: isize,
            flags: u32,
        ) -> isize;
        fn SetupDiEnumDeviceInfo(set: isize, idx: u32, data: *mut u8) -> c_int;
        fn SetupDiGetDeviceInstanceIdW(
            set: isize,
            data: *const u8,
            buf: *mut u16,
            len: u32,
            need: *mut u32,
        ) -> c_int;
        fn SetupDiDestroyDeviceInfoList(set: isize) -> c_int;
    }
    const GUID_DEVCLASS_PORTS: [u8; 16] = [
        0x78, 0xE9, 0x36, 0x4D, 0x25, 0xE3, 0xCE, 0x11, 0xBF, 0xC1, 0x08, 0x00, 0x2B, 0xE1, 0x03,
        0x18,
    ];
    const SPDRP_FRIENDLYNAME: u32 = 0x0000_000C;

    // 复用 serial.rs 的属性读取？这里是独立模块，自己取一次友好名。
    #[link(name = "setupapi")]
    extern "system" {
        fn SetupDiGetDeviceRegistryPropertyW(
            set: isize,
            data: *const u8,
            prop: u32,
            ty: *mut u32,
            buf: *mut u8,
            len: u32,
            need: *mut u32,
        ) -> c_int;
    }

    let mut out: Vec<BtComBinding> = Vec::new();
    unsafe {
        let set = SetupDiGetClassDevsW(
            GUID_DEVCLASS_PORTS.as_ptr(),
            std::ptr::null(),
            0,
            0x0000_0002, // DIGCF_PRESENT
        );
        if set <= 0 {
            return out;
        }
        for i in 0..128u32 {
            let mut dd = SpDevInfoData {
                cb_size: std::mem::size_of::<SpDevInfoData>() as u32,
                class_guid: [0u8; 16],
                dev_inst: 0,
                reserved: 0,
            };
            if SetupDiEnumDeviceInfo(set, i, &mut dd as *mut _ as *mut u8) == 0 {
                break;
            }
            let mut idbuf = [0u16; 512];
            let mut need = 0u32;
            let ok = SetupDiGetDeviceInstanceIdW(
                set,
                &dd as *const _ as *const u8,
                idbuf.as_mut_ptr(),
                512,
                &mut need,
            );
            if ok == 0 {
                continue;
            }
            let len = idbuf.iter().position(|&c| c == 0).unwrap_or(512);
            let id = String::from_utf16_lossy(&idbuf[..len]);
            // 只关心蓝牙 SPP：BTHENUM\{00001101-...}
            if !id.contains("BTHENUM") || !id.to_uppercase().contains("{00001101") {
                continue;
            }
            let mut namebuf = [0u8; 512];
            let mut ty = 0u32;
            let mut nneed = 0u32;
            let nok = SetupDiGetDeviceRegistryPropertyW(
                set,
                &dd as *const _ as *const u8,
                SPDRP_FRIENDLYNAME,
                &mut ty,
                namebuf.as_mut_ptr(),
                512,
                &mut nneed,
            );
            let friendly = if nok != 0 {
                let words = std::slice::from_raw_parts(namebuf.as_ptr() as *const u16, 256);
                let s = String::from_utf16_lossy(words);
                s.trim_end_matches('\0').to_string()
            } else {
                String::new()
            };
            let port = crate::serial::extract_com(&friendly)
                .or_else(|| crate::serial::extract_com(&id))
                .unwrap_or_default();
            if port.is_empty() {
                continue;
            }
            // 绑到了具体设备地址 = 这个口是给那台设备用的（Windows 侧主动连它）；
            // 地址全零 = 通配的空口，谁也不属于，打开能写 0 字节。
            let addr = extract_mac_from_id(&id).unwrap_or_default();
            let outgoing = !addr.is_empty() && addr != "000000000000";
            out.push(BtComBinding {
                port,
                addr,
                outgoing,
            });
        }
        SetupDiDestroyDeviceInfoList(set);
    }
    out.sort_by_key(|b| crate::serial::port_number(&b.port));
    out
}

/// 从设备实例 ID 里抠出 12 位 MAC（形如 `...&409151E8E772_C00000000`）。
fn extract_mac_from_id(id: &str) -> Option<String> {
    let up = id.to_uppercase();
    let bytes: Vec<char> = up.chars().collect();
    let mut i = 0usize;
    while i + 12 <= bytes.len() {
        let seg: String = bytes[i..i + 12].iter().collect();
        if seg.chars().all(|c| c.is_ascii_digit() || ('A'..='F').contains(&c)) {
            // 排除 UUID 里的片段：要求两侧不是 GUID 分隔符
            let before = if i == 0 {
                ' '
            } else {
                bytes[i - 1]
            };
            if before != '-' && before != '{' {
                return Some(seg);
            }
        }
        i += 1;
    }
    None
}

// ───────────────────────────── 诊断输出 ─────────────────────────────

/// 生成一份蓝牙链路体检报告（给人看的整段文本）。
pub fn bt_report(name: &str) -> String {
    let mut out = String::new();
    let mut w = |s: &str| out.push_str(s);

    let radios = bt_radios();
    w("══ 蓝牙链路体检 ══\n\n");
    if radios.is_empty() {
        w("✗ 本机没有检测到蓝牙适配器（或蓝牙已被关闭）。\n");
        w("  处理：打开 Windows 的蓝牙开关；笔记本注意有没有物理飞行模式开关。\n");
        return out;
    }
    w(&format!("● 蓝牙适配器：{}\n", radios.join("，")));

    let bindings = bt_com_bindings();
    if bindings.is_empty() {
        w("\n● 蓝牙串口：系统里没有任何蓝牙 COM 口。\n");
    } else {
        w("\n● 蓝牙串口（SPP）与设备的绑定情况：\n");
        for b in &bindings {
            if b.bound() {
                w(&format!("  {}  绑定设备 {}\n", b.port, b.addr));
            } else {
                w(&format!(
                    "  {}  ⚠ 空口（没绑任何设备）→ 能打开但写进去 0 字节，别选它\n",
                    b.port
                ));
            }
        }
    }

    let dev = bt_find(name);
    match dev {
        None => {
            w(&format!(
                "\n✗ 现在看不到「{}」：既不在已配对列表，主动查询也没回应。\n",
                name
            ));
            w("\n  按顺序排查（越靠前越常见）：\n");
            w("  ① 写字机**通电且蓝牙指示灯在闪**——没通电当然搜不到；\n");
            w("  ② 供电不足：蓝牙模块电压一跌就复位，表现就是「一会儿看到一会儿看不到」。\n");
            w("     换个电源适配器/换个 USB 口，不要用电脑 USB 口给写字机供电；\n");
            w("  ③ 距离与遮挡：先放到离电脑 1 米内、无金属遮挡再试；\n");
            w("  ④ 2.4G 干扰：临时关掉 WiFi 或改用 5G 频段，微波炉/无线鼠标接收器也是干扰源；\n");
            w("  ⑤ 模块已连着别的主机（比如手机）——多数蓝牙模块只能同时连一个，\n");
            w("     先把手机上的连接断开。\n");
            w("\n  想看它到底是不是间歇性出现：wrobot-cli bt watch\n");
        }
        Some(d) => {
            w(&format!("\n● 目标设备：{}\n", d.status_line()));
            if let Some(has) = d.spp_installed {
                if has {
                    w("  ✓ 已安装串口（SPP）服务\n");
                } else {
                    w("  ⚠ 还没安装串口（SPP）服务 → 不会有可用 COM 口，需要「绑定蓝牙串口」\n");
                }
            }
            let mine = bindings.iter().find(|b| b.bound() && b.addr == d.addr);
            match mine {
                Some(b) => {
                    w(&format!("  ● 分配给它的 COM 口：{}\n", b.port));
                    if d.connected {
                        w("  ● 底层蓝牙**已连接** → 直接连这个口；机器没反应就点「自动波特率」\n");
                    } else {
                        w("  ⚠ 底层蓝牙**未连接** → 现在打开它会超时（错误码 121）。\n\n");
                        w("    ★ 先看清一件事：它现在能被搜到，说明写字机通电、蓝牙模块在正常广播，\n");
                        w("      **不是供电问题、也不是电脑蓝牙的问题**——问题在于「连接」这一步没成功。\n\n");
                        w("    按这个顺序处理（①最常见）：\n");
                        w("    ① 检查它是不是已经连着手机/平板了：这类蓝牙模块通常**同时只接受一个连接**，\n");
                        w("       被别的主机占着时，Windows 这边就会「搜得到但连不上」。\n");
                        w("       把手机上的 BtWriter 断开（或关掉手机蓝牙），再回来连。\n");
                        w("    ② **删掉配对重新配一次**：配对密钥可能对不上了（模块换过/掉过电），\n");
                        w("       Windows 拿着旧密钥去连，表现就是反复超时。\n");
                        w("       设置 → 蓝牙和其他设备 → BtWriter → 删除设备 → 重新添加，按提示完成配对。\n");
                        w("    ③ 重建蓝牙串口端口：更多蓝牙选项 → COM 端口 → 删掉旧的 → 添加 →【传出】→ 选它。\n");
                        w("       （建好后回程序点「扫描串口」，新口会带上它的设备地址）\n");
                        w("    ④ 还不行就用手机上的「蓝牙串口」App 连一下 BtWriter，\n");
                        w("       手机也连不上 = 写字机端模块坏了。\n");
                    }
                }
                None => {
                    w("  ⚠ 没有任何 COM 口绑到这台设备 → 能选的口都是空壳，写进去 0 字节。\n");
                    w("    做法：Windows 设置 → 蓝牙和其他设备 → 更多蓝牙选项 → COM 端口\n");
                    w("          → 添加 → 传出 → 选「BtWriter」，建好后回来扫描串口。\n");
                }
            }
        }
    }
    out
}

/// 可见性观察器：周期性查询，把「看得到 / 看不到」记成时间线。
///
/// 用途：如果它**规律性地**消失又出现（比如每 30 秒一轮），几乎可以断定是
/// 写字机端蓝牙模块在反复复位（供电问题），而不是电脑蓝牙的毛病。
pub fn bt_watch(name: &str, rounds: usize, interval_secs: u64) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "══ 观察「{}」的可见性（{} 轮，每轮间隔 {} 秒）══\n",
        name, rounds, interval_secs
    ));
    out.push_str("判定：稳定出现 = 电脑侧没问题；忽有忽无 = 写字机端模块在复位/断电。\n\n");

    let start = std::time::Instant::now();
    let mut last_visible: Option<bool> = None;
    let mut flips = 0usize;
    let mut visible_count = 0usize;
    let mut connected_count = 0usize;

    for i in 0..rounds.max(1) {
        let devs = bt_devices(true, 2);
        let hit = devs.iter().find(|d| {
            d.name.to_lowercase().contains(&name.to_lowercase())
                || (!d.addr.is_empty() && name.to_lowercase() == d.addr.to_lowercase())
        });
        let secs = start.elapsed().as_secs();
        match hit {
            Some(d) => {
                visible_count += 1;
                let tag = if d.connected {
                    connected_count += 1;
                    "已连接"
                } else {
                    "仅可见"
                };
                out.push_str(&format!(
                    "[{:>3}s] ✓ 可见  {}  {}\n",
                    secs,
                    d.addr,
                    tag
                ));
                if last_visible == Some(false) {
                    flips += 1;
                    out.push_str("        ↳ 刚刚从「看不到」恢复为「可见」\n");
                }
                last_visible = Some(true);
            }
            None => {
                out.push_str(&format!("[{:>3}s] ✗ 不可见（查询无回应）\n", secs));
                if last_visible == Some(true) {
                    flips += 1;
                    out.push_str("        ↳ 刚刚从「可见」掉线了\n");
                }
                last_visible = Some(false);
            }
        }
        if i + 1 < rounds.max(1) {
            std::thread::sleep(std::time::Duration::from_secs(interval_secs));
        }
    }

    out.push_str(&format!(
        "\n── 汇总：{} 轮中可见 {} 次，状态翻转 {} 次 ──\n",
        rounds.max(1),
        visible_count,
        flips
    ));
    if visible_count == 0 {
        out.push_str("✗ 全程看不到 → 写字机没通电/不在可发现状态/已连着别的主机。\n");
    } else if visible_count == rounds.max(1) && connected_count == 0 {
        out.push_str("✓ 全程可见，但**一次都没连上**（每次都是「仅可见」）。\n");
        out.push_str("  ★ 这个组合说明：写字机通电正常、模块在正常广播、电脑蓝牙也正常，\n");
        out.push_str("    只有「建立连接」这一件事失败了。别再查供电和信号了。\n");
        out.push_str("    ① 它是不是已经被手机/平板连着了？这类模块同时只接受一个连接；\n");
        out.push_str("    ② 删掉 BtWriter 的配对重新配一次（旧密钥对不上就会反复超时）；\n");
        out.push_str("    ③ 重建【传出】方向的蓝牙串口端口。\n");
    } else if visible_count == rounds.max(1) {
        out.push_str("✓ 全程可见 → 电脑蓝牙与可见性都正常。\n");
        out.push_str("  若此时仍连不上，问题在 SPP 通道（看 bt 报告里的 COM 口绑定）。\n");
    } else if flips >= 2 {
        out.push_str("⚠ 反复出现又消失 → 高度怀疑写字机端蓝牙模块在反复复位：\n");
        out.push_str("  ① 供电不足/接触不良（换电源、别用电脑 USB 供电）；\n");
        out.push_str("  ② 模块过热；③ 2.4G 强干扰。\n");
    } else {
        out.push_str("○ 偶发丢失 → 先排除干扰与距离，再长时间观察一次。\n");
    }
    out
}

// ───────────────────────────── 内部工具 ─────────────────────────────

#[cfg(windows)]
fn wide_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
        .trim()
        .trim_end_matches('\0')
        .to_string()
}

/// `BTH_ADDR` 的字节顺序与常见 MAC 书写顺序相反，这里两次都算出来，
/// 取「更像正常设备地址」的那个（只要能自洽地跟实例 ID 对上就行）。
#[cfg(windows)]
fn addr_to_hex(a: &ffi::BluetoothAddress) -> String {
    let b = a.bytes;
    let rev = format!(
        "{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        b[5], b[4], b[3], b[2], b[1], b[0]
    );
    let fwd = format!(
        "{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        b[0], b[1], b[2], b[3], b[4], b[5]
    );
    if rev == "000000000000" {
        fwd
    } else {
        rev
    }
}

#[cfg(windows)]
fn hex_to_addr(hex: &str) -> ffi::BluetoothAddress {
    let mut bytes = [0u8; 8];
    let h: Vec<char> = hex.chars().collect();
    // 实例 ID 里的顺序是高字节在前，转成 BTH_ADDR 要反过来放
    for i in 0..6 {
        let idx = 5 - i;
        let v = if idx * 2 + 1 < h.len() {
            u8::from_str_radix(&h[idx * 2..idx * 2 + 2].iter().collect::<String>(), 16)
                .unwrap_or(0)
        } else {
            0
        };
        bytes[i] = v;
    }
    ffi::BluetoothAddress { bytes }
}

#[cfg(windows)]
fn sys_time_to_string(t: &ffi::SystemTime) -> Option<String> {
    if t.year == 0 || t.year > 9999 {
        return None;
    }
    if t.month == 0 || t.month > 12 || t.day == 0 || t.day > 31 {
        return None;
    }
    Some(format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        t.year, t.month, t.day, t.hour, t.minute, t.second
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_mac_from_instance_id() {
        let id = "BTHENUM\\{00001101-0000-1000-8000-00805F9B34FB}_LOCALMFG&005D\\A&20ED00E0&0&409151E8E772_C00000000";
        assert_eq!(extract_mac_from_id(id), Some("409151E8E772".to_string()));
    }

    #[test]
    fn empty_mac_is_detected_as_unbound() {
        let b = BtComBinding {
            port: "COM4".to_string(),
            addr: "000000000000".to_string(),
            outgoing: true,
        };
        assert!(!b.bound());
        let b2 = BtComBinding {
            port: "COM5".to_string(),
            addr: "409151E8E772".to_string(),
            outgoing: false,
        };
        assert!(b2.bound());
    }

    #[test]
    fn non_windows_paths_do_not_panic() {
        // 这些在非 Windows 上是空实现；在 Windows 上即使没有适配器也必须优雅返回。
        let _ = bt_radios();
        let _ = bt_devices(false, 1);
        let _ = bt_com_bindings();
        let _ = bt_find("__肯定不存在的设备__");
    }

    #[cfg(windows)]
    #[test]
    fn struct_sizes_match_win32() {
        use ffi::*;
        // 布局对不上会导致蓝牙 API 直接失败甚至崩溃，这里钉死。
        assert_eq!(std::mem::size_of::<BluetoothDeviceInfo>(), 560);
        assert_eq!(std::mem::size_of::<BluetoothRadioInfo>(), 520);
        assert_eq!(std::mem::size_of::<BluetoothDeviceSearchParams>(), 40);
    }
}
