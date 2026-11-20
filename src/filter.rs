//! 效果契约：滤镜（filter）与混合模式（blend）——**纯描述，零依赖，不碰任何系统 API**。
//!
//! # 为什么效果在这里，而不在GPU 层
//!
//! Skia 把效果切在 [`include/effects`](https://skia.googlesource.com/skia/+/refs/heads/main/include/effects/)
//! ——与 `include/core` **并列**的独立层。构建依赖是单向的：`include/effects` 依赖
//! `//src/core`，反向零命中。所以它的切法是：
//!
//! > 效果的**描述**是纯数据（只用 `SkScalar` / `SkPath` 这类 core 词汇表），
//! > 效果的**执行**永远在 GPU 侧（`src/gpu/ganesh/ops/`）。
//!
//! 这条边界正是 [`crate::paint::DisplayList`] 契约成立的前提：效果一旦降级成
//! DisplayList 里的纯字段，换一个 base 实现（DXGI ↔ DirectComposition，或
//! Windows ↔ Wayland）产出的 DisplayList 必然逐字节相同 —— 因为效果从没
//! 参与过"画什么"的决策，它只是"画的东西要多经过一道什么处理"的声明。
//!
//! # 为什么**不**用 `PushLayer` / `PopLayer` 配对命令
//!
//! 这是本模块最容易被写错的地方，而DisplayList 的结构直接否掉了那条路：
//!
//! `DisplayList` 构造时即按 `(layer, z, order)` **稳定排序**（见
//! [`crate::paint`]）。配对命令靠"在有序流里前后夹住内容"生效，可排序会把
//! 中间内容插到别的 `layer` 上去 —— `PushLayer(Content)` 包住的命令可能被排到
//! `Popup` 之后，配对当场失效，且**没有任何报错**。
//!
//! Skia 也没有这个问题，因为它压根不用配对命令：滤镜挂在 `SkPaint` 上，离屏由
//! `SkCanvas::aboutToDraw` → `AutoLayerForImageFilter`（`SkCanvasPriv.h:129`）
//! 在**回放时按需自动开** —— 有 imageFilter 才 `addImageFilterLayer`
//! (`SkCanvasPriv.cpp:234`)。命令流里没有 save/restore 标记。
//!
//! 所以这里沿用同一思路：[`crate::paint::Command`] 各变体**自己**带
//! `filter`与 `blend` 字段，回放方看到非恒等的 filter 才去开离屏。
//!
//! # 离屏边界由谁算
//!
//! 由**滤镜自己**给出，而不是录进命令流。Skia 走
//! `paint.computeFastBounds(drawBounds)`（`SkCanvasPriv.cpp:308`），本质是滤镜
//! 贡献一份 outset。本模块用 [`Filter::outsets`] 做同一件事。
//!
//! ⚠ 那条 outset 与四边形 SDF 的抗锯齿余量（`AA_BAND = 0.75`）
//! 是**两套独立依据**，不要混为一谈：
//!
//! | | 谁贡献 | 值 | 挡什么 |
//! |---|---|---|---|
//! | `AA_BAND` | 着色器 SDF | 固定 0.75px | 边缘一个像素的抗锯齿过渡带 |
//! | [`Filter::outsets`] | 滤镜半径 | `3σ`（blur）/ `offset + 3σ`（shadow） | 模糊/阴影溢出原矩形的那部分 |

use crate::core::{Color, Point, Rect};

/// 高斯模糊的标准差 → 实际半径的倍数。
///
/// 3σ 覆盖 99.7% 的高斯能量，所以 blur 的离屏矩形按`3σ` 外扩后，
/// 边界外残留的可见强度低于 0.3%，肉眼与像素断言都不可见。
///
/// 与 Skia 一致：`SkImageFilters::Blur` 内部走 `SigmaToRadius(sigma, scale)`，
/// CPU 侧系数为 3（GPU 侧因线性空间放大而略有差异）。
pub const SIGMA_TO_RADIUS: f32 = 3.0;

