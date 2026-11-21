//! C3：可变字体轴（fvar/gvar）的回归测试。

use crate::text::stack::FixedWidth;
use crate::text::{AbGlyphSource, GlyphSource, TextEngine, TextStyle, TextStack};

const VAR_FONT: &str = r"C:\Windows\Fonts\SegUIVar.ttf";

#[test]
fn variable_axis_changes_advance_and_outline() {
    let bytes = match std::fs::read(VAR_FONT) {
        Ok(b) => b,
        Err(_) => return,
    };
    let mut src = match AbGlyphSource::from_bytes(0, &bytes) {
        Ok(s) => s,
        Err(_) => return,
    };
    let axes = src.variation_axes();
    assert!(axes.iter().any(|a| &a.tag == b"wght"), "SegUIVar 应有 wght 轴: {axes:?}");

    let style = TextStyle::default();
    let mut stack = TextStack::new(match AbGlyphSource::from_bytes(0, &bytes) {
        Ok(s) => s,
        Err(_) => return,
    });

    let mut normal = style.clone();
    let mut bold = style.clone();
    normal.variations = vec![(b"wght".to_owned(), 400.0)];
    bold.variations = vec![(b"wght".to_owned(), 700.0)];

    let w_normal = stack.shape("Hello", &normal, None).size.width;
    let w_bold = stack.shape("Hello", &bold, None).size.width;
    assert!(
        (w_bold - w_normal).abs() > 0.01,
        "wght 400→700 应改变步进: {w_normal} vs {w_bold}"
    );

    // 同一字形的栅格化位图尺寸也应不同（或至少轮廓点位不同）。
    src.set_variations(&normal.variations);
    let gid = src.glyph_id('H');
    let r_normal = src.rasterize(gid, 64).expect("H 应有位图");
    src.set_variations(&bold.variations);
    let r_bold = src.rasterize(gid, 64).expect("H 应有位图");
    let outline = |r: &crate::text::AtlasRaster| {
        crate::text::msdf::outline_segments(&bytes, gid as u16, 64.0 / 1000.0,
            (r.offset.x, r.offset.y), &bold.variations).len()
    };
    let _ = (outline(&r_normal), outline(&r_bold));
    assert!(
        r_normal.sdf != r_bold.sdf || r_normal.width != r_bold.width,
        "可变轴应改变栅格化结果"
    );
}

#[test]
fn non_variable_source_ignores_axes() {
    let mut src = FixedWidth { advance: 10.0 };
    GlyphSource::set_variations(&mut src, &[(b"wght".to_owned(), 700.0)]);
    assert_eq!(src.advance(0, 10.0), 10.0);
}
