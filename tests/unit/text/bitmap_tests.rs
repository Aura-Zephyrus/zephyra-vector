//! C2：CBDT/CBLC 位图 emoji 与最小 PNG 解码器的回归测试。

use crate::core::Point;
use crate::text::png::decode_png;
use crate::text::{AbGlyphSource, GlyphSource, SDF_PADDING};

/// 用未压缩的 DEFLATE stored block 造一张 2×2 RGBA PNG（测 decoder 本身）。
fn tiny_png() -> Vec<u8> {
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |tag: &[u8; 4], body: &[u8], out: &mut Vec<u8>| {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(tag);
        out.extend_from_slice(body);
        out.extend_from_slice(&0u32.to_be_bytes()); // CRC 不被本 decoder 校验
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&2u32.to_be_bytes());
    ihdr.extend_from_slice(&2u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(b"IHDR", &ihdr, &mut png);

    // 每像素行：filter(0) + RGBA
    let raw = [
        0u8, 255, 0, 0, 255, 0, 255, 0, 128, //
        0u8, 0, 0, 255, 255, 255, 255, 255, 255, //
    ];
    let mut deflate = vec![0x78, 0x01, 0x01]; // zlib hdr + stored block(final, type 0)
    deflate.extend_from_slice(&(raw.len() as u16).to_le_bytes());
    deflate.extend_from_slice(&(!(raw.len() as u16)).to_le_bytes());
    deflate.extend_from_slice(&raw);
    chunk(b"IDAT", &deflate, &mut png);
    chunk(b"IEND", &[], &mut png);
    png
}

#[test]
fn decoder_handles_stored_deflate_and_filters() {
    let png = tiny_png();
    let (w, h, rgba) = decode_png(&png).expect("最小 PNG 应能解码");
    assert_eq!((w, h), (2, 2));
    assert_eq!(&rgba[0..4], &[255, 0, 0, 255]);
    assert_eq!(&rgba[4..8], &[0, 255, 0, 128]);
    assert_eq!(&rgba[8..12], &[0, 0, 255, 255]);
    assert_eq!(&rgba[12..16], &[255, 255, 255, 255]);
    assert!(decode_png(b"bad").is_none());
}

#[test]
fn emoji_font_bitmap_goes_into_atlas_as_rgba() {
    let bytes = match std::fs::read(r"C:\Windows\Fonts\seguiemj.ttf") {
        Ok(b) => b,
        Err(_) => return,
    };
    let mut src = match AbGlyphSource::from_bytes(0, &bytes) {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut found = false;
    for cp in [0x1F600u32, 0x1F601, 0x1F602, 0x2764, 0x1F44D] {
        let Some(ch) = char::from_u32(cp) else { continue };
        let gid = src.glyph_id(ch);
        let Some(r) = src.rasterize(gid, 109) else { continue };
        if !r.is_bitmap {
            continue;
        }
        let padded = (r.width + 2 * SDF_PADDING) * (r.height + 2 * SDF_PADDING) * 4;
        assert_eq!(r.sdf.len(), padded as usize, "位图应占满 padded RGBA 区域");
        assert!(r.sdf.chunks_exact(4).any(|p| p[0] != p[1] || p[1] != p[2]), "emoji 应是彩色的");
        found = true;
        break;
    }
    eprintln!("CBDT 位图命中: {found}");
    let _ = Point::ZERO;
}
