//! 离屏目标规划：把"哪些命令需要离屏、要多大离屏纹理"算出来。
//!
//! # 职责边界
//!
//! 本模块**只算尺寸**，不碰任何 GPU 对象、不调任何 D3D API、不分配纹理。
//! 判据很简单：**能不能在没有 GPU 的机器上写单测并断言输出**。
//!
//! 这条边界带来两个实际好处：
//!
//! 1. CPU 参考光栅器（`cpu_raster`）能复用同一份尺寸计算，于是无 GPU 环境
//!    也能验证"离屏区域算得对不对"。
//! 2. 离屏尺寸是**效果正确性**的前提：算小了，模糊/阴影在边界处被硬切，
//!    症状是"效果的边缘有一道看不见的直边"——那种 bug 在真机上极难定位，
//!    而在这里是一行assertion。
//!
//! # 为什么不需要配对的层标记
//!
//! [`crate::paint::DisplayList`] 构造时即按 `(layer, z, order)` **稳定排序**。
//! `PushLayer` / `PopLayer` 这类配对标记靠"在有序流里前后夹住内容"生效，可排序
//! 会把中间内容插到别的 `layer` 上去，配对当场失效且不报错。
//!
//! 所以本模块走**按内容几何**的路子：每条命令自己算自己的离屏区域。这与 Skia
//! 一致 —— 它也从不录save/restore，而是靠 `SkCanvas::aboutToDraw` →
//! `AutoLayerForImageFilter`（`SkCanvasPriv.h:129`）在回放时按需开离屏，尺寸
//! 来自 `paint.computeFastBounds(drawBounds)`（`SkCanvasPriv.cpp:308`）。
//!
//! 与我们不同的地方是：Skia 那个 API 在 GPU 侧，尺寸算在 device 里；我们把它
//! 放在纯 CPU 层，所以能单测。

use crate::core::{Point, Rect};
use crate::decoration::Decorations;
use crate::filter::FilterChain;
use crate::paint::{command_content_rect, command_filter, Command, DisplayList, SortKey};
use crate::paint::{command_corner_radius, command_corner_shape, command_decorations};

/// 一帧的回放计划：普通段与离屏段按 `DisplayList` 顺序**交替**。
///
/// # 为什么放在这里而不是两个后端各写一份
///
/// 这是"合成顺序必须等于绘制顺序"这条规则的**唯一**实现。两边各切一次的话，
/// 早晚有一边把离屏段排到了末尾 —— 而症状只是"某个元素的阴影被卡片背景盖住了"，
/// 不报任何错。
#[derive(Debug, Clone, PartialEq)]
pub enum Segment {
    /// 一段**不含**滤镜命令的普通图元（`DisplayList` 下标区间，半开）。
    ///
    /// 空区间也会产出（见 [`plan_segments`]），因为那样「段数 = 滤镜命令数
    /// × 2 + 1」是一条可以直接断言的不变量（Plain/Offscreen 严格交替，
    /// 首尾都是 Plain）。
    Plain(std::ops::Range<usize>),
    /// 一条要走离屏 pass 的命令（按 `DisplayList` 下标定位）。
    ///
    /// `pass` **装箱**：玻璃装饰把 `OffscreenPass` 撑到 ~260 字节，而
    /// `Plain` 只有 24 —— 不装箱的话 clippy 的 `large_enum_variant` 会红，
    /// 且每个 `Segment` 都按最大变体占栈。
    Offscreen {
        /// 在 `DisplayList` 里的下标。
        index: usize,
        /// 已经算好的计划（避免二次解析）。
        pass: Box<OffscreenPass>,
        /// 合成之后要不要把原命令再照常画一遍（投影为 `true`）。
        redraw: bool,
        /// 原命令的排序键 —— 造单条列表时要它，否则离屏内容的 `SortKey` 会是
        /// 编造的默认值，而 `sort()` 的稳定性依赖真实键。
        key: SortKey,
    },
    /// 一条**外部渲染源**命令（`Command::Custom`，按 `DisplayList` 下标定位）。
    ///
    /// 它不是离屏——它是「这一位交给宿主钩子」的标记。单独成段（而不是混进
    /// 前后 `Plain`）的唯一理由是**顺序**：钩子必须在它该出现的位置被调用，
    /// 提前则盖住它上面的内容，拖后则被盖。与 Plain/Offscreen 交替同理，
    /// 后端按顺序逐段回放时自然撞上它。
    Custom {
        /// 在 `DisplayList` 里的下标。
        index: usize,
    },
}

/// 把一帧切成 [`Segment`] 序列。
///
/// # 结构不变量：段数 = 滤镜命令数 × 2 + 自绘源段数 + 1
///
/// 每条带滤镜的命令产出**一对**段（它前面的 `Plain` + 它自己的 `Offscreen`），
/// 每条不带滤镜的 `Custom` 命令产出**一对**段（同理），首尾再各补一个 `Plain`。
/// 于是序列严格交替：`Plain, Offscreen/Custom, Plain, …, Plain`，总段数 =
/// `2 × 滤镜命令数 + 2 × 自绘源段数 + 1`（都没有时就是单个覆盖全部的
/// `Plain(0..len)`）。
///
/// # 为什么空段也要产出
///
/// 不产出空段的话，"整帧只有一条滤镜命令"会得到 `vec![Offscreen]`，段序列
/// 从「首尾是 Plain」变成「有时是、有时不是」—— 而「严格交替、首尾 Plain」
/// 正是各后端回放循环直接依赖的结构（见 `zephyra-winit` / `-d3d12` /
/// `zephyra-vulkan` 三个 renderer 的同一段循环）。产出空段换来的是这条关系
/// 恒成立，且区间拼接恰好铺满 `[0, len)`。
///
/// ⚠ 旧注释这里写的是「段数 = 滤镜命令数 + 1」—— 那个数是错的（每条滤镜
/// 命令贡献两段而非一段），2026-10-25 由 `stress_plan_segments_*` 压力
/// 测试抓出并更正。
pub fn plan_segments(list: &DisplayList) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::new();
    let mut start = 0usize;
    for (i, item) in list.iter().enumerate() {
        // 外部渲染源：单独成段（见 `Segment::Custom` 的顺序论证）。带滤镜的
        // Custom **不**走这里——它会落进下面的离屏分支，在几何展开层被计数
        // （v1 离屏不支持自绘，见 `Command::Custom` 文档）。
        if let Command::Custom { source, .. } = &item.command {
            if !source.is_none() && command_filter(&item.command).is_some_and(|f| f.is_identity()) {
                out.push(Segment::Plain(start..i));
                out.push(Segment::Custom { index: i });
                start = i + 1;
                continue;
            }
        }
        let Some(pass) = plan_pass(&item.command) else {
            continue;
        };
        out.push(Segment::Plain(start..i));
        out.push(Segment::Offscreen {
            index: i,
            redraw: pass.redraw_original,
            pass: Box::new(pass),
            key: item.key,
        });
        start = i + 1;
    }
    out.push(Segment::Plain(start..list.len()));
    out
}

/// 一张离屏纹理的规划。
///
/// `atlas` 那类共享资源不在此处——本模块只管**效果**离屏，纹理复用策略属于
/// 资源管理（对应 [`crate::text::atlas`] 里的淘汰策略那一层）。
#[derive(Debug, Clone, PartialEq)]
pub struct OffscreenPlan {
    /// 要开离屏的命令在 [`DisplayList`] 中的下标。
    pub command_index: usize,
    /// RT 要覆盖的区域（已含滤镜 outset）。
    pub bounds: Rect,
    /// 内容在 RT 内的局部原点。
    ///
    /// 几何坐标仍是屏幕坐标，所以宿主必须把 `bounds.x/y` 减掉才能定位 ——
    /// 这就是 [`crate::filter::FilterChain::origin_in`] 的用处。
    pub origin: crate::core::Point,
    /// 内容区未外扩的原始矩形。
    pub content: Rect,
    /// 该离屏承载的滤镜链。
    pub filter: FilterChain,
}

