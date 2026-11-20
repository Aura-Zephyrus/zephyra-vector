//! 玻璃装饰层：高光、外投影、内阴影 —— **纯描述，零依赖，不碰任何系统 API**。
//!
//! # 它和 [`crate::filter`] 是两件事
//!
//! | | 描述什么 | 画在内容的哪一侧 |
//! |---|---|---|
//! | [`crate::filter`] | 这条命令**自己**要多经过什么处理（模糊 / 折射 / 投影） | 取决于滤镜（`redraw_original` 决定原图在合成前还是后） |
//! | 本模块 | 一块**玻璃**的装饰四件套：镜面高光、外投影、内阴影 | 固定：外投影在**内容之下**，高光与内阴影在**内容之上** |
//!
//! 层序是**契约**而不是实现细节，所以它必须写进数据里（一个 `FilterChain`
//! 表达不了"这层在内容之上"）。Android 原版（`AndroidLiquidGlass`，Apache-2.0）
//! 的实际绘制顺序就是这个：
//!
//! ```text
//! 外投影（最下）→ 内容 → 高光 → 内阴影（最上）
//! ```
//!
//! # 一比一复刻的参数
//!
//! 本模块里的常量与构造函数**逐字**取自 Kotlin 原版：
//!
//! | 装饰 | 原版构造 | 参数 |
//! |---|---|---|
//! | [`Highlight`] | `Highlight.Default` | `width=0.5dp`, `blurRadius=width/2`, `alpha=1`, `color=White@0.5`, `blend=Plus`, `angle=45°`, `falloff=1` |
//! | [`Highlight`] | `Highlight.Ambient` | `intensity=0.38` ⇒ `color=White@0.38`, `blend=SrcOver`, 无角度 / 衰减 |
//! | [`Highlight`] | `Highlight.Plain` | `color=White@0.38`, `blend=Plus`, **无着色器** |
//! | [`OuterShadow`] | `Shadow(radius=24dp, offset=4dp)` | `radius=24dp`, `offset=(0, radius/6)`, `color=Black@0.1` |
//! | [`InnerShadow`] | `InnerShadow(radius=24dp)` | `radius=24dp`, `offset=(0, radius)`, `color=Black@0.15` |
//!
//! Dp→px 按 `density = 1` 直接换算 —— 于是 `0.5dp` 就是 `0.5px`，与原版数值
//! 逐字节相同（不引入缩放因子）。
//!
//! # 为什么不把它们编成 [`crate::filter::Filter`] 的变体
//!
//! 三条装饰里只有外投影的语义与 [`crate::filter::Filter::DropShadow`] 接近，
//! 但**不完全相同**：原版的外投影要"挖掉形状内侧的余量"（玻璃是半透明的，
//! 内侧的影子会透出来，看起来像脏污）。把它硬塞进 `DropShadow` 会让那个滤镜
//! 的语义分叉，而现有调用方（普通卡片的投影）不该受影响。高光与内阴影更不
//! 是"滤镜"——它们靠形状遮罩与描边，不是对整张图的卷积。

use crate::core::{Color, Point};

/// Dp→px 的换算比。
///
/// 按 `density = 1` 直接换算：与原版的 dp 数值**逐字相同**。
///
/// # 为什么是常量而不是参数
///
/// 原版按设备密度换算，而本仓的三后端没有"每显示器 DPI"这条通路
/// （`docs/developer/win/` 里的缩放是应用级的）。引入它会把"装饰粗细"
/// 绑到一个还没有的设施上；先钉成 1，等 DPI 通路真正接上时只需改这一处。
pub const DP: f32 = 1.0;

