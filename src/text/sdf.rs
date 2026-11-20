//! 有符号距离场（SDF）：把覆盖率位图变成可任意缩放的字形表示。
//!
//! # 为什么不按显示字号栅格化
//!
//! 覆盖率位图与字号绑定：14.5px 的字只能栅格化成 14px 或 15px 的位图，
//! 而排版步进仍按 14.5px —— 位图与落点错位，双线性采样再一插值就发虚；
//! 而图集固定 1024²，单个字形超过它就直接画不出来。SDF 把"距离"而非
//! "覆盖率"存进图集：一份基准分辨率的距离场，绘制时按 smoothstep 重建
//! 覆盖率，于是任意字号都锐利、且没有尺寸上限。

/// SDF 编码范围（基准像素）。真实距离 `d ∈ [-spread, +spread]` 映射到 `[0,1]`。
pub const SDF_SPREAD: f32 = 8.0;

/// 默认（最小）SDF 基准字号（em 像素），也是分级表的第一档。
pub const SDF_BASE_PX: u32 = 64;

/// 多分辨率 SDF 分级。单通道 SDF 若在基准分辨率上过度放大，笔画会变钝、
/// 圆角会被抹平；按显示字号选"不小于它"的最小基准，绘制时只缩小不放大，
/// 于是任意字号都有足够的距离场采样密度。
pub const SDF_TIERS: [u32; 3] = [64, 128, 256];

/// 给显示字号选一档基准。超过最大档时退回最大档（仍比原来的单档好得多）。
pub fn sdf_tier(display_px: f32) -> u32 {
    let d = display_px.max(1.0);
    for &tier in &SDF_TIERS {
        if tier as f32 >= d {
            return tier;
        }
    }
    SDF_TIERS[SDF_TIERS.len() - 1]
}

/// 图集留白（像素）。必须 ≥ `SDF_SPREAD`，否则轮廓外侧的距离场采不到。
pub const SDF_PADDING: u32 = SDF_SPREAD as u32;