/// 一个图像滤镜。
///
/// 只列 UI 真正需要的两种。Skia 有二十多种（displacement / morphology /
/// turbulence / lighting …），但它们要么需要额外资源，要么对UI 没有可感知的
/// 收益 —— 缺了可以后加，加这个枚举不影响任何契约。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Filter {
    /// 各向异性高斯模糊。`sigma` 是**标准差**（不是半径），单位像素。
    ///
    /// 两个 σ 分开给，是因为 UI 里水平方向的模糊（文字选中态的边缘柔化）
    /// 与垂直方向的模糊需求常常不同。
    Blur { sigma_x: f32, sigma_y: f32 },
    /// 外投影。
    ///
    /// 阴影画在**原图之外**，所以离屏矩形必须按 `offset + 3σ` 外扩；若
    /// `offset` 为负（向左上投影），矩形向左上扩。
    DropShadow {
        color: Color,
        offset: Point,
        sigma: f32,
    },
    /// **SVG 式像素偏移玻璃** —— 便宜版液态玻璃。
    ///
    /// # 它替代的是什么
    ///
    /// [`Filter::Refraction`]（真实液态玻璃）要做两件事，而**两件都很贵**：
    ///
    /// 1. 把元素**背后**的画面拷一份（`g_dst`）—— MSAA 开着时那是一次
    ///    **整幅解析**（D3D12 没有「按区解析」），而**每个**带滤镜的元素
    ///    都要拷一次；
    /// 2. 按**斯涅尔定律**算出折射位移，再**逐通道分色**地采样那张副本
    ///    （色散 7 次采样）。
    ///
    /// 实测（gallery 1400×800 / Intel UHD 630 / D3D12）：材质页一帧 7 个带
    /// 滤镜的元素 ⇒ **玻璃税 8.4 ms/帧**（占那一帧 GPU 开销的 74%），其中
    /// 目标副本 5.15 ms、滤镜着色器 1.60 ms。
    ///
    /// # 这个变体怎么做
    ///
    /// 照 **SVG 的做法**（`feDisplacementMap` + `feSpecularLighting`）：
    ///
    /// - **位移图来自元素自己的形状**（SDF 的梯度），**不采样背景**；
    /// - 于是**不需要背景副本**，也就不需要离屏 pass —— 整块玻璃只是主趟里
    ///   的**一个四边形**；
    /// - 边缘的高光（`feSpecularLighting` 那一步）由 SDF 的法线**解析**算出。
    ///
    /// ⇒ 代价从「每元素一次全屏拷贝 + 离屏 RT + 7 次分色采样」降到
    /// 「主趟里多一个四边形」。
    ///
    /// # 参数
    ///
    /// - `amount`：边缘处形状被推开的最大距离（**像素**）。
    /// - `bezel`：位移只作用在**靠边**的一条带上的宽度（像素）；带内用
    ///   平滑过渡，形状内部**一点不动**（内部的位移对实心图形没有意义，
    ///   只会把边推歪）。
    ///
    /// # 与 [`Filter::Refraction`] 的关系
    ///
    /// **不是替代，是取舍**：物理折射把**背景**掰弯，看得见「后面有什么」；
    /// 像素偏移把**形状自己的边**掰弯，看得见「这是一块有厚度的玻璃」。
    /// 后者便宜两个数量级，代价是**背景不再被折射**。
    ///
    /// 真实的液态玻璃那条路**完整保留**（[`Filter::Refraction`] +
    /// [`Material::GlassRefract`]），要就用它。
    PixelOffset {
        /// 边缘位移的幅度（像素）。
        amount: f32,
        /// 位移作用带的宽度（像素）。
        bezel: f32,
    },
    /// 折射（"液态玻璃"）：把元素**背后**的画面按边缘法线推开再显示。
    ///
    /// # 与 [`Filter::Blur`] 的根本区别
    ///
    /// `Blur` 模糊的是元素**自己**的离屏内容（RT_A 里只有它一条命令）；
    /// 折射模糊/位移的是元素**背后**已经画在屏幕上的东西 —— 也就是
    /// `g_dst` 那张目标副本。两者读的是**不同的纹理**，所以它们的离屏
    /// 内容不能互换。这也是本变体存在的全部理由：没有"读背景"这个动作，
    /// 液态玻璃做不出来。
    ///
    /// # 参数
    ///
    /// - `strength`：折射强度，**像素**。边缘处背景被推开的最大距离。
    /// - `dispersion`：色散量（0..1 常用）。>0 时 R / B 通道相对 G 通道
    ///   沿法线方向偏移不同的量，于是边缘出现彩色描边 —— 真实玻璃的色散。
    ///   0 = 单通道位移，无色散。
    ///
    /// # 为什么 `redraw_original` 必须是 `true`
    ///
    /// 合成阶段输出的是**被扰动的背景**，不是这个元素本身。元素自己的内容
    /// （描边、文字、图标）要在那之后再照常画一遍 —— 与 `DropShadow`
    /// 同一条理由：影子/折射都是**额外**的一层。
    Refraction {
        /// 玻璃**厚度**（像素）。
        ///
        /// # 语义
        ///
        /// 它不是「位移多少像素」，而是**光学厚度** —— 位移由它与
        /// `bezel`（同一个枚举的字段）一起经斯涅尔定律算出来（见
        /// `zephyra.hlsl` 的 `PSRefraction`）。于是同一个 `thickness` 在
        /// 厚玻璃（倒角窄）上弯得多、在薄玻璃（倒角宽）上弯得少，这与
        /// 「位移 = 强度 × 系数」的线性手感是两回事。
        ///
        /// # 它也是**位移的上界**
        ///
        /// 着色器里的位移是 `δ = h · thickness · sin(Δ) / cos(Θr)`，
        /// 其中 `h ∈ [0, 1]` 是高度剖面、`Δ = Θs - Θr` 是偏折角。
        /// 而 `sin(Δ)/cos(Θr) = sin(Θs) - cos(Θs)·tan(Θr) ≤ 1`
        /// （`Θs → 90°` 时取等号）—— 于是 **δ ≤ thickness 恒成立**。
        ///
        /// 这条不等式是**契约**：离屏外扩、玻璃边缘的采样范围、参数面板上
        /// 「厚度 = 位移上限」这三件事都按它推导。改着色器里的位移式子时
        /// 必须重新验它（`refraction_displacement_is_bounded_by_the_thickness`）。
        strength: f32,
        /// 倒角带**宽度**（像素）：折射只发生在这条带子里。
        ///
        /// # 为什么必须有它
        ///
        /// 没有倒角带的概念时，位移只能按「到边缘的距离」单调衰减，于是元素
        /// **整块**都在被弯、只是越靠里越弱 —— 看起来像「把画面整体揉了一下」，
        /// 而不是玻璃。真实的玻璃只在边缘那一圈弯（光在那里斜着进出），
        /// 中间是直的。
        bezel: f32,
        /// 色散量（0..1 常用，`0` = R / B 通道相对 G 通道沿法线方向偏移不同的量，
        /// 于是边缘出现彩色描边 —— 真实玻璃的色散。`0` = 单通道位移，无色散）。
        dispersion: f32,
        /// **折射率**（IOR）。1.0 = 没有折射；普通冕牌玻璃约 1.5。
        ///
        /// # 它为什么不是常量
        ///
        /// 它是整条位移公式里唯一让「同一块玻璃看起来厚还是薄」的量：
        /// IOR 越大，光线偏折越小，玻璃越「硬」。真实取值在 1.0–2.2 之间，
        /// 超出这个区间就不再像玻璃，所以给了上界。
        ior: f32,
    },
    /// 透镜（原版 `lens()`）：把元素**背后**的画面按边缘法线推开再显示。
    ///
    /// # 与 [`Filter::Refraction`] 的关系
    ///
    /// `Refraction` 是本仓为玻璃块总结的「斯涅尔公式」实现；本变体是
    /// AndroidLiquidGlass（`kyant/backdrop`, Apache-2.0）里 `liquid toggle`
    /// 实际使用的 `lens()` 的**逐字转译**——`RoundedRectRefractionWithDispersionShaderString`：
    /// `circleMap(1 - -sd / refraction_height) · refraction_amount` 位移剖面、
    /// `gradSdRoundedRect`（`min(radius * 1.5, min(halfSize))` 的倒角带）、
    /// 7 通道分色（R/O/Y/G/C/B/P，按 `(cx·cy)/(hx·hy)` 强度缩放）。
    ///
    /// # 为什么加变体而不是改 `Refraction`
    ///
    /// 材质页与 `glass` 材质建立在 `Refraction` 之上（它的
    /// `refraction_displacement_is_bounded_by_the_thickness` 门禁按斯涅尔式推导），
    /// 换公式会波及所有玻璃块。本变体只给液态开关把手用，语义完全照搬原版：
    ///
    /// - `refractionHeight <= 0` 或 `refractionAmount <= 0` → 不产生 pass（原版
    ///   直接 `return`），由 [`Filter::is_identity`] 落实；
    /// - `padding > 0` 时 `padding -= refractionHeight`（clamp ≥ 0）——本仓 padding
    ///   恒为 0（`BackdropBlur` 不设 padding），所以 offset 恒 0，不单独占字段；
    /// - `chromaticAberration = true` 恒为 true（液态开关写死），7 通道全部参与。
    Lens {
        /// 倒角带宽度**（元素内单位）**：`-sd < refraction_height` 才参与，带外原样返回。
        refraction_height: f32,
        /// 位移量（像素）：带内边缘方向最大 `refraction_amount`。
        refraction_amount: f32,
    },
    /// 背景模糊（磨砂玻璃）：把元素**背后**已经画在屏幕上的画面模糊后再透出。
    ///
    /// # 与 [`Filter::Blur`] / [`Filter::Refraction`] 的关系
    ///
    /// - [`Filter::Blur`] 模糊的是元素**自己**的离屏内容（读 `g_tex`）；
    ///   本变体模糊的是**背后**的画面（读 `g_dst`）。两者读的纹理不同 ——
    ///   这与折射（[`Filter::Refraction`]）和 blur 的差别是同一个。
    /// - [`Filter::Refraction`] 把背后画面**位移**（斯涅尔折射），本变体把背后
    ///   画面**模糊**。磨砂玻璃（frosted）与液态玻璃（liquid glass）是两种
    ///   材质 —— 前者软化背景、后者弯曲背景。
    ///
    /// # 为什么 σ 用一个而不是两个
    ///
    /// 磨砂玻璃的模糊是**各向同性**的（磨砂没有方向），所以只给一个 σ。
    /// [`Filter::Blur`] 留两个是因为文字选区的水平/垂直柔化需求不同 ——
    /// 那是"柔化元素自身"，不是"软化背景"。
    ///
    /// # 与 `redraw_original` 的关系
    ///
    /// 与折射同一条：合成阶段输出的是**被模糊的背景**，不是元素自己 ——
    /// 所以元素自己的描边 / 文字 / 图标必须在合成**之后**再画一遍。
    BackdropBlur { sigma: f32 },
}

