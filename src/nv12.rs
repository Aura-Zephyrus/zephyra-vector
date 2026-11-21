//! NV12 → RGB 的 CPU 参考实现（着色器公式的对偶端）。
//!
//! 与 `shaders/hlsl/shade.hlsli::nv12_to_rgb` 和 `shaders/wgsl/10_shade.wgsl.part`
//! 的 `nv12_to_rgb` **逐值一致**；数值口径与 `zephyra-camera::Nv12Planes::to_rgba`
//! 相同（BT.601 limited range，整数近似 `>>8`）。
//!
//! 本模块零依赖，用途有三：
//! 1. 门禁：把 YUV 三元组喂进它，逐值比对 camera 的参考色；
//! 2. CPU 离线宿主（`--dump-cpu`）：给 NV12 源提供不依赖 GPU 的 RGBA 出口；
//! 3. 文档：着色器里那串魔数（298 / 409 / 100 / 208 / 516）在这里有可执行的解释。

/// BT.601 limited range NV12 单像素 → RGB8。
///
/// `y/u/v` 是 NV12 采样得到的**字节**（不是归一化浮点）：Y 平面给 `y`，
/// 交错 UV 平面给 `u`（偶字节）与 `v`（奇字节）。alpha 恒 255，由调用方处理。
pub fn nv12_to_rgb8(y: u8, u: u8, v: u8) -> [u8; 3] {
    let yy = (y as i32 - 16) * 298;
    let u = u as i32 - 128;
    let v = v as i32 - 128;
    [
        ((yy + 409 * v) >> 8).clamp(0, 255) as u8,
        ((yy - 100 * u - 208 * v) >> 8).clamp(0, 255) as u8,
        ((yy + 516 * u) >> 8).clamp(0, 255) as u8,
    ]
}

/// 两平面 NV12 → 逐像素 RGBA8，平面布局与 camera / D3D11 importer 一致：
/// `y_plane` 为 `width * height` 字节，`uv_plane` 为 `(width/2)*(height/2)*2` 字节
/// （交错 U、V）。尺寸为奇数或缓冲不足时返回 `None` —— 静默截断会让最后一行
/// 取到相邻平面的字节，症状是「画面底部一条彩带」。
pub fn nv12_planes_to_rgba8(
    y_plane: &[u8],
    uv_plane: &[u8],
    width: usize,
    height: usize,
) -> Option<Vec<u8>> {
    if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
        return None;
    }
    let y_len = width * height;
    let uv_w = width / 2;
    let uv_len = uv_w * (height / 2) * 2;
    if y_plane.len() < y_len || uv_plane.len() < uv_len {
        return None;
    }
    let mut out = Vec::with_capacity(y_len * 4);
    for row in 0..height {
        for col in 0..width {
            let i = row * width + col;
            let uv_i = (row / 2) * uv_w * 2 + (col / 2) * 2;
            let [r, g, b] = nv12_to_rgb8(y_plane[i], uv_plane[uv_i], uv_plane[uv_i + 1]);
            out.extend_from_slice(&[r, g, b, 255]);
        }
    }
    Some(out)
}
