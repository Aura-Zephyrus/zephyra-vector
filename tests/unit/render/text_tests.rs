// 本文件由 text.rs 的 #[cfg(test)] mod tests 抽出 —— 测试代码与生产代码分离。
// 模块路径不变（src 里的声明用 #[path] 指过来），所以 use super::* 与私有可见性都照旧。

use super::*;
use crate::text::stack::FixedWidth;
use crate::text::{GlyphSource, TextDirection, TextEngine, TextRender, TextStyle};

/// 按路径加载字体并直接拿到来源（不给文本栈），供整形级断言使用。
fn source_at(path: &str) -> Option<AbGlyphSource> {
    AbGlyphSource::from_bytes(0, &std::fs::read(path).ok()?).ok()
}

/// GSUB/GPOS 断言需要"确实带这些表"的字体。本机 Windows 字体满足，否则跳过。
fn shaping_source() -> Option<AbGlyphSource> {
    source_at(r"C:\Windows\Fonts\segoeui.ttf")
}

/// 系统字体可用时才跑这些断言 —— CI 上不该因为"没装某字体"而失败。
fn system_stack() -> Option<TextStack<AbGlyphSource>> {
    match AbGlyphSource::from_bytes(0, std::fs::read(FONT_CANDIDATES[0]).ok()?.as_slice()) {
        Ok(s) => Some(TextStack::new(s)),
        Err(_) => None,
    }
}

/// `px_scale` 是把 em 翻成 ab_glyph 行高的唯一入口。契约：
/// 换算后的 scale 装回字体，`.height()`（行高像素）必须等于
/// `em × hhea行高 / upem`。
///
/// 这条钉的是本次实测的 bug：ab_glyph 的 `PxScale::from(x)` 是**行高**语义，
/// 直接拿 em 传进去（`.height() == em`），Segoe UI（hhea = 1.33 em）的全部
/// 文字一律渲成 0.75 倍。此处对 Segoe 期望 15.96 而实测 12 —— 两种写法
/// 只差这一个比例，症状是整库文字变小且行距松弛。
#[test]
fn px_scale_maps_em_to_font_line_height() {
    use ab_glyph::{Font, ScaleFont};
    let Ok(bytes) = std::fs::read(FONT_CANDIDATES[0]) else {
        eprintln!("跳过：找不到系统字体");
        return;
    };
    let src = match AbGlyphSource::from_bytes(0, &bytes) {
        Ok(s) => s,
        Err(_) => return,
    };
    let upem = src.font.units_per_em().unwrap_or(1000.0).max(1.0);
    let ratio = src.font.height_unscaled() / upem;
    for em in [12.0f32, 14.0, 16.0, 34.0, 48.0] {
        let scaled = src.font.as_scaled(src.px_scale(em));
        let expect = em * ratio;
        assert!(
            (scaled.height() - expect).abs() < 0.01,
            "px_scale({em}) 的行高应为 {expect:.2}（= em × {ratio:.3}），实测 {}。\
                 em 与行高混用时这里恰好等于 em —— 那是 0.75 倍文字的成因",
            scaled.height()
        );
    }
}

/// 大写字母的位图必须是**cap 高度**（≈0.7em），不是 0.75 倍缩放后的 0.53em。
///
/// 实测过：34px 的 'Z' 位图曾只有 15×18（GDI 基准 19×24）。0.75 这个比例
/// 对所有拉丁字体都成立，所以阈值取 0.55em / 0.9em 两侧夹。
#[test]
fn capital_ink_is_cap_height_not_three_quarters() {
    use crate::text::GlyphSource as _;
    let Ok(bytes) = std::fs::read(FONT_CANDIDATES[0]) else {
        eprintln!("跳过：找不到系统字体");
        return;
    };
    let mut src = match AbGlyphSource::from_bytes(0, &bytes) {
        Ok(s) => s,
        Err(_) => return,
    };
    let id = src.glyph_id('H');
    // 传入的是排版端选好的 SDF 基准档位，这里用默认档 64。
    let base = crate::text::SDF_BASE_PX;
    let r = src.rasterize(id, base).expect("'H' 必须栅格化出位图");
    assert_eq!(r.base_px, base as f32);
    let ratio = r.height as f32 / crate::text::SDF_BASE_PX as f32;
    assert!(
        (0.55..0.90).contains(&ratio),
        "'H' 基准位图高 {} = {ratio:.2}em；cap 高度应约 0.7em。\
             实测 0.5 出头说明字被按 upem/hhea 比例缩小了（em 与行高混用）",
        r.height
    );
}

