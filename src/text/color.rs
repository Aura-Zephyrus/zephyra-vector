//! COLR v0 / v1 彩色字形（emoji）解析。
//!
//! # 为什么用 ttf-parser 的 `Painter`
//!
//! COLR v0 是「baseglyph → layer records + CPAL 调色板」，自己读表就够了；
//! 但 Windows 的 Segoe UI Emoji 用的是 **COLR v1**（paint graph，含渐变），
//! 自己解析 paint graph 的工作量很大。ttf-parser 0.25 提供 `Face::paint_color_glyph`
//! 与 `colr::Painter`，对 v0/v1 一视同仁：它把每一层的轮廓 glyph 与颜色按调用
//! 顺序回调出来，我们只需要把「当前轮廓 + 当前颜色」收成 `ColorLayer` 即可。
//!
//! 渐变取第一个色标作为该层的近似颜色（MSDF 只支持纯色 quad，无法逐像素渐变）。
//! 这样至少保证彩色 emoji 不再退化成黑白，且零新增依赖。

use std::marker::PhantomData;

use rustybuzz::ttf_parser::colr::{ClipBox, CompositeMode, Paint, Painter};
use rustybuzz::ttf_parser::{Face, GlyphId, RgbaColor, Tag};

use crate::core::Color;

/// COLR 的一层：一个字形 id + 它在当前调色板里的颜色。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorLayer {
    /// 该层的字形 id（在字体里是普通 glyph id，可独立栅格化）。
    pub glyph_id: u32,
    /// 该层颜色（来自 CPAL 调色板 0）。
    pub color: Color,
}

fn rgba(c: RgbaColor) -> Color {
    Color::rgba(
        c.red as f32 / 255.0,
        c.green as f32 / 255.0,
        c.blue as f32 / 255.0,
        c.alpha as f32 / 255.0,
    )
}

/// 渐变的第一个不透明色标；若全透明则退到第一个。MSDF quad 只能是纯色，
/// 取不透明色标能让彩色 emoji 不选到「渐变起点透明」那层而整层消失。
fn gradient_color<'a>(stops: impl Iterator<Item = rustybuzz::ttf_parser::colr::ColorStop>) -> Color {
    let mut first = None;
    for stop in stops {
        let c = rgba(stop.color);
        if first.is_none() {
            first = Some(c);
        }
        if c.a > 0.5 {
            return c;
        }
    }
    first.unwrap_or(Color::rgb(1.0, 1.0, 1.0))
}

/// 收集 `paint_color_glyph` 回调出的「轮廓 + 颜色」对。
struct LayerPainter<'a> {
    layers: Vec<ColorLayer>,
    pending_glyph: Option<u32>,
    _lt: PhantomData<&'a ()>,
}

impl<'a> Painter<'a> for LayerPainter<'a> {
    fn outline_glyph(&mut self, glyph_id: GlyphId) {
        self.pending_glyph = Some(glyph_id.0 as u32);
    }

    fn paint(&mut self, paint: Paint<'a>) {
        let color = match paint {
            Paint::Solid(c) => rgba(c),
            Paint::LinearGradient(g) => gradient_color(g.stops(0, &[])),
            Paint::RadialGradient(g) => gradient_color(g.stops(0, &[])),
            Paint::SweepGradient(g) => gradient_color(g.stops(0, &[])),
        };
        if let Some(gid) = self.pending_glyph.take() {
            self.layers.push(ColorLayer {
                glyph_id: gid,
                color,
            });
        }
    }

    fn push_clip(&mut self) {}
    fn push_clip_box(&mut self, _clipbox: ClipBox) {}
    fn pop_clip(&mut self) {}
    fn push_layer(&mut self, _mode: CompositeMode) {}
    fn pop_layer(&mut self) {}
    fn push_transform(&mut self, _transform: rustybuzz::ttf_parser::Transform) {}
    fn pop_transform(&mut self) {}
}

/// 按需解析**单个** glyph 的 COLR 分层（v0 与 v1）。
///
/// 不能像早期版本那样在字体加载时把整张 COLR 表 3487 个 glyph 全跑一遍 ——
/// Segoe UI Emoji 的 paint graph 很深，实测会让启动卡住几十秒。这里只解析
/// 真正被排版到的字形，结果由 `AbGlyphSource` 缓存。
pub fn parse_color_glyph(font_bytes: &[u8], glyph_id: u32) -> Option<Vec<ColorLayer>> {
    let face = Face::parse(font_bytes, 0).ok()?;
    if face.raw_face().table(Tag::from_bytes(b"COLR")).is_none() {
        return None;
    }
    let mut painter = LayerPainter {
        layers: Vec::new(),
        pending_glyph: None,
        _lt: PhantomData,
    };
    face.paint_color_glyph(
        GlyphId(glyph_id as u16),
        0,
        RgbaColor::new(255, 255, 255, 255),
        &mut painter,
    )?;
    (!painter.layers.is_empty()).then_some(painter.layers)
}
