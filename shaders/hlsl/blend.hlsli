// ═══════════════════════════════════════════════════════════════════════════
//  E4.2 / E4.3：可分离与���可分离混合模式
// ═══════════════════════════════════════════════════════════════════════════
//
// # 为什么必须用 PS 变体而不是统一一个 PS
//
// 系数可表达的那 15 个（`Clear`..`Screen`）是 `r = Cs·ks + Cd·kd` 的形式，D3D 的
// 固定功能混合方程**原样就能算**。往后的 14 个不是：
//
// - **可分离** 10 个（`Overlay`..`Multiply`）：`B = f(Cb, Cs)` 逐通道，但 `f` 不是
//   线性组合（`Overlay` 里有 `1−2·Cb` 这种分支），固定功能表达不了。
// - **非可分离** 4 个（`Hue` / `Saturation` / `Color` / `Luminosity`）：结果依赖
//   **对方的色相 / 饱和度 / 明度**，逐通道算不出来。
//
// # 为什么是"每个模式一个入口"而不是"一个入口 + 一个模式常量"
//
// 模式常量要有地方放：常量缓冲（要新建 `ID3D11Buffer` + `SetConstantBuffers`，
// D3D12 还要根签名里加项）或顶点数据（会把混���模式塞进与颜色无关的通道，
// 污染 `quad.rs` 的顶点契约）。而**离线编译**的变体零运行时开销、零新管线状态，
// 代价只是 `.bin` 变多 —— 那本来就是离线成本。
//
// 所以用宏生成 14 个入口：编译期展开，运行时按 `runs` 段的 `blend` 选 PS。
//
// # 合成公式（预乘空间）
//
// ```text
//   as = s.a                       源的 alpha
//   Cs = s.rgb / as                反预乘
//   Cd = d.rgb / d.a               反预乘
//   B  = blend(Cs, Cd)             混合函数的**非预乘**结果
//   out.rgb = (1−as)·Cd + as·B     ← 已经是预乘
//   out.a   = as + ad·(1−as)
// ```
//
// `as == 0` 时直接返回全 0：`(1−as)·Cd` 会把目标**擦掉**，而"什么都没画"绝不该
// 改变目标像素。
//
// # 与系数模式的等价性
//
// `SrcOver` 在这个公式下的结果是 `(1−as)·Cd + as·Cs = Cs + (1−as)·Cd`，与固定功能
// 的 `ONE/INV_SRC_ALPHA` **完全一致**。所以 15 个系数模式不需要走这里 —— 少一条
// 路径就少一处能漂移的地方。

float Lum(float3 c) { return dot(c, float3(0.3, 0.59, 0.11)); }

float3 ClipColor(float3 c) {
    // 非可分离模式的标准前置：把色域外的负分量拉到 0（保住明度关系）。
    float l = Lum(c);
    float n = min(c.r, min(c.g, c.b));
    float x = max(c.r, max(c.g, c.b));
    if (n < 0.0) { c = l + (c - l) * l / max(l - n, 1e-6); }
    if (x > 1.0) { c = l + (c - l) * (1.0 - l) / max(x - l, 1e-6); }
    return c;
}

float3 SetLum(float3 c, float l) { return ClipColor(c + (l - Lum(c))); }

// ⚠ 返回**标量**，不是 float3。
//
// 写成 `float3 Sat(float3)` 时 `fxc` 会给 X3206「implicit truncation of vector type」
// ——而它**能编译**、结果也对（标量被 splat 成 float3(x,x,x)），于是这个警告看起来
// 可以忽略。实际上后面 `SetSat(c, Sat(other))` 把它当标量用，签名在说谎。
float Sat(float3 c) { return max(c.r, max(c.g, c.b)) - min(c.r, min(c.g, c.b)); }

float3 SetSat(float3 c, float s) {
    // 找出各通道在 min/max 之间的相对位置，按新的饱和度重排。
    float mn = min(c.r, min(c.g, c.b));
    float mx = max(c.r, max(c.g, c.b));
    float d = mx - mn;
    if (d <= 0.0) { return float3(0, 0, 0); }
    return (c - mn) / d * s;
}

