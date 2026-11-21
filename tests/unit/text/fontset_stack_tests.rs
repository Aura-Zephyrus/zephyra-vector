//! C1：多字体文本栈构造的回归测试。

use crate::text::{from_system_font_set, GlyphSource, TextStyle};

#[test]
fn system_font_stack_has_fallback_coverage() {
    let (mut stack, info) = from_system_font_set();
    eprintln!("{info}");
    assert!(stack.source().len() >= 1, "至少应加载一份字体");
    // 中文应落到有 CJK 的那份，emoji 应落到 emoji 那份（若系统有 emoji 字体）。
    let style = TextStyle::default();
    let (fid_cjk, g_cjk) = stack.source_mut().glyph_in('中', &style);
    assert_ne!(g_cjk, 0, "中字应有字形");
    let _ = fid_cjk;
}
