// ═══════════════════════════════════════════════════════════════════════════
//  背景模糊（磨砂玻璃）
// ═══════════════════════════════════════════════════════════════════════════
//
// 与折射共用同一套顶点字段约定（`local`/`half_size` 是**元素**的、`uv` 是元素
// 内归一化、屏幕 uv 走 `DstUv`），也读同一张 `g_dst`（元素背后的目标副本）。
// 区别只在采样方式：折射按 SDF 法线**位移**，本入口对背景做**各向同性高斯卷积**
// （不位移）。
//
// # 为什么是单趟二维卷积而不是 H / V 两趟
//
// 元素自身的模糊（`Filter::Blur`）走 `blurs` 的 H/V 乒乓 RT —— 那条链读的是
// `g_tex`（元素自己的离屏）。背景模糊读的是 `g_dst`（屏幕副本），拿不到那张
// 乒乓 RT，也不该为它另建一套。所以这里一次性做二维核，代价是 O(r²) 采样；
// 用**截半径 + 步长抽样**把每轴压到 ~13 个采样，磨砂到看不出细节即可，不必
// 逐像素精确。
//
// σ 塞在 `corner.y`，圆角半径在 `corner.x`（与折射同位置、不同语义）——这是
// 为了让本入口复用 `backdrop_blur_composite_quad` 的顶点布局，不新增顶点字段。
float4 PSBackdropBlur(VSOut i) : SV_TARGET {
    float radius = i.corner.x;
    float sigma  = i.corner.y;

    // 元素内坐标 → SDF → 1px 抗锯齿遮罩（与折射同一条纪律：元素外不输出）。
    float2 p = i.local - i.half_size;
    float d = sd_round_rect(p, i.half_size, radius, i.shape);
    float mask = saturate(-d / 1.0);

    float2 uv = DstUv(i);
    // 一个像素对应多少 uv：走 ddx/ddy，不占顶点字段（与折射同）。
    float2 px2uv = float2(ddx(uv.x), ddy(uv.y));

    // 核半径：3σ 截到 12（磨砂不需要更宽）；步长把每轴抽样压到 ≤ ~13 个。
    float r = min(ceil(3.0 * sigma), 12.0);
    float stride = max(1.0, r / 6.0);
    // ⚠ 循环上界按**实际要采的点数**算，而不是写死 12。
    //
    // 写死 12 时，循环**照样**跑 25×25 = 625 次 —— 里面绝大多数是
    // `continue` 掉的空转。着色器里的循环是**串行**的：这 625 圈的开销
    // 全都记在每个片元头上，而真正用到的采样只有 ~13×13。
    //
    // 改成 `taps` 之后循环次数与采样次数一致，**采到的纹素一个不差**
    // （`taps = ceil(r / stride)` ⇒ `taps * stride == r`，恰好覆盖
    // 原来 `|f| ≤ r` 的全部偏移）⇒ **画面逐像素相同**，只是不再空转。
    int taps = (int)ceil(r / stride);

    float4 sum = float4(0.0, 0.0, 0.0, 0.0);
    float wsum = 0.0;
    // 上界由 `taps` 决定（r ≤ 12 ⇒ taps ≤ 12），循环次数有界，不会因异常 σ 跑飞。
    [loop]
    for (int ky = -taps; ky <= taps; ++ky) {
        float fy = float(ky) * stride;
        [loop]
        for (int kx = -taps; kx <= taps; ++kx) {
            float fx = float(kx) * stride;
            float w = GaussWeight(fy, sigma) * GaussWeight(fx, sigma);
            float2 suv = saturate(uv + float2(fx, fy) * px2uv);
            sum += g_dst.SampleLevel(g_smp, suv, 0) * w;
            wsum += w;
        }
    }
    if (wsum <= 0.0) { return float4(0.0, 0.0, 0.0, 0.0); }
    float4 bg = sum / wsum;
    // `g_dst` 预乘：覆盖率取 alpha，rgb 不再乘。遮罩同时缩 rgb 与 alpha，
    // 保持预乘不变量（rgb ≤ a）。
    float a = bg.a * mask;
    return float4(bg.rgb * mask, a);
}