/// offset 必须是**屏幕方向**（y 向下，基线之上为负）。
///
/// 这条钉的是第二个 bug：offset 的约定说反后，core 的 `dst.y` 公式也跟着反，
/// 每个字形的**顶部贴到基线**上 —— 同一行里 'h' 与 'e' 顶相同、底差一个字高，
/// descender 整个不见。实测 34px 行 'e' 顶 71 / 'h' 顶 72（应相差约 7）。
#[test]
fn raster_offset_is_screen_space_y_down() {
    use crate::text::GlyphSource as _;
    let Ok(bytes) = std::fs::read(FONT_CANDIDATES[0]) else {
        eprintln!("跳过：找不到系统字体");
        return;
    };
    let mut src = match AbGlyphSource::from_bytes(0, &bytes) {
        Ok(s) => s,
        Err(_) => return,
    };
    // 'H'：无 descender，整个位图在基线**之上** → offset.y 负、bottom 恰在 0 附近。
    let h_id = src.glyph_id('H');
    let h = src.rasterize(h_id, 34).expect("'H'");
    assert!(
        h.offset.y < 0.0,
        "'H' 的 offset.y 应为负（位图在基线之上），实测 {}",
        h.offset.y
    );
    assert!(
        (h.offset.y + h.height as f32).abs() <= 1.5,
        "'H' 无 descender，位图底应落在基线上，实测 {}",
        h.offset.y + h.height as f32
    );
    // 'p'：有 descender → 位图底伸到基线**下面**（offset.y + height > 0）。
    let p_id = src.glyph_id('p');
    let p = src.rasterize(p_id, 34).expect("'p'");
    assert!(
        p.offset.y < 0.0,
        "'p' 的 offset.y 应为负（位图主体在基线之上），实测 {}",
        p.offset.y
    );
    assert!(
        p.offset.y + p.height as f32 > 1.0,
        "'p' 有 descender，位图底应在基线之下，实测 {}",
        p.offset.y + p.height as f32
    );
}

#[test]
fn a_real_font_measures_and_renders() {
    let Some(mut s) = system_stack() else {
        eprintln!("跳过：找不到系统字体");
        return;
    };
    let style = TextStyle {
        size: 16.0,
        ..Default::default()
    };
    let shaped = s.shape("Hello", &style, None);
    assert!(shaped.size.width > 0.0, "真实字体必须测出非零宽度");
    assert_eq!(shaped.lines.len(), 1);

    let (_atlas, quads) = s
        .glyph_quads(&shaped, Point::new(0.0, 0.0), &style)
        .unwrap();
    assert!(!quads.is_empty(), "应产出字形四边形");
    for q in &quads {
        assert!(q.uv.x >= 0.0 && q.uv.y >= 0.0 && q.uv.x + q.uv.width <= 1.0);
        assert!(q.dst.width > 0.0 && q.dst.height > 0.0);
    }
}

#[test]
fn space_advances_but_renders_nothing() {
    let Some(mut s) = system_stack() else {
        eprintln!("跳过：找不到系统字体");
        return;
    };
    let style = TextStyle::default();
    // 空格的步进应大于 0（它确实占空间）。
    assert!(
        s.advance(' ', &style) > 0.0,
        "空格必须推进光标，否则单词会粘在一起"
    );
    let shaped = s.shape("a a", &style, None);
    let (_atlas, quads) = s
        .glyph_quads(&shaped, Point::new(0.0, 0.0), &style)
        .unwrap();
    // 3 个字符，但空格没有位图 → 只有 2 个可绘制四边形。
    assert_eq!(quads.len(), 2, "空格有步进无位图，不应产出四边形");
}

