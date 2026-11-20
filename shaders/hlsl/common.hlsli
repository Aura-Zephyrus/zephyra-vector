// ZephyraUI 的单一渲染着色器（SDF 圆角矩形 + 字形图集 + 图像采样）。
//
// 与 quad.rs 是严格对偶的两半：那里的 Vertex 字段顺序 / 偏移 / kind 阈值，
// 与这里的 VSIn 语义与 PSMain 分支必须逐项对应。
Texture2D<float4> g_tex : register(t0);
SamplerState      g_smp : register(s0);

// **目标像素的副本**（t1）。可分离 / 非可分离混合模式必须读 `dst`，而 D3D11
// **不能采样当前绑定的 RTV** —— 所以宿主先把渲染目标 `CopyResource` 到另一张纹理，
// 再把它当 SRV 绑到这里。见 `renderer.rs` 的 `dst_copy`。
//
// 系数可表达的那 15 个模式（`Clear`..`Screen`）**不读**这里：它们由固定功能混合
// 方程完成，PS 只负责算出源色。见 `PSMain` 与 `MAKE_BLEND_PS` 的分工。
Texture2D<float4> g_dst : register(t1);

struct VSIn {
    // 顶点位置刻意**不用** `POSITION`。
    //
    // 从 SM4 起 `POSITION` 在顶点着色器**输入**上是保留名：`fxc` 把它编成系统值
    // 语义（ISGN 里 `SystemValueType = 3` = `D3D_SV_POSITION`），于是 D3D11 认为这个
    // 输入由流水线自己提供，**不会**从顶点缓冲里取。
    //
    // 症状安静到极点：`CreateInputLayout` 成功、日志零错误，而每个顶点的位置都是
    // (0,0)，所有三角形退化到 NDC 原点，每个四边形只剩 1 个像素。
    //
    // ⚠⚠ 语义索引**必须互不重叠** —— 这是本文件最容易踩的一类错，且**与上面那条
    // 症状长得完全不一样**，所以更容易被误诊。
    //
    // HLSL 里 `float2 x : TEXCOORDn` 会**占掉 TEXCOORDn 与 TEXCOORD(n+1) 两格**。
    // 下面这份分配曾经写成这样 —— 逐项看索引都不同，但**按「float2 占两格」一算就
    // 大量重叠**（❌ 已修，留在这里当反例）：
    //
    // ```text
    // position  : TEXCOORD4 → 占 4,5     half_size : TEXCOORD5 → 占 5,6  ← 重叠
    // local     : TEXCOORD0 → 占 0,1     uv        : TEXCOORD1 → 占 1,2  ← 重叠
    // corner    : TEXCOORD2 → 占 2,3     kind      : TEXCOORD3 → 占 3    ← 重叠
    // ```
    //
    // `fxc` **不报错**，ISGN 里也如实记着每项自己的索引，所以
    // `the_shader_signature_matches_our_vertex_layout` 照样通过。但 D3D11 的
    // `CreateInputLayout` 是按**区间**绑定的，只有不冲突的那部分生效。实测症状
    // （`tests/minimal_draw.rs::逐个属性探测`）：
    //
    // - 几何**完全正确** —— 三角形铺满视口 4096/4096；
    // - 而每个属性的**第一个 float 都读到 0**，第二个分量正常；
    // - 于是 SDF 的 `local.x` 恒 0 → 每行只有一个 x 位置满足 `d ≈ 0` → 画面上是
    //   一条约 300 像素的线，**而不是空白**。
    //
    // 「不是空白」是最误导的地方：它看起来像「画了一点点」，于是排查被带到视口、
    // 数据、抗锯齿上去 —— 而这三样全都正常。
    //
    // ✅ **现行分配**（下方声明）：按「float2 占两格、float 占一格」**递增分配**，
    // 任何两项都不重叠。
    // ⚠ 改这里必须同步改 `shader.rs` 的 `VERTEX_LAYOUT`，并用 `fxc` 重新生成
    // `.bin` —— `include_bytes!` 不会因为源文件变化而重新编译。
    float2 position  : TEXCOORD0;   // 占 0,1
    float2 local     : TEXCOORD2;   // 占 2,3
    float2 uv        : TEXCOORD4;   // 占 4,5
    float4 color     : COLOR0;
    float2 corner    : TEXCOORD6;   // 占 6,7  x=圆角半径 y=描边宽度
    // 四边形的半宽半高。四个顶点填**同一个**值，插值后仍是它 —— 这是把一个
    // 「整个四边形的常量」送进片元着色器的标准做法。
    //
    // ⚠ 不能在片元里用 `local * 0.5` 之类推出来：那会让 `center` 恒等于 `half_size`，
    // 于是 SDF 退化成与位置无关的常数（方角块均匀半透明、圆角块直接消失）。
    // 完整推导见 `crates/base/zephyra-winit/src/quad.rs` 的 `Vertex::half_size`。
    float2 half_size : TEXCOORD8;   // 占 8,9
    float  kind      : TEXCOORD10;  // 占 10
    // 角形状（CSS corner-shape / superellipse(K) 的 K）：1=round, 2=squircle。
    float  shape     : TEXCOORD11;  // 占 11
};

struct VSOut {
    float4 clip   : SV_Position;
    float2 local  : TEXCOORD0;
    float2 uv     : TEXCOORD1;
    float4 color  : COLOR0;
    float2 corner : TEXCOORD2;
    // 整个四边形要么全是一类要么全不是：逐片元插值会在四边形内部产生中间值，
    // 导致部分像素走错分支（"图标变成纯色块" / "字变成方块"）。
    nointerpolation float kind : TEXCOORD3;
    // `half_size` 在片元里必须**精确**，不能是插值出来的近似值 —— 它要参与
    // `abs(p) - half_size` 这种相减。所以标成 `nointerpolation`。
    //
    // 实际上四个顶点填的是同一个值，插值结果本来就精确；标 `nointerpolation` 是为了
    // 让这件事在代码里显式，且避免未来有人改动顶点值时悄悄失去这个性质。
    nointerpolation float2 half_size : TEXCOORD4;
    // 原始 NDC 位置透传到片元。
    //
    // 用途只有一个：采样 `g_dst` 时需要**屏幕 uv**。`SV_Position` 给的是像素坐标，
    // 要换算成 uv 就得知道目标尺寸 —— 而那需要一个常量缓冲（本管线没有）。
    // 原始 NDC 位置透传到片元（屏幕 uv 用）。
    float2 ndc : TEXCOORD5;
    // 角形状 K（corner-shape），flat/nointerpolation。
    nointerpolation float shape : TEXCOORD6;
};

VSOut VSMain(VSIn i) {
    VSOut o;
    o.clip      = float4(i.position, 0.0, 1.0);
    o.local     = i.local;
    o.uv        = i.uv;
    o.color     = i.color;
    o.corner    = i.corner;
    o.kind      = i.kind;
    o.half_size = i.half_size;
    o.ndc       = i.position;
    o.shape     = i.shape;
    return o;
}

/// 目标像素的屏幕 uv（与 `quad::expand_into` 里 `to_ndc` 的 y 方向相反）。
///
/// NDC 的 y 向上、纹理 uv 的 y 向下，所以是 `0.5 - y*0.5` 而不是 `y*0.5 + 0.5`。
/// 写反的症状是"混合效果整体上下颠倒"，且只在 `dst` 与源不同时才看得出来。
float2 DstUv(VSOut i) {
    return float2(i.ndc.x * 0.5 + 0.5, 0.5 - i.ndc.y * 0.5);
}