//! A7：COLR v0 / v1 彩色字形（emoji）的回归测试。

use crate::core::Point;
use crate::render::quad::kind;
use crate::text::stack::FixedWidth;
use crate::text::{
    AbGlyphSource, ColorLayer, GlyphSource, TextEngine, TextRender, TextStyle, TextStack,
};

const EMOJI_FONT: &str = r"C:\Windows\Fonts\seguiemj.ttf";

fn emoji_source() -> Option<AbGlyphSource> {
    AbGlyphSource::from_bytes(0, &std::fs::read(EMOJI_FONT).ok()?).ok()
}

#[test]
fn colr_layers_are_parsed_from_real_emoji_font() {
    let Some(mut src) = emoji_source() else {
        eprintln!("Segoe UI Emoji 不可用，跳过");
        return;
    };
    // 在常见 emoji 里找一个确实带 COLR 分层的。
    let mut found: Option<(char, Vec<ColorLayer>)> = None;
    for cp in [0x1F600u32, 0x1F601, 0x1F602, 0x1F603, 0x2764, 0x1F44D, 0x1F600 + 100] {
        let Some(ch) = char::from_u32(cp) else { continue };
        let gid = src.glyph_id(ch);
        if let Some(layers) = src.color_layers(gid) {
            found = Some((ch, layers.to_vec()));
            break;
        }
    }
    let Some((ch, layers)) = found else {
        panic!("Segoe UI Emoji 应有 COLR v1 分层，却一个都没解析到");
    };
    assert!(!layers.is_empty(), "{ch} 应有至少一层");
    assert!(
        layers.iter().any(|l| l.gradient.is_some()),
        "Segoe UI Emoji 的 paint graph 应含真实渐变，而不是被压成纯色"
    );
    let colorful = layers
        .iter()
        .any(|l| layers.iter().any(|o| o.color != l.color));
    assert!(colorful, "分层应至少有两种颜色: {layers:?}");
}

#[test]
fn color_glyph_is_one_exact_rgba_quad() {
    let Some(mut src) = emoji_source() else {
        return;
    };
    let mut colored = None;
    for cp in [0x1F600u32, 0x1F601, 0x1F602, 0x2764, 0x1F44D] {
        let Some(ch) = char::from_u32(cp) else { continue };
        let gid = src.glyph_id(ch);
        if src.color_layers(gid).is_some() {
            colored = Some(ch);
            break;
        }
    }
    let Some(ch) = colored else { return };

    // paint graph 已精确合成进一张 RGBA 位图：一个 base 字形 = 一个 quad。
    let mut stack = TextStack::new(emoji_source().unwrap());
    let style = TextStyle {
        size: 32.0,
        ..Default::default()
    };
    let shaped = stack.shape(&ch.to_string(), &style, None);
    let (_, quads) = stack.glyph_quads(&shaped, Point::ZERO, &style).unwrap();
    let glyph_quads: Vec<_> = quads.iter().filter(|q| q.kind == kind::GLYPH).collect();
    assert_eq!(glyph_quads.len(), 1, "整张 paint graph 应合成一个 quad");
    assert_eq!(glyph_quads[0].mask, crate::paint::GlyphMask::ColorRgba);
    assert!(glyph_quads[0].color_glyph);
}

#[test]
fn non_color_source_has_no_layers() {
    let src = FixedWidth { advance: 10.0 };
    assert!(GlyphSource::color_layers(&src, 'A' as u32).is_none());
}