#[test]
fn a_missing_glyph_becomes_the_replacement_character() {
    let Some(mut s) = system_stack() else {
        eprintln!("跳过：找不到系统字体");
        return;
    };
    use crate::text::GlyphSource as _;
    let style = TextStyle::default();
    // 私用区字符几乎肯定不在任何系统字体里。
    let missing = '\u{E000}';
    let id = s.source_mut().glyph_id(missing);
    let normal = s.source_mut().glyph_id('A');
    assert_ne!(id, normal, "缺失字符应映射到替换字符而不是复用正常字形");
    // 关键是它**有宽度**（会被画成豆腐框），而不是静默消失。
    assert!(
        s.advance(missing, &style) > 0.0,
        "缺失字符必须有步进，否则布局会把它当不存在"
    );
}

#[test]
fn the_same_text_packs_each_glyph_once() {
    let Some(mut s) = system_stack() else {
        eprintln!("跳过：找不到系统字体");
        return;
    };
    let style = TextStyle::default();
    let shaped = s.shape("repeat repeat", &style, None);
    let _ = s.glyph_quads(&shaped, Point::new(0.0, 0.0), &style);
    let first = s.atlas_stats().glyphs;
    let _ = s.glyph_quads(&shaped, Point::new(0.0, 0.0), &style);
    assert_eq!(s.atlas_stats().glyphs, first, "同一字形不得重复打包");
}

#[test]
fn one_sdf_bitmap_serves_every_display_size() {
    let Some(mut s) = system_stack() else {
        eprintln!("跳过：找不到系统字体");
        return;
    };
    // 12 / 36 同属 64 档：共用一份位图，四边形按字号缩放。
    let small = TextStyle {
        size: 12.0,
        ..Default::default()
    };
    let large = TextStyle {
        size: 36.0,
        ..Default::default()
    };
    let before = s.atlas_stats().glyphs;
    let a = s.shape("H", &small, None);
    let (_, qa) = s.glyph_quads(&a, Point::new(0.0, 0.0), &small).unwrap();
    let b = s.shape("H", &large, None);
    let (_, qb) = s.glyph_quads(&b, Point::new(0.0, 0.0), &large).unwrap();
    assert_eq!(
        s.atlas_stats().glyphs,
        before + 1,
        "SDF 与字号无关：同一字形只该打包一份位图"
    );
    assert!(
        (qb[0].dst.width / qa[0].dst.width - 3.0).abs() < 0.01,
        "四边形应随显示字号线性放大 3 倍"
    );
    // 字号越大，smoothstep 在 SDF 值域上的半宽越窄 —— 边缘因此保持锐利。
    assert!(qb[0].sdf.unwrap() < qa[0].sdf.unwrap());
}

/// 大字号自动切到高档位：图集里多一份位图，但四边形尺寸仍随字号线性变化。
/// 这就是"超大字号不糊"的兜底 —— 不再把 64px 的距离场放大数倍使用。
#[test]
fn large_text_uses_a_higher_sdf_tier() {
    let Some(mut s) = system_stack() else {
        eprintln!("跳过：找不到系统字体");
        return;
    };
    let small = TextStyle {
        size: 16.0,
        ..Default::default()
    };
    let huge = TextStyle {
        size: 200.0,
        ..Default::default()
    };
    let a = s.shape("H", &small, None);
    let (_, qa) = s.glyph_quads(&a, Point::new(0.0, 0.0), &small).unwrap();
    let before = s.atlas_stats().glyphs;

    let b = s.shape("H", &huge, None);
    let (_, qb) = s.glyph_quads(&b, Point::new(0.0, 0.0), &huge).unwrap();
    assert_eq!(
        s.atlas_stats().glyphs,
        before + 1,
        "高档位应单独缓存一份 SDF"
    );
    assert!(
        qb[0].dst.width > qa[0].dst.width * 5.0,
        "200px 文本应明显大于 16px"
    );
    // 200px 用 256 档（只缩小不放大），16px 用 64 档。
    assert!(
        qb[0].sdf.unwrap() < qa[0].sdf.unwrap(),
        "高档位在 SDF 值域的过渡更窄"
    );
}

