//! 字体回退链：按覆盖范围与 `TextStyle` 的 family/weight 在多份字体间选字。
//!
//! # 为什么它实现 `GlyphSource` 而不是改动 `TextStack`
//!
//! `TextStack` 持有的那个 `GlyphSource` 就是"字体从哪来"的唯一抽象点。回退链本质上
//! 也是"字体从哪来"—— 它只是从"一份字体"变成"一组带优先级的字体"。让它作为一个
//! `GlyphSource` 实现，排版层的断行、基线、图集、uv 一行都不用改。
//!
//! 图集键 `(font_id, glyph_id, px)` 本来就带 font_id，于是两篇不同语言的混排文本
//! 自然会占用两串图集条目，无需额外记账。
//!
//! # 覆盖范围的查询方式
//!
//! 用 `rustybuzz::ttf_parser` 的 cmap（`Face::glyph_index`）而不是 `glyph_id != 0`：
//! 后者是 `ab_glyph` 的便利封装，语义相同但拿不到 face 的 script/feature 信息。
//! 这里只做覆盖查询，真正的 GSUB/GPOS 仍复用 `AbGlyphSource::shape_run`。
//!
//! # 不做的事
//!
//! - **不枚举系统字体**：那是宿主（`fontdb`）的职责；本模块只接收已读好的字节；
//! - **不跨字体整形**：HarfBuzz 无法在两份字体之间做连字/字距。回退边界上的连字
//!   断开是正确行为，强行拼接只会得到错误的字形；
//! - **不解析可变字体轴**：本轮只在离散字体之间选，字重无匹配时用伪粗体合成。

use unicode_segmentation::UnicodeSegmentation;

use super::ab_glyph_source::AbGlyphSource;
use super::shape::{bidi_runs, dominant_script, ShapedGlyph, ShapedRun};
use super::{AtlasRaster, FontMetrics, GlyphSource, TextStack, TextStyle};

/// 一份字体的选择元数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontMeta {
    /// 字体族名（如 `"Segoe UI"` / `"Microsoft YaHei"`）。
    pub family: String,
    /// 字重（400 = Regular，700 = Bold）。
    pub weight: u16,
    /// 是否斜体。
    pub italic: bool,
}

impl FontMeta {
    pub fn new(family: impl Into<String>, weight: u16) -> Self {
        Self {
            family: family.into(),
            weight,
            italic: false,
        }
    }

    pub fn italic(mut self) -> Self {
        self.italic = true;
        self
    }
}

#[derive(Debug)]
struct FontSlot {
    meta: FontMeta,
    source: AbGlyphSource,
}

/// 一组按加入顺序排列的字体（第一份是默认）。
#[derive(Debug)]
pub struct FontSet {
    fonts: Vec<FontSlot>,
}

impl Default for FontSet {
    fn default() -> Self {
        Self::new()
    }
}

impl FontSet {
    pub fn new() -> Self {
        Self { fonts: Vec::new() }
    }

    /// 加入一份字体，族名/字重从字体表自动读取。
    pub fn add_with_family(
        &mut self,
        font_id: u32,
        bytes: &[u8],
        family: &str,
    ) -> Result<&mut Self, String> {
        if self.fonts.iter().any(|s| s.source.font_id() == font_id) {
            return Err(format!("重复的 font_id {font_id}"));
        }
        let source = AbGlyphSource::from_bytes(font_id, bytes)?;
        let weight = source.weight_class();
        self.fonts.push(FontSlot {
            meta: FontMeta::new(family, weight),
            source,
        });
        Ok(self)
    }

    /// 加入一份字体。`font_id` 必须唯一 —— 它进图集键，重复会让同一字形存两份。
    pub fn add(&mut self, font_id: u32, bytes: &[u8], meta: FontMeta) -> Result<&mut Self, String> {
        if self.fonts.iter().any(|s| s.source.font_id() == font_id) {
            return Err(format!("重复的 font_id {font_id}"));
        }
        let source = AbGlyphSource::from_bytes(font_id, bytes)?;
        self.fonts.push(FontSlot { meta, source });
        Ok(self)
    }

    pub fn len(&self) -> usize {
        self.fonts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fonts.is_empty()
    }

    pub fn font_ids(&self) -> Vec<u32> {
        self.fonts.iter().map(|s| s.source.font_id()).collect()
    }