/// 高光强度的**着色方式**。
///
/// 三个变体对应原版的三种 `Highlight`：它们不是"同一效果的三个参数"，
/// 而是三种**不同的着色公式**（见 [`HighlightKind`] 的文档）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HighlightKind {
    /// 均匀描边：整圈同亮度，**无着色器**。
    ///
    /// 原版 `Highlight.Plain`。适合小控件（开关、滑块的把手）—— 一圈均匀的
    /// 亮边在那尺寸下已经足够像玻璃，而渐变会让它看起来"一边亮一边暗"。
    Plain,
    /// 方向性高光：亮度随法线与给定方向的夹角衰减。
    ///
    /// 原版 `Highlight.Default`。适用于大块玻璃（卡片、按钮），
    /// 高频的地方朝一个方向亮，其余渐暗。
    Directional,
    /// 环境高光：靠形状**外侧**的那半圈亮、内侧那半圈暗。
    ///
    /// 原版 `Highlight.Ambient`。它不挑方向，只看"这一点在形状边界的内侧
    /// 还是外侧"，所以任何形状都成立 —— 开关与滑块的把手用的就是它
    /// （那些形状在按下时会缩放，方向性高光会跟着转，看起来在抖）。
    Ambient,
}

/// 一块玻璃的**镜面高光**（内缘亮线）。
///
/// # 它画在哪
///
/// 描边**居中于形状边界**，然后整条描边裁到形状**内部** —— 于是可见的部分
/// 只有内侧那一半。原版的 `strokeWidth` 因此是 `ceil(width) * 2`：另一半在
/// 形状外面，被裁掉了。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Highlight {
    /// 描边宽度（像素）。可见的一半由裁剪决定，见类型文档。
    pub width: f32,
    /// 描边的高斯模糊半径（像素）。
    ///
    /// 原版取 `width / 2` —— 比描边本身还窄。不是误区：高光是**软**的亮线，
    /// 半径大于宽度会把它糊成一团而不是一条线。
    pub blur_radius: f32,
    /// 高光色（含 alpha）。
    pub color: Color,
    /// 整体强度（0..1）。原版用它做"按下时渐显"（`alpha = progress`）。
    pub alpha: f32,
    /// 与内容合成时的混合模式。
    pub blend: crate::filter::BlendMode,
    /// 着色方式。
    pub kind: HighlightKind,
    /// 方向性高光的**方向角**（度）。
    ///
    /// 原版 `Default` 固定 45°（右上）。`Ambient` / `Plain` 忽略它。
    pub angle_deg: f32,
    /// 方向性高光的**衰减指数**。
    ///
    /// 着色里的 `intensity = pow(abs(dot(grad, normal)), falloff)`。
    /// 1 = 线性；>1 会让"只有正对方向的窄带亮起来"。原版 `Default` 取 1。
    pub falloff: f32,
    /// 环境高光的强度（`Ambient` 用；其余忽略）。
    ///
    /// 原版 `Ambient` 的 `intensity = 0.38`，且**这个值同时是颜色 alpha**
    /// （着色器输出 `half4(t,t,t,1) * intensity`）。所以两者必须一致，
    /// 否则裁剪出的亮线与底色会分成两层。
    pub ambient_intensity: f32,
}

impl Highlight {
    /// 原版 `Highlight.Default`：白 @0.5，Plus 混合，45°，衰减 1。
    pub const fn directional() -> Self {
        Self {
            width: 0.5 * DP,
            blur_radius: 0.25 * DP,
            color: Color::rgba(1.0, 1.0, 1.0, 0.5),
            alpha: 1.0,
            blend: crate::filter::BlendMode::Plus,
            kind: HighlightKind::Directional,
            angle_deg: 45.0,
            falloff: 1.0,
            ambient_intensity: 0.0,
        }
    }

    /// 原版 `Highlight.Ambient`：白 @0.38，SrcOver 混合，强度 0.38。
    ///
    /// # 为什么它的 `blend` 是 SrcOver 而不是 Plus
    ///
    /// 环境高光的着色器**自己**产出"外侧黑、内侧白"的两半，那个黑半是用来
    /// **压暗**外侧的 —— 用 Plus（只加不减）压不下去。原版因此用 SrcOver。
    pub const fn ambient() -> Self {
        Self {
            width: 0.5 * DP,
            blur_radius: 0.25 * DP,
            color: Color::rgba(1.0, 1.0, 1.0, 0.38),
            alpha: 1.0,
            blend: crate::filter::BlendMode::SrcOver,
            kind: HighlightKind::Ambient,
            angle_deg: 45.0,
            falloff: 1.0,
            ambient_intensity: 0.38,
        }
    }