/// 规划整帧的离屏目标，按 DisplayList 顺序返回。
///
/// # 复用：相同矩形会被合并成一条
///
/// 两条命令若离屏矩形**完全相同**（常见于同一个卡片上的背景 + 文本都带同一
/// 个阴影），合并成一张 RT。判据是矩形相等而不是"重叠" —— 重叠合并需要
/// 面积包含判断，而两张 RT 恰好重叠时复用错误的后果是画面错位，收益
/// （省一张小 RT）远不抵风险。
pub fn plan_offscreens(list: &DisplayList) -> Vec<OffscreenPlan> {
    let mut out: Vec<OffscreenPlan> = Vec::new();
    for (i, item) in list.iter().enumerate() {
        let Some((filter, content)) = split_filter_and_content(&item.command) else {
            continue;
        };
        // 装饰与滤镜**任一**非恒等都要开离屏 —— 理由见 [`plan_pass`]。
        let decorations = crate::paint::command_decorations(&item.command)
            .copied()
            .unwrap_or_default();
        if filter.is_identity() && decorations.is_identity() {
            continue;
        }
        // 外扩取"滤镜 ∪ 装饰外投影"的逐边并集（与 [`pass_for_chain`] 同口径）。
        let decor_outset = decorations.outsets();
        let bounds = if decor_outset.is_zero() {
            filter.bounds_for(content)
        } else {
            filter.bounds_for(content).outset_by(decor_outset)
        };
        // origin 必须跟**最终 bounds**走：装饰外扩比滤镜大时，按滤镜算的
        // origin 会把 RT 的左上角放在元素里面，内容整体错位。
        let origin = Point::new(bounds.x, bounds.y);
        // 与已有的一张**矩形与滤镜都**相同就复用 —— 典型场景是同一张卡片的
        // 背景与文本共用一个投影。
        //
        // ⚠ 判据必须是「矩形相等**且**滤镜相等」的**全表**扫描，不是「找到
        // 第一张矩形相等的就停下」：后者在 (同矩形, 不同滤镜) 交替出现时会
        // 漏合并 —— A(B,F1)、B(B,F2) 之后再来一条 (B,F2) 时，第一轮找到的
        // 是 A（滤镜不同于是放行），于是 (B,F2) 被 push 两遍。症状是同一块
        // 区域拿到两张一模一样的 RT（纯浪费），且「相同矩形应合并」的契约
        // 静默失效。2026-10-25 由 `stress_plan_offscreens_*` 压力测试抓出。
        if out
            .iter()
            .any(|p| p.bounds == bounds && p.filter == *filter)
        {
            continue;
        }
        out.push(OffscreenPlan {
            command_index: i,
            bounds,
            origin,
            content,
            filter: filter.clone(),
        });
    }
    out
}

/// 取出命令的滤镜与内容矩形；`None` = 该命令不参与效果离屏。
///
/// 转发到 `paint` 里的那一份实现 —— 那里同时被
/// [`crate::paint::DisplayList::offscreen_requirements`] 用着。
/// 两处各写一遍的话，漏掉某个命令变体的后果是"那个变体的滤镜区域算小一圈"，
/// 症状是模糊在边界被硬切，**且不报任何错**。
fn split_filter_and_content(cmd: &Command) -> Option<(&FilterChain, Rect)> {
    Some((command_filter(cmd)?, command_content_rect(cmd)?))
}

/// 本帧有多少条命令的混合模式不是默认值。
///
/// 混合模式**不**产生离屏需求 —— 它改的是混合方程，不是"要多少像素"。
/// 绝大多数模式能在现有 target 上换混合状态完成（Skia 的 `GrBlendMode` 就是
/// 这么用的）。但它需要 PSO 变体，所以宿主要知道有没有。
pub fn count_non_default_blends(list: &DisplayList) -> usize {
    list.iter()
        .filter(|i| {
            let b = match &i.command {
                Command::Quad { blend, .. }
                | Command::Path { blend, .. }
                | Command::Image { blend, .. }
                | Command::Text { blend, .. }
                | Command::Custom { blend, .. } => *blend,
                Command::PushClip(_) | Command::PopClip => crate::filter::BlendMode::default(),
            };
            b != crate::filter::BlendMode::default()
        })
        .count()
}

// ═══════════════════════════════════════════════════════════════════════════
//  E4.4 / E4.5 / E4.6：离屏 pass 的**执行**计划
// ═══════════════════════════════════════════════════════════════════════════
//
//  上面 [`OffscreenPlan`] 回答"要开几张 RT、每张多大"，本节回答"每张 RT 上要做
//  哪几趟、每趟参数是什么、结果贴回哪里"。两者分开的理由与模块顶部那条边界
//  相同：这里仍然**一个 GPU 对象都不碰**，所以能被无设备环境单测断言。
//
//  分开的实际收益是：模糊趟的方向与顺序一旦在 D3D11 与 D3D12 的 `renderer.rs`
//  里各写一遍，早晚有一边顺序反了，而症状只是"某个元素的阴影糊的方向不对"，
//  不报任何错。现在两个后端只剩"照着计划设状态、画一个铺满四边形"。
//
//  ── 一趟离屏渲染长什么样 ──
//
//    ┌───────────── 目标（屏幕）───────────────┐
//    │  …普通图元… │ …普通图元… │ …普通图元…  │
//    └────────────────────────────────────────┘
//                    ↑ 遇到带滤镜的命令（分段回放）
//    1. 建 / 复用 RT_A（尺寸 = bounds 向上取整），清成全 0
//    2. 把该命令的几何按 `translate` 平移进 RT_A，逐段回放
//    3. 每一趟模糊：铺满四边形 → PSBlurH / PSBlurV，A⇄B **乒乓**
//    4. 铺满四边形 → PSComposite，把结果贴回目标的 `bounds` 处
//    5. 若 `redraw_original`（投影），再把原命令照常画一遍 —— 影子在下、原图在上
//
//  ── 乒乓只需要两张 RT ──
//
//  链长任意（E4.6）：每一趟都"读一张写另一张"，写的那张就是下一趟的源。所以池子
//  里固定两张就够，与链长无关。

