//! 渲染契约资产：顶点布局的对偶端（着色器源与预编译字节码）。
//!
//! 本 crate 的几何/文本层只产出 `Vertex` 流；真正上屏的 GPU 管线由宿主实现。
//! 但顶点的字段语义、`kind` 阈值以及字形 SDF 的采样公式必须与着色器严格一致 ——
//! 所以把这些**契约资产**放在这里做单一真相源，宿主通过本模块消费。
//!
//! HLSL 按功能拆成 `shaders/hlsl/*.hlsli`，`zephyra.hlsl` 只做 `#include` 聚合；
//! WGSL 没有 include，按 `shaders/wgsl/*.wgsl.part` 文件名字典序拼接。

/// 聚合入口（`fxc /E <Entry>` 指向它）。
pub const HLSL_ENTRY: &str = include_str!("../shaders/hlsl/zephyra.hlsl");

/// WGSL 各分段，按声明顺序（必须保持：WGSL 要求函数先声明后使用）。
const WGSL_PARTS: [&str; 10] = [
    include_str!("../shaders/wgsl/00_common.wgsl.part"),
    include_str!("../shaders/wgsl/01_sdf.wgsl.part"),
    include_str!("../shaders/wgsl/10_shade.wgsl.part"),
    include_str!("../shaders/wgsl/20_blend.wgsl.part"),
    // NV12 变体入口必须排在 10_shade / 20_blend 之后（WGSL 要求先声明后使用），
    // 且排在 30_blur 之前 —— 与混合入口的顺序契约一致。
    include_str!("../shaders/wgsl/21_nv12.wgsl.part"),
    include_str!("../shaders/wgsl/30_blur.wgsl.part"),
    include_str!("../shaders/wgsl/40_refraction.wgsl.part"),
    include_str!("../shaders/wgsl/41_lens.wgsl.part"),
    include_str!("../shaders/wgsl/42_backdrop.wgsl.part"),
    include_str!("../shaders/wgsl/43_decoration.wgsl.part"),
];

/// 拼接后的完整 WGSL 源。供宿主 build script 交给 naga 编译成 SPIR-V。
pub fn wgsl_source() -> String {
    let mut out = String::new();
    for part in WGSL_PARTS {
        out.push_str(part);
        out.push('\n');
    }
    out
}

/// 顶点着色器字节码（fxc `/T vs_5_0 /E VSMain`）。
pub const VS_BYTECODE: &[u8] = include_bytes!("../shaders/dxbc/zephyra.vs.bin");

