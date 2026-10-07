//! PDF 导入：解析页面内容流，提取**矢量路径 / 嵌入图像 / 文本**，换算成页面毫米。
//!
//! 纯 Rust 实现（复用字库模块的 DEFLATE 解压器），不引入任何外部 crate。
//! 支持：
//! - 普通交叉引用表与**对象流（ObjStm）**（现代 PDF 常把页面字典压进对象流）；
//! - `FlateDecode`（含 PNG 预测器）与 `DCTDecode`（JPEG，直接透传字节）；
//! - 内容流里的路径算子（`m/l/c/v/y/h/re` + `S/s/f/F/B/b/n`），贝塞尔曲线自动展平；
//! - 图像 XObject（灰度 / RGB / CMYK / 索引色，1/8 位深）及其在页面上的位置矩形；
//! - 文本算子（`Tf/Td/TD/Tm/Tj/TJ/'/"`），带 `ToUnicode` 映射，交给上层用手写字体重排。
//!
//! 不支持：加密 PDF、JBIG2/JPX 等 exotic 滤镜、透明与渐变（对描边无意义）。

use std::collections::HashMap;

use crate::font::inflate::inflate;
use crate::render::Polyline;

/// PDF 点 → 毫米。
const PT_MM: f64 = 25.4 / 72.0;

/// PDF 导入错误。
#[derive(Debug)]
pub struct PdfError(pub String);

impl std::fmt::Display for PdfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PDF 导入失败：{}", self.0)
    }
}
impl std::error::Error for PdfError {}

type R<T> = Result<T, PdfError>;

fn err<T>(s: impl Into<String>) -> R<T> {
    Err(PdfError(s.into()))
}

// ─────────────────────────── 对象模型 ───────────────────────────

/// 一个 PDF 对象。
#[derive(Debug, Clone)]
enum Obj {
    Null,
    Bool,
    Num(f64),
    Str(Vec<u8>),
    Name(String),
    Ref(u32),
    Array(Vec<Obj>),
    Dict(HashMap<String, Obj>),
    Stream(HashMap<String, Obj>, Vec<u8>),
}

impl Obj {
    fn dict(&self) -> Option<&HashMap<String, Obj>> {
        match self {
            Obj::Dict(d) | Obj::Stream(d, _) => Some(d),
            _ => None,
        }
    }
    fn get(&self, k: &str) -> Option<&Obj> {
        self.dict().and_then(|d| d.get(k))
    }
    fn num(&self) -> Option<f64> {
        match self {
            Obj::Num(v) => Some(*v),
            _ => None,
        }
    }
    fn int(&self) -> Option<u32> {
        self.num().map(|v| v.round() as u32)
    }
    fn name(&self) -> Option<&str> {
        match self {
            Obj::Name(s) => Some(s.as_str()),
            _ => None,
        }
    }
    fn arr(&self) -> Option<&Vec<Obj>> {
        match self {
            Obj::Array(a) => Some(a),
            _ => None,
        }
    }
    fn rf(&self) -> Option<u32> {
        match self {
            Obj::Ref(n) => Some(*n),
            _ => None,
        }
    }
}

fn is_ws(b: u8) -> bool {
    matches!(b, 0 | 9 | 10 | 12 | 13 | 32)
}
fn is_delim(b: u8) -> bool {
    matches!(b, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}
fn is_regular(b: u8) -> bool {
    !is_ws(b) && !is_delim(b)
}

// ─────────────────────────── 词法 / 语法解析 ───────────────────────────

struct P<'a> {
    d: &'a [u8],
    i: usize,
}

impl<'a> P<'a> {
    fn ws(&mut self) {
        while self.i < self.d.len() {
            let b = self.d[self.i];
            if is_ws(b) {
                self.i += 1;
            } else if b == b'%' {
                while self.i < self.d.len() && self.d[self.i] != b'\n' {
                    self.i += 1;
                }
            } else {
                break;
            }
        }
    }
    fn eat(&mut self, kw: &[u8]) -> bool {
        self.ws();
        if self.i + kw.len() <= self.d.len() && &self.d[self.i..self.i + kw.len()] == kw {
            self.i += kw.len();
            true
        } else {
            false
        }
    }

    /// 解析任意对象（不含间接引用的自动解引用）。
    fn obj(&mut self) -> R<Obj> {
        self.ws();
        if self.i >= self.d.len() {
            return err("意外结束");
        }
        match self.d[self.i] {
            b'/' => {
                self.i += 1;
                let mut s = String::new();
                while self.i < self.d.len() && is_regular(self.d[self.i]) {
                    if self.d[self.i] == b'#' && self.i + 2 < self.d.len() {
                        if let (Some(h1), Some(h2)) = (
                            hexv(self.d[self.i + 1]),
                            hexv(self.d[self.i + 2]),
                        ) {
                            s.push((h1 * 16 + h2) as char);
                            self.i += 3;
                            continue;
                        }
                    }
                    s.push(self.d[self.i] as char);
                    self.i += 1;
                }
                Ok(Obj::Name(s))
            }
            b'(' => {
                self.i += 1;
                let mut v = Vec::new();
                let mut depth = 1i32;
                while self.i < self.d.len() && depth > 0 {
                    let b = self.d[self.i];
                    match b {
                        b'\\' => {
                            self.i += 1;
                            if self.i >= self.d.len() {
                                break;
                            }
                            let c = self.d[self.i];
                            self.i += 1;
                            match c {
                                b'n' => v.push(b'\n'),
                                b'r' => v.push(b'\r'),
                                b't' => v.push(b'\t'),
                                b'b' => v.push(8),
                                b'f' => v.push(12),
                                b'(' => v.push(b'('),
                                b')' => v.push(b')'),
                                b'\\' => v.push(b'\\'),
                                b'\n' => {}
                                c if c.is_ascii_digit() => {
                                    let mut o = (c - b'0') as u32;
                                    for _ in 0..2 {
                                        if self.i < self.d.len()
                                            && self.d[self.i].is_ascii_digit()
                                        {
                                            o = o * 8 + (self.d[self.i] - b'0') as u32;
                                            self.i += 1;
                                        } else {
                                            break;
                                        }
                                    }
                                    v.push((o & 0xFF) as u8);
                                }
                                other => v.push(other),
                            }
                        }
                        b'(' => {
                            depth += 1;
                            v.push(b'(');
                            self.i += 1;
                        }
                        b')' => {
                            depth -= 1;
                            self.i += 1;
                            if depth > 0 {
                                v.push(b')');
                            }
                        }
                        _ => {
                            v.push(b);
                            self.i += 1;
                        }
                    }
                }
                Ok(Obj::Str(v))
            }
            b'<' => {
                if self.i + 1 < self.d.len() && self.d[self.i + 1] == b'<' {
                    self.i += 2;
                    return self.dict();
                }
                self.i += 1;
                let mut hex = Vec::new();
                while self.i < self.d.len() && self.d[self.i] != b'>' {
                    if let Some(h) = hexv(self.d[self.i]) {
                        hex.push(h);
                    }
                    self.i += 1;
                }
                self.i += 1;
                let mut v = Vec::with_capacity(hex.len() / 2 + 1);
                for pair in hex.chunks(2) {
                    if pair.len() == 2 {
                        v.push(pair[0] * 16 + pair[1]);
                    } else {
                        v.push(pair[0] * 16);
                    }
                }
                Ok(Obj::Str(v))
            }
            b'[' => {
                self.i += 1;
                let mut a = Vec::new();
                loop {
                    self.ws();
                    if self.i >= self.d.len() {
                        break;
                    }
                    if self.d[self.i] == b']' {
                        self.i += 1;
                        break;
                    }
                    a.push(self.obj()?);
                }
                Ok(Obj::Array(a))
            }
            b']' | b')' | b'>' | b'}' | b'{' => err("多余的分隔符"),
            _ => {
                // 数字 / 引用 / 关键字
                let start = self.i;
                while self.i < self.d.len()
                    && (self.d[self.i].is_ascii_digit()
                        || matches!(self.d[self.i], b'+' | b'-' | b'.' | b'e' | b'E'))
                {
                    self.i += 1;
                }
                if self.i > start {
                    let s = std::str::from_utf8(&self.d[start..self.i]).unwrap_or("0");
                    let v = s.parse::<f64>().unwrap_or(0.0);
                    self.ws();
                    // 可能是「N G R」间接引用：先记下位置，判定失败要**完整回退**，
                    // 否则数组里的 `[0 0 283.46 283.46]` 会被误当成引用吃掉后面的数。
                    let saved = self.i;
                    if self.i + 1 < self.d.len() && self.d[self.i].is_ascii_digit() {
                        let gs = self.i;
                        while self.i < self.d.len() && self.d[self.i].is_ascii_digit() {
                            self.i += 1;
                        }
                        let ge = self.i;
                        self.ws();
                        if self.i < self.d.len() && self.d[self.i] == b'R' {
                            self.i += 1;
                            let gen = std::str::from_utf8(&self.d[gs..ge])
                                .ok()
                                .and_then(|s| s.parse::<u32>().ok())
                                .unwrap_or(0);
                            let _ = gen;
                            return Ok(Obj::Ref(v as u32));
                        }
                        self.i = saved;
                    }
                    return Ok(Obj::Num(v));
                }
                // 关键字
                while self.i < self.d.len() && is_regular(self.d[self.i]) {
                    self.i += 1;
                }
                let s = std::str::from_utf8(&self.d[start..self.i])
                    .unwrap_or("")
                    .to_string();
                match s.as_str() {
                    "true" | "false" => Ok(Obj::Bool),
                    "null" => Ok(Obj::Null),
                    _ => err(format!("无法识别的关键字 {s}")),
                }
            }
        }
    }

