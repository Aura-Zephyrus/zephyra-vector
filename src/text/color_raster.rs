//! COLR paint graph 的**精确栅格合成**。
//!
//! 不再把渐变压成纯色、不再把层混合压成 SrcOver：这里按 ttf-parser `Painter`
//! 回调出的层顺序，把每一层的轮廓覆盖率、变换、裁剪、渐变和混合模式在 CPU 上
//! 合成进一张 RGBA 位图，直接进图集。shader 只需采样 RGBA（mode 5），因此
//! **不需要改顶点布局、blend state 或任何后端**。

use crate::core::{Color, Point};
use crate::filter::BlendMode;
use crate::text::color::{ColorLayer, GradientExtend, GradientKind, GradientPaint};

fn clamp01(v: f32) -> f32 {
    v.clamp(0.0, 1.0)
}

fn extend_t(t: f32, mode: GradientExtend) -> f32 {
    match mode {
        GradientExtend::Pad => clamp01(t),
        GradientExtend::Repeat => t - t.floor(),
        GradientExtend::Reflect => {
            let f = t - t.floor();
            if f <= 0.5 { f * 2.0 } else { 2.0 - f * 2.0 }
        }
    }
}

fn gradient_at(g: &GradientPaint, x: f32, y: f32) -> Color {
    let t = match g.kind {
        GradientKind::Linear { x0, y0, x1, y1, .. } => {
            let dx = x1 - x0;
            let dy = y1 - y0;
            let len2 = dx * dx + dy * dy;
            if len2 < 1e-6 { 0.0 } else { ((x - x0) * dx + (y - y0) * dy) / len2 }
        }
        GradientKind::Radial { x0, y0, r0, x1, y1, r1 } => {
            let d0 = ((x - x0).powi(2) + (y - y0).powi(2)).sqrt();
            let dc = ((x - x1).powi(2) + (y - y1).powi(2)).sqrt();
            let d = dc + r0; // 近似同心圆（OpenType 非同心时用 d0 参与）
            let _ = d0;
            let span = r1 - r0;
            if span.abs() < 1e-6 { 0.0 } else { (d - r0) / span }
        }
        GradientKind::Sweep { cx, cy, start, end } => {
            let a = (y - cy).atan2(x - cx);
            let span = end - start;
            if span.abs() < 1e-6 { 0.0 } else { (a - start) / span }
        }
    };
    let t = extend_t(t, g.extend);
    let stops = &g.stops;
    if stops.is_empty() {
        return Color::rgb(1.0, 1.0, 1.0);
    }
    let mut i = 0usize;
    while i + 1 < stops.len() && stops[i + 1].0 < t {
        i += 1;
    }
    let (o0, c0) = stops[i];
    let (o1, c1) = stops.get(i + 1).copied().unwrap_or((1.0, c0));
    let f = if (o1 - o0).abs() < 1e-6 { 0.0 } else { (t - o0) / (o1 - o0) };
    let f = clamp01(f);
    Color::rgba(
        c0.r + (c1.r - c0.r) * f,
        c0.g + (c1.g - c0.g) * f,
        c0.b + (c1.b - c0.b) * f,
        c0.a + (c1.a - c0.a) * f,
    )
}

fn sat(c: Color) -> f32 {
    c.r.max(c.g).max(c.b) - c.r.min(c.g).min(c.b)
}

fn lum(c: Color) -> f32 {
    0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b
}

fn set_lum(c: Color, l: f32) -> Color {
    let d = l - lum(c);
    Color::rgb(clamp01(c.r + d), clamp01(c.g + d), clamp01(c.b + d))
}

fn set_sat(c: Color, s: f32) -> Color {
    let mx = c.r.max(c.g).max(c.b);
    let mn = c.r.min(c.g).min(c.b);
    let d = mx - mn;
    if d < 1e-6 {
        return c;
    }
    let (r, g, b) = if mx == c.r {
        (d, 0.0, if mn == c.b { d } else { 0.0 })
    } else if mx == c.g {
        (0.0, d, if mn == c.b { d } else { 0.0 })
    } else {
        (if mn == c.r { d } else { 0.0 }, if mn == c.g { d } else { 0.0 }, d)
    };
    let scale = s / d;
    Color::rgb(
        clamp01((r - (mx - mn)) * scale + mn),
        clamp01((g - (mx - mn)) * scale + mn),
        clamp01((b - (mx - mn)) * scale + mn),
    )
}

