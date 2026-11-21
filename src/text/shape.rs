//! 整形结果的数据结构与与语言无关的辅助算法。
//!
//! GSUB/GPOS 由 `rustybuzz` 完成（在 `ab_glyph_source` 里），本模块负责：
//! BiDi 分级与视觉顺序、UAX #14 断行、字素簇边界。这样 `TextStack` 只消费
//! "视觉顺序的字形 run"，不再自己按 `char` 一个一个摆字。

use std::ops::Range;

use unicode_bidi::{ParagraphBidiInfo, LTR_LEVEL, RTL_LEVEL};
use unicode_linebreak::{linebreaks, BreakOpportunity};
use unicode_script::{Script as UniScript, UnicodeScript};

use crate::core::Point;

use super::{GlyphSource, TextDirection, TextStyle};

/// 一个整形后的字形。坐标单位已经换算到 DIP（显示像素），不再是字体单位。
#[derive(Debug, Clone, PartialEq)]
pub struct ShapedGlyph {
    /// 字形 id（与 [`crate::text::GlyphSource::rasterize`] 使用同一套 id）。
    pub glyph_id: u32,
    /// 产生该字形的字体 id（多字体回退时用于查图集）。
    pub font_id: u32,
    /// 对应原文里的字节区间（用于字素簇级的字距与断行）。
    pub cluster: Range<usize>,
    /// 水平步进（DIP）。
    pub advance: f32,
    /// 相对基线的偏移（DIP，y 向下）。
    pub offset: Point,
    /// 彩色字形（emoji）标记：`TextStack` 据此把 COLR/CBDT 字形展开成多层带色 quad。
    pub color_glyph: bool,
    /// 伪粗体阈值偏移（SDF 值域）。多字体回退在请求字重无匹配字体时用它合成，
    /// 0 表示不合成。
    pub embolden: f32,
}

/// 一个整形后的 run：同一方向、同一 script、视觉顺序连续。
#[derive(Debug, Clone, PartialEq)]
pub struct ShapedRun {
    pub glyphs: Vec<ShapedGlyph>,
    /// 该 run 的 BiDi 嵌入层级是否为 RTL。
    pub rtl: bool,
    /// ISO 15924 script 短名（如 `Latn` / `Arab` / `Deva`）。空串表示未知/通用。
    pub script: &'static str,
}

/// 把 Unicode script 映射到 rustybuzz 的 script tag。
///
/// `unicode_script` 的短名就是 ISO 15924 的 4 字符码，rustybuzz 直接接受它。
pub fn rustybuzz_script(script: UniScript) -> Option<rustybuzz::Script> {
    let name = script.short_name();
    if name.is_empty() || name.len() != 4 {
        return None;
    }
    let mut b = [0u8; 4];
    b.copy_from_slice(name.as_bytes());
    rustybuzz::Script::from_iso15924_tag(rustybuzz::ttf_parser::Tag::from_bytes(&b))
}

/// 选出一段文本里第一个"强"script（跳过 Common / Inherited / Unknown）。
pub fn dominant_script(text: &str) -> UniScript {
    let fallback = UniScript::Unknown;
    for ch in text.chars() {
        let s = ch.script();
        if s == UniScript::Common || s == UniScript::Inherited || s == UniScript::Unknown {
            continue;
        }
        return s;
    }
    fallback
}

/// BiDi 分级。返回 `(视觉顺序的 range, 是否 RTL)` 列表，range 是字节区间。
pub fn bidi_runs(text: &str, direction: TextDirection) -> Vec<(Range<usize>, bool)> {
    if text.is_empty() {
        return Vec::new();
    }
    let base = match direction {
        TextDirection::Auto => None,
        TextDirection::Ltr => Some(LTR_LEVEL),
        TextDirection::Rtl => Some(RTL_LEVEL),
    };
    let info = ParagraphBidiInfo::new(text, base);
    let line = 0..text.len();
    let (levels, runs) = info.visual_runs(line);
    runs.into_iter()
        .map(|r| {
            let rtl = levels.get(r.start).map(|l| l.is_rtl()).unwrap_or(false);
            (r, rtl)
        })
        .collect()
}

/// UAX #14 断行机会（字节偏移），已对齐到字素簇边界并补上末尾。
pub fn line_break_opportunities(text: &str) -> Vec<usize> {
    let boundaries: Vec<usize> =
        unicode_segmentation::UnicodeSegmentation::grapheme_indices(text, true)
            .map(|(i, _)| i)
            .chain(std::iter::once(text.len()))
            .collect();

    let mut out: Vec<usize> = linebreaks(text)
        .filter_map(|(offset, opportunity)| match opportunity {
            BreakOpportunity::Mandatory | BreakOpportunity::Allowed => Some(offset),
        })
        .map(|raw| {
            // 不能从字素簇中间断开：向前找到最近的簇边界。
            match boundaries.binary_search(&raw) {
                Ok(i) => boundaries[i],
                Err(i) => boundaries[i.saturating_sub(1)],
            }
        })
        .filter(|&p| p > 0 && p < text.len())
        .collect();
    out.push(text.len());
    out.sort_unstable();
    out.dedup();
    out
}

