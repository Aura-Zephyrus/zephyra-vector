//! 复合数据类型的 `Debug` 输出。
//!
//! 滤镜报项数、装饰报哪几件在场、矩形与 uv 矩形报字段 —— 这些字符串常被
//! 诊断输出与快照直接消费，所以值得用断言钉住。
//!
//! 注意：vector 里的这些类型直接 `#[derive(Debug)]`（没有 core 里那层
//! `PropertyValue` 包装与自定义 `Debug`），所以期望字符串是派生格式：
//! `Type { field: value, ... }`。同名断言的语义不变，只是串的写法随实现走。

use crate::core::{CornerShape, Rect, UvRect};
use crate::decoration::Decorations;
use crate::filter::{Filter, FilterChain, MaterialParams};

/// 复合变体的 `Debug` 输出：滤镜报项数、装饰报哪几件在场、矩形取整、参数全 None。
#[test]
fn debug_output_pins_composite_variants() {
    // 空链：派生 Debug 直接把内部 Vec 打出来，`[]` 就是"0 项"。
    assert_eq!(
        format!("{:?}", FilterChain::none()),
        "FilterChain { filters: [] }"
    );
    // 非空链：逐项列出，顺序即执行顺序，顺带钉住"报项数"这件事。
    assert_eq!(
        format!("{:?}", FilterChain::single(Filter::Blur {
            sigma_x: 1.0,
            sigma_y: 2.0
        })),
        "FilterChain { filters: [Blur { sigma_x: 1.0, sigma_y: 2.0 }] }"
    );
    assert_eq!(
        format!("{:?}", MaterialParams::none()),
        "MaterialParams { amount: None, bezel: None, dispersion: None, ior: None }"
    );
    assert_eq!(
        format!("{:?}", Decorations::none()),
        "Decorations { highlight: None, shadow: None, inner_shadow: None }"
    );
    // glass() 的三个 Option 都是 Some；派生 Debug 会把嵌套结构整串打出来，
    // 所以只钉"哪几件在场"这个契约，不把各字段的具体数值固化进断言。
    let glass = format!("{:?}", Decorations::glass());
    assert!(
        glass.contains("highlight: Some("),
        "glass() 应带高光，实得 {glass}"
    );
    assert!(
        glass.contains(", shadow: Some("),
        "glass() 应带外投影，实得 {glass}"
    );
    assert!(
        glass.contains("inner_shadow: Some("),
        "glass() 应带内阴影，实得 {glass}"
    );
    assert_eq!(
        format!("{:?}", UvRect::new(0.0, 0.0, 1.0, 1.0)),
        "UvRect { x: 0.0, y: 0.0, width: 1.0, height: 1.0 }"
    );
    assert_eq!(
        format!("{:?}", CornerShape::ROUND),
        "CornerShape { k: 1.0 }"
    );
    assert_eq!(
        format!("{:?}", Rect::new(1.0, 2.0, 30.0, 40.0)),
        "Rect { x: 1.0, y: 2.0, width: 30.0, height: 40.0 }"
    );
}