    /// 某份字体的元数据（按 `font_id` 查）。调用方可用它诊断回退结果。
    pub fn meta(&self, font_id: u32) -> Option<&FontMeta> {
        self.fonts
            .iter()
            .find(|s| s.source.font_id() == font_id)
            .map(|s| &s.meta)
    }

    /// 为一段文本选字体索引。
    ///
    /// 先按覆盖范围过滤（没有这些字的字体不参与），再按 family 匹配度与字重距离
    /// 打分。没有字体覆盖时返回 0 —— 那里会由第 0 份字体产出 U+FFFD。
    fn select(&self, text: &str, style: &TextStyle) -> usize {
        let mut best = 0usize;
        let mut best_score = i32::MIN;
        for (i, slot) in self.fonts.iter().enumerate() {
            let covered = text.chars().any(|ch| Self::has_glyph(&slot.source, ch));
            if !covered {
                continue;
            }
            let mut score = 0i32;
            match &style.family {
                Some(f) => {
                    let f = f.to_ascii_lowercase();
                    let fam = slot.meta.family.to_ascii_lowercase();
                    if fam == f {
                        score += 10_000;
                    } else if fam.contains(&f) {
                        score += 3_000;
                    }
                }
                None if i == 0 => score += 500,
                None => {}
            }
            score -= (slot.meta.weight as i32 - style.weight as i32).abs() * 4;
            if slot.meta.italic {
                score -= 1;
            }
            // 同分时靠前的候选胜出，使结果稳定。
            score -= i as i32;
            if score > best_score {
                best_score = score;
                best = i;
            }
        }
        best
    }

    fn has_glyph(source: &AbGlyphSource, ch: char) -> bool {
        let Ok(face) = rustybuzz::ttf_parser::Face::parse(source.font_bytes(), 0) else {
            return false;
        };
        face.glyph_index(ch).map(|g| g.0 != 0).unwrap_or(false)
    }

    /// 请求字重高于实际选中字体时，合成一个伪粗体阈值。
    ///
    /// 尺度：SDF 编码值 1.0 对应 `2 × SDF_SPREAD = 16` 个基准像素，所以 `0.02`
    /// 约等于把轮廓向外推 0.32 基准像素 —— 400→600 的轻量合成。上限避免
    /// 极端字重把笔画糊成一团。
    fn embolden_for(&self, idx: usize, style: &TextStyle) -> f32 {
        let actual = self.fonts[idx].meta.weight as i32;
        let want = style.weight as i32;
        ((want - actual).clamp(0, 400) as f32 / 400.0 * 0.04).min(0.04)
    }

    /// 把一段"连续、同字体"的字素簇整形后追加到 `out`。
    #[allow(clippy::too_many_arguments)]
    fn push_segment(
        &mut self,
        text: &str,
        out: &mut Vec<ShapedGlyph>,
        start: usize,
        end: usize,
        fidx: usize,
        rtl: bool,
        px_size: f32,
        style: &TextStyle,
    ) {
        let upem = self.fonts[fidx].source.units_per_em();
        let scale = px_size / upem;
        let embolden = self.embolden_for(fidx, style);
        let slot = &mut self.fonts[fidx];
        let mut shaped = slot
            .source
            .shape_run(&text[start..end], rtl, px_size, scale, &style.font_features);
        for g in &mut shaped.glyphs {
            // `shape_run` 的 cluster 是段内字节偏移；回退链的分析基于整段文本。
            g.cluster.start += start;
            g.cluster.end += start;
            g.font_id = slot.source.font_id();
            g.embolden = embolden;
        }
        out.extend(shaped.glyphs);
    }
}

impl GlyphSource for FontSet {
    fn glyph_id(&mut self, ch: char) -> u32 {
        self.glyph_in(ch, &TextStyle::default()).1
    }

    fn glyph_in(&mut self, ch: char, style: &TextStyle) -> (u32, u32) {
        if self.fonts.is_empty() {
            return (0, 0);
        }
        let idx = self.select(&ch.to_string(), style);
        let slot = &mut self.fonts[idx];
        let gid = slot.source.glyph_id(ch);
        (slot.source.font_id(), gid)
    }

    fn advance(&mut self, glyph_id: u32, px_size: f32) -> f32 {
        self.fonts
            .first_mut()
            .map(|s| s.source.advance(glyph_id, px_size))
            .unwrap_or(0.0)
    }

    fn metrics(&self, px_size: f32) -> Option<FontMetrics> {
        // 行框取默认字体的度量。混排时逐字体度量不同是常态，但强制定 strut
        // 的语义就是"所有行用同一套框"，所以这里用第 0 份字体作为统一基准。
        self.fonts.first()?.source.metrics(px_size)
    }

