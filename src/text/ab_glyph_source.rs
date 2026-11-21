//! 字形来源：把 `ab_glyph` 适配到 base 层为文字做的**唯一**一件事。
//!
//! # 为什么只做这一件事
//!
//! 文本排版的一切都在 core 的 [`crate::text`]：拆行、基线、逐字形四边形、图集打包
//! 与 uv。这里只回答一个问题：**怎么把字符变成位图**，也就是怎么把 `.ttf` / `.otf`
//! 在内存里接成 core 的 [`GlyphSource`]。
//!
//! ```text
//! core                              base（这里）
//! 排版 + 图集 + uv → Command::Text   AbGlyphSource：唯一实现
//! GlyphSource  ?───────────────────▶
//! TextStack    拆行 + 基线 + 打包
//! GlyphAtlas   CPU 位图            宿主 upload_atlas(pixels) → GPU 纹理
//! ```
//!
//! 把这层抽成独立 crate（`ab_glyph` 或 `cosmic-text` 或 CoreText）= 换一个实现，
//! core 一行不用改。
//!
//! # 与 Skia 的一个关键差异：不做子像素位置量化
//!
//! Skia 的 `SkPackedGlyphID`（`src/core/SkGlyph.h`）把「字形 id + 子像素 x + 子像素 y」
//! 打包进一个 u32，其中子像素位置**只有 2 bit** —— 即量化到 1/4 像素：
//!
//! ```c
//! static constexpr uint32_t kSubPixelPosLen = 2u;
//! x = (x - floorf(x)) + 1.0f;
//! sub_x = (int)(x * (1u << (kSubPixelPosLen + kSubPixelX))) & kSubPixelPosMask;
//! ```
//!
//! **这里刻意不做这件事**，因为两者的架构前提不同：
//!
//! | | Skia | 本项目 |
//! |---|---|---|
//! | 位图按什么位置栅格化 | 每个量化子像素位置各存一份 | 每个 (font, glyph, px) **一份** |
//! | 绘制时 | 画在整数位置 + 量化后的子像素偏移 | 顶点是浮点，**GPU 自己**做亚像素光栅化 |
//!
//! Skia 要量化，是因为它把位图缓存当作**最终产物** —— 位置必须进键。而本项目把位图只当作
//! 「覆盖率来源」，落点是浮点顶点，由 GPU 做亚像素采样。照搬量化会：
//!
//! - 把字形位置**吸附到 1/4 像素**，小字号下肉眼可见地抖；
//! - 换不到任何好处 —— 我们的键本来就没有子像素这一维。
//!
//! 所以「参考 Skia」在这里的结论是**不要**照抄这一项。
//!
//! # `AtlasRaster.coverage` 的坐标契约（踩过一次）
//!
//! 本模块返回的 `coverage` 的 `(0,0)` 就是**字形左上角**，行宽 = `width`。
//! 它**不是**相对基线原点的坐标 —— 基线偏移由 [`AtlasRaster::offset`] 单独给出，
//! 由 core 拿去算 `GlyphQuad::dst`。
//!
//! 违反这条的后果极端：core 的图集原本据此把 `offset` 又减了一次，而大写字母的
//! `floor(offset.y)` 就是 cap 高度（32px 的 'H' 约 22），于是每个像素都被越界判断
//! 跳过 —— **图集全空，屏幕上字完全不出现**。见 `zephyra/src/text/atlas.rs` 里的修正，
//! 以及 `zephyra/src/text/atlas_tests.rs` 里用假字形来源钉死的契约测试。
use std::cell::RefCell;
use std::collections::HashMap;

use crate::core::Point;
use crate::text::color::parse_color_glyph;
use crate::text::msdf::outline_segments;
use crate::text::png::decode_png;
use crate::text::sdf::{SDF_BASE_PX, SDF_PADDING, SDF_SPREAD};
use crate::text::shape::{bidi_runs, dominant_script, rustybuzz_script, ShapedGlyph, ShapedRun};
use crate::text::{AtlasRaster, ColorLayer, FontMetrics, GlyphSource, TextStack, TextStyle};
use ab_glyph::{Font, FontVec, GlyphId, PxScale, ScaleFont, VariableFont};
use rustybuzz::ttf_parser::Tag;

/// 四字节 OpenType tag 的两种表示（用户 API 用 u32，rustybuzz 用 `[u8; 4]`）。
fn feature_tag_bytes(name: &str) -> [u8; 4] {
    let b = name.as_bytes();
    [b[0], b[1], b[2], b[3]]
}

fn feature_tag(name: &str) -> u32 {
    u32::from_be_bytes(feature_tag_bytes(name))
}

/// ttf-parser 的 CBDT `image.y` 是 y-up 的上边界；图集要 y-down。
fn bitmap_offset(x: i16, y: i16) -> Point {
    Point::new(x as f32, -(y as f32))
}

