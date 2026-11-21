
#ifdef ZEPHYRA_NV12
/// BT.601 limited range 的 NV12 → RGB，与 `zephyra-camera::Nv12Planes::to_rgba`
/// 的整数近似逐值一致（右移 8 位 + clamp 到 0..255）。
///
/// 采样 NV12 SRV 得到的 float4 是 `(Y/255, U/255, V/255, 1)`；本函数把它变成
/// 预乘空间图像分支能直接用的直通 RGB。只在定义了 `ZEPHYRA_NV12` 的变体入口
/// （`zephyra_nv12.hlsl`）里编进这段代码，默认 `PSMain` 不含一条相关指令。
float3 nv12_to_rgb(float4 texel) {
    float y = texel.r * 255.0f - 16.0f;
    float u = texel.g * 255.0f - 128.0f;
    float v = texel.b * 255.0f - 128.0f;
    float yy = y * 298.0f;
    // `floor(x / 256)` 等价于 camera 侧 int 的算术右移 8（含负数）。
    float3 c = floor(float3(yy + 409.0f * v,
                            yy - 100.0f * u - 208.0f * v,
                            yy + 516.0f * u) / 256.0f);
    return clamp(c / 255.0f, 0.0f, 1.0f);
}
#endif

// 覆盖率对比度预扭曲，照 Skia `SkMaskGamma::apply_contrast`：
// `a' = a + (1-a) * contrast * a`（默认 contrast = 0.5，SK_GAMMA_CONTRAST）。
// 线性 alpha 混合会把细笔画冲淡；预扭曲让中间覆盖率略暗，小字更实、边缘更干净。
static const float TEXT_CONTRAST = 0.5f;
float apply_text_contrast(float a) {
    return a + (1.0f - a) * TEXT_CONTRAST * a;
}
float3 apply_text_contrast(float3 a) {
    return a + (1.0f - a) * TEXT_CONTRAST * a;
}

// sRGB 传递函数（Skia `SkMaskGamma` 用的分段曲线）。
float srgb_to_linear(float c) {
    return c <= 0.04045f ? c / 12.92f : pow(max((c + 0.055f) / 1.055f, 1e-5f), 2.4f);
}
float linear_to_srgb(float c) {
    return c <= 0.0031308f ? c * 12.92f : 1.055f * pow(max(c, 1e-5f), 1.0f / 2.4f) - 0.055f;
}

// 覆盖率 gamma 预扭曲：Skia `SkTMaskGamma_build_correcting_lut` 的逐像素版本。
// 假设背景是文字色的感知反色（Skia 的 CanonicalColor 近似），算出「线性光混合
// 后再编码」的目标值，再反解出让当前 gamma 空间 blend 得到同一结果的 alpha。
// 这样白色文字不会过粗、黑色文字不会过细，且任意文字颜色都有一致观感。
float gamma_correct_coverage(float a, float src_encoded) {
    float dst = 1.0f - src_encoded;
    float denom = 2.0f * src_encoded - 1.0f;
    if (abs(denom) < 0.05f) {
        return a; // 中灰文字：线性与 gamma 空间差异很小，避免除零。
    }
    float lin_s = srgb_to_linear(src_encoded);
    float lin_d = srgb_to_linear(dst);
    float corrected = linear_to_srgb(lin_s * a + lin_d * (1.0f - a));
    return clamp((corrected - dst) / denom, 0.0f, 1.0f);
}

// Skia `pack4xHToMask` 的 12-tap 高斯 FIR：R/G/B 三个子像素中心分别在
// 1/6、1/2、5/6 px。系数和 = 272/256 = 1.0625，模拟轻微墨迹扩散。
static const float FIR_R[12] = { 3, 11, 28, 51, 64, 57, 36, 16, 5, 1, 0, 0 };
static const float FIR_G[12] = { 0, 2, 8, 22, 43, 61, 61, 43, 22, 8, 2, 0 };
static const float FIR_B[12] = { 0, 0, 1, 5, 16, 36, 57, 64, 51, 28, 11, 3 };