/// 一次离屏渲染的**执行**计划。
///
/// 全部字段都是值，没有引用 —— 计划要跨后端、跨测试地存活。
#[derive(Debug, Clone, PartialEq)]
pub struct OffscreenPass {
    /// 离屏纹理尺寸（像素，整数）。
    ///
    /// 取 `bounds` 的**向上取整**：`bounds` 是像素空间的浮点矩形，而 RT 只能是
    /// 整数像素。向下取整会把最后不足一个像素的那条边切掉，症状是模糊在
    /// 右/下边缘少一列/一行 —— 只在 σ 大到肉眼可见时才显形。
    pub rt_size: (u32, u32),
    /// 离屏纹理左上角在**屏幕坐标**里的位置（= `bounds` 的左上角）。
    pub origin: (f32, f32),
    /// 离屏纹理覆盖的屏幕矩形（= `bounds`）。
    pub bounds: Rect,
    /// 画进离屏 RT 之前，几何要平移的量（像素）= `-origin + offset`。
    ///
    /// `offset` 来自 `DropShadow`：影子是"剪影模糊后**整体**平移"，所以平移必须
    /// 在**模糊之前**做。模糊之后再挪，会把已经算好的模糊边缘一起挪出 RT ——
    /// 症状是影子的某一侧凭空少一圈模糊。
    pub translate: (f32, f32),
    /// 依次执行的模糊趟，按顺序应用。
    pub blurs: Vec<BlurStep>,
    /// 合成阶段的染色色；`None` = 原样透传。
    ///
    /// 只有投影会给出 Some（"剪影 → 模糊 → 染色"三步里的第三步）。同一条链里
    /// 出现多次投影时**后者覆盖前者**：链语义是从左到右依次执行，而染色是就地
    /// 替换色相，后一次的结果覆盖前一次 —— 与「就地替换」而不是「两次合成」一致。
    pub tint: Option<crate::core::Color>,
    /// 原命令是否要在离屏合成**之后**照常再画一遍。
    ///
    /// 只有投影为 `true`：影子是**额外**画上去的一层，原图本身仍要画在它上面。
    /// 纯模糊相反 —— 离屏结果**就是**这条命令的最终形态；再画一次原图等于
    /// "模糊 + 清晰"叠在一起，视觉上完全看不出模糊。
    pub redraw_original: bool,
    /// 折射参数；`None` = 这条链不折射。
    ///
    /// # 为什么折射**不是**一种 blur step
    ///
    /// 模糊趟读的是 `g_tex`（离屏 RT，即元素自己），而折射必须读**目标副本**
    /// （`g_dst`，元素背后已经画在屏幕上的东西）。两者是**不同的纹理**，所以
    /// 折射不能塞进 `blurs` —— 那样会被 ping-pong 逻辑接上，采样到 RT 而不是
    /// 屏幕背景，症状是"玻璃边缘把元素自己的轮廓糊开了"，看起来像 bug 而
    /// 不像接线错误。
    ///
    /// 它是一个**合成期**参数：合成阶段同时能拿到 RT 结果与背景副本。
    pub refraction: Option<Refraction>,
    /// 透镜（原版 `lens()`）参数；`None` = 这条链不带透镜。
    ///
    /// 与 [`Self::refraction`] / [`Self::backdrop_blur`] **互斥**：三者都读
    /// `g_dst`、都在合成期执行、都"替换"了元素背后那块区域 —— 一次合成只能
    /// 选一种。后者覆盖前者（链的就地替换语义），见 [`pass_for_chain`]。
    pub lens: Option<Lens>,
    /// 背景模糊（磨砂玻璃）参数；`None` = 这条链不做背景模糊。
    ///
    /// # 为什么与 `refraction` 并列而不是复用它
    ///
    /// 两者都读 `g_dst`、都在合成期执行，但**着色器完全不同**：折射按 SDF 法线
    /// 做斯涅尔位移，背景模糊做各向同性高斯卷积。一次合成只能选一种（它们都
    /// "替换"了背景那块区域）—— 所以是**互斥**的两个字段，不是可叠加的列表。
    /// 一条链里同时写 `material: glass` + `material: frosted` 时后者覆盖前者
    /// （与 `tint` 同一条"就地替换"语义）。
    pub backdrop_blur: Option<BackdropBlur>,
    /// 玻璃装饰（高光 / 外投影 / 内阴影），见 [`crate::decoration`]。
    ///
    /// # 三件怎么落到这条 pass 上
    ///
    /// | 装饰 | 落点 |
    /// |---|---|
    /// | 外投影 | 并进既有模糊管线（`blurs` / `tint` / `translate`），但合成趟换成**挖洞**入口（[`Self::hollow_shadow`]）—— 形状内侧的影子要被 `Clear` 挖掉，玻璃是半透明的，不挖会从内侧透出一圈脏影 |
    /// | 高光 | [`Self::highlight`] —— 内容贴回**之后**直接画在屏幕上（纯 SDF 计算，不采样任何纹理） |
    /// | 内阴影 | [`Self::inner_shadow`] —— 同上，且在高光**之后**（层序契约：投影 → 内容 → 高光 → 内阴影） |
    pub decorations: Decorations,
    /// 元素内容矩形（**屏幕坐标**）—— 挖洞与 overlay 趟算 SDF 用。
    pub element_rect: Rect,
    /// 元素圆角半径（SDF 用）。四角同径（`CornerRadius::uniform` 的读法）。
    pub corner_radius: f32,
    /// 角形状 `superellipse(K)` 的 `K`（CSS `corner-shape`）；`1.0`=round，`2.0`=squircle。
    pub corner_shape_k: f32,
    /// 外投影要不要用**挖洞**合成入口（`PSHollowShadow`）。
    pub hollow_shadow: bool,
    /// 高光 overlay 趟；`None` = 不画。
    pub highlight: Option<HighlightOverlay>,
    /// 内阴影 overlay 趟；`None` = 不画。画在**高光之后**（层序契约）。
    pub inner_shadow: Option<InnerShadowOverlay>,
}

/// 挖洞外投影的执行参数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HollowShadow {
    /// 元素内容矩形（**屏幕坐标**）—— 合成趟按它把"元素内侧"挖掉。
    pub element: Rect,
    /// 元素圆角半径。
    pub corner_radius: f32,
    /// 角形状 `superellipse(K)` 的 `K`。
    pub corner_shape_k: f32,
}

/// 高光 overlay 趟的执行参数（纯 SDF，零纹理采样）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HighlightOverlay {
    /// 元素内容矩形（屏幕坐标）。
    pub element: Rect,
    /// 元素圆角半径。
    pub corner_radius: f32,
    /// 角形状 `superellipse(K)` 的 `K`。
    pub corner_shape_k: f32,
    /// 描边宽度（像素，已含"居中于边界 ×2"换算，见
    /// [`crate::decoration::Highlight::stroke_width`]）。
    pub stroke_width: f32,
    /// 描边模糊 σ（像素）。
    pub blur: f32,
    /// 整体强度（0..1）—— 动画时由宿主随按压进度改。
    pub alpha: f32,
    /// 方向角（度）。`Directional` 用。
    pub angle_deg: f32,
    /// 着色公式：`0` = Plain、`1` = Directional、`2` = Ambient。
    pub kind: f32,
}

/// 内阴影 overlay 趟的执行参数（纯 SDF，零纹理采样）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InnerShadowOverlay {
    /// 元素内容矩形（屏幕坐标）。
    pub element: Rect,
    /// 元素圆角半径。
    pub corner_radius: f32,
    /// 角形状 `superellipse(K)` 的 `K`。
    pub corner_shape_k: f32,
    /// 模糊 σ（像素）。
    pub sigma: f32,
    /// 偏移（像素）—— 阴影形状 = 元素形状被平移这一份之后**挖掉**。
    pub offset: crate::core::Point,
    /// 总强度（已含阴影色 alpha × 整体 alpha，plan 阶段乘好）。
    pub alpha: f32,
}

/// 背景模糊（磨砂玻璃）的执行参数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BackdropBlur {
    /// 标准差（像素），**不是**半径。着色器内部自己乘 3 得到半径。
    ///
    /// 与 [`crate::filter::Filter::BackdropBlur`] 的 `sigma` 同名同义 ——
    /// 中间任何一步做了重复的 `×SIGMA_TO_RADIUS` 换算，误差会直接体现在模糊宽度上。
    pub sigma: f32,
}

/// 折射（液态玻璃）的执行参数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Refraction {
    /// 玻璃**厚度**（像素）。
    ///
    /// 不是「位移多少像素」——位移由它与 `bezel` 一起经
    /// 斯涅尔定律算出来（`zephyra.hlsl` 的 `PSRefraction`）。
    pub strength: f32,
    /// 倒角带宽度（像素）：折射只发生在这条带里。
    pub bezel: f32,
    /// 色散量。>0 时 R / B 通道相对 G 沿法线偏移不同的比例。
    pub dispersion: f32,
    /// 折射率（IOR）：1.0 = 没有折射，普通冕牌玻璃约 1.5。
    pub ior: f32,
}