/// 基于 ab_glyph 的字形来源。
///
/// 只做三件事：查字形 id、算步进、栅格化。三者都是 `ab_glyph` 的直接映射 —— 这里
/// 没有任何"排版策略"，那全在 core 的 [`TextStack`] 里。
#[derive(Debug)]
pub struct AbGlyphSource {
    font: FontVec,
    font_id: u32,
    /// COLR 彩色分层的按需缓存。Segoe UI Emoji 的 v1 paint graph 全量解析
    /// 有 3487 个 glyph，构造时全跑会卡住启动；只在实际排版到某个字形时解析。
    /// key = (palette, glyph_id)：切换调色板必须重新解析层颜色。
    color_cache: RefCell<HashMap<(u16, u32), Vec<ColorLayer>>>,
    /// 当前生效的可变字体轴；用来做幂等跳过与回退默认轴。
    applied_variations: Vec<([u8; 4], f32)>,
    /// 当前 CPAL 调色板索引（COLR v1）。
    palette: u16,
    /// cmap 覆盖查询缓存：`FontSet::select` 每帧对每字符每字体都要查，
    /// 不能每次都重新 parse 一遍字体表。
    coverage_cache: RefCell<HashMap<char, bool>>,
}

impl AbGlyphSource {
    /// 从字体字节构造。
    ///
    /// `font_id` 是**调用方给的稳定整数**，而不是字体指针或名称的哈希：图集要按
    /// `(font_id, glyph_id, px_size)` 做键，若用哈希字符串，每帧每个字形都要算一次
    /// 字符串哈希。同一个 `font_id` 必须始终对应同一份字体字节 —— 否则同一个字形会
    /// 在图集里存多份。
    pub fn from_bytes(font_id: u32, bytes: &[u8]) -> Result<Self, String> {
        let font = FontVec::try_from_vec(bytes.to_vec())
            .map_err(|e| format!("字体解析失败（可能是 CFF/可变字体或文件已损坏）: {e}"))?;
        Ok(Self {
            font,
            font_id,
            color_cache: RefCell::new(HashMap::new()),
            applied_variations: Vec::new(),
            palette: 0,
            coverage_cache: RefCell::new(HashMap::new()),
        })
    }

    /// 原始字体字节（供回退链查询 cmap 覆盖、重建 rustybuzz face）。
    pub(crate) fn font_bytes(&self) -> &[u8] {
        self.font.as_slice()
    }

    /// 字体支持的可变轴列表（空表示不可变字体）。
    pub fn variation_axes(&self) -> Vec<ab_glyph::VariationAxis> {
        self.font.variations()
    }

    /// OS/2 字重类（400 = Regular），供回退链按 family/weight 选字体。
    pub(crate) fn weight_class(&self) -> u16 {
        let Ok(face) = rustybuzz::ttf_parser::Face::parse(self.font.as_slice(), 0) else {
            return 400;
        };
        face.weight().to_number()
    }

    /// 字符覆盖查询（带缓存）。`FontSet` 选字体时高频调用，不能每次 parse 表。
    pub(crate) fn covers(&self, ch: char) -> bool {
        if let Some(&v) = self.coverage_cache.borrow().get(&ch) {
            return v;
        }
        let v = rustybuzz::ttf_parser::Face::parse(self.font.as_slice(), 0)
            .ok()
            .and_then(|face| face.glyph_index(ch))
            .map(|g| g.0 != 0)
            .unwrap_or(false);
        self.coverage_cache.borrow_mut().insert(ch, v);
        v
    }

    /// 字体的 units-per-em（供回退链算 `px / upem` 缩放）。
    pub(crate) fn units_per_em(&self) -> f32 {
        self.font.units_per_em().unwrap_or(1000.0).max(1.0)
    }

    /// **em 像素 → ab_glyph 的 `PxScale`**。全部字号换算的唯一入口。
    ///
    /// # 为什么必须有这一刀
    ///
    /// ab_glyph 的 `PxScale::from(x)` 的语义是**行高**（`hhea` 的
    /// `ascent − descent (+ lineGap)`），不是 em 尺寸：`as_scaled(PxScale::from(45))`
    /// 之后 `.height() == 45.0`，而 `.height()` 是行高。而 CSS / 本框架的
    /// `TEXT_SIZE` 给的是 **em**（`font-size`）。
    ///
    /// 直接把 em 当 `PxScale` 传的后果：整份文本按 `upem / height_unscaled` 的比例
    /// 缩小。Segoe UI 的 hhea 行高 = 1.33 em，于是**所有文字一律渲染成 0.75 倍** ——
    /// 而布局（步进、行高）仍按 em 算：字变小、行距变松、不同字号之间对不齐。
    /// 实测症状：34px 的 'Z' 位图只有 15×18（应为 19×24）。
    ///
    /// 两个调用点（`advance` / `rasterize`）都必须经过这里 —— 漏一个的后果是
    /// 「步进按 em、位图按行高」，字形互相叠压。
    fn px_scale(&self, px_em: f32) -> PxScale {
        let upem = self.font.units_per_em().unwrap_or(1000.0).max(1.0);
        // hhea 行高（字体单位）。ascent 为正、descent 为负，相减就是总行高。
        let line = self.font.height_unscaled().max(1.0);
        PxScale::from(px_em.max(1.0) * line / upem)
    }