impl Filter {
    /// 本滤镜要求离屏矩形向外扩多少（每边，单位像素）。
    ///
    /// 多个滤镜串联时取**逐边最大值**，不是求和 —— 见 [`FilterChain::outsets`]。
    ///
    /// # 为什么是纯函数
    ///
    /// 它是"要多大离屏纹理"这个问题的唯一来源，而那个问题必须在**不碰 GPU**
    /// 的前提下能回答：这样无 GPU 环境（CPU 参考光栅器、单测）也能验证离屏
    /// 尺寸算得对不对。它是纯几何计算，不含任何系统 API。
    pub fn outsets(&self) -> Outset {
        match self {
            // blur 的强度处处向外，不分方向 ⇒ 四边相同。
            //
            // ⚠ 半径必须 clamp 到 0：负 σ 或 NaN 会让 outset 变成负数，于是
            // 离屏矩形**比**原矩形还小，模糊在边界处被硬生生切掉，且没有任何
            // 报错。`f32::max` 对 NaN 无效（NaN 的一切比较都是 false，直接返回
            // 另一个参数），所以先用 `is_finite` 挡掉。
            Filter::Blur { sigma_x, sigma_y } => {
                let rx = blur_radius(*sigma_x);
                let ry = blur_radius(*sigma_y);
                Outset {
                    left: rx,
                    top: ry,
                    right: rx,
                    bottom: ry,
                }
            }
            Filter::DropShadow { offset, sigma, .. } => {
                let r = blur_radius(*sigma);
                // 影子 = 原矩形被模糊（**四边**各扩 r）后整体平移 offset。
                //
                // ⚠ 四边都要留blur 半径，只有 offset 那一侧才额外多出 |offset|：
                // 向右下投影时，右边扩 `r + offset.x`，而左边只剩 `r - offset.x`
                //（模糊仍会漏出 |offset| 那么多，只是恰好被 offset 抵消掉了）。
                // 写成"只有投影方向扩"会让左边凭空多扩 r —— 症状是离屏矩形被
                // 整体平移，元素左上角出现一小段未渲染的空白，且不报错。
                Outset {
                    left: (r - offset.x).max(0.0),
                    top: (r - offset.y).max(0.0),
                    right: (r + offset.x).max(0.0),
                    bottom: (r + offset.y).max(0.0),
                }
            }
            // 折射**不外扩** —— 而且这不是「忘了填」，是它的采样对象决定的。
            //
            // ⚠⚠ 早先这里写的是「折射的采样会往元素外多读 `strength` 那么多
            // 像素，所以离屏矩形必须留出这一圈，否则边缘的位移在 RT 边界被
            // 硬切」。那句话**把采样对象搞错了**：
            //
            // | | 读的是哪张纹理 | 覆盖范围 |
            // |---|---|---|
            // | 折射 | **`g_dst`** = 目标的**全屏**副本 | 整块屏幕 |
            // | 模糊 / 投影 | 离屏 RT（元素自己的内容） | 元素 ± 外扩 |
            //
            // 折射的合成趟画的是**元素矩形**、采样的��**全屏副本**，所以
            // 「元素外那一圈」根本不在离屏 RT 里 —— 外扩它**不解决任何问题**，
            // 只是把离屏 RT 撑大（每块玻璃四周各多一圈 `strength` 像素），
            // 而那张 RT 在折射路径上**根本没人采样**：`PSRefraction` 只采
            // `g_dst`，元素自己的内容是之后由 `redraw_original` 那趟画回屏幕的。
            //
            // 而且外扩还有**副作用**：拿不到目标副本时（`refresh_dst_copy`
            // 失败）会降级成普通合成，那一趟画的是 `pass.bounds` —— 外扩
            // 之后它会把元素自己的内容**多画一圈**到元素外面，症状是
            // 玻璃四周多出一圈同色毛边。
            //
            // 所以这里给**零**外扩：离屏 RT 正好等于元素矩形。
            Filter::Refraction { .. } => Outset::default(),
            // 背景模糊（磨砂玻璃）同样**不外扩** —— 与折射同一个理由：它采样的是
            // `g_dst`（目标副本 = 元素背后已经画在屏幕上的画面），不是元素自己。
            // 外扩会把"背景被模糊的区域"扩到元素矩形之外，而那圈里根本没有
            // 元素的背景可糊 —— 纯浪费，还会在拿不到目标副本时把元素自己的内容
            // 多画一圈出去。
            // 透镜同样**不外扩** —— 与折射同理：位移 ≤ refraction_amount，采样的是
            // 全屏目标副本（或它的模糊产物），外扩只撑大 RT、解决不了任何问题。
            Filter::Lens { .. } => Outset::default(),
            Filter::BackdropBlur { .. } => Outset::default(),
            // 像素偏移玻璃也**不外扩** —— 它的位移是**解析**的（位移形状自己的
            // 边），不是采样来的，所以不需要多一圈 RT 来装位移后的内容。
            // ⚠ 但位移本身会把边**推出去** `amount` 像素 ⇒ 几何必须留出那条
            // 余量，否则被推出去的边会被裁掉（症状是「玻璃的角被削平」）。
            // 那条余量由**样式表侧的尺寸**负责，不在滤镜的外扩里 ——
            // 因为外扩会同时把**离屏矩形**撑大，而这条路径压根不开离屏。
            Filter::PixelOffset { .. } => Outset::default(),
        }
    }

