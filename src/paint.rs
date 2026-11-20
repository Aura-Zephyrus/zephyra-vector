//! 绘制契约：[`DisplayList`] ——核心与 GPU 后端之间**唯一**的接口。
//!
//! # 为什么这是整个框架最关键的契约
//!
//! 布局、控件、主题最终全部降到一个**排序后的绘制命令列表**。后端只消费它。
//! 这意味着：
//!
//! - 换宿主（宿主 → winit+D3D11）时，**核心一行都不用改**——因为核心从不认识
//!   宿主；
//! - 宿主可以自由选择批次策略：全程一个 draw call，或按图层切若干 pass；
//! - 「这一帧重算了什么」可以直接从 DisplayList 上读出来，回答 GPU 占用率问题；
//! - 单元测试不需要 GPU：断言 DisplayList 的内容即可验证整个降级过程。
//!
//! # 排序
//!
//! 命令按 `(layer, z_index, order)` 稳定排序。`layer` 是粗粒度的合成层
//! （背景 / 内容 / 装饰 / 浮层），后端据此切 render pass；`z_index` 是层内顺序；
//! `order` 是同 z 内的稳定次序（**稳定**排序保证同样输入产出同样列表，
//! 否则每次 diff 出来的顺序都不同，纹理缓存命中率会随机抖动）。
//!
//! # 为什么不是「绘制回调」
//!
//! 另一种常见设计是给后端一个闭包列表（`Vec<Box<dyn Fn(&mut Renderer)>>`）。
//! 那样无法跨线程、无法缓存、无法测试——DisplayList 用数据换来了这三样。
//!
//! 本模块同时提供 `render`：把一棵**已布局**的节点树降级成本模块的数据结构。
//! 它是核心对外的唯一产出 —— 宿主拿到 [`DisplayList`] 之后就不再关心树的存在。
//! 几何必须由 `crate::layout` 先算好，本函数只读已摆好的矩形。

use std::any::Any;
use std::borrow::Cow;
use std::fmt;
use std::sync::{Arc, OnceLock};

use crate::core::{Color, CornerRadius, CornerShape, Point, Rect};

/// 合成层。粗粒度，决定后端如何切 render pass。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Layer {
    /// 窗口底色。
    Background = 0,
    /// 内容（卡片、文本、图像）。
    Content = 1,
    /// 装饰（边框、分隔线、焦点环）。
    Overlay = 2,
    /// 浮层（弹出菜单、提示、对话框）。
    Popup = 3,
}

impl Layer {
    /// 该层是否需要独立离屏纹理。
    ///
    /// 浮层需要——它必须能盖住下层内容并带自己的阴影/模糊，若直接画进同一
    /// target 就无法在绘制下层之前保留其像素。
    pub fn needs_offscreen(self) -> bool {
        matches!(self, Layer::Popup)
    }
}

