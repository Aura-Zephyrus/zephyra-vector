//! 玻璃装饰契约的门禁。
//!
//! # 为什么这些数字要单独钉
//!
//! 装饰的**参数是外观**，而外观在这一层只是数据 —— 真正画出来的是着色器。
//! 于是"参数改错了"在 GPU 上表现为"亮线粗了一点""投影淡了一档"，
//! 肉眼几乎看不出来，也没有任何报错。而它们全部是**一比一复刻**的目标值
//! （Kotlin 原版 `AndroidLiquidGlass`）。所以逐项钉死。

use super::{Decorations, Highlight, HighlightKind, InnerShadow, OuterShadow, DP};
use crate::core::{Color, Point};
use crate::filter::BlendMode;

// ─────────────────────────── 高光：三种变体 ───────────────────────────

/// `Highlight.Default`：白 @0.5、Plus、45°、衰减 1。
#[test]
fn the_directional_highlight_matches_the_original() {
    let h = Highlight::directional();
    assert_eq!(h.width, 0.5 * DP);
    assert_eq!(h.blur_radius, 0.5 * DP / 2.0, "原版 blurRadius = width / 2");
    assert_eq!(h.alpha, 1.0);
    assert_eq!(h.color, Color::rgba(1.0, 1.0, 1.0, 0.5));
    assert_eq!(h.blend, BlendMode::Plus);
    assert_eq!(h.kind, HighlightKind::Directional);
    assert_eq!(h.angle_deg, 45.0);
    assert_eq!(h.falloff, 1.0);
}

/// `Highlight.Ambient`：白 @0.38、**SrcOver**、强度 0.38。
///
/// 混合模式必须是 SrcOver：着色器产出"外侧黑 / 内侧白"两半，黑那半要能
/// 压暗外侧 —— Plus 只加不减，压不下去。
#[test]
fn the_ambient_highlight_matches_the_original() {
    let h = Highlight::ambient();
    assert_eq!(h.color, Color::rgba(1.0, 1.0, 1.0, 0.38));
    assert_eq!(h.blend, BlendMode::SrcOver);
    assert_eq!(h.kind, HighlightKind::Ambient);
    assert_eq!(h.ambient_intensity, 0.38, "颜色 alpha 与强度必须同一个数");
}

/// `Highlight.Plain`：白 @0.38、Plus、无着色器。
#[test]
fn the_plain_highlight_matches_the_original() {
    let h = Highlight::plain();
    assert_eq!(h.color, Color::rgba(1.0, 1.0, 1.0, 0.38));
    assert_eq!(h.blend, BlendMode::Plus);
    assert_eq!(h.kind, HighlightKind::Plain);
}

/// 描边宽度 = `ceil(min(width, min_side/2)) * 2` —— 乘 2 因为居中于边界。
#[test]
fn the_stroke_width_is_doubled_and_clamped() {
    let h = Highlight::directional();
    // 正常情况：0.5 → ceil → 1 → ×2 = 2。
    assert_eq!(h.stroke_width(100.0), 2.0);
    // 宽描边撞上薄形状：夹到 min_side/2 = 4.5 → ceil 5 → ×2 = 10。
    let wide = Highlight {
        width: 10.0,
        ..Highlight::directional()
    };
    assert_eq!(wide.stroke_width(9.0), 10.0);
    // 同一份宽描边在大形状上不被夹：10 → ×2 = 20。
    assert_eq!(wide.stroke_width(100.0), 20.0);
}

/// 强度为 0 的高光不画。
#[test]
fn a_zero_alpha_highlight_is_identity() {
    let mut h = Highlight::ambient();
    h.alpha = 0.0;
    assert!(h.is_identity());
    assert!(!Highlight::ambient().is_identity());
}

// ─────────────────────────── 投影 ───────────────────────────

/// 大玻璃的投影：`radius=24`、`offset=(0,4)`、黑 @0.1。
#[test]
fn the_glass_shadow_matches_the_original() {
    let s = OuterShadow::glass();
    assert_eq!(s.radius, 24.0 * DP);
    assert_eq!(s.offset, Point::new(0.0, 4.0 * DP), "offset.y = radius / 6");
    assert_eq!(s.color, Color::rgba(0.0, 0.0, 0.0, 0.1));
    assert!(s.hollow_inside, "玻璃的投影要挖掉形状内侧的余量");
}

