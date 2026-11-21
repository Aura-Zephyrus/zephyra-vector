//! COLR v0 / v1 彩色字形（emoji）解析。
//!
//! # 为什么用 ttf-parser 的 `Painter`
//!
//! COLR v0 是「baseglyph → layer records + CPAL 调色板」；COLR v1 是 paint graph，
//! 含 transform / clip / composite / 渐变。ttf-parser 0.25 的 `Face::paint_color_glyph`
//! 与 `colr::Painter` 把每层的轮廓 glyph、颜色以及当前变换/裁剪回调出来，我们收成
//! `ColorLayer` 即可，零新增依赖。
//!
//! # 精确合成
//!
//! COLR 层不再展开成多片纯色 MSDF quad，而是由 [`crate::text::color_raster`]
//! 在栅格化阶段把真实渐变、仿射变换、裁剪盒与 `CompositeMode` 逐像素合成进一张
//! RGBA 位图（一个字形 = 一个 quad = 一次 ColorRgba 采样）。调色板固定 palette 0，
//! 与 `paint_color_glyph(..., 0, ...)` 一致。

use std::marker::PhantomData;

use rustybuzz::ttf_parser::colr::{ClipBox, CompositeMode, Paint, Painter};
use rustybuzz::ttf_parser::{Face, GlyphId, RgbaColor, Tag, Transform};

use crate::core::{Color, Rect};
use crate::filter::BlendMode;

/// 渐变的延展方式（COLR `GradientExtend`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GradientExtend {
    Pad,
    Repeat,
    Reflect,
}

/// COLR v1 渐变几何（字体单位，y-up）。
#[derive(Debug, Clone, PartialEq)]
pub enum GradientKind {
    Linear {
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
    },
    Radial {
        x0: f32,
        y0: f32,
        r0: f32,
        x1: f32,
        y1: f32,
        r1: f32,
    },
    Sweep {
        cx: f32,
        cy: f32,
        start: f32,
        end: f32,
    },
}

/// 一个真实的渐变（色标 + 几何 + 延展），用于栅格化时逐像素求值。
#[derive(Debug, Clone, PartialEq)]
pub struct GradientPaint {
    pub kind: GradientKind,
    pub extend: GradientExtend,
    /// `(offset, color)`，按 offset 升序。
    pub stops: Vec<(f32, Color)>,
}

/// COLR 的一层：一个字形 id + 它在当前调色板里的颜色及几何修饰。
#[derive(Debug, Clone, PartialEq)]
pub struct ColorLayer {
    /// 该层的字形 id（在字体里是普通 glyph id，可独立栅格化）。
    pub glyph_id: u32,
    /// 该层颜色（纯色 paint；渐变层为第一个色标，仅供调试显示）。
    pub color: Color,
    /// 该层的真实渐变（若 paint 是渐变）。栅格化时逐像素求值，不再近似为纯色。
    pub gradient: Option<GradientPaint>,
    /// paint graph 的仿射变换 `[a,b,c,d,e,f]`（字体单位）。
    pub transform: Option<[f32; 6]>,
    /// paint graph 的裁剪盒（字体单位，已转成 y-down）。
    pub clip: Option<Rect>,
    /// 层混合模式（当前仅作信息保留，绘制按 SourceOver 顺序叠加）。
    pub blend: Option<BlendMode>,
}

fn rgba(c: RgbaColor) -> Color {
    Color::rgba(
        c.red as f32 / 255.0,
        c.green as f32 / 255.0,
        c.blue as f32 / 255.0,
        c.alpha as f32 / 255.0,
    )
}

fn compose(a: [f32; 6], b: [f32; 6]) -> [f32; 6] {
    // 行优先 2x3：x' = a*x + c*y + e, y' = b*x + d*y + f。
    [
        a[0] * b[0] + a[2] * b[1],
        a[1] * b[0] + a[3] * b[1],
        a[0] * b[2] + a[2] * b[3],
        a[1] * b[2] + a[3] * b[3],
        a[0] * b[4] + a[2] * b[5] + a[4],
        a[1] * b[4] + a[3] * b[5] + a[5],
    ]
}

fn to_matrix(t: Transform) -> [f32; 6] {
    [t.a, t.b, t.c, t.d, t.e, t.f]
}

fn clip_to_rect(c: ClipBox) -> Rect {
    Rect::new(
        c.x_min,
        -c.y_max,
        c.x_max - c.x_min,
        c.y_max - c.y_min,
    )
}

fn blend_from(mode: CompositeMode) -> Option<BlendMode> {
    use BlendMode::*;
    Some(match mode {
        CompositeMode::Clear => Clear,
        CompositeMode::Source => Src,
        CompositeMode::Destination => Dst,
        CompositeMode::SourceOver => SrcOver,
        CompositeMode::DestinationOver => DstOver,
        CompositeMode::SourceIn => SrcIn,
        CompositeMode::DestinationIn => DstIn,
        CompositeMode::SourceOut => SrcOut,
        CompositeMode::DestinationOut => DstOut,
        CompositeMode::SourceAtop => SrcATop,
        CompositeMode::DestinationAtop => DstATop,
        CompositeMode::Xor => Xor,
        CompositeMode::Plus => Plus,
        _ => return None,
    })
}

