//! 多通道有符号距离场（MSDF）生成。
//!
//! # 为什么单通道 SDF 会把拐角磨圆
//!
//! 单通道 SDF 在真实距离场的**中轴**（medial axis）上不可微：一个拐角附近，
//! 两条边的距离场在此相交。位图把它采样成 8 位、绘制时再双线性插值，中轴处
//! 的折角就被抹成一个圆弧。字号放大得越多、这个圆弧越明显。
//!
//! MSDF 把"到哪条边的距离"分开存进 RGB：每条边按其法线方向归到三个通道之一，
//! 片元里取 `median(r,g,b)`。中轴处三通道的值不再相等，中位数挑出的那条恰好
//! 保留尖角 —— 这是 msdfgen 的核心思想。
//!
//! # 本实现与完整 msdfgen 的差别
//!
//! - 用**真实 SDF**（已有 EDT）作为"没有同色边"的通道的回退值；这样平滑边缘上
//!   三个通道一致，中位数就是真实距离。
//! - 曲线按弦长均匀细分而非 msdfgen 的自适应细分。基准档最高 256px，分段数
//!   足够让误差落到半个基准像素以下。
//! - 不做 msdfgen 的"错误校正"后处理。上面那条回退策略已经把误差限制在中轴
//!   附近；若测试暴露伪影，再加中点校正。
//!
//! 这些取舍不影响 API：输入是轮廓线段 + 覆盖率位图，输出是 RGBA8（RGB = 三通道
//! MSDF，A = 真实 SDF 灰度副本，便于把图集当图片看）。

use rustybuzz::ttf_parser::{Face, GlyphId, OutlineBuilder};

use super::sdf::{decode, encode};

/// 一段轮廓（已换算到墨迹相对坐标，y 向下，单位 = 基准像素）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    pub a: (f32, f32),
    pub b: (f32, f32),
}

impl Segment {
    pub fn new(a: (f32, f32), b: (f32, f32)) -> Self {
        Self { a, b }
    }
}

/// 三个通道的方向（相隔 120°）。边法线最接近哪个方向，就归到哪个通道。
const CHANNEL_DIRS: [(f32, f32); 3] = [
    (1.0, 0.0),
    (-0.5, 0.866_025_4),
    (-0.5, -0.866_025_4),
];

/// 轮廓收集器：`ttf_parser` 回调的是字体单位、y 向上的坐标，这里只记录原始点，
/// 换算到像素坐标与 y 翻转交给 [`outline_segments`]。
#[derive(Default)]
struct Collector {
    contours: Vec<Vec<(f32, f32)>>,
    current: Vec<(f32, f32)>,
}

impl OutlineBuilder for Collector {
    fn move_to(&mut self, x: f32, y: f32) {
        if !self.current.is_empty() {
            self.contours.push(std::mem::take(&mut self.current));
        }
        self.current.push((x, y));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.current.push((x, y));
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let Some(&p0) = self.current.last() else {
            return;
        };
        const N: usize = 8;
        for i in 1..=N {
            let t = i as f32 / N as f32;
            let u = 1.0 - t;
            self.current.push((
                u * u * p0.0 + 2.0 * u * t * x1 + t * t * x,
                u * u * p0.1 + 2.0 * u * t * y1 + t * t * y,
            ));
        }
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let Some(&p0) = self.current.last() else {
            return;
        };
        // 弦长决定细分数：基准档越大，需要的分段越多。上限防止退化字体卡死。
        let chord = ((x - p0.0).powi(2) + (y - p0.1).powi(2)).sqrt();
        let n = (chord / 3.0).round().clamp(8.0, 32.0) as usize;
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            let a = u * u * u;
            let b = 3.0 * u * u * t;
            let c = 3.0 * u * t * t;
            let d = t * t * t;
            self.current.push((
                a * p0.0 + b * x1 + c * x2 + d * x,
                a * p0.1 + b * y1 + c * y2 + d * y,
            ));
        }
    }

    fn close(&mut self) {
        if !self.current.is_empty() {
            self.contours.push(std::mem::take(&mut self.current));
        }
    }
}

