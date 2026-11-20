
// ═══════════════════════════════════════════════════════════════════════════
//  折射（液态玻璃）
// ═══════════════════════════════════════════════════════════════════════════
//
// # 与模糊的本质区别
//
// 模糊读 `g_tex`（离屏 RT，里面**只有这个元素自己**）；折射读 `g_dst`
// （目标副本 = 元素**背后**已经画在屏幕上的画面）。两者是不同的纹理，
// 所以本入口是**合成期**的：它同时拿到两者。
//
// # 顶点字段约定（与 `PSBlurH` / `PSComposite` 不同，见 `offscreen.rs` 的表）
//
// | 字段       | 含义 |
// |------------|------|
// | `local`    | **元素内**的像素坐标 |
// | `half_size`| **元素**的半宽半高（`nointerpolation`）|
// | `uv`       | 元素内归一化坐标 |
// | `corner.x` | 圆角半径 |
// | `corner.y` | 玻璃厚度（**像素**）|
// | `color.r`  | 色散量 |
// | `color.g`  | 倒角带宽度（像素）|
// | `color.b`  | 折射率 IOR |
//
// 屏幕 uv 走 `DstUv(i)`（从 `ndc` 推），不占顶点字段。

/// 玻璃的**高度剖面**：`t` = 倒角带里的归一化深度。
///
/// # 为什么是这个形状
///
/// 它是一条**圆拱的四次方根**：`t = 0` 在边缘（高度 0），`t = 1` 在带内侧（高度 1）。
///
/// 关键在它在 `t → 0` 处**斜率发散**—— 那正是「光在边缘斜着进出」的数学表达。
/// 若改成线性减少，边缘就会变成一条平滑地推进去的带子——那是「模糊边」，
/// 不是玻璃。
float glass_surface_height(float t) {
    float s = 1.0 - t;
    return pow(1.0 - s * s * s * s, 0.25);
}

/// 圆角矩形 SDF 的**单位外法线**。
///
/// # 为什么用有限差分而不是解析梯度
///
/// 圆角矩形的解析梯度要在「直边区」与「圆角区」之间分支，而两段的公式不同。
/// 分支写错时**不会报错**，只会让法线在某个区域偏转 90° —— 症状是玻璃的
/// 某一侧不折射、另一侧折射方向相反，看起来像"效果只做了一半"。
///
/// 有限差分（4 次 SDF 求值）没有分支，也就没有这类错。代价可以忽略：
/// 本像素着色器里 SDF 求值本来就被 `PSMain` 每片元调一次。
float2 SdNormal(float2 p, float2 half_size, float radius, float shape_k) {
    // 步长取 0.75，与 `quad::AA_BAND` 同量级 —— 再小会被浮点噪声淹没，
    // 再大在 1px 宽的边缘上会跨过整个直边区，梯度反而失真。
    const float e = 0.75;
    float2 g;
    g.x = sd_round_rect(p + float2(e, 0.0), half_size, radius, shape_k)
        - sd_round_rect(p - float2(e, 0.0), half_size, radius, shape_k);
    g.y = sd_round_rect(p + float2(0.0, e), half_size, radius, shape_k)
        - sd_round_rect(p - float2(0.0, e), half_size, radius, shape_k);
    float len = length(g);
    // 元素**正中心**处 SDF 的梯度确实为 0（对称）。那不是错误 ——
    // 中心本来就没有"边缘方向"。返回 0 让那里的位移为零，正是想要的。
    return (len > 1e-5) ? (g / len) : float2(0.0, 0.0);
}
float2 SdNormal(float2 p, float2 half_size, float radius) {
    return SdNormal(p, half_size, radius, 1.0);
}