    fn dict(&mut self) -> R<Obj> {
        let mut d = HashMap::new();
        loop {
            self.ws();
            if self.i >= self.d.len() {
                break;
            }
            if self.d[self.i] == b'>' && self.i + 1 < self.d.len() && self.d[self.i + 1] == b'>' {
                self.i += 2;
                break;
            }
            let k = match self.obj()? {
                Obj::Name(n) => n,
                _ => {
                    // 键不是名字：跳过一个值
                    let _ = self.obj()?;
                    continue;
                }
            };
            let v = self.obj()?;
            d.insert(k, v);
        }
        Ok(Obj::Dict(d))
    }
}

fn hexv(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

// ─────────────────────────── 文档结构 ───────────────────────────

struct Pdf {
    objs: HashMap<u32, Obj>,
}

impl Pdf {
    /// 解析整个文件：扫出全部间接对象（含对象流里的）。
    fn parse(d: &[u8]) -> R<Pdf> {
        if !d.starts_with(b"%PDF") {
            // 允许前面有垃圾字节（有些生成器会加 BOM/注释）
            let head = &d[..d.len().min(1024)];
            if !head.windows(5).any(|w| w == b"%PDF-") {
                return err("不是 PDF 文件（缺少 %PDF 头）");
            }
        }
        let mut objs: HashMap<u32, Obj> = HashMap::new();
        let offsets = scan_objects(d);
        for (num, off) in offsets {
            let mut p = P { d, i: off };
            // 对象头是「编号 生成号 obj」：只用词法器吃编号，生成号直接跳过数字，
            // 免得把后面的 obj 关键字当成对象体吃掉。
            let _ = p.obj().ok();
            p.ws();
            while p.i < d.len() && d[p.i].is_ascii_digit() {
                p.i += 1;
            }
            if !p.eat(b"obj") {
                continue;
            }
            if let Ok(o) = p.obj() {
                // 可能是流
                let o = if p.eat(b"stream") {
                    if d[p.i] == b'\r' {
                        p.i += 1;
                    }
                    if d.get(p.i) == Some(&b'\n') {
                        p.i += 1;
                    }
                    let dict = match &o {
                        Obj::Dict(dd) => dd.clone(),
                        _ => HashMap::new(),
                    };
                    let start = p.i;
                    let len = dict
                        .get("Length")
                        .and_then(|v| v.int())
                        .unwrap_or(0) as usize;
                    let end = if len > 0 && start + len <= d.len() {
                        let mut e = start + len;
                        // 容忍 endstream 前的换行
                        while e < d.len() && is_ws(d[e]) {
                            e += 1;
                        }
                        if e + 9 <= d.len() && &d[e..e + 9] == b"endstream" {
                            start + len
                        } else {
                            find_end(d, start, b"endstream").unwrap_or(d.len())
                        }
                    } else {
                        find_end(d, start, b"endstream").unwrap_or(d.len())
                    };
                    Obj::Stream(dict, d[start..end.min(d.len())].to_vec())
                } else {
                    o
                };
                objs.insert(num, o);
            }
        }
        // 对象流：把压缩在里面的对象也展开（现代 PDF 的页面字典常在这里）
        let stm_ids: Vec<u32> = objs
            .iter()
            .filter(|(_, o)| o.get("Type").and_then(|v| v.name()) == Some("ObjStm"))
            .map(|(n, _)| *n)
            .collect();
        for id in stm_ids {
            // 先取出展开结果，再写回（避免同时借 objs 的可变与不可变引用）
            if let Ok(pairs) = unpack_objstm(&objs, id) {
                for (num, obj) in pairs {
                    objs.entry(num).or_insert(obj);
                }
            }
        }
        if objs.is_empty() {
            return err("没有解析到任何 PDF 对象（文件可能已损坏或加密）");
        }
        Ok(Pdf { objs })
    }

    fn get(&self, n: u32) -> Option<&Obj> {
        self.objs.get(&n)
    }

    fn catalog(&self) -> R<&HashMap<String, Obj>> {
        for o in self.objs.values() {
            if let Some(d) = o.dict() {
                if d.get("Type").and_then(|v| v.name()) == Some("Catalog") {
                    return Ok(d);
                }
            }
        }
        err("找不到文档目录（Catalog）")
    }

    fn pages(&self) -> R<Vec<u32>> {
        let cat = self.catalog()?;
        let root = cat
            .get("Pages")
            .and_then(|v| v.rf())
            .ok_or_else(|| PdfError("目录里没有 /Pages".into()))?;
        let mut out = Vec::new();
        self.collect_pages(root, 0, &mut out)?;
        if out.is_empty() {
            return err("这个 PDF 没有任何页面");
        }
        Ok(out)
    }

    fn collect_pages(&self, node: u32, depth: usize, out: &mut Vec<u32>) -> R<()> {
        if depth > 32 {
            return Ok(());
        }
        let o = match self.get(node) {
            Some(o) => o,
            None => return Ok(()),
        };
        let d = match o.dict() {
            Some(d) => d,
            None => return Ok(()),
        };
        match d.get("Type").and_then(|v| v.name()).unwrap_or("") {
            "Page" => out.push(node),
            _ => {
                if let Some(kids) = d.get("Kids").and_then(|v| v.arr()) {
                    for k in kids {
                        if let Some(n) = k.rf() {
                            self.collect_pages(n, depth + 1, out)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// 扫出文件里全部 `N G obj` 的对象编号与偏移。
fn scan_objects(d: &[u8]) -> HashMap<u32, usize> {
    let mut m = HashMap::new();
    let n = d.len();
    if n < 4 {
        return m;
    }
    let mut i = 0usize;
    while i + 3 < n {
        if d[i] == b'o' && d[i + 1] == b'b' && d[i + 2] == b'j' && i > 0 && is_ws(d[i - 1]) {
            let mut k = i - 1;
            while k > 0 && is_ws(d[k]) {
                k -= 1;
            }
            // 生成号
            let e = k + 1;
            let mut s = k;
            while s > 0 && d[s - 1].is_ascii_digit() {
                s -= 1;
            }
            if s < e {
                let mut k2 = s;
                while k2 > 0 && is_ws(d[k2 - 1]) {
                    k2 -= 1;
                }
                let e2 = k2;
                let mut s2 = k2;
                while s2 > 0 && d[s2 - 1].is_ascii_digit() {
                    s2 -= 1;
                }
                if s2 < e2 && (s2 == 0 || is_ws(d[s2 - 1]) || is_delim(d[s2 - 1])) {
                    if let Ok(num) = std::str::from_utf8(&d[s2..e2])
                        .unwrap_or("")
                        .parse::<u32>()
                    {
                        m.insert(num, s2);
                    }
                }
            }
            i += 3;
            continue;
        }
        i += 1;
    }
    m
}

fn find_end(d: &[u8], from: usize, kw: &[u8]) -> Option<usize> {
    let mut i = from;
    while i + kw.len() <= d.len() {
        if &d[i..i + kw.len()] == kw {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// 展开一个对象流（ObjStm）：`N 对 (对象号 偏移)` + 首对象数据。
fn unpack_objstm(objs: &HashMap<u32, Obj>, id: u32) -> R<Vec<(u32, Obj)>> {
    let mut out: Vec<(u32, Obj)> = Vec::new();
    let (dict, raw) = match objs.get(&id) {
        Some(Obj::Stream(d, s)) => (d.clone(), s.clone()),
        _ => return err("对象流不是流对象"),
    };
    let data = decode_stream(objs, &dict, &raw)?;
    let n = dict.get("N").and_then(|v| v.int()).unwrap_or(0) as usize;
    let first = dict.get("First").and_then(|v| v.int()).unwrap_or(0) as usize;
    if n == 0 || first >= data.len() {
        return Ok(out);
    }
    // 解析前 N 对整数
    let mut nums: Vec<u32> = Vec::with_capacity(n * 2);
    let mut cur = 0u32;
    let mut started = false;
    let mut i = 0usize;
    let head = &data[..first];
    while i < head.len() {
        let b = head[i];
        if b.is_ascii_digit() {
            cur = cur * 10 + (b - b'0') as u32;
            started = true;
        } else if started {
            nums.push(cur);
            cur = 0;
            started = false;
        }
        i += 1;
    }
    if started {
        nums.push(cur);
    }
    for k in 0..n {
        let num = match nums.get(k * 2) {
            Some(v) => *v,
            None => break,
        };
        let off = match nums.get(k * 2 + 1) {
            Some(v) => *v as usize,
            None => break,
        };
        let start = first + off;
        if start >= data.len() {
            continue;
        }
        let mut p = P { d: &data, i: start };
        if let Ok(o) = p.obj() {
            out.push((num, o));
        }
    }
    Ok(out)
}

// ─────────────────────────── 流解码 ───────────────────────────

fn decode_stream(objs: &HashMap<u32, Obj>, dict: &HashMap<String, Obj>, raw: &[u8]) -> R<Vec<u8>> {
    let mut out = raw.to_vec();
    let filters: Vec<String> = match dict.get("Filter") {
        Some(Obj::Name(n)) => vec![n.clone()],
        Some(Obj::Array(a)) => a.iter().filter_map(|v| v.name().map(|s| s.to_string())).collect(),
        Some(o) => {
            if let Some(n) = objs.get(&o.rf().unwrap_or(0)).and_then(|v| v.name()) {
                vec![n.to_string()]
            } else {
                vec![]
            }
        }
        None => vec![],
    };
    for (k, f) in filters.iter().enumerate() {
        let parms = match dict.get("DecodeParms") {
            Some(Obj::Array(a)) => a.get(k).cloned(),
            other => other.cloned(),
        };
        match f.as_str() {
            "FlateDecode" => {
                // 去掉 2 字节 zlib 头
                let src = if out.len() > 2 && out[0] == 0x78 {
                    &out[2..]
                } else {
                    &out[..]
                };
                let hint = dict.get("Length1").and_then(|v| v.int()).unwrap_or(0) as usize;
                let mut v = inflate(src, hint.max(4096)).map_err(|e| PdfError(format!("FlateDecode 失败：{e:?}")))?;
                if let Some(p) = parms.and_then(|p| match p {
                    Obj::Ref(n) => objs.get(&n).cloned(),
                    other => Some(other),
                }) {
                    if let Some(pd) = p.dict() {
                        let pred = pd.get("Predictor").and_then(|v| v.int()).unwrap_or(1);
                        if pred >= 10 {
                            let colors = pd.get("Colors").and_then(|v| v.int()).unwrap_or(1) as usize;
                            let bpc = pd.get("BitsPerComponent").and_then(|v| v.int()).unwrap_or(8) as usize;
                            let cols = pd.get("Columns").and_then(|v| v.int()).unwrap_or(1) as usize;
                            v = png_unpredict(&v, colors.max(1), bpc.max(8) / 8, cols.max(1));
                        }
                    }
                }
                out = v;
            }
            "LZWDecode" | "Fl" => {
                out = lzw_decode(&out);
            }
            "ASCIIHexDecode" => {
                let mut hex = Vec::new();
                for b in &out {
                    if let Some(h) = hexv(*b) {
                        hex.push(h);
                    } else if *b == b'>' {
                        break;
                    }
                }
                let mut v = Vec::with_capacity(hex.len() / 2 + 1);
                for pair in hex.chunks(2) {
                    if pair.len() == 2 {
                        v.push(pair[0] * 16 + pair[1]);
                    } else {
                        v.push(pair[0] * 16);
                    }
                }
                out = v;
            }
            "DCTDecode" | "JPXDecode" | "JBIG2Decode" | "CCITTFaxDecode" => {
                // 交给上层（JPEG 字节直接透传）
            }
            "" | "None" => {}
            other => return err(format!("暂不支持的滤镜 {other}")),
        }
    }
    Ok(out)
}

/// PNG 预测器还原（PDF 里 Flate 图像常用）。
fn png_unpredict(data: &[u8], colors: usize, bpc_bytes: usize, cols: usize) -> Vec<u8> {
    let bpp = colors * bpc_bytes; // 每像素字节数
    let stride = cols * colors * bpc_bytes;
    if stride == 0 || data.is_empty() {
        return data.to_vec();
    }
    let rows = data.len() / (stride + 1);
    let mut out = vec![0u8; rows * stride];
    let mut prev = vec![0u8; stride];
    for r in 0..rows {
        let ft = data[r * (stride + 1)];
        let src = &data[r * (stride + 1) + 1..r * (stride + 1) + 1 + stride];
        let mut cur = vec![0u8; stride];
        for i in 0..stride {
            let a = if i >= bpp { cur[i - bpp] } else { 0 };
            let b = prev[i];
            let c = if i >= bpp { prev[i - bpp] } else { 0 };
            let x = src[i];
            cur[i] = match ft {
                0 => x,
                1 => x.wrapping_add(a),
                2 => x.wrapping_add(b),
                3 => x.wrapping_add(((a as u16 + b as u16) / 2) as u8),
                4 => x.wrapping_add(paeth(a, b, c)),
                _ => x,
            };
        }
        out[r * stride..(r + 1) * stride].copy_from_slice(&cur);
        prev = cur;
    }
    out
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i32 + b as i32 - c as i32;
    let pa = (p - a as i32).abs();
    let pb = (p - b as i32).abs();
    let pc = (p - c as i32).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// 极简 LZW 解码（老 PDF 偶有使用）。
fn lzw_decode(data: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut dict: Vec<Vec<u8>> = Vec::new();
    for i in 0..256u32 {
        dict.push(vec![i as u8]);
    }
    dict.push(Vec::new()); // 256 clear
    dict.push(Vec::new()); // 257 eod
    let mut bitpos = 0usize;
    let mut code_len = 9u32;
    let total_bits = data.len() * 8;
    let mut prev: Option<Vec<u8>> = None;
    let bit_at = |pos: usize| -> u8 {
        let byte = data[pos / 8];
        (byte >> (7 - (pos % 8))) & 1
    };
    while bitpos + code_len as usize <= total_bits {
        let mut code = 0u32;
        for k in 0..code_len {
            code = (code << 1) | bit_at(bitpos + k as usize) as u32;
        }
        bitpos += code_len as usize;
        if code == 256 {
            dict.truncate(258);
            code_len = 9;
            prev = None;
            continue;
        }
        if code == 257 {
            break;
        }
        let entry: Vec<u8> = if (code as usize) < dict.len() {
            dict[code as usize].clone()
        } else if let Some(p) = &prev {
            let mut v = p.clone();
            v.push(p[0]);
            v
        } else {
            break;
        };
        out.extend_from_slice(&entry);
        if let Some(p) = &prev {
            let mut nv = p.clone();
            nv.push(entry[0]);
            dict.push(nv);
            if dict.len() as u32 >= (1u32 << code_len) && code_len < 12 {
                code_len += 1;
            }
        }
        prev = Some(entry);
    }
    out
}

// ─────────────────────────── 导入选项与结果 ───────────────────────────

/// PDF 导入参数。
#[derive(Debug, Clone, Copy)]
pub struct PdfImportOpts {
    /// 贝塞尔曲线展平精度（毫米，越小越平滑、点越多）
    pub curve_mm: f64,
    /// 小于此尺寸（毫米）的路径视为噪点丢弃
    pub min_path_mm: f64,
}

impl Default for PdfImportOpts {
    fn default() -> Self {
        PdfImportOpts {
            curve_mm: 0.25,
            min_path_mm: 0.3,
        }
    }
}

/// 页面上一个矩形（毫米，左上原点、Y 向下，与其它模块一致）。
#[derive(Debug, Clone, Copy)]
pub struct PdfRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// 图像像素数据（已解码为可直接用 `image` 打开的原始样本）。
#[derive(Debug, Clone)]
pub enum PdfImageData {
    /// 灰度 8 位，长度 = w*h
    Gray(Vec<u8>),
    /// RGB 8 位，长度 = w*h*3
    Rgb(Vec<u8>),
    /// JPEG 原始字节（DCTDecode 直接透传，交给上层解码）
    Jpeg(Vec<u8>),
}

/// 页面里的一张嵌入图像及其位置。
#[derive(Debug, Clone)]
pub struct PdfImage {
    pub w: u32,
    pub h: u32,
    pub data: PdfImageData,
    pub rect: PdfRect,
}

/// 一段文本（位置为**基线左端**，毫米；上层可用自己的字库重排）。
#[derive(Debug, Clone)]
pub struct PdfTextRun {
    pub text: String,
    /// 左端 X（毫米）
    pub x_mm: f64,
    /// 基线 Y（毫米）
    pub baseline_mm: f64,
    /// 字号（毫米）
    pub size_mm: f64,
}

/// 一页的导入结果。
#[derive(Debug, Clone, Default)]
pub struct PdfPage {
    /// 页面物理宽（毫米，来自 MediaBox）
    pub w_mm: f64,
    /// 页面物理高（毫米）
    pub h_mm: f64,
    /// 矢量路径（页面毫米，左上原点、Y 向下）
    pub paths: Vec<Polyline>,
    /// 嵌入图像
    pub images: Vec<PdfImage>,
    /// 文本（未描边，供上层用字库重排）
    pub text_runs: Vec<PdfTextRun>,
}

// ─────────────────────────── 公开接口 ───────────────────────────

/// 读取 PDF 页数（失败时返回错误说明）。
pub fn page_count(data: &[u8]) -> R<usize> {
    let pdf = Pdf::parse(data)?;
    Ok(pdf.pages()?.len())
}

/// 导入第 `idx` 页（**从 0 开始**）。
pub fn import_page(data: &[u8], idx: usize, opts: &PdfImportOpts) -> R<PdfPage> {
    let pdf = Pdf::parse(data)?;
    if pdf.objs.values().any(|o| {
        o.dict()
            .and_then(|d| d.get("Encrypt"))
            .is_some()
    }) || data.windows(8).any(|w| w == b"/Encrypt")
    {
        return err("这个 PDF 被加密（需要口令），请先解除保护再导入");
    }
    let pages = pdf.pages()?;
    let id = *pages.get(idx).ok_or_else(|| {
        PdfError(format!(
            "页码超出范围：共 {} 页，请求第 {} 页",
            pages.len(),
            idx + 1
        ))
    })?;
    import_page_obj(&pdf, id, opts, 0)
}

fn import_page_obj(pdf: &Pdf, id: u32, opts: &PdfImportOpts, depth: usize) -> R<PdfPage> {
    if depth > 8 {
        return err("页面嵌套过深");
    }
    let d = pdf
        .get(id)
        .and_then(|o| o.dict())
        .ok_or_else(|| PdfError("找不到页面对象".into()))?;

    // MediaBox（PDF 点，左下原点）
    let mb: Vec<f64> = d
        .get("MediaBox")
        .and_then(|v| v.arr())
        .map(|a| a.iter().filter_map(|v| v.num()).collect())
        .unwrap_or_else(|| vec![0.0, 0.0, 595.276, 841.89]);
    let (llx, lly, urx, ury) = if mb.len() >= 4 {
        (mb[0], mb[1], mb[2], mb[3])
    } else {
        (0.0, 0.0, 595.276, 841.89)
    };
    let w_mm = (urx - llx).abs() * PT_MM;
    let h_mm = (ury - lly).abs() * PT_MM;

    // 资源：可能直接内联，也可能是引用；页面没有时向上找 Pages 节点的继承资源
    let res = resolve_dict(pdf, d.get("Resources")).unwrap_or_else(|| {
        let mut node = d.get("Parent").and_then(|v| v.rf());
        let mut depth = 0;
        while let Some(n) = node {
            if depth > 8 {
                break;
            }
            if let Some(pd) = pdf.get(n).and_then(|o| o.dict()) {
                if let Some(r) = resolve_dict(pdf, pd.get("Resources")) {
                    return r;
                }
                node = pd.get("Parent").and_then(|v| v.rf());
            } else {
                break;
            }
            depth += 1;
        }
        HashMap::new()
    });

    // 内容流
    let content = match d.get("Contents") {
        Some(o) => join_contents(pdf, o)?,
        None => Vec::new(),
    };

    let mut itp = Interp {
        pdf,
        opts: *opts,
        llx,
        ury,
        w_mm,
        h_mm,
        out: Vec::new(),
        images: Vec::new(),
        runs: Vec::new(),
        ctm: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        gstack: Vec::new(),
        path: Vec::new(),
        fill: [0.0, 0.0, 0.0],
        stroke: [0.0, 0.0, 0.0],
        fonts: HashMap::new(),
        to_unicode: HashMap::new(),
        depth,
    };
    let res0 = Res::new(pdf, &res);
    if !content.is_empty() {
        itp.run(&content, &res0)?;
    }
    Ok(PdfPage {
        w_mm,
        h_mm,
        paths: itp.out,
        images: itp.images,
        text_runs: itp.runs,
    })
}

/// 取一个字典对象：内联字典直接返回，间接引用则解引用（引用还可能指向对象流里的对象）。
fn resolve_dict(pdf: &Pdf, o: Option<&Obj>) -> Option<HashMap<String, Obj>> {
    let o = o?;
    match o {
        Obj::Dict(d) => Some(d.clone()),
        Obj::Ref(n) => pdf.get(*n).and_then(|v| v.dict()).cloned(),
        _ => None,
    }
}

fn join_contents(pdf: &Pdf, o: &Obj) -> R<Vec<u8>> {
    let mut out = Vec::new();
    match o {
        Obj::Array(a) => {
            for e in a {
                if let Some(n) = e.rf() {
                    if let Some(Obj::Stream(dict, raw)) = pdf.get(n) {
                        let v = decode_stream(&pdf.objs, dict, raw)?;
                        out.extend_from_slice(&v);
                        out.push(b'\n');
                    }
                }
            }
        }
        other => {
            if let Some(n) = other.rf() {
                if let Some(Obj::Stream(dict, raw)) = pdf.get(n) {
                    let v = decode_stream(&pdf.objs, dict, raw)?;
                    out.extend_from_slice(&v);
                }
            }
        }
    }
    Ok(out)
}

// ─────────────────────────── 内容流解释 ───────────────────────────

/// 当前资源（图像 / 字体 / 表单）。
struct Res {
    xobj: HashMap<String, Obj>,
    font: HashMap<String, Obj>,
}

impl Res {
    fn new(pdf: &Pdf, d: &HashMap<String, Obj>) -> Res {
        let sub = |k: &str| -> HashMap<String, Obj> {
            d.get(k)
                .and_then(|v| match v {
                    Obj::Dict(dd) => Some(dd.clone()),
                    Obj::Ref(n) => pdf.get(*n).and_then(|o| o.dict()).cloned(),
                    _ => None,
                })
                .unwrap_or_default()
        };
        Res {
            xobj: sub("XObject"),
            font: sub("Font"),
        }
    }
}

struct Interp<'a> {
    pdf: &'a Pdf,
    opts: PdfImportOpts,
    llx: f64,
    ury: f64,
    w_mm: f64,
    h_mm: f64,
    out: Vec<Polyline>,
    images: Vec<PdfImage>,
    runs: Vec<PdfTextRun>,
    ctm: [f64; 6],
    gstack: Vec<[f64; 6]>,
    path: Vec<Vec<(f64, f64)>>,
    fill: [f64; 3],
    stroke: [f64; 3],
    fonts: HashMap<String, f64>,
    to_unicode: HashMap<String, HashMap<u32, String>>,
    depth: usize,
}

impl<'a> Interp<'a> {
    /// 用户空间 → 页面毫米（左上原点、Y 向下）。
    fn to_mm(&self, x: f64, y: f64) -> (f64, f64) {
        let m = self.ctm;
        let dx = m[0] * x + m[2] * y + m[4];
        let dy = m[1] * x + m[3] * y + m[5];
        ((dx - self.llx) * PT_MM, (self.ury - dy) * PT_MM)
    }

    fn emit_path(&mut self, kind: Paint) {
        if self.path.is_empty() {
            return;
        }
        // 近白色的填充/描边一律跳过：PDF 里的整页白底矩形最常见，
        // 描它的边等于白画一个大框。
        let col = match kind {
            Paint::Fill => self.fill,
            Paint::Stroke => self.stroke,
        };
        if col[0] > 0.95 && col[1] > 0.95 && col[2] > 0.95 {
            self.path.clear();
            return;
        }
        let mut pts_all: Vec<Vec<(f64, f64)>> = Vec::new();
        for sp in &self.path {
            let v: Vec<(f64, f64)> = sp.iter().map(|&(x, y)| self.to_mm(x, y)).collect();
            if v.len() >= 2 {
                pts_all.push(v);
            }
        }
        // 覆盖整页的填充块（背景）不描边
        if matches!(kind, Paint::Fill) && !pts_all.is_empty() {
            let (x0, y0, x1, y1) = bounds_of(&pts_all);
            let area = (x1 - x0) * (y1 - y0);
            if area > self.w_mm * self.h_mm * 0.9 {
                self.path.clear();
                return;
            }
        }
        for v in pts_all {
            // 太短的笔画是噪点
            let len: f64 = v.windows(2)
                .map(|w| ((w[1].0 - w[0].0).powi(2) + (w[1].1 - w[0].1).powi(2)).sqrt())
                .sum();
            if len < self.opts.min_path_mm {
                continue;
            }
            self.out.push(Polyline::new(v));
        }
        self.path.clear();
    }

    fn run(&mut self, content: &[u8], res: &Res) -> R<()> {
        let mut c = Cs { d: content, i: 0 };
        // 文本状态
        let mut tm = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0f64];
        let mut tlm = tm;
        let mut font_name = String::new();
        let mut font_size = 0.0f64;
        let mut leading = 0.0f64;
        let mut in_text = false;
        let mut args: Vec<Tok> = Vec::new();
        let mut cs: f64 = 0.0;

        while let Some(t) = c.next() {
            let op = match t {
                Tok::Op(s) => s,
                other => {
                    args.push(other);
                    continue;
                }
            };
            let a = std::mem::take(&mut args);
            let nums: Vec<f64> = a.iter().filter_map(|t| match t {
                Tok::Num(v) => Some(*v),
                _ => None,
            }).collect();
            match op.as_str() {
                // ── 图形状态 ──
                "q" => self.gstack.push(self.ctm),
                "Q" => {
                    if let Some(m) = self.gstack.pop() {
                        self.ctm = m;
                    }
                }
                "cm" => {
                    if nums.len() >= 6 {
                        let n = [nums[0], nums[1], nums[2], nums[3], nums[4], nums[5]];
                        self.ctm = mul(n, self.ctm);
                    }
                }
                "gs" | "w" | "J" | "j" | "M" | "d" | "ri" | "i" => {}
                "g" => {
                    if let Some(v) = nums.first() {
                        self.fill = gray_rgb(*v);
                    }
                }
                "G" => {
                    if let Some(v) = nums.first() {
                        self.stroke = gray_rgb(*v);
                    }
                }
                "rg" => {
                    if nums.len() >= 3 {
                        self.fill = [nums[0], nums[1], nums[2]];
                    }
                }
                "RG" => {
                    if nums.len() >= 3 {
                        self.stroke = [nums[0], nums[1], nums[2]];
                    }
                }
                "k" => {
                    if nums.len() >= 4 {
                        self.fill = cmyk_rgb(&nums[0..4]);
                    }
                }
                "K" => {
                    if nums.len() >= 4 {
                        self.stroke = cmyk_rgb(&nums[0..4]);
                    }
                }
                "cs" | "CS" | "sc" | "SC" | "scn" | "SCN" => {
                    // 复杂色彩空间：取前三个数值当 RGB 近似（够用于判断「是不是白底」）
                    if nums.len() >= 3 {
                        let c3 = [nums[0], nums[1], nums[2]];
                        if op.starts_with('s') || op == "sc" || op == "scn" {
                            self.fill = c3;
                        } else {
                            self.stroke = c3;
                        }
                    }
                }
                // ── 路径构造 ──
                "m" => {
                    if nums.len() >= 2 {
                        self.path.push(vec![(nums[0], nums[1])]);
                    }
                }
                "l" => {
                    if nums.len() >= 2 {
                        if self.path.is_empty() {
                            self.path.push(Vec::new());
                        }
                        self.path.last_mut().unwrap().push((nums[0], nums[1]));
                    }
                }
                "c" => {
                    if nums.len() >= 6 {
                        let p = self.cur_pt();
                        self.push_curve(p, (nums[0], nums[1]), (nums[2], nums[3]), (nums[4], nums[5]));
                    }
                }
                "v" => {
                    if nums.len() >= 4 {
                        let p = self.cur_pt();
                        self.push_curve(p, p, (nums[0], nums[1]), (nums[2], nums[3]));
                    }
                }
                "y" => {
                    if nums.len() >= 4 {
                        let p = self.cur_pt();
                        self.push_curve(p, (nums[0], nums[1]), (nums[2], nums[3]), (nums[2], nums[3]));
                    }
                }
                "re" => {
                    if nums.len() >= 4 {
                        let (x, y, w, h) = (nums[0], nums[1], nums[2], nums[3]);
                        self.path.push(vec![
                            (x, y),
                            (x + w, y),
                            (x + w, y + h),
                            (x, y + h),
                            (x, y),
                        ]);
                    }
                }
                "h" => {
                    if let Some(sp) = self.path.last_mut() {
                        if sp.len() >= 2 && sp.first() != sp.last() {
                            let f = *sp.first().unwrap();
                            sp.push(f);
                        }
                    }
                }
                // ── 绘制 ──
                "S" | "s" => self.emit_path(Paint::Stroke),
                "f" | "F" | "f*" => self.emit_path(Paint::Fill),
                "B" | "B*" | "b" | "b*" => self.emit_path(Paint::Fill),
                "n" | "W" | "W*" => {
                    self.path.clear();
                }
                // ── XObject ──
                "Do" => {
                    let name = a.iter().find_map(|t| match t {
                        Tok::Name(n) => Some(n.clone()),
                        _ => None,
                    });
                    if let Some(n) = name {
                        let _ = self.do_xobject(&n, res);
                    }
                }
                "BI" => {
                    // 内联图像：跳过二进制数据
                    if let Some(off) = find_end(c.d, c.i, b"EI") {
                        c.i = off + 2;
                    } else {
                        c.i = c.d.len();
                    }
                }
                // ── 文本 ──
                "BT" => {
                    in_text = true;
                    tm = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
                    tlm = tm;
                }
                "ET" => {
                    in_text = false;
                }
                "Tf" => {
                    if let Some(Tok::Name(n)) = a.first() {
                        font_name = n.clone();
                    }
                    if let Some(v) = nums.first() {
                        font_size = *v;
                    }
                }
                "Td" => {
                    if nums.len() >= 2 {
                        tlm = mul([1.0, 0.0, 0.0, 1.0, nums[0], nums[1]], tlm);
                        tm = tlm;
                    }
                }
                "TD" => {
                    if nums.len() >= 2 {
                        leading = -nums[1];
                        tlm = mul([1.0, 0.0, 0.0, 1.0, nums[0], nums[1]], tlm);
                        tm = tlm;
                    }
                }
                "Tm" => {
                    if nums.len() >= 6 {
                        tm = [nums[0], nums[1], nums[2], nums[3], nums[4], nums[5]];
                        tlm = tm;
                    }
                }
                "TL" => {
                    if let Some(v) = nums.first() {
                        leading = *v;
                    }
                }
                "T*" => {
                    tlm = mul([1.0, 0.0, 0.0, 1.0, 0.0, -leading], tlm);
                    tm = tlm;
                }
                "Tc" => {
                    if let Some(v) = nums.first() {
                        cs = *v;
                    }
                }
                "Tw" | "Tz" | "Ts" | "Tr" => {}
                "Tj" => {
                    if in_text {
                        if let Some(Tok::Str(s)) = a.first() {
                            self.show_text(s, &tm, font_size, &font_name, res);
                        }
                    }
                }
                "TJ" => {
                    if in_text {
                        if let Some(Tok::Arr(items)) = a.first() {
                            for it in items {
                                match it {
                                    Tok::Str(s) => {
                                        self.show_text(s, &tm, font_size, &font_name, res);
                                        let w = self.string_width(s, font_size, &font_name);
                                        tm = mul([1.0, 0.0, 0.0, 1.0, w + cs, 0.0], tm);
                                    }
                                    Tok::Num(v) => {
                                        tm = mul(
                                            [1.0, 0.0, 0.0, 1.0, -v / 1000.0 * font_size, 0.0],
                                            tm,
                                        );
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                "'" => {
                    if in_text {
                        tlm = mul([1.0, 0.0, 0.0, 1.0, 0.0, -leading], tlm);
                        tm = tlm;
                        if let Some(Tok::Str(s)) = a.first() {
                            self.show_text(s, &tm, font_size, &font_name, res);
                        }
                    }
                }
                "\"" => {
                    if in_text {
                        if let Some(Tok::Str(s)) = a.last() {
                            self.show_text(s, &tm, font_size, &font_name, res);
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn cur_pt(&self) -> (f64, f64) {
        self.path
            .last()
            .and_then(|sp| sp.last().copied())
            .unwrap_or((0.0, 0.0))
    }

    fn push_curve(
        &mut self,
        p0: (f64, f64),
        p1: (f64, f64),
        p2: (f64, f64),
        p3: (f64, f64),
    ) {
        if self.path.is_empty() {
            self.path.push(vec![p0]);
        }
        // 展平：按控制多边形在设备空间的长度决定分段数
        let scale = {
            let a = self.to_mm(0.0, 0.0);
            let b = self.to_mm(1.0, 0.0);
            ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt().max(1e-6)
        };
        let poly = dist(p0, p1) + dist(p1, p2) + dist(p2, p3);
        let n = ((poly * scale) / self.opts.curve_mm.max(0.02))
            .ceil()
            .clamp(4.0, 64.0) as usize;
        // 先算好点，再一次性写入子路径（避免同时借 self 的可变与不可变引用）
        let mut pts: Vec<(f64, f64)> = Vec::with_capacity(n);
        for k in 1..=n {
            let t = k as f64 / n as f64;
            pts.push(cubic(p0, p1, p2, p3, t));
        }
        let sp = self.path.last_mut().unwrap();
        sp.extend(pts);
    }

    fn do_xobject(&mut self, name: &str, res: &Res) -> R<()> {
        let o = match res.xobj.get(name) {
            Some(o) => o.clone(),
            None => return Ok(()),
        };
        let (dict, raw) = match &o {
            Obj::Stream(d, s) => (d.clone(), s.clone()),
            Obj::Ref(n) => match self.pdf.get(*n) {
                Some(Obj::Stream(d, s)) => (d.clone(), s.clone()),
                _ => return Ok(()),
            },
            _ => return Ok(()),
        };
        match dict.get("Subtype").and_then(|v| v.name()).unwrap_or("") {
            "Image" => self.add_image(&dict, &raw),
            "Form" => {
                let saved = self.ctm;
                if let Some(m) = dict.get("Matrix").and_then(|v| v.arr()) {
                    let v: Vec<f64> = m.iter().filter_map(|x| x.num()).collect();
                    if v.len() >= 6 {
                        self.ctm = mul([v[0], v[1], v[2], v[3], v[4], v[5]], self.ctm);
                    }
                }
                let sub_res = resolve_dict(self.pdf, dict.get("Resources")).unwrap_or_default();
                let sub = if sub_res.is_empty() {
                    res.clone_ref()
                } else {
                    Res::new(self.pdf, &sub_res)
                };
                let content = decode_stream(&self.pdf.objs, &dict, &raw)?;
                if self.depth < 6 {
                    self.depth += 1;
                    self.run(&content, &sub)?;
                    self.depth -= 1;
                }
                self.ctm = saved;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn add_image(&mut self, dict: &HashMap<String, Obj>, raw: &[u8]) -> R<()> {
        let w = dict.get("Width").and_then(|v| v.int()).unwrap_or(0);
        let h = dict.get("Height").and_then(|v| v.int()).unwrap_or(0);
        if w == 0 || h == 0 {
            return Ok(());
        }
        // 位置：图像占单位正方形，按当前 CTM 求四角外接矩形
        let cs = [
            self.to_mm(0.0, 0.0),
            self.to_mm(1.0, 0.0),
            self.to_mm(0.0, 1.0),
            self.to_mm(1.0, 1.0),
        ];
        let mut x0 = f64::MAX;
        let mut y0 = f64::MAX;
        let mut x1 = f64::MIN;
        let mut y1 = f64::MIN;
        for (x, y) in cs {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
        let rect = PdfRect {
            x: x0,
            y: y0,
            w: (x1 - x0).max(0.1),
            h: (y1 - y0).max(0.1),
        };
        let filters: Vec<String> = match dict.get("Filter") {
            Some(Obj::Name(n)) => vec![n.clone()],
            Some(Obj::Array(a)) => a.iter().filter_map(|v| v.name().map(|s| s.to_string())).collect(),
            _ => vec![],
        };
        if filters.iter().any(|f| f == "DCTDecode") {
            self.images.push(PdfImage {
                w,
                h,
                data: PdfImageData::Jpeg(raw.to_vec()),
                rect,
            });
            return Ok(());
        }
        let samples = decode_stream(&self.pdf.objs, dict, raw)?;
        let bpc = dict.get("BitsPerComponent").and_then(|v| v.int()).unwrap_or(8);
        let cs_obj = dict.get("ColorSpace");
        let cs_name = cs_obj.and_then(|v| v.name()).unwrap_or("DeviceGray").to_string();
        let cs_arr = cs_obj.and_then(|v| v.arr()).cloned();
        let (kind, ncomp) = match cs_name.as_str() {
            "DeviceRGB" | "RGB" => (1usize, 3usize),
            "DeviceCMYK" | "CMYK" => (2, 4),
            "DeviceGray" | "G" | "CalGray" => (0, 1),
            "Indexed" | "I" => (3, 1),
            _ => {
                if let Some(a) = &cs_arr {
                    match a.first().and_then(|v| v.name()).unwrap_or("") {
                        "ICCBased" => {
                            let n = a.get(1)
                                .and_then(|v| v.rf())
                                .and_then(|n2| self.pdf.get(n2))
                                .and_then(|o| o.get("N").and_then(|x| x.int()))
                                .unwrap_or(3) as usize;
                            (if n == 1 { 0 } else { 1 }, n)
                        }
                        "Indexed" => (3, 1),
                        _ => (0, 1),
                    }
                } else {
                    (0, 1)
                }
            }
        };
        let mut px: Vec<u8> = match bpc {
            8 => samples.clone(),
            1 => unpack_bits(&samples, w as usize, h as usize, 1),
            2 => unpack_bits(&samples, w as usize, h as usize, 2),
            4 => unpack_bits(&samples, w as usize, h as usize, 4),
            _ => samples.clone(),
        };
        let expect = (w as usize) * (h as usize) * ncomp;
        if px.len() < expect {
            // 数据不足：补零（坏文件也要能继续跑）
            px.resize(expect, 0);
        }
        let data = match kind {
            0 => PdfImageData::Gray(px),
            1 => PdfImageData::Rgb(px),
            2 => {
                // CMYK → 灰度（转 RGB 也行，灰度足够描边用）
                let mut g = Vec::with_capacity((w * h) as usize);
                for i in 0..(w * h) as usize {
                    let (c, m, y, k) = (
                        px[i * 4] as f64 / 255.0,
                        px[i * 4 + 1] as f64 / 255.0,
                        px[i * 4 + 2] as f64 / 255.0,
                        px[i * 4 + 3] as f64 / 255.0,
                    );
                    let r = (1.0 - (c * (1.0 - k) + k)) * 255.0;
                    let gg = (1.0 - (m * (1.0 - k) + k)) * 255.0;
                    let b = (1.0 - (y * (1.0 - k) + k)) * 255.0;
                    let lum = (0.299 * r + 0.587 * gg + 0.114 * b).clamp(0.0, 255.0);
                    g.push(lum as u8);
                }
                PdfImageData::Gray(g)
            }
            _ => {
                // 索引色：查调色板转 RGB
                let pal = cs_arr
                    .as_ref()
                    .and_then(|a| a.get(2))
                    .and_then(|v| match v {
                        Obj::Str(s) => Some(s.clone()),
                        Obj::Ref(n) => match self.pdf.get(*n) {
                            Some(Obj::Str(s)) => Some(s.clone()),
                            Some(Obj::Stream(_, s)) => Some(s.clone()),
                            _ => None,
                        },
                        _ => None,
                    })
                    .unwrap_or_default();
                let mut rgb = Vec::with_capacity((w * h * 3) as usize);
                for i in 0..(w * h) as usize {
                    let idx = px[i] as usize;
                    let (r, g, b) = if idx * 3 + 2 < pal.len() {
                        (pal[idx * 3], pal[idx * 3 + 1], pal[idx * 3 + 2])
                    } else {
                        (0, 0, 0)
                    };
                    rgb.push(r);
                    rgb.push(g);
                    rgb.push(b);
                }
                PdfImageData::Rgb(rgb)
            }
        };
        self.images.push(PdfImage { w, h, data, rect });
        Ok(())
    }

    // ── 文本 ──

    fn unicode_map(&mut self, font: &str, res: &Res) -> HashMap<u32, String> {
        if self.to_unicode.contains_key(font) {
            return self.to_unicode[font].clone();
        }
        let mut m: HashMap<u32, String> = HashMap::new();
        if let Some(fd) = res.font.get(font) {
            let fd = match fd {
                Obj::Ref(n) => self.pdf.get(*n).cloned(),
                other => Some(other.clone()),
            };
            if let Some(fd) = fd.and_then(|o| o.dict().cloned()) {
                if let Some(Obj::Stream(sd, s)) = fd
                    .get("ToUnicode")
                    .and_then(|v| v.rf())
                    .and_then(|n| self.pdf.get(n))
                {
                    // 必须带上流自己的字典（Filter 在那里），否则解不开压缩的 CMap
                    let sd = sd.clone();
                    let s = s.clone();
                    let cmap = decode_stream(&self.pdf.objs, &sd, &s).unwrap_or(s);
                    parse_cmap(&String::from_utf8_lossy(&cmap), &mut m);
                }
            }
        }
        self.to_unicode.insert(font.to_string(), m.clone());
        m
    }

    fn decode_str(&mut self, s: &[u8], font: &str, res: &Res) -> String {
        let map = self.unicode_map(font, res);
        if !map.is_empty() {
            // 先试双字节（CID），命中率低则退回单字节
            let mut out2 = String::new();
            let mut ok2 = true;
            let mut i = 0usize;
            while i + 1 < s.len() {
                let code = ((s[i] as u32) << 8) | s[i + 1] as u32;
                match map.get(&code) {
                    Some(v) => out2.push_str(v),
                    None => {
                        ok2 = false;
                        break;
                    }
                }
                i += 2;
            }
            if ok2 && !out2.is_empty() && s.len() % 2 == 0 {
                return out2;
            }
            let mut out1 = String::new();
            for &b in s {
                match map.get(&(b as u32)) {
                    Some(v) => out1.push_str(v),
                    None => out1.push(b as char),
                }
            }
            if out1.chars().any(|c| c as u32 > 127) || out1.chars().any(|c| c.is_alphanumeric()) {
                return out1;
            }
        }
        // 退回：单字节按 Latin-1 解释
        s.iter().map(|&b| b as char).collect()
    }

    fn string_width(&self, s: &[u8], size: f64, _font: &str) -> f64 {
        let mut w = 0.0;
        for &b in s {
            w += size * if b > 127 { 1.0 } else { 0.5 };
        }
        w
    }

    fn show_text(
        &mut self,
        s: &[u8],
        tm: &[f64; 6],
        size_pt: f64,
        font: &str,
        res: &Res,
    ) {
        // 无 ToUnicode 的 CID 字体解出来是控制字符——宁可丢弃，也不要写进页面
        let raw = self.decode_str(s, font, res);
        let clean: String = raw
            .chars()
            .filter(|c| !c.is_control() || *c == '\n')
            .collect();
        let text = clean.trim().to_string();
        if text.is_empty() || size_pt <= 0.0 {
            return;
        }
        let scale = (tm[0] * tm[0] + tm[1] * tm[1]).sqrt().max(1e-6);
        let (x_mm, y_mm) = self.to_mm(tm[4], tm[5]);
        let size_mm = size_pt * scale * PT_MM;
        if size_mm < 0.4 || size_mm > 300.0 {
            return;
        }
        let _ = self.fonts.insert(font.to_string(), size_pt);
        self.runs.push(PdfTextRun {
            text,
            x_mm,
            baseline_mm: y_mm,
            size_mm,
        });
    }
}

impl Res {
    fn clone_ref(&self) -> Res {
        Res {
            xobj: self.xobj.clone(),
            font: self.font.clone(),
        }
    }
}

enum Paint {
    Fill,
    Stroke,
}

fn bounds_of(paths: &[Vec<(f64, f64)>]) -> (f64, f64, f64, f64) {
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for sp in paths {
        for &(x, y) in sp {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    if x0 == f64::MAX {
        (0.0, 0.0, 0.0, 0.0)
    } else {
        (x0, y0, x1, y1)
    }
}

fn mul(m: [f64; 6], n: [f64; 6]) -> [f64; 6] {
    let (a1, b1, c1, d1, e1, f1) = (m[0], m[1], m[2], m[3], m[4], m[5]);
    let (a2, b2, c2, d2, e2, f2) = (n[0], n[1], n[2], n[3], n[4], n[5]);
    [
        a1 * a2 + c1 * b2,
        b1 * a2 + d1 * b2,
        a1 * c2 + c1 * d2,
        b1 * c2 + d1 * d2,
        a1 * e2 + c1 * f2 + e1,
        b1 * e2 + d1 * f2 + f1,
    ]
}

fn dist(a: (f64, f64), b: (f64, f64)) -> f64 {
    ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt()
}

fn cubic(p0: (f64, f64), p1: (f64, f64), p2: (f64, f64), p3: (f64, f64), t: f64) -> (f64, f64) {
    let u = 1.0 - t;
    let a = u * u * u;
    let b = 3.0 * u * u * t;
    let c = 3.0 * u * t * t;
    let d = t * t * t;
    (
        a * p0.0 + b * p1.0 + c * p2.0 + d * p3.0,
        a * p0.1 + b * p1.1 + c * p2.1 + d * p3.1,
    )
}

fn gray_rgb(v: f64) -> [f64; 3] {
    [v, v, v]
}

fn cmyk_rgb(v: &[f64]) -> [f64; 3] {
    let (c, m, y, k) = (v[0], v[1], v[2], v[3]);
    [
        1.0 - (c * (1.0 - k) + k).min(1.0),
        1.0 - (m * (1.0 - k) + k).min(1.0),
        1.0 - (y * (1.0 - k) + k).min(1.0),
    ]
}

/// 位深 < 8 的样本解包成每像素 1 字节。
fn unpack_bits(data: &[u8], w: usize, h: usize, bpc: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(w * h);
    let mask = (1u32 << bpc) - 1;
    let mut bitpos = 0usize;
    for _ in 0..h {
        for _ in 0..w {
            let byte = data.get(bitpos / 8).copied().unwrap_or(0);
            let bit = bitpos % 8;
            let shift = (8u32 - bpc).saturating_sub(bit as u32);
            let v = (byte as u32 >> shift) & mask;
            out.push(((v * 255) / mask.max(1)) as u8);
            bitpos += bpc as usize;
        }
        // 每行按字节对齐
        bitpos = (bitpos + 7) / 8 * 8;
    }
    out
}

/// 解析 ToUnicode CMap 里的 `bfchar` / `bfrange`。
fn parse_cmap(s: &str, out: &mut HashMap<u32, String>) {
    let mut lines = s.lines();
    while let Some(line) = lines.next() {
        let t = line.trim();
        if t.contains("beginbfchar") {
            while let Some(l) = lines.next() {
                let l = l.trim();
                if l.starts_with("endbfchar") {
                    break;
                }
                // <src> <dst>
                let mut it = l.split_whitespace();
                let a = it.next().unwrap_or("");
                let b = it.next().unwrap_or("");
                if let (Some(code), Some(uni)) = (hex_code(a), hex_code(b)) {
                    out.insert(code, uni_to_str(uni));
                }
            }
        } else if t.contains("beginbfrange") {
            while let Some(l) = lines.next() {
                let l = l.trim();
                if l.starts_with("endbfrange") {
                    break;
                }
                let mut it = l.split_whitespace();
                let a = it.next().unwrap_or("");
                let b = it.next().unwrap_or("");
                let c = it.next().unwrap_or("");
                if let (Some(lo), Some(hi)) = (hex_code(a), hex_code(b)) {
                    if c.starts_with('[') {
                        // 数组形式
                        let items: Vec<&str> = l
                            .split('[')
                            .nth(1)
                            .unwrap_or("")
                            .split(']')
                            .next()
                            .unwrap_or("")
                            .split_whitespace()
                            .collect();
                        for (k, item) in items.iter().enumerate() {
                            if let Some(u) = hex_code(item) {
                                out.insert(lo + k as u32, uni_to_str(u));
                            }
                        }
                    } else if let Some(u) = hex_code(c) {
                        for k in 0..=(hi.saturating_sub(lo)) {
                            out.insert(lo + k, uni_to_str(u + k));
                        }
                    }
                }
            }
        }
    }
}

fn hex_code(s: &str) -> Option<u32> {
    let s = s.trim().trim_matches('<').trim_matches('>').trim();
    if s.is_empty() {
        return None;
    }
    u32::from_str_radix(s, 16).ok()
}

fn uni_to_str(u: u32) -> String {
    char::from_u32(u).map(|c| c.to_string()).unwrap_or_default()
}

// ─────────────────────────── 内容流词法 ───────────────────────────

#[derive(Debug, Clone)]
enum Tok {
    Num(f64),
    Str(Vec<u8>),
    Name(String),
    Arr(Vec<Tok>),
    Op(String),
}

struct Cs<'a> {
    d: &'a [u8],
    i: usize,
}

impl<'a> Cs<'a> {
    fn ws(&mut self) {
        while self.i < self.d.len() {
            let b = self.d[self.i];
            if is_ws(b) {
                self.i += 1;
            } else if b == b'%' {
                while self.i < self.d.len() && self.d[self.i] != b'\n' {
                    self.i += 1;
                }
            } else {
                break;
            }
        }
    }

    fn next(&mut self) -> Option<Tok> {
        self.ws();
        if self.i >= self.d.len() {
            return None;
        }
        let b = self.d[self.i];
        match b {
            b'/' => {
                self.i += 1;
                let mut s = String::new();
                while self.i < self.d.len() && is_regular(self.d[self.i]) {
                    s.push(self.d[self.i] as char);
                    self.i += 1;
                }
                Some(Tok::Name(s))
            }
            b'(' => {
                self.i += 1;
                let mut v = Vec::new();
                let mut depth = 1i32;
                while self.i < self.d.len() && depth > 0 {
                    let c = self.d[self.i];
                    if c == b'\\' && self.i + 1 < self.d.len() {
                        self.i += 1;
                        let e = self.d[self.i];
                        self.i += 1;
                        match e {
                            b'n' => v.push(b'\n'),
                            b'r' => v.push(b'\r'),
                            b't' => v.push(b'\t'),
                            b'(' | b')' | b'\\' => v.push(e),
                            other => v.push(other),
                        }
                        continue;
                    }
                    if c == b'(' {
                        depth += 1;
                    } else if c == b')' {
                        depth -= 1;
                        self.i += 1;
                        if depth == 0 {
                            break;
                        }
                        v.push(b')');
                        continue;
                    }
                    v.push(c);
                    self.i += 1;
                }
                Some(Tok::Str(v))
            }
            b'<' => {
                if self.d.get(self.i + 1) == Some(&b'<') {
                    // 属性字典（BDC/DP 之类）：对描边无意义，整段跳过
                    let mut depth = 0i32;
                    while self.i + 1 < self.d.len() {
                        if self.d[self.i] == b'<' && self.d[self.i + 1] == b'<' {
                            depth += 1;
                            self.i += 2;
                            continue;
                        }
                        if self.d[self.i] == b'>' && self.d[self.i + 1] == b'>' {
                            depth -= 1;
                            self.i += 2;
                            if depth <= 0 {
                                break;
                            }
                            continue;
                        }
                        self.i += 1;
                    }
                    return None;
                }
                self.i += 1;
                let mut hex = Vec::new();
                while self.i < self.d.len() && self.d[self.i] != b'>' {
                    if let Some(h) = hexv(self.d[self.i]) {
                        hex.push(h);
                    }
                    self.i += 1;
                }
                self.i += 1;
                let mut v = Vec::with_capacity(hex.len() / 2 + 1);
                for pair in hex.chunks(2) {
                    if pair.len() == 2 {
                        v.push(pair[0] * 16 + pair[1]);
                    } else {
                        v.push(pair[0] * 16);
                    }
                }
                Some(Tok::Str(v))
            }
            b'[' => {
                self.i += 1;
                let mut v = Vec::new();
                loop {
                    self.ws();
                    if self.i >= self.d.len() {
                        break;
                    }
                    if self.d[self.i] == b']' {
                        self.i += 1;
                        break;
                    }
                    match self.next() {
                        Some(t) => v.push(t),
                        None => break,
                    }
                }
                Some(Tok::Arr(v))
            }
            b']' | b')' | b'>' | b'{' | b'}' => {
                self.i += 1;
                None
            }
            _ => {
                if b.is_ascii_digit() || matches!(b, b'+' | b'-' | b'.') {
                    let start = self.i;
                    while self.i < self.d.len()
                        && (self.d[self.i].is_ascii_digit()
                            || matches!(self.d[self.i], b'+' | b'-' | b'.' | b'e' | b'E'))
                    {
                        self.i += 1;
                    }
                    let s = std::str::from_utf8(&self.d[start..self.i]).unwrap_or("0");
                    return Some(Tok::Num(s.parse::<f64>().unwrap_or(0.0)));
                }
                let start = self.i;
                while self.i < self.d.len() && is_regular(self.d[self.i]) {
                    self.i += 1;
                }
                if self.i == start {
                    self.i += 1;
                    return None;
                }
                let s = String::from_utf8_lossy(&self.d[start..self.i]).to_string();
                Some(Tok::Op(s))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用本模块的 PDF 导出器造一个 PDF，再导入回来，验证「导出→导入」往返。


    #[test]
    fn roundtrip_export_import() {
        let paths = vec![
            Polyline::new(vec![(10.0, 10.0), (60.0, 10.0)]),
            Polyline::new(vec![(10.0, 30.0), (60.0, 30.0), (60.0, 60.0)]),
        ];
        let bytes = crate::pdf::to_pdf(&paths, 100.0, 100.0);
        let n = page_count(&bytes).unwrap();
        assert_eq!(n, 1, "导出的 PDF 应有 1 页");
        let pg = import_page(&bytes, 0, &PdfImportOpts::default()).unwrap();
        assert!((pg.w_mm - 100.0).abs() < 0.5, "页宽应为 100mm，实际 {}", pg.w_mm);
        assert!((pg.h_mm - 100.0).abs() < 0.5, "页高应为 100mm");
        assert_eq!(pg.paths.len(), 2, "应还原出 2 条折线，实际 {}", pg.paths.len());
        // Y 已翻成「向下」，第一条横线 y≈10
        let (x0, y0, x1, y1) = pg.paths[0].bounds();
        assert!((y0 - 10.0).abs() < 0.5 && (y1 - 10.0).abs() < 0.5, "横线 y 应为 10：{y0}/{y1}");
        assert!((x0 - 10.0).abs() < 0.5 && (x1 - 60.0).abs() < 0.5, "横线 x 应为 10→60");
    }

    #[test]
    fn rejects_non_pdf() {
        assert!(page_count(b"hello world, not a pdf").is_err());
    }

    #[test]
    fn page_index_out_of_range() {
        let bytes = crate::pdf::to_pdf(&[Polyline::new(vec![(0.0, 0.0), (1.0, 1.0)])], 50.0, 50.0);
        assert!(import_page(&bytes, 5, &PdfImportOpts::default()).is_err());
    }

    #[test]
    fn decimate_and_matrix_helpers() {
        // mul：单位矩阵相乘不变
        let id = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        let m = mul([1.0, 0.0, 0.0, 1.0, 5.0, 6.0], id);
        assert!((m[4] - 5.0).abs() < 1e-9 && (m[5] - 6.0).abs() < 1e-9);
        // 位解包
        let v = unpack_bits(&[0b1010_0000], 2, 1, 1);
        assert_eq!(v, vec![255, 0]);
    }



















    /// 真实 PDF（本工作区自带的说明书）端到端导入。
    #[test]
    fn imports_real_pdf() {
        let path = "D:/WritingRobot/4.教案本操作图文版.pdf";
        let Ok(data) = std::fs::read(path) else {
            eprintln!("跳过：找不到 {}", path);
            return;
        };
        let n = page_count(&data).expect("应能读出页数");
        assert!(n > 5, "这份说明书应有几十页，实际 {}", n);

        // 第 2 页：文字 + 图片
        let pg = import_page(&data, 1, &PdfImportOpts::default()).expect("第 2 页应能导入");
        assert!(pg.w_mm > 100.0 && pg.h_mm > 100.0, "页面尺寸异常 {}x{}", pg.w_mm, pg.h_mm);
        assert!(!pg.text_runs.is_empty(), "应有文字");
        let joined: String = pg.text_runs.iter().map(|r| r.text.clone()).collect();
        assert!(
            joined.chars().any(|c| c as u32 > 0x2000),
            "中文应通过 ToUnicode 正确解出，实际：{}",
            joined
        );
        for r in &pg.text_runs {
            assert!(r.size_mm > 0.0 && r.size_mm < 200.0, "字号异常 {}", r.size_mm);
        }
        if !pg.images.is_empty() {
            let im = &pg.images[0];
            assert!(im.w > 0 && im.h > 0, "图像尺寸异常");
            assert!(im.rect.w > 1.0 && im.rect.h > 1.0, "图像位置矩形异常");
        }
    }

    /// 白底填充矩形必须被跳过（PDF 几乎每页都有一个整页白矩形）。
    #[test]
    fn skips_white_background_rect() {
        let paths = vec![Polyline::new(vec![(10.0, 10.0), (60.0, 10.0)])];
        let mut bytes = crate::pdf::to_pdf(&paths, 100.0, 100.0);
        // 手工塞一个「1 1 1 rg 0 0 283 283 re f」的整页白底
        let s = String::from_utf8_lossy(&bytes).to_string();
        let patched = s.replace("0.08 w 0 0 0 RG", "0.08 w 0 0 0 RG
1 1 1 rg 0 0 283 283 re f");
        bytes = patched.into_bytes();
        let pg = import_page(&bytes, 0, &PdfImportOpts::default()).unwrap();
        // 只有原来那条横线，白底矩形不应出现
        assert_eq!(pg.paths.len(), 1, "整页白底矩形应被跳过，实际 {} 条", pg.paths.len());
    }

}