/// 透镜（原版 `lens()`）的执行参数。
///
/// 逐字对应 `RoundedRectRefractionWithDispersionShaderString`：
/// `circleMap(1 - -sd / refractionHeight) * refractionAmount` 的位移剖面，
/// 沿 `gradSdRoundedRect`（`min(radius * 1.5, min(halfSize))` 的倒角带）
/// 给出的方向推开背景，并按 `(cx·cy)/(hx·hy)` 做 7 通道分色。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lens {
    /// 倒角带宽度（元素内像素）：`-sd < refraction_height` 才参与位移。
    pub refraction_height: f32,
    /// 位移量（像素）：带内边缘处最大 `refraction_amount`。
    ///
    /// 原版 `lens()` 把它以**负数**写进 uniform（`setFloatUniform("refractionAmount",
    /// -refractionAmount)`）—— 即沿外法线**向内**采样，那是「透镜放大」而非
    /// 「把背景挤出去」。符号在着色器里落实，这里存正值。
    pub refraction_amount: f32,
}

/// 一次模糊趟。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlurStep {
    /// `true` = 水平（H），`false` = 垂直（V）。
    ///
    /// # 为什么拆成两个方向而不是一个"任意方向"
    ///
    /// 拆开之后每趟对应**一个** PS 入口（`PSBlurH` / `PSBlurV`），方向不必走
    /// 常量缓冲也不必挤进 `Vertex` —— 而 `Vertex` 的每个字段都已被正常图元占满。
    /// 若合成一个入口，方向就得进 cbuffer（D3D12 还要动根签名）。
    pub horizontal: bool,
    /// 标准差（像素），**不是**半径。着色器内部自己乘 3 得到半径。
    ///
    /// 存 σ 而不是半径是为了与 [`crate::filter::Filter`] 的字段同名同义
    /// —— 中间任何一步做了重复的 `×SIGMA_TO_RADIUS` 换算，误差会直接体现在
    /// 模糊宽度上。
    pub sigma: f32,
}

/// 为一条命令算出执行计划；`None` = 它不需要离屏。
///
/// 「不需要」的三种情况：
///
/// 1. 滤镜链恒等（空链，或每一项都恒等）—— 见
///    [`FilterChain::is_identity`]。为恒等链开离屏会让每帧多两次全 RT 模糊。
/// 2. 命令没有内容矩形（`PushClip` / `PopClip`）。
/// 3. 内容矩形为空或非有限。模糊一张空图得到的是全透明 RT，合成上去什么也
///    不改变，只是白白花掉两趟全 RT 模糊；非有限值则更糟 —— 它会一路穿过
///    `ceil` 变成天文数字的尺寸，症状是"程序卡死"而不是"某个元素少画了"。
pub fn plan_pass(cmd: &Command) -> Option<OffscreenPass> {
    let filter = command_filter(cmd)?;
    let decorations = command_decorations(cmd).copied().unwrap_or_default();
    // 滤镜与装饰**任一**非恒等就要开离屏：overlay（高光 / 内阴影）虽然不采样
    // RT，但它们的绘制时机是"内容贴回屏幕之后"—— 只有走离屏块的尾部才有
    // 那个时机（主循环按批次画命令，插不进"这一条命令之后"）。
    if filter.is_identity() && decorations.is_identity() {
        return None;
    }
    // ★ **SVG 式像素偏移玻璃不开离屏**。
    //
    // 它的位移是**解析**的（掰形状自己的边，不采样背景）⇒ 没有离屏 RT，
    // 也没有全屏目标副本 —— 图元展开那层已经把它画成带玻璃参数的四边形了
    // （见 `quad::expand_range_translated_into` 的 `Command::Quad` 分支）。
    //
    // ⚠ 判据必须与展开那层**逐字一致**（都是「链长 1 且那一条是
    // `PixelOffset`」）—— 两边不一致的后果是「玻璃被画了两遍」或
    // 「玻璃根本没画」：前者多花一次离屏，后者元素凭空消失。
    //
    // ⚠ 链里混了别的滤镜时**不能**走这条捷径 —— 那些要离屏 ⇒ 整条链都得
    // 走离屏 ⇒ 这里返回 `None` 会让它们**全部丢掉**。
    if filter.pixel_offset().is_some() && decorations.is_identity() {
        return None;
    }
    let content = command_content_rect(cmd)?;
    if !(content.width > 0.0 && content.height > 0.0) {
        return None;
    }
    if !(content.x.is_finite()
        && content.y.is_finite()
        && content.width.is_finite()
        && content.height.is_finite())
    {
        return None;
    }
    let shape = command_corner_shape(cmd);
    Some(pass_for_chain_shaped(
        filter,
        content,
        &decorations,
        command_corner_radius(cmd),
        shape.k,
    ))
}

/// 按内容矩形、滤镜链与玻璃装饰算计划（`corner-shape: round`）。[`pass_for_chain_shaped`] 的兼容包装。
pub fn pass_for_chain(
    filter: &FilterChain,
    content: Rect,
    decorations: &Decorations,
    corner_radius: f32,
) -> OffscreenPass {
    pass_for_chain_shaped(filter, content, decorations, corner_radius, 1.0)
}

