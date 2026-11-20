//! A2：OpenType feature 开关与 word-spacing 的回归测试。

use std::cell::RefCell;

use crate::core::Point;
use crate::text::stack::FixedWidth;
use crate::text::{AtlasRaster, GlyphSource, ShapedRun, TextEngine, TextStyle, TextStack};

/// 记录 `shape` 收到的样式，验证 features 真的透传到字形来源。
#[derive(Debug, Default)]
struct Spy {
    inner: FixedWidth,
    seen: RefCell<Vec<(u32, u32)>>,
}

impl GlyphSource for Spy {
    fn glyph_id(&mut self, ch: char) -> u32 {
        self.inner.glyph_id(ch)
    }
    fn advance(&mut self, id: u32, px: f32) -> f32 {
        self.inner.advance(id, px)
    }
    fn rasterize(&mut self, id: u32, px: u32) -> Option<AtlasRaster> {
        self.inner.rasterize(id, px)
    }
    fn font_id(&self) -> u32 {
        0
    }
    fn shape(&mut self, text: &str, style: &TextStyle) -> Vec<ShapedRun> {
        *self.seen.borrow_mut() = style.font_features.clone();
        self.inner.shape(text, style)
    }
}

fn tag(s: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*s)
}

#[test]
fn font_features_reach_glyph_source() {
    let tnum = tag(b"tnum");
    let mut stack = TextStack::new(Spy::default());
    let style = TextStyle {
        font_features: vec![(tnum, 1), (tag(b"liga"), 0)],
        ..Default::default()
    };
    let _ = stack.shape("123", &style, None);
    assert_eq!(
        stack.source().seen.borrow().as_slice(),
        &[(tnum, 1), (tag(b"liga"), 0)]
    );
}

#[test]
fn word_spacing_widens_only_whitespace_clusters() {
    let mut stack = TextStack::new(FixedWidth { advance: 10.0 });
    let base = stack.shape("A B", &TextStyle::default(), None).size.width;
    assert_eq!(base, 30.0);

    let style = TextStyle {
        word_spacing: 5.0,
        ..Default::default()
    };
    assert_eq!(stack.shape("A B", &style, None).size.width, 35.0);
    // 没有空格的文本不该被词间距影响。
    assert_eq!(stack.shape("AB", &style, None).size.width, 20.0);
}

#[test]
fn letter_spacing_is_counted_in_line_width() {
    let mut stack = TextStack::new(FixedWidth { advance: 10.0 });
    let style = TextStyle {
        letter_spacing: 2.0,
        ..Default::default()
    };
    // 3 个字素簇，每个追加 2px。
    assert_eq!(stack.shape("A B", &style, None).size.width, 36.0);
}

#[test]
fn real_font_accepts_custom_feature_without_panicking() {
    let bytes = match std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") {
        Ok(b) => b,
        Err(_) => return,
    };
    let src = match crate::text::AbGlyphSource::from_bytes(0, &bytes) {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut stack = TextStack::new(src);
    let style = TextStyle {
        font_features: vec![(tag(b"tnum"), 1), (tag(b"liga"), 0)],
        ..Default::default()
    };
    let shaped = stack.shape("0123 fi", &style, None);
    assert!(shaped.lines.iter().any(|l| !l.glyphs.is_empty()));
    let _ = Point::ZERO;
}
