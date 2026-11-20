#![forbid(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]
//! 后端无关的矢量绘制：从路径 / 文字描述到顶点流与 CPU 像素。
//!
//! 本 crate 拥有绘制所需的全部纯数据与算法：
//!
//! - [`core`] / [`paint`] / [`filter`] / [`decoration`]：绘制契约与基元；
//! - [`geom`] / [`icon`]：SVG 路径解析、展平、描边、三角化、图标注册表；
//! - [`text`]：文本排版、SDF 字形图集与 `ab_glyph` 字体来源；
//! - `render`：`DisplayList` → 顶点流、裁剪、离屏规划与 CPU 光栅；
//! - [`shaders`]：顶点布局的对偶端 —— 着色器源与预编译字节码契约。
//!
//! 依赖方向严格单向：**使用方 → 本库**，本库不依赖任何 workspace 内的 crate。
//! 门禁：`cargo tree -p zephyra-vector --depth 1` 只能出现 `ab_glyph` 与 `bytemuck`。

pub mod core;
pub mod decoration;
pub mod filter;
pub mod geom;
pub mod icon;
pub mod paint;
pub mod shaders;
pub mod render {
    pub mod cpu_raster;
    pub mod geom_clip;
    pub mod offscreen;
    pub mod pixels;
    pub mod quad;
    pub mod texture;
}
pub mod text;

// 让搬迁后的历史路径继续可用（`crate::quad` 等）。
pub use render::{cpu_raster, geom_clip, offscreen, pixels, quad, texture};

pub use render::pixels::{srgb_bytes, write_bmp, PixelRead};
pub use render::quad::RenderReport;
pub use render::texture::TextureNames;
pub use text::ab_glyph_source::{
    from_font_file, from_system_font, AbGlyphSource, FONT_OVERRIDE_ENV,
};

#[cfg(test)]
#[path = "../tests/unit/geom_tests.rs"]
mod geom_tests;
#[cfg(test)]
#[path = "../tests/unit/icon_tests.rs"]
mod icon_tests;
#[cfg(test)]
#[path = "../tests/unit/shader_sync_tests.rs"]
mod shader_sync_tests;