/// 拉丁文字的音节断点（字节偏移），用于 `TextStyle::hyphens`。
///
/// 不依赖词典文件（那会引入数据依赖）；用经典的 V|CV / VC|CV 启发式：
/// - 元音-辅音-元音：在辅音前断（"ta|ble"）；
/// - 辅音-辅音-元音：在第二个辅音前断（"win|ter"）。
/// 只在单词内部、且两侧至少留 2 个字符，避免产生 "a-" 之类碎片。
pub fn hyphenation_opportunities(text: &str) -> Vec<usize> {
    fn is_vowel(c: char) -> bool {
        matches!(c.to_ascii_lowercase(), 'a' | 'e' | 'i' | 'o' | 'u' | 'y')
    }
    let mut out = Vec::new();
    for word in text.split(|c: char| !c.is_ascii_alphabetic()) {
        let start = word.as_ptr() as usize - text.as_ptr() as usize;
        let chars: Vec<char> = word.chars().collect();
        if chars.len() < 4 {
            continue;
        }
        for i in 2..chars.len() - 1 {
            let (prev, cur, next) = (chars[i - 1], chars[i], chars[i + 1]);
            let breakable = (is_vowel(prev) && !is_vowel(cur) && is_vowel(next))
                || (!is_vowel(prev) && !is_vowel(cur) && is_vowel(next) && prev != cur);
            if breakable {
                out.push(start + i);
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// 默认整形：逐字符，不做 GSUB/GPOS。给没有字体引擎的替身与降级路径使用。
pub fn naive_shape<S: GlyphSource + ?Sized>(
    source: &mut S,
    text: &str,
    style: &TextStyle,
) -> Vec<ShapedRun> {
    let px = style.effective_size().max(1.0);
    let mut glyphs = Vec::new();
    for (i, ch) in text.char_indices() {
        let next = text[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        let id = source.glyph_id(ch);
        glyphs.push(ShapedGlyph {
            glyph_id: id,
            font_id: source.font_id(),
            cluster: i..i + next,
            advance: source.advance(id, px),
            offset: Point::ZERO,
            color_glyph: false,
            embolden: 0.0,
        });
    }
    if glyphs.is_empty() {
        Vec::new()
    } else {
        vec![ShapedRun {
            glyphs,
            rtl: false,
            script: "Zyyy",
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bidi_reorders_rtl_paragraph() {
        let runs = bidi_runs("שלום", TextDirection::Auto);
        assert_eq!(runs.len(), 1);
        assert!(runs[0].1, "希伯来语段落应为 RTL");
        assert_eq!(&"שלום"[runs[0].0.clone()], "שלום");
    }

    #[test]
    fn bidi_mixed_direction_produces_multiple_runs() {
        let runs = bidi_runs("abc שלום xyz", TextDirection::Auto);
        assert!(runs.len() >= 3, "混合方向应分成多 run，实得 {}", runs.len());
        assert!(runs.iter().any(|(_, rtl)| *rtl), "应存在 RTL run");
    }

    #[test]
    fn explicit_direction_overrides_auto() {
        // 强方向字符（拉丁/希伯来）会覆盖段落基准，这符合 UBA；
        // 显式方向决定的是"没有强方向字符"时的段落方向。
        assert!(!bidi_runs("abc", TextDirection::Auto)[0].1);
        // 纯中性字符（标点）完全服从段落基准方向。
        assert!(bidi_runs("-+-", TextDirection::Rtl).iter().all(|(_, r)| *r));
        assert!(bidi_runs("שלום", TextDirection::Auto)[0].1);
    }

    #[test]
    fn line_breaks_never_split_a_grapheme() {
        let text = "ab cd\u{0301} ef";
        let breaks = line_break_opportunities(text);
        assert_eq!(*breaks.last().unwrap(), text.len());
        for &p in &breaks {
            assert!(text.is_char_boundary(p), "断点 {p} 不是字节边界");
        }
    }

    #[test]
    fn dominant_script_skips_common() {
        use unicode_script::UnicodeScript;
        assert_eq!('A'.script(), UniScript::Latin);
        assert_eq!(dominant_script("123漢字"), UniScript::Han);
    }

    #[test]
    fn rustybuzz_script_maps_latin_and_arabic() {
        assert!(rustybuzz_script(UniScript::Latin).is_some());
        assert!(rustybuzz_script(UniScript::Arabic).is_some());
    }
}
