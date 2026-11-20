//! A7：COLR v0 / CPAL 彩色字形（emoji）的回归测试。

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
        eprintln!("该字体没有 COLR v0 分层（可能是 CBDT 版本），跳过");
        return;
    };
    assert_eq!(layers.len() >= 1, true, "{ch} 应有至少一层");
    let colorful = layers
        .iter()
        .any(|l| layers.iter().any(|o| o.color != l.color));
    assert!(colorful, "分层应至少有两种颜色: {layers:?}");
}

#[test]
fn color_glyph_expands_into_one_quad_per_layer() {
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
    let expected = src.glyph_id(ch);
    let expected = src.color_layers(expected).unwrap().len();

    let mut stack = TextStack::new(emoji_source().unwrap());
    let style = TextStyle {
        size: 32.0,
        ..Default::default()
    };
    let shaped = stack.shape(&ch.to_string(), &style, None);
    let (_, quads) = stack.glyph_quads(&shaped, Point::ZERO, &style).unwrap();
    let glyph_quads: Vec<_> = quads.iter().filter(|q| q.kind == kind::GLYPH).collect();
    assert_eq!(glyph_quads.len(), expected, "每层一个 quad");
    assert!(
        glyph_quads.iter().all(|q| q.color.is_some() && q.color_glyph),
        "彩色层应带独立颜色与 color_glyph 标记"
    );
}

#[test]
fn non_color_source_has_no_layers() {
    let src = FixedWidth { advance: 10.0 };
    assert!(GlyphSource::color_layers(&src, 'A' as u32).is_none());
}
