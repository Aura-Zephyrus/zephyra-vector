// 本文件由 filter.rs 的 #[cfg(test)] mod tests 抽出 —— 测试代码与生产代码分离。
// 模块路径不变（src 里的声明用 #[path] 指过来），所以 use super::* 与私有可见性都照旧。

use super::*;

/// `BlendMode` 的 29 个变体与 Skia 的 `SkBlendMode` **逐值同序**。
///
/// 这条测试的价值在于：一旦有人在本枚举中间插入变体，两侧的下标就全错位了，
/// 而错位不会报错 —— 只会让 `kMultiply` 悄悄按 `kOverlay` 渲染。
#[test]
fn blend_modes_match_skia_value_by_value() {
    // 按 Skia `SkBlendMode.h:38-74` 的声明顺序抄下来，不允许改序。
    let skia_order = [
        BlendMode::Clear,
        BlendMode::Src,
        BlendMode::Dst,
        BlendMode::SrcOver,
        BlendMode::DstOver,
        BlendMode::SrcIn,
        BlendMode::DstIn,
        BlendMode::SrcOut,
        BlendMode::DstOut,
        BlendMode::SrcATop,
        BlendMode::DstATop,
        BlendMode::Xor,
        BlendMode::Plus,
        BlendMode::Modulate,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::Darken,
        BlendMode::Lighten,
        BlendMode::ColorDodge,
        BlendMode::ColorBurn,
        BlendMode::HardLight,
        BlendMode::SoftLight,
        BlendMode::Difference,
        BlendMode::Exclusion,
        BlendMode::Multiply,
        BlendMode::Hue,
        BlendMode::Saturation,
        BlendMode::Color,
        BlendMode::Luminosity,
    ];
    assert_eq!(
        skia_order.len(),
        BlendMode::COUNT,
        "Skia 的 kSkBlendModeCount 是 29；本枚举若增删了变体，这条测试该同步"
    );
    // `#[derive(Ord)]` 给出的是声明序下的序数，逐一断言它落在下标 i 上。
    for (i, m) in skia_order.iter().enumerate() {
        assert_eq!(
            *m as usize, i,
            "{m:?} 的序数是 {} 而非 {i} —— 有人改了枚举顺序",
            *m as usize
        );
    }
    assert_eq!(skia_order[BlendMode::COUNT - 1], BlendMode::Luminosity);
}

/// 系数表逐项对齐 Skia `SkBlendMode_AsCoeff`（`SkBlendMode.cpp:68-84`）。
///
/// 这张表是 GPU 混合状态与 CPU 参考光栅器的**唯一共同基准**：两侧若不一致，
/// 同一DisplayList 在 GPU 上与 CPU 上会画出不同的颜色，而测试全绿。
#[test]
fn coefficient_table_aligns_with_skia_item_by_item() {
    use BlendMode::*;
    use Coeff::*;
    // (src coeff, dst coeff)，抄自 kCoeffs。
    let expected = [
        (Clear, Zero, Zero),
        (Src, One, Zero),
        (Dst, Zero, One),
        (SrcOver, One, OneMinusSrcAlpha),
        (DstOver, OneMinusDstAlpha, One),
        (SrcIn, DstAlpha, Zero),
        (DstIn, Zero, SrcAlpha),
        (SrcOut, OneMinusDstAlpha, Zero),
        (DstOut, Zero, OneMinusSrcAlpha),
        (SrcATop, DstAlpha, OneMinusSrcAlpha),
        (DstATop, OneMinusDstAlpha, SrcAlpha),
        (Xor, OneMinusDstAlpha, OneMinusSrcAlpha),
        (Plus, One, One),
        (Modulate, Zero, SrcColor),
        (Screen, One, OneMinusSrcColor),
    ];
    for (mode, src, dst) in expected {
        assert_eq!(mode.as_coeff(), Some((src, dst)), "{mode:?} 的系数对不对");
    }
}

