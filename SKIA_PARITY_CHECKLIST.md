# Skia 文字渲染对齐 —— 执行清单

> 本文件是跨会话的执行台账。每完成一项就勾选并在底部「进度日志」记一笔。
> 范围：第一档 5 项 + 第二档 4 项，全做，不设时间上限。

## 0. 环境与约束（动手前读）

- vector 仓：`E:\zephyra-vector`，分支 `main`，起点 `260d442`，工作区干净。
- 主仓：`E:\ZephyraUI`，分支 `main`，起点 `c08cc2a`（已含图集串位修复）。
- vector 是主仓的 git 依赖（主仓 Cargo.lock 锁 `#260d4429`）。**vector 改完必须 commit+push，主仓再 `cargo update -p zephyra-vector`**。
- 硬约束：
  - 不引入新的系统依赖 / Cargo 依赖（vector 严格依赖门禁）。
  - 字体发现 / FontMgr 归宿主，不在 vector 做。
  - shader 源码/二进制只在 `E:\zephyra-vector\shaders`，主仓只消费 `zephyra_fx::shaders` 常量；改 shader 要重编 DXBC 并过 fxc 字节一致门禁。
  - 所有文本文件 LF 行尾。
- Viewstage-Rust 锁着旧 vector `#d995088`；最后可选更新它做人工截图回归。

## Step 0：拉参考源码（只读，不并入仓库）

- [ ] `git clone --filter=blob:none --depth 1 https://github.com/google/skia.git E:\skia`
- [ ] `git -C E:\skia sparse-checkout set include/core include/skshaper src/core src/skshaper src/gpu/ganesh/text`
- [ ] `git clone --depth 1 https://github.com/Chlumsky/msdfgen.git E:\msdfgen`
- [ ] 读 Skia 关键文件：
  - 度量/Strut：`include/core/SkFontMetrics.h`、`include/core/SkStrutStyle.h`、`src/core/SkFont.cpp`、`src/core/SkStrutStyle.cpp`
  - features：`include/skshaper/SkShaper.h`、`src/skshaper/`
  - 装饰线：`src/core/SkFontPriv.cpp`、`src/text/`
  - 省略号：`src/text/SkParagraph.cpp`、`include/core/SkTextBlob.h`
  - LCD：`src/gpu/ganesh/text/`、`src/core/SkScalerContext.cpp`、`src/core/SkGlyphRunPainter.cpp`
  - COLR：`src/core/SkTypeface.cpp` / `src/core/SkScalerContext.cpp` 附近颜色字体分支
- [ ] 读 msdfgen：`src/msdfgen.cpp`、`src/editor.cpp`（`msdfErrorCorrection`）

## 批次 A：vector 仓（按序，每步一个 commit + 测试）

### A1 — 真实字体度量 / StrutStyle（第二档，先做）
- [x] `src/text/mod.rs` 新增 `FontMetrics`（ascent/descent/leading/x_height/cap_height/underline_thickness/underline_position + strikethrough）与 `StrutStyle`（可覆盖 ascent/descent/leading + force）
- [x] `GlyphSource` trait 加 `fn metrics(&self, px_size: f32) -> Option<FontMetrics>`（默认实现，保护测试替身）
- [x] `AbGlyphSource` 从 hhea + ttf-parser 的 x_height/capital_height/underline/strikeout 表读真实值；`FontSet` 用首字体
- [x] `stack.rs` 新增 `resolved_line_height()`：显式倍数 > strut > 真实度量 > 1.2×字号；装饰线默认厚度/位置改用度量
- [x] 测试 `tests/unit/text/metrics_tests.rs`（3 条）；全量 336 passed（333+3），commit `b368c44`
- [ ] 主仓可能需要同步：若 main 依赖默认行高 1.2 的布局断言，cargo update 后要调整（B1 时验证）

### A2 — OpenType features + word-spacing（第一档，整形层）
- [x] `TextStyle` 加 `font_features: Vec<(u32,u32)>`、`word_spacing: f32`
- [x] 透传：`AbGlyphSource::shape_run` 加 user_features 参数，用户同 tag 覆盖默认（liga/kern/mark…）；`FontSet::push_segment` 同步
- [x] word-spacing 按空格字素簇追加；顺带修正 letter_spacing 未计入 `pen`（行宽/对齐）的旧问题
- [x] 测试 `tests/unit/text/features_tests.rs`（4 条）；全量 340 passed，commit `d944b32`

### A3 — 装饰线样式（第一档，依赖 A1 度量默认值）
- [x] 按 Skia `TextStyle.h`：`TextDecorationStyle`（Solid/Double/Dotted/Dashed/Wavy）+ `TextDecorationMode`（Through/Gaps）+ `decoration_thickness_multiplier`
- [x] `stack.rs::push_decoration`：Dotted/Dashed 多段小矩形；Wavy 一串重叠小矩形近似正弦；走 RECT 路径，不动顶点布局
- [x] kGaps：用图集字形墨迹盒在穿过线的字处挖缺口（Skia `kGaps` / CSS skip-ink）
- [x] 测试 `tests/unit/text/decoration_tests.rs`（3 条）；全量 343 passed，commit `f3c709e`

