// ═══════════════════════════════════════════════════════════════════════════
//  透镜（原版 `lens()`）
// ═══════════════════════════════════════════════════════════════════════════
//
// 逐字转译 `RoundedRectRefractionWithDispersionShaderString`（AndroidLiquidGlass,
// Apache-2.0）。与上方 `PSRefraction` 是**不同的公式**，不要合并：
//
// - 位移剖面是 `circleMap(1 - -sd / refractionHeight) * refractionAmount`，
//   在元素边缘取最大值、到带内侧归零；
// - 方向来自 `gradSdRoundedRect`（倒角半径取 `min(radius * 1.5, min(halfSize))`），
//   而不是 `SdNormal` 的有限差分；
// - 7 个通道按 `dispersionIntensity = (cx·cy)/(hx·hy)` 分配，红/橙/黄/绿/青/蓝/紫
//   各采样一次。

/// `gradSdRoundedRect` 的逐字转译（round），shape-aware 时回退到有限差分。
float2 grad_sd_rounded_rect(float2 coord, float2 half_size, float radius, float shape_k) {
    float k = shape_k;
    if (abs(k - 1.0) < 1e-4) {
        float2 corner_coord = abs(coord) - (half_size - float2(radius, radius));
        if (corner_coord.x >= 0.0 || corner_coord.y >= 0.0) {
            return sign(coord) * normalize(max(corner_coord, float2(0.0, 0.0)));
        } else {
            float grad_x = step(corner_coord.y, corner_coord.x);
            return sign(coord) * float2(grad_x, 1.0 - grad_x);
        }
    }
    // 非 round 形状：有限差分（与 SdNormal 同法），保持 corner-shape 严格一致。
    const float e = 0.5;
    float2 g;
    g.x = sd_round_rect(coord + float2(e, 0.0), half_size, radius, shape_k)
        - sd_round_rect(coord - float2(e, 0.0), half_size, radius, shape_k);
    g.y = sd_round_rect(coord + float2(0.0, e), half_size, radius, shape_k)
        - sd_round_rect(coord - float2(0.0, e), half_size, radius, shape_k);
    return g;
}
float2 grad_sd_rounded_rect(float2 coord, float2 half_size, float radius) {
    return grad_sd_rounded_rect(coord, half_size, radius, 1.0);
}

float circle_map(float x) {
    return 1.0 - sqrt(1.0 - x * x);
}

float4 PSLens(VSOut i) : SV_TARGET {
    float2 half_size = i.half_size;
    // `offset = -padding` 在本仓恒为 0（见 `offscreen::lens_composite_quad`）。
    float2 centered_coord = i.local - half_size;
    float radius = i.corner.x;
    float refraction_height = i.color.g;
    // 原版以负数写入 uniform：沿外法线向内采样 = 放大，而不是把背景挤出去。
    float refraction_amount = -i.corner.y;

    float sd = sd_round_rect(centered_coord, half_size, radius, i.shape);
    float2 uv = DstUv(i);
    if (-sd >= refraction_height) {
        return g_dst.SampleLevel(g_smp, uv, 0);
    }
    // 元素外那一圈：原版靠 layer 裁剪不画，本仓的四边形铺的是元素矩形，
    // 所以要自己乘 1px 遮罩（与 `PSRefraction` 同一条纪律）。
    float mask = saturate(-sd / 1.0);
    sd = min(sd, 0.0);

    float d = circle_map(1.0 - -sd / refraction_height) * refraction_amount;
    float grad_radius = min(radius * 1.5, min(half_size.x, half_size.y));
    // `depthEffect = false`（LiquidToggle 的默认值）⇒ 只剩 SDF 梯度那一项。
    float2 grad = normalize(grad_sd_rounded_rect(centered_coord, half_size, grad_radius, i.shape));
    // 像素位移 → uv 位移。与 `PSRefraction` 同理走 `ddx/ddy`，不占顶点字段。
    float2 px2uv = float2(ddx(uv.x), ddy(uv.y));
    float2 base = d * grad * px2uv;
    float dispersion_intensity =
        (centered_coord.x * centered_coord.y) / (half_size.x * half_size.y);
    float2 dispersed = base * dispersion_intensity;

    float4 color = float4(0.0, 0.0, 0.0, 0.0);

    float4 red = g_dst.SampleLevel(g_smp, saturate(uv + base + dispersed), 0);
    color.r += red.r / 3.5;
    color.a += red.a / 7.0;

    float4 orange = g_dst.SampleLevel(g_smp, saturate(uv + base + dispersed * (2.0 / 3.0)), 0);
    color.r += orange.r / 3.5;
    color.g += orange.g / 7.0;
    color.a += orange.a / 7.0;

    float4 yellow = g_dst.SampleLevel(g_smp, saturate(uv + base + dispersed * (1.0 / 3.0)), 0);
    color.r += yellow.r / 3.5;
    color.g += yellow.g / 3.5;
    color.a += yellow.a / 7.0;

    float4 green = g_dst.SampleLevel(g_smp, saturate(uv + base), 0);
    color.g += green.g / 3.5;
    color.a += green.a / 7.0;

    float4 cyan = g_dst.SampleLevel(g_smp, saturate(uv + base - dispersed * (1.0 / 3.0)), 0);
    color.g += cyan.g / 3.5;
    color.b += cyan.b / 3.0;
    color.a += cyan.a / 7.0;

    float4 blue = g_dst.SampleLevel(g_smp, saturate(uv + base - dispersed * (2.0 / 3.0)), 0);
    color.b += blue.b / 3.0;
    color.a += blue.a / 7.0;

    float4 purple = g_dst.SampleLevel(g_smp, saturate(uv + base - dispersed), 0);
    color.r += purple.r / 7.0;
    color.b += purple.b / 3.0;
    color.a += purple.a / 7.0;

    // 预乘空间：rgb 与 a 一起乘遮罩。
    return color * mask;
}