    /// 该滤镜是否为恒等（不改变任何像素）。
    ///
    /// σ 为 0 的 blur 是恒等；`DropShadow` 恒不为恒等（哪怕全透明也有影子形状，
    /// 且 alpha=0 时渲不渲染都可能 —— 保守地判为非恒等，交给离屏处理）。
    pub fn is_identity(&self) -> bool {
        match self {
            Filter::Blur { sigma_x, sigma_y } => {
                blur_radius(*sigma_x) <= 0.0 && blur_radius(*sigma_y) <= 0.0
            }
            Filter::DropShadow { .. } => false,
            // 折射的**唯一**有效参数是 `strength`（玻璃厚度）。
            //
            // ⚠ `dispersion` **不参与**这条判据 —— 而这不是「省事」，
            // 是着色器那边的**事实**：`PSRefraction` 里三条通道的采样点是
            //
            //     uvR = uv + off_uv * (1 + dispersion)
            //     uvG = uv + off_uv
            //     uvB = uv + off_uv * (1 - dispersion)
            //
            // 而 `off_uv = -n * displacement * px2uv`，`displacement` 里
            // **带 `strength` 这个因子**。所以 `strength = 0` ⇒ `off_uv = 0`
            // ⇒ 三条通道采样同一点 ⇒ 色散**乘的是 0** ⇒ 整条链是恒等。
            //
            // 早先这里写的是「strength=0 但 dispersion>0 时仍然有效果」——
            // 那句话**与着色器不符**（它以为色散是独立于位移的一条通道偏移）。
            // 后果：厚度调成 0 的那一档，链被判成「非恒等」⇒ 照样开离屏 pass、
            // 照样拷一份全屏目标副本，而画面一个字都不变 —— 纯浪费，
            // 而 `RenderReport::offscreen_passes` 还多记一次。
            //
            // ⚠ `ior` 同样**不参与**：它是 1.0 就等于「没有折射」，而那正是
            // `strength = 0` 的情形；把它判进来会写出 `strength=0 但 ior=1.5
            // ⇒ 非恒等`，于是「没写厚度却写了折射率」的样式表会让整条滤镜
            // 命令**既不恒等也不生效** —— 离屏照开、像素照弯成 0，白付一遍
            // 离屏开销。
            //
            // 而 `ior = 1`（其余参数正常）**要**判成非恒等：那一档是
            // 「位移应当恰好为 0」的诊断位 —— 门禁要的是「跑了 PSRefraction
            // 且画面没变」，所以 pass 必须真的跑起来。
            Filter::Refraction {
                strength,
                bezel,
                dispersion,
                ior,
            } => {
                let _ = (bezel, dispersion, ior);
                !param_active(*strength)
            }
            // 透镜的唯一有效参数是 `refraction_height` / `refraction_amount`：
            // 任一为 0（原版 `lens()` 直接 return）整个滤镜是恒等。
            // 与 `PSLens` 里 `refractionHeight <= 0 || refractionAmount <= 0` 早退一致。
            Filter::Lens {
                refraction_height,
                refraction_amount,
            } => !(param_active(*refraction_height) && param_active(*refraction_amount)),
            // 背景模糊在 σ ≤ 0 时恒等（与 [`Filter::Blur`] 同判据）。
            Filter::BackdropBlur { sigma } => blur_radius(*sigma) <= 0.0,
            // 像素偏移玻璃：`amount = 0` ⇒ 形状一点都不被推 ⇒ 恒等。
            //
            // ⚠ `bezel` **不参与**这条判据 —— 它只决定「位移作用带有多宽」，
            // 带宽为 0 时位移落在一条零宽的线上 ⇒ 效果为零，但那是**参数写错**，
            // 不是「这条链不生效」⇒ 判成非恒等（保守：宁可画一趟，不要静默不动）。
            Filter::PixelOffset { amount, .. } => !param_active(*amount),
        }
    }
}

/// 折射的某个参数是否**真的在起作用**。
///
/// 抽出成函数而不是内联两个比较：`is_identity` 与 `outsets` 都要用同一条
/// 判据（`strength=0` 在两处必须得出同一个结论），而内联写两遍就有漂移的机会。
fn param_active(v: f32) -> bool {
    v.is_finite() && v > 0.0
}