/// 折射合成：把元素**背后**的画面按边缘法线推开后显示出来。
///
/// ⚠ 输出的是**被扰动的背景**，不是这个元素本身 —— 所以宿主必须在这趟
/// 之后再把原命令照常画一遍（`OffscreenPass::redraw_original`）。漏了那一步
/// 的症状是"玻璃上的描边和文字消失了"，而不是"背景没被扰动"。
float4 PSRefraction(VSOut i) : SV_TARGET {
    float radius   = i.corner.x;
    float strength = i.corner.y;   // 玻璃厚度（像素）
    float dispersion = i.color.r;  // 色散量
    // `bezel`（倒角带宽）与 `ior`（折射率）直接从 `i.color` 取，
    // 不抽成局部变量：各用一次，局部变量只会多一份拷贝阅读。

    // 元素内坐标 → SDF（矩形内为负）。
    float2 p = i.local - i.half_size;
    float d = sd_round_rect(p, i.half_size, radius, i.shape);

    // ⚠ 元素**外**（d > 0）必须完全不输出：那里的背景没有被任何东西遮挡，
    // 位移它只会凭空撕裂画面。遮罩取 `saturate(-d / edge)`。
    //
    // ⚠⚠ `edge` 必须是**极窄**的一条（1px 量级的抗锯齿过渡带），**不能**按
    // 元素尺寸取（`min(half_size) * 0.5` 那一版）。原因：遮罩是**乘**在位移上的，
    // 而位移最大的地方恰好是 `d → 0`（边缘）—— 那一版遮罩在边缘正好是 0，
    // 于是「最强的折射」被「最弱的遮罩」乘成 0。
    //
    // 实测（默认参数 thickness=28 / bezel=26 / ior=1.50，220×120 的玻璃块）：
    // 宽遮罩那一版的**可见**位移峰值只有 ~2px（乘积），1px 窄遮罩之后
    // 同一处是 ~13px。差 6 倍 —— 而这正是「折射看起来没生效」的主因。
    //
    // 抗锯齿只需要 1px：元素外那一圈本来就不该有输出，1px 的淡出足够
    // 让边缘不锯齿，而不会把整个倒角带一起压平。
    float edge = 1.0;
    float mask = saturate(-d / edge);

    float2 n = SdNormal(p, i.half_size, radius, i.shape);

    // 像素位移 → uv 位移。屏幕尺寸走 `ddx/ddy`，**不占顶点字段** ——
    // 加一个字段会让 `Vertex` 从 52 涨到 56 字节，进而要同步改两个后端的
    // `VERTEX_LAYOUT` 与着色器语义分配（后者有索引重叠陷阱，见 `VSIn` 注释）。
    //
    // `DstUv` 对 NDC 是**线性**的，所以它的逐像素增量就是"一个像素多少 uv"。
    // 这比传屏幕尺寸更稳：不需要调用方保证传进来的尺寸与实际一致，
    // 而那种不一致的症状是折射整体错位，极难归因。
    float2 uv = DstUv(i);
    float2 px2uv = float2(ddx(uv.x), ddy(uv.y));

    // ---- 位移：斯涅尔定律 ----
    //
    // 三步：① 深度归一化 `t`（只在倒角带内 0..1）→ ② 由高度剖面求斜率
    // → ③ 入射角与斯涅尔角的**差**。
    //
    // # 为什么不是「强度 × 衰减」
    //
    // 那个形式给出的是一条**单调**曲线：边缘与内部的弯曲量只差一个系数。
    // 而真实玻璃的弯曲在「新的入射角减旧的入射角」时形成，所以它在深度上
    // 是**先增后减**的：近边缘几乎不弯，倒角带中段最弯，到带内侧又回到 0。
    //
    // 颜色参这一点在下面的 IOR：`IOR = 1` 时 `Θr ≡ Θs`，偏折角为 0 ⇒ 位移恰好为 0。
    //
    // ⚠⚠ 倒角带宽度**不按圆角半径夹**。物理上一块玻璃的边缘是「倒角平面 +
    // 圆角」，倒角面（平的那一圈）比圆角**宽**是正常的；而把 `bezel` 夹到
    // `radius` 会让玻璃块上**整个倒角参数的上 3/4 变成空档**（默认玻璃的
    // 圆角半径只有 12px，而倒角带宽的表到 48px）—— 参数面板上「调大了没反应」。
    // 只按半边长夹：那才是「倒角带不能宽过整个元素」这条真正的约束。
    float bezel = min(max(i.color.g, 1.0), min(i.half_size.x, i.half_size.y));
    float t = saturate((-d) / bezel);

    // 斜率用**有限差分**：高度剖面在 t→0 处的导数无界，解析式开方会在
    // 那里发散成 NaN，而 NaN 会把整个像素变成不可见的黑点。
    const float dt = 0.001;
    float h = glass_surface_height(t);
    float dh = (glass_surface_height(saturate(t + dt)) - h) / dt;

    float slope = atan(dh * (strength / bezel));
    float sin_r = clamp(sin(slope) / max(i.color.b, 1e-3), -1.0, 1.0);
    float theta_r = asin(sin_r);
    // ---- 位移量：楔形棱镜的偏折 ----
    //
    // ⚠⚠ 这里**不能**用薄棱镜近似 `h * (tan(Θs) - tan(Θr))`。
    //
    // 那条式子只在**小角度**成立，而这里的 `Θs` 常常接近 90°（高度剖面在
    // `t → 0` 处斜率发散），于是 `tan(Θs)` 发散：**实测**默认参数下
    // 位移峰值是 **491px**（最大参数下 2181px）—— 而「位移不超过玻璃厚度」
    // 这条契约的标称值是 `thickness`（28px）。后果是玻璃边缘把屏幕上
    // 几百像素外的内容拉进来，看着像「把画面揉了一团」而不是玻璃。
    //
    // 用**有界**的楔形偏折式：棱镜高 `h`、入射角 `Θs`、折射角 `Θr` 时，
    // 出射方向相对入射方向的偏折角是 `Δ = Θs - Θr`，而横向位移是
    //
    //     δ = h * sin(Δ) / cos(Θr)
    //
    // 这个式子在 `Θs → 90°` 时趋于 `h`（而不是 ∞），并且**恒有 δ ≤ h** ——
    // 于是「位移不超过玻璃厚度」这条契约**由公式本身保证**，而不是靠离屏
    // 外扩去追。`IOR = 1` 时 `Δ = 0` ⇒ `δ = 0`（不折射）这条性质也仍然成立。
    //
    // 逐字对应：`zephyra-core/src/filter.rs` 的 `Filter::Refraction::outsets`
    // （外扩 = 厚度 = δ 的上界）与 `crates/base/zephyra-winit-d3d12/tests/` 下
    // 的折射门禁。改这里必须同步改那两处 —— 两处不是两份真相，是**一条契约
    // 的两端**。
    float displacement = h * strength * (sin(slope - theta_r) / max(cos(theta_r), 1e-3));

    // 方向 = **-**SDF 外法线：透射取的是「背后那个点的光」，所以位移与外法线相反。
    //
    // 写正的病状是“对称场景里看不出来”：左右两侧拉得一样长，而真实错误只在
    // 非对称图形上才现。
    float2 off_uv = -n * displacement * px2uv;

    // 色散：R / B 相对 G 沿法线拉开不同比例，于是边缘出现彩色描边 ——
    // 真实玻璃的色散。`dispersion = 0` 时三条通道偏移相同，等于无色散。
    float2 uvR = saturate(uv + off_uv * (1.0 + dispersion));
    float2 uvG = saturate(uv + off_uv);
    float2 uvB = saturate(uv + off_uv * (1.0 - dispersion));

    float4 dG = g_dst.SampleLevel(g_smp, uvG, 0);
    float3 bg = float3(
        g_dst.SampleLevel(g_smp, uvR, 0).r,
        dG.g,
        g_dst.SampleLevel(g_smp, uvB, 0).b
    );

    // `g_dst` 是预乘的，所以覆盖率取 alpha，rgb 不再乘 —— 与 `PSComposite`
    // 透传分支同一纪律。
    float a = dG.a * mask;
    return float4(bg * a, a);
}