/// 只有 Porter-Duff 那 15 个能用系数表达，且分类与 Skia 的两个 Last 常量一致。
#[test]
fn only_first_fifteen_expressible_with_coefficients() {
    for m in [
        BlendMode::Clear,
        BlendMode::Modulate,
        BlendMode::Screen,
        BlendMode::Multiply,
        BlendMode::Overlay,
        BlendMode::Hue,
        BlendMode::Luminosity,
    ] {
        assert_eq!(
            m.as_coeff().is_some(),
            m.is_coeff(),
            "{m:?} 的 is_coeff 与 as_coeff 自相矛盾"
        );
    }
    // 系数模式必然可分离，但反之不成立。
    for i in 0..BlendMode::COUNT {
        let m = enum_by_index(i);
        if m.is_coeff() {
            assert!(m.is_separable(), "{m:?} 是系数模式却不可分离");
        }
    }
    assert!(BlendMode::Overlay.is_separable());
    assert!(!BlendMode::Overlay.is_coeff());
    assert!(!BlendMode::Hue.is_separable());
}

/// `Overlay` 在系数表之外，且`Multiply` 是最后一个可分离模式。
#[test]
fn filter_boundaries_match_skia() {
    assert_eq!(BlendMode::LAST_COEFF_MODE, BlendMode::Screen);
    assert_eq!(BlendMode::LAST_SEPARABLE_MODE, BlendMode::Multiply);
    assert!(BlendMode::Screen.as_coeff().is_some());
    assert!(BlendMode::Overlay.as_coeff().is_none());
    assert!(BlendMode::Multiply.as_coeff().is_none());
}

/// blur 的外扩是 `3σ`，且各向异性。
#[test]
fn blur_outsets_three_sigma() {
    let o = Filter::Blur {
        sigma_x: 2.0,
        sigma_y: 5.0,
    }
    .outsets();
    assert_eq!(o.left, 6.0, "3 × 2");
    assert_eq!(o.top, 15.0, "3 × 5");
    assert_eq!(o.right, 6.0);
    assert_eq!(o.bottom, 15.0);
}

/// 非有限σ 必须退化成 0 外扩，而不是产生 NaN 矩形。
///
/// 这条是真实防御：负σ 会让 outset 为负，离屏矩形**比**原矩形还小，
/// 模糊在边界处被切掉；而 NaN 会让整个矩形坐标变 NaN，光栅化器直接丢弃
/// 一切（症状是"元素凭空消失"）。
#[test]
fn non_finite_or_non_positive_sigma_degenerates_to_zero() {
    for bad in [-1.0, 0.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let f = Filter::Blur {
            sigma_x: bad,
            sigma_y: 2.0,
        };
        assert_eq!(f.outsets().left, 0.0, "σ={bad} 应当退化成 0 半径（左）");
        assert_eq!(f.outsets().top, 6.0, "另一轴仍是 3 × 2");
        let b = FilterChain::single(f).bounds_for(Rect::new(10.0, 10.0, 100.0, 50.0));
        assert!(b.x.is_finite() && b.width.is_finite(), "矩形不能是 NaN");
        assert!(
            b.width >= 100.0,
            "外扩不能为负（σ={bad} 得到 width={}）",
            b.width
        );
    }
}

/// 投影的不对称外扩：右下投影只扩右下。
#[test]
fn shadow_outset_is_direction_asymmetric() {
    let f = Filter::DropShadow {
        color: Color::rgba(0.0, 0.0, 0.0, 0.4),
        offset: Point::new(4.0, 6.0),
        sigma: 2.0,
    };
    let o = f.outsets();
    assert_eq!(
        (o.left, o.top, o.right, o.bottom),
        (2.0, 0.0, 10.0, 12.0),
        "右下投影 (4,6) + 半径 6：右侧 6+4=10、下侧 6+6=12，\
             而左侧被 offset 抵消成 6-4=2、上侧恰好抵消成 0"
    );
    let b = FilterChain::single(f).bounds_for(Rect::new(0.0, 0.0, 100.0, 50.0));
    assert_eq!((b.x, b.y, b.width, b.height), (-2.0, 0.0, 112.0, 62.0));
}

