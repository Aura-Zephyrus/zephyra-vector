//! C5：文字旋转与圆弧路径的回归测试。

use std::f32::consts::PI;

use crate::core::Point;
use crate::render::quad::kind;
use crate::text::stack::FixedWidth;
use crate::text::{TextEngine, TextPath, TextRender, TextStyle, TextStack};

fn glyphs(style: &TextStyle) -> Vec<crate::paint::GlyphQuad> {
    let mut stack = TextStack::new(FixedWidth { advance: 20.0 });
    let shaped = stack.shape("ABC", style, None);
    stack.glyph_quads(&shaped, Point::ZERO, style).unwrap().1
}

#[test]
fn rotation_reaches_quads_and_expanded_vertices() {
    let plain = glyphs(&TextStyle::default());
    let style = TextStyle {
        rotation: Some(PI / 4.0),
        ..Default::default()
    };
    let rotated = glyphs(&style);
    assert!(rotated.iter().all(|q| (q.rotation - PI / 4.0).abs() < 1e-5));

    // 展开成顶点：旋转后第一个顶点不应再位于未旋转矩形的左上角。
    let cmd = |qs: Vec<crate::paint::GlyphQuad>| crate::paint::Command::Text {
        atlas: crate::paint::TextureRef::new("a"),
        glyphs: qs.into(),
        offset: Point::ZERO,
        color: crate::core::Color::rgb(1.0, 1.0, 1.0),
        filter: crate::filter::FilterChain::none(),
        blend: crate::filter::BlendMode::default(),
    };
    let list_of = |qs| {
        let mut d = crate::paint::DisplayList::new();
        d.push(
            crate::paint::SortKey {
                layer: crate::paint::Layer::Content,
                z: 0,
                order: 0,
            },
            cmd(qs),
        );
        d
    };
    let a = crate::quad::expand(&list_of(plain), (100, 100), |_| false);
    let b = crate::quad::expand(&list_of(rotated), (100, 100), |_| false);
    assert_ne!(a.solid[0].position, b.solid[0].position, "旋转应改变顶点位置");
    let _ = kind::GLYPH;
}

#[test]
fn arc_path_rotates_each_glyph_differently() {
    let style = TextStyle {
        path: Some(TextPath::Arc { radius: 100.0 }),
        ..Default::default()
    };
    let qs = glyphs(&style);
    assert_eq!(qs.len(), 3);
    let rotations: Vec<f32> = qs.iter().map(|q| q.rotation).collect();
    assert!(rotations[0] > rotations[1] && rotations[1] > rotations[2], "圆弧上旋转角应递变: {rotations:?}");
    // 中间的字正对弧顶 ⇒ 旋转为 0，两侧一正一负。
    assert!(rotations[0] > 0.0 && rotations[2] < 0.0 && rotations[1].abs() < 1e-6);
}