/// 按内容矩形、滤镜链与玻璃装饰算计划。[`plan_pass`] 的纯计算内核，测试可直接调。
///
/// `decorations` / `corner_radius` / `corner_shape_k` 是玻璃装饰的承载：外投影要并进模糊管线，
/// 高光与内阴影要展开成 overlay 趟 —— SDF 用 `corner_radius` + `superellipse(K)`。
pub fn pass_for_chain_shaped(
    filter: &FilterChain,
    content: Rect,
    decorations: &Decorations,
    corner_radius: f32,
    corner_shape_k: f32,
) -> OffscreenPass {
    // 滤镜外扩与装饰外扩（只有外投影要）取**逐边并集**。
    let decor_outset = decorations.outsets();
    let bounds = if decor_outset.is_zero() {
        filter.bounds_for(content)
    } else {
        filter.bounds_for(content).outset_by(decor_outset)
    };
    let origin = (bounds.x, bounds.y);

    let mut blurs: Vec<BlurStep> = Vec::new();
    let mut tint = None;
    let mut offset = crate::core::Point::new(0.0, 0.0);
    let mut redraw_original = false;
    let mut refraction = None;
    let mut lens = None;
    let mut backdrop_blur = None;

    // 装饰的**外投影**并进既有模糊管线：影 = 圆角剪影 → 模糊 → 染色 → 平移，
    // 与 `DropShadow` 同一条流水线；差别只在合成趟要挖掉形状内侧（玻璃半透明，
    // 不挖会从内侧透出一圈脏影），见 `hollow_shadow`。
    let mut hollow_shadow = false;
    if let Some(s) = &decorations.shadow {
        if !s.is_identity() {
            // 原版 `Shadow(radius)` 的 radius 就是高斯 σ（Android `BlurMaskFilter`
            // 的 blurRadius 语义）；`outset` 按 `radius*4` 留的余量 ≥ 3σ，安全。
            push_blur(&mut blurs, true, s.radius);
            push_blur(&mut blurs, false, s.radius);
            offset.x += s.offset.x;
            offset.y += s.offset.y;
            // 阴影色 × 整体强度，一次乘好（与 `InnerShadowOverlay::alpha` 同口径）。
            tint = Some(s.color.with_alpha(s.color.a * s.alpha));
            redraw_original = true;
            hollow_shadow = true;
        }
    }

    // `filters[0]` 最先执行 —— 见 `FilterChain` 的顺序语义。
    for f in filter.filters() {
        match f {
            crate::filter::Filter::Blur { sigma_x, sigma_y } => {
                push_blur(&mut blurs, true, *sigma_x);
                push_blur(&mut blurs, false, *sigma_y);
            }
            crate::filter::Filter::DropShadow {
                color,
                offset: o,
                sigma,
            } => {
                // `color` 进 `tint`（染色），`offset` 进 `translate`（整体平移）。
                // ⚠ 两者都**必须**在模糊趟**之前**定下来：染色发生在合成阶段，
                // 平移发生在画内容阶段 —— 顺序反了的话，影子会带着 blur 半径一起
                // 挪出去（某一侧凭空少一圈模糊）。
                push_blur(&mut blurs, true, *sigma);
                push_blur(&mut blurs, false, *sigma);
                offset.x += o.x;
                offset.y += o.y;
                tint = Some(*color);
                redraw_original = true;
            }
            crate::filter::Filter::Refraction {
                strength,
                bezel,
                dispersion,
                ior,
            } => {
                // 后者覆盖前者，与 `tint` 同一条语义：链是就地依次执行，
                // 同一种效果出现两次时后者赢。
                refraction = Some(Refraction {
                    strength: *strength,
                    bezel: *bezel,
                    dispersion: *dispersion,
                    ior: *ior,
                });
                lens = None;
                backdrop_blur = None;
                // ⚠ 合成阶段输出的是**被扰动的背景**，不是这个元素本身 ——
                // 所以元素自己的描边 / 文字 / 图标必须在合成**之后**再画一遍。
                // 与 `DropShadow` 同一条理由（那是"额外一层"），但方向相反：
                // 投影是"原图在影子之上"，折射是"原图在扰动过的背景之上"。
                redraw_original = true;
            }
            crate::filter::Filter::Lens {
                refraction_height,
                refraction_amount,
            } => {
                // 与 `Refraction` / `BackdropBlur` 同一条就地替换语义：三者都在
                // 合成期占用"元素背后那一块"，同时生效只会互相覆盖。
                lens = Some(Lens {
                    refraction_height: *refraction_height,
                    refraction_amount: *refraction_amount,
                });
                refraction = None;
                backdrop_blur = None;
                // 透镜输出的是**被推开的背景**，元素自身仍要在其后重画。
                redraw_original = true;
            }
            crate::filter::Filter::BackdropBlur { sigma } => {
                // 与折射同一个形态：合成阶段输出的是**被模糊的背景**，元素自己的
                // 描边/文字/图标之后由 `redraw_original` 再画一遍。两者互斥 ——
                // 谁出现在链里靠后，谁就是最终合成那趟（就地替换语义）。
                backdrop_blur = Some(BackdropBlur { sigma: *sigma });
                refraction = None;
                lens = None;
                redraw_original = true;
            }
            crate::filter::Filter::PixelOffset { .. } => {
                // ★ 像素偏移玻璃**不占离屏的任何一环**。
                //
                // 它没有离屏 RT、没有目标副本、没有合成趟 —— 位移是**解析**的，
                // 图元展开那层已经把它画成带玻璃参数的四边形了。
                //
                // ⚠ 走到这里说明「链里混了别的滤镜」（`plan_pass` 的短路只认
                // 「链长 1 且那一条是 PixelOffset」）。此时**不能**把像素偏移
                // 丢掉 —— 那会让玻璃整块消失。
                //
                // 怎么办：像素偏移是**就地替换**语义（与折射 / 透镜 / 磨砂同一族：
                // 「这一块区域被换成我的结果」），而它不占「元素背后那一块」——
                // 所以它与其它滤镜**正交**：这里记下来，让合成趟在**普通合成**
                // 之后**追加**一次「带玻璃参数的重画」。
                //
                // ⚠ 那一次重画要读**元素自己的离屏内容** ⇒ 合成趟的 `final_rt`
                // 就是它 ⇒ 直接重画 `final_rt` 即可（不额外开 RT）。
                //
                // 但 `final_rt` 里存的是**元素自己**（不含玻璃效果）⇒ 重画它
                // 就是「原样贴回」⇒ 玻璃效果丢失。
                //
                // ⇒ 所以这条路径**暂不支持**：链里混了别的滤镜 + 像素偏移时，
                // 像素偏移**不生效**（其它滤镜照常）。
                //
                // 为什么不干脆报错：`.zstyle` 的滤镜链是「就地依次执行」，
                // 混用是允许的；这里少做一件事（少一个效果）比整帧失败好，
                // 而且 `unimplemented_filters` 会记一笔（那是唯一的诊断手段）。
                //
                // TODO：把玻璃参数带进合成趟（`OffscreenPass` 加一个字段，
                // 合成趟按它重画）—— 那样「混合链」也能用玻璃。
                refraction = None;
                lens = None;
                backdrop_blur = None;
            }
        }
    }

    // 向上取整，且至少 1×1：视口为 0 时 D3D11 **不报错**，只是什么都不画。
    let rt_size = (
        (bounds.width.ceil() as u32).max(1),
        (bounds.height.ceil() as u32).max(1),
    );

    // 高光与内阴影的执行参数在 plan 阶段**展平**：着色器只收数值，三档预设
    // 的"种类"折成 `kind` 编号，颜色（白 / 黑）是着色器常量，只有强度与几何
    // 是 per-pass 的 —— 那正是动画要动的东西（按压进度改 alpha）。
    let highlight = decorations.highlight.filter(|h| !h.is_identity()).map(|h| {
        let min_side = content.width.min(content.height);
        HighlightOverlay {
            element: content,
            corner_radius,
            corner_shape_k,
            stroke_width: h.stroke_width(min_side),
            blur: h.blur_radius,
            alpha: h.alpha,
            angle_deg: h.angle_deg,
            kind: match h.kind {
                crate::decoration::HighlightKind::Plain => 0.0,
                crate::decoration::HighlightKind::Directional => 1.0,
                crate::decoration::HighlightKind::Ambient => 2.0,
            },
        }
    });
    let inner_shadow = decorations
        .inner_shadow
        .filter(|s| !s.is_identity())
        .map(|s| InnerShadowOverlay {
            element: content,
            corner_radius,
            corner_shape_k,
            sigma: s.radius,
            offset: s.offset,
            alpha: s.color.a * s.alpha,
        });

    OffscreenPass {
        rt_size,
        origin,
        bounds,
        translate: (-origin.0 + offset.x, -origin.1 + offset.y),
        blurs,
        tint,
        redraw_original,
        refraction,
        lens,
        backdrop_blur,
        decorations: *decorations,
        element_rect: content,
        corner_radius,
        corner_shape_k,
        hollow_shadow,
        highlight,
        inner_shadow,
    }
}

/// σ ≤ 0 的趟是恒等，不入列表。
///
/// # 为什么不留给着色器判
///
/// 着色器里 `BlurRadius(σ) = 0` 时循环只跑 k=0、权重 1，结果确实等于原值 ——
/// 但那是**两次全 RT 的 draw call + 一次完整的纹理往返**。`Filter::Blur{0,0}`
/// 在语义上是恒等，链里出现它多半是"σ 由上层算出来恰好是 0"的正常情况，
/// 不该为它付这个钱。
fn push_blur(out: &mut Vec<BlurStep>, horizontal: bool, sigma: f32) {
    if sigma.is_finite() && sigma > 0.0 {
        out.push(BlurStep { horizontal, sigma });
    }
}

/// 模糊 / 合成 pass 用的**铺满四边形**（`kind == kind::BLUR`）。
///
/// # 顶点字段怎么复用
///
/// 着色器对 `kind == 4` 的四边形有一套**与正常图元完全不同**的字段约定
/// （见 `zephyra.hlsl` 里 `PSBlurH` 上方那张表）：
///
/// | 字段       | 这里填                     |
/// |------------|----------------------------|
/// | `local`    | 该顶点在**源 RT** 里的像素坐标 |
/// | `half_size`| **源 RT** 的尺寸（四顶点同值） |
/// | `corner`   | `(方向, σ)` —— `.y` 是 σ     |
/// | `uv`       | 像素坐标 ÷ 源 RT 尺寸        |
/// | `color`    | 染色色（仅合成 pass 用）     |
///
/// `local` 原点在左上角、y 向下，与纹理 uv 一致 —— 所以 `uv = local / src_size`
/// 天然正确，不需要再翻一次 y。（再翻一次是这类代码最常见的错，而症状只是
/// "模糊方向上下颠倒"。）
///
/// # 几何铺满**目标**，采样**源**
///
/// 两者尺寸可以不同（源可能是上一趟乒乓过来的那张 RT）。`local` 必须跟着
/// **源**走、`position` 必须跟着**目标**走 —— 混了就是"采样坐标整体缩放"，
/// 症状是模糊变成一团糊在错误位置的东西。
pub fn pass_quad(
    dst_size: (f32, f32),
    src_size: (f32, f32),
    corner: [f32; 2],
    color: [f32; 4],
) -> [crate::render::quad::Vertex; 4] {
    pass_quad_shaped(dst_size, src_size, corner, color, 1.0)
}