/// 向左上投影：左上扩得更多，右下只留 blur 半径。
#[test]
fn top_left_shadow_outsets_towards_top_left() {
    let f = Filter::DropShadow {
        color: Color::rgba(0.0, 0.0, 0.0, 0.4),
        offset: Point::new(-10.0, -4.0),
        sigma: 2.0,
    };
    let o = f.outsets();
    assert_eq!(
        (o.left, o.top, o.right, o.bottom),
        (16.0, 10.0, 0.0, 2.0),
        "左上投影 (-10,-4) + 半径 6：左侧 6+10=16、上侧 6+4=10，\
             右侧被 offset 抵消成 0、下侧剩 6-4=2"
    );
    let b = FilterChain::single(f).bounds_for(Rect::new(0.0, 0.0, 100.0, 50.0));
    assert_eq!((b.x, b.y, b.width, b.height), (-16.0, -10.0, 116.0, 62.0));
}

/// 串联滤镜取逐边最大值，**不是**求和。
///
/// 求和会让 blur+blur 的离屏矩形大一倍 —— 而实际上只有最后一级的输出
/// 边缘在起作用。
#[test]
fn chained_filters_take_elementwise_max_not_sum() {
    let chain = FilterChain::new([
        Filter::Blur {
            sigma_x: 2.0,
            sigma_y: 2.0,
        },
        Filter::Blur {
            sigma_x: 4.0,
            sigma_y: 1.0,
        },
    ]);
    let o = chain.outsets();
    assert_eq!((o.left, o.right), (12.0, 12.0), "取 max(6, 12)");
    assert_eq!((o.top, o.bottom), (6.0, 6.0), "取 max(6, 3)");
    assert_ne!(
        chain.outsets().left,
        6.0 + 12.0,
        "求和会给 18；这条断言钉住我们用的是 max"
    );
}

/// 空链与恒等滤镜都判为恒等，宿主据此跳过开离屏。
#[test]
fn identity_chain_does_not_allocate_offscreen() {
    assert!(FilterChain::none().is_identity());
    assert!(FilterChain::default().is_identity());
    assert!(
        FilterChain::single(Filter::Blur {
            sigma_x: 0.0,
            sigma_y: 0.0
        })
        .is_identity(),
        "σ=0 的 blur 不改变任何像素"
    );
    assert!(!FilterChain::single(Filter::Blur {
        sigma_x: 0.5,
        sigma_y: 0.0
    })
    .is_identity());
    assert!(!FilterChain::single(Filter::DropShadow {
        color: Color::rgba(0.0, 0.0, 0.0, 0.0),
        offset: Point::new(0.0, 0.0),
        sigma: 0.0,
    })
    .is_identity());
}

/// 折射的位移**不得超过玻璃厚度**。
///
/// # 这条钉住的是什么
///
/// `PSRefraction` 里的位移式是 `δ = h · thickness · sin(Δ) / cos(Θr)`。
/// `h ∈ [0,1]`、`sin(Δ)/cos(Θr) ≤ 1`，于是 `δ ≤ thickness` 恒成立。
///
/// 早先那版用的是薄棱镜近似 `δ = h · (tan Θs - tan Θr)` —— 它只在**小角度**
/// 成立，而高度剖面在 `t → 0` 处斜率发散（`Θs → 90°`），于是 `tan Θs` 发散：
/// **实测**默认参数（thickness=28 / bezel=26 / ior=1.50）下位移峰值是 **491px**
/// （最大参数下 2181px），而契约的标称值是 28px。
///
/// 后果不是「看起来怪」，是**玻璃边缘把屏幕上几百像素外的内容拉进来**，
/// 且离屏外扩按 28px 算 ⇒ 采样落在副本之外 ⇒ 被 `saturate` 夹到屏幕边上。
///
/// 这条门禁在 core 侧用**纯 Rust 镜像**验（`filter::refraction`）——
/// 它不参与渲染，只用来钉住契约。着色器那边改了式子，这里必须同步改。
#[test]
fn refraction_displacement_is_bounded_by_the_thickness() {
    // 默认档（gallery 的 `Glass::default()`：thickness 28 / bezel 26 / ior 1.50）
    // 与参数面板的四个角（最小 / 最大）。
    let cases = [
        (28.0_f32, 26.0_f32, 1.50_f32, "默认档"),
        (4.0, 2.0, 1.05, "最小档"),
        (80.0, 48.0, 2.00, "最大档"),
        (28.0, 12.0, 1.50, "窄倒角带"),
    ];
    for (thickness, bezel, ior, what) in cases {
        let d = refraction::max_displacement(thickness, bezel, ior);
        assert!(
            d <= thickness,
            "{what}：位移峰值 {d:.2}px 超过玻璃厚度 {thickness}px —— \
             契约（位移 ≤ 厚度）破了，离屏外扩与边缘采样都按错了尺寸算"
        );
        // 同时给一个**下界**：恒等于 0 说明公式退化成「什么都不做」，
        // 那样这条门禁会恒真地绿。
        assert!(
            d > 0.0,
            "{what}：位移峰值是 0 —— 公式退化了（IOR=1 那一档才允许为 0）"
        );
    }
}

