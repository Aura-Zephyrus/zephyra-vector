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
- [x] 主仓 `cargo update -p zephyra-vector` → `#81fb9f6a`
- [x] `style.rs` 用 `..Default::default()` 自动获得新字段；`line_height` 未设置时改给 `None`，让字体真实度量生效
- [x] `cargo test -p zephyra-core` 全绿、`cargo check --workspace --tests` 通过；commit `e252c69`

### B2 — 逐通道 LCD 混合（第二档，最后，跨仓）
- [x] 读 Skia `src/gpu/ganesh/text/` 与 msdfgen 参考
- [x] **采用方案：shader 内逐通道覆盖率**（不换 blend 状态、不升 SM5.1）：
  - LCD 分支取三次 MSDF 样本得 `cov.rgb`，直接乘进源颜色；`alpha = max(cov)` 供 SrcOver 决定背景让位；
  - 修正原先 `out_rgb * coverage` 把逐通道覆盖率又乘一次标量均值的错误；
  - HLSL/WGSL 同步修改，25 个 DXBC 全部重编，fxc 字节一致门禁通过。
- [x] 说明：这是"逐通道源覆盖"，不是 GPU 双源混合那种逐通道 dst 系数；但对三条子像素分别着色，已明显优于均值近似，且零后端改动、无设备兼容风险。
- [x] `TextStyle::lcd` 默认 None（opt-in）；commit `81fb9f6`（vector）

### B3 — 验证与收尾
- [x] vector：`cargo test` 352 passed / 0 failed / 1 ignored；fxc/shader 同步门禁通过
- [x] main：`cargo test -p zephyra-core` 全绿；`cargo check --workspace --tests` 通过；`cargo test --workspace` 83 个 test result 全 ok
- [x] LF 行尾确认（新增文件均 LF）
- [x] vector push 成功（`81fb9f6`）；main 待 push
- [ ] 可选：Viewstage-Rust `cargo update -p zephyra-vector` 后重编截图设置页，确认无串位复发、新装饰线/省略号/彩色 emoji 正常

## 补全阶段（用户 2026-11-21 要求「补全」——把上面明确不做的也做掉）

### C1 — 宿主 FontSet 接线 + 字体发现
- [x] vector：`from_system_font_set()` / `from_font_file_set()` 构造多字体栈（系统候选 + emoji 字体，OS/2 字重类）
- [x] main：`TextEngine = TextStack<FontSet>`；4 个宿主构造函数改用 set 版本；新增 `Font.Family` / `Font.Weight` 属性并接到 TextStyle
- [x] `cargo test -p zephyra-core` 全绿、`cargo check --workspace --tests` 通过；commits `8aecbc7`(vector) / `49228bc`(main)

### C2 — CBDT/CBLC 位图 emoji（零新增依赖）
- [x] 自写 `src/text/png.rs`：DEFLATE(stored/fixed/dynamic) + 5 种 PNG filter，8-bit RGB/RGBA/灰度
- [x] `AbGlyphSource::raster_bitmap` 用 ttf-parser 的 `glyph_raster_image` 取 PNG 解码，带留白写进 RGBA 图集；`GlyphMask::ColorRgba`（mode 5）+ HLSL/WGSL + DXBC 重编
- [x] 测试：合成 PNG 解码 4 色断言通过；本机无 CBDT 字体，Segoe UI Emoji 走 COLR 路径（已验证）
- [x] commit `7bb7818`

### C3 — 可变字体轴 fvar/gvar
- [x] 用现成 `ab_glyph::VariableFont` + ttf-parser `set_variation`（outline_glyph 自动应用 gvar）
- [x] `TextStyle::variations`，`GlyphSource::set_variations`；ab_glyph face、MSDF 轮廓、rustybuzz face 三处同步
- [x] 真实 SegUIVar.ttf 验证 wght 400→700 改变步进与位图；commit `b41bce9`

### C4 — Hyphenation（无词典文件的规则回退）
- [x] `hyphenation_opportunities`：V|CV / VC|CV 拉丁音节启发式；`TextStyle::hyphens`
- [x] 断词时行尾补 '-' 字形；2 条测试，commit `d714d19`

### C5 — 文字沿路径 / 旋转缩放
- [x] `TextStyle::rotation` / `TextPath::Arc`；`GlyphQuad::rotation` 与旋转角点顶点展开（零后端改动）
- [x] 2 条测试（顶点位置变化、圆弧旋转递变），commit `9fb6146`

### C6 — Viewstage-Rust 回归
- [x] `cargo update -p zephyra-vector` → `#8aecbc7d`，release + debug 构建成功
- [x] 以 `ZEPHYRA_DEBUG_PORT=7878` 启动 debug 版，打开菜单→设置窗口
- [x] `find_by_text` 命中「设置以临时文件替换方式写入，损坏时自动备份为 .json.corrupt」——中文正常、无串位复发；截图存于临时目录
- [x] Viewstage-Rust 的 Cargo.lock/Cargo.toml 有大量用户未提交改动，未代为提交

## 进度日志

（每完成一项记一行：日期 / 项 / commit / 备注）

- 2026-11-20 建立清单。起点：vector `260d442`、main `c08cc2a`。