/// 同 [`pass_quad`]，但可指定角形状 `superellipse(K)` 的 `K`。
pub fn pass_quad_shaped(
    dst_size: (f32, f32),
    src_size: (f32, f32),
    corner: [f32; 2],
    color: [f32; 4],
    shape_k: f32,
) -> [crate::render::quad::Vertex; 4] {
    let w = dst_size.0.max(1e-6);
    let h = dst_size.1.max(1e-6);
    let sw = src_size.0.max(1e-6);
    let sh = src_size.1.max(1e-6);
    let nx = |x: f32| x / w * 2.0 - 1.0;
    let ny = |y: f32| 1.0 - y / h * 2.0;
    let x0 = nx(0.0);
    let x1 = nx(dst_size.0);
    let y0 = ny(0.0);
    let y1 = ny(dst_size.1);
    // `half_size` 在 HLSL 里是 `nointerpolation`：四个顶点必须填**同一个**值，
    // 否则插值出的"源 RT 尺寸"随位置漂移，模糊的归一化坐标随之漂移 —— 症状
    // 是画面一侧正常、另一侧糊成斜纹。
    let half = [sw, sh];
    let v = |position: [f32; 2], local: [f32; 2]| crate::render::quad::Vertex {
        position,
        local,
        uv: [local[0] / sw, local[1] / sh],
        color,
        corner,
        half_size: half,
        kind: crate::render::quad::kind::BLUR,
        shape: shape_k,
    };
    [
        v([x0, y0], [0.0, 0.0]),
        v([x1, y0], [sw, 0.0]),
        v([x1, y1], [sw, sh]),
        v([x0, y1], [0.0, sh]),
    ]
}

/// 一次模糊趟的铺满四边形。
pub fn blur_quad(
    step: &BlurStep,
    dst_size: (f32, f32),
    src_size: (f32, f32),
) -> [crate::render::quad::Vertex; 4] {
    pass_quad(
        dst_size,
        src_size,
        [f32::from(step.horizontal), step.sigma],
        [0.0, 0.0, 0.0, 1.0],
    )
}

/// 合成 pass 的铺满四边形：采样源 RT 并贴到**屏幕上的 `dst`**。
///
/// `tint` 走 `color`（着色器按 `corner.y > 0` 判是否染色），所以 `corner.y`
/// 这里就是"染色浓度"的存在标记 —— 传 0 或 `tint.a` 都对，但**不能传 NaN**，
/// 否则 `if (tintAmount <= 0.0)` 对 NaN 为 false，于是走进染色分支算出一个
/// 全 NaN 的像素。
pub fn composite_quad(
    dst_screen: Rect,
    dst_size: (f32, f32),
    src_size: (f32, f32),
    tint: Option<crate::core::Color>,
) -> [crate::render::quad::Vertex; 4] {
    // `pass_quad` 铺满**目标**，而这里的源是 RT（尺寸通常与目标不同）。先把
    // 四边形的几何挪到屏幕矩形，再把采样尺寸换成源的 —— 两个尺寸必须分别
    // 处理，见 [`pass_quad`] 的文档。
    let mut v = pass_quad(
        (dst_screen.width, dst_screen.height),
        src_size,
        [0.0, tint.map_or(0.0, |c| c.a)],
        match tint {
            // 顶点色在 `Vertex::quad` 里会被预乘；这里**故意**传非预乘值，
            // 因为 `PSComposite` 只用 `i.color.rgb` 当色相、并用 `t.a * i.color.a`
            // 自己算覆盖率（见那个函数里"染色不能整体乘 alpha"的注释）。
            Some(c) => [c.r, c.g, c.b, c.a],
            None => [0.0, 0.0, 0.0, 1.0],
        },
    );
    // 几何：按屏幕矩形重算 NDC（目标尺寸 = `dst_size`），因为合成四边形只覆盖
    // 离屏纹理对应的那块屏幕区域，而不是铺满整个目标。
    let w = dst_size.0.max(1e-6);
    let h = dst_size.1.max(1e-6);
    let (x0, x1) = (
        dst_screen.x / w * 2.0 - 1.0,
        (dst_screen.x + dst_screen.width) / w * 2.0 - 1.0,
    );
    let (y0, y1) = (
        1.0 - dst_screen.y / h * 2.0,
        1.0 - (dst_screen.y + dst_screen.height) / h * 2.0,
    );
    v[0].position = [x0, y0];
    v[1].position = [x1, y0];
    v[2].position = [x1, y1];
    v[3].position = [x0, y1];
    v
}

/// 折射合成趟的铺满四边形。
///
/// # 与 [`composite_quad`] 的字段约定**不同**（这是本函数存在的原因）
///
/// 折射要算元素的 SDF，而 SDF 需要**元素自己**的半宽半高与圆角半径；
/// 模糊趟的 `local` / `half_size` 走的是**源 RT**。两者必须分开填：
///
/// | 字段 | 模糊 / 合成 | 折射 |
/// |---|---|---|
/// | `local` | 源 RT 内的像素坐标 | **元素内**的像素坐标（算 SDF） |
/// | `half_size` | 源 RT 的尺寸（`nointerpolation`） | **元素**的半宽半高 |
/// | `uv` | `local / src_size` | 元素内归一化坐标（采样背景用） |
/// | `corner` | `(0, tintAmount)` | `(圆角半径, 折射强度)` |
/// | `color` | 染色色 | `.r` = 色散量 |
///
/// 折射的合成阶段不染色，所以 `color` 的前三个通道空着，`.r` 让给色散。
/// 这看着像是"挪用字段"，而它确实是 —— 但加一整个新顶点字段会让 `Vertex`
/// 从 52 字节涨到 56，进而要同步改**两个**后端的 `VERTEX_LAYOUT` 与
/// 着色器语义分配（后者有重叠陷阱，见 `zephyra.hlsl` 的 `VSIn` 注释）。
/// 为一个 per-pass 常量付那个代价不划算。
///
/// 屏幕 uv 由着色器从 `VSOut.ndc` 推出（`ndc * 0.5 + 0.5`），不占顶点字段 ——
/// 那条已经在 `zephyra.hlsl` 的 `VSOut` 注释里写明为什么。
///
/// # 几何只铺**元素矩形**，不铺离屏范围
///
/// 这一节记的是一个**已经犯过、且错得极难发现**的错误。
///
/// 旧版把 `position` 铺的是 `pass.bounds`（含 outset 的离屏范围，而折射的
/// outset 是 0，所以它等于命令矩形 —— 但合成趟拿到的是**整块离屏 RT 的范围**），
/// 而 `local` 仍按元素矩形铺。两者不匹配的后果是：四边形越出元素的那一圈，
/// `local` 仍落在 `[0, ew]` 内，SDF 于是说它「在元素内部」，`mask` 恰好是 1。
///
/// 于是折射合成**把整幅画面都变成了玻璃** —— 而那正是它应该有的样子，所以
/// 没人会当它是坏的。真正的病状只在一个地方露出来：四边形本身只该覆盖元素。
///
/// 修法就是让 `position` 与 `local` 覆盖**同一块区域**。正常形态下两者本来
/// 就一致（outset 为 0），所以这个错在正常路径上完全看不出来。
pub fn refraction_composite_quad(
    element_screen: Rect,
    dst_size: (f32, f32),
    corner_radius: f32,
    corner_shape_k: f32,
    refraction: Refraction,
) -> [crate::render::quad::Vertex; 4] {
    let ew = element_screen.width.max(1e-6);
    let eh = element_screen.height.max(1e-6);
    let sw = dst_size.0.max(1e-6);
    let sh = dst_size.1.max(1e-6);
    // 几何铺的是**元素矩形**（而不是离屏范围），见函数文档。
    let (x0, x1) = (
        element_screen.x / sw * 2.0 - 1.0,
        (element_screen.x + element_screen.width) / sw * 2.0 - 1.0,
    );
    let (y0, y1) = (
        1.0 - element_screen.y / sh * 2.0,
        1.0 - (element_screen.y + element_screen.height) / sh * 2.0,
    );
    let half = [ew * 0.5, eh * 0.5];
    let v = |position: [f32; 2], local: [f32; 2]| crate::render::quad::Vertex {
        position,
        local,
        // 元素内归一化：0..1 对应元素的左/上边到右/下边。
        uv: [local[0] / ew, local[1] / eh],
        // 若干参数塞进同一个 `vec4` 的不同通道（见上面的表）：
        // `.r` = 色散、`.g` = 倒角带宽、`.b` = 折射率。
        color: [refraction.dispersion, refraction.bezel, refraction.ior, 1.0],
        // x = 圆角半径，y = 玻璃厚度
        corner: [corner_radius, refraction.strength],
        half_size: half,
        kind: crate::render::quad::kind::BLUR,
        shape: corner_shape_k,
    };
    [
        v([x0, y0], [0.0, 0.0]),
        v([x1, y0], [ew, 0.0]),
        v([x1, y1], [ew, eh]),
        v([x0, y1], [0.0, eh]),
    ]
}