/// 一条绘制命令。
///
/// # 为什么效果挂在命令上，而不是 `PushLayer` / `PopLayer` 配对标记
///
/// 本列表**构造时即按 `(layer, z, order)` 稳定排序**（见 [`DisplayList::sort`]）。
/// 配对命令靠"在有序流里前后夹住内容"生效，可排序会把中间内容插到别的
/// `layer` 上去 —— `PushLayer(Content)` 包住的命令可能被排到 `Popup` 之后，
/// 配对当场失效，**且不报任何错**。
///
/// （本模块确实有 [`Command::PushClip`] / [`Command::PopClip`] 这对配对标记，
/// 但它们**零生产者**——只有定义处与 fx 的消费处。真在 `render_rec` 里发出来，
/// 会撞上同一个问题。）
///
/// Skia 也没这个问题，因为它压根不用配对命令：滤镜挂在 `SkPaint` 上，离屏由
/// `SkCanvas::aboutToDraw` → `AutoLayerForImageFilter`（`SkCanvasPriv.h:129`）
/// 在**回放时按需自动开** —— 有 imageFilter 才`addImageFilterLayer`
/// (`SkCanvasPriv.cpp:234`)。命令流里没有 save/restore 标记。
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// 填充一个圆角矩形（着色器内以 SDF 求出圆角与描边）。
    ///
    /// 刻意**不**提供「任意路径填充」作为主要手段：路径填充要 CPU 三角化，而
    /// UI 里绝大多数图形就是圆角矩形 + 描边——把它做成一个 draw call 级别的
    /// 图元能省掉整条三角化管线。需要任意路径时用 [`Command::Path`]。
    Quad {
        rect: Rect,
        radius: CornerRadius,
        /// 角形状（CSS `corner-shape`）；与 `radius` 配合决定 SDF。
        shape: CornerShape,
        /// 填充色。
        fill: Color,
        /// 描边；`None` 表示不描边。
        stroke: Option<(Color, f32)>,
        /// 滤镜链；空链 = 恒等（回放方据此跳过开离屏）。
        ///
        /// 见本类型文档里"为什么效果挂在命令上"。
        filter: crate::filter::FilterChain,
        /// 混合模式（与已画内容合成）。
        blend: crate::filter::BlendMode,
        /// 玻璃装饰（高光 / 外投影 / 内阴影）；`none` = 不装饰。
        ///
        /// # 为什么挂在命令上而不进 `filter`
        ///
        /// 滤镜描述"这条命令自己怎么处理"；装饰描述的是**三件独立的层**，
        /// 各画在内容的固定一侧（外投影在下、高光与内阴影在上）——那个
        /// 层序是契约，滤镜链里没有"这一层在内容之上"的表达。
        /// 见 [`crate::decoration::Decorations`]。
        decorations: crate::decoration::Decorations,
    },
    /// 填充一段路径（图标、矢量图形等非常规形状）。
    Path {
        /// 已三角化的顶点；由宿主的路径栅格化器或 [`crate::text`] 之外的来源提供。
        ///
        /// # 为什么是 `Arc<[Vertex]>` 而不是 `Vec<Vertex>`
        ///
        /// 几何来自 [`crate::icon::IconSet`] 的三角化缓存，而那条缓存里存的就是
        /// `Arc<TriMesh>` —— 每帧把它克隆成 `Vec` 是**纯浪费**：demo 的 8 个图标
        /// 519 个顶点，每帧拷一遍，拷完当帧丢掉。
        ///
        /// 消费者（`crate::geom_clip` 与各后端）只用 `&[Vertex]` 上的方法
        /// （`len` / `is_empty` / `iter` / `chunks_exact`），而 `Arc<[T]>` 的
        /// `Deref` 让这些调用**一字不用改**。
        ///
        /// 共享所有权在这里是**正当**的（不是 `ElementKey` 那种没人需要的
        /// refcount）：命令要活到展开趟结束，而几何缓存在帧之后仍然有效。
        vertices: std::sync::Arc<[Vertex]>,
        fill: Color,
        filter: crate::filter::FilterChain,
        blend: crate::filter::BlendMode,
    },
    /// 绘制一张纹理图像。
    ///
    /// `texture` 是宿主定义的句柄（宿主 的 `Handle` 或自建的 D3D11 纹理 id）。
    /// 核心**不**认识它——这正是宿主边界所在。
    Image {
        texture: TextureRef,
        /// 纹理内的源区域（`None` = 整张）。
        ///
        /// 用 [`crate::core::UvRect`] 而不是 [`Rect`]：后者是 DIP 屏幕矩形，
        /// 两者坐标系不同，混用会让图形上下颠倒且无任何其他症状。
        src: Option<crate::core::UvRect>,
        dst: Rect,
        /// 圆角半径（DIP）。`0` = 直角。
        ///
        /// # 与 [`Command::Quad::radius`] 同机制，但**多一处 uv 外插**
        ///
        /// 位图本身**不带**抗锯齿（它的边缘是硬的），所以一旦要圆角，边缘就变成
        /// 由 SDF 决定 —— 于是四边形必须像 [`Command::Quad`] 那样按 `AA_BAND`
        /// 向外扩，否则 `d ∈ (0, px]` 那半边过渡带被光栅化器裁掉，**圆角会是硬边**。
        ///
        /// 而外扩若沿用「把 uv 铺在扩后四边形上」的做法，会**裁掉并放大**一圈图像
        /// （32px 的图裁 4.7%），所以 fx 侧改成把 uv **外插**，由采样器的 Clamp 兜住。
        ///
        /// 半径为 0 时**不**外扩、**不**施加遮罩 —— 否则最外一圈像素会被压到
        /// 50% alpha，表现为图像四周发暗一圈。
        radius: CornerRadius,
        /// 角形状（CSS `corner-shape`）；与 `radius` 配合决定圆角 SDF。
        shape: CornerShape,
        tint: Color,
        filter: crate::filter::FilterChain,
        blend: crate::filter::BlendMode,
    },
    /// 绘制一段已排版的文本。
    ///
    /// 文本形状（glyph run 的位置与轮廓）在绘制前由宿主按 [`crate::text`] 的
    /// 测量结果算好，核心不参与字形整形。
    Text {
        /// 字形纹理句柄（宿主定义）。
        atlas: TextureRef,
        /// 已定位的字形四边形，坐标**相对 `offset`**。
        ///
        /// # 为什么是 `Arc<[GlyphQuad]>` 而不是 `Vec`
        ///
        /// 这条命令每帧都会被重建（`DisplayList` 不跨帧存活），而**字形位置本身
        /// 几乎从不变** —— 变的是 `offset`（元素原点随布局/resize 移动）。
        /// 若四边形里烘焙了绝对坐标，那么 resize 的每一帧都要重新算一遍 239 个
        /// 字形、每帧分配 239 个 `Vec`。
        ///
        /// 实测（`docs/dev-log/task-resize.md` §批 C · C1b）：`Vec<GlyphQuad>` 每帧
        /// **136 KB**（239 × 1024 B），占 resize 帧分配量的 **40%**。
        ///
        /// 改成 `Arc<[GlyphQuad]>` + 独立的 `offset` 后，缓存挂在节点上，
        /// 命令每帧只克隆一个 `Arc`（引用计数 +1，**零分配**）。
        ///
        /// 官方对应物：`CLayoutStorage` 存的是**相对元素原点**的布局结果，
        /// 绘制趟叠加变换（`uielement.cpp` 的 `Render`/`Arrange` 都不重排文字）。
        glyphs: std::sync::Arc<[GlyphQuad]>,
        /// `glyphs` 的局部原点要落到屏幕上的哪个点。
        ///
        /// 坐标系是**像素**，与 `Quad::rect` / `Image::dst` 一致。
        offset: Point,
        color: Color,
        filter: crate::filter::FilterChain,
        blend: crate::filter::BlendMode,
    },
    /// 裁剪后续命令到该矩形。
    ///
    /// 作用于**其后**的命令直到下一个 `PopClip`——这是有序列表而非栈的代价：
    /// 用显式配对换取「回放时无需维护状态」。
    ///
    /// ⚠ 见本类型文档：排序会拆散配对，所以这个命令**目前零生产者**。
    PushClip(Rect),
    /// 结束裁剪。见 [`Command::PushClip`] 的排序警告。
    PopClip,
    /// 交给**外部渲染源**自绘（视频帧、GPU 图表、嵌人的另一套渲染器……）。
    ///
    /// 这是 DisplayList 的**唯一开放位**：其余命令核心都认识，唯独这一条，
    /// 核心只知道「这里有一块屏幕矩形归某个外部代码管」。契约与
    /// [`Command::Image::texture`] 同哲学——**核心只搬运，宿主解释**：
    ///
    /// - 排序：它是一条普通命令，参与 `(layer, z, order)` 排序，与相邻命令的
    ///   合成顺序由分段回放（`plan_segments` 的 `Segment::Custom`）保住——
    ///   钩子在它该出现的位置被调用，不会提前或拖后。
    /// - 裁剪/混合状态：钩子被调用时继承**当前后端状态**（已绑定的 RT、
    ///   视口、scissor、混合状态）。钩子内部可以自由改状态，但**必须**
    ///   自理恢复——后端不替它兜底（与 `PushClip` 零生产者的现状一致）。
    /// - 未注册钩子时：后端**跳过并计数**（报告的 `unhandled_custom`），
    ///   症状是"这块区域没画"而不是崩——与 `TextureRef` 查不到纹理同款降级。
    /// - 滤镜：带非恒等滤镜的 `Custom` 会照常进离屏段，但离屏回放的几何
    ///   展开画不出自绘内容（计数）；**钩子自己负责外观，v1 不提供离屏**。
    Custom {
        bounds: Rect,
        /// 外部源的身份与载荷。后端按 `kind` 查注册表，载荷由宿主解释。
        source: RenderSource,
        /// 外部纹理资源键（`ExtContext` 纹理池查找用；与 `source` 并存做兼容迁移）。
        resource_key: Option<std::borrow::Cow<'static, str>>,
        /// 滤镜链（进离屏段用；见上——离屏内的自绘 v1 不执行）。
        filter: crate::filter::FilterChain,
        /// 混合模式。钩子被调用前由后端设好；钩子自理恢复。
        blend: crate::filter::BlendMode,
    },
}

