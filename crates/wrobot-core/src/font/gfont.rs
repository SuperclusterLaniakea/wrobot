//! `.gfont` 单线字库解析。
//!
//! # 逆向结论（已完全确认）
//!
//! `.gfont` **并未加密**。真实结构是：
//!
//! ```text
//! [0..8)    头部: u32 BE 版本(=5) + u32 BE 元数据长度
//! [8..N)    字库自有前缀（元数据/缓存，与字形无关，可忽略）
//! [N..EOF)  标准 ZIP 归档（PK\x03\x04 / PK\x01\x02 / PK\x05\x06）
//! ```
//!
//! ZIP 的每个条目名为该字形的 **Unicode 码点十进制字符串**
//! （如 `19968` = U+4E00 = 「一」），内容为 deflate 压缩的字形记录：
//!
//! ```text
//! u16 BE   码点（与条目名一致）
//! u32 BE   浮点数个数 N（= 点数 × 2）
//! f32 BE × N   坐标序列 x0,y0,x1,y1,...（y 轴向上，单位为字库单位）
//! u32 BE   点数 M（= N / 2）
//! u8   × M     落笔标记：0 = 抬笔移动到该点，1 = 落笔绘制到该点
//! ```
//!
//! 另：原生 `KvenjoyEncrypt.dll`（init/decrypt/encrypt/freeMemory）确实存在，
//! 但仅用于 `licene` 激活文件与 jar 加固，与字库无关。
//! 机器指纹密钥推导（MD5 of HashSet{CPU ProcessorId, MAC, java.version,
//! java.vm.version, os.version}.toString()）已逆向并在 `key_derive` 中给出。

use crate::error::{Result, WError};
use super::inflate::{crc32, inflate};
use super::{Glyph, InkMetrics, Point, Stroke, StrokeFont};

/// gfont 文件头（明文部分）。
#[derive(Debug, Clone, Copy)]
pub struct GfontHeader {
    /// 版本，实测为 5
    pub version: u32,
    /// 元数据长度 / 加密体偏移
    pub meta_len: u32,
}

/// 读取 gfont 头部（大端）。
pub fn read_header(bytes: &[u8]) -> Result<GfontHeader> {
    if bytes.len() < 8 {
        return Err(WError::Font("gfont too short".into()));
    }
    let version = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let meta_len = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    Ok(GfontHeader { version, meta_len })
}

/// 16 字节密钥。
#[derive(Debug, Clone)]
pub enum Key {
    /// 32 个十六进制字符 → 16 字节
    Hex(String),
    /// 直接给 16 个字节
    Bytes([u8; 16]),
}

impl Key {
    pub fn to_i32(&self) -> Result<[i32; 16]> {
        let b: [u8; 16] = match self {
            Key::Bytes(b) => *b,
            Key::Hex(h) => {
                let s = h.trim();
                if s.len() != 32 {
                    return Err(WError::Font("hex key must be 32 chars".into()));
                }
                let mut out = [0u8; 16];
                for i in 0..16 {
                    out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
                        .map_err(|_| WError::Font("bad hex key".into()))?;
                }
                out
            }
        };
        let mut k = [0i32; 16];
        for i in 0..16 {
            k[i] = b[i] as i32; // 与 Java 侧 readUnsignedByte 一致（0..255）
        }
        Ok(k)
    }
}

/// 把任意字节流转成 `int[]`（每字节一个元素，0..255），与 Java 侧一致。
pub fn bytes_to_ints(data: &[u8]) -> Vec<i32> {
    data.iter().map(|&b| b as i32).collect()
}

// ---------------------------------------------------------------------------
// 原生库绑定
// ---------------------------------------------------------------------------

#[cfg(windows)]
#[link(name = "kernel32")]
extern "C" {
    fn LoadLibraryA(path: *const u8) -> isize;
    fn GetProcAddress(module: isize, name: *const u8) -> isize;
}

type DecryptFn = unsafe extern "C" fn(*const i32, i32, *const i32) -> *mut u8;
type EncryptFn = unsafe extern "C" fn(*const i32, i32, *const i32) -> *mut u8;
type FreeFn = unsafe extern "C" fn(*mut u8);
type InitFn = unsafe extern "C" fn();

/// KvenjoyEncrypt 原生库句柄。
pub struct EncryptLib {
    module: isize,
    init: Option<InitFn>,
    decrypt: Option<DecryptFn>,
    encrypt: Option<EncryptFn>,
    free: Option<FreeFn>,
}