/// 算出这一片元的**预乘**源色（不含任何混合）。
///
/// 抽出来是因为可分离 / 非可分离混合要与 `dst` 合成，而「算出源色」这部分
/// 与混合模式**完全无关**。共用一份才不会出现「某个混合模式下的圆角矩形与
/// 默认路径长得不一样」这类漂移。
float4 ShadeSource(VSOut i) {
    // 字形 LCD 分支需要「每像素的 uv 步长」。导数必须在分支**外**求：HLSL
    // 对非一致控制流里的 ddx 会报警，而字形那一支虽是逐绘制常量，编译器不知道。
    float duv_dx = ddx(i.uv.x);

    // kind == 5：**SVG 式像素偏移玻璃**（便宜版液态玻璃）。
    //
    // # 它为什么排第一
    //
    // 它要算 SDF（圆角矩形）⇒ 必须排在「多边形 / 图像」那两条**之前** ——
    // 那两条的判据是 `kind > 2.5` / `kind > 1.5`，而 5.0 两条都满足。
    //
    // # 配方照着 SVG 版液态玻璃抄（`index.css` 的 `.glassDiv` +
    // `index.html` 的 `liquid-glass-filter`），四层：
    //
    // | SVG 那层 | 值 | 这里对应什么 |
    // |---|---|---|
    // | `background-color: rgba(255,255,255,.06)` | 玻璃**体** | `tint`（6% 白） |
    // | `inset 0 0 20px -5px rgba(255,255,255,.45)` | 内侧白辉光 | `glow` |
    // | `spec_layer`（`generateSpecularMap`：贴边一条 × \|dot\|） | 高光 | `spec` |
    // | `feDisplacementMap` | 位移 | 位移**玻璃自己的着色**（见下） |
    // | `box-shadow: 0 4px 24px rgba(0,0,0,.18)` | 外投影 | 宿主那一侧的装饰 |
    //
    // ⚠ **位移的对象不同**：SVG 版被位移的是**背景**（`backdrop-filter` 的
    // SourceGraphic 是背景图），玻璃本体（tint / 辉光 / 高光）**不缩**。
    // 廉价路没有背景可采样 ⇒ 位移落到玻璃**自己的着色**上，而**轮廓不动**。
    //
    // ⚠⚠ 轮廓**不能**跟着位移缩：早先那版把形状整个向内推（`alpha` 用 `d2`），
    // 而玻璃的填充是透明的 ⇒ 缩出来的环里什么都没有 ⇒ 屏幕上就是一只
    // **空框**（按钮只剩一圈白描边，中间是背景）。现在轮廓由 `d` 定，
    // 位移只作用在**辉光的峰值位置**上 ⇒ 读起来是「玻璃的边往里弯」。
    //
    // ⇒ **没有背景副本、没有离屏 RT、没有分色采样** —— 整块玻璃只是主趟里的
    // 一个四边形。
    //
    // ⚠ `i.uv` 在这一支里**不是纹理坐标**，而是玻璃参数：
    // `uv.x` = 位移幅度（像素）、`uv.y` = 作用带宽度（像素）。
    // 两者都是**逐顶点常量**（四个角填同一个值）⇒ 插值后仍恒定。
    // 这不占任何新的顶点通道：圆角矩形的 `uv` 本来就空着。
    if (i.kind > 4.5) {
        float2 half_size = i.half_size;
        float2 p = i.local - half_size;
        float d = sd_round_rect(p, half_size, i.corner.x, i.shape);
        float amount = i.uv.x;
        float bezel = max(i.uv.y, 1e-3);

        // ---- 位移（廉价路的「像素偏移」）----
        //
        // ⚠ `amount` 是**绝对像素**：一颗 36px 高的按钮给 20px 就把形状吃掉了。
        // 夹到短边一半的 40%，保证任何尺寸的玻璃都还剩个形状。
        float short_half = min(half_size.x, half_size.y);
        float amt = min(amount, short_half * 0.4);

        // 位移带：边缘处最大，到带内侧归零（形状内部一点不动 ——
        // 实心图形的内部位移没有意义，只会把边推歪）。
        float band = 1.0 - smoothstep(0.0, bezel, abs(d));
        // 形状的**外法线**（`grad_sd_rounded_rect` 与 `PSLens` 用的是同一份）。
        float2 n = grad_sd_rounded_rect(p, half_size, i.corner.x, i.shape);
        // ⚠ 位移**向内**（`-n`）：向外推的话形状会变大，而四边形的几何是按
        // **原矩形**扩的（只扩了 AA 带）⇒ 向外推出去的部分会被裁掉，
        // 症状是「玻璃的角被削平」。向内推不需要扩几何。
        float2 off = -n * (amt * band);
        float d2 = sd_round_rect(p - off, half_size, i.corner.x, i.shape);

        float px = 0.75;
        // 轮廓：按**原形状** `d` —— 位移不改玻璃的外形（见上面的警告）。
        float alpha = 1.0 - smoothstep(-px, px, d);
        // 被位移的是**玻璃体**的边（`d2`）—— SVG 版动的是背景，廉价路没有背景，
        // ⇒ 动玻璃自己的体（见下面的 tint）。
        float alpha_tint = 1.0 - smoothstep(-px, px, d2);
        // 到边的距离（px，内为正）：`depth` 到位移**前**的边。
        float depth = -d;

        // ---- ① 玻璃体：6% 白（SVG `background-color: rgba(255,255,255,.06)`）----
        //
        // 它的**边**被位移（`alpha_tint` 走 `d2`）：玻璃越厚，体的边界越往里
        // 掰 —— 这就是廉价路的「像素偏移」。
        float a_fill = i.color.a * alpha;
        float3 rgb_fill = i.color.rgb * alpha;
        float a_tint = 0.06 * alpha_tint;
        float a_body = a_fill + a_tint * (1.0 - a_fill);
        float3 rgb_body = rgb_fill + float3(a_tint, a_tint, a_tint) * (1.0 - a_fill);

        // ---- ② 内侧白辉光：贴边的一条**薄**高光，不是一整圈奶白 ----
        //
        // ⚠ 旧值（带宽 24px、峰值 0.42）在 220×120 的玻璃块上是一圈 ~22px 宽、
        //   42% 白的**奶白带** —— 看起来像磨砂瓷砖，而不是原版液态玻璃那种
        //   「背景直直透过来、边缘一条亮线」的清玻璃。原版 `lens()` 根本没有这层
        //   大面积内辉光，光学带宽只有倒角那几像素。
        //
        // ⇒ 带宽随短边缩放但**封顶 10px**，峰值降到 0.16，峰值贴着轮廓线：
        //   只补一点内侧的亮度过渡，让亮线不显得是硬贴上去的描边。
        float gw = clamp(short_half * 2.0 * 0.05 + amt * 0.3, 2.0, 10.0);
        float gsigma = gw * 0.4;
        float gpos = gw * 0.15;
        float gg = (depth - gpos) / gsigma;
        float a_glow = exp(-gg * gg) * 0.16 * alpha;

        // ---- ③ 高光：SVG `spec_layer`（`generateSpecularMap`）----
        //
        // SVG 那份的剖面是 `sqrt(1 - (1 - f)^2)`（f = 到边的距离 / px），
        // 峰值**贴着轮廓线**、宽度 ~1px，强度再乘 `|dot|`（光向）。
        // ⇒ 这里也用一条**贴着边**的细线。
        // ⚠ 上一版用的是「带内侧最亮」的大面积高光（`sqrt(2t - t²)`），
        //   对 36px 的按钮等于**整颗刷白** —— 症状是「玻璃变成一块白板」。
        //
        // ⚠⚠ 这里必须是「**从边往里衰减**」：`1 - smoothstep(0, rim_w, depth)`。
        //   写成 `smoothstep(-rim_w, 0, depth)` 会得到**整片内部都是 1** ——
        //   那是「整颗按钮刷白 + 沿超椭圆法线翻转的那条对角线出现亮痕」
        //   的根源（`facing` 在 `max(q.x,q.y)` 的切换对角线上会跳变）。
        float rim_w = clamp(short_half * 0.09, 1.0, 2.0);
        float rim_p = 1.0 - smoothstep(0.0, rim_w, max(depth, 0.0));
        // 光从左上角来。用**法线**而不是位置，形状歪一点也还成立。
        // ⚠ 取**绝对值**：SVG 那份也是 `abs(dot)` —— 背光的那侧也有一层弱反光，
        // 这正是 iOS 液态玻璃的观感（不是只有一条亮线）。
        float2 light = normalize(float2(-0.6, -0.8));
        float facing = abs(dot(n, light));
        float a_spec = rim_p * facing * 0.5 * alpha;

        // ---- 合成（预乘；每层都是「白 + 自己的 alpha」）----
        //
        // ③ 压在玻璃体之上，④ 高光压在最上。
        float a_b2 = a_glow + a_body * (1.0 - a_glow);
        float3 rgb_b2 = float3(a_glow, a_glow, a_glow) + rgb_body * (1.0 - a_glow);
        float a = a_spec + a_b2 * (1.0 - a_spec);
        clip(a - 0.0039);
        float3 rgb = float3(a_spec, a_spec, a_spec) + rgb_b2 * (1.0 - a_spec);
        return float4(rgb, a);
    }

    // kind == 3：已 CPU 三角化的多边形。**不采样纹理**，直接返回顶点色。
    //
    // # 必须排在图像分支之前
    //
    // 图像分支的判据是 `kind > 1.5`，而 3 > 1.5 —— 若多边形分支排在其后，
    // 每个三角形都会去`g_tex.Sample()` 当前批次绑着的那张纹理。症状是多边形
    // 变成"图集/图像的内容"而非路径形状，且**换批次就换颜色**，看起来像随机
    // 噪声而不是错误 —— 这类症状极难回溯到`kind` 配错上。
    //
    // 阈值用 2.5 而不是 3.0：三个`kind` 靠"相邻且间距 < 1"区分，阈值取相邻
    // 两类的中点。写 3.0 的话 `> 3.0` 会把恰好等于 3.0 的多边形漏进图像分支。
    //
    // 抗锯齿：这里没有 `smoothstep`。三角化后的多边形没有解析距离场，
    // 覆盖率只能来自 MSAA 的多点采样 —— 见 `crates/zephyra/src/geom/mod.rs`
    // 的「抗锯齿不在这里」。
    if (i.kind > 2.5) {
        clip(i.color.a - 0.0039);
        return i.color;
    }

    // kind == 1：字形。采样方式由 `i.shape`（模式码，flat）决定：
    //   0 = 旧覆盖率（读 alpha）；1 = 单通道 SDF（读 red）；
    //   2 = MSDF（rgb 取中位数）；3/4 = MSDF + LCD 子像素（RGB/BGR）。
    //
    // `i.corner.x` 是排版端给出的平滑半宽 `w`（CPU 参考光栅用）；GPU 侧改用
    // `fwidth(MSDF)` 自适应半宽，任意字号/缩放都约 1px，且不会固定过宽发糊。
    // `i.corner.y` 是伪粗体阈值偏移（仅 MSDF/LCD 读）。
    if (i.kind > 0.5 && i.kind < 1.5) {
        float mode = i.shape;
        float w = i.corner.x;
        float emb = i.corner.y;
        float4 texel = g_tex.Sample(g_smp, i.uv);
        float coverage = 0.0;
        float3 out_rgb = i.color.rgb;
        // 顶点色是预乘的；gamma 校正需要文字本色（非预乘）。
        float3 src_rgb = i.color.rgb / max(i.color.a, 1e-4f);
        float src_lum = dot(src_rgb, float3(0.2126f, 0.7152f, 0.0722f));
        // LCD 分支把逐通道覆盖率直接乘进 RGB，末尾不能再乘一次标量 coverage。
        bool per_channel_rgb = false;

        // 屏幕空间导数决定过渡带宽度：一个屏幕像素对应多少 SDF 值域变化，
        // 就取半个像素作为 smoothstep 半宽。固定 `w` 在缩放/不同基准档下会
        // 偏宽（糊）或偏窄（锯齿）；fwidth 自适应，且放大时更锐利、
        // 缩小时自动加宽以避免闪烁。导数在分支外求，保证 uniform 控制流。
        float s_msdf = max(min(texel.r, texel.g), min(max(texel.r, texel.g), texel.b));
        float aa = clamp(0.4 * fwidth(s_msdf), 1e-5, 0.5);
        // CPU 参考光栅与无导数环境用 `w` 兜底。
        aa = max(aa, w * 0.15);

        if (mode < 0.5) {
            // 替身/降级：没有真实距离场，退回覆盖率位图。
            coverage = texel.a;
        } else if (mode < 1.5) {
            coverage = smoothstep(0.5 - aa, 0.5 + aa, texel.r);
        } else if (mode < 2.5) {
            coverage = smoothstep(0.5 - aa - emb, 0.5 + aa - emb, s_msdf);
        } else if (mode < 4.5) {
            // LCD：Skia 的 12-tap 高斯 FIR。BGR 把偏移方向反过来。
            // 直接在 MSDF 上按子像素位置取覆盖率，再用 R/G/B 三组权重卷积，
            // 比早先 3 点等权采样更接近 ClearType：彩边更少、细竖边更稳。
            float dir = (mode < 3.5) ? 1.0 : -1.0;
            float3 cov = 0;
            [unroll]
            for (int k = 0; k < 12; ++k) {
                float off = (float(k) - 5.5f) * 0.25f;
                float3 s3 = g_tex.Sample(g_smp, i.uv + float2(duv_dx * dir * off, 0.0)).rgb;
                float sd = max(min(s3.r, s3.g), min(max(s3.r, s3.g), s3.b));
                float c = smoothstep(0.5 - aa - emb, 0.5 + aa - emb, sd);
                cov.r += FIR_R[k] * (1.0f / 256.0f) * c;
                cov.g += FIR_G[k] * (1.0f / 256.0f) * c;
                cov.b += FIR_B[k] * (1.0f / 256.0f) * c;
            }
            cov = min(cov, 1.0f);
            cov = apply_text_contrast(cov);
            // 每个子像素按自身通道的文字色做 gamma 预扭曲。
            cov.r = gamma_correct_coverage(cov.r, src_rgb.r);
            cov.g = gamma_correct_coverage(cov.g, src_rgb.g);
            cov.b = gamma_correct_coverage(cov.b, src_rgb.b);
            out_rgb = i.color.rgb * cov;
            // alpha 的取值按 Skia：目标更不透明时用 min/max 中较保守的一个，
            // 避免半透明背景上的彩边。g_dst 未刷新时读到 alpha=0 ⇒ 退化为 max。
            float dst_a = g_dst.Sample(g_smp, DstUv(i)).a;
            float lo = min(cov.r, min(cov.g, cov.b));
            float hi = max(cov.r, max(cov.g, cov.b));
            coverage = (i.color.a - 1.0f) < dst_a ? lo : hi;
            per_channel_rgb = true;
        } else if (mode < 5.5) {
            // CBDT/CBLC 彩色位图：RGBA 已解码在图集里，alpha 即覆盖率；
            // RGB 先预乘自身 alpha，末尾不再乘标量 coverage。
            coverage = apply_text_contrast(texel.a);
            out_rgb = texel.rgb * coverage;
            per_channel_rgb = true;
        }

        // 灰度/单通道/多通道 SDF：对比度预扭曲 + 按文字亮度做 gamma 覆盖校正。
        // LCD 与彩色位图已在各自分支内处理（且它们各自的颜色不同，不能用文字色统一校正）。
        if (!per_channel_rgb) {
            coverage = apply_text_contrast(coverage);
            coverage = gamma_correct_coverage(coverage, src_lum);
        }

        float a = coverage * i.color.a;
        clip(a - 0.0039);
        // 预乘空间：非 LCD 路径 `i.color.rgb` 在顶点展开时已乘过 `i.color.a`，
        // 这里再乘一次覆盖率；LCD 路径 out_rgb 已含逐通道覆盖率，不可重复相乘。
        return per_channel_rgb ? float4(out_rgb, a) : float4(out_rgb * coverage, a);
    }

    // kind == 2：图像。采样当前绑定的纹理（每批一种）。
    if (i.kind > 1.5) {
        float4 texel = g_tex.Sample(g_smp, i.uv);
#ifdef ZEPHYRA_NV12
        // NV12 变体：把 (Y,U,V) 转成直通 RGB，alpha 恒 1（视频帧没有透明通道）。
        // 这段只编进 PSMainNv12 / PS*Nv12 变体；默认 PSMain 的字节码不受影响。
        texel = float4(nv12_to_rgb(texel), 1.0f);
#endif
        float a = texel.a * i.color.a;
        // tint 是**乘算**：图标要染色时用它，而不是改纹理内容。
        //
        // ⚠️ 圆角遮罩**必须**以 `corner.x > 0` 为条件，不能无条件施加。
        //
        // 圆角矩形的 SDF 在 `d == 0` 处正是矩形边界，于是
        // `1 - smoothstep(-px, px, 0) == 0.5` —— 无条件施加会把最外一圈像素压到
        // 50% alpha。症状是**图像四周发暗一圈**，而所有计数器正常。
        //
        // 直角图像四边形**没有**外扩（`quad.rs` 的 `pad` 对 `corner.x == 0` 取 0），
        // 所以那半边过渡带本来就不可见 —— 也就是说即使施加了也不会「更抗锯齿」，
        // 只会让边缘变暗。这条分支不是优化，是正确性。
        //
        // 带圆角时 `quad.rs` 已按 `AA_BAND` 外扩，并用 **uv 外插**保证图像内容
        // 不被裁掉；采样器的 Clamp 兜住外插出去的那一点点。
        if (i.corner.x > 0.0) {
            float2 half_size = i.half_size;
            float2 p = i.local - half_size;
            float d = sd_round_rect(p, half_size, i.corner.x, i.shape);
            float px_img = 0.75;
            a *= 1.0 - smoothstep(-px_img, px_img, d);
        }
        clip(a - 0.0039);
        // 预乘空间：采样到的图像纹理是**直通 alpha**（R8G8B8A8），所以 rgb 要先乘自己的
        // alpha；`i.color.rgb` 则已在顶点展开时乘过 tint 的 alpha（见 `quad.rs`）。
        return float4(texel.rgb * texel.a * i.color.rgb, a);
    }

    // kind == 0：SDF 圆角矩形（shape-aware, CSS corner-shape）。
    //
    // `p` 是「片元相对四边形中心的偏移」，`half_size` 是四边形的半宽半高 ——
    // **两者来自不同的顶点通道**，且都是常量/线性插值的正确量。
    float2 half_size = i.half_size;
    float2 p = i.local - half_size;
    float d = sd_round_rect(p, half_size, i.corner.x, i.shape);

    // 1 像素过渡带：抗锯齿。带宽不随缩放变化，所以高 DPI 下边缘不会变毛糙。
    float px = 0.75;
    float alpha = 1.0 - smoothstep(-px, px, d);
    clip(alpha - 0.0039);

    if (i.corner.y > 0.0) {
        // 描边：距边界 stroke 宽度以内的环带。
        //
        // ⚠ 这里**不能**写成 `1.0 - smoothstep(...)`。`d < 0` 在内部、`d == 0` 在边界，
        // 环带对应 `d ∈ [-width, 0]`；而 `smoothstep(-width-px, -width+px, d)` 在
        // `d == 0`（边界）处取 1、在 `d == -width`（环带内侧）处取 0 —— 正好是我们
        // 要的。再取一次 `1.0 -` 会把整条环带**翻过来**：越靠里越实、越靠边越透明，
        // 描边看起来变成一块实心填充。
        float inner = -i.corner.y;
        float sa = smoothstep(inner - px, inner + px, d);

        // 外侧**必须**再乘一道淡出：`d > 0` 在环带之外，而 smoothstep 在那里饱和为 1。
        // 四边形按 px（AA_BAND）向外扩过，所以缺这一道会让描边外侧多出一圈实色 ——
        // 症状是「描边比设定值粗一圈，且那一圈没有抗锯齿」。
        //
        // 注意它**只**乘在描边分支上：填充分支的 `alpha` 本身已经含外侧淡出，
        // 再乘一次会让填充边缘被淡两次（更瘦）。
        float outerFade = 1.0 - smoothstep(-px, px, d);
        sa *= outerFade;

        clip(sa - 0.0039);
        return float4(i.color.rgb * sa, i.color.a * sa);
    }
    return float4(i.color.rgb * alpha, i.color.a * alpha);
}

/// 默认路径：源色交给**固定功能混合方程**（`SrcOver`，或 `runs` 段指定的系数对）。
///
/// 系数可表达的那 15 个模式都用这一个 PS —— 混合因子在 `ID3D11BlendState` /
/// PSO 的 `BlendState` 里。见 `renderer.rs` 的 `blends` 表。
float4 PSMain(VSOut i) : SV_TARGET {
    return ShadeSource(i);
}