/// `ior = 1` 必须是**零位移** —— 那是「不折射」的诊断位。
///
/// 门禁 `refraction_resamples_the_backdrop_on_d3d12` 要靠这条性质做反向断言：
/// `IOR = 1` 时画面必须与「没有玻璃」逐像素相同。若这条破了，那个门禁
/// 的容差就不再有意义。
///
/// # 判据是**亚像素**，不是「恰好 0」
///
/// 着色器里 `theta_r = asin(sin(slope) / ior)`，而 `ior = 1` 时它要走一趟
/// `sin → asin` 的往返；f32 的舍入误差（约 1e-7）在 `1/cos(Θr)` 上被放大
/// （`Θr → 90°` 时 `cos` 趋零），最后落在**千分之一像素**量级 —— 实测
/// 0.00086px（thickness=28）。
///
/// 那是**渲染上不可见**的量（比一个像素小三个数量级），而 D3D12 那边
/// 「IOR=1 ⇒ 与没有玻璃几乎逐像素相同」的门禁容差是 0.5% —— 三个数量级
/// 的余量，判据稳。
#[test]
fn refraction_with_ior_one_has_no_displacement() {
    // 0.01px = 百分之一像素。实测最大 0.0025px（thickness=80），
    // 留 4 倍余量。
    const SUB_PIXEL: f32 = 0.01;
    for (thickness, bezel) in [(28.0_f32, 26.0_f32), (80.0, 48.0), (4.0, 2.0)] {
        let d = refraction::max_displacement(thickness, bezel, 1.0);
        assert!(
            d < SUB_PIXEL,
            "IOR=1（不折射）时位移应当是亚像素的（< {SUB_PIXEL}px），\
             实测 {d:.6}px（thickness={thickness}）"
        );
    }
}

/// 厚度为 0 时整条折射链是**恒等**。
///
/// # 为什么这条重要
///
/// 着色器里色散是**乘**在位移上的（`uvR = uv + off_uv * (1 + dispersion)`），
/// 而 `off_uv` 带 `thickness` 因子 —— 所以 `thickness = 0` ⇒ 采样点不动
/// ⇒ 三条通道采同一点 ⇒ **色散也乘的是 0** ⇒ 整条链恒等。
///
/// 早先的实现把 `dispersion` 也算进判据，于是「厚度 0 + 色散 > 0」被判成
/// **非恒等**：照样开离屏 pass、照样拷一份**全屏**目标副本，而画面一个字
/// 都不变 —— 纯浪费，且 `RenderReport::offscreen_passes` 多记一次。
#[test]
fn refraction_without_thickness_is_the_identity() {
    let f = Filter::Refraction {
        strength: 0.0,
        bezel: 26.0,
        dispersion: 0.0,
        ior: 1.5,
    };
    assert!(f.is_identity(), "厚度 0 ⇒ 恒等（色散乘的是 0）");

    // 色散不为 0 也一样 —— 它乘的是位移，而位移是 0。
    let f = Filter::Refraction {
        strength: 0.0,
        bezel: 26.0,
        dispersion: 0.8,
        ior: 1.5,
    };
    assert!(
        f.is_identity(),
        "厚度 0 ⇒ 恒等，色散不改变这一点（色散乘的是位移）"
    );

    // 而厚度非 0 时，即便 IOR = 1（不折射）也**不是**恒等 ——
    // 那一档是诊断位：pass 必须真的跑起来，门禁才能验「画面逐像素不变」。
    let f = Filter::Refraction {
        strength: 28.0,
        bezel: 26.0,
        dispersion: 0.0,
        ior: 1.0,
    };
    assert!(
        !f.is_identity(),
        "IOR=1 但厚度非 0：pass 仍要跑（诊断位），不能判成恒等"
    );
}