#[cfg(windows)]
fn cstr(s: &str) -> Vec<u8> {
    let mut v: Vec<u8> = s.as_bytes().to_vec();
    v.push(0);
    v
}

impl EncryptLib {
    /// 加载 DLL（默认取奎享雕刻目录下的 x64 版本）。
    #[cfg(windows)]
    pub fn load(path: &std::path::Path) -> Result<EncryptLib> {
        let p = cstr(&path.to_string_lossy());
        let module = unsafe { LoadLibraryA(p.as_ptr()) };
        if module == 0 {
            return Err(WError::Font(format!(
                "LoadLibrary failed: {}",
                path.display()
            )));
        }
        let sym = |n: &str| -> isize {
            let c = cstr(n);
            unsafe { GetProcAddress(module, c.as_ptr()) }
        };
        unsafe {
            Ok(EncryptLib {
                module,
                init: transmute_fn(sym("init")),
                decrypt: transmute_fn(sym("decrypt")),
                encrypt: transmute_fn(sym("encrypt")),
                free: transmute_fn(sym("freeMemory")),
            })
        }
    }

    #[cfg(not(windows))]
    pub fn load(_path: &std::path::Path) -> Result<EncryptLib> {
        Err(WError::Unsupported(
            "KvenjoyEncrypt.dll 仅支持 Windows".into(),
        ))
    }

    /// 库自带的初始化（打印 "encrypt lib init"）。
    pub fn init(&self) {
        if let Some(f) = self.init {
            unsafe { f() };
        }
    }

    /// 调用 `decrypt`，返回原生缓冲区中的 `int` 序列。
    ///
    /// 返回向量第 0 个元素是长度字段（与 Java 侧 `getInt(0)` 一致），
    /// 其后为解密数据。调用方负责 `free`。
    pub fn decrypt(&self, data: &[i32], key: &[i32; 16]) -> Result<Vec<i32>> {
        let f = self
            .decrypt
            .ok_or_else(|| WError::Unsupported("decrypt symbol missing".into()))?;
        let ptr = unsafe { f(data.as_ptr(), data.len() as i32, key.as_ptr()) };
        if ptr.is_null() {
            return Err(WError::Font("decrypt returned null".into()));
        }
        let out = unsafe {
            let n = std::ptr::read(ptr as *const i32);
            if n <= 0 || n > 64_000_000 {
                self.free_ptr(ptr);
                return Err(WError::Font(format!("implausible result length {}", n)));
            }
            let slice = std::slice::from_raw_parts(ptr as *const i32, n as usize);
            let v = slice.to_vec();
            self.free_ptr(ptr);
            v
        };
        Ok(out)
    }

    /// 解密并以字节形式返回（取每个 int 的低字节）。
    pub fn decrypt_bytes(&self, data: &[i32], key: &[i32; 16]) -> Result<Vec<u8>> {
        let v = self.decrypt(data, key)?;
        Ok(v.iter().map(|&x| (x & 0xFF) as u8).collect())
    }

    pub fn encrypt(&self, data: &[i32], key: &[i32; 16]) -> Result<Vec<i32>> {
        let f = self
            .encrypt
            .ok_or_else(|| WError::Unsupported("encrypt symbol missing".into()))?;
        let ptr = unsafe { f(data.as_ptr(), data.len() as i32, key.as_ptr()) };
        if ptr.is_null() {
            return Err(WError::Font("encrypt returned null".into()));
        }
        let out = unsafe {
            let n = std::ptr::read(ptr as *const i32);
            if n <= 0 || n > 64_000_000 {
                self.free_ptr(ptr);
                return Err(WError::Font(format!("implausible result length {}", n)));
            }
            let v = std::slice::from_raw_parts(ptr as *const i32, n as usize).to_vec();
            self.free_ptr(ptr);
            v
        };
        Ok(out)
    }

    unsafe fn free_ptr(&self, p: *mut u8) {
        if let Some(f) = self.free {
            f(p);
        }
    }
}

#[cfg(windows)]
unsafe fn transmute_fn<T: Copy>(addr: isize) -> Option<T> {
    if addr == 0 {
        return None;
    }
    Some(std::mem::transmute_copy::<isize, T>(&addr))
}

#[cfg(not(windows))]
unsafe fn transmute_fn<T: Copy>(_addr: isize) -> Option<T> {
    None
}