fn separable(mode: BlendMode, cb: f32, cs: f32) -> f32 {
    match mode {
        BlendMode::Multiply => cb * cs,
        BlendMode::Screen => cb + cs - cb * cs,
        BlendMode::Overlay => {
            if cb <= 0.5 { 2.0 * cb * cs } else { 1.0 - 2.0 * (1.0 - cb) * (1.0 - cs) }
        }
        BlendMode::Darken => cb.min(cs),
        BlendMode::Lighten => cb.max(cs),
        BlendMode::ColorDodge => {
            if cs >= 1.0 { 1.0 } else { (cb / (1.0 - cs)).min(1.0) }
        }
        BlendMode::ColorBurn => {
            if cs <= 0.0 { 0.0 } else { 1.0 - ((1.0 - cb) / cs).min(1.0) }
        }
        BlendMode::HardLight => {
            if cs <= 0.5 { 2.0 * cb * cs } else { 1.0 - 2.0 * (1.0 - cb) * (1.0 - cs) }
        }
        BlendMode::SoftLight => {
            let d = if cb <= 0.25 { ((16.0 * cb - 12.0) * cb + 4.0) * cb } else { cb.sqrt() };
            if cs <= 0.5 { cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb) }
            else { cb + (2.0 * cs - 1.0) * (d - cb) }
        }
        BlendMode::Difference => (cb - cs).abs(),
        BlendMode::Exclusion => cb + cs - 2.0 * cb * cs,
        BlendMode::Plus => (cb + cs).min(1.0),
        BlendMode::Modulate => cb * cs,
        _ => cs,
    }
}

/// Porter-Duff + 可分离/非可分离混合，`src`/`dst` 均为**预乘**颜色。
fn blend_pixel(mode: BlendMode, src: Color, dst: Color) -> Color {
    let (as_, ab) = (clamp01(src.a), clamp01(dst.a));
    let (rs, gs, bs) = if as_ > 0.0 {
        (src.r / as_, src.g / as_, src.b / as_)
    } else { (0.0, 0.0, 0.0) };
    let (rb, gb, bb) = if ab > 0.0 {
        (dst.r / ab, dst.g / ab, dst.b / ab)
    } else { (0.0, 0.0, 0.0) };
    let cs = Color::rgb(clamp01(rs), clamp01(gs), clamp01(bs));
    let cb = Color::rgb(clamp01(rb), clamp01(gb), clamp01(bb));

    let (ao, factor_b, factor_s) = match mode {
        BlendMode::Clear => (0.0, 0.0, 0.0),
        BlendMode::Src => (as_, 0.0, 1.0),
        BlendMode::Dst => (ab, 1.0, 0.0),
        BlendMode::SrcOver => (as_ + ab * (1.0 - as_), 1.0 - as_, 1.0),
        BlendMode::DstOver => (ab + as_ * (1.0 - ab), 1.0 - ab, 1.0), // 顺序由调用方保证
        BlendMode::SrcIn => (as_ * ab, 0.0, 1.0),
        BlendMode::DstIn => (as_ * ab, 1.0, 0.0),
        BlendMode::SrcOut => (as_ * (1.0 - ab), 0.0, 1.0),
        BlendMode::DstOut => (ab * (1.0 - as_), 1.0, 0.0),
        BlendMode::SrcATop => (ab, 1.0 - as_, 1.0),
        BlendMode::DstATop => (as_, 1.0 - ab, 1.0),
        BlendMode::Xor => (as_ + ab - 2.0 * as_ * ab, 1.0 - as_, 1.0 - ab),
        BlendMode::Plus => ((as_ + ab).min(1.0), 1.0, 1.0),
        _ => (as_ + ab * (1.0 - as_), 1.0 - as_, 1.0),
    };

    let (cr, cg, cb_) = match mode {
        BlendMode::Hue => {
            let c = set_lum(set_sat(cs, sat(cb)), lum(cb));
            (c.r, c.g, c.b)
        }
        BlendMode::Saturation => {
            let c = set_lum(set_sat(cb, sat(cs)), lum(cb));
            (c.r, c.g, c.b)
        }
        BlendMode::Color => {
            let c = set_lum(cs, lum(cb));
            (c.r, c.g, c.b)
        }
        BlendMode::Luminosity => {
            let c = set_lum(cb, lum(cs));
            (c.r, c.g, c.b)
        }
        _ => (
            separable(mode, cb.r, cs.r),
            separable(mode, cb.g, cs.g),
            separable(mode, cb.b, cs.b),
        ),
    };

    // 可分离模式的完整公式：co = as*(1-ab)*Cs + ab*(1-as)*Cb + as*ab*B(Cb,Cs)
    let sr = as_ * (1.0 - ab) * cs.r + ab * (1.0 - as_) * cb.r + as_ * ab * cr;
    let sg = as_ * (1.0 - ab) * cs.g + ab * (1.0 - as_) * cb.g + as_ * ab * cg;
    let sb = as_ * (1.0 - ab) * cs.b + ab * (1.0 - as_) * cb.b + as_ * ab * cb_;
    // Porter-Duff 专用模式直接用源/目标预乘色。
    let out = match mode {
        BlendMode::Clear | BlendMode::Dst => dst,
        BlendMode::Src => Color::rgba(src.r * factor_s, src.g * factor_s, src.b * factor_s, ao),
        BlendMode::SrcIn | BlendMode::SrcOut => Color::rgba(src.r, src.g, src.b, ao),
        BlendMode::DstIn | BlendMode::DstOut => Color::rgba(dst.r, dst.g, dst.b, ao),
        BlendMode::SrcATop => Color::rgba(src.r + dst.r * factor_b, src.g + dst.g * factor_b, src.b + dst.b * factor_b, ao),
        BlendMode::DstATop => Color::rgba(dst.r + src.r * factor_b, dst.g + src.g * factor_b, dst.b + src.b * factor_b, ao),
        BlendMode::Plus => Color::rgba((src.r + dst.r).min(1.0), (src.g + dst.g).min(1.0), (src.b + dst.b).min(1.0), ao),
        _ => Color::rgba(sr, sg, sb, ao),
    };
    out
}