    /// 只取字形覆盖率（不生成 MSDF），供 COLR paint graph 的层合成用。
    fn coverage_for(&mut self, glyph_id: u32, px_size: u32) -> Option<(u32, u32, Vec<u8>, Point)> {
        let px = px_size.max(SDF_BASE_PX) as f32;
        let id = GlyphId(glyph_id.min(u16::MAX as u32) as u16);
        let scale = self.px_scale(px);
        let scaled = self.font.as_scaled(scale);
        let outlined = scaled.outline_glyph(id.with_scale(scale))?;
        let bounds = outlined.px_bounds();
        let (w, h) = (bounds.width() as u32, bounds.height() as u32);
        if w == 0 || h == 0 {
            return None;
        }
        let mut coverage = vec![0u8; (w * h) as usize];
        outlined.draw(|x, y, alpha| {
            let (ax, ay) = (x as usize, y as usize);
            if ax >= w as usize || ay >= h as usize {
                return;
            }
            coverage[ay * w as usize + ax] = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
        });
        Some((w, h, coverage, Point::new(bounds.min.x.floor(), bounds.min.y.floor())))
    }

    /// COLR paint graph 精确合成：渐变、变换、裁剪、层混合全部在栅格化时求值，
    /// 产出一张直出 RGBA 位图，shader 只需 mode 5 采样，无任何近似回退。
    fn raster_color_graph(&mut self, glyph_id: u32, px_size: u32) -> Option<AtlasRaster> {
        use crate::text::color_raster::compose_color_layers;
        let layers = self.color_layers(glyph_id)?;
        let px = px_size.max(SDF_BASE_PX);
        let upem = self.units_per_em();
        let scale = px as f32 / upem.max(1.0);
        let face = rustybuzz::ttf_parser::Face::parse(self.font.as_slice(), 0).ok()?;
        let bbox = face.glyph_bounding_box(rustybuzz::ttf_parser::GlyphId(
            glyph_id.min(u16::MAX as u32) as u16,
        ))?;
        let (xmin, ymin, xmax, ymax) = (
            bbox.x_min as f32,
            bbox.y_min as f32,
            bbox.x_max as f32,
            bbox.y_max as f32,
        );
        let out_rect = crate::core::Rect::new(
            xmin * scale,
            -ymax * scale,
            (xmax - xmin) * scale,
            (ymax - ymin) * scale,
        );

        // 覆盖率按层字形缓存（同一字形可能出现在多层）。
        let mut cache: HashMap<u32, Option<(u32, u32, Vec<u8>, Point)>> = HashMap::new();
        for layer in &layers {
            if !cache.contains_key(&layer.glyph_id) {
                let cov = self.coverage_for(layer.glyph_id, px);
                cache.insert(layer.glyph_id, cov);
            }
        }
        let cov_map: HashMap<u32, (u32, u32, Vec<u8>, Point)> = cache
            .into_iter()
            .filter_map(|(k, v)| v.map(|v| (k, v)))
            .collect();

        let (w, h, rgba) = compose_color_layers(
            &layers,
            out_rect,
            px,
            upem,
            SDF_PADDING,
            |gid, _px| cov_map.get(&gid).cloned(),
        )?;
        Some(AtlasRaster {
            offset: Point::new(out_rect.x, out_rect.y),
            // compose 返回的 w/h 已含留白；AtlasRaster 的 width/height 只记墨迹。
            width: w - 2 * SDF_PADDING,
            height: h - 2 * SDF_PADDING,
            sdf: rgba,
            spread: SDF_SPREAD,
            base_px: px as f32,
            is_bitmap: true,
        })
    }