/// 外部渲染源：一个**类型名**加一份**宿主载荷**。
///
/// # 为什么是 `kind: Cow<'static, str>` 而不是 `TypeId`
///
/// 后端的钩子注册表按它查表；跨 crate 边界（宿主进程内任意代码）用字符串
/// 登记，与 [`TextureRef`] 的纹理名、`crate::props::ICON_NAME` 的图标名
/// 同一惯例——**字符串是本框架的跨边界身份**。
///
/// # 为什么载荷是 `Arc<dyn Any>`
///
/// 核心不认识视频帧、图表数据或任何宿主类型；它只保证把**同一个对象**
/// 搬到钩子被调用的地方。`Arc` 让 [`PartialEq`] 能用指针相等判「没变」
/// （diff 不为此抖动），`Any` 让宿主拿回自己的类型。
#[derive(Clone)]
pub struct RenderSource {
    /// 源类型名（后端注册表的查表键，如 `"demo:fill"`、`"video"`）。
    pub kind: Cow<'static, str>,
    payload: Arc<dyn Any + Send + Sync>,
}

impl RenderSource {
    /// 语义上的「无」：`kind` 为空串，属性键的默认值用它。
    ///
    /// 载荷取**共享单例**——两个「无」必须判等（见 [`PartialEq`] 的指针相等），
    /// 否则每个带着默认值的节点在 diff 眼里永远在变。
    pub fn none() -> Self {
        static NONE: OnceLock<Arc<dyn Any + Send + Sync>> = OnceLock::new();
        Self {
            kind: Cow::Borrowed(""),
            payload: NONE.get_or_init(|| Arc::new(())).clone(),
        }
    }