#[test]
fn a_broken_font_is_rejected_rather_than_silently_ignored() {
    let r = AbGlyphSource::from_bytes(0, b"this is not a font");
    assert!(r.is_err(), "损坏的字体必须报错而不是产出空栈");
}

// ── 文本整形（GSUB / GPOS / BiDi / 字素簇） ──────────────────────────────

/// rustybuzz 与 ab_glyph 必须给出同一套 glyph id，否则整形结果拿去栅格化会画错字。
/// 两者共用 ttf-parser 0.25，这条测试钉住这个前提。
#[test]
fn shaper_and_rasterizer_agree_on_glyph_ids() {
    let Some(mut src) = shaping_source() else {
        return;
    };
    let style = TextStyle {
        size: 64.0,
        ..Default::default()
    };
    // 只用字体确定覆盖的拉丁字符：缺字会走替换字形，步进比较没有意义。
    for ch in ['A', 'H', 'g'] {
        let shaped = src.shape(&ch.to_string(), &style);
        let id = shaped
            .first()
            .and_then(|r| r.glyphs.first())
            .map(|g| g.glyph_id);
        assert_eq!(
            id,
            Some(src.glyph_id(ch)),
            "字符 {ch:?} 的整形/栅格化 glyph id 不一致"
        );
        // 步进也必须一致：整形用字体单位 × px/upem，ab_glyph 用同一个比例。
        let shaped_advance: f32 = shaped
            .iter()
            .flat_map(|r| &r.glyphs)
            .map(|g| g.advance)
            .sum();
        assert!(
            (shaped_advance - src.advance(id.unwrap(), style.effective_size())).abs() < 0.01,
            "字符 {ch:?} 的整形步进 {} 与 ab_glyph 步进不一致",
            shaped_advance
        );
    }
}

/// GSUB 连字：`fi` 在 Calibri 里合成为**一个**字形，cluster 覆盖两个字符。
#[test]
fn gsub_ligature_merges_glyphs_and_keeps_cluster_range() {
    let Some(mut src) = source_at(r"C:\Windows\Fonts\calibri.ttf") else {
        return;
    };
    let style = TextStyle {
        size: 64.0,
        ..Default::default()
    };
    let runs = src.shape("fi", &style);
    let glyphs: Vec<_> = runs.into_iter().flat_map(|r| r.glyphs).collect();
    assert_eq!(
        glyphs.len(),
        1,
        "fi 应合成一个连字字形，实得 {} 个",
        glyphs.len()
    );
    assert_eq!(glyphs[0].cluster, 0..2, "连字的 cluster 必须覆盖 f 和 i");
    assert!(glyphs[0].advance > 0.0);
}

/// GPOS 字距对：Segoe UI 的 "AT" 步进应小于 A 与 T 单独步进之和。
#[test]
fn gpos_kerning_changes_advance() {
    let Some(mut src) = shaping_source() else {
        return;
    };
    let style = TextStyle {
        size: 64.0,
        ..Default::default()
    };
    let advance_of = |s: &str, src: &mut AbGlyphSource| -> f32 {
        src.shape(s, &style)
            .into_iter()
            .flat_map(|r| r.glyphs)
            .map(|g| g.advance)
            .sum()
    };
    let pair = advance_of("AT", &mut src);
    let separate = advance_of("A", &mut src) + advance_of("T", &mut src);
    assert!(
        pair < separate - 1.0,
        "AT 应因 kerning 变窄：pair={pair} separate={separate}"
    );
}