/// 背景模糊（磨砂玻璃）**不外扩**离屏矩形，σ ≤ 0 时恒等。
///
/// # 为什么不外扩
///
/// 与折射同一个理由：`BackdropBlur` 合成趟采的是 `g_dst`（目标的**全屏**副本），
/// 不是离屏 RT 里元素自己的内容。外扩会让"背景被模糊的区域"扩到元素矩形之外，
/// 而那圈里根本没有元素的背景可糊 —— 纯浪费。
#[test]
fn backdrop_blur_has_no_outset_and_zero_sigma_is_identity() {
    let f = Filter::BackdropBlur { sigma: 6.0 };
    assert_eq!(f.outsets(), Outset::default(), "背景模糊不外扩（与折射同）");
    assert!(!f.is_identity(), "σ>0 非恒等");

    let f = Filter::BackdropBlur { sigma: 0.0 };
    assert!(f.is_identity(), "σ=0 恒等（与 Blur 同判据）");
}

/// 材质展开成滤镜链：纯色→空、磨砂→BackdropBlur、玻璃→PixelOffset、
/// 液态玻璃→Refraction。
#[test]
fn material_expands_to_the_right_filter() {
    use crate::filter::Material;

    assert!(
        Material::solid().filter().is_identity(),
        "纯色不产生任何滤镜（恒等空链）"
    );

    let frosted = Material::frosted().filter();
    assert_eq!(frosted.filters().len(), 1);
    assert!(
        matches!(frosted.filters()[0], Filter::BackdropBlur { .. }),
        "磨砂展开成 BackdropBlur"
    );

    // 玻璃（默认）= SVG 式像素偏移：**不采样背景** ⇒ 不开离屏 ⇒ 几乎不花钱。
    let glass = Material::glass().filter();
    assert_eq!(glass.filters().len(), 1);
    assert!(
        matches!(glass.filters()[0], Filter::PixelOffset { .. }),
        "玻璃展开成 PixelOffset"
    );
    assert!(
        glass.pixel_offset().is_some(),
        "像素偏移玻璃必须能被认出来（展开那层靠它决定画法）"
    );

    // 真实液态玻璃：保留，但贵。
    let refract = Material::glass_refract().filter();
    assert_eq!(refract.filters().len(), 1);
    assert!(
        matches!(refract.filters()[0], Filter::Refraction { .. }),
        "液态玻璃展开成 Refraction"
    );
    assert!(
        refract.pixel_offset().is_none(),
        "折射**不是**像素偏移（它要离屏、要目标副本）"
    );
}

/// 像素偏移玻璃：链**恰好**只有它时才能走解析画法。
///
/// 判据是「链长 1」—— 混了别的滤镜时那些要离屏，整条链都得走离屏。
#[test]
fn only_a_lone_pixel_offset_counts_as_the_analytic_glass() {
    use crate::filter::{Filter, FilterChain, Material};

    assert!(Material::glass().filter().pixel_offset().is_some());

    // 混一个投影 ⇒ 不是解析玻璃。
    let mixed = FilterChain::single(Filter::PixelOffset {
        amount: 6.0,
        bezel: 12.0,
    })
    .then(Filter::DropShadow {
        color: crate::core::Color::rgba(0.0, 0.0, 0.0, 0.5),
        offset: crate::core::Point::new(0.0, 2.0),
        sigma: 4.0,
    });
    assert!(
        mixed.pixel_offset().is_none(),
        "链里混了别的滤镜就不能走解析画法（那些要离屏）"
    );

    // 空链 / 别的滤镜 ⇒ 都不是。
    assert!(FilterChain::none().pixel_offset().is_none());
    assert!(FilterChain::single(Filter::Blur {
        sigma_x: 2.0,
        sigma_y: 2.0
    })
    .pixel_offset()
    .is_none());
}

