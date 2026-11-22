//! 滤镜 / 装饰外扩的字面量断言（从 ZephyraCore 的 `visual::damage_bounds` 测试移植）。
//!
//! 框架侧的 `damage_bounds` = visual rect → `filter.bounds_for(b)` →
//! `b.outset_by(decorations.outsets())`。vector 没有框架层的 `damage_bounds`，
//! 所以这里把同三条测试的字面量**逐字**钉到 vector 自己拥有的 API 上：
//! `FilterChain::outsets` / `bounds_for`、`Decorations::glass().outsets()` 与
//! `Rect::outset_by`。公式与框架逐行一致（见框架 `src/visual.rs::damage_bounds`），
//! 因此三个期望矩形都能原样复现。

use crate::core::Rect;
use crate::decoration::Decorations;
use crate::filter::{Filter, FilterChain, Outset};

/// 滤镜 outset 必须外扩脏区：σ=(2,3) × 3 ⇒ 左右 +6、上下 +9。
///
/// 变异点：漏掉 `filter.bounds_for`，或把它写成 `filter.outsets()` 的加法而漏
/// 掉左上角位移。具体字面量在 scissor 裁掉模糊边缘时会变成可见的硬切边。
#[test]
fn filter_bounds_outset_matches_framework_expectation() {
    let chain = FilterChain::single(Filter::Blur {
        sigma_x: 2.0,
        sigma_y: 3.0,
    });
    // 框架的 visual_rect 是节点级概念，vector 没有对应物：这里直接把
    // Rect::new(10,20,100,50) 当作"未外扩的视觉矩形"喂给 bounds_for。
    // σ=(2,3) × 3σ ⇒ left/right=6、top/bottom=9。
    assert_eq!(
        chain.outsets(),
        Outset {
            left: 6.0,
            top: 9.0,
            right: 6.0,
            bottom: 9.0,
        },
        "blur 的 outset 必须是 3σ，四边都要给"
    );
    // 框架侧 damage_bounds 的期望矩形：把左上角位移与尺寸一起钉住。
    assert_eq!(
        chain.bounds_for(Rect::new(10.0, 20.0, 100.0, 50.0)),
        Rect::new(4.0, 11.0, 112.0, 68.0),
        "bounds_for 必须同时外扩四边并移动左上角"
    );
}

/// 玻璃装饰的外投影必须外扩脏区：radius=24 ⇒ r=60、offset=(0,4) ⇒
/// left=60 / top=56 / right=60 / bottom=64。
#[test]
fn decoration_shadow_outset_matches_framework_expectation() {
    let outsets = Decorations::glass().outsets();
    assert_eq!(
        outsets,
        Outset {
            left: 60.0,
            top: 56.0,
            right: 60.0,
            bottom: 64.0,
        },
        "radius=24×2.5=60，再按 offset=(0,4) 分到上下边"
    );
    // 框架侧 damage_bounds 的期望矩形，用 vector 的 Rect::outset_by 复现。
    assert_eq!(
        Rect::new(10.0, 20.0, 100.0, 50.0).outset_by(outsets),
        Rect::new(-50.0, -36.0, 220.0, 170.0)
    );
}

/// 滤镜与装饰同时存在时两者**串联**外扩（先滤镜、再装饰），且 translate 平移
/// 整个结果。断言具体矩形，钉住"只算其中一个 outset"的变异。
#[test]
fn filter_and_decoration_outsets_compose_to_framework_expectation() {
    let chain = FilterChain::single(Filter::Blur {
        sigma_x: 2.0,
        sigma_y: 3.0,
    });
    // translate=(5,7) 先作用在视觉矩形上：(10,20) → (15,27)。
    let translated = Rect::new(15.0, 27.0, 100.0, 50.0);
    // 第一趟：滤镜 bounds_for ⇒ (15−6, 27−9, 100+12, 50+18)。
    let after_filter = chain.bounds_for(translated);
    assert_eq!(after_filter, Rect::new(9.0, 18.0, 112.0, 68.0));
    // 第二趟：装饰 outset ⇒ (9−60, 18−56, 112+120, 68+120)。
    assert_eq!(
        after_filter.outset_by(Decorations::glass().outsets()),
        Rect::new(-51.0, -38.0, 232.0, 188.0),
        "两次外扩缺一不可；少算滤镜或少算装饰都会露馅"
    );
    // 等价地：两份 outset 逐边相加后一次性作用于平移矩形，结果必须一致。
    let combined = Outset {
        left: 66.0,
        top: 65.0,
        right: 66.0,
        bottom: 73.0,
    };
    assert_eq!(
        translated.outset_by(combined),
        Rect::new(-51.0, -38.0, 232.0, 188.0),
        "滤镜 + 装饰的逐边和必须等于串联结果"
    );
}