    /// 构造一个外部渲染源。
    ///
    /// `payload` 每帧重建会打破 diff 判等（指针不等 = 「变了」）——跨帧不变的
    /// 载荷应存在宿主状态里、每帧克隆同一个 `Arc`（与字形缓存同纪律）。
    pub fn new(kind: impl Into<Cow<'static, str>>, payload: impl Any + Send + Sync) -> Self {
        Self {
            kind: kind.into(),
            payload: Arc::new(payload),
        }
    }

    /// 是否为「无」。
    pub fn is_none(&self) -> bool {
        self.kind.is_empty()
    }

    /// 借出载荷的具体类型。
    pub fn payload<T: 'static>(&self) -> Option<&T> {
        self.payload.downcast_ref::<T>()
    }

    /// 借出类型擦除的载荷（供扩展桥接）。
    pub fn payload_any(&self) -> &(dyn Any + Send + Sync) {
        &*self.payload
    }
}

impl fmt::Debug for RenderSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 载荷是类型擦除的，打印它的类型名即可——值本体对诊断没意义
        // （能看到的只有地址），kind + 类型名足以回答「这是谁」。
        f.debug_struct("RenderSource")
            .field("kind", &self.kind)
            .field("payload", &self.payload.type_id())
            .finish()
    }
}

impl PartialEq for RenderSource {
    fn eq(&self, other: &Self) -> bool {
        // 指针相等 = 同一个对象 = 没变。kind 相等 + 载荷指针不等 = 变了。
        self.kind == other.kind && Arc::ptr_eq(&self.payload, &other.payload)
    }
}

/// 宿主定义的纹理句柄。
///
/// 核心刻意不定义纹理类型——`宿主::widget::image::Handle` 与自建的 D3D11 纹理
/// id 毫无共性。核心只搬运它，由宿主解释。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TextureRef(pub Cow<'static, str>);

impl TextureRef {
    /// 由宿主提供的稳定字符串 id 构造。
    ///
    /// 接受 `&'static str`（零分配）与 `String` / `Arc<str>`（运行期构造）——
    /// 资产句柄这类东西绝大多数是编译期已知的常量，没必要为它们分配。
    pub fn new(id: impl Into<Cow<'static, str>>) -> Self {
        Self(id.into())
    }

    /// id 字符串。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 三角化后的顶点（位置 + 纹理坐标）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vertex {
    pub position: Point,
    pub uv: Point,
}

/// 宿主提供的**动态路径几何**（Canvas 式动画）。
///
/// 与图标路径的区别：图标按名字从 `IconSet` 取、几何可缓存；这里顶点由宿主每帧
/// 生成（如旋转弧线），通过 `crate::props::PATH_DATA` 的 `Custom` 载荷送进来。
/// 顶点用**节点局部坐标**（`0..rect.width` / `0..rect.height`），绘制时由
/// `transform_vertices` 套上节点的平移 / 缩放。
///
/// `PartialEq` 按顶点值比较 —— 宿主每帧重建但内容没变时 diff 仍判「没变」。
#[derive(Debug, Clone, PartialEq)]
pub struct DynamicPath {
    pub vertices: std::sync::Arc<[Vertex]>,
    /// `true` = 顶点是**节点局部坐标**（0..rect 宽高），绘制前加上 `node.rect()` 左上；
    /// `false` = 已是绝对布局坐标（与 `Command::Path` 的其余生产者一致）。
    ///
    /// 宿主通常只知道「我这控件 56×56，弧心在 (28,28)」，不知道自己被布局到哪 ——
    /// 所以局部坐标是自然写法，默认 `true`。
    pub local: bool,
}