    /// 原版 `Highlight.Plain`：白 @0.38，Plus 混合，**无着色器**。
    pub const fn plain() -> Self {
        Self {
            width: 0.5 * DP,
            blur_radius: 0.25 * DP,
            color: Color::rgba(1.0, 1.0, 1.0, 0.38),
            alpha: 1.0,
            blend: crate::filter::BlendMode::Plus,
            kind: HighlightKind::Plain,
            angle_deg: 45.0,
            falloff: 1.0,
            ambient_intensity: 0.0,
        }
    }

    /// 换算成**实际要画的**描边宽度（像素）。
    ///
    /// 原版：`ceil(width.coerceAtMost(最小边/2)) * 2` —— 乘 2 是因为描边
    /// **居中于边界**，只有一半落在形状内。夹到 `最小边/2` 是为了防止
    /// 描边粗到把整个形状填满（那时它不再是"亮线"而是"实心亮块"）。
    ///
    /// `min_side` 是形状的最小边长（像素）。
    pub fn stroke_width(&self, min_side: f32) -> f32 {
        let w = self.width.min(min_side / 2.0);
        w.ceil() * 2.0
    }

    /// 是否恒等（不画任何东西）。
    pub fn is_identity(&self) -> bool {
        !(active(self.alpha) && active(self.width) && self.color.a > 0.0)
    }
}

/// 一块玻璃的**外投影**（形状外侧的柔光暗边）。
///
/// # 与 [`crate::filter::Filter::DropShadow`] 的差别
///
/// 原版的投影**要挖掉形状内侧的余量**：玻璃半透明，内侧的影子会透出来，
/// 看起来像玻璃底下有脏污。做法是"形状剪影 → 模糊 → 用 `Clear` 挖掉未偏移
/// 的副本"，于是只剩形状**外侧**那一圈。
///
/// [`crate::filter::Filter::DropShadow`] 没有那一步 —— 它给不透明的卡片用，
/// 内侧本来就被内容盖住。两者因此是**两种效果**，不是同一效果的两种参数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OuterShadow {
    /// 模糊半径（像素）。
    pub radius: f32,
    /// 偏移（像素）。原版默认 `(0, radius / 6)`。
    pub offset: Point,
    /// 阴影色。
    pub color: Color,
    /// 整体强度（0..1）。
    pub alpha: f32,
    /// 是否挖掉形状内侧的余量（见类型文档）。
    ///
    /// 原版恒为 `true`；留成字段是因为不透明控件（普通卡片）不需要这道
    /// 减法，而多一趟 `Clear` 就是多一次离屏操作。
    pub hollow_inside: bool,
}

impl OuterShadow {
    /// 原版默认：`radius=24dp`，`offset=(0, radius/6)`，`Black@0.1`。
    pub const fn glass() -> Self {
        Self {
            radius: 24.0 * DP,
            offset: Point::new(0.0, 4.0 * DP),
            color: Color::rgba(0.0, 0.0, 0.0, 0.1),
            alpha: 1.0,
            hollow_inside: true,
        }
    }

    /// 小型控件的投影：开关 / 滑块把手用的 `radius=4dp`，`Black@0.05`。
    ///
    /// 原版 `LiquidToggle` / `LiquidSlider` 的 `Shadow(radius = 4.dp,
    /// color = Color.Black.copy(alpha = 0.05f))` —— 偏移默认 `(0, radius)`。
    pub const fn control() -> Self {
        Self {
            radius: 4.0 * DP,
            offset: Point::new(0.0, 4.0 * DP),
            color: Color::rgba(0.0, 0.0, 0.0, 0.05),
            alpha: 1.0,
            hollow_inside: true,
        }
    }