### A4 — 文字阴影（第一档）
- [x] `TextShadow`（color/offset/blur_sigma，按 Skia TextShadow）+ `TextStyle::shadow`
- [x] `GlyphQuad` 加 `color: Option<Color>` 覆盖；`render/quad.rs` 逐字形用它 —— 阴影副本独立颜色
- [x] 阴影 quad 插在正文前；blur 用加宽 SDF smoothstep 带实现（无额外纹理/高斯）
- [x] 测试 `tests/unit/text/shadow_tests.rs`（2 条）；全量 345 passed，commit `65d4c9f`

### A5 — 省略号 / max_lines（第一档）
- [x] `TextStyle::max_lines`、`ellipsis`（默认 "…"）；`shape_impl` 超限截断 + 末行补省略号，退字保证不超 max_width
- [x] 测试 `tests/unit/text/ellipsis_tests.rs`（3 条）；全量 348 passed，commit `133b0ae`
- [ ] 注意：已有的 `ellipsize()` 单行字符串函数保留（不同用途）

### A6 — MSDF error correction（第二档）
- [x] 读 `E:\msdfgen\core\MSDFErrorCorrection.cpp`：其 apply 是把伪影纹素三通道压到中位值，保护颜色切换的拐角
- [x] `msdf::error_correct`：检测相邻纹素"中位通道中途切换"的插值伪影，只压平离边更远的那个，保护拐角
- [x] 在 `generate()` 末尾调用；既有"拐角仍尖"断言与新伪影/拐角断言均通过
- [x] 全量 349 passed，commit `fc6fb73`

### A7 — COLR/CPAL 彩色字形（第二档，vector 最大块）
- [ ] 自解析 COLR（base glyph + layer records）与 CPAL（palette BGRA）原始表字节，不引 skrifa
- [ ] `GlyphSource` 加 `fn color_layers(&self, glyph_id, px_size) -> Option<Vec<ColorLayer>>`（默认 None）；`AbGlyphSource` 实现
- [ ] atlas key 支持 `(glyph_id, layer_idx)` 编码成 u64（最侵入点，单独 commit，重点回归）
- [ ] `stack.rs` 每 layer 出带色 quad，复用现有 mask 采样通道
- [ ] 测试 + commit（CBDT/CBLC 位图 emoji 本轮不做：需 PNG 解码器，破零依赖约束）
- [ ] **A1–A7 全部 commit + push**

## 批次 B：主仓（vector push 之后）

### B1 — 适配新 TextStyle
- [ ] `cargo update -p zephyra-vector`（主仓）
- [ ] `zephyra-core/src/style.rs` 构造 TextStyle 处补新字段（`..Default::default()`）
- [ ] 确认 `node.rs`/`paint.rs` 缓存指纹未被破坏（TextStyle 派生 PartialEq，新字段自动进指纹）
- [ ] `cargo test -p zephyra-core` + `cargo check --workspace --tests`

### B2 — 逐通道 LCD 混合（第二档，最后，跨仓）
- [ ] 先读 Skia `src/gpu/ganesh/text/` 确认 GPU LCD 实际路径
- [ ] 方案二选一（都做能力检测 + 回退当前近似，老设备不能黑屏）：
  - 双源混合：片元 `SV_Target0=text·mask`、`SV_Target1=mask`，blend `ONE/INV_SRC1_COLOR`（需 SM5.1+/D3D11.3，核实 fxc 与目标设备；Vulkan 原生支持）
  - 两遍法：第一遍 `ZERO/INV_SRC_COLOR` 挖空 dst，第二遍 `ONE/ONE` 加 `text·mask`（通用，两次 draw）
- [ ] 影响面：`shaders/hlsl/shade.hlsli`、`shaders/wgsl/10_shade.wgsl.part`、DXBC 重编 + fxc 字节门禁、`zephyra-fx::shaders` 常量、D3D11/D3D12/Vulkan/winit 四处 blend state、`LcdOrder` RGB/BGR 顺序
- [ ] `TextStyle::lcd` 默认保持 None（opt-in）
- [ ] 测试 + fxc 门禁 + commit

### B3 — 验证与收尾
- [ ] vector：`cargo test`（基线 333 passed / 1 ignored 全绿）+ fxc/shader 同步门禁
- [ ] main：`cargo test -p zephyra-core` + `cargo check --workspace --tests`
- [ ] 全部文件 LF 行尾确认
- [ ] vector / main 分别 commit + push
- [ ] 可选：Viewstage-Rust `cargo update -p zephyra-vector` 后重编截图设置页，确认无串位复发、新装饰线/省略号/彩色 emoji 正常

## 明确不做（本轮）

- 可变字体轴 fvar/gvar（需 skrifa 或自写 gvar 插值，破依赖门禁）
- 文字沿路径 / 旋转缩放（要改 Command::Text + 三后端顶点展开）
- Hyphenation 词典
- 系统字体发现 / FontMgr（归宿主）
- CBDT/CBLC 位图 emoji（需 PNG 解码器）
- 宿主 `TextStack<AbGlyphSource>` → `TextStack<FontSet>` 切换（用户本轮未列，单独确认）

## 进度日志

（每完成一项记一行：日期 / 项 / commit / 备注）

- 2026-11-20 建立清单。起点：vector `260d442`、main `c08cc2a`。