/// 折射位移的**纯 Rust 镜像**（与 `zephyra.hlsl` 的 `PSRefraction` 逐字对应）。
///
/// # 为什么要有它
///
/// 位移公式住在**着色器**里（GPU 上才算得出来），而「位移有多大」这条
/// **契约**住在 core 里（离屏外扩、参数面板的语义都按它推导）。
/// 两边如果各写各的，公式一改契约就悄悄失效 —— 而失效的样子是
/// 「玻璃边缘把屏幕上几百像素外的内容拉进来」，看不出是契约错了。
///
/// 所以这里把同一条式子写成**纯函数**：它不参与渲染，只用来**验**那条契约
/// （见 `tests/filter_tests.rs` 的 `refraction_displacement_is_bounded_by_the_thickness`）。
/// 着色器那边改了式子，这里必须同步改 —— 门禁会红。
///
/// # 与着色器的对应
///
/// | 本函数 | `PSRefraction` |
/// |---|---|
/// | `glass_surface_height(t)` | `glass_surface_height(t)` |
/// | `dh`（前向差分，`dt = 0.001`） | 同 |
/// | `slope = atan(dh * strength / bezel)` | 同 |
/// | `theta_r = asin(clamp(sin(slope) / ior, -1, 1))` | 同 |
/// | `δ = h · strength · sin(Δ) / cos(Θr)` | 同 |
pub mod refraction {
    /// 深度归一化 `t` → 玻璃表面高度。`t = 0` 在边缘（高度 0），`t = 1` 在带内侧。
    ///
    /// 圆拱的四次方根：它在 `t → 0` 处**斜率发散** —— 那正是「光在边缘斜着进出」
    /// 的数学表达。
    pub fn glass_surface_height(t: f32) -> f32 {
        let s = 1.0 - t;
        (1.0 - s * s * s * s).max(0.0).powf(0.25)
    }

    /// 倒角带内某一点的**横向位移**（像素，带符号）。
    ///
    /// `t` = 倒角带里的归一化深度（0 在边缘，1 在带内侧）。
    ///
    /// # 返回 0 的两种情形
    ///
    /// - `ior = 1` ⇒ `Δ = 0` ⇒ 不折射（这是诊断位：门禁要「画面逐像素不变」）；
    /// - `t = 0` ⇒ 高度为 0 ⇒ 楔形没有高度 ⇒ 没有偏折。
    pub fn displacement(t: f32, strength: f32, bezel: f32, ior: f32) -> f32 {
        if !(t.is_finite() && strength.is_finite() && bezel.is_finite() && ior.is_finite()) {
            return 0.0;
        }
        if bezel <= 0.0 || ior <= 0.0 {
            return 0.0;
        }
        let t = t.clamp(0.0, 1.0);
        let dt = 0.001;
        let h = glass_surface_height(t);
        let dh = (glass_surface_height((t + dt).min(1.0)) - h) / dt;
        let slope = (dh * (strength / bezel)).atan();
        let sin_r = (slope.sin() / ior).clamp(-1.0, 1.0);
        let theta_r = sin_r.asin();
        let delta = slope - theta_r;
        h * strength * (delta.sin() / theta_r.cos().max(1e-3))
    }

    /// 整个倒角带里的**最大位移**（像素）。
    ///
    /// 采样 `t ∈ [0, 1]`（步长 1/2000）。**采样**而不是解析求极值，是因为
    /// 着色器那边也是**离散**地算斜率（有限差分）—— 契约要跟实现同口径。
    ///
    /// 契约：结果 **≤ `strength`**（见 [`Filter::Refraction`](super::Filter::Refraction)
    /// 的 `strength` 文档）。
    pub fn max_displacement(strength: f32, bezel: f32, ior: f32) -> f32 {
        const STEPS: usize = 2000;
        let mut max = 0.0f32;
        for i in 0..=STEPS {
            let t = i as f32 / STEPS as f32;
            let d = displacement(t, strength, bezel, ior).abs();
            if d > max {
                max = d;
            }
        }
        max
    }
}

/// blur 的 σ → 半径，挡掉非有限值。
fn blur_radius(sigma: f32) -> f32 {
    if !sigma.is_finite() || sigma <= 0.0 {
        0.0
    } else {
        sigma * SIGMA_TO_RADIUS
    }
}

/// 离屏矩形每边要外扩的量。
///
/// 分四边而不是一个标量，是因为 [`Filter::DropShadow`] 的外扩**不对称**：
/// 向右下投影只扩右下，向左上投影只扩左上。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Outset {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Outset {
    /// 四边取逐边最大值（并集）。
    ///
    /// 串联滤镜时**不能求和** —— 那会让 blur+blur 的离屏矩形大一倍，而实际
    /// 只有最后一个滤镜的输出边缘在起作用。取最大值是保守且正确的上界。
    pub fn union(self, other: Outset) -> Outset {
        Outset {
            left: self.left.max(other.left),
            top: self.top.max(other.top),
            right: self.right.max(other.right),
            bottom: self.bottom.max(other.bottom),
        }
    }

    /// 是否为零（无需外扩）。
    pub fn is_zero(&self) -> bool {
        self.left <= 0.0 && self.top <= 0.0 && self.right <= 0.0 && self.bottom <= 0.0
    }
}

/// 有序滤镜链。
///
/// # 顺序语义
///
/// `chain[0]` 是**最先**执行的 —— 也就是最贴近源图的那一道，与 Skia 的
/// `SkImageFilters::Blur(..., input = SkImageFilters::Offset(...))` 里
/// `input` 继续嵌套的方向一致。
///
/// # 为什么不用 `Option<Box<FilterChain>>` 而存 `Vec`
///
/// 递归结构更贴近 Skia 的嵌套定义，但带来两个实际代价：
///
/// 1. `Command` 要么深（递归嵌套）要么boxed（每次分配）——而 `Command` 是
///    每帧每节点都产出的热路径数据。
/// 2. 无法在单测里对**扁平**的链断言顺序。
///
/// `Vec` 让 `Command` 保持 `Clone + PartialEq` 且拷贝是一次小分配，且顺序
/// 可以直接断言。两级已足够表达 UI 需求（"先投影再模糊"这种三段以上链
/// 在 Skia 里也是用 `SkImageFilters::Shader` 组合的，不走链）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FilterChain {
    filters: Vec<Filter>,
}