/// 透镜合成趟的铺满四边形。
///
/// 与 [`refraction_composite_quad`] 同一套字段约定（`local`/`half_size` 是元素的、
/// `uv` 是元素内归一化、屏幕 uv 由着色器从 `ndc` 推、几何只铺元素矩形）。参数
/// 塞法不同：
///
/// | 字段 | 透镜 |
/// |---|---|
/// | `corner.x` | 圆角半径 |
/// | `corner.y` | `refraction_amount`（像素，**正值**，着色器取负） |
/// | `color.g` | `refraction_height`（元素内像素） |
///
/// 原版 `lens()` 的 `offset = -padding` 在本仓恒为 0（`BackdropBlur` 不设 padding），
/// 所以 `local` 直接就是原版的 `coord`，不需要额外平移。
pub fn lens_composite_quad(
    element_screen: Rect,
    dst_size: (f32, f32),
    corner_radius: f32,
    corner_shape_k: f32,
    lens: Lens,
) -> [crate::render::quad::Vertex; 4] {
    let ew = element_screen.width.max(1e-6);
    let eh = element_screen.height.max(1e-6);
    let sw = dst_size.0.max(1e-6);
    let sh = dst_size.1.max(1e-6);
    let (x0, x1) = (
        element_screen.x / sw * 2.0 - 1.0,
        (element_screen.x + element_screen.width) / sw * 2.0 - 1.0,
    );
    let (y0, y1) = (
        1.0 - element_screen.y / sh * 2.0,
        1.0 - (element_screen.y + element_screen.height) / sh * 2.0,
    );
    let half = [ew * 0.5, eh * 0.5];
    let v = |position: [f32; 2], local: [f32; 2]| crate::render::quad::Vertex {
        position,
        local,
        uv: [local[0] / ew, local[1] / eh],
        // `.g` = 倒角带宽度；其余通道不用（透镜的 7 通道分色强度由几何推出）。
        color: [0.0, lens.refraction_height, 0.0, 1.0],
        // x = 圆角半径，y = 位移量
        corner: [corner_radius, lens.refraction_amount],
        half_size: half,
        kind: crate::render::quad::kind::BLUR,
        shape: corner_shape_k,
    };
    [
        v([x0, y0], [0.0, 0.0]),
        v([x1, y0], [ew, 0.0]),
        v([x1, y1], [ew, eh]),
        v([x0, y1], [0.0, eh]),
    ]
}

/// 背景模糊合成趟的铺满四边形。
///
/// 与 [`refraction_composite_quad`] 同一套顶点字段约定（`local`/`half_size` 是
/// **元素**的、`uv` 是元素内归一化、屏幕 uv 由着色器从 `ndc` 推），差别只在
/// 参数塞法：折射把 `corner.y` 当玻璃厚度、`color` 前三通道当色散/倒角/IOR；
/// 背景模糊只有一个 σ，塞进 `corner.y`，`color` 留常量。
///
/// 几何同样只铺**元素矩形**（理由见 [`refraction_composite_quad`] 的文档：
/// 四边形必须与 `local` 覆盖同一块区域，否则整幅画面都成了磨砂）。
pub fn backdrop_blur_composite_quad(
    element_screen: Rect,
    dst_size: (f32, f32),
    corner_radius: f32,
    corner_shape_k: f32,
    sigma: f32,
) -> [crate::render::quad::Vertex; 4] {
    let ew = element_screen.width.max(1e-6);
    let eh = element_screen.height.max(1e-6);
    let sw = dst_size.0.max(1e-6);
    let sh = dst_size.1.max(1e-6);
    let (x0, x1) = (
        element_screen.x / sw * 2.0 - 1.0,
        (element_screen.x + element_screen.width) / sw * 2.0 - 1.0,
    );
    let (y0, y1) = (
        1.0 - element_screen.y / sh * 2.0,
        1.0 - (element_screen.y + element_screen.height) / sh * 2.0,
    );
    let half = [ew * 0.5, eh * 0.5];
    let v = |position: [f32; 2], local: [f32; 2]| crate::render::quad::Vertex {
        position,
        local,
        uv: [local[0] / ew, local[1] / eh],
        // 背景模糊不染色，`color` 留常量。
        color: [0.0, 0.0, 0.0, 1.0],
        // x = 圆角半径，y = σ。
        corner: [corner_radius, sigma],
        half_size: half,
        kind: crate::render::quad::kind::BLUR,
        shape: corner_shape_k,
    };
    [
        v([x0, y0], [0.0, 0.0]),
        v([x1, y0], [ew, 0.0]),
        v([x1, y1], [ew, eh]),
        v([x0, y1], [0.0, eh]),
    ]
}