/// GSUB 复杂脚本：阿拉伯语 "سلام" 的首字用起笔形、末字用收笔形，不同于孤立字形。
#[test]
fn arabic_uses_contextual_forms() {
    let Some(mut src) = shaping_source() else {
        return;
    };
    let style = TextStyle {
        size: 64.0,
        direction: TextDirection::Auto,
        ..Default::default()
    };
    let word = src.shape("سلام", &style);
    assert_eq!(word.len(), 1);
    assert!(word[0].rtl, "阿拉伯语段落应为 RTL");
    assert_eq!(word[0].script, "Arab");

    let first = word[0].glyphs.first().unwrap().glyph_id;
    let last = word[0].glyphs.last().unwrap().glyph_id;
    let isolated_seen = src.shape("س", &style)[0].glyphs[0].glyph_id;
    let isolated_alef = src.shape("ا", &style)[0].glyphs[0].glyph_id;
    assert_ne!(first, isolated_seen, "词首 س 应使用起笔形");
    assert_ne!(last, isolated_alef, "词尾 ا 应使用收笔形");
    // RTL run 的 cluster 从右往左递减。
    let clusters: Vec<usize> = word[0].glyphs.iter().map(|g| g.cluster.start).collect();
    assert!(
        clusters.windows(2).all(|w| w[0] > w[1]),
        "RTL cluster 应递减：{clusters:?}"
    );
}

/// BiDi 混合段落：`TextStack` 产出的字形 x 位置按视觉顺序递增（左→右）。
#[test]
fn bidi_reaches_placed_glyphs() {
    let Some(mut stack) = system_stack() else {
        return;
    };
    let style = TextStyle {
        size: 32.0,
        ..Default::default()
    };
    let shaped = stack.shape("aسلام", &style, None);
    let glyphs = &shaped.lines[0].glyphs;
    assert_eq!(glyphs.len(), 5);
    for pair in glyphs.windows(2) {
        assert!(
            pair[0].offset.x < pair[1].offset.x,
            "视觉顺序的 x 必须递增：{:?}",
            glyphs.iter().map(|g| g.offset.x).collect::<Vec<_>>()
        );
    }
}

/// 字素簇感知断行：带组合音标的字符不得被拆到两行。
#[test]
fn line_break_never_splits_grapheme_in_stack() {
    let mut s = TextStack::new(FixedWidth { advance: 10.0 });
    let style = TextStyle {
        size: 10.0,
        ..Default::default()
    };
    let text = "ab cd\u{0301} ef gh";
    let shaped = s.shape(text, &style, Some(28.0));
    assert!(shaped.lines.len() > 1, "限宽应产生多行");
    for line in &shaped.lines {
        if let Some(g) = line.glyphs.first() {
            let id = g.glyph_id;
            assert_ne!(id, 0x0301, "行首不能是孤立的组合音标");
        }
    }
}

/// Emoji ZWJ 序列在整形层合并为同一个 cluster；彩色光栅化尚未支持，
/// 所以这里只验整形，不断言位图。
#[test]
fn emoji_zwj_sequence_is_one_cluster() {
    let Some(mut src) = shaping_source() else {
        return;
    };
    let style = TextStyle {
        size: 64.0,
        ..Default::default()
    };
    let runs = src.shape("👩‍💻", &style);
    let glyphs: Vec<_> = runs.into_iter().flat_map(|r| r.glyphs).collect();
    assert!(!glyphs.is_empty());
    assert!(
        glyphs.iter().all(|g| g.cluster.start == 0),
        "ZWJ 序列应合并为同一个 cluster：{:?}",
        glyphs.iter().map(|g| g.cluster.clone()).collect::<Vec<_>>()
    );
    assert!(glyphs.iter().all(|g| !g.color_glyph));
}