/// 像素着色器变体表。哪个混合模式用哪个变体由宿主决定（后端自己的决策）。
pub const PS_VARIANTS: [(&str, &[u8]); 24] = [
    (
        "PSBackdropBlur",
        include_bytes!("../shaders/dxbc/zephyra.PSBackdropBlur.ps.bin"),
    ),
    (
        "PSBlurH",
        include_bytes!("../shaders/dxbc/zephyra.PSBlurH.ps.bin"),
    ),
    (
        "PSBlurV",
        include_bytes!("../shaders/dxbc/zephyra.PSBlurV.ps.bin"),
    ),
    (
        "PSColor",
        include_bytes!("../shaders/dxbc/zephyra.PSColor.ps.bin"),
    ),
    (
        "PSColorBurn",
        include_bytes!("../shaders/dxbc/zephyra.PSColorBurn.ps.bin"),
    ),
    (
        "PSColorDodge",
        include_bytes!("../shaders/dxbc/zephyra.PSColorDodge.ps.bin"),
    ),
    (
        "PSComposite",
        include_bytes!("../shaders/dxbc/zephyra.PSComposite.ps.bin"),
    ),
    (
        "PSDarken",
        include_bytes!("../shaders/dxbc/zephyra.PSDarken.ps.bin"),
    ),
    (
        "PSDifference",
        include_bytes!("../shaders/dxbc/zephyra.PSDifference.ps.bin"),
    ),
    (
        "PSExclusion",
        include_bytes!("../shaders/dxbc/zephyra.PSExclusion.ps.bin"),
    ),
    (
        "PSHardLight",
        include_bytes!("../shaders/dxbc/zephyra.PSHardLight.ps.bin"),
    ),
    (
        "PSHighlight",
        include_bytes!("../shaders/dxbc/zephyra.PSHighlight.ps.bin"),
    ),
    (
        "PSHollowShadow",
        include_bytes!("../shaders/dxbc/zephyra.PSHollowShadow.ps.bin"),
    ),
    (
        "PSHue",
        include_bytes!("../shaders/dxbc/zephyra.PSHue.ps.bin"),
    ),
    (
        "PSInnerShadow",
        include_bytes!("../shaders/dxbc/zephyra.PSInnerShadow.ps.bin"),
    ),
    (
        "PSLens",
        include_bytes!("../shaders/dxbc/zephyra.PSLens.ps.bin"),
    ),
    (
        "PSLighten",
        include_bytes!("../shaders/dxbc/zephyra.PSLighten.ps.bin"),
    ),
    (
        "PSLuminosity",
        include_bytes!("../shaders/dxbc/zephyra.PSLuminosity.ps.bin"),
    ),
    (
        "PSMain",
        include_bytes!("../shaders/dxbc/zephyra.PSMain.ps.bin"),
    ),
    (
        "PSMultiply",
        include_bytes!("../shaders/dxbc/zephyra.PSMultiply.ps.bin"),
    ),
    (
        "PSOverlay",
        include_bytes!("../shaders/dxbc/zephyra.PSOverlay.ps.bin"),
    ),
    (
        "PSRefraction",
        include_bytes!("../shaders/dxbc/zephyra.PSRefraction.ps.bin"),
    ),
    (
        "PSSaturation",
        include_bytes!("../shaders/dxbc/zephyra.PSSaturation.ps.bin"),
    ),
    (
        "PSSoftLight",
        include_bytes!("../shaders/dxbc/zephyra.PSSoftLight.ps.bin"),
    ),
];

/// 具名字节码常量。宿主端 `PS_VARIANTS` 的顺序绑定 `BlendMode` 枚举，不是字典序，
/// 所以不直接复用上面的字典序表，而是按月台自己的顺序从这里逐项取。
#[allow(non_upper_case_globals)] // 常量名与 HLSL 入口名一致，便于核对。
pub mod ps {
    pub const PSMain: &[u8] = include_bytes!("../shaders/dxbc/zephyra.PSMain.ps.bin");
    pub const PSOverlay: &[u8] = include_bytes!("../shaders/dxbc/zephyra.PSOverlay.ps.bin");
    pub const PSDarken: &[u8] = include_bytes!("../shaders/dxbc/zephyra.PSDarken.ps.bin");
    pub const PSLighten: &[u8] = include_bytes!("../shaders/dxbc/zephyra.PSLighten.ps.bin");
    pub const PSColorDodge: &[u8] = include_bytes!("../shaders/dxbc/zephyra.PSColorDodge.ps.bin");
    pub const PSColorBurn: &[u8] = include_bytes!("../shaders/dxbc/zephyra.PSColorBurn.ps.bin");
    pub const PSHardLight: &[u8] = include_bytes!("../shaders/dxbc/zephyra.PSHardLight.ps.bin");
    pub const PSSoftLight: &[u8] = include_bytes!("../shaders/dxbc/zephyra.PSSoftLight.ps.bin");
    pub const PSDifference: &[u8] = include_bytes!("../shaders/dxbc/zephyra.PSDifference.ps.bin");
    pub const PSExclusion: &[u8] = include_bytes!("../shaders/dxbc/zephyra.PSExclusion.ps.bin");
    pub const PSMultiply: &[u8] = include_bytes!("../shaders/dxbc/zephyra.PSMultiply.ps.bin");
    pub const PSHue: &[u8] = include_bytes!("../shaders/dxbc/zephyra.PSHue.ps.bin");
    pub const PSSaturation: &[u8] = include_bytes!("../shaders/dxbc/zephyra.PSSaturation.ps.bin");
    pub const PSColor: &[u8] = include_bytes!("../shaders/dxbc/zephyra.PSColor.ps.bin");
    pub const PSLuminosity: &[u8] = include_bytes!("../shaders/dxbc/zephyra.PSLuminosity.ps.bin");
    pub const PSBlurH: &[u8] = include_bytes!("../shaders/dxbc/zephyra.PSBlurH.ps.bin");
    pub const PSBlurV: &[u8] = include_bytes!("../shaders/dxbc/zephyra.PSBlurV.ps.bin");
    pub const PSComposite: &[u8] = include_bytes!("../shaders/dxbc/zephyra.PSComposite.ps.bin");
    pub const PSRefraction: &[u8] = include_bytes!("../shaders/dxbc/zephyra.PSRefraction.ps.bin");
    pub const PSBackdropBlur: &[u8] =
        include_bytes!("../shaders/dxbc/zephyra.PSBackdropBlur.ps.bin");
    pub const PSHollowShadow: &[u8] =
        include_bytes!("../shaders/dxbc/zephyra.PSHollowShadow.ps.bin");
    pub const PSHighlight: &[u8] = include_bytes!("../shaders/dxbc/zephyra.PSHighlight.ps.bin");
    pub const PSInnerShadow: &[u8] = include_bytes!("../shaders/dxbc/zephyra.PSInnerShadow.ps.bin");
    pub const PSLens: &[u8] = include_bytes!("../shaders/dxbc/zephyra.PSLens.ps.bin");
}

