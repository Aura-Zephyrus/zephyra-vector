//! A4：文字阴影（Skia TextShadow）的回归测试。

use crate::core::Color;
use crate::core::Point;
use crate::paint::GlyphMask;
use crate::render::quad::kind;
use crate::text::stack::FixedWidth;
use crate::text::{TextEngine, TextRender, TextStyle, TextShadow, TextStack};

fn quads(style: &TextStyle) -> Vec<crate::paint::GlyphQuad> {
    let mut stack = TextStack::new(FixedWidth { advance: 10.0 });
    let shaped = stack.shape("ABC", style, None);
    stack.glyph_quads(&shaped, Point::ZERO, style).unwrap().1
}

#[test]
fn shadow_duplicates_glyphs_with_offset_and_own_color() {
    let base = quads(&TextStyle::default());
    assert_eq!(base.len(), 3);

    let style = TextStyle {
        shadow: Some(TextShadow {
            color: Color::rgba(1.0, 0.0, 0.0, 0.5),
            offset: Point::new(2.0, 3.0),
            blur_sigma: 0.0,
        }),
        ..Default::default()
    };
    let q = quads(&style);
    assert_eq!(q.len(), 6);
    // 阴影在前（先画），正文在后。
    for (shadow, main) in q[..3].iter().zip(&q[3..]) {
        assert_eq!(shadow.kind, kind::GLYPH);
        assert_eq!(shadow.color, Some(Color::rgba(1.0, 0.0, 0.0, 0.5)));
        assert_eq!(main.color, None);
        assert!((shadow.dst.x - main.dst.x - 2.0).abs() < 0.01);
        assert!((shadow.dst.y - main.dst.y - 3.0).abs() < 0.01);
    }
}

#[test]
fn blur_widens_smoothstep_band() {
    let style = TextStyle {
        shadow: Some(TextShadow {
            blur_sigma: 2.0,
            ..Default::default()
        }),
        ..Default::default()
    };
    let q = quads(&style);
    let GlyphMask::Msdf { w: main_w, .. } = q[3].mask else {
        panic!("正文应为 MSDF");
    };
    let GlyphMask::Msdf { w: shadow_w, .. } = q[0].mask else {
        panic!("阴影应为 MSDF");
    };
    assert!(shadow_w > main_w, "阴影的过渡带应更宽: {shadow_w} vs {main_w}");
    assert!(shadow_w - main_w <= 0.25, "模糊量需被夹住");
}