// ---- 可分离：逐通道 ----
float3 BOverlay(float3 b, float3 s) {
    return lerp(2.0 * b * s, 1.0 - 2.0 * (1.0 - b) * (1.0 - s), step(0.5, b));
}
float3 BDarken(float3 b, float3 s) { return min(b, s); }
float3 BLighten(float3 b, float3 s) { return max(b, s); }
float3 BColorDodge(float3 b, float3 s) {
    float3 z = step(0.0, b);
    float3 m = min(1.0, (1.0 - b) / max(s, 1e-6));
    // `lerp(float3(0.0), m, z)` 写成 `m * z`：`fxc` 拒绝把单标量 splat 当
    // `lerp` 的第一个实参（X3014），而这里的 else 分支恰好是 0，等价。
    return m * z;
}
float3 BColorBurn(float3 b, float3 s) {
    float3 z = step(b, 1.0);
    float3 m = min(1.0, 1.0 - (1.0 - b) / max(1.0 - s, 1e-6));
    return m * z + (1.0 - z);   // 同上：else 分支是 1。
}
float3 BHardLight(float3 b, float3 s) { return BOverlay(s, b); }
/// SoftLight（Pegtop / W3C Compositing 标准式）。
///
/// ```text
/// s <= 0.5:  b - (1 - 2s)·b·(1 - b)
/// s >  0.5:  b + (2s - 1)·(D(b) - b)
/// D(b) = ((16b - 12)b + 4)b   (b <= 0.25)   ← 与 sqrt(b) 在 b=0.25 处都等于 0.75，连续
///        sqrt(b)              (b >  0.25)
/// ```
///
/// # 2026-10-25：这里原来写的是一个**错公式**，已按 parity 分歧定因后重修
///
/// 原实现：
/// ```hlsl
/// float3 d = ((16.0*b - 12.0)*b + 4.0)*b * step(b, 0.25);
/// return sqrt(max(b, 0.0)) * step(0.5, b) * s + d * (1.0 - s);
/// ```
/// 两处错，叠出一个极难查的画面 bug：
///
/// 1. **死区**：`0.25 < b < 0.5` 时两个 `step` 同时为 0 ⇒ **B 恒等于 0**
///    （混合结果纯黑）。中灰底（0.3~0.45）上的 SoftLight 内容整块变黑，
///    而没有任何报错。
/// 2. **公式本身不对**：即便逃过死区，`sqrt(b)·s`（b≥0.5 支）也不是任何
///    标准定义；Pegtop 的该支是 `b + (2s-1)(D(b)-b)`，低支是
///    `b - (1-2s)b(1-b)`。
///
/// 死区平时看不见，是因为两个后端的 **UNORM8 量化差 1 个 LSB**：同一帧
/// 0.5 灰底色，D3D11 存成 128/255 = 0.502（逃过死区），D3D12 存成
/// 127/255 = 0.498（掉进死区）—— 于是只有 D3D12 整块变黑，parity 差 87 级。
/// 定因过程记在 `docs/dev-log/`（backend_parity SoftLight 分歧条目）。
float3 BSoftLight(float3 b, float3 s) {
    float3 poly = ((16.0 * b - 12.0) * b + 4.0) * b;
    // b > 0.25 用 sqrt(b)，否则多项式；两者在 0.25 处连续（都等于 0.75）。
    float3 D = lerp(sqrt(max(b, 0.0)), poly, step(b, 0.25));
    float3 lo = b - (1.0 - 2.0 * s) * b * (1.0 - b);       // s <= 0.5
    float3 hi = b + (2.0 * s - 1.0) * (D - b);             // s >  0.5
    return lerp(hi, lo, step(s, 0.5));
}
float3 BDifference(float3 b, float3 s) { return abs(b - s); }
float3 BExclusion(float3 b, float3 s) { return b + s - 2.0 * b * s; }
float3 BMultiply(float3 b, float3 s) { return b * s; }

// ---- 非可分离：需要 ClipColor / SetLum / SetSat ----
float3 BHue(float3 b, float3 s) { return SetLum(SetSat(s, Sat(b)), Lum(b)); }
float3 BSaturation(float3 b, float3 s) { return SetLum(SetSat(b, Sat(s)), Lum(b)); }
float3 BColorMode(float3 b, float3 s) { return SetLum(s, Lum(b)); }
float3 BLuminosity(float3 b, float3 s) { return SetLum(b, Lum(s)); }

/// 生成一个「PS 变体合成」入口。
///
/// `expr` 求的是**非预乘**的 `B`，参数 `b` = 目标色、`s` = 源色。
///
/// `ZEPHYRA_NV12` 变体把入口名统一加 `Nv12` 后缀，于是同一份宏能编出两套 PS：
/// 默认那套逐字节不变，NV12 那套只在图像分支多一次 YUV→RGB（见 shade.hlsli）。
#ifdef ZEPHYRA_NV12
#define ZEPHYRA_PS_NAME(name) name##Nv12
#else
#define ZEPHYRA_PS_NAME(name) name
#endif
#define MAKE_BLEND_PS(entry, expr)                                 \
    float4 ZEPHYRA_PS_NAME(entry)(VSOut i) : SV_TARGET {           \
        float4 s = ShadeSource(i);                                 \
        if (s.a <= 0.0) { return float4(0.0, 0.0, 0.0, 0.0); }     \
        float4 d = g_dst.Sample(g_smp, DstUv(i));                  \
        float3 Cs = s.rgb / s.a;                                   \
        float3 Cd = d.a > 0.0 ? d.rgb / d.a : float3(0.0, 0.0, 0.0); \
        float3 B = expr;                                           \
        float a = s.a;                                             \
        return float4((1.0 - a) * Cd + a * B, a + d.a * (1.0 - a)); \
    }

// 可分离 10 个。顺序与 `filter.rs` 的枚举**一致** —— `mode_by_index` 依赖它。
MAKE_BLEND_PS(PSOverlay,     BOverlay(Cd, Cs))
MAKE_BLEND_PS(PSDarken,      BDarken(Cd, Cs))
MAKE_BLEND_PS(PSLighten,     BLighten(Cd, Cs))
MAKE_BLEND_PS(PSColorDodge,  BColorDodge(Cd, Cs))
MAKE_BLEND_PS(PSColorBurn,   BColorBurn(Cd, Cs))
MAKE_BLEND_PS(PSHardLight,   BHardLight(Cd, Cs))
MAKE_BLEND_PS(PSSoftLight,   BSoftLight(Cd, Cs))
MAKE_BLEND_PS(PSDifference,  BDifference(Cd, Cs))
MAKE_BLEND_PS(PSExclusion,   BExclusion(Cd, Cs))
MAKE_BLEND_PS(PSMultiply,    BMultiply(Cd, Cs))

// 非可分离 4 个
MAKE_BLEND_PS(PSHue,         BHue(Cd, Cs))
MAKE_BLEND_PS(PSSaturation,  BSaturation(Cd, Cs))
MAKE_BLEND_PS(PSColor,       BColorMode(Cd, Cs))
MAKE_BLEND_PS(PSLuminosity,  BLuminosity(Cd, Cs))