/// 小控件的投影：`radius=4`、黑 @0.05。
#[test]
fn the_control_shadow_matches_the_original() {
    let s = OuterShadow::control();
    assert_eq!(s.radius, 4.0 * DP);
    assert_eq!(s.color, Color::rgba(0.0, 0.0, 0.0, 0.05));
}

/// 外扩 = `radius * 2.5 ± offset`（2.5σ 之后高斯的残量已被 alpha 压到 8 位以下）。
#[test]
fn the_shadow_outset_leaves_two_and_a_half_times_the_radius() {
    let o = OuterShadow::glass().outset();
    // radius*2.5 = 60；offset=(0,4) ⇒ 上 60-4=56、下 60+4=64；左右 60。
    assert_eq!(o.left, 60.0);
    assert_eq!(o.right, 60.0);
    assert_eq!(o.top, 56.0);
    assert_eq!(o.bottom, 64.0);
}

// ─────────────────────────── 内阴影 ───────────────────────────

/// 内阴影：`radius=24`、`offset=(0,24)`、黑 @0.15。
#[test]
fn the_inner_shadow_matches_the_original() {
    let s = InnerShadow::glass();
    assert_eq!(s.radius, 24.0 * DP);
    assert_eq!(s.offset, Point::new(0.0, 24.0 * DP), "offset.y = radius");
    assert_eq!(s.color, Color::rgba(0.0, 0.0, 0.0, 0.15));
}

// ─────────────────────────── 四件套 ───────────────────────────

/// 空装饰是恒等的，且不外扩。
#[test]
fn no_decorations_is_identity_and_needs_no_outset() {
    let d = Decorations::none();
    assert!(d.is_identity());
    assert!(d.outsets().is_zero());
}

/// 大玻璃的四件套：Directional 高光 + 24dp 投影 + 24dp 内阴影。
#[test]
fn the_glass_decorations_are_the_three_layers() {
    let d = Decorations::glass();
    assert_eq!(
        d.highlight.map(|h| h.kind),
        Some(HighlightKind::Directional)
    );
    assert_eq!(d.shadow.map(|s| s.radius), Some(24.0 * DP));
    assert_eq!(d.inner_shadow.map(|s| s.radius), Some(24.0 * DP));
    assert!(!d.is_identity());
    // 只有投影外扩，高光与内阴影都裁在形状内。
    assert_eq!(d.outsets().left, 60.0);
}

/// 小控件的四件套：Ambient 高光（宽度 /1.5）+ 4dp 投影 + 4dp 内阴影。
#[test]
fn the_control_decorations_shrink_the_highlight() {
    let d = Decorations::control();
    let h = d.highlight.expect("小控件用环境高光");
    assert_eq!(h.kind, HighlightKind::Ambient);
    assert_eq!(h.width, 0.5 * DP / 1.5);
    assert_eq!(h.blur_radius, 0.25 * DP / 1.5);
    assert_eq!(d.shadow.map(|s| s.radius), Some(4.0 * DP));
    assert_eq!(d.inner_shadow.map(|s| s.radius), Some(4.0 * DP));
}

/// 按压进度版的**原版式子**：高光 alpha = progress、内阴影半径 = 4dp×progress、
/// 内阴影 alpha = progress；投影恒定不随进度。
#[test]
fn the_control_decorations_follow_the_press_progress() {
    // 松开（progress = 0）：高光 / 内阴影全灭（恒等），投影仍在。
    let d = Decorations::control_with_progress(0.0);
    let h = d.highlight.expect("结构仍在（强度为 0）");
    assert_eq!(h.alpha, 0.0);
    assert!(d.highlight.unwrap().is_identity());
    assert_eq!(d.inner_shadow.unwrap().radius, 0.0);
    assert_eq!(d.shadow.map(|s| s.radius), Some(4.0 * DP), "投影不随进度");

    // 全按（progress = 1）：高光 alpha 1、内阴影 4dp / alpha 1。
    let d = Decorations::control_with_progress(1.0);
    assert_eq!(d.highlight.unwrap().alpha, 1.0);
    assert_eq!(d.inner_shadow.unwrap().radius, 4.0 * DP);
    assert_eq!(d.inner_shadow.unwrap().alpha, 1.0);

    // 中间帧：一切随进度缩放，且夹在 [0,1]。
    let d = Decorations::control_with_progress(0.5);
    assert_eq!(d.highlight.unwrap().alpha, 0.5);
    assert_eq!(d.inner_shadow.unwrap().radius, 2.0 * DP);
    // 越界值夹回。
    let d = Decorations::control_with_progress(7.0);
    assert_eq!(d.highlight.unwrap().alpha, 1.0);
}
