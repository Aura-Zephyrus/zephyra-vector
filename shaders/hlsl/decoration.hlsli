// ═══════════════════════════════════════════════════════════════════════════
//  玻璃装饰（高光 / 外投影挖洞 / 内阴影）
// ═══════════════════════════════════════════════════════════════════════════
//
// 一比一复刻 Android `AndroidLiquidGlass`（Apache-2.0）的 drawBackdrop 四件套
// 里除折射外的三件。层序是契约：外投影（最下）→ 内容 → 高光 → 内阴影（最上）。
//
// # 与折射共用 SDF 家族
//
// 三件装饰全部按**圆角矩形 SDF**（`sd_round_rect` + `SdNormal`）算覆盖，
// 顶点字段走折射那套"字段挪用"约定（`local`/`half_size` 是元素的、
// `uv` 是元素内归一化）。参数怎么塞进 `corner` / `color`，见
// `offscreen.rs` 里三个对应构造函数的表 —— 两处必须同改。
//
// # 模糊的近似
//
// 原版描边 / 内阴影的柔边走 `BlurMaskFilter`（高斯）。这里用距离场的解析
// 高斯 `exp(-d²/2σ²)` 直接算覆盖率 —— 对一维"到边界的距离"分布，两者的
// 覆盖率曲线在视觉上不可分，而省掉整条乒乓模糊。

/// 高斯柔边的覆盖率：距边界 `d`（向内为负、外侧为正）。
///
/// 描边**居中于边界**（内侧一半可见），所以覆盖从 `d = 0` 向内按 σ 衰减、
/// 向外不画（clip 到形状内是高光那边的纪律；内阴影正好相反）。
float SoftBand(float d, float sigma) {
    float s = max(sigma, 1e-4);
    return exp(-(d * d) / (2.0 * s * s));
}

/// 挖洞外投影合成：影子（已模糊、未染色）→ 染色 → **挖掉元素内侧**。
///
/// # 为什么必须挖
///
/// 玻璃是半透明的：不挖的话，形状内侧的影子会透出来叠在背景上，
/// 看起来像玻璃底下有脏污。原版的做法就是"剪影 → 模糊 → Clear 挖掉
/// 未偏移副本"，等价于把内侧 alpha 清零。
///
/// 顶点字段见 `hollow_shadow_composite_quad`：`local` 走**元素坐标系**
/// （元素左上为原点，外侧为负/超出），`corner.x` 是圆角半径，
/// `corner.y`/`color` 与 `PSComposite` 的染色约定相同。
float4 PSHollowShadow(VSOut i) : SV_TARGET {
    float4 t = g_tex.SampleLevel(g_smp, i.uv, 0);
    float tintAmount = i.corner.y;
    // 挖洞：`sd > 0` 是元素**外**（影子保留），`sd <= 0` 是内侧（清零）。
    // 覆盖带给 1px 抗锯齿过渡，避免挖出一条硬边。
    float2 p = i.local - i.half_size;
    float sd = sd_round_rect(p, i.half_size, i.corner.x, i.shape);
    float keep = saturate(sd / 1.0);
    if (tintAmount <= 0.0) {
        return t * keep;
    }
    float a = t.a * i.color.a * keep;
    return float4(i.color.rgb * a, a);
}