impl FilterChain {
    /// 空链（恒等）。
    pub const fn none() -> Self {
        Self {
            filters: Vec::new(),
        }
    }

    /// 单个滤镜。
    pub fn single(f: Filter) -> Self {
        Self { filters: vec![f] }
    }

    /// 按顺序构造（`filters[0]` 最先执行）。
    pub fn new(filters: impl IntoIterator<Item = Filter>) -> Self {
        Self {
            filters: filters.into_iter().collect(),
        }
    }

    /// 追加一道滤镜，返回新链（`self` 不变）。
    pub fn then(mut self, f: Filter) -> Self {
        self.filters.push(f);
        self
    }

    /// 链中的滤镜，按执行顺序。
    pub fn filters(&self) -> &[Filter] {
        &self.filters
    }

    /// 链是否恒等（空链，或每一项都恒等）。
    ///
    /// 回放方**必须**先判这个再决定要不要开离屏 —— 为恒等链开离屏会让每帧
    /// 多一次全屏 RT 绑定，是纯浪费（Skia 的
    /// `AutoLayerForImageFilter` 就只在 `getImageFilter()` 非空时才进）。
    pub fn is_identity(&self) -> bool {
        self.filters.iter().all(Filter::is_identity)
    }

    /// 这条链是不是**「SVG 式像素偏移玻璃」**（一条 [`Filter::PixelOffset`]）。
    ///
    /// # 为什么需要单独认出来
    ///
    /// 像素偏移玻璃**不开离屏 pass**：它的位移是**解析**的（掰形状自己的边），
    /// 不采样任何背景 ⇒ 没有离屏 RT，也没有全屏目标副本。
    ///
    /// ⇒ 回放方（图元展开）看到它就要**改画法**：把这条命令
    /// 当成一个带玻璃参数的四边形画在**主趟**里，而不是交给离屏管线。
    ///
    /// # 为什么要求链**恰好**只有它
    ///
    /// 链里混了别的滤镜（模糊 / 投影 / 折射）时，那些要离屏 ⇒ 整条链都得
    /// 走离屏 ⇒ 这时就不能再用解析画法（否则别的滤镜就丢了）。
    ///
    /// ⇒ 判据是「链长 1 且那一条是 `PixelOffset`」，多一条都不算。
    pub fn pixel_offset(&self) -> Option<(f32, f32)> {
        match self.filters.as_slice() {
            [Filter::PixelOffset { amount, bezel }] => Some((*amount, *bezel)),
            _ => None,
        }
    }

    /// 整条链要求的外扩（逐边最大值）。
    pub fn outsets(&self) -> Outset {
        self.filters
            .iter()
            .fold(Outset::default(), |acc, f| acc.union(f.outsets()))
    }

    /// 给定内容矩形，算出离屏纹理要覆盖的区域。
    ///
    /// 这是 [`crate::paint::DisplayList`] 之外的**纯计算**：宿主拿它去决定
    /// 离屏纹理尺寸，核心不碰GPU 对象。
    ///
    /// # 与 `quad::AA_BAND` 的关系
    ///
    /// 两者是**叠加**的。外扩之后SDF 的过渡带仍然在四边形内（`AA_BAND`
    /// 已经先把几何扩出去了），所以这里**只**加滤镜自己的 outset，不重复
    /// 加 0.75。
    pub fn bounds_for(&self, content: Rect) -> Rect {
        let o = self.outsets();
        Rect::new(
            content.x - o.left,
            content.y - o.top,
            content.width + o.left + o.right,
            content.height + o.top + o.bottom,
        )
    }

    /// 内容矩形映射回离屏纹理内的局部坐标原点。
    ///
    /// 宿主拿它在离屏 RT 上定位几何 —— 因为 [`Self::bounds_for`] 把左上角
    /// 移到了 `content.x - left`，而几何的坐标仍是屏幕坐标。
    pub fn origin_in(&self, content: Rect) -> Point {
        let o = self.outsets();
        Point::new(content.x - o.left, content.y - o.top)
    }
}

/// 混合模式。
///
/// 与 Skia 的 `SkBlendMode` **逐值对齐**（含顺序），见本模块底部的对照测试。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum BlendMode {
    /// `r = 0`。清空目标。
    Clear,
    /// `r = s`。只画源。
    Src,
    /// `r = d`。只保留目标（丢弃源）。
    Dst,
    /// `r = s + (1-sa)·d`。**默认**。
    #[default]
    SrcOver,
    /// `r = d + (1-da)·s`。
    DstOver,
    /// `r = s·da`。
    SrcIn,
    /// `r = d·sa`。
    DstIn,
    /// `r = s·(1-da)`。
    SrcOut,
    /// `r = d·(1-sa)`。
    DstOut,
    /// `r = s·da + d·(1-sa)`。
    SrcATop,
    /// `r = d·sa + s·(1-da)`。
    DstATop,
    /// `r = s·(1-da) + d·(1-sa)`。
    Xor,
    /// `r = min(s + d, 1)`。
    Plus,
    /// `r = s·d`。
    Modulate,
    /// `r = s + d - s·d`。
    Screen,
    // ---- 以下为可分离（separable）模式：逐通道独立计算 ----
    /// 视目标而定multiply 或 screen。
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Multiply,
    // ---- 以下为非可分离（non-separable）模式：需整幅颜色参与 ----
    /// 取源的色相，目标的饱和度与明度。
    Hue,
    /// 取源的饱和度，目标的色相与明度。
    Saturation,
    /// 取源的色相与饱和度，目标的明度。
    Color,
    /// 取源的明度，目标的色相与饱和度。
    Luminosity,
}

/// 混合方程的系数。
///
/// 对应 Skia 的 `SkBlendModeCoeff`。方程是
/// `result = dst · dst_coeff + src · src_coeff`，其中系数可以是常数、源/目标的
/// 某个分量、或它们的补。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coeff {
    /// `0`
    Zero,
    /// `1`
    One,
    /// `sc`
    SrcColor,
    /// `1 - sc`
    OneMinusSrcColor,
    /// `dc`
    DstColor,
    /// `1 - dc`
    OneMinusDstColor,
    /// `sa`
    SrcAlpha,
    /// `1 - sa`
    OneMinusSrcAlpha,
    /// `da`
    DstAlpha,
    /// `1 - da`
    OneMinusDstAlpha,
}