/// NV12 纹理的像素着色器变体表（15 个：`PSMainNv12` + 14 个混合模式）。
///
/// 与 [`PS_VARIANTS`] 的区别只在图像分支多一次 YUV→RGB（BT.601 limited range），
/// 矩形 / 字形 / 多边形走的是同一份 `ShadeSource` 代码，行为逐值不变。
/// 这些字节码由 `shaders/hlsl/zephyra_nv12.hlsl` 打开 `ZEPHYRA_NV12` 宏后用 fxc
/// 编译生成；默认 `zephyra.hlsl` 编出的那 25 个入口**一字未改**。
pub const PS_NV12_VARIANTS: [(&str, &[u8]); 15] = [
    (
        "PSMainNv12",
        include_bytes!("../shaders/dxbc/zephyra.PSMainNv12.ps.bin"),
    ),
    (
        "PSOverlayNv12",
        include_bytes!("../shaders/dxbc/zephyra.PSOverlayNv12.ps.bin"),
    ),
    (
        "PSDarkenNv12",
        include_bytes!("../shaders/dxbc/zephyra.PSDarkenNv12.ps.bin"),
    ),
    (
        "PSLightenNv12",
        include_bytes!("../shaders/dxbc/zephyra.PSLightenNv12.ps.bin"),
    ),
    (
        "PSColorDodgeNv12",
        include_bytes!("../shaders/dxbc/zephyra.PSColorDodgeNv12.ps.bin"),
    ),
    (
        "PSColorBurnNv12",
        include_bytes!("../shaders/dxbc/zephyra.PSColorBurnNv12.ps.bin"),
    ),
    (
        "PSHardLightNv12",
        include_bytes!("../shaders/dxbc/zephyra.PSHardLightNv12.ps.bin"),
    ),
    (
        "PSSoftLightNv12",
        include_bytes!("../shaders/dxbc/zephyra.PSSoftLightNv12.ps.bin"),
    ),
    (
        "PSDifferenceNv12",
        include_bytes!("../shaders/dxbc/zephyra.PSDifferenceNv12.ps.bin"),
    ),
    (
        "PSExclusionNv12",
        include_bytes!("../shaders/dxbc/zephyra.PSExclusionNv12.ps.bin"),
    ),
    (
        "PSMultiplyNv12",
        include_bytes!("../shaders/dxbc/zephyra.PSMultiplyNv12.ps.bin"),
    ),
    (
        "PSHueNv12",
        include_bytes!("../shaders/dxbc/zephyra.PSHueNv12.ps.bin"),
    ),
    (
        "PSSaturationNv12",
        include_bytes!("../shaders/dxbc/zephyra.PSSaturationNv12.ps.bin"),
    ),
    (
        "PSColorNv12",
        include_bytes!("../shaders/dxbc/zephyra.PSColorNv12.ps.bin"),
    ),
    (
        "PSLuminosityNv12",
        include_bytes!("../shaders/dxbc/zephyra.PSLuminosityNv12.ps.bin"),
    ),
];

