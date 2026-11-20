//! A5：max_lines + 省略号（SkParagraph 行为）的回归测试。

use crate::text::stack::FixedWidth;
use crate::text::{TextEngine, TextStyle, TextStack};

#[test]
fn max_lines_truncates_and_appends_ellipsis_within_width() {
    let mut stack = TextStack::new(FixedWidth { advance: 10.0 });
    let style = TextStyle {
        max_lines: Some(2),
        ..Default::default()
    };
    // 10 字符、每行 3 个 ⇒ 自然断成 4 行；截断到 2 行。
    let shaped = stack.shape("ABCDEFGHIJ", &style, Some(30.0));
    assert_eq!(shaped.lines.len(), 2);
    let last = shaped.lines.last().unwrap();
    assert!(last.is_last);
    // "DEF" + "…" 宽 40 超出 30 ⇒ 退掉 'F'，得到 "DE…" 宽 30。
    assert_eq!(last.glyphs.len(), 3);
    assert!((last.width - 30.0).abs() < 0.01, "{}", last.width);
    assert_eq!(last.glyphs.last().unwrap().glyph_id, '…' as u32);

    // 高度按截断后的行数算，而不是原始 4 行。
    let natural = stack.shape("ABCDEFGHIJ", &TextStyle::default(), Some(30.0));
    assert!(shaped.size.height < natural.size.height);
}

#[test]
fn custom_ellipsis_is_used() {
    let mut stack = TextStack::new(FixedWidth { advance: 10.0 });
    let style = TextStyle {
        max_lines: Some(1),
        ellipsis: Some("..".into()),
        ..Default::default()
    };
    let shaped = stack.shape("ABCD", &style, Some(30.0));
    let last = &shaped.lines[0];
    // "ABC"(30) + ".."(20) 超宽 ⇒ 退到 "A"(10) + ".."(20) = 30。
    assert_eq!(last.glyphs.len(), 3);
    assert!((last.width - 30.0).abs() < 0.01);
    assert_eq!(last.glyphs[1].glyph_id, '.' as u32);
}

#[test]
fn no_max_lines_leaves_text_untouched() {
    let mut stack = TextStack::new(FixedWidth { advance: 10.0 });
    let shaped = stack.shape("ABCDEFGHIJ", &TextStyle::default(), Some(30.0));
    assert_eq!(shaped.lines.len(), 4);
    assert!(!shaped.lines.iter().any(|l| l.glyphs.iter().any(|g| g.glyph_id == '…' as u32)));
}