impl BlendMode {
    /// 最后一个能用系数对表达的混合模式。
    ///
    /// 与 Skia 的 `kLastCoeffMode = kScreen` 对齐。超过它的模式**不可分离**
    /// （或需要整幅颜色参与），无法拆成 `dst·Cd + src·Cs` 的形式 ——
    /// 这不是实现偷懒，是数学上做不到。
    pub const LAST_COEFF_MODE: BlendMode = BlendMode::Screen;

    /// 最后一个可分离模式（逐通道独立运算）。
    ///
    /// 与 Skia 的 `kLastSeparableMode = kMultiply` 对齐。
    pub const LAST_SEPARABLE_MODE: BlendMode = BlendMode::Multiply;

    /// 模式总数（与 Skia 的 `kSkBlendModeCount` 一致：29）。
    pub const COUNT: usize = 29;

    /// 本模式是否可分离（逐通道独立）。
    pub fn is_separable(&self) -> bool {
        *self <= BlendMode::LAST_SEPARABLE_MODE
    }

    /// 本模式是否只能用系数对表达（Porter-Duff那批）。
    pub fn is_coeff(&self) -> bool {
        *self <= BlendMode::LAST_COEFF_MODE
    }

    /// 分解为 `(src_coeff, dst_coeff)`。
    ///
    /// 返回 `None` 表示该模式不可用系数表达（见 [`Self::is_coeff`]）。
    /// 逐项对齐 Skia `SkBlendMode_AsCoeff` 的 `kCoeffs` 表
    /// （`src/core/SkBlendMode.cpp:68-84`）。
    pub fn as_coeff(&self) -> Option<(Coeff, Coeff)> {
        use BlendMode::*;
        use Coeff::*;
        Some(match self {
            Clear => (Zero, Zero),
            Src => (One, Zero),
            Dst => (Zero, One),
            SrcOver => (One, OneMinusSrcAlpha),
            DstOver => (OneMinusDstAlpha, One),
            SrcIn => (DstAlpha, Zero),
            DstIn => (Zero, SrcAlpha),
            SrcOut => (OneMinusDstAlpha, Zero),
            DstOut => (Zero, OneMinusSrcAlpha),
            SrcATop => (DstAlpha, OneMinusSrcAlpha),
            DstATop => (OneMinusDstAlpha, SrcAlpha),
            Xor => (OneMinusDstAlpha, OneMinusSrcAlpha),
            Plus => (One, One),
            Modulate => (Zero, SrcColor),
            Screen => (One, OneMinusSrcColor),
            _ => return None,
        })
    }

    /// 该模式是否需要覆盖已写入的目标像素。
    ///
    /// `Src` / `SrcOver` 之外的所有模式都要读 `dst`，所以**必须**在绘制前
    /// 保证目标区已有内容 —— 这正是"先画背景再画浮层"不能颠倒的原因。
    ///
    /// 只有一个例外值得注意：[`BlendMode::Clear`] 不读 `dst`，可以画在
    /// 未初始化的 RT 上（而不必先清）。
    pub fn needs_destination(&self) -> bool {
        !matches!(self, BlendMode::Clear | BlendMode::Src)
    }
}

/// 一种**材质**（`material:` 声明的高层语义），与 [`Filter`] 正交。
///
/// # 为什么要有材质而不是直接用 [`Filter`]
///
/// `filter:` 是**底层效果链**（`blur` / `drop-shadow` / `refraction` …），把
/// 效果一个接一个排起来。`material:` 是**高层材质语义** —— `.zstyle` 写一个
/// 词（`solid` / `frosted` / `glass`）就能得到一整块材质选型。
///
/// 两者**可共存且正交**：一个节点可以 `filter: drop-shadow(...)`（阴影）+
/// `material: glass`（液态玻璃底）—— 材质展开成滤镜链，与 `filter:` 那条链
/// **合并**后交给渲染。材质不覆盖 filter，也不被 filter 覆盖。
///
/// # 种类与参数分开
///
/// 本枚举只表示**种类**，**不带参数**。参数（σ / 厚度 / 倒角 / 色散 / 折射率）
/// 由**独立的** [`MaterialParams`] 携带（`.zstyle` 的 `material-params:` 属性）。
/// 这样「选哪种材质」与「调它的参数」是两件互不耦合的事 —— 同一个
/// `material: glass` 可以在不同页面用不同的 `material-params`。
///
/// # 三种材质
///
/// | 材质 | `.zstyle` | 展开成 | 含义 |
/// |---|---|---|---|
/// | [`Material::Solid`] | `solid`（默认） | 空链 | 纯色（靠 `background:`）|
/// | [`Material::Frosted`] | `frosted` | [`Filter::BackdropBlur`] | 磨砂玻璃：背后被模糊 |
/// | [`Material::Glass`] | `glass` | [`Filter::Refraction`] | 液态玻璃：背后被折射弯曲 |
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Material {
    /// 纯色：不做任何滤镜（默认）。外观全靠 `background:` 填充。
    #[default]
    Solid,
    /// 磨砂玻璃：把元素**背后**的画面高斯模糊后透出（σ 见 [`MaterialParams::amount`]）。
    Frosted,
    /// **玻璃**（便宜版）：SVG 式像素偏移 —— 形状的边被推开，边上有一道高光。
    ///
    /// # 为什么它是默认的玻璃
    ///
    /// 「液态玻璃」那条路（[`Material::GlassRefract`]）要读元素**背后**的画面，
    /// 而那要求一份**全屏**的目标副本 —— MSAA 开着时那是一次**整幅解析**
    /// （D3D12 没有「按区解析」），**每个**带滤镜的元素都要拷一次。
    ///
    /// 实测（gallery 1400×800 / Intel UHD 630 / D3D12）：材质页一帧 7 个带滤镜的
    /// 元素 ⇒ **玻璃税 8.4 ms/帧**（占那一帧 GPU 开销的 74%）。
    ///
    /// ⇒ 本变体把整块玻璃变成主趟里的**一个四边形**（位移是**解析**的，
    /// 不采样背景）⇒ 玻璃税从 8.4 ms 降到 **~0**。
    ///
    /// 代价说清楚：**背景不再被折射**（像素偏移掰的是**形状自己的边**，
    /// 不是背后的画面）。要真实液态玻璃就用 [`Material::GlassRefract`]。
    Glass,
    /// **液态玻璃**（真实折射）：把元素**背后**的画面按斯涅尔折射弯曲后透出。
    ///
    /// 这条路**完整保留**，但它贵：每个带滤镜的元素都要一次全屏目标副本。
    /// 参数见 [`MaterialParams`]。
    GlassRefract,
}