impl Drop for EncryptLib {
    fn drop(&mut self) {
        // 不主动 FreeLibrary：保持简单，避免影响其它调用方
        let _ = self.module;
    }
}

// ---------------------------------------------------------------------------
// 字库解析（ZIP + deflate）
// ---------------------------------------------------------------------------

/// 解密后的 gfont 容器。
pub struct GfontRaw {
    pub header: GfontHeader,
    /// 原始文件字节（内嵌 ZIP）
    pub plain: Vec<u8>,
    /// 内嵌 ZIP 在文件中的起始偏移
    pub zip_base: usize,
}

impl GfontRaw {
    /// 读取一个 `.gfont`（不加密，直接定位内嵌 ZIP）。
    pub fn open(path: &std::path::Path) -> Result<GfontRaw> {
        let bytes = std::fs::read(path)?;
        let header = read_header(&bytes)?;
        let zip_base = locate_zip(&bytes)
            .ok_or_else(|| WError::Font(format!("no ZIP found in {}", path.display())))?;
        Ok(GfontRaw { header, plain: bytes, zip_base })
    }
}

/// 在文件中定位内嵌 ZIP 的起始偏移。
///
/// 做法：找到中央目录 EOCD，读取 `cd_offset`；再找到第一个中央目录条目
/// `PK\x01\x02` 的实际位置，两者相减即得 ZIP 基址。
pub fn locate_zip(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < 22 {
        return None;
    }
    // 从尾部搜 EOCD（注释最长 65535）
    let start = bytes.len().saturating_sub(22 + 65535);
    let mut eocd = None;
    let mut i = bytes.len() - 22;
    loop {
        if &bytes[i..i + 4] == b"PK\x05\x06" {
            eocd = Some(i);
            break;
        }
        if i == start {
            break;
        }
        i -= 1;
    }
    let eocd = eocd?;
    if eocd + 22 > bytes.len() {
        return None;
    }
    let cd_offset = u32::from_le_bytes([
        bytes[eocd + 16],
        bytes[eocd + 17],
        bytes[eocd + 18],
        bytes[eocd + 19],
    ]) as usize;
    // 第一个中央目录条目
    let first_cd = find_all(bytes, b"PK\x01\x02").into_iter().next()?;
    if cd_offset > first_cd {
        return None;
    }
    Some(first_cd - cd_offset)
}

fn find_all(hay: &[u8], needle: &[u8]) -> Vec<usize> {
    let mut out = Vec::new();
    if needle.is_empty() || hay.len() < needle.len() {
        return out;
    }
    for i in 0..=(hay.len() - needle.len()) {
        if &hay[i..i + needle.len()] == needle {
            out.push(i);
        }
    }
    out
}

struct ZipEntry {
    name: String,
    csize: usize,
    usize_: usize,
    crc: u32,
    method: u16,
    data_off: usize,
}

/// 在文件中定位内嵌 ZIP 的中央目录（EOCD），返回其绝对起始偏移。
///
/// EOCD 在尾部，`PK\x05\x06` 之后第 16 字节起为
/// `cd_offset`（相对 ZIP 起始 `base` 的偏移）。
fn central_dir_offset(bytes: &[u8], base: usize) -> Option<usize> {
    if bytes.len() < 22 {
        return None;
    }
    let start = bytes.len().saturating_sub(22 + 65535);
    let mut eocd = None;
    let mut i = bytes.len() - 22;
    loop {
        if &bytes[i..i + 4] == b"PK\x05\x06" {
            eocd = Some(i);
            break;
        }
        if i == start {
            break;
        }
        i -= 1;
    }
    let eocd = eocd?;
    let cd = u32::from_le_bytes([
        bytes[eocd + 16],
        bytes[eocd + 17],
        bytes[eocd + 18],
        bytes[eocd + 19],
    ]) as usize;
    Some(base + cd)
}