/// 具名 NV12 变体常量，与 [`ps`] 一一对应（只含主路径 + 14 个混合）。
#[allow(non_upper_case_globals)]
pub mod ps_nv12 {
    pub const PSMainNv12: &[u8] = include_bytes!("../shaders/dxbc/zephyra.PSMainNv12.ps.bin");
    pub const PSOverlayNv12: &[u8] = include_bytes!("../shaders/dxbc/zephyra.PSOverlayNv12.ps.bin");
    pub const PSDarkenNv12: &[u8] = include_bytes!("../shaders/dxbc/zephyra.PSDarkenNv12.ps.bin");
    pub const PSLightenNv12: &[u8] = include_bytes!("../shaders/dxbc/zephyra.PSLightenNv12.ps.bin");
    pub const PSColorDodgeNv12: &[u8] =
        include_bytes!("../shaders/dxbc/zephyra.PSColorDodgeNv12.ps.bin");
    pub const PSColorBurnNv12: &[u8] =
        include_bytes!("../shaders/dxbc/zephyra.PSColorBurnNv12.ps.bin");
    pub const PSHardLightNv12: &[u8] =
        include_bytes!("../shaders/dxbc/zephyra.PSHardLightNv12.ps.bin");
    pub const PSSoftLightNv12: &[u8] =
        include_bytes!("../shaders/dxbc/zephyra.PSSoftLightNv12.ps.bin");
    pub const PSDifferenceNv12: &[u8] =
        include_bytes!("../shaders/dxbc/zephyra.PSDifferenceNv12.ps.bin");
    pub const PSExclusionNv12: &[u8] =
        include_bytes!("../shaders/dxbc/zephyra.PSExclusionNv12.ps.bin");
    pub const PSMultiplyNv12: &[u8] =
        include_bytes!("../shaders/dxbc/zephyra.PSMultiplyNv12.ps.bin");
    pub const PSHueNv12: &[u8] = include_bytes!("../shaders/dxbc/zephyra.PSHueNv12.ps.bin");
    pub const PSSaturationNv12: &[u8] =
        include_bytes!("../shaders/dxbc/zephyra.PSSaturationNv12.ps.bin");
    pub const PSColorNv12: &[u8] = include_bytes!("../shaders/dxbc/zephyra.PSColorNv12.ps.bin");
    pub const PSLuminosityNv12: &[u8] =
        include_bytes!("../shaders/dxbc/zephyra.PSLuminosityNv12.ps.bin");
}

/// 按 `BlendMode` 的下标取 NV12 变体的字节码。
///
/// `0..=14`（系数可表达模式）都走 `PSMainNv12`，与宿主端「`None` ⇒ `ps[0]`」
/// 的规则对称；`15..=28` 与 `ps_for_blend_mode` 的 `Some(k)` 一一对应。
/// 非 NV12 纹理请用宿主自己的 `ps_for_blend_mode` + [`ps`] 表 —— 那才是 RGBA 路径。
pub const fn nv12_ps_for_blend_mode(i: usize) -> Option<&'static [u8]> {
    match i {
        0..=14 => Some(ps_nv12::PSMainNv12),
        15 => Some(ps_nv12::PSOverlayNv12),
        16 => Some(ps_nv12::PSDarkenNv12),
        17 => Some(ps_nv12::PSLightenNv12),
        18 => Some(ps_nv12::PSColorDodgeNv12),
        19 => Some(ps_nv12::PSColorBurnNv12),
        20 => Some(ps_nv12::PSHardLightNv12),
        21 => Some(ps_nv12::PSSoftLightNv12),
        22 => Some(ps_nv12::PSDifferenceNv12),
        23 => Some(ps_nv12::PSExclusionNv12),
        24 => Some(ps_nv12::PSMultiplyNv12),
        25 => Some(ps_nv12::PSHueNv12),
        26 => Some(ps_nv12::PSSaturationNv12),
        27 => Some(ps_nv12::PSColorNv12),
        28 => Some(ps_nv12::PSLuminosityNv12),
        _ => None,
    }
}

/// 着色器资产目录（供同步门禁在磁盘上读源、重编比对）。
pub const SHADER_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/shaders");
