//! NV12 → RGB 的纯逻辑门禁：与 camera 的 `to_rgba` 参考色逐值一致。
//!
//! 没有把 camera 加依赖（vector 的核心门禁是零外部依赖），所以这里把 camera
//! `tests/pixels_tests.rs::nv12_to_rgba_bt601_limited_range_reference_colors`
//! 的参考值抄进来做对偶断言 —— 公式两边各自独立实现，漂移会当场红。

use crate::nv12::{nv12_planes_to_rgba8, nv12_to_rgb8};

/// camera 参考：`white = Y235/U128/V128 → (254,254,254)`；
/// `black = Y16 → (0,0,0)`；`red = Y81/U90/V240 → (254,0,0)`。
#[test]
fn nv12_reference_colors_match_camera_to_rgba() {
    assert_eq!(nv12_to_rgb8(235, 128, 128), [254, 254, 254]);
    assert_eq!(nv12_to_rgb8(16, 128, 128), [0, 0, 0]);
    assert_eq!(nv12_to_rgb8(81, 90, 240), [254, 0, 0]);
    // 任务书点名的偏绿场景：Y=200/U=V=128 必须是中性灰，不能有绿色偏色。
    let gray = nv12_to_rgb8(200, 128, 128);
    assert_eq!(gray, [214, 214, 214]);
    assert_eq!(gray[0], gray[1]);
    assert_eq!(gray[1], gray[2]);
}

/// 与浮点 BT.601 limited range 公式交叉验证：整数近似与精确值的偏差 ≤ 1。
#[test]
fn nv12_integer_formula_stays_within_one_of_float_reference() {
    let samples = [
        (16u8, 128u8, 128u8),
        (81, 90, 240),
        (128, 64, 192),
        (200, 100, 156),
        (235, 16, 240),
        (219, 240, 16),
        (64, 200, 40),
    ];
    for (y, u, v) in samples {
        let got = nv12_to_rgb8(y, u, v);
        let yy = (y as f32 - 16.0) * (298.0 / 256.0);
        let u = u as f32 - 128.0;
        let v = v as f32 - 128.0;
        let exact = [
            (yy + (409.0 / 256.0) * v).clamp(0.0, 255.0),
            (yy - (100.0 / 256.0) * u - (208.0 / 256.0) * v).clamp(0.0, 255.0),
            (yy + (516.0 / 256.0) * u).clamp(0.0, 255.0),
        ];
        for (g, e) in got.iter().zip(exact.iter()) {
            assert!(
                (*g as f32 - *e).abs() <= 1.0,
                "Y={y} U={u} V={v}: got {g}, exact {e}"
            );
        }
    }
}

/// 两平面布局：Y 逐像素，UV 每 2×2 块共用一个 (U,V)，alpha 恒 255。
#[test]
fn nv12_planes_to_rgba8_follows_camera_indexing() {
    let (w, h) = (4usize, 2usize);
    let y_plane = vec![200u8; w * h];
    let uv_plane = vec![128u8; (w / 2) * (h / 2) * 2];
    let rgba = nv12_planes_to_rgba8(&y_plane, &uv_plane, w, h).expect("合法 NV12 平面");
    assert_eq!(rgba.len(), w * h * 4);
    for px in rgba.chunks_exact(4) {
        assert_eq!(px, &[214, 214, 214, 255]);
    }
}

#[test]
fn nv12_planes_reject_bad_dimensions_and_short_buffers() {
    assert!(nv12_planes_to_rgba8(&[0; 9], &[0; 4], 3, 3).is_none()); // 奇数尺寸
    assert!(nv12_planes_to_rgba8(&[0; 3], &[0; 2], 2, 2).is_none()); // Y 平面短一字节
    assert!(nv12_planes_to_rgba8(&[0; 4], &[0; 1], 2, 2).is_none()); // UV 平面短一字节
    assert!(nv12_planes_to_rgba8(&[0; 4], &[0; 2], 2, 2).is_some()); // 恰好够
}

/// 着色器源里两套方言都要有 YUV→RGB 转换，且 RGBA 主入口默认不开。
#[test]
fn shader_sources_carry_nv12_variants_without_touching_default_path() {
    let hlsl = crate::shaders::HLSL_ENTRY;
    assert!(hlsl.contains("ZEPHYRA_NV12"), "HLSL 缺 NV12 开关");
    let wgsl = crate::shaders::wgsl_source();
    assert!(wgsl.contains("fn fs_main_nv12"), "WGSL 缺 NV12 入口");
    assert!(wgsl.contains("fn nv12_to_rgb"), "WGSL 缺转换函数");
    assert!(
        wgsl.contains("shade_source(i, false)"),
        "RGBA 主入口必须默认关闭"
    );
    // 15 个变体字节码都非空（include_bytes 不会是空切片）。
    assert_eq!(crate::shaders::PS_NV12_VARIANTS.len(), 15);
    assert!(crate::shaders::PS_NV12_VARIANTS
        .iter()
        .all(|(_, b)| !b.is_empty()));
    assert!(crate::shaders::nv12_ps_for_blend_mode(0).is_some());
    assert!(crate::shaders::nv12_ps_for_blend_mode(28).is_some());
    assert!(crate::shaders::nv12_ps_for_blend_mode(29).is_none());
}