/// 解析内嵌 ZIP 的中央目录，返回全部条目及其数据绝对偏移。
fn zip_entries(bytes: &[u8], base: usize) -> Result<Vec<ZipEntry>> {
    let mut out = Vec::new();
    let cd_start = central_dir_offset(bytes, base)
        .ok_or_else(|| WError::Font("no ZIP EOCD found".into()))?;
    let mut i = cd_start;
    while i + 46 <= bytes.len() && &bytes[i..i + 4] == b"PK\x01\x02" {
        let nlen = u16::from_le_bytes([bytes[i + 28], bytes[i + 29]]) as usize;
        let elen = u16::from_le_bytes([bytes[i + 30], bytes[i + 31]]) as usize;
        let clen = u16::from_le_bytes([bytes[i + 32], bytes[i + 33]]) as usize;
        let method = u16::from_le_bytes([bytes[i + 10], bytes[i + 11]]);
        let crc = u32::from_le_bytes([
            bytes[i + 16], bytes[i + 17], bytes[i + 18], bytes[i + 19],
        ]);
        let csize = u32::from_le_bytes([
            bytes[i + 20], bytes[i + 21], bytes[i + 22], bytes[i + 23],
        ]) as usize;
        let usize_ = u32::from_le_bytes([
            bytes[i + 24], bytes[i + 25], bytes[i + 26], bytes[i + 27],
        ]) as usize;
        let lh = u32::from_le_bytes([
            bytes[i + 42], bytes[i + 43], bytes[i + 44], bytes[i + 45],
        ]) as usize;
        let name = String::from_utf8_lossy(&bytes[i + 46..i + 46 + nlen]).into_owned();
        // 本地头：30 字节固定 + nlen + elen
        let lhp = base + lh;
        if lhp + 30 > bytes.len() || &bytes[lhp..lhp + 4] != b"PK\x03\x04" {
            return Err(WError::Font(format!("bad local header at {}", lhp)));
        }
        let lnlen = u16::from_le_bytes([bytes[lhp + 26], bytes[lhp + 27]]) as usize;
        let lelen = u16::from_le_bytes([bytes[lhp + 28], bytes[lhp + 29]]) as usize;
        let data_off = lhp + 30 + lnlen + lelen;
        out.push(ZipEntry { name, csize, usize_, crc, method, data_off });
        i += 46 + nlen + elen + clen;
    }
    if out.is_empty() {
        return Err(WError::Font("zip central directory empty".into()));
    }
    Ok(out)
}

/// 把解压后的字形记录解析为 (码点, 点序列, 落笔标记)。
fn parse_glyph_record(data: &[u8]) -> Result<(u32, Vec<(f32, f32)>, Vec<u8>)> {
    if data.len() < 10 {
        return Err(WError::Font("glyph record too short".into()));
    }
    let cp = u16::from_be_bytes([data[0], data[1]]) as u32;
    let nfloat = u32::from_be_bytes([data[2], data[3], data[4], data[5]]) as usize;
    let mut p = 6usize;
    if p + nfloat * 4 > data.len() {
        return Err(WError::Font("glyph coordinate truncated".into()));
    }
    let mut pts = Vec::with_capacity(nfloat / 2);
    for _ in 0..(nfloat / 2) {
        let x = f32::from_be_bytes([data[p], data[p + 1], data[p + 2], data[p + 3]]);
        let y = f32::from_be_bytes([data[p + 4], data[p + 5], data[p + 6], data[p + 7]]);
        pts.push((x, y));
        p += 8;
    }
    if p + 4 > data.len() {
        return Err(WError::Font("glyph flag count truncated".into()));
    }
    let nflag = u32::from_be_bytes([data[p], data[p + 1], data[p + 2], data[p + 3]]) as usize;
    p += 4;
    if p + nflag > data.len() {
        return Err(WError::Font("glyph flags truncated".into()));
    }
    let flags = data[p..p + nflag].to_vec();
    Ok((cp, pts, flags))
}

/// 把解压后的字形记录解析为 (码点, 笔画集)。
/// 笔画切分：标记 0 = 抬笔移动（开新笔画），1 = 落笔（延续当前笔画）。
fn record_to_strokes(data: &[u8]) -> Result<(u32, Vec<Stroke>)> {
    let (cp, pts, flags) = parse_glyph_record(data)?;
    let mut strokes: Vec<Stroke> = Vec::new();
    let mut cur: Vec<Point> = Vec::new();
    for (i, &pt) in pts.iter().enumerate() {
        let down = flags.get(i).copied().unwrap_or(1) != 0;
        if down {
            cur.push(pt);
        } else {
            if cur.len() >= 2 {
                strokes.push(Stroke { points: std::mem::take(&mut cur) });
            } else {
                cur.clear();
            }
            cur.push(pt);
        }
    }
    if cur.len() >= 2 {
        strokes.push(Stroke { points: cur });
    }
    Ok((cp, strokes))
}

