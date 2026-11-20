// ═══════════════════════════════════════════════════════════════════════════
//  E4.4 / E4.5：模糊与投影
// ═══════════════════════════════════════════════════════════════════════════
//
// # 顶点数据怎么带参数
//
// 模糊 pass 画的是一张**铺满离屏 RT 的四边形**，它的顶点字段含义与正常图元不同，
// 于是用 `kind` 分流：
//
// | 字段      | 正常图元          | 模糊 pass                    |
// |-----------|-------------------|------------------------------|
// | `kind`    | 0..3（图元类别）  | 4（BLUR）                    |
// | `local`   | 相对四边形中心    | 该片元在源 RT 里的**像素坐标** |
// | `half_size`| 四边形半宽半高    | 源 RT 的**尺寸**（nointerp）  |
// | `corner`  | (圆角, 描边)      | (方向, σ) —— 方向 0=H 1=V     |
// | `uv`      | 纹理 uv           | 片元中心（0..1）              |
//
// 之所以能把参数塞进这些字段而不是用常量缓冲：模糊 pass 每个 RT 只有一组参数，
// 放进顶点数据就省掉一个 `ID3D11Buffer` + `SetConstantBuffers`（D3D12 那边还要动
// 根签名）。代价是 `local` 的语义被复用了 —— 用 `kind` 分流，不会串。
//
// # 为什么要 H / V 两趟
//
// 高斯核在二维上是**可分离**的：先沿 x 模糊再沿 y 模糊，与一次性做二维卷积等价，
// 但代价从 O(r²) 降到 O(2r)。这是 E4.4 能跑得动的原因。
//
// 方向不进常量而是拆成两个入口 —— 与混合模式同样的理由：离线变体零运行时开销。

/// 高斯权重。σ ≤ 0 时退化成「不模糊」，避免除零。
float GaussWeight(float i, float sigma) {
    if (sigma <= 0.0) { return i == 0.0 ? 1.0 : 0.0; }
    return exp(-(i * i) / (2.0 * sigma * sigma));
}

/// 半径 = ⌈3σ⌉，截到 32 —— 32 像素的 σ 已经超出「看得清边缘」的范畴，再多是浪费。
float BlurRadius(float sigma) {
    return min(ceil(3.0 * sigma), 32.0);
}

/// 抽样的**步长**（像素）。
///
/// # 为什么要有步长
///
/// 核半径 r = 3σ ⇒ 逐纹素采样要 2r+1 次（σ=10 时 **61 次**）。而实测
/// （gallery，1400×800，Intel UHD 630，材质页一帧）**光「模糊乒乓」这一项
/// 就是 5.6 ms** —— 比整帧的基础开销还大，是那一帧 GPU 占用的大头。
///
/// 高斯核在 3σ 之外几乎没有质量，而**抽稀**（每隔 step 个纹素取一个，
/// 权重仍按**真实偏移**算）是无偏的：和仍是同一个高斯，只是方差大一点。
/// 对「柔和投影」这种低频用途，肉眼看不出差别。
///
/// step = ⌊r/6⌋ ⇒ 每轴最多 ~13~17 次采样（σ=10 时 61 → 13，4.7 倍）。
/// 小 σ（r ≤ 6）时 step = 1，**逐字等于**原来的核 —— 绝大多数 UI 里的
/// 细描边 / 小阴影根本走不到抽稀那条路。
float BlurStride(float r) {
    return max(1.0, floor(r / 6.0));
}

