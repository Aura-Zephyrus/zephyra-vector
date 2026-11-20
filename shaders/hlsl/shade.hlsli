
/// 算出这一片元的**预乘**源色（不含任何混合）。
///
/// 抽出来是因为可分离 / 非可分离混合要与 `dst` 合成，而「算出源色」这部分
/// 与混合模式**完全无关**。共用一份才不会出现「某个混合模式下的圆角矩形与
/// 默认路径长得不一样」这类漂移。
float4 ShadeSource(VSOut i) {
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

    // kind == 1：字形。图集存的是有符号距离场（SDF），覆盖率由 smoothstep 重建。
    //
    // `i.corner.x` 是 smoothstep 在 SDF 值域上的半宽 `w`，由排版端按当前显示字号
    // 算好：`w = AA_BAND / (2 · spread · (显示字号 / 基准字号))`。字号越大 w 越小，
    // 于是过渡带在屏幕空间恒为约 1px —— 这才是「任意字号都锐利」的根据。
    //
    // ⚠ 读 **red** 通道而不是 alpha：图集四通道都写了同一份 SDF，但 alpha 在
    // 旧 coverage 路径里另有含义；CPU 参考光栅（`cpu_raster.rs`）读同一个通道。
    if (i.kind > 0.5 && i.kind < 1.5) {
        float4 texel = g_tex.Sample(g_smp, i.uv);
        float w = i.corner.x;
        // w == 0 是替身/降级路径（没有真实 SDF 图集）：退回旧的 alpha 覆盖率。
        float coverage = (w > 0.0)
            ? smoothstep(0.5 - w, 0.5 + w, texel.r)
            : texel.a;
        float a = coverage * i.color.a;
        clip(a - 0.0039);
        // 预乘空间：`i.color.rgb` 在顶点展开时已乘过 `i.color.a`，这里再乘一次
        // 覆盖率；漏掉会让 rgb 与 a 不同步。
        return float4(i.color.rgb * coverage, a);
    }

    // kind == 2：图像。采样当前绑定的纹理（每批一种）。
    if (i.kind > 1.5) {
        float4 texel = g_tex.Sample(g_smp, i.uv);
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