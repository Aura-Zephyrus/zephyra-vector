//! C4：规则连字符（hyphenation）的回归测试。

use crate::text::stack::FixedWidth;
use crate::text::{hyphenation_opportunities, TextEngine, TextStyle, TextStack};

#[test]
fn syllable_breaks_are_found_in_latin_words() {
    let breaks = hyphenation_opportunities("wintertable");
    assert!(breaks.contains(&3), "win|tertable: {breaks:?}");
    assert!(breaks.contains(&6), "winter|table: {breaks:?}");
    // 短词不参与。
    assert!(hyphenation_opportunities("at").is_empty());
}

#[test]
fn hyphen_is_inserted_only_at_syllable_break() {
    let mut stack = TextStack::new(FixedWidth { advance: 10.0 });
    let with = TextStyle {
        hyphens: true,
        ..Default::default()
    };
    let without = TextStyle::default();

    let a = stack.shape("wintertable", &with, Some(40.0));
    let b = stack.shape("wintertable", &without, Some(40.0));

    let last_of = |s: &crate::text::ShapedText| s.lines[0].glyphs.last().unwrap().glyph_id;
    assert_eq!(last_of(&a), '-' as u32, "断词行尾应有连字符");
    assert_ne!(last_of(&b), '-' as u32, "未开启时不应有连字符");
    // "win-" 共 4 个字符 × 10px，正好 40。
    assert!((a.lines[0].width - 40.0).abs() < 0.01, "{}", a.lines[0].width);
}