impl DynamicPath {
    pub fn new(vertices: impl Into<std::sync::Arc<[Vertex]>>) -> Self {
        Self {
            vertices: vertices.into(),
            local: true,
        }
    }

    /// 顶点已是绝对布局坐标时使用。
    pub fn absolute(vertices: impl Into<std::sync::Arc<[Vertex]>>) -> Self {
        Self {
            vertices: vertices.into(),
            local: false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.vertices.is_empty()
    }
}

/// LCD 子像素排列顺序。
///
/// RGB 与 BGR 在采样时的唯一差别是三次采样的水平偏移方向相反 —— 见
/// [`GlyphMask::LcdMsdf`] 与着色器字形分支。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LcdOrder {
    #[default]
    Rgb,
    Bgr,
}

/// 字形掩码的**采样方式**。
///
/// # 为什么是一个枚举，而不是一组布尔
///
/// 覆盖率、SDF、MSDF、LCD 是四种互斥的采样语义。若用
/// `is_sdf: bool` + `is_msdf: bool` + `is_lcd: bool`，就会出现 `true/true/true`
/// 这种不合法组合，而着色器只能靠"先检查哪个"来消歧 —— 那是静默错误的温床。
/// 枚举把不合法状态从类型里删掉。
///
/// 模式码（`mode_code`）送进顶点 `shape` 通道：字形分支读它，矩形分支仍把
/// `shape` 解释为角形状参数，两者互不干扰。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GlyphMask {
    /// 旧式覆盖率位图：alpha 通道即覆盖率（替身、无字体降级）。
    Coverage,
    /// 单通道 SDF：读 `red`，按 `w` 做 smoothstep。
    Sdf {
        /// smoothstep 在 SDF 值域上的过渡半宽。
        w: f32,
    },
    /// 多通道 SDF：读 `rgb` 取中位数，按 `w` 做 smoothstep。
    Msdf {
        w: f32,
        /// 伪粗体阈值偏移（正值变粗）：在 smoothstep 输入上减掉它。
        embolden: f32,
    },
    /// LCD 亚像素：在 MSDF 上按子像素偏移采样三次，得到 RGB 各自覆盖率。
    LcdMsdf {
        w: f32,
        embolden: f32,
        order: LcdOrder,
    },
}

impl GlyphMask {
    /// 送进顶点 `shape` 通道的模式码。与 HLSL/WGSL 字形分支的 `mode` 常量一致。
    pub fn mode_code(self) -> f32 {
        match self {
            Self::Coverage => 0.0,
            Self::Sdf { .. } => 1.0,
            Self::Msdf { .. } => 2.0,
            Self::LcdMsdf {
                order: LcdOrder::Rgb,
                ..
            } => 3.0,
            Self::LcdMsdf {
                order: LcdOrder::Bgr,
                ..
            } => 4.0,
        }
    }

    /// smoothstep 半宽；覆盖率路径返回 0。
    pub fn w(self) -> f32 {
        match self {
            Self::Coverage => 0.0,
            Self::Sdf { w } | Self::Msdf { w, .. } | Self::LcdMsdf { w, .. } => w,
        }
    }

    /// 伪粗体阈值偏移；仅 MSDF/LCD 支持。
    pub fn embolden(self) -> f32 {
        match self {
            Self::Msdf { embolden, .. } | Self::LcdMsdf { embolden, .. } => embolden,
            _ => 0.0,
        }
    }
}

/// 一个已定位的字形四边形。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlyphQuad {
    /// 纹理内的字形区域（atlas 的归一化 uv）。
    ///
    /// 用 [`crate::core::UvRect`] 而非 [`Rect`]：图集 uv 与屏幕矩形坐标系不同，
    /// 用同一个类型表达两者，颠倒的错误不会有任何其他症状。
    pub uv: crate::core::UvRect,
    /// 屏幕位置与尺寸。
    pub dst: Rect,
    /// 该字形是否来自彩色字体（如 emoji）——决定采样时是否忽略 `color`。
    pub color_glyph: bool,
    /// 采样方式。取代原先的 `sdf: Option<f32>`。
    pub mask: GlyphMask,
    /// 逐字形颜色覆盖；`None` 用 `Command::Text::color`（正文/装饰线默认色）。
    ///
    /// 文字阴影用它给副本一个独立颜色；这比给 `Command::Text` 加"阴影色再画一遍"
    /// 更省一条命令，且阴影与正文天然共用同一张图集。
    pub color: Option<Color>,
    /// 顶点类型。字形走 `GLYPH`；装饰线走 `RECT`（纯色 SDF，不采纹理）。
    ///
    /// 默认常量见 [`crate::render::quad::kind::GLYPH`]，直接写 1.0 是为了
    /// 避免 paint 层反向依赖 render 层。
    pub kind: f32,
}