/// 挖洞外投影的**合成趟**四边形。
///
/// # 与 [`composite_quad`] 的差别
///
/// 影子铺的是**整块 bounds**（含外投影 outset），但挖洞要算**元素**的 SDF ——
/// 所以 `local` 走元素坐标系（以元素左上为原点，可为负）、`half_size` 是元素的
/// 半宽半高。这正好是 [`refraction_composite_quad`] 那套"字段挪用"的镜像：
///
/// | 字段 | 这里填 |
/// |---|---|
/// | `position` | bounds 区域的 NDC（几何铺影子整块） |
/// | `local` | **元素坐标系**下的像素坐标（`0..ew` 是元素本身，外侧为负/超出） |
/// | `uv` | `local / 元素尺寸`（归一化，SDF 用） |
/// | `half_size` | **元素**半宽半高（`nointerpolation`） |
/// | `corner` | `(圆角半径, 染色量)` |
/// | `color` | 染色色（**非预乘**，与 `PSComposite` 同口径） |
///
/// 着色器（`PSHollowShadow`）在 `sd < 0`（元素内侧）处把 alpha 清零 ——
/// 只剩形状**外侧**的影子余量。
pub fn hollow_shadow_composite_quad(
    bounds_screen: Rect,
    element_screen: Rect,
    dst_size: (f32, f32),
    corner_radius: f32,
    corner_shape_k: f32,
    tint: Option<crate::core::Color>,
) -> [crate::render::quad::Vertex; 4] {
    let ew = element_screen.width.max(1e-6);
    let eh = element_screen.height.max(1e-6);
    let sw = dst_size.0.max(1e-6);
    let sh = dst_size.1.max(1e-6);
    // 几何铺 bounds（影子整块），与 [`composite_quad`] 的 NDC 算法同形。
    let (x0, x1) = (
        bounds_screen.x / sw * 2.0 - 1.0,
        (bounds_screen.x + bounds_screen.width) / sw * 2.0 - 1.0,
    );
    let (y0, y1) = (
        1.0 - bounds_screen.y / sh * 2.0,
        1.0 - (bounds_screen.y + bounds_screen.height) / sh * 2.0,
    );
    let half = [ew * 0.5, eh * 0.5];
    let v = |position: [f32; 2], local: [f32; 2]| crate::render::quad::Vertex {
        position,
        local,
        uv: [local[0] / ew, local[1] / eh],
        color: match tint {
            Some(c) => [c.r, c.g, c.b, c.a],
            None => [0.0, 0.0, 0.0, 1.0],
        },
        corner: [corner_radius, tint.map_or(0.0, |c| c.a)],
        half_size: half,
        kind: crate::render::quad::kind::BLUR,
        shape: corner_shape_k,
    };
    // local 覆盖**元素坐标系**下的 bounds 区域（元素左上为原点，外侧为负）。
    let lx0 = bounds_screen.x - element_screen.x;
    let ly0 = bounds_screen.y - element_screen.y;
    [
        v([x0, y0], [lx0, ly0]),
        v([x1, y0], [lx0 + bounds_screen.width, ly0]),
        v(
            [x1, y1],
            [lx0 + bounds_screen.width, ly0 + bounds_screen.height],
        ),
        v([x0, y1], [lx0, ly0 + bounds_screen.height]),
    ]
}

/// 高光 overlay 趟的四边形 —— 画在**屏幕**上，纯 SDF，不采样。
///
/// # 几何铺**元素 + 描边外侧余量**
///
/// 描边**居中于边界**，外侧那一半若不铺出来，`Ambient` 着色（外侧白半）就
/// 画不到 —— 症状是环境高光只剩"内侧压暗"一半。余量 = 描边宽一半 + 3σ
/// （高斯尾巴）；`local` 相应从扩展矩形的左上算。
///
/// | 字段 | 这里填 |
/// |---|---|
/// | `position` | 扩展矩形的 NDC |
/// | `local` / `uv` | **元素坐标系**像素坐标（扩展区域可为负）/ 归一化 |
/// | `half_size` | 元素半宽半高 |
/// | `corner` | `(圆角半径, 着色公式 kind)` |
/// | `color` | `(描边宽, 模糊σ, 强度alpha, 方向角)` —— **自定义语义**，`PSHighlight` 自己解释 |
pub fn highlight_overlay_quad(
    element_screen: Rect,
    dst_size: (f32, f32),
    p: HighlightOverlay,
) -> [crate::render::quad::Vertex; 4] {
    let ew = element_screen.width.max(1e-6);
    let eh = element_screen.height.max(1e-6);
    let sw = dst_size.0.max(1e-6);
    let sh = dst_size.1.max(1e-6);
    let margin = p.stroke_width * 0.5 + p.blur * 3.0;
    let ex = Rect::new(
        element_screen.x - margin,
        element_screen.y - margin,
        element_screen.width + margin * 2.0,
        element_screen.height + margin * 2.0,
    );
    let (x0, x1) = (ex.x / sw * 2.0 - 1.0, (ex.x + ex.width) / sw * 2.0 - 1.0);
    let (y0, y1) = (1.0 - ex.y / sh * 2.0, 1.0 - (ex.y + ex.height) / sh * 2.0);
    let half = [ew * 0.5, eh * 0.5];
    let v = |position: [f32; 2], local: [f32; 2]| crate::render::quad::Vertex {
        position,
        local,
        uv: [local[0] / ew, local[1] / eh],
        color: [p.stroke_width, p.blur, p.alpha, p.angle_deg],
        corner: [p.corner_radius, p.kind],
        half_size: half,
        kind: crate::render::quad::kind::BLUR,
        shape: p.corner_shape_k,
    };
    let lx0 = -margin;
    let ly0 = -margin;
    [
        v([x0, y0], [lx0, ly0]),
        v([x1, y0], [lx0 + ex.width, ly0]),
        v([x1, y1], [lx0 + ex.width, ly0 + ex.height]),
        v([x0, y1], [lx0, ly0 + ex.height]),
    ]
}

/// 内阴影 overlay 趟的四边形 —— 同 [`highlight_overlay_quad`]，参数塞法不同：
///
/// | 字段 | 这里填 |
/// |---|---|
/// | `corner` | `(圆角半径, 模糊σ)` |
/// | `color` | `(偏移x, 偏移y, 强度alpha, 0)` —— `PSInnerShadow` 自己解释 |
pub fn inner_shadow_overlay_quad(
    element_screen: Rect,
    dst_size: (f32, f32),
    p: InnerShadowOverlay,
) -> [crate::render::quad::Vertex; 4] {
    let ew = element_screen.width.max(1e-6);
    let eh = element_screen.height.max(1e-6);
    let sw = dst_size.0.max(1e-6);
    let sh = dst_size.1.max(1e-6);
    let (x0, x1) = (
        element_screen.x / sw * 2.0 - 1.0,
        (element_screen.x + element_screen.width) / sw * 2.0 - 1.0,
    );
    let (y0, y1) = (
        1.0 - element_screen.y / sh * 2.0,
        1.0 - (element_screen.y + element_screen.height) / sh * 2.0,
    );
    let half = [ew * 0.5, eh * 0.5];
    let v = |position: [f32; 2], local: [f32; 2]| crate::render::quad::Vertex {
        position,
        local,
        uv: [local[0] / ew, local[1] / eh],
        color: [p.offset.x, p.offset.y, p.alpha, 0.0],
        corner: [p.corner_radius, p.sigma],
        half_size: half,
        kind: crate::render::quad::kind::BLUR,
        shape: p.corner_shape_k,
    };
    [
        v([x0, y0], [0.0, 0.0]),
        v([x1, y0], [ew, 0.0]),
        v([x1, y1], [ew, eh]),
        v([x0, y1], [0.0, eh]),
    ]
}

/// 把只含 `cmd` 的新列表准备好，让调用方 `expand` 出局部几何。
///
/// # 为什么要临时造一个单条列表，而不是给 `expand` 加"只处理第 N 条"的参数
///
/// `expand` 内部按命令类型分派，加筛选参数就要在**每一个**分派点重复判断，
/// 漏一处就是"某种命令偷偷被画两遍"或"漏画"。造一个单条列表则完全复用既有
/// 路径，筛选只发生一次。
///
/// 代价是一次 `Command` 克隆，而最重的字段 `Arc<[Vertex]>` 克隆只是引用计数 +1。
pub fn single_command_list(cmd: &Command, key: crate::paint::SortKey) -> DisplayList {
    let mut out = DisplayList::new();
    out.push(key, cmd.clone());
    out
}

impl OffscreenPass {
    /// 离屏纹理尺寸转成 `f32` 对。
    ///
    /// # 为什么专门给一个方法而不是调用处写 `(w as f32, h as f32)`
    ///
    /// 因为 `blur_quad` 的第三个参数是**源**尺寸，而 `pass_quad` 里 `local` 与
    /// `half_size` 都由它推导 —— 两处都写一遍 `as f32` 的话，某一处忘了写就会
    /// 让归一化坐标差一个数量级，症状是"模糊变成斜纹"。
    pub fn rt_size_as_f32(&self) -> (f32, f32) {
        (self.rt_size.0 as f32, self.rt_size.1 as f32)
    }
}
#[cfg(test)]
#[path = "../../tests/unit/render/offscreen_stress_tests.rs"]
mod stress_tests;
#[cfg(test)]
#[path = "../../tests/unit/render/offscreen_tests.rs"]
mod tests;