    /// CBDT/CBLC（或 sbix/EBDT）内嵌位图 → 带留白的 RGBA 图集条目。
    fn raster_bitmap(&self, glyph_id: u32, px_size: u32) -> Option<AtlasRaster> {
        use rustybuzz::ttf_parser::RasterImageFormat;
        let face = rustybuzz::ttf_parser::Face::parse(self.font.as_slice(), 0).ok()?;
        let image = face.glyph_raster_image(
            rustybuzz::ttf_parser::GlyphId(glyph_id.min(u16::MAX as u32) as u16),
            px_size.min(u16::MAX as u32) as u16,
        )?;
        if image.format != RasterImageFormat::PNG {
            return None;
        }
        let (w, h, rgba) = decode_png(image.data)?;
        if w == 0 || h == 0 {
            return None;
        }
        let (m, pw) = (SDF_PADDING, w + 2 * SDF_PADDING);
        let ph = h + 2 * SDF_PADDING;
        let mut sdf = vec![0u8; (pw * ph * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let src = ((y * w + x) * 4) as usize;
                let dst = (((y + m) * pw + x + m) * 4) as usize;
                sdf[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
            }
        }
        Some(AtlasRaster {
            // 见 `bitmap_offset`：CBDT y-up 上边界要转成图集的 y-down 偏移。
            offset: bitmap_offset(image.x, image.y),
            width: w,
            height: h,
            sdf,
            spread: SDF_SPREAD,
            base_px: image.pixels_per_em as f32,
            is_bitmap: true,
        })
    }

    /// 字体里是否真的有这个字形。
    ///
    /// `ab_glyph` 的 `glyph_id` 对缺失字符返回 `.notdef`（id 0），**不是**错误。
    /// 而多数字体的 `.notdef` 是空轮廓 —— 缺字会静默消失。所以要显示豆腐框就得
    /// 显式替换，而 [`GlyphSource::glyph_id`] 的文档要求实现做这件事。
    fn has_glyph(&self, id: GlyphId) -> bool {
        id.0 != 0
    }

    /// 缺字时用于替换 `.notdef` 的可见字形 id。
    fn replacement_id(&self) -> u32 {
        let id = self.font.glyph_id('\u{FFFD}');
        if self.has_glyph(id) {
            id.0 as u32
        } else {
            0
        }
    }

    /// 对单个 BiDi run 做 rustybuzz 整形。
    ///
    /// `pub(crate)` 供 `FontSet` 逐字体分段复用 —— 回退链仍需同样的 GSUB/GPOS，
    /// 只是按覆盖范围把一段文本切成若干段，各段用自己的字体整形。
    pub(crate) fn shape_run(
        &self,
        run_text: &str,
        rtl: bool,
        px_size: f32,
        scale: f32,
        user_features: &[(u32, u32)],
    ) -> ShapedRun {
        let script = dominant_script(run_text);
        let mut face = match rustybuzz::Face::from_slice(self.font.as_slice(), 0) {
            Some(f) => f,
            None => {
                // 字体字节已被 `from_bytes` 验证过，理论上不会走到这里；真到了就
                // 退化成逐字符，至少保证文字可见。
                let mut glyphs = Vec::new();
                for (i, ch) in run_text.char_indices() {
                    let end = i + ch.len_utf8();
                    let id = self.font.glyph_id(ch);
                    glyphs.push(ShapedGlyph {
                        glyph_id: id.0 as u32,
                        font_id: self.font_id,
                        cluster: i..end,
                        advance: self.font.as_scaled(self.px_scale(px_size)).h_advance(id),
                        offset: Point::ZERO,
                        color_glyph: false,
                        embolden: 0.0,
                    });
                }
                return ShapedRun {
                    glyphs,
                    rtl,
                    script: "Zyyy",
                };
            }
        };

        // 与 ab_glyph 侧保持同一套可变轴：整形的步进（含 HVAR/VVAR）也随之变化。
        for (tag, value) in &self.applied_variations {
            let _ = face.set_variation(Tag::from_bytes(tag), *value);
        }

        // 显式打开常用特性：连字、上下文替代、字距对、mark 定位。
        // 字体里没有的表会被 rustybuzz 安全忽略。用户给的同 tag 项覆盖默认值
        // （值 0 即关闭）；这与 Skia 的 feature settings 合并规则一致。
        let mut features: Vec<rustybuzz::Feature> = [
            "liga", "clig", "calt", "ccmp", "kern", "mark", "mkmk", "locl",
        ]
        .iter()
        .filter(|name| !user_features.iter().any(|(t, _)| *t == feature_tag(name)))
        .map(|tag| rustybuzz::Feature::new(Tag::from_bytes(&feature_tag_bytes(tag)), 1, ..))
        .collect();
        for &(tag, value) in user_features {
            features.push(rustybuzz::Feature::new(Tag::from_bytes(&tag.to_be_bytes()), value, ..));
        }

        let mut buffer = rustybuzz::UnicodeBuffer::new();
        // cluster 用**字节偏移**，这样 letter_spacing 能按字素簇追加。
        for (i, ch) in run_text.char_indices() {
            buffer.add(ch, i as u32);
        }
        buffer.set_direction(if rtl {
            rustybuzz::Direction::RightToLeft
        } else {
            rustybuzz::Direction::LeftToRight
        });
        if let Some(s) = rustybuzz_script(script) {
            buffer.set_script(s);
        }
        buffer.set_cluster_level(rustybuzz::BufferClusterLevel::MonotoneGraphemes);
        let shaped = rustybuzz::shape(&face, &features, buffer);

        let infos = shaped.glyph_infos();
        let positions = shaped.glyph_positions();
        // cluster 的 end 取"下一个更大的 cluster 起点"，没有就到 run 末尾。
        // 这对 LTR（0,1,4…）与 RTL（6,4,2,0）都成立；连字因此覆盖多个字符。
        let mut starts: Vec<usize> = infos
            .iter()
            .map(|i| (i.cluster as usize).min(run_text.len()))
            .collect();
        starts.sort_unstable();
        starts.dedup();
        let mut glyphs = Vec::with_capacity(infos.len());
        for (info, pos) in infos.iter().zip(positions.iter()) {
            let start = (info.cluster as usize).min(run_text.len());
            let end = starts
                .binary_search(&start)
                .ok()
                .and_then(|i| starts.get(i + 1).copied())
                .unwrap_or(run_text.len());
            let id = if info.glyph_id == 0 {
                self.replacement_id()
            } else {
                info.glyph_id
            };
            glyphs.push(ShapedGlyph {
                glyph_id: id,
                font_id: self.font_id,
                cluster: start..end,
                // ab_glyph 的 h_advance 用 hhea 行高比例；整形也必须乘同一比例，
                // 否则同一段文字"测量"与"绘制"的步进不一致。
                advance: pos.x_advance as f32 * scale,
                // HarfBuzz 的 y 向上为正；核心坐标系 y 向下，所以取负。
                offset: Point::new(pos.x_offset as f32 * scale, -(pos.y_offset as f32) * scale),
                color_glyph: self.color_layers(id).is_some(),
                embolden: 0.0,
            });
        }
        ShapedRun {
            glyphs,
            rtl,
            script: script.short_name(),
        }
    }
}

/// 候选系统字体路径。
///
/// 刻意**不**引入 `fontdb` 去枚举字体目录：那会遍历数百个文件（本机实测上千个），
/// 而这里只需要一个能显示字形的字体。真实应用应当用 fontdb 加用户偏好。
///
/// # ⚠ 首个候选不含 CJK —— 这是本表最容易被误读的地方
///
/// `segoeui.ttf` 只有拉丁与部分符号，**没有汉字**。而 [`from_system_font`] 命中第一个
/// 就返回，所以排第二的 `msyh.ttc`（含 CJK）**永远轮不到**，中文一律走 U+FFFD
/// 变豆腐块。
///
/// 顺序不是「按显示效果排的」，只是「挑一个存在的字体」—— 曾经没人在意拉丁字体的
/// 观感，于是中文坏掉也没人发现。要显示中文就设 [`FONT_OVERRIDE_ENV`]，
/// 或把一份含 CJK 的字体挪到表首（代价：拉丁观感变）。
pub(crate) const FONT_CANDIDATES: &[&str] = &[
    // Windows
    "C:\\Windows\\Fonts\\segoeui.ttf",
    "C:\\Windows\\Fonts\\msyh.ttc", // 微软雅黑 —— 含 CJK
    "C:\\Windows\\Fonts\\arial.ttf",
    "C:\\Windows\\Fonts\\tahoma.ttf",
    // Linux
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    // macOS
    "/System/Library/Fonts/Helvetica.ttc",
    "/System/Library/Fonts/PingFang.ttc",
];

/// 环境变量名：指定字体文件的绝对或相对路径，**优先于**内置候选表。
///
/// # 为什么需要它
///
/// `FONT_CANDIDATES` 里第一个 `segoeui.ttf` **不含 CJK**，于是中文全变豆腐块
/// （`glyph_id` 退到 U+FFFD）。这不是 bug，是那份字体的客观事实 —— 候选表里
/// 排第二的 `msyh.ttc` 含 CJK，但它**永远轮不到**，因为 `from_system_font` 命中第一个
/// 就返回。
///
/// 换掉首选字体的两种办法：
///
/// - 把系统首选改成一份含 CJK 的字体 —— 代价是拉丁字母的观感随机器而变；
/// - 指定一份随程序分发的字体 —— 观感在任何机器上都一样。
///
/// gallery 走后者（`gallery/fonts/MiSans-Regular.ttf`，含 CJK 与拉丁）。
pub const FONT_OVERRIDE_ENV: &str = "ZEPHYRA_FONT";

/// 从指定路径造一个文本栈。
///
/// 与 [`from_system_font`] 的差别只有「字体从哪来」，其余（失败时退回禁用模式而不
/// panic）完全一致。
pub fn from_font_file(path: &str) -> (TextStack<AbGlyphSource>, String) {
    match std::fs::read(path) {
        Ok(bytes) if !bytes.is_empty() => match AbGlyphSource::from_bytes(0, &bytes) {
            Ok(src) => {
                let stack = TextStack::new(src);
                (
                    stack,
                    format!(
                        "字体 {path}（{} 字节，图集 {}×{}）",
                        bytes.len(),
                        crate::text::atlas::ATLAS_SIZE,
                        crate::text::atlas::ATLAS_SIZE
                    ),
                )
            }
            Err(e) => (
                TextStack::disabled(AbGlyphSource {
                    font: FontVec::try_from_vec(minimal_sfnt()).expect("最小 sfnt 头应可解析"),
                    font_id: u32::MAX,
                    color_cache: RefCell::new(HashMap::new()),
                    applied_variations: Vec::new(),
            palette: 0,
            coverage_cache: RefCell::new(HashMap::new()),
                }),
                format!("字体 {path} 解析失败（{e}），已退回无文本模式"),
            ),
        },
        Ok(_) => (
            TextStack::disabled(AbGlyphSource {
                font: FontVec::try_from_vec(minimal_sfnt()).expect("最小 sfnt 头应可解析"),
                font_id: u32::MAX,
                color_cache: RefCell::new(HashMap::new()),
                applied_variations: Vec::new(),
            palette: 0,
            coverage_cache: RefCell::new(HashMap::new()),
            }),
            format!("字体 {path} 是空文件，已退回无文本模式"),
        ),
        Err(e) => (
            TextStack::disabled(AbGlyphSource {
                font: FontVec::try_from_vec(minimal_sfnt()).expect("最小 sfnt 头应可解析"),
                font_id: u32::MAX,
                color_cache: RefCell::new(HashMap::new()),
                applied_variations: Vec::new(),
            palette: 0,
            coverage_cache: RefCell::new(HashMap::new()),
            }),
            format!("读不到字体 {path}（{e}），已退回无文本模式"),
        ),
    }
}

/// 挑一个可用的系统字体 —— 或用 [`FONT_OVERRIDE_ENV`] 指定的字体。
///
/// # 优先读环境变量
///
/// 设了 [`FONT_OVERRIDE_ENV`] 就用它，且**不**再尝试系统字体。这样「指定字体」这件事
/// 是宿主的事，不用改宿主的候选表。
pub fn from_system_font() -> (TextStack<AbGlyphSource>, String) {
    if let Ok(path) = std::env::var(FONT_OVERRIDE_ENV) {
        if !path.is_empty() {
            return from_font_file(&path);
        }
    }
    let mut tried = Vec::new();
    for (i, path) in FONT_CANDIDATES.iter().enumerate() {
        match std::fs::read(path) {
            Ok(bytes) if !bytes.is_empty() => match AbGlyphSource::from_bytes(i as u32, &bytes) {
                Ok(src) => {
                    let stack = TextStack::new(src);
                    return (
                        stack,
                        format!(
                            "字体 {}（{} 字节，图集 {}×{}）",
                            path,
                            bytes.len(),
                            crate::text::atlas::ATLAS_SIZE,
                            crate::text::atlas::ATLAS_SIZE
                        ),
                    );
                }
                Err(e) => tried.push(format!("{path}（{e}）")),
            },
            Ok(_) => tried.push(format!("{path}（空文件）")),
            Err(_) => tried.push(path.to_string()),
        }
    }
    (
        TextStack::disabled(AbGlyphSource {
            // 占位字体：`font_id` 任意，因为 disabled 模式不产出任何字形。
            font: FontVec::try_from_vec(minimal_sfnt()).expect("最小 sfnt 头应可解析"),
            font_id: u32::MAX,
            color_cache: RefCell::new(HashMap::new()),
            applied_variations: Vec::new(),
            palette: 0,
            coverage_cache: RefCell::new(HashMap::new()),
        }),
        format!(
            "未找到可用系统字体（试过 {} 个路径），已退回无文本模式",
            tried.len()
        ),
    )
}

/// 最小 sfnt 头：12 字节 sfnt + 16 字节表记录，表数为 0。
///
/// 只在"无字体模式"下使用 —— 那时任何查询都返回零尺寸/零字形，字体本身永不被查询。
/// 构造它是为了让 [`FontVec`] 能构造成功（它要求非空字节）。
fn minimal_sfnt() -> Vec<u8> {
    let mut v = vec![0u8; 12 + 16];
    v[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes()); // sfnt version 1.0
                                                            // numTables = searchRange = entrySelector = rangeShift 全为 0
    v
}

impl GlyphSource for AbGlyphSource {
    fn glyph_id(&mut self, ch: char) -> u32 {
        let id = self.font.glyph_id(ch);
        if self.has_glyph(id) {
            return id.0 as u32;
        }
        // 缺字：换成 U+FFFD（REPLACEMENT CHARACTER）。多数字体的替换字符是个豆腐框
        // 或问号 —— 让用户看见"这里有个字不认识"，好过静默留白。
        let replacement = self.font.glyph_id('\u{FFFD}');
        if self.has_glyph(replacement) {
            replacement.0 as u32
        } else {
            // 连替换字符都没有：退回 .notdef（多半是空轮廓）。
            id.0 as u32
        }
    }

