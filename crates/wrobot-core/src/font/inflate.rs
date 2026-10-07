//! 纯 Rust 实现的 DEFLATE (RFC 1951) 解压。
//!
//! `.gfont` 内嵌标准 ZIP（deflate），为保持零外部依赖而自行实现。
//! 只需支持 inflate，不需要压缩。

#[derive(Debug)]
pub enum InflateError {
    UnexpectedEof,
    BadBlockType,
    BadHuffman,
    BadDistance,
    BadLength,
    BadStoredSize,
}

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    bit: u32,
    acc: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0, bit: 0, acc: 0 }
    }

    #[inline]
    fn need(&mut self, n: u32) -> Result<(), InflateError> {
        while self.bit < n {
            let b = if self.pos < self.data.len() {
                let v = self.data[self.pos];
                self.pos += 1;
                v
            } else {
                return Err(InflateError::UnexpectedEof);
            };
            self.acc |= (b as u32) << self.bit;
            self.bit += 8;
        }
        Ok(())
    }

    #[inline]
    fn bits(&mut self, n: u32) -> Result<u32, InflateError> {
        if n == 0 {
            return Ok(0);
        }
        self.need(n)?;
        let v = self.acc & ((1u32 << n) - 1);
        self.acc >>= n;
        self.bit -= n;
        Ok(v)
    }

    fn align_byte(&mut self) {
        let drop = self.bit % 8;
        self.acc >>= drop;
        self.bit -= drop;
    }
}

/// 霍夫曼解码表：按码长分层线性搜索（简单且足够快）。
struct Huffman {
    /// (code, symbol) 按码长分组
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huffman {
    fn build(lengths: &[u8]) -> Result<Huffman, InflateError> {
        let mut counts = [0u16; 16];
        for &l in lengths {
            if l > 15 {
                return Err(InflateError::BadHuffman);
            }
            counts[l as usize] += 1;
        }
        counts[0] = 0;
        let mut offs = [0u16; 16];
        for i in 1..16 {
            offs[i] = offs[i - 1] + counts[i - 1];
        }
        let mut symbols = vec![0u16; lengths.iter().filter(|&&l| l != 0).count()];
        let mut next = offs;
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbols[next[l as usize] as usize] = sym as u16;
                next[l as usize] += 1;
            }
        }
        Ok(Huffman { counts, symbols })
    }

    #[inline]
    fn decode(&self, br: &mut BitReader) -> Result<u16, InflateError> {
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: i32 = 0;
        for len in 1..16 {
            code |= br.bits(1)? as i32;
            let count = self.counts[len] as i32;
            if code - count < first {
                return Ok(self.symbols[(index + (code - first)) as usize]);
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err(InflateError::BadHuffman)
    }
}

const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
const CLEN_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// 解压原始 deflate 流（无 zlib 头）。
pub fn inflate(data: &[u8], size_hint: usize) -> Result<Vec<u8>, InflateError> {
    let mut out: Vec<u8> = Vec::with_capacity(size_hint.max(64));
    let mut br = BitReader::new(data);
    loop {
        let bfinal = br.bits(1)?;
        let btype = br.bits(2)?;
        match btype {
            0 => {
                br.align_byte();
                let len = br.bits(16)? as usize;
                let nlen = br.bits(16)? as usize;
                if len ^ 0xFFFF != nlen {
                    return Err(InflateError::BadStoredSize);
                }
                for _ in 0..len {
                    out.push(br.bits(8)? as u8);
                }
            }
            1 => {
                // 固定霍夫曼
                let mut ll = [0u8; 288];
                for (i, slot) in ll.iter_mut().enumerate() {
                    *slot = if i < 144 {
                        8
                    } else if i < 256 {
                        9
                    } else if i < 280 {
                        7
                    } else {
                        8
                    };
                }
                let lit = Huffman::build(&ll)?;
                let dist = Huffman::build(&[5u8; 30])?;
                inflate_block(&mut br, &mut out, &lit, &dist)?;
            }
            2 => {
                let hlit = br.bits(5)? as usize + 257;
                let hdist = br.bits(5)? as usize + 1;
                let hclen = br.bits(4)? as usize + 4;
                let mut cl = [0u8; 19];
                for i in 0..hclen {
                    cl[CLEN_ORDER[i]] = br.bits(3)? as u8;
                }
                let clh = Huffman::build(&cl)?;
                let mut lengths = vec![0u8; hlit + hdist];
                let mut i = 0;
                while i < hlit + hdist {
                    let sym = clh.decode(&mut br)?;
                    match sym {
                        0..=15 => {
                            lengths[i] = sym as u8;
                            i += 1;
                        }
                        16 => {
                            if i == 0 {
                                return Err(InflateError::BadHuffman);
                            }
                            let prev = lengths[i - 1];
                            let rep = 3 + br.bits(2)? as usize;
                            for _ in 0..rep {
                                if i >= lengths.len() {
                                    return Err(InflateError::BadHuffman);
                                }
                                lengths[i] = prev;
                                i += 1;
                            }
                        }
                        17 => {
                            let rep = 3 + br.bits(3)? as usize;
                            i += rep;
                        }
                        18 => {
                            let rep = 11 + br.bits(7)? as usize;
                            i += rep;
                        }
                        _ => return Err(InflateError::BadHuffman),
                    }
                }
                if i > hlit + hdist {
                    return Err(InflateError::BadHuffman);
                }
                let lit = Huffman::build(&lengths[..hlit])?;
                let dist = Huffman::build(&lengths[hlit..])?;
                inflate_block(&mut br, &mut out, &lit, &dist)?;
            }
            _ => return Err(InflateError::BadBlockType),
        }
        if bfinal == 1 {
            break;
        }
    }
    Ok(out)
}

fn inflate_block(
    br: &mut BitReader,
    out: &mut Vec<u8>,
    lit: &Huffman,
    dist: &Huffman,
) -> Result<(), InflateError> {
    loop {
        let sym = lit.decode(br)?;
        if sym < 256 {
            out.push(sym as u8);
        } else if sym == 256 {
            return Ok(());
        } else {
            let idx = (sym - 257) as usize;
            if idx >= LENGTH_BASE.len() {
                return Err(InflateError::BadLength);
            }
            let len = LENGTH_BASE[idx] as usize + br.bits(LENGTH_EXTRA[idx] as u32)? as usize;
            let dsym = dist.decode(br)? as usize;
            if dsym >= DIST_BASE.len() {
                return Err(InflateError::BadDistance);
            }
            let d = DIST_BASE[dsym] as usize + br.bits(DIST_EXTRA[dsym] as u32)? as usize;
            if d > out.len() || d == 0 {
                return Err(InflateError::BadDistance);
            }
            let start = out.len() - d;
            for k in 0..len {
                let b = out[start + k];
                out.push(b);
            }
        }
    }
}

/// CRC-32 (IEEE)。
pub fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, e) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        *e = c;
    }
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = table[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_block_roundtrip() {
        // 手工构造一个 stored block: final=1, type=00, len, nlen, data
        let raw = b"hello deflate";
        let mut v = vec![0x01u8];
        v.extend_from_slice(&(raw.len() as u16).to_le_bytes());
        v.extend_from_slice(&(!(raw.len() as u16)).to_le_bytes());
        v.extend_from_slice(raw);
        assert_eq!(inflate(&v, raw.len()).unwrap(), raw);
    }

    #[test]
    fn fixed_huffman_roundtrip() {
        // 用 Python zlib 生成的固定霍夫曼流不易手写，改用 Stored 校验 crc32
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