fn collect_stops<'a>(
    stops: impl Iterator<Item = rustybuzz::ttf_parser::colr::ColorStop>,
) -> Vec<(f32, Color)> {
    let mut v: Vec<_> = stops
        .map(|s| (s.stop_offset, rgba(s.color)))
        .collect();
    v.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    v
}

fn extend_of(e: rustybuzz::ttf_parser::colr::GradientExtend) -> GradientExtend {
    match e {
        rustybuzz::ttf_parser::colr::GradientExtend::Pad => GradientExtend::Pad,
        rustybuzz::ttf_parser::colr::GradientExtend::Repeat => GradientExtend::Repeat,
        rustybuzz::ttf_parser::colr::GradientExtend::Reflect => GradientExtend::Reflect,
    }
}

fn representative(stops: &[(f32, Color)]) -> Color {
    stops
        .iter()
        .find(|(_, c)| c.a > 0.5)
        .or_else(|| stops.first())
        .map(|(_, c)| *c)
        .unwrap_or(Color::rgb(1.0, 1.0, 1.0))
}

/// 收集 `paint_color_glyph` 回调出的「轮廓 + 颜色 + 变换/裁剪」。
struct LayerPainter<'a> {
    layers: Vec<ColorLayer>,
    pending: Option<(u32, Option<[f32; 6]>, Option<Rect>, Option<BlendMode>)>,
    transforms: Vec<[f32; 6]>,
    clips: Vec<Rect>,
    _lt: PhantomData<&'a ()>,
}

impl<'a> LayerPainter<'a> {
    fn current_transform(&self) -> Option<[f32; 6]> {
        self.transforms.last().copied()
    }
    fn current_clip(&self) -> Option<Rect> {
        self.clips.last().copied()
    }
}

impl<'a> Painter<'a> for LayerPainter<'a> {
    fn outline_glyph(&mut self, glyph_id: GlyphId) {
        self.pending = Some((
            glyph_id.0 as u32,
            self.current_transform(),
            self.current_clip(),
            None,
        ));
    }

    fn paint(&mut self, paint: Paint<'a>) {
        let (color, gradient) = match paint {
            Paint::Solid(c) => (rgba(c), None),
            Paint::LinearGradient(g) => {
                let stops = collect_stops(g.stops(0, &[]));
                let gp = GradientPaint {
                    kind: GradientKind::Linear {
                        x0: g.x0, y0: g.y0, x1: g.x1, y1: g.y1, x2: g.x2, y2: g.y2,
                    },
                    extend: extend_of(g.extend),
                    stops: stops.clone(),
                };
                (representative(&stops), Some(gp))
            }
            Paint::RadialGradient(g) => {
                let stops = collect_stops(g.stops(0, &[]));
                let gp = GradientPaint {
                    kind: GradientKind::Radial {
                        x0: g.x0, y0: g.y0, r0: g.r0, x1: g.x1, y1: g.y1, r1: g.r1,
                    },
                    extend: extend_of(g.extend),
                    stops: stops.clone(),
                };
                (representative(&stops), Some(gp))
            }
            Paint::SweepGradient(g) => {
                let stops = collect_stops(g.stops(0, &[]));
                let gp = GradientPaint {
                    kind: GradientKind::Sweep {
                        cx: g.center_x, cy: g.center_y, start: g.start_angle, end: g.end_angle,
                    },
                    extend: extend_of(g.extend),
                    stops: stops.clone(),
                };
                (representative(&stops), Some(gp))
            }
        };
        if let Some((glyph_id, transform, clip, blend)) = self.pending.take() {
            self.layers.push(ColorLayer {
                glyph_id,
                color,
                gradient,
                transform,
                clip,
                blend,
            });
        }
    }

    fn push_clip(&mut self) {
        // 用当前轮廓的 bbox 作为 clip 需要额外轮廓 API；ttf-parser 的 paint graph
        // 里 push_clip 通常紧跟 outline_glyph，这里退化为「不裁剪」，
        // push_clip_box 才是精确路径。
    }
    fn push_clip_box(&mut self, clipbox: ClipBox) {
        self.clips.push(clip_to_rect(clipbox));
    }
    fn pop_clip(&mut self) {
        self.clips.pop();
    }
    fn push_layer(&mut self, mode: CompositeMode) {
        if let Some((_, _, _, ref mut b)) = self.pending {
            *b = blend_from(mode);
        }
    }
    fn pop_layer(&mut self) {}
    fn push_transform(&mut self, transform: Transform) {
        let t = to_matrix(transform);
        let next = match self.current_transform() {
            Some(cur) => compose(cur, t),
            None => t,
        };
        self.transforms.push(next);
    }
    fn pop_transform(&mut self) {
        self.transforms.pop();
    }
}

/// 按需解析**单个** glyph 的 COLR 分层（v0 与 v1）。
pub fn parse_color_glyph(font_bytes: &[u8], glyph_id: u32) -> Option<Vec<ColorLayer>> {
    let face = Face::parse(font_bytes, 0).ok()?;
    if face.raw_face().table(Tag::from_bytes(b"COLR")).is_none() {
        return None;
    }
    let mut painter = LayerPainter {
        layers: Vec::new(),
        pending: None,
        transforms: Vec::new(),
        clips: Vec::new(),
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