impl Material {
    /// 磨砂玻璃的**默认 σ**（像素）。
    pub const FROSTED_SIGMA: f32 = 6.0;
    /// 液态玻璃的**默认厚度**（像素）。
    pub const GLASS_STRENGTH: f32 = 20.0;
    /// 液态玻璃的**默认倒角带**（像素）。
    pub const GLASS_BEZEL: f32 = 16.0;
    /// 液态玻璃的**默认色散**。
    pub const GLASS_DISPERSION: f32 = 0.0;
    /// 液态玻璃的**默认折射率**。
    pub const GLASS_IOR: f32 = 1.5;
    /// 像素偏移玻璃的**默认位移幅度**（像素）。
    pub const GLASS_OFFSET_AMOUNT: f32 = 6.0;
    /// 像素偏移玻璃的**默认作用带宽度**（像素）。
    pub const GLASS_OFFSET_BEZEL: f32 = 14.0;

    /// 默认材质 = 纯色。
    pub const fn solid() -> Self {
        Material::Solid
    }

    /// 磨砂玻璃（参数由 [`MaterialParams`] 覆盖）。
    pub const fn frosted() -> Self {
        Material::Frosted
    }

    /// 液态玻璃（参数由 [`MaterialParams`] 覆盖）。
    pub const fn glass() -> Self {
        Material::Glass
    }

    /// 真实液态玻璃（斯涅尔折射那条路）。
    pub const fn glass_refract() -> Self {
        Material::GlassRefract
    }

    /// 本材质用**默认参数**展开成的滤镜链（等价 `filter_with(&MaterialParams::default())`）。
    pub fn filter(&self) -> FilterChain {
        self.filter_with(&MaterialParams::default())
    }

    /// 本材质用给定**参数覆盖**展开成的滤镜链。
    ///
    /// - [`Material::Solid`] → 空链（恒等），参数无关。
    /// - [`Material::Frosted`] → 一条 [`Filter::BackdropBlur`]，σ = `params.amount` 或默认。
    /// - [`Material::Glass`] → 一条 [`Filter::Refraction`]，四参数各取 `params` 或默认。
    ///
    /// 这条链由使用方与 `filter:` 的链**合并**成最终 `FilterChain`。
    /// 渲染后端只看到合并后的链，不知道 `material` 存在。
    pub fn filter_with(&self, params: &MaterialParams) -> FilterChain {
        match self {
            Material::Solid => FilterChain::none(),
            Material::Frosted => FilterChain::single(Filter::BackdropBlur {
                sigma: params.amount.unwrap_or(Self::FROSTED_SIGMA),
            }),
            // 玻璃（默认）= **SVG 式像素偏移**：形状的边被推开 + 边缘高光。
            // 不采样背景 ⇒ 不开离屏 pass ⇒ 不拷全屏目标副本 ⇒ 玻璃几乎不花钱。
            Material::Glass => FilterChain::single(Filter::PixelOffset {
                amount: params.amount.unwrap_or(Self::GLASS_OFFSET_AMOUNT),
                bezel: params.bezel.unwrap_or(Self::GLASS_OFFSET_BEZEL),
            }),
            // 真实液态玻璃：保留，但贵（每个元素一次全屏目标副本）。
            Material::GlassRefract => FilterChain::single(Filter::Refraction {
                strength: params.amount.unwrap_or(Self::GLASS_STRENGTH),
                bezel: params.bezel.unwrap_or(Self::GLASS_BEZEL),
                dispersion: params.dispersion.unwrap_or(Self::GLASS_DISPERSION),
                ior: params.ior.unwrap_or(Self::GLASS_IOR),
            }),
        }
    }
}

/// 材质参数的**独立**覆盖（`.zstyle` 的 `material-params:`，与 `material:` 分开）。
///
/// 每个字段是 `Option`：`None` = 用对应材质的默认值（见 [`Material`] 的常量）。
/// 这样「不写 `material-params`」与「写了但只调其中几项」都是合法的。
///
/// # 字段顺序 = `.zstyle` 里的书写顺序
///
/// `material-params: <amount> <bezel> <dispersion> <ior>`，从第一个数起依次填。
/// 少写的项保持 `None`（用默认）。
///
/// - `amount` 是**主调量**：磨砂玻璃 = σ；液态玻璃 = 玻璃厚度。
/// - `bezel` / `dispersion` / `ior` 只对液态玻璃有意义（磨砂玻璃忽略它们）。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MaterialParams {
    /// 主调量：磨砂 = σ（模糊标准差）；液态玻璃 = 厚度（像素）。
    pub amount: Option<f32>,
    /// 液态玻璃的倒角带宽度（像素）。
    pub bezel: Option<f32>,
    /// 液态玻璃的色散量。
    pub dispersion: Option<f32>,
    /// 液态玻璃的折射率。
    pub ior: Option<f32>,
}

impl MaterialParams {
    /// 全空（所有项用种类默认值）。`const` 可用 —— `props::MATERIAL_PARAMS` 的
    /// 静态默认值要在 `static` 里构造。
    pub const fn none() -> Self {
        Self {
            amount: None,
            bezel: None,
            dispersion: None,
            ior: None,
        }
    }

    /// 从最多四个数依次填入（`material-params` 的解析结果）。多余项忽略。
    pub fn from_numbers(nums: &[f32]) -> Self {
        Self {
            amount: nums.first().copied(),
            bezel: nums.get(1).copied(),
            dispersion: nums.get(2).copied(),
            ior: nums.get(3).copied(),
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/filter_tests.rs"]
mod tests;