    fn advance(&mut self, glyph_id: u32, px_size: f32) -> f32 {
        let scale = self.px_scale(px_size);
        self.font
            .as_scaled(scale)
            .h_advance(GlyphId(glyph_id.min(u16::MAX as u32) as u16))
    }

    fn set_palette(&mut self, palette: u16) {
        if self.palette == palette {
            return;
        }
        self.palette = palette;
        // 层颜色来自 CPAL，调色板变了必须整套重解。
        self.color_cache.borrow_mut().clear();
    }

    fn set_variations(&mut self, variations: &[([u8; 4], f32)]) {
        if self.applied_variations.as_slice() == variations {
            return;
        }
        // 先把所有轴回默认值，避免上一次的设置在本次未指定的轴上残留。
        for axis in self.font.variations() {
            let _ = self.font.set_variation(&axis.tag, axis.default_value);
        }
        for (tag, value) in variations {
            let _ = self.font.set_variation(tag, *value);
        }
        self.applied_variations = variations.to_vec();
    }

    fn color_layers(&self, glyph_id: u32) -> Option<Vec<ColorLayer>> {
        let key = (self.palette, glyph_id);
        if let Some(cached) = self.color_cache.borrow().get(&key) {
            return (!cached.is_empty()).then(|| cached.clone());
        }
        let layers =
            parse_color_glyph(self.font.as_slice(), glyph_id, self.palette).unwrap_or_default();
        let result = (!layers.is_empty()).then(|| layers.clone());
        self.color_cache.borrow_mut().insert(key, layers);
        result
    }