/// `material-params` 独立于 `material`：同一材质可换参数，未写的项用默认。
#[test]
fn material_params_override_independently() {
    use crate::filter::{Material, MaterialParams};

    // 磨砂：只给 amount（σ）。
    let p = MaterialParams::from_numbers(&[12.0]);
    match Material::frosted().filter_with(&p).filters()[0] {
        Filter::BackdropBlur { sigma } => assert_eq!(sigma, 12.0),
        other => panic!("磨砂应是 BackdropBlur，得到 {other:?}"),
    }

    // 液态玻璃：四个数全给。
    let p = MaterialParams::from_numbers(&[24.0, 20.0, 0.3, 1.5]);
    match Material::glass_refract().filter_with(&p).filters()[0] {
        Filter::Refraction {
            strength,
            bezel,
            dispersion,
            ior,
        } => {
            assert_eq!((strength, bezel, dispersion, ior), (24.0, 20.0, 0.3, 1.5));
        }
        other => panic!("液态玻璃应是 Refraction，得到 {other:?}"),
    }

    // 只给一部分：其余落默认（bezel 默认 16、dispersion 默认 0、ior 默认 1.5）。
    let p = MaterialParams::from_numbers(&[30.0]);
    match Material::glass_refract().filter_with(&p).filters()[0] {
        Filter::Refraction {
            strength,
            bezel,
            dispersion,
            ior,
        } => {
            assert_eq!(strength, 30.0, "只给了 amount");
            assert_eq!(bezel, Material::GLASS_BEZEL, "bezel 落默认");
            assert_eq!(dispersion, Material::GLASS_DISPERSION);
            assert_eq!(ior, Material::GLASS_IOR);
        }
        other => panic!("应是 Refraction，得到 {other:?}"),
    }

    // 玻璃（像素偏移）：`amount` = 位移幅度、`bezel` = 作用带宽度。
    let p = MaterialParams::from_numbers(&[8.0, 16.0]);
    match Material::glass().filter_with(&p).filters()[0] {
        Filter::PixelOffset { amount, bezel } => {
            assert_eq!((amount, bezel), (8.0, 16.0));
        }
        other => panic!("玻璃应是 PixelOffset，得到 {other:?}"),
    }

    // 只给 `amount`：`bezel` 落默认。
    let p = MaterialParams::from_numbers(&[4.0]);
    match Material::glass().filter_with(&p).filters()[0] {
        Filter::PixelOffset { amount, bezel } => {
            assert_eq!(amount, 4.0, "只给了 amount");
            assert_eq!(bezel, Material::GLASS_OFFSET_BEZEL, "bezel 落默认");
        }
        other => panic!("应是 PixelOffset，得到 {other:?}"),
    }

    // 纯色无视参数。
    assert!(Material::solid().filter_with(&p).is_identity());
}

/// 折射**不外扩**离屏矩形。
///
/// # 为什么
///
/// 折射合成趟采的是 `g_dst` —— 目标的**全屏**副本；离屏 RT 里装的是
/// 元素自己的内容，而 `PSRefraction` **不采它**（元素的内容是之后由
/// `redraw_original` 那趟画回屏幕的）。所以「元素外那一圈」根本不在
/// 离屏 RT 里，外扩它不解决任何问题。
///
/// 而且外扩有**副作用**：拿不到目标副本时降级成普通合成，那一趟画的是
/// `pass.bounds` —— 外扩之后会把元素自己的内容多画一圈到元素外面。
#[test]
fn refraction_does_not_grow_the_offscreen_rect() {
    let f = Filter::Refraction {
        strength: 28.0,
        bezel: 26.0,
        dispersion: 0.2,
        ior: 1.5,
    };
    let o = f.outsets();
    assert!(
        o.is_zero(),
        "折射不外扩：离屏 RT 应当正好等于元素矩形，实测 {o:?}"
    );
    let content = Rect::new(100.0, 200.0, 220.0, 120.0);
    assert_eq!(
        FilterChain::single(f).bounds_for(content),
        content,
        "离屏矩形应当与元素矩形逐项相同"
    );
}