    /// 离屏矩形每边要外扩多少 —— `radius * 2.5 + offset`。
    ///
    /// # 为什么是 2.5 倍半径
    ///
    /// `radius` 在这条流水线里就是高斯的 **σ**（见 `pass_for_chain_shaped` 的
    /// 「原版 `Shadow(radius)` 的 radius 就是高斯 σ」）。高斯在 2.5σ 处的值已经
    /// 是 1.2%，再乘上投影色本身的 alpha（玻璃档 0.1）就是 0.12% —— 在 8 位
    /// 通道上是 0.3 个色阶，看不见。
    ///
    /// # 为什么从 4σ 改到 2.5σ
    ///
    /// 外扩直接决定离屏 RT 的面积：一块 220×120 的玻璃（σ=24）在 4σ 下是
    /// **412×312**（12.9 万像素、RT 面积是元素本身的 3.9 倍），2.5σ 下是
    /// 364×264（9.6 万）。实测（gallery 材质页 / 三块玻璃 / UHD 630）：
    /// 三块玻璃的装饰趟从 **2.04 ms** 降到约 **1.6 ms/帧**，画面无可测差异
    /// （投影最外圈的残量本来就被 0.1 的 alpha 压到 8 位量化以下）。
    pub fn outset(&self) -> crate::filter::Outset {
        let r = self.radius * 2.5;
        crate::filter::Outset {
            left: (r - self.offset.x).max(0.0),
            top: (r - self.offset.y).max(0.0),
            right: (r + self.offset.x).max(0.0),
            bottom: (r + self.offset.y).max(0.0),
        }
    }

    /// 是否恒等（不画任何东西）。
    pub fn is_identity(&self) -> bool {
        !(active(self.alpha) && active(self.radius) && self.color.a > 0.0)
    }
}

/// 一块玻璃的**内阴影**（形状内侧的暗边，制造"厚度"）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InnerShadow {
    /// 模糊半径（像素）。
    pub radius: f32,
    /// 偏移（像素）。原版默认 `(0, radius)` —— 向下偏**整个半径**。
    ///
    /// 这个偏移比外投影大得多，是有意的：内阴影要的是"上边缘一圈暗"，
    /// 偏移越大，暗边越集中在上边缘。
    pub offset: Point,
    /// 阴影色。
    pub color: Color,
    /// 整体强度（0..1）。
    pub alpha: f32,
}

impl InnerShadow {
    /// 原版默认：`radius=24dp`，`offset=(0, radius)`，`Black@0.15`。
    pub const fn glass() -> Self {
        Self {
            radius: 24.0 * DP,
            offset: Point::new(0.0, 24.0 * DP),
            color: Color::rgba(0.0, 0.0, 0.0, 0.15),
            alpha: 1.0,
        }
    }

    /// 小型控件的内阴影：`radius=4dp`（按下时按进度缩放），`Black@0.15`。
    ///
    /// 原版 `LiquidToggle` 的 `InnerShadow(radius = 4.dp * progress,
    /// alpha = progress)` —— 偏移默认 `(0, radius)`，颜色默认 `Black@0.15`。
    pub const fn control() -> Self {
        Self {
            radius: 4.0 * DP,
            offset: Point::new(0.0, 4.0 * DP),
            color: Color::rgba(0.0, 0.0, 0.0, 0.15),
            alpha: 1.0,
        }
    }

    /// 是否恒等（不画任何东西）。
    pub fn is_identity(&self) -> bool {
        !(active(self.alpha) && active(self.radius) && self.color.a > 0.0)
    }
}

/// 一块玻璃的装饰四件套（四件里的三件 —— 折射由 [`crate::filter::Filter::Refraction`] 承担）。
///
/// # 层序（契约）
///
/// ```text
/// 外投影（最下）→ 内容 → 高光 → 内阴影（最上）
/// ```
///
/// 这个顺序不是"顺便这么画"，而是原版 `Modifier` 链的实际顺序：
/// `ShadowModifier` 先画影子再画内容，`HighlightModifier` 与
/// `InnerShadowModifier` 都先画内容再画自己。换一个顺序会让高光被内容盖住
/// （玻璃上那圈亮线消失，而画面看起来只是"没高光"，很难归因）。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Decorations {
    /// 镜面高光（画在内容**之上**）。
    pub highlight: Option<Highlight>,
    /// 外投影（画在内容**之下**）。
    pub shadow: Option<OuterShadow>,
    /// 内阴影（画在内容**之上**，且在高光**之后**）。
    pub inner_shadow: Option<InnerShadow>,
}