/// 镜面高光 overlay：内缘亮线，画在内容**之上**（Plus 混合）。
///
/// # 三种着色公式（`corner.y` = kind）
///
/// | kind | 公式（原版 AGSL） |
/// |---|---|
/// | 0 Plain | 均匀描边，无着色 |
/// | 1 Default | `intensity = pow(abs(dot(grad, normal)), falloff)`，白 @0.5 |
/// | 2 Ambient | `t = step(0, d)` 半黑半白 × 0.38（SrcOver，由宿主切混合槽） |
///
/// 顶点字段见 `highlight_overlay_quad`：`color` = `(描边宽, 模糊σ, 强度, 方向角)`。
/// 颜色是本入口的**常量**（白），强度走 `color.z` —— 动画按压进度只改它。
float4 PSHighlight(VSOut i) : SV_TARGET {
    float strokeW = i.color.x;   // 描边宽（已含 ×2 换算）
    float sigma   = i.color.y;   // 模糊 σ
    float alpha   = i.color.z;   // 整体强度（0..1）
    float angle   = radians(i.color.w);
    float kind    = i.corner.y;

    float2 p = i.local - i.half_size;
    float d  = sd_round_rect(p, i.half_size, i.corner.x, i.shape);
    // ⚠ 原版 `HighlightNode` 把描边**裁到轮廓内**（`canvas.clipOutline`）：
    // 可见的是靠边的一条**内**带，形状外面一个像素都不画。
    // 这里曾经允许 `d <= 0.75`，于是 Ambient 的白色那半圈画到了形状**外面**，
    // 在深色背景上变成一条锯齿亮线 —— 看着像「描边坏了一层」。
    if (d > 0.5) { discard; }
    // `drawOutline(Stroke, W)` 的 W **居中**于轮廓 ⇒ 裁掉外侧半条后只剩 W/2
    // 的内带；`blurRadius` 再把它软化。下限 0.5px 防止整数化后带子消失。
    float band = max(strokeW * 0.5, 0.5);
    float cover = SoftBand(d + band * 0.5, max(sigma, 0.5));
    if (cover <= 0.0039) { discard; }

    // 边缘法线用 `gradSdRoundedRect`（与 `PSLens` / 原版高光同一份），
    // 而不是有限差分 —— 原版高光与透镜共用这一个函数。
    float grad_radius = min(i.corner.x * 1.5, min(i.half_size.x, i.half_size.y));
    float2 grad = grad_sd_rounded_rect(p, i.half_size, grad_radius, i.shape);
    float2 normal = float2(cos(angle), sin(angle));
    float dd = dot(grad, normal);
    float intensity = pow(abs(dd), 1.0);

    float3 tint = float3(1.0, 1.0, 1.0);
    float base = 0.5;
    if (kind > 1.5) {
        // Ambient（原版 `AmbientHighlightShaderString`）：
        //   `t = step(0, dot(grad, normal))` ⇒ **受光那半圈白、背光那半圈黑**，
        //   强度 `pow(abs(dot), 1)`。判据是「法线朝哪」，不是「形状内还是外」。
        //   旧实现写的是 `step(0, sd)`（外侧=白），方向信息整个丢掉：整圈外沿发白、
        //   整圈内沿发黑，看起来像一圈描边而不是环境光。
        tint = (dd >= 0.0) ? float3(1.0, 1.0, 1.0) : float3(0.0, 0.0, 0.0);
        base = 0.38;
    }
    float a = base * alpha * intensity * cover;
    // 预乘输出。
    return float4(tint * a, a);
}

/// 内阴影 overlay：形状内侧的暗边，画在内容**之上**（SrcOver，在高光之后）。
///
/// 数学：原版"clip 形状 → 填阴影色 → Clear 挖偏移副本 → blur → clip 回"
/// 等价于——阴影区 = 元素内 **且** 偏移形状外的点集，覆盖率按到偏移形状
/// 边界的高斯衰减。
///
/// 顶点字段见 `inner_shadow_overlay_quad`：`corner.y` = σ，
/// `color` = `(偏移x, 偏移y, 总强度, 0)`；颜色是常量黑 @0.15（原版默认）。
float4 PSInnerShadow(VSOut i) : SV_TARGET {
    float sigma = i.corner.y;
    float2 off  = i.color.xy;
    float alpha = i.color.z;

    float2 p = i.local - i.half_size;
    float d = sd_round_rect(p, i.half_size, i.corner.x, i.shape);
    // 元素内才画（`d < 0`），1px 过渡。
    float inside = saturate(-d / 1.0);
    if (inside <= 0.0039) { discard; }

    // 偏移形状的 SDF：阴影从"偏移形状之外"衰减进来。
    float di = sd_round_rect(p - off, i.half_size, i.corner.x, i.shape);
    float cover = SoftBand(di, sigma) * inside;
    if (cover <= 0.0039) { discard; }

    float a = 0.15 * alpha * cover;
    return float4(0.0, 0.0, 0.0, a);
}