    fn set_variations(&mut self, variations: &[([u8; 4], f32)]) {
        // 每份字体都同步，回退切换时不必再设置一次。
        for slot in &mut self.fonts {
            slot.source.set_variations(variations);
        }
    }

    fn color_layers_in(&self, font_id: u32, glyph_id: u32) -> Option<Vec<super::ColorLayer>> {
        self.fonts
            .iter()
            .find(|s| s.source.font_id() == font_id)?
            .source
            .color_layers(glyph_id)
    }

    fn advance_in(&mut self, font_id: u32, glyph_id: u32, px_size: f32) -> f32 {
        self.fonts
            .iter_mut()
            .find(|s| s.source.font_id() == font_id)
            .map(|s| s.source.advance(glyph_id, px_size))
            .unwrap_or(0.0)
    }

    fn rasterize(&mut self, glyph_id: u32, px_size: u32) -> Option<AtlasRaster> {
        self.fonts.first_mut()?.source.rasterize(glyph_id, px_size)
    }

    fn rasterize_in(&mut self, font_id: u32, glyph_id: u32, px_size: u32) -> Option<AtlasRaster> {
        self.fonts
            .iter_mut()
            .find(|s| s.source.font_id() == font_id)?
            .source
            .rasterize(glyph_id, px_size)
    }

    fn shape(&mut self, text: &str, style: &TextStyle) -> Vec<ShapedRun> {
        if text.is_empty() || self.fonts.is_empty() {
            return Vec::new();
        }
        let px_size = style.effective_size().max(1.0);
        let mut out = Vec::new();
        for (range, rtl) in bidi_runs(text, style.direction) {
            let run_text = &text[range.clone()];
            // 字素簇边界：回退切换不能切在 ZWJ 序列 / 组合记号中间。
            let clusters: Vec<(usize, usize)> = run_text
                .grapheme_indices(true)
                .map(|(i, s)| (range.start + i, range.start + i + s.len()))
                .collect();
            let Some(first) = clusters.first() else {
                continue;
            };
            let script = dominant_script(run_text).short_name();
            let mut glyphs: Vec<ShapedGlyph> = Vec::new();

            let mut seg_start = first.0;
            let mut seg_end = first.1;
            let mut seg_font = self.select(&text[seg_start..seg_end], style);
            for &(cs, ce) in clusters.iter().skip(1) {
                let f = self.select(&text[cs..ce], style);
                if f == seg_font {
                    seg_end = ce;
                } else {
                    self.push_segment(
                        text,
                        &mut glyphs,
                        seg_start,
                        seg_end,
                        seg_font,
                        rtl,
                        px_size,
                        style,
                    );
                    seg_start = cs;
                    seg_end = ce;
                    seg_font = f;
                }
            }
            self.push_segment(
                text,
                &mut glyphs,
                seg_start,
                seg_end,
                seg_font,
                rtl,
                px_size,
                style,
            );

            if !glyphs.is_empty() {
                out.push(ShapedRun {
                    glyphs,
                    rtl,
                    script,
                });
            }
        }
        out
    }

    fn font_id(&self) -> u32 {
        self.fonts.first().map(|s| s.source.font_id()).unwrap_or(0)
    }
}

/// 多字体文本栈：从指定文件 + emoji 字体构造。
///
/// 与单字体版不同，这里总是把一份 emoji 字体加进回退链，这样 COLR/CBDT 彩色
/// emoji 在 UI 里能直接出现，而不必依赖宿主单独处理。
pub fn from_font_file_set(path: &str) -> (TextStack<FontSet>, String) {
    let mut set = FontSet::new();
    if let Ok(bytes) = std::fs::read(path) {
        if !bytes.is_empty() {
            let _ = set.add_with_family(0, &bytes, &family_from_path(path));
        }
    }
    add_emoji(&mut set, 100);
    let count = set.len();
    (TextStack::new(set), format!("字体栈 {path}（{count} 份）"))
}