/// 链的顺序被保留：`then` 追加到末尾。
#[test]
fn chain_order_is_execution_order() {
    let a = Filter::Blur {
        sigma_x: 1.0,
        sigma_y: 1.0,
    };
    let b = Filter::DropShadow {
        color: Color::rgba(0.0, 0.0, 0.0, 0.5),
        offset: Point::new(1.0, 1.0),
        sigma: 2.0,
    };
    let chain = FilterChain::single(a).then(b);
    assert_eq!(chain.filters(), &[a, b], "先执行的在前");
    assert_eq!(
        FilterChain::new([b, a]).filters(),
        &[b, a],
        "构造顺序即顺序"
    );
}

/// 离屏原点：几何坐标仍是屏幕坐标，宿主需要知道往哪儿挪。
#[test]
fn offscreen_origin_is_content_origin_minus_outset() {
    let f = Filter::Blur {
        sigma_x: 2.0,
        sigma_y: 2.0,
    };
    let chain = FilterChain::single(f);
    let content = Rect::new(100.0, 50.0, 20.0, 20.0);
    let bounds = chain.bounds_for(content);
    let origin = chain.origin_in(content);
    assert_eq!((origin.x, origin.y), (94.0, 44.0));
    // 关键性质：内容左上角在离屏纹理内的局部坐标恒为外扩量。
    assert_eq!(content.x - bounds.x, 6.0);
    assert_eq!(content.y - bounds.y, 6.0);
}

/// 恒等链的离屏矩形就是内容本身 —— 尺寸决策不引入任何额外开销。
#[test]
fn identity_chain_leaves_rect_unchanged() {
    let r = Rect::new(3.0, 7.0, 40.0, 24.0);
    assert_eq!(FilterChain::none().bounds_for(r), r);
    assert!(FilterChain::none().outsets().is_zero());
}

/// `Clear` 不需要目标像素，其余模式都需要 —— 这决定"先清屏还是先画背景"。
#[test]
fn read_destination_mode_decision() {
    assert!(!BlendMode::Clear.needs_destination());
    assert!(!BlendMode::Src.needs_destination());
    assert!(BlendMode::SrcOver.needs_destination());
    assert!(BlendMode::Multiply.needs_destination());
}

/// `Default` 是 `SrcOver`，与现有渲染器的固定混合一致（零回归的前提）。
#[test]
fn default_blend_mode_is_src_over() {
    assert_eq!(BlendMode::default(), BlendMode::SrcOver);
}

/// 按声明序取变体（供分类测试用）。
fn enum_by_index(i: usize) -> BlendMode {
    // 与 `blend_modes_match_skia_value_by_value` 的数组同序。
    const ALL: [BlendMode; BlendMode::COUNT] = [
        BlendMode::Clear,
        BlendMode::Src,
        BlendMode::Dst,
        BlendMode::SrcOver,
        BlendMode::DstOver,
        BlendMode::SrcIn,
        BlendMode::DstIn,
        BlendMode::SrcOut,
        BlendMode::DstOut,
        BlendMode::SrcATop,
        BlendMode::DstATop,
        BlendMode::Xor,
        BlendMode::Plus,
        BlendMode::Modulate,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::Darken,
        BlendMode::Lighten,
        BlendMode::ColorDodge,
        BlendMode::ColorBurn,
        BlendMode::HardLight,
        BlendMode::SoftLight,
        BlendMode::Difference,
        BlendMode::Exclusion,
        BlendMode::Multiply,
        BlendMode::Hue,
        BlendMode::Saturation,
        BlendMode::Color,
        BlendMode::Luminosity,
    ];
    ALL[i]
}
