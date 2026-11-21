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

use std::collections::HashMap;
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

fn u16at(data: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_be_bytes(data.get(off..off + 2)?.try_into().ok()?))
}

/// 从 COLR 表头读出所有定义了彩色涂层的 glyph id。
///
/// v0 与 v1 的 baseglyph 记录都是 6 字节、首 2 字节为 glyph id，只是记录数量
/// 字段宽度不同（v0 = u16，v1 = u32）。
fn colored_glyph_ids(colr: &[u8]) -> Vec<u32> {
    let Some(version) = u16at(colr, 0) else {
        return Vec::new();
    };
    let read_u32 = |off: usize| -> Option<u32> {
        Some(u32::from_be_bytes(colr.get(off..off + 4)?.try_into().ok()?))
    };
    let (count, records_offset) = match version {
        0 => (
            u16at(colr, 2).unwrap_or(0) as usize,
            read_u32(4).unwrap_or(0) as usize,
        ),
        1 => (read_u32(2).unwrap_or(0) as usize, read_u32(6).unwrap_or(0) as usize),
        _ => return Vec::new(),
    };
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let off = records_offset + i * 6;
        if let Some(gid) = u16at(colr, off) {
            out.push(gid as u32);
        }
    }
    out
}

/// 解析字体里的 COLR 彩色字形表（v0 与 v1）。无相关表或表损坏时返回空表。
pub fn parse_color_layers(font_bytes: &[u8]) -> HashMap<u32, Vec<ColorLayer>> {
    let mut out = HashMap::new();
    let Ok(face) = Face::parse(font_bytes, 0) else {
        return out;
    };
    let Some(colr) = face.raw_face().table(Tag::from_bytes(b"COLR")) else {
        return out;
    };
    for gid in colored_glyph_ids(colr) {
        let mut painter = LayerPainter {
            layers: Vec::new(),
            pending_glyph: None,
            _lt: PhantomData,
        };
        if face
            .paint_color_glyph(GlyphId(gid as u16), 0, RgbaColor::new(255, 255, 255, 255), &mut painter)
            .is_some()
            && !painter.layers.is_empty()
        {
            out.insert(gid, painter.layers);
        }
    }
    out
}
