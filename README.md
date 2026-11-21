# zephyra-vector

后端无关的矢量绘制库：路径几何、图标、文字排版与图集、顶点展开、离屏规划与 CPU 光栅。

本库不依赖任何 workspace 内的 crate，也不依赖系统库；依赖仅为 `ab_glyph`、`rustybuzz`、`bytemuck` 与 `unicode-*` 四个纯 Rust crate。

## 分层

- `core` / `filter` / `decoration`：纯数据类型与数学
- `geom` / `icon`：路径、三角化、SVG 解析
- `text`：`GlyphSource` / `TextStack` / `GlyphAtlas`，GSUB/GPOS、BiDi、UAX #14、COLR v0/v1（paint graph 精确栅格合成：渐变/变换/裁剪/混合）、CBDT/CBLC、可变字体、连字符、路径/旋转
- `render`：DisplayList → 顶点流、裁剪、离屏规划、CPU 参考光栅
- `shaders`：HLSL/WGSL 源与预编译 DXBC；主仓通过 `zephyra_fx::shaders` 消费

## 文字抗锯齿

字形用多档 SDF/MSDF 图集，绘制时用屏幕空间导数（`fwidth`）自适应平滑半宽；LCD 模式采用 Skia 12-tap 高斯 FIR 生成 R/G/B 子像素覆盖率，并做亮度感知 sRGB 覆盖校正。详见 `src/text/mod.rs` 的「已知限制」。

## 门禁

```powershell
cargo test                      # 单元 + shader 同步 + fxc 字节一致（需 fxc）
cargo tree -p zephyra-vector --depth 1
```

着色器源或字节码改动必须同步重编 `shaders/dxbc/*.bin`，否则 `shader_sync_tests` 会失败。

测试应用不放在本仓，见独立仓库 `zephyra-probes`。