/// 从字体字节提取字形轮廓，换算到墨迹相对坐标。
///
/// `px_per_unit = px / upem`（与 `ab_glyph` 内部的水平缩放因子一致），
/// `origin` 是 `ab_glyph` 报出的 `px_bounds.min` —— 两者一起保证 MSDF 与
/// 覆盖率位图像素对齐。
pub fn outline_segments(
    font_bytes: &[u8],
    glyph_id: u16,
    px_per_unit: f32,
    origin: (f32, f32),
) -> Vec<Segment> {
    let Ok(face) = Face::parse(font_bytes, 0) else {
        return Vec::new();
    };
    let mut collector = Collector::default();
    if face
        .outline_glyph(GlyphId(glyph_id), &mut collector)
        .is_none()
    {
        return Vec::new();
    }
    if collector.current.len() > 1 {
        collector.contours.push(std::mem::take(&mut collector.current));
    }

    // 先变换后求 bbox，再用 origin 对齐（正常情况下 bbox.min 就等于 origin）。
    let mut transformed: Vec<Vec<(f32, f32)>> = collector
        .contours
        .into_iter()
        .map(|c| {
            c.into_iter()
                .map(|(x, y)| (x * px_per_unit - origin.0, -y * px_per_unit - origin.1))
                .collect()
        })
        .collect();
    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    for c in &transformed {
        for &(x, y) in c {
            min_x = min_x.min(x);
            min_y = min_y.min(y);
        }
    }
    // 变换后已经是**墨迹相对坐标**（屏幕坐标减 origin），bbox.min 理论上是 0。
    // 只减去残差做对齐 —— 绝不能加回 origin，那会把整条轮廓平移出墨迹区域。
    if min_x.is_finite() && min_y.is_finite() {
        for c in &mut transformed {
            for p in c {
                p.0 -= min_x;
                p.1 -= min_y;
            }
        }
    }

    let mut segments = Vec::new();
    for c in transformed {
        let n = c.len();
        if n < 2 {
            continue;
        }
        for i in 0..n {
            let a = c[i];
            let b = c[(i + 1) % n];
            if ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)) > 1e-8 {
                segments.push(Segment::new(a, b));
            }
        }
    }
    segments
}

/// 点到线段的无符号距离。
fn distance_to_segment(p: (f32, f32), s: &Segment) -> f32 {
    let dx = s.b.0 - s.a.0;
    let dy = s.b.1 - s.a.1;
    let len2 = dx * dx + dy * dy;
    if len2 <= 0.0 {
        return ((p.0 - s.a.0).powi(2) + (p.1 - s.a.1).powi(2)).sqrt();
    }
    let t = (((p.0 - s.a.0) * dx + (p.1 - s.a.1) * dy) / len2).clamp(0.0, 1.0);
    let qx = s.a.0 + t * dx;
    let qy = s.a.1 + t * dy;
    ((p.0 - qx).powi(2) + (p.1 - qy).powi(2)).sqrt()
}

/// 覆盖率查询（墨迹坐标）。越界视为外部。
fn inside(coverage: &[u8], w: u32, h: u32, p: (f32, f32)) -> bool {
    let x = p.0.floor() as i32;
    let y = p.1.floor() as i32;
    if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
        return false;
    }
    coverage[(y as usize) * (w as usize) + (x as usize)] >= 128
}

/// 给每条边分配通道。用覆盖率测试法线方向：若法线正侧在形状内，说明它是内法线，翻转。
fn assign_colors(segments: &[Segment], coverage: &[u8], w: u32, h: u32) -> Vec<usize> {
    segments
        .iter()
        .map(|s| {
            let dx = s.b.0 - s.a.0;
            let dy = s.b.1 - s.a.1;
            let len = (dx * dx + dy * dy).sqrt().max(1e-6);
            let mut nx = -dy / len;
            let mut ny = dx / len;
            let mid = ((s.a.0 + s.b.0) * 0.5, (s.a.1 + s.b.1) * 0.5);
            if inside(
                coverage,
                w,
                h,
                (mid.0 + nx * 0.75, mid.1 + ny * 0.75),
            ) {
                nx = -nx;
                ny = -ny;
            }
            let mut best = 0usize;
            let mut best_dot = f32::NEG_INFINITY;
            for (i, &(ux, uy)) in CHANNEL_DIRS.iter().enumerate() {
                let d = nx * ux + ny * uy;
                if d > best_dot {
                    best_dot = d;
                    best = i;
                }
            }
            best
        })
        .collect()
}