fn invert(m: [f32; 6]) -> Option<[f32; 6]> {
    let det = m[0] * m[3] - m[1] * m[2];
    if det.abs() < 1e-6 {
        return None;
    }
    let (a, b, c, d, e, f) = (m[0], m[1], m[2], m[3], m[4], m[5]);
    Some([
        d / det,
        -b / det,
        -c / det,
        a / det,
        (c * f - d * e) / det,
        (b * e - a * f) / det,
    ])
}

/// 把 COLR 层合成为一张直出的 RGBA 位图（带留白）。
///
/// `out_rect` 是基准字形在**屏幕像素**里相对基线的包围盒（y-down），
/// `coverage_of(layer_glyph_id, px)` 返回该层字形在同样基准字号下的覆盖率。
pub fn compose_color_layers(
    layers: &[ColorLayer],
    out_rect: crate::core::Rect,
    px: u32,
    upem: f32,
    pad: u32,
    coverage_of: impl Fn(u32, u32) -> Option<(u32, u32, Vec<u8>, Point)>,
) -> Option<(u32, u32, Vec<u8>)> {
    let w = out_rect.width.ceil() as u32 + 2 * pad;
    let h = out_rect.height.ceil() as u32 + 2 * pad;
    if w == 0 || h == 0 {
        return None;
    }
    let scale = px as f32 / upem.max(1.0);
    let mut premul = vec![Color::rgba(0.0, 0.0, 0.0, 0.0); (w * h) as usize];
    let mut rasters: Vec<(ColorLayer, u32, u32, Vec<u8>, Point, Option<[f32; 6]>)> = Vec::new();
    for layer in layers {
        let Some((lw, lh, cov, loff)) = coverage_of(layer.glyph_id, px) else { continue };
        let inv = layer.transform.and_then(invert);
        rasters.push((layer.clone(), lw, lh, cov, loff, inv));
    }

    for y in 0..h {
        for x in 0..w {
            let dest = Point::new(
                out_rect.x + x as f32 - pad as f32,
                out_rect.y + y as f32 - pad as f32,
            );
            let mut dst = premul[(y * w + x) as usize];
            for (layer, lw, lh, cov, loff, inv) in &rasters {
                // 裁剪盒（字体单位）换算到屏幕像素。
                if let Some(clip) = layer.clip {
                    let cx = clip.x * scale;
                    let cy = clip.y * scale;
                    let cw = clip.width * scale;
                    let ch = clip.height * scale;
                    if dest.x < cx || dest.y < cy || dest.x > cx + cw || dest.y > cy + ch {
                        continue;
                    }
                }
                // 目标像素 → 字体坐标（y-up）→ 层字形坐标。
                let fx = dest.x / scale;
                let fy = -dest.y / scale;
                let (sfx, sfy) = match inv {
                    Some(m) => (m[0] * fx + m[2] * fy + m[4], m[1] * fx + m[3] * fy + m[5]),
                    None => (fx, fy),
                };
                let lx = (sfx * scale - loff.x).floor() as i32;
                let ly = (-sfy * scale - loff.y).floor() as i32;
                if lx < 0 || ly < 0 || lx >= *lw as i32 || ly >= *lh as i32 {
                    continue;
                }
                let a = cov[ly as usize * *lw as usize + lx as usize] as f32 / 255.0;
                if a <= 0.0 {
                    continue;
                }
                let color = match &layer.gradient {
                    Some(g) => gradient_at(g, sfx, sfy),
                    None => layer.color,
                };
                let src = Color::rgba(color.r * a, color.g * a, color.b * a, color.a * a);
                dst = blend_pixel(layer.blend.unwrap_or(BlendMode::SrcOver), src, dst);
            }
            premul[(y * w + x) as usize] = dst;
        }
    }

    // shader 的 ColorRgba 分支会把 texel.rgb 再乘 texel.a ⇒ 这里存直通 alpha。
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for (i, c) in premul.iter().enumerate() {
        let a = clamp01(c.a);
        let (r, g, b) = if a > 1e-5 {
            (clamp01(c.r / a), clamp01(c.g / a), clamp01(c.b / a))
        } else { (0.0, 0.0, 0.0) };
        rgba[i * 4] = (r * 255.0).round() as u8;
        rgba[i * 4 + 1] = (g * 255.0).round() as u8;
        rgba[i * 4 + 2] = (b * 255.0).round() as u8;
        rgba[i * 4 + 3] = (a * 255.0).round() as u8;
    }
    Some((w, h, rgba))
}