/// 一条命令的排序键。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SortKey {
    /// 合成层。
    pub layer: Layer,
    /// 层内 z 序。
    pub z: i32,
    /// 同 z 内的稳定次序。
    pub order: u32,
}

/// 带排序键的命令。
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// 排序键。
    pub key: SortKey,
    /// 绘制命令。
    pub command: Command,
}

/// 排序后的绘制命令列表 —— 核心与后端之间的契约。
///
/// 构造时即完成稳定排序，因此后端可以顺序回放而无需再比较。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DisplayList {
    items: Vec<Item>,
}

impl DisplayList {
    /// 空列表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 追加一条命令。
    pub fn push(&mut self, key: SortKey, command: Command) {
        self.items.push(Item { key, command });
    }

    /// 按 `(layer, z, order)` **稳定**排序。
    ///
    /// 稳定性是缓存命中率的前提：同样输入必须产出逐字节相同的列表，否则
    /// 宿主侧的批次缓存每次都失效。
    pub fn sort(&mut self) {
        self.items.sort_by_key(|i| i.key);
    }

    /// 命令条数。
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// 当前已分配容量（用于验证稳态零分配）。
    pub fn capacity_hint(&self) -> usize {
        self.items.capacity()
    }

    /// 顺序迭代。
    pub fn iter(&self) -> impl Iterator<Item = &Item> {
        self.items.iter()
    }

    /// 某合成层是否出现。
    pub fn has_layer(&self, layer: Layer) -> bool {
        self.items.iter().any(|i| i.key.layer == layer)
    }