/// 统计字库的「典型墨迹框」。
///
/// 优先只统计 CJK 区（U+4E00..U+9FFF）——这些手写字库里 ASCII / 符号与汉字
/// 的比例差异很大，混在一起会把汉字的墨迹框带偏；CJK 不足 32 个时退回全字库。
pub fn ink_metrics_for_glyphs(
    glyphs: &std::collections::HashMap<char, Glyph>,
) -> InkMetrics {
    let cjk: Vec<&Glyph> = glyphs
        .iter()
        .filter(|(c, _)| (0x4E00..=0x9FFF).contains(&(**c as u32)))
        .map(|(_, g)| g)
        .collect();
    if cjk.len() >= 32 {
        super::ink_metrics(cjk)
    } else {
        super::ink_metrics(glyphs.values())
    }
}

/// 已解析的 `.gfont` 字库，实现 [`StrokeFont`]。
///
/// 坐标系：原点在**基线**上，x 向右、y **向下**（实测字形墨迹 y ∈ [-0.8em, +0.3em]，
/// 即字身基本位于基线上方 → 负半轴）。
/// `em` 默认取 500 字库单位（实测字形中位宽约为 0.72 em，符合手写体比例），
/// 可用 [`GfontFile::with_em`] 覆盖以做整体缩放标定。
#[derive(Clone)]
pub struct GfontFile {
    name: String,
    glyphs: std::collections::HashMap<char, Glyph>,
    em: f32,
    /// 本字库统计出的典型墨迹框（加载时算好，渲染层据此把字身居中）。
    ink: InkMetrics,
}

impl GfontFile {
    /// 读取并解析 `.gfont`。
    pub fn open(path: &std::path::Path) -> Result<GfontFile> {
        let raw = GfontRaw::open(path)?;
        let entries = zip_entries(&raw.plain, raw.zip_base)?;
        let mut glyphs = std::collections::HashMap::with_capacity(entries.len());
        for e in entries {
            if e.method != 8 {
                return Err(WError::Font(format!(
                    "unsupported zip method {} for entry {}",
                    e.method, e.name
                )));
            }
            if e.data_off + e.csize > raw.plain.len() {
                return Err(WError::Font(format!("entry {} out of range", e.name)));
            }
            let comp = &raw.plain[e.data_off..e.data_off + e.csize];
            let data = inflate(comp, e.usize_)
                .map_err(|err| WError::Font(format!("inflate {}: {:?}", e.name, err)))?;
            if data.len() != e.usize_ {
                return Err(WError::Font(format!(
                    "entry {}: inflated size {} != {}",
                    e.name,
                    data.len(),
                    e.usize_
                )));
            }
            if crc32(&data) != e.crc {
                return Err(WError::Font(format!("entry {} crc mismatch", e.name)));
            }
            let (cp, strokes) = record_to_strokes(&data)?;
            let ch = char::from_u32(cp)
                .ok_or_else(|| WError::Font(format!("bad codepoint {}", cp)))?;
            glyphs.insert(ch, Glyph::new(strokes, 0.0));
        }
        if glyphs.is_empty() {
            return Err(WError::Font("gfont has no glyphs".into()));
        }
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "gfont".into());
        let ink = ink_metrics_for_glyphs(&glyphs);
        Ok(GfontFile {
            name,
            glyphs,
            em: 500.0,
            ink,
        })
    }

    /// 覆盖 em 尺寸（字库单位），用于整体缩放标定。
    pub fn with_em(mut self, em: f32) -> Self {
        self.em = em;
        self
    }

    /// 典型墨迹框（字体单位）。
    pub fn ink_metrics(&self) -> InkMetrics {
        self.ink
    }

    /// 字形数量。
    pub fn len(&self) -> usize {
        self.glyphs.len()
    }
    pub fn is_empty(&self) -> bool {
        self.glyphs.is_empty()
    }
    /// 只读访问字形表。
    pub fn glyphs(&self) -> &std::collections::HashMap<char, Glyph> {
        &self.glyphs
    }
}

impl StrokeFont for GfontFile {
    fn name(&self) -> &str {
        &self.name
    }
    fn em(&self) -> f32 {
        self.em
    }
    fn glyph(&self, ch: char) -> Option<Glyph> {
        self.glyphs.get(&ch).map(|g| {
            let mut g = g.clone();
            g.advance = self.em;
            g
        })
    }
    fn ink(&self) -> InkMetrics {
        self.ink
    }
}