    fn metrics(&self, px_size: f32) -> Option<FontMetrics> {
        if px_size <= 0.0 {
            return None;
        }
        // 用 font units / upem × em 字号缩放。ab_glyph 的 `ascent_unscaled` 等
        // 直接来自 ttf-parser 的 hhea，已经是字体单位；**不能**走 `px_scale`
        // —— 那里面乘了 hhea 行高/em，会把升部再放一次。
        let s = px_size / self.units_per_em();
        let face = rustybuzz::ttf_parser::Face::parse(self.font.as_slice(), 0).ok()?;
        let underline = face.underline_metrics();
        let strikeout = face.strikeout_metrics();
        Some(FontMetrics {
            ascent: self.font.ascent_unscaled() * s,
            descent: self.font.descent_unscaled() * s,
            leading: self.font.line_gap_unscaled() * s,
            x_height: face.x_height().unwrap_or(0) as f32 * s,
            cap_height: face.capital_height().unwrap_or(0) as f32 * s,
            // ttf-parser 的 position 是 y 向上（下划线在基线下方 ⇒ 负值），
            // 这里统一成 y 向下、基线下方为正，和 stack.rs 的装饰线计算一致。
            underline_thickness: underline.map(|m| m.thickness as f32 * s).unwrap_or(0.0),
            underline_position: underline.map(|m| -(m.position as f32) * s).unwrap_or(0.0),
            strikethrough_thickness: strikeout.map(|m| m.thickness as f32 * s).unwrap_or(0.0),
            strikethrough_position: strikeout.map(|m| -(m.position as f32) * s).unwrap_or(0.0),
        })
    }