float4 PSBlurH(VSOut i) : SV_TARGET {
    float sigma = i.corner.y;
    float r = BlurRadius(sigma);
    float stride = BlurStride(r);
    float2 size = i.half_size;
    // ⚠ 累加器是 **float4**，不是 float3 —— 四通道**一起**模糊，与 Chrome / Skia 一致。
    //
    // 之前这里是 `float3`，alpha 从中心那一抽样直接取。那样做的后果不是"偏暗"，
    // 而是**投影有硬边**：影子颜色糊开了，覆盖率却还是原图的硬边，于是半透明阴影
    // 周围出现一圈 alpha 不连续的暗框。
    //
    // 为什么"只糊 rgb"会被想成更安全：不糊 alpha 时 rgb 与 a **本来就不同步**，这才是
    // 暗边的来源。预乘空间里正确的做法是对 `(rgb, a)` 一起做同一个卷积 —— 出来仍然
    // 满足 `rgb ≤ a`，因为卷积是线性的、且输入逐像素都满足这个不等式。
    float4 sum = float4(0.0, 0.0, 0.0, 0.0);
    float wsum = 0.0;
    // `[0, r]` 的整数上界。写成动态上界的循环在 SM5 里没问题，但**必须**给上限，
    // 否则恶意/异常的 σ 会让它跑几百万次。
    [loop]
    for (int k = 0; k <= 32; ++k) {
        float off = float(k) * stride;
        if (off > r) { break; }
        float2 uv = (i.local + float2(off, 0.0)) / size;
        float w = GaussWeight(off, sigma);
        sum += g_tex.SampleLevel(g_smp, uv, 0) * w;
        wsum += w;
        // 对称项：k = 0 只算一次（否则权重翻倍）。
        if (k > 0) {
            float2 uv2 = (i.local - float2(off, 0.0)) / size;
            sum += g_tex.SampleLevel(g_smp, uv2, 0) * w;
            wsum += w;
        }
    }
    // `wsum > 0` 恒成立（k=0 的权重是 1），但仍挡一道：σ 异常时宁可输出全 0
    // 也不要 NaN —— NaN 会顺着预乘链把整帧污染掉。
    if (wsum <= 0.0) { return float4(0.0, 0.0, 0.0, 0.0); }
    // 归一化后的 alpha 就是「被模糊的覆盖率」，直接送进后面的合成。
    return sum / wsum;
}

float4 PSBlurV(VSOut i) : SV_TARGET {
    float sigma = i.corner.y;
    float r = BlurRadius(sigma);
    float stride = BlurStride(r);
    float2 size = i.half_size;
    float4 sum = float4(0.0, 0.0, 0.0, 0.0);
    float wsum = 0.0;
    [loop]
    for (int k = 0; k <= 32; ++k) {
        float off = float(k) * stride;
        if (off > r) { break; }
        float2 uv = (i.local + float2(0.0, off)) / size;
        float w = GaussWeight(off, sigma);
        sum += g_tex.SampleLevel(g_smp, uv, 0) * w;
        wsum += w;
        if (k > 0) {
            float2 uv2 = (i.local - float2(0.0, off)) / size;
            sum += g_tex.SampleLevel(g_smp, uv2, 0) * w;
            wsum += w;
        }
    }
    if (wsum <= 0.0) { return float4(0.0, 0.0, 0.0, 0.0); }
    return sum / wsum;
}

/// 把离屏 RT 的结果**合成**回当前目标。
///
/// `corner.y > 0` 时把 rgb 换成一个**常量色**（投影的染色），alpha 保留 —— 那就是
/// DropShadow 的「剪影 → 模糊 → 染色」三步里的第三步。`corner.y == 0` 时原样透传，
/// 那就是普通模糊的回放。
float4 PSComposite(VSOut i) : SV_TARGET {
    float4 t = g_tex.SampleLevel(g_smp, i.uv, 0);
    float tintAmount = i.corner.y;
    if (tintAmount <= 0.0) {
        // 直通。已经是预乘的，别再乘一次 alpha。
        return t;
    }
    float3 tint = i.color.rgb;   // 顶点里带的是**非预乘**的目标颜色
    // 染色不能整体乘 alpha —— 那会让半透明阴影变成"颜色越淡越看不见"。
    // 正确做法：保留覆盖率，用目标色替换色相。`tint.a` 是阴影浓度。
    float a = t.a * i.color.a;
    return float4(tint * a, a);
}