/// 生成带留白的 MSDF 缓冲：`(w + 2m) × (h + 2m)` 个 RGBA8 像素。
///
/// `coverage` 是墨迹覆盖率（行宽 `w`），它同时提供"真实距离"回退与边的内外判定。
pub fn generate(
    coverage: &[u8],
    width: u32,
    height: u32,
    margin: u32,
    segments: &[Segment],
) -> Vec<u8> {
    let (wi, hi, m) = (width as usize, height as usize, margin as usize);
    let (mw, mh) = (wi + 2 * m, hi + 2 * m);
    let genuine = super::sdf::from_coverage(coverage, width, height, margin);
    let colors = assign_colors(segments, coverage, width, height);
    let by_color: [Vec<usize>; 3] = {
        let mut groups = [Vec::new(), Vec::new(), Vec::new()];
        for (i, &c) in colors.iter().enumerate() {
            groups[c].push(i);
        }
        groups
    };

    let mut out = vec![0u8; mw * mh * 4];
    for sy in 0..mh {
        for sx in 0..mw {
            let idx = sy * mw + sx;
            let g = decode(genuine[idx]);
            let ink = (sx as f32 - m as f32 + 0.5, sy as f32 - m as f32 + 0.5);
            let is_in = inside(coverage, width, height, ink);
            for c in 0..3 {
                let mut best = f32::INFINITY;
                for &si in &by_color[c] {
                    let d = distance_to_segment(ink, &segments[si]);
                    if d < best {
                        best = d;
                    }
                }
                let d = if best.is_finite() {
                    // 符号必须与 `sdf::encode` 一致：**内侧为正、外侧为负**
                    // （`smoothstep(0.5-w, 0.5+w, s)` 在高值处给 1 = 墨迹）。
                    if is_in { best } else { -best }
                } else {
                    // 该方向没有边：用真实距离，保证平滑边缘上三通道一致。
                    g
                };
                out[idx * 4 + c] = encode(d);
            }
            // alpha 留真实 SDF 灰度副本：把图集导成图片时仍能看出字形。
            out[idx * 4 + 3] = genuine[idx];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::sdf::SDF_SPREAD;
    use crate::text::GlyphSource;

    /// 直角轮廓：中位距离应比真实 SDF 更"贴边"，即拐角更尖。
    #[test]
    fn corner_stays_sharp_under_median() {
        let (w, h, m) = (8u32, 8u32, 8u32);
        let coverage = vec![255u8; (w * h) as usize];
        let segments = vec![
            Segment::new((0.0, 0.0), (8.0, 0.0)),
            Segment::new((8.0, 0.0), (8.0, 8.0)),
            Segment::new((8.0, 8.0), (0.0, 8.0)),
            Segment::new((0.0, 8.0), (0.0, 0.0)),
        ];
        let out = generate(&coverage, w, h, m, &segments);
        let mw = (w + 2 * m) as usize;
        // 右上角外侧：(9.5, 9.5)。到右边/上边各 1.5px，真实距离 √2·1.5 ≈ 2.12。
        let sx = m as usize + 9;
        let sy = m as usize + 9;
        let px = &out[(sy * mw + sx) * 4..][..4];
        let (r, g, b) = (decode(px[0]), decode(px[1]), decode(px[2]));
        let median = r.max(g).min(b.max(r.min(g)));
        let genuine = decode(px[3]);
        // 仓内约定"内侧为正"：外侧两点都是负值。中位距离应比真实 SDF 更靠 0，
        // 即拐角外侧的距离场上升更慢 —— 重建出的拐角更尖。
        assert!(
            median > genuine + 0.15,
            "中位距离 {median} 应比真实 SDF {genuine} 更贴边（拐角更尖），rgb=({r:.2},{g:.2},{b:.2})"
        );
        // 到两条正交边**线段**的最近点都是角点 (8,8)，距离 √2·1.5 ≈ 2.12。
        assert!(
            (median + 2.12).abs() < 0.3,
            "中位距离应接近 -2.12（到角点的距离），实得 {median}"
        );
        assert!(SDF_SPREAD > 0.0);
    }

    /// 真实字体：轮廓必须落在墨迹坐标里，且 MSDF 确实带三通道信息。
    #[test]
    fn real_glyph_outline_aligns_and_has_multichannel_data() {
        let bytes = match std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") {
            Ok(b) => b,
            Err(_) => return,
        };
        let mut src = crate::text::AbGlyphSource::from_bytes(0, &bytes).unwrap();
        let gid = src.glyph_id('H');
        let px = 64u32;
        let raster = src.rasterize(gid, px).expect("'H' 应有位图");
        let upem = src.units_per_em();
        let segs = outline_segments(
            &bytes,
            gid as u16,
            px as f32 / upem,
            (raster.offset.x, raster.offset.y),
        );
        assert!(!segs.is_empty(), "真实字形应能提取到轮廓线段");
        let min_x = segs.iter().map(|s| s.a.0.min(s.b.0)).fold(f32::MAX, f32::min);
        let min_y = segs.iter().map(|s| s.a.1.min(s.b.1)).fold(f32::MAX, f32::min);
        assert!(
            min_x.abs() <= 1.5 && min_y.abs() <= 1.5,
            "轮廓应对齐到墨迹原点 (0,0)，实得 ({min_x:.2},{min_y:.2})"
        );

        let different = raster
            .sdf
            .chunks_exact(4)
            .filter(|p| p[0] != p[1] || p[1] != p[2])
            .count();
        assert!(
            different > 0,
            "真实字形的 MSDF 应有三通道差异（拐角信息），实得 0"
        );
    }

    #[test]
    fn smooth_edge_has_identical_channels() {
        let (w, h, m) = (16u32, 16u32, 8u32);
        let coverage = vec![255u8; (w * h) as usize];
        // 全实心：没有边，三通道都应回退到真实 SDF 且彼此相等。
        let out = generate(&coverage, w, h, m, &[]);
        for px in out.chunks_exact(4) {
            assert_eq!(px[0], px[1]);
            assert_eq!(px[1], px[2]);
        }
    }
}