/// 多字体文本栈：环境变量指定的字体优先，随后是系统候选与 emoji 字体。
pub fn from_system_font_set() -> (TextStack<FontSet>, String) {
    let mut set = FontSet::new();
    let mut paths: Vec<String> = Vec::new();
    if let Ok(p) = std::env::var(super::FONT_OVERRIDE_ENV) {
        if !p.is_empty() {
            paths.push(p);
        }
    }
    paths.extend(super::ab_glyph_source::FONT_CANDIDATES.iter().map(|s| s.to_string()));
    paths.extend(EMOJI_CANDIDATES.iter().map(|s| s.to_string()));
    let mut tried = 0usize;
    for (i, path) in paths.iter().enumerate() {
        tried += 1;
        if let Ok(bytes) = std::fs::read(path) {
            if !bytes.is_empty() && set.add_with_family(i as u32, &bytes, &family_from_path(path)).is_err() {
                tried -= 1;
            }
        }
    }
    let count = set.len();
    (
        TextStack::new(set),
        format!("字体栈（{count} 份，试过 {tried} 个路径）"),
    )
}

const EMOJI_CANDIDATES: &[&str] = &[
    "C:\\Windows\\Fonts\\seguiemj.ttf",
    "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf",
];

fn family_from_path(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "default".into())
}

fn add_emoji(set: &mut FontSet, font_id: u32) {
    for path in EMOJI_CANDIDATES {
        if let Ok(bytes) = std::fs::read(path) {
            if !bytes.is_empty() && set.add_with_family(font_id, &bytes, "Emoji").is_ok() {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::TextEngine;
    use crate::text::TextStack;

    fn font(path: &str) -> Vec<u8> {
        std::fs::read(path).expect("测试字体应可读")
    }

    #[test]
    fn font_set_rejects_duplicate_ids() {
        let bytes = font(r"C:\Windows\Fonts\segoeui.ttf");
        let mut set = FontSet::new();
        set.add(1, &bytes, FontMeta::new("A", 400)).unwrap();
        assert!(set.add(1, &bytes, FontMeta::new("B", 700)).is_err());
    }

    #[test]
    fn missing_cjk_falls_through_to_second_font() {
        // segoeui.ttf 无 CJK，msyh.ttc 有。用两个 font_id 区分来源。
        let latin = font(r"C:\Windows\Fonts\segoeui.ttf");
        let cjk = font(r"C:\Windows\Fonts\msyh.ttc");
        let mut set = FontSet::new();
        set.add(10, &latin, FontMeta::new("Segoe UI", 400)).unwrap();
        set.add(11, &cjk, FontMeta::new("Microsoft YaHei", 400))
            .unwrap();

        let (_fid, g) = set.glyph_in('中', &TextStyle::default());
        assert_ne!(g, 0, "中字应落到第二份字体上");
        let (fid, _) = set.glyph_in('中', &TextStyle::default());
        assert_eq!(fid, 11, "中字应选含 CJK 的那份字体");

        let (fid_l, _) = set.glyph_in('A', &TextStyle::default());
        assert_eq!(fid_l, 10, "拉丁应留在第一份字体");
    }

    #[test]
    fn mixed_script_shape_produces_both_font_ids() {
        let latin = font(r"C:\Windows\Fonts\segoeui.ttf");
        let cjk = font(r"C:\Windows\Fonts\msyh.ttc");
        let mut set = FontSet::new();
        set.add(10, &latin, FontMeta::new("Segoe UI", 400)).unwrap();
        set.add(11, &cjk, FontMeta::new("Microsoft YaHei", 400))
            .unwrap();
        let mut stack = TextStack::new(set);
        let shaped = stack.shape("AB中文CD", &TextStyle::default(), None);
        let ids: std::collections::HashSet<u32> = shaped
            .lines
            .iter()
            .flat_map(|l| l.glyphs.iter())
            .map(|g| g.font_id)
            .collect();
        assert!(
            ids.contains(&10) && ids.contains(&11),
            "混排应产出两个 font_id，实得 {ids:?}"
        );
    }

    #[test]
    fn weight_selection_prefers_exact_match() {
        let regular = font(r"C:\Windows\Fonts\segoeui.ttf");
        let bold = font(r"C:\Windows\Fonts\segoeuib.ttf");
        if std::fs::metadata(r"C:\Windows\Fonts\segoeuib.ttf").is_err() {
            return;
        }
        let mut set = FontSet::new();
        set.add(20, &regular, FontMeta::new("Segoe UI", 400))
            .unwrap();
        set.add(21, &bold, FontMeta::new("Segoe UI", 700)).unwrap();
        let style = TextStyle {
            weight: 700,
            family: Some("Segoe UI".into()),
            ..Default::default()
        };
        let (fid, _) = set.glyph_in('A', &style);
        assert_eq!(fid, 21, "weight=700 应选粗体");
    }
}