/// 距离（基准像素）→ u8。`0.5`（编码 128）对应轮廓边缘。
pub fn encode(distance: f32) -> u8 {
    let v = 0.5 + distance / (2.0 * SDF_SPREAD);
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// u8 → 距离（基准像素）。仅供测试与诊断使用。
pub fn decode(v: u8) -> f32 {
    ((v as f32 / 255.0) - 0.5) * 2.0 * SDF_SPREAD
}

const INF: f32 = 1e30;

/// 一维平方欧氏距离变换（Felzenszwalb–Huttenlocher，O(n)）。
///
/// `f` 中距离为 0 的位置是"源点"，其余为 `INF`。返回每个位置到最近源点的
/// **平方**距离。
fn edt_1d(f: &[f32]) -> Vec<f32> {
    let n = f.len();
    let mut d = vec![0.0; n];
    if n == 0 {
        return d;
    }
    let mut v = vec![0usize; n];
    let mut z = vec![0.0; n + 1];
    let mut k = 0usize;
    v[0] = 0;
    z[0] = -INF;
    z[1] = INF;
    for q in 1..n {
        let mut s = ((f[q] + (q * q) as f32) - (f[v[k]] + (v[k] * v[k]) as f32))
            / (2.0 * q as f32 - 2.0 * v[k] as f32);
        while s <= z[k] {
            k -= 1;
            s = ((f[q] + (q * q) as f32) - (f[v[k]] + (v[k] * v[k]) as f32))
                / (2.0 * q as f32 - 2.0 * v[k] as f32);
        }
        k += 1;
        v[k] = q;
        z[k] = s;
        z[k + 1] = INF;
    }
    k = 0;
    for q in 0..n {
        while z[k + 1] < q as f32 {
            k += 1;
        }
        let dx = q as f32 - v[k] as f32;
        d[q] = dx * dx + f[v[k]];
    }
    d
}

/// 二维平方 EDT：逐行、逐列各做一次一维变换。
fn edt_2d(f: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut tmp = vec![0.0; w * h];
    let mut out = vec![0.0; w * h];
    for y in 0..h {
        let row = &f[y * w..(y + 1) * w];
        tmp[y * w..(y + 1) * w].copy_from_slice(&edt_1d(row));
    }
    for x in 0..w {
        let col: Vec<f32> = (0..h).map(|y| tmp[y * w + x]).collect();
        let d = edt_1d(&col);
        for y in 0..h {
            out[y * w + x] = d[y];
        }
    }
    out
}

/// 从覆盖率位图生成带 `margin` 留白的 SDF。
///
/// 返回的缓冲尺寸为 `(width + 2*margin) × (height + 2*margin)`，第 `(0,0)`
/// 个像素是**留白区域**的左上角（墨迹从 `(margin, margin)` 开始）。图集直接
/// 把它写进 padded 矩形；`uv`/`size`/`offset` 仍按墨迹记录。
pub fn from_coverage(coverage: &[u8], width: u32, height: u32, margin: u32) -> Vec<u8> {
    let (wi, hi, m) = (width as usize, height as usize, margin as usize);
    let (mw, mh) = (wi + 2 * m, hi + 2 * m);
    let mut inside = vec![INF; mw * mh]; // inside 像素为 0 → EDT 给出到最近 inside 的距离
    let mut outside = vec![INF; mw * mh]; // outside 像素为 0 → EDT 给出到最近 outside 的距离
    for y in 0..hi {
        for x in 0..wi {
            let idx = (y + m) * mw + (x + m);
            if coverage.get(y * wi + x).copied().unwrap_or(0) >= 128 {
                inside[idx] = 0.0;
            } else {
                outside[idx] = 0.0;
            }
        }
    }
    let d_inside = edt_2d(&inside, mw, mh);
    let d_outside = edt_2d(&outside, mw, mh);
    let mut sdf = vec![0u8; mw * mh];
    for i in 0..mw * mh {
        let dist = if inside[i] == 0.0 {
            d_outside[i].sqrt()
        } else {
            -d_inside[i].sqrt()
        };
        sdf[i] = encode(dist);
    }
    sdf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_selection_never_upscales() {
        assert_eq!(sdf_tier(1.0), 64);
        assert_eq!(sdf_tier(16.0), 64);
        assert_eq!(sdf_tier(64.0), 64);
        assert_eq!(sdf_tier(65.0), 128);
        assert_eq!(sdf_tier(128.0), 128);
        assert_eq!(sdf_tier(200.0), 256);
        // 超过最大档时退回最大档（仍远好于单档 64）。
        assert_eq!(sdf_tier(2000.0), 256);
        for px in [1.0, 16.0, 64.0, 65.0, 200.0, 2000.0] {
            assert!(sdf_tier(px) as f32 >= px.min(256.0), "tier < display: {px}");
        }
    }

    #[test]
    fn encode_decode_roundtrip() {
        for d in [-8.0, -4.0, -0.5, 0.0, 0.5, 4.0, 8.0] {
            assert!((decode(encode(d)) - d).abs() <= 0.1, "d={d}");
        }
    }

    #[test]
    fn center_of_solid_block_is_far_inside() {
        // 8×8 全实心，margin=8：中心应接近 +8（饱和到 spread）。
        let sdf = from_coverage(&vec![255u8; 64], 8, 8, 8);
        let (mw, mh) = (24, 24);
        assert_eq!(sdf.len(), mw * mh);
        assert_eq!(sdf[(8 + 4) * mw + 8 + 4], 255, "中心应为实心最内侧");
    }

    #[test]
    fn outside_margin_is_negative() {
        let sdf = from_coverage(&vec![255u8; 64], 8, 8, 8);
        let mw = 24;
        // 留白左上角离墨迹约 11px（对角），超出 spread，应编码为 0。
        assert_eq!(sdf[0], 0);
        // 紧贴墨迹上方的留白（8 行 0 列）距离约 1px → 负值但接近 0.5。
        let v = sdf[7 * mw + 8];
        assert!(v < 128, "外侧应编码为负值：{v}");
    }
}