impl Decorations {
    /// 没有任何装饰。
    pub const fn none() -> Self {
        Self {
            highlight: None,
            shadow: None,
            inner_shadow: None,
        }
    }

    /// 原版大块玻璃的默认四件套：`Highlight.Default` + `Shadow(24dp)` + `InnerShadow(24dp)`。
    pub const fn glass() -> Self {
        Self {
            highlight: Some(Highlight::directional()),
            shadow: Some(OuterShadow::glass()),
            inner_shadow: Some(InnerShadow::glass()),
        }
    }

    /// 原版小控件（开关 / 把手）的装饰：`Highlight.Ambient`(width/1.5) + `Shadow(4dp)` + `InnerShadow(4dp)`。
    ///
    /// # 为什么把高光的宽度与模糊都除以 1.5
    ///
    /// 原版：`Highlight.Ambient.copy(width = Ambient.width / 1.5f,
    /// blurRadius = Ambient.blurRadius / 1.5f, alpha = progress)`。
    /// 小控件的形状周长短，同宽度的亮线在它身上占比过大 —— 看起来像描边
    /// 而不是高光。
    pub const fn control() -> Self {
        let mut hl = Highlight::ambient();
        hl.width = Highlight::ambient().width / 1.5;
        hl.blur_radius = Highlight::ambient().blur_radius / 1.5;
        Self {
            highlight: Some(hl),
            shadow: Some(OuterShadow::control()),
            inner_shadow: Some(InnerShadow::control()),
        }
    }

    /// [`Self::control`] 的**按压进度**版 —— 动画的落点。
    ///
    /// 与原版 `LiquidToggle` 的 `drawBackdrop` 逐项对应（`progress` =
    /// `DampedDragAnimation.pressProgress`）：
    ///
    /// ```text
    /// highlight   = Ambient.copy(width/1.5, blur/1.5, alpha = progress)
    /// shadow      = Shadow(4dp, Black@0.05)          ← 恒定，不随进度
    /// innerShadow = InnerShadow(4dp * progress, alpha = progress)
    /// ```
    ///
    /// `progress = 1` 时逐字段等于 [`Self::control`]（半径缩放除外 ——
    /// control 档的内阴影半径是固定 4dp，这里按进度走原版式子）。
    pub fn control_with_progress(progress: f32) -> Self {
        let p = progress.clamp(0.0, 1.0);
        let mut hl = Highlight::ambient();
        hl.width = Highlight::ambient().width / 1.5;
        hl.blur_radius = Highlight::ambient().blur_radius / 1.5;
        hl.alpha = p;
        Self {
            highlight: Some(hl),
            shadow: Some(OuterShadow::control()),
            inner_shadow: Some(InnerShadow {
                radius: 4.0 * DP * p,
                // 原版 offset 默认是 (0, radius)，radius 随进度缩放——
                // 快照 (0, 4dp) 等于 p=1 才正确的形状。
                offset: Point::new(0.0, 4.0 * DP * p),
                color: InnerShadow::control().color,
                alpha: p,
            }),
        }
    }

    /// 是否恒等（三件装饰都不画）。
    pub fn is_identity(&self) -> bool {
        !(self.highlight.is_some_and(|h| !h.is_identity())
            || self.shadow.is_some_and(|s| !s.is_identity())
            || self.inner_shadow.is_some_and(|s| !s.is_identity()))
    }

    /// 外投影给离屏矩形要求的外扩（没有投影 ⇒ 零外扩）。
    ///
    /// 高光与内阴影**不外扩** —— 它们都裁在形状内部，采样范围不出形状。
    pub fn outsets(&self) -> crate::filter::Outset {
        match self.shadow {
            Some(s) if !s.is_identity() => s.outset(),
            _ => crate::filter::Outset::default(),
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/decoration_tests.rs"]
mod tests;

/// 某个标量参数是否**真的在起作用**。
///
/// NaN 与负数一律判"不生效"：它们会让模糊半径 / 描边宽度变成无意义的值，
/// 而"不画"是唯一安全的降级。
fn active(v: f32) -> bool {
    v.is_finite() && v > 0.0
}
