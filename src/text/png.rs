//! 最小 PNG 解码器：只用于字体内嵌的 CBDT/CBLC 位图 emoji。
//!
//! 不引 `png`/`flate2`：字体里的 emoji PNG 很小（几十到几百像素），这里实现一个
//! 覆盖 8-bit、非隔行、RGB/RGBA/灰度的 DEFLATE + PNG filter 解码即可，零依赖。
//! 其他格式（16-bit、隔行、调色板）返回 `None`，调用方退回轮廓渲染。

/// LSB-first 位读取器（DEFLATE 的位序）。
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn read(&mut self, n: usize) -> Option<u32> {
        let mut v = 0u32;
        for i in 0..n {
            let byte = *self.data.get(self.pos / 8)?;
            let bit = (byte >> (self.pos % 8)) & 1;
            v |= (bit as u32) << i;
            self.pos += 1;
        }
        Some(v)
    }

    fn align_to_byte(&mut self) {
        self.pos = (self.pos + 7) & !7;
    }
}

/// 规范 Huffman 表的解码条目。
struct Huffman {
    entries: Vec<(u16, u8, u16)>, // (code, len, symbol)
    max_len: u8,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Option<Self> {
        let mut count = [0u32; 16];
        for &l in lengths {
            if l > 15 {
                return None;
            }
            count[l as usize] += 1;
        }
        let mut next_code = [0u32; 16];
        let mut code = 0u32;
        for bits in 1..16 {
            code = (code + count[bits - 1]) << 1;
            next_code[bits] = code;
        }
        let mut entries = Vec::new();
        let mut max_len = 0;
        for (sym, &len) in lengths.iter().enumerate() {
            if len == 0 {
                continue;
            }
            entries.push((next_code[len as usize] as u16, len, sym as u16));
            max_len = max_len.max(len);
        }
        Some(Self { entries, max_len })
    }

    fn decode(&self, bits: &mut Bits<'_>) -> Option<u16> {
        let mut code = 0u32;
        for len in 1..=self.max_len {
            code |= bits.read(1)? as u32;
            if let Some(&(_, _, sym)) = self
                .entries
                .iter()
                .find(|&&(c, l, _)| l == len && c as u32 == code)
            {
                return Some(sym);
            }
        }
        None
    }
}

const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115,
    131, 163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12,
    13, 13,
];

fn fixed_tables() -> Option<(Huffman, Huffman)> {
    let mut lit = vec![0u8; 288];
    for i in 0..144 {
        lit[i] = 8;
    }
    for i in 144..256 {
        lit[i] = 9;
    }
    for i in 256..280 {
        lit[i] = 7;
    }
    for i in 280..288 {
        lit[i] = 8;
    }
    Some((Huffman::new(&lit)?, Huffman::new(&[5u8; 32])?))
}

