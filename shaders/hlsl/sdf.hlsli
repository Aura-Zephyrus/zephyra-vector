
// 圆角矩形的有符号距离：矩形内为负，外部为正。
// shape_k = CSS corner-shape / superellipse(K) 的 K（1=round, 2=squircle）。
//
// # 为什么超椭圆那一支要单独两个函数
//
// 超椭圆的定义是 `|x/a|^n + |y/a|^n = 1`，`n = 2K`。写成
// `pow(pow(ax, n) + pow(ay, n), 1/n)` 时，**`n` 是运行期变量**
// （`shape_k` 是顶点插值出来的），编译器**没法**把它折成乘法链 ——
// 于是每个片元要付 **3 次完整的 `pow`**（每次 ≈ log2 + exp2 + 一次乘）。
//
// 样式表里有 **139 处** `corner-shape: superellipse(1.5)`（也就是 n=3），
// 也就是说**几乎每个面板、每个按钮的每个片元**都在付这 3 次 `pow`。
//
// 实测（gallery，1400×800，Intel UHD 630）：整帧光栅化 **3.0 ms** ——
// 而「n 取整」时可以用乘法链 + 一次开根代替：
//
// | n | `pow(x, n)` | 根 |
// |---|---|---|
// | 2 | `x*x` | `sqrt` |
// | 3 | `x*x*x` | `pow(x, 1/3)` |
// | 4 | `x*x*x*x` | `sqrt(sqrt)` |
//
// ⇒ 3 次 `pow`（≈ 9 次超越函数运算）降到 **1 次**。
//
// ⚠ 分支是**逐绘制**一致的：`shape_k` 标了 `nointerpolation`，
// 整趟绘制里它是常量 ⇒ 分支在 warp 内不发散。
// ⚠ 非整数 n 仍走 `pow`（样式表里目前没有，但接口留着）。
// ⚠ HLSL 没有 `cbrt`（`ps_5_0` 未声明），n=3 的根仍用 `pow` —— 但只
// **一次**，而原来是三次。
float SuperellipsePow(float x, float n) {
    if (abs(n - 3.0) < 1e-3) { float x2 = x * x; return x2 * x; }
    if (abs(n - 4.0) < 1e-3) { float x2 = x * x; return x2 * x2; }
    if (abs(n - 2.0) < 1e-3) { return x * x; }
    return pow(x, n);
}

float SuperellipseRoot(float x, float n) {
    if (abs(n - 4.0) < 1e-3) { return sqrt(sqrt(x)); }
    if (abs(n - 2.0) < 1e-3) { return sqrt(x); }
    return pow(x, 1.0 / n);
}

float sd_round_rect(float2 p, float2 half_size, float radius, float shape_k) {
    float r = min(radius, min(half_size.x, half_size.y));
    if (r <= 0.0 || shape_k > 1e6) {
        float2 q = abs(p) - half_size;
        return length(max(q, float2(0.0, 0.0))) + min(max(q.x, q.y), 0.0);
    }
    float2 q = abs(p) - half_size + float2(r, r);
    float qx = max(q.x, 0.0);
    float qy = max(q.y, 0.0);
    float inside = min(max(q.x, q.y), 0.0);
    float k = shape_k;
    // round（与历史公式一致）
    if (abs(k - 1.0) < 1e-4) {
        return length(float2(qx, qy)) + inside - r;
    }
    // bevel (K=0)
    if (abs(k) < 1e-4) {
        return (qx + qy) + inside - r;
    }
    if (k > 0.0) {
        // 凸超椭圆（squircle K=2 → n=4）：L^n
        float n = 2.0 * k;
        float ax = max(qx / r, 0.0);
        float ay = max(qy / r, 0.0);
        float corner_d = 0.0;
        if (ax > 0.0 || ay > 0.0) {
            corner_d = r * SuperellipseRoot(SuperellipsePow(max(ax, 1e-7), n) + SuperellipsePow(max(ay, 1e-7), n), n);
        }
        return corner_d + inside - r;
    }
    // scoop/notch（凹形）近似
    if (k < -1e6) {
        float d_outer = length(float2(qx, qy)) + inside - r;
        if (qx > 0.0 && qy > 0.0) {
            float notch = -min(q.x, q.y);
            return max(notch, d_outer);
        }
        return d_outer;
    }
    float n2 = 2.0 * (-k);
    float ax2 = max(qx / r, 0.0);
    float ay2 = max(qy / r, 0.0);
    float convex = 0.0;
    if (ax2 > 0.0 || ay2 > 0.0) {
        convex = SuperellipseRoot(SuperellipsePow(max(ax2, 1e-7), n2) + SuperellipsePow(max(ay2, 1e-7), n2), n2);
    }
    float scoop_d = r * (1.0 - convex);
    float base = length(float2(qx, qy)) + inside - r;
    if (qx > 0.0 && qy > 0.0) {
        return max(scoop_d, base);
    }
    return base;
}

// 兼容旧调用点（默认 round）。
float sd_round_rect(float2 p, float2 half_size, float radius) {
    return sd_round_rect(p, half_size, radius, 1.0);
}

// 前向声明：`PSHighlight` 用 `grad_sd_rounded_rect`，而它的定义与 `PSLens`
// 住在一起（下方「透镜」那一节）。HLSL 允许原型，WGSL 不允许 —— 那边把定义
// 挪到了 `fs_highlight` 之前。
float2 grad_sd_rounded_rect(float2 coord, float2 half_size, float radius, float shape_k);
float2 grad_sd_rounded_rect(float2 coord, float2 half_size, float radius);