    fn rasterize(&mut self, glyph_id: u32, px_size: u32) -> Option<AtlasRaster> {
        // 位图 emoji（CBDT/CBLC / sbix / EBDT）优先：有内嵌位图时直接用它，
        // 而不是栅格化轮廓 —— 那会丢失颜色。
        if let Some(bitmap) = self.raster_bitmap(glyph_id, px_size) {
            return Some(bitmap);
        }
        // COLR v0/v1：整张 paint graph 精确合成为 RGBA。
        if let Some(graph) = self.raster_color_graph(glyph_id, px_size) {
            return Some(graph);
        }
        // `px_size` 是排版端按显示字号选好的 SDF 基准档位（64/128/256）。
        // 大字号用高档位，避免单通道 SDF 过度放大导致笔画变钝、圆角被抹平。
        let px = px_size.max(SDF_BASE_PX) as f32;
        let id = GlyphId(glyph_id.min(u16::MAX as u32) as u16);
        // em → 行高换算必须走同一个入口：`outline_glyph` 用的是 **Glyph 自带**的
        // scale（见 `ScaleFont::outline_glyph` 的注释「does not make use of the
        // associated scale」），所以这里给 `with_scale` 的也必须是换算后的值。
        let scale = self.px_scale(px);
        let scaled = self.font.as_scaled(scale);

        // 空白字形（空格、零宽字符）没有轮廓 —— 返回 `None`，core 会给它一个零尺寸
        // 图集条目并推进光标（步进仍由 `advance` 给出）。
        let outlined = scaled.outline_glyph(id.with_scale(scale))?;

        let bounds = outlined.px_bounds();
        // ⚠ 尺寸必须用**截断**，不是 `ceil`。
        //
        // `ab_glyph` 的 `OutlinedGlyph::draw` 内部是这样开栅格器的（0.2.32，`outlined.rs`）：
        //
        // ```text
        // let (w, h) = (self.px_bounds.width() as usize,   // `as usize` = 截断
        //               self.px_bounds.height() as usize);
        // let offset = self.glyph.position - self.px_bounds.min;
        // Rasterizer::new(w, h)
        // ```
        //
        // 也就是说回调收到的坐标**已经在** `[0, w) × [0, h)` 里，`w`/`h` 是截断值。
        // 若这里按 `ceil` 分配缓冲区并按 `ceil` 报尺寸，多出来的那一行/一列
        // **永远是 0** —— 而图集会把它们当作这个字形的一部分写进去，于是屏幕上
        // 每个字形边缘都出现一条上一个字形的残留。
        let (w, h) = (bounds.width() as u32, bounds.height() as u32);
        if w == 0 || h == 0 {
            return None;
        }

        // ⚠ 回调坐标**已经是**相对 `px_bounds.min` 的，不要再减一次。
        //
        // 这个函数原先写的是 `let ax = x as i32 - min.0 as i32;` —— 减了第二次。
        // 后果是缓冲区左边与上边各被砍掉 `floor(min)` 个像素；32px 的 `'H'` 那类
        // `bounds.min.y ≈ 22` 的字形会**整块丢失**（`ay` 恒为负，全部被下面的越界
        // 判断 `continue` 掉）。字形进图集时再被减第三次（见 `crate::text::atlas`），
        // 于是图集里什么都没有 —— 屏幕上"字完全不出现"。
        let mut coverage = vec![0u8; (w * h) as usize];
        outlined.draw(|x, y, alpha| {
            let (ax, ay) = (x as usize, y as usize);
            if ax >= w as usize || ay >= h as usize {
                return;
            }
            let v = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
            coverage[ay * w as usize + ax] = v;
        });

        // 轮廓线段（供 MSDF 用）：ttf_parser 的字体单位 → 像素用 px/upem，
        // 这正是 ab_glyph 内部的水平缩放因子；origin 用它的 px_bounds.min 对齐。
        let px_per_unit = px / self.units_per_em();
        let segments = outline_segments(
            self.font.as_slice(),
            id.0,
            px_per_unit,
            (bounds.min.x, bounds.min.y),
            &self.applied_variations,
        );
        let mask = crate::text::msdf::generate(&coverage, w, h, SDF_PADDING, &segments);

        Some(AtlasRaster {
            // offset 是**屏幕方向**（y 向下）相对基线原点的偏移，基线之上为**负**。
            //
            // ab_glyph 的 `px_bounds` 就是这个约定：`min.y * -scale_factor.vertical`
            // 把字体坐标（y-up）翻成屏幕坐标（y-down），于是 'h' 的 min.y ≈ −26、
            // 'p' 的 min.y ≈ −19 而 `min.y + height > 0`（descender 伸到基线下面）。
            // core 拿它直接加在基线上（`GlyphQuad::dst.y = baseline + offset.y`）。
            //
            // ⚠ 这里曾经写成整数 `min.floor()` 并注释"y 向上为正"—— 约定说反了，
            // 于是 core 的公式也跟着反，每个字形的顶部贴到基线上（同一行基线错开、
            // descender 消失）。整数部分保留：位图从 `floor(min)` 开始，dst 必须与
            // 位图严格对齐，否则小字号下整体偏一个像素。
            offset: Point::new(bounds.min.x.floor(), bounds.min.y.floor()),
            width: w,
            height: h,
            // 三通道 MSDF（RGB）+ 真实 SDF 灰度副本（alpha）。
            // 替身来源仍可只填灰度 SDF：三通道相等时 median 退化为单通道。
            sdf: mask,
            spread: SDF_SPREAD,
            base_px: px,
            is_bitmap: false,
        })
    }