fn dynamic_tables(bits: &mut Bits<'_>) -> Option<(Huffman, Huffman)> {
    let hlit = bits.read(5)? as usize + 257;
    let hdist = bits.read(5)? as usize + 1;
    let hclen = bits.read(4)? as usize + 4;
    let order = [16usize, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
    let mut cl = [0u8; 19];
    for i in 0..hclen {
        cl[order[i]] = bits.read(3)? as u8;
    }
    let code_lengths = Huffman::new(&cl)?;
    let mut lengths = vec![0u8; hlit + hdist];
    let mut i = 0;
    while i < lengths.len() {
        let sym = code_lengths.decode(bits)? as usize;
        match sym {
            0..=15 => {
                lengths[i] = sym as u8;
                i += 1;
            }
            16 => {
                let prev = *lengths.get(i.checked_sub(1)?)?;
                let n = bits.read(2)? as usize + 3;
                for _ in 0..n {
                    *lengths.get_mut(i)? = prev;
                    i += 1;
                }
            }
            17 => {
                let n = bits.read(3)? as usize + 3;
                for _ in 0..n {
                    *lengths.get_mut(i)? = 0;
                    i += 1;
                }
            }
            18 => {
                let n = bits.read(7)? as usize + 11;
                for _ in 0..n {
                    *lengths.get_mut(i)? = 0;
                    i += 1;
                }
            }
            _ => return None,
        }
    }
    Some((
        Huffman::new(&lengths[..hlit])?,
        Huffman::new(&lengths[hlit..])?,
    ))
}

/// 解压 DEFLATE 流（zlib 头已剥离）。
fn inflate(data: &[u8]) -> Option<Vec<u8>> {
    let mut bits = Bits::new(data);
    let mut out = Vec::new();
    loop {
        let last = bits.read(1)? == 1;
        let kind = bits.read(2)?;
        match kind {
            0 => {
                bits.align_to_byte();
                let len = bits.read(16)? as usize;
                let nlen = bits.read(16)? as usize;
                if (len ^ 0xffff) != nlen {
                    return None;
                }
                for _ in 0..len {
                    out.push(bits.read(8)? as u8);
                }
            }
            1 | 2 => {
                let (lit, dist) = if kind == 1 {
                    fixed_tables()?
                } else {
                    dynamic_tables(&mut bits)?
                };
                loop {
                    let sym = lit.decode(&mut bits)? as usize;
                    if sym < 256 {
                        out.push(sym as u8);
                    } else if sym == 256 {
                        break;
                    } else {
                        let li = sym.checked_sub(257)?;
                        let length = *LEN_BASE.get(li)? as usize + bits.read(LEN_EXTRA[li] as usize)? as usize;
                        let ds = dist.decode(&mut bits)? as usize;
                        let distance = *DIST_BASE.get(ds)? as usize + bits.read(DIST_EXTRA[ds] as usize)? as usize;
                        if distance == 0 || distance > out.len() {
                            return None;
                        }
                        for _ in 0..length {
                            let b = out[out.len() - distance];
                            out.push(b);
                        }
                    }
                }
            }
            _ => return None,
        }
        if last {
            break;
        }
    }
    Some(out)
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (a, b, c) = (a as i32, b as i32, c as i32);
    let p = a + b - c;
    let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
    if pa <= pb && pa <= pc {
        a as u8
    } else if pb <= pc {
        b as u8
    } else {
        c as u8
    }
}

/// 解码一张 8-bit 非隔行 PNG，输出 RGBA8。不支持的格式返回 `None`。
pub fn decode_png(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    if data.len() < 8 || &data[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let mut pos = 8usize;
    let (mut width, mut height) = (0u32, 0u32);
    let (mut depth, mut color) = (0u8, 0u8);
    let mut interlace = 0u8;
    let mut idat = Vec::new();
    while pos + 8 <= data.len() {
        let len = u32::from_be_bytes(data[pos..pos + 4].try_into().ok()?) as usize;
        let tag = &data[pos + 4..pos + 8];
        let body = data.get(pos + 8..pos + 8 + len)?;
        match tag {
            b"IHDR" => {
                width = u32::from_be_bytes(body.get(0..4)?.try_into().ok()?);
                height = u32::from_be_bytes(body.get(4..8)?.try_into().ok()?);
                depth = *body.get(8)?;
                color = *body.get(9)?;
                interlace = *body.get(12)?;
            }
            b"IDAT" => idat.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
        pos += 12 + len;
    }
    if depth != 8 || interlace != 0 || width == 0 || height == 0 {
        return None;
    }
    let channels = match color {
        0 => 1usize,
        2 => 3,
        6 => 4,
        _ => return None,
    };
    if idat.len() < 2 {
        return None;
    }
    let raw = inflate(&idat[2..])?;
    let stride = width as usize * channels;
    if raw.len() < (stride + 1) * height as usize {
        return None;
    }
    let mut out = vec![0u8; (width * height * 4) as usize];
    let mut prev = vec![0u8; stride];
    for y in 0..height as usize {
        let filter = raw[y * (stride + 1)];
        let row = &raw[y * (stride + 1) + 1..(y + 1) * (stride + 1)];
        let mut cur = vec![0u8; stride];
        for x in 0..stride {
            let a = if x >= channels { cur[x - channels] } else { 0 };
            let b = prev[x];
            let c = if x >= channels { prev[x - channels] } else { 0 };
            let v = row[x];
            cur[x] = match filter {
                0 => v,
                1 => v.wrapping_add(a),
                2 => v.wrapping_add(b),
                3 => v.wrapping_add(((a as u16 + b as u16) / 2) as u8),
                4 => v.wrapping_add(paeth(a, b, c)),
                _ => return None,
            };
        }
        for x in 0..width as usize {
            let s = x * channels;
            let d = (y * width as usize + x) * 4;
            match channels {
                1 => {
                    out[d] = cur[s];
                    out[d + 1] = cur[s];
                    out[d + 2] = cur[s];
                    out[d + 3] = 255;
                }
                3 => {
                    out[d] = cur[s];
                    out[d + 1] = cur[s + 1];
                    out[d + 2] = cur[s + 2];
                    out[d + 3] = 255;
                }
                _ => out[d..d + 4].copy_from_slice(&cur[s..s + 4]),
            }
        }
        prev = cur;
    }
    Some((width, height, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_png() {
        assert!(decode_png(b"not a png").is_none());
    }
}
