//! A3：Skia 风格装饰线样式（solid/double/dotted/dashed/wavy + gaps）的回归测试。

use crate::core::Point;
use crate::render::quad::kind;
use crate::text::stack::FixedWidth;
use crate::text::{
    TextDecoration, TextDecorationMode, TextDecorationStyle, TextEngine, TextRender, TextStyle,
    TextStack,
};

fn quads(style: &TextStyle) -> Vec<crate::paint::GlyphQuad> {
    let mut stack = TextStack::new(FixedWidth { advance: 10.0 });
    let shaped = stack.shape("ABC", style, None);
    stack.glyph_quads(&shaped, Point::ZERO, style).unwrap().1
}

fn counts(style: &TextStyle) -> (usize, usize) {
    let qs = quads(style);
    let glyphs = qs.iter().filter(|q| q.kind == kind::GLYPH).count();
    let rects = qs.iter().filter(|q| q.kind == kind::RECT).count();
    (glyphs, rects)
}

fn underlined(style: TextDecorationStyle) -> TextStyle {
    TextStyle {
        decoration: TextDecoration {
            underline: true,
            ..Default::default()
        },
        decoration_style: style,
        decoration_thickness: Some(2.0),
        ..Default::default()
    }
}

#[test]
fn decoration_styles_emit_expected_segment_counts() {
    assert_eq!(counts(&underlined(TextDecorationStyle::Solid)), (3, 1));
    assert_eq!(counts(&underlined(TextDecorationStyle::Double)), (3, 2));
    assert!(counts(&underlined(TextDecorationStyle::Dotted)).1 > 2);
    assert!(counts(&underlined(TextDecorationStyle::Dashed)).1 > 1);
    assert!(counts(&underlined(TextDecorationStyle::Wavy)).1 > 2);
}

#[test]
fn decoration_thickness_multiplier_scales_rect_height() {
    let mut style = underlined(TextDecorationStyle::Solid);
    style.decoration_thickness_multiplier = 2.0;
    let rect = quads(&style)
        .into_iter()
        .find(|q| q.kind == kind::RECT)
        .unwrap();
    assert!((rect.dst.height - 4.0).abs() < 0.01, "{}", rect.dst.height);
}

#[test]
fn gaps_mode_skips_ink_crossing_glyphs() {
    let solid = underlined(TextDecorationStyle::Solid);
    let mut gaps = solid.clone();
    gaps.decoration_mode = TextDecorationMode::Gaps;
    // 替身字形的墨迹从基线一直向下，必穿过下划线 ⇒ 整段被挖空。
    assert_eq!(counts(&solid).1, 1);
    assert_eq!(counts(&gaps).1, 0);
}