    /// 按层切分，供后端决定 render pass 边界。
    pub fn layers(&self) -> Vec<Layer> {
        let mut out: Vec<Layer> = self.items.iter().map(|i| i.key.layer).collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// 本列表里所有需要离屏的命令及其离屏矩形。
    ///
    /// 这是"要多大离屏纹理"这个问题的**唯一**答案来源，且它是**纯计算**：
    /// 不碰任何 GPU 对象、不查任何系统 API。所以 CPU 参考光栅器与单元测试
    /// 都能验证离屏尺寸算得对不对——而离屏尺寸算错的后果是"效果在边界处被
    /// 切掉"，那种 bug 极难在真机上看出来。
    ///
    /// # 为什么不需要配对的层标记
    ///
    /// 因为排序会拆散配对（见 [`Command`] 文档）。这里改成**按内容几何**回答：
    /// 每条带非恒等 `filter` 的命令自己声明"我要多大离屏目标"，宿主据此按需
    /// 绑定 RT。相邻命令若离屏矩形相同/重叠，宿主可以复用同一张——复用策略
    /// 属于资源管理，不属于契约。
    ///
    /// 混合模式**不**产生离屏需求：它改的是混合方程，而不是"要多少像素"。
    /// 绝大多数模式可以直接在现有 target 上换混合状态完成（Skia 的
    /// `GrBlendMode` 就是这么用的）。
    pub fn offscreen_requirements(&self) -> Vec<OffscreenRequirement> {
        let mut out = Vec::new();
        for item in &self.items {
            let (Some(content), Some(filter)) = (
                command_content_rect(&item.command),
                command_filter(&item.command),
            ) else {
                continue;
            };
            if filter.is_identity() {
                continue;
            }
            out.push(OffscreenRequirement {
                bounds: filter.bounds_for(content),
                content,
                filter: filter.clone(),
                layer: item.key.layer,
            });
        }
        out
    }

    /// 本列表里是否出现过非默认混合模式。
    ///
    /// 宿主据此决定要不要为混合状态建 PSO 变体。这是纯查询，无副作用。
    pub fn has_non_default_blend(&self) -> bool {
        self.items.iter().any(|i| match &i.command {
            Command::Quad { blend, .. }
            | Command::Path { blend, .. }
            | Command::Image { blend, .. }
            | Command::Text { blend, .. }
            | Command::Custom { blend, .. } => *blend != crate::filter::BlendMode::default(),
            Command::PushClip(_) | Command::PopClip => false,
        })
    }

    /// 清空（复用分配，稳态零分配的关键）。
    pub fn clear(&mut self) {
        // 保留容量：每帧重建 DisplayList 时复用同一块缓冲。
        self.items.clear();
    }
}

/// 一条命令的离屏需求：宿主据此分配 RT 并把内容画进去。
#[derive(Debug, Clone, PartialEq)]
pub struct OffscreenRequirement {
    /// 离屏纹理要覆盖的区域（已含滤镜外扩）。
    pub bounds: Rect,
    /// 未外扩的原始内容区域。
    ///
    /// 几何坐标仍是屏幕坐标，所以宿主要用
    /// [`crate::filter::FilterChain::origin_in`] 算出局部原点才知道往哪儿挪。
    pub content: Rect,
    /// 要执行的滤镜链。
    pub filter: crate::filter::FilterChain,
    /// 该命令所在的合成层 —— 决定 RT 的生命周期跨度。
    pub layer: Layer,
}

/// 一条命令的**滤镜链**；`None` = 这条命令不参与离屏（剪裁标记）。
pub fn command_filter(cmd: &Command) -> Option<&crate::filter::FilterChain> {
    match cmd {
        Command::Quad { filter, .. }
        | Command::Path { filter, .. }
        | Command::Image { filter, .. }
        | Command::Text { filter, .. }
        | Command::Custom { filter, .. } => Some(filter),
        Command::PushClip(_) | Command::PopClip => None,
    }
}

/// 一条命令携带的**玻璃装饰**；`None` = 该命令类型不携带（只有 Quad 有）。
///
/// # 为什么只有 Quad
///
/// 装饰的着色靠**圆角矩形 SDF**（高光描边、内阴影、投影挖洞全都按
/// `sd_round_rect` 算），而 SDF 需要"元素矩形 + 圆角半径"——只有
/// [`Command::Quad`] 同时带着这两样。玻璃控件本来就是圆角矩形。
pub fn command_decorations(cmd: &Command) -> Option<&crate::decoration::Decorations> {
    match cmd {
        Command::Quad { decorations, .. } => Some(decorations),
        _ => None,
    }
}

/// 一条命令要模糊 / 投影的**原始内容矩形**（未外扩）。
///
/// # 为什么单独抽出来
///
/// [`DisplayList::offscreen_requirements`]（纯查询，给资源规划用）与渲染器的
/// 「分段回放」（要真的建 RT 并画一遍）都需要这个矩形。两处各写一遍的话，
/// 迟早有一处漏掉某个命令变体 —— 症状是那个变体的滤镜区域算小了一圈，
/// 模糊在边界处被硬生生切掉，**且不报任何错**。
pub fn command_content_rect(cmd: &Command) -> Option<Rect> {
    match cmd {
        Command::Quad { rect, .. } => Some(*rect),
        Command::Image { dst, .. } => Some(*dst),
        // 文本的范围是所有字形四边形的并集 —— 一条命令里有 N 个字形四边形，
        // 整体模糊才正确（逐字形模糊会把每个字都糊成独立一块，视觉上完全不是
        // "这段文字被模糊了"）。
        Command::Text { glyphs, offset, .. } => Some(union_of_quads(glyphs, *offset)),
        // Path 没有包围盒**字段**，但顶点里就带着位置 —— 遍历一次即得紧包围盒。
        //
        // 用**紧**包围盒而不是节点的布局矩形：三角化的结果不会铺满布局矩形
        // （图标的空白边角、外接圆与图形的差集），按布局矩形算会让每张 RT
        // 白白大一圈；按紧包围盒算，包围盒相同的两条 Path 还能复用同一张 RT。
        Command::Path { vertices, .. } => Some(bounds_of_vertices(vertices)),
        // 自绘源自己声明它的占地——核心不知道里面画了什么，只能信它。
        // 虚报的代价由虚报者承担：报大了浪费离屏 RT / 命中测试区域，
        // 报小了被相邻内容盖住。
        Command::Custom { bounds, .. } => Some(*bounds),
        Command::PushClip(_) | Command::PopClip => None,
    }
}

/// 一条命令的**形状圆角半径**（像素）—— 折射算 SDF 用。
///
/// # 为什么不能靠内容矩形反推
///
/// 内容矩形只是包围盒。圆角矩形的 SDF 要的是**半径**：同一个包围盒，
/// 方角与 40px 圆角是两个完全不同的形状，而折射的边缘高光位置完全由半径
/// 决定 —— 半径错了，高光落在错误的地方，画面仍然"有折射效果"，
/// 于是极难归因。
///
/// # 非矩形命令返回 0
///
/// `Path`（图标 / 矢量图形）与 `Text`（一排字形四边形）没有单一的圆角半径：
/// 前者的形状来自三角化后的多边形，后者是多个独立四边形。给它们一个假的
/// 半径只会在合成阶段算出一个**看似合理但毫无意义**的 SDF。
/// 返回 0（方角）是诚实的退化 —— 折射对它们仍然工作，只是不带边缘高光。
pub fn command_corner_radius(cmd: &Command) -> f32 {
    match cmd {
        // 四角相同（`CornerRadius` 只有一个 `radius`），所以取它即可。
        Command::Quad { radius, .. } => radius.radius,
        // 图像可以裁成圆角，所以它的半径也是真的。
        Command::Image { radius, .. } => radius.radius,
        Command::Path { .. } | Command::Text { .. } => 0.0,
        // 自绘源外观归宿主，核心没有形状信息可给——方角是诚实退化。
        Command::Custom { .. } => 0.0,
        Command::PushClip(_) | Command::PopClip => 0.0,
    }
}

/// 一条命令的**角形状**（CSS `corner-shape`）—— 圆角遮罩/SDF 用。
/// 非矩形命令返回 [`CornerShape::ROUND`]（与半径 0 的退化一致）。
pub fn command_corner_shape(cmd: &Command) -> CornerShape {
    match cmd {
        Command::Quad { shape, .. } => *shape,
        Command::Image { shape, .. } => *shape,
        Command::Path { .. }
        | Command::Text { .. }
        | Command::Custom { .. }
        | Command::PushClip(_)
        | Command::PopClip => CornerShape::ROUND,
    }
}

/// 一批字形四边形的并集，平移 `offset`。
///
/// 空列表返回零尺寸矩形（不是 `None`）——调用方拿到后自然画不出东西，
/// 而 `Option` 会逼每个调用点都写一遍 `if let`。
///
/// # `offset` 为什么是参数而不是烘焙进每个 `dst`
///
/// 因为四边形要**跨帧复用**（见 [`Command::Text::glyphs`]）：烘焙了绝对坐标就
/// 没法复用，每帧都得重算 + 重分配。
///
/// `pub` 是为了让离屏规划**调用同一份实现**而不是抄一份：
/// 两条路径必须给出相同答案（见 [`crate::render::offscreen`] 的模块文档），
/// 抄一份就等于把「必须相同」变成「碰巧相同」。
pub fn union_of_quads(glyphs: &[GlyphQuad], offset: Point) -> Rect {
    let mut it = glyphs.iter();
    let Some(first) = it.next() else {
        return Rect::new(0.0, 0.0, 0.0, 0.0);
    };
    // ⚠ 偏移**只**在这里加一次。四个边界各自加同一个 `offset` 等价于把
    // 整个矩形平移 —— 与逐字形平移再取并集**完全相同**（平移对 min/max 可交换）。
    // 写成一次而不是四次，是为了让"漏掉某一处平移"这件事在代码上就不可能发生。
    let (mut x0, mut y0, mut x1, mut y1) = (
        first.dst.x + offset.x,
        first.dst.y + offset.y,
        first.dst.right() + offset.x,
        first.dst.bottom() + offset.y,
    );
    for g in it {
        x0 = x0.min(g.dst.x + offset.x);
        y0 = y0.min(g.dst.y + offset.y);
        x1 = x1.max(g.dst.right() + offset.x);
        y1 = y1.max(g.dst.bottom() + offset.y);
    }
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}

/// 一批顶点的紧包围盒。
///
/// 与 [`union_of_quads`] 同一约定：空列表返回零尺寸矩形而非 `None`。
///
/// # 为什么遍历而不是在 `Command::Path` 上缓存一个包围盒字段
///
/// 缓存的代价是**每条命令**都多 8 字节，而包围盒只在带滤镜时才被读到 —— 图标
/// 里绝大多数命令 `filter` 是恒等链，这条字段对它们永远是死重量。遍历的代价
/// 则是有条件的（`filter.is_identity()` 时根本不进），且 CPU 侧本来就要多遍
/// 遍历同一批顶点（三角化、展开、光栅化）。
///
/// # NaN
///
/// `f32::min`/`max` 忽略 NaN（返回非 NaN 的那个），所以**个别**坏顶点会被静默
/// 跳过，不会污染结果；全部顶点都是 NaN 时返回 NaN 矩形 —— 与"一个 NaN 的
/// `Command::Quad::rect`"同样的下场。有意不特判：那需要一个"哪些顶点算数"的
/// 策略，而这套策略在别处（Quad/Image/Text 的矩形）并不存在，特判只会让
/// Path 成为唯一一条路径不同的命令。
pub fn bounds_of_vertices(vertices: &[Vertex]) -> Rect {
    let Some(first) = vertices.first() else {
        return Rect::new(0.0, 0.0, 0.0, 0.0);
    };
    let (mut x0, mut y0) = (first.position.x, first.position.y);
    let (mut x1, mut y1) = (x0, y0);
    for v in &vertices[1..] {
        x0 = x0.min(v.position.x);
        y0 = y0.min(v.position.y);
        x1 = x1.max(v.position.x);
        y1 = y1.max(v.position.y);
    }
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}
