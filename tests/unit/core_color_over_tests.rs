//! `Color::over` 的核心不变量。
//!
//! 这些断言钉住的是**契约**，不是实现细节：换实现时它们应当继续成立。
//! 尤其要盯住"alpha 合成 ≠ lerp"：`lerp` 是颜色的直觉插值，而 `over`
//! 遵循 `out = src + dst·(1 − src.a)`，半透明层叠时两者结果明显不同。

use crate::core::Color;

#[test]
fn alpha_composite_differs_from_lerp_over_translucent_backdrop() {
    // 关键前提：dst 必须**半透明**。dst 不透明时 out_a 恒为 1，`over` 在数学上
    // 退化成 lerp —— 那是恒等式而非巧合（见下一个测试）。
    //
    // 真实场景：半透明浮层叠在半透明遮罩上。用 lerp 会得到两层颜色的直觉中点，
    // 而合成必须让**下层透过上层**，结果明显不同。
    let src = Color::rgba(1.0, 0.0, 0.0, 0.5);
    let dst = Color::rgba(0.0, 0.0, 1.0, 0.5);
    let over = src.over(dst);
    let lerped = src.lerp(dst, 0.5);

    assert_ne!(over, lerped, "半透明背景上合成与插值不是一回事");
    // out_a = 0.75 → 红渗入更多、蓝被压低，两者不再对称。
    assert!(
        (over.a - 0.75).abs() < 1e-6,
        "合成后透明度应累加，实得 {}",
        over.a
    );
    assert!(
        over.r > over.b,
        "上层红应主导，实得 r={} b={}",
        over.r,
        over.b
    );
    assert!((over.a - lerped.a).abs() > 1e-6);
}

#[test]
fn composite_over_opaque_backdrop_is_exactly_lerp() {
    // 这是一个**恒等式**，不是巧合：dst.a = 1 时 out.a = 1，于是
    //   r = src.r·a + dst.r·(1−a) = lerp(dst, src, a)
    // 值得钉住，因为一旦有人给 `over` 加"更聪明"的处理，这条会最先失效。
    let src = Color::rgba(1.0, 0.0, 0.0, 0.5);
    let dst = Color::rgba(0.0, 0.0, 1.0, 1.0);
    let over = src.over(dst);
    assert!((over.r - 0.5).abs() < 1e-6);
    assert!((over.g - 0.0).abs() < 1e-6);
    assert!((over.b - 0.5).abs() < 1e-6);
    assert!((over.a - 1.0).abs() < 1e-6, "不透明背景下合成结果应不透明");
    assert_eq!(over.r, over.b, "对半叠加应得到对称的紫");
}

#[test]
fn faint_source_lets_backdrop_dominate() {
    // 10% 的红叠在不透明蓝上：合成应几乎保持蓝。
    // 用 lerp 实现浮层会让深色背景明显发灰 —— 这是 UI 里能直接看出来的缺陷。
    let src = Color::rgba(1.0, 0.0, 0.0, 0.1);
    let dst = Color::rgba(0.0, 0.0, 1.0, 1.0);
    let over = src.over(dst);
    assert!(over.b > 0.85, "蓝色应占主导，实得 {}", over.b);
    assert!(over.r < 0.15, "红色只应渗入一点，实得 {}", over.r);
}

#[test]
fn over_transparent_src_is_identity() {
    let dst = Color::rgb(0.2, 0.4, 0.6);
    let transparent = Color::rgba(0.0, 0.0, 0.0, 0.0);
    assert_eq!(transparent.over(dst), dst);
}

#[test]
fn over_opaque_src_replaces_dst() {
    let dst = Color::rgb(0.2, 0.4, 0.6);
    let src = Color::rgb(1.0, 1.0, 1.0);
    assert_eq!(src.over(dst), src);
}