    fn shape(&mut self, text: &str, style: &TextStyle) -> Vec<ShapedRun> {
        if text.is_empty() {
            return Vec::new();
        }
        let px_size = style.effective_size().max(1.0);
        // rustybuzz 的位置是**字体单位**（upem），不是 hhea 行高单位。
        // ab_glyph 的 `h_advance` 内部用的是 `px / upem`（见 scale.rs 的
        // `h_scale_factor = scale.x / height_unscaled`，而 scale.x 已乘过
        // `height_unscaled / upem`）。这里必须用同一个 px/upem，否则步进会
        // 被放大 1.3 倍左右，长文本直接跑出屏幕。
        let upem = self.font.units_per_em().unwrap_or(1000.0).max(1.0);
        let scale = px_size / upem;
        let mut out = Vec::new();
        for (range, rtl) in bidi_runs(text, style.direction) {
            let run_text = &text[range.clone()];
            let mut shaped = self.shape_run(run_text, rtl, px_size, scale, &style.font_features);
            // `shape_run` 的 cluster 是 run 内字节偏移；下游按整段文本解释它们
            // （字间距去重、装饰线跨度都以整行字节为基准）。
            for g in &mut shaped.glyphs {
                g.cluster.start += range.start;
                g.cluster.end += range.start;
            }
            if !shaped.glyphs.is_empty() {
                out.push(shaped);
            }
        }
        out
    }

    fn font_id(&self) -> u32 {
        self.font_id
    }
}

#[cfg(test)]
#[path = "../../tests/unit/render/text_tests.rs"]
mod tests;

#[cfg(test)]
mod bitmap_offset_tests {
    use super::*;

    #[test]
    fn cbdt_y_up_becomes_atlas_y_down() {
        let p = bitmap_offset(3, 12);
        assert_eq!(p.x, 3.0);
        assert_eq!(p.y, -12.0, "CBDT 上边界在基线之上 ⇒ 图集偏移应为负");
    }
}