/// 解析 `.gfont` 为字形表（便捷封装）。
pub fn parse(raw: &GfontRaw) -> Result<std::collections::HashMap<char, Glyph>> {
    let entries = zip_entries(&raw.plain, raw.zip_base)?;
    let mut map = std::collections::HashMap::with_capacity(entries.len());
    for e in entries {
        if e.method != 8 {
            return Err(WError::Font(format!(
                "unsupported zip method {} for entry {}",
                e.method, e.name
            )));
        }
        let comp = &raw.plain[e.data_off..e.data_off + e.csize];
        let data = inflate(comp, e.usize_)
            .map_err(|err| WError::Font(format!("inflate {}: {:?}", e.name, err)))?;
        if crc32(&data) != e.crc {
            return Err(WError::Font(format!("entry {} crc mismatch", e.name)));
        }
        let (cp, strokes) = record_to_strokes(&data)?;
        let ch = char::from_u32(cp)
            .ok_or_else(|| WError::Font(format!("bad codepoint {}", cp)))?;
        map.insert(ch, Glyph::new(strokes, 0.0));
    }
    Ok(map)
}

// ---------------------------------------------------------------------------
// 机器指纹密钥推导（KvenjoyEncrypt / licene 用，字库本身不需要）
// ---------------------------------------------------------------------------

/// 依据 `com.kenjoy.cnc.encrypt.b` 字节码逆向出的密钥推导：
/// `key = MD5( HashSet{...}.toString() ).toUpperCase()` 的前 16 个 ASCII 字符。
///
/// 集合元素依次为：CPU ProcessorId（wmic）、MAC 地址、java.version、
/// java.vm.version、os.version。`HashSet.toString()` 的顺序由
/// `hashCode & 15` 桶序决定，此处按 Java HashMap 语义复现。
pub fn key_derive(elements: &[String]) -> Option<[u8; 16]> {
    // Java String.hashCode
    let jhash = |s: &str| -> i32 {
        let mut h: i32 = 0;
        for c in s.chars() {
            h = h.wrapping_mul(31).wrapping_add(c as i32);
        }
        h
    };
    let spread = |h: i32| -> u32 { (h as u32) ^ ((h as u32) >> 16) };
    // 默认容量 16，元素数 <= 12 不会扩容
    let mut seen: Vec<&String> = Vec::new();
    let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); 16];
    for (idx, it) in elements.iter().enumerate() {
        if seen.iter().any(|s| *s == it) {
            continue;
        }
        seen.push(it);
        let b = (spread(jhash(it)) & 15) as usize;
        buckets[b].push(idx);
    }
    let mut order: Vec<&String> = Vec::new();
    for b in &buckets {
        for &i in b {
            order.push(&elements[i]);
        }
    }
    let joined = format!(
        "[{}]",
        order
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let digest = md5(joined.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{:02X}", b)).collect();
    let mut out = [0u8; 16];
    for (i, b) in hex.bytes().take(16).enumerate() {
        out[i] = b;
    }
    Some(out)
}

/// 纯 Rust MD5（用于密钥推导，避免外部依赖）。
pub fn md5(data: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6,
        10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];
    let mut msg = data.to_vec();
    let bitlen = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bitlen.to_le_bytes());
    let mut a0: u32 = 0x67452301;
    let mut b0: u32 = 0xefcdab89;
    let mut c0: u32 = 0x98badcfe;
    let mut d0: u32 = 0x10325476;
    for chunk in msg.chunks(64) {
        let mut m = [0u32; 16];
        for (i, v) in m.iter_mut().enumerate() {
            *v = u32::from_le_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let tmp = d;
            d = c;
            c = b;
            let x = a
                .wrapping_add(f)
                .wrapping_add(K[i])
                .wrapping_add(m[g]);
            b = b.wrapping_add(x.rotate_left(S[i]));
            a = tmp;
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..16].copy_from_slice(&d0.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_of_real_gfont() {
        // 真实文件：版本 5 + 元数据长度
        let p = std::path::Path::new(
            r"D:\WritingRobot\奎享雕刻\gcodeFonts\qq英文.gfont",
        );
        if !p.exists() {
            eprintln!("skip: sample font not found");
            return;
        }
        let b = std::fs::read(p).unwrap();
        let h = read_header(&b).unwrap();
        assert_eq!(h.version, 5);
        assert!(h.meta_len >= 8);
    }

    #[test]
    fn key_hex_parses() {
        let k = Key::Hex("D164B546120B1E1A0E7DA83AED69F62E".into());
        let a = k.to_i32().unwrap();
        assert_eq!(a[0], 0xD1);
        assert_eq!(a[15], 0x2E);
    }
}
