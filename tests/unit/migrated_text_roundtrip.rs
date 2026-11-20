//! 从主仓迁移来的文字闭环测试：**栅格化 → 打包 → uv → 顶点 → 采样**。
//!
//! GPU 侧的采样无法在此仓验证，但「按 uv 去图集取值」这一步完全可以在 CPU 上
//! 重演：若采样有墨，则"文字空白"不可能来自本仓负责的五步。

use crate::core::{Color, Point};
use crate::filter::{BlendMode, FilterChain};
use crate::paint::{Command, DisplayList, Layer, SortKey};
use crate::render::quad::{self, Vertex};
use crate::text::atlas::ATLAS_SIZE;
use crate::text::{from_system_font, TextEngine, TextRender, TextStack, TextStyle};

/// 目标尺寸（DIP）。
const TARGET: (u32, u32) = (400, 200);

/// 字号（DIP）。
const SIZE: f32 = 32.0;

/// 建一个真实的字体栈；没有系统字体就跳过（而不是假装通过）。
fn stack() -> Option<TextStack<crate::text::AbGlyphSource>> {
    let (mut s, _desc) = from_system_font();
    let shaped = TextEngine::shape(
        &mut s,
        "Hi",
        &TextStyle {
            size: SIZE,
            ..Default::default()
        },
        None,
    );
    if shaped.lines.iter().all(|l| l.glyphs.is_empty()) {
        eprintln!("跳过：这台机器上没有可用的系统字体");
        return None;
    }
    Some(s)
}

/// 按 uv 在图集里取 SDF 值（重演片元着色器的 `g_tex.Sample(...).r`）。
fn sample_atlas(atlas: &[u8], u: f32, v: f32) -> u8 {
    let x = ((u * ATLAS_SIZE as f32) as usize).min(ATLAS_SIZE as usize - 1);
    let y = ((v * ATLAS_SIZE as f32) as usize).min(ATLAS_SIZE as usize - 1);
    atlas[(y * ATLAS_SIZE as usize + x) * 4]
}

/// 把 `DisplayList` 展开成顶点流。
fn expand(list: &DisplayList) -> quad::QuadBatch {
    quad::expand(list, TARGET, |_| true)
}

#[test]
fn glyphs_line_up_from_rasterization_through_sampling() {
    let Some(mut stack) = stack() else { return };

    let style = TextStyle {
        size: SIZE,
        ..Default::default()
    };
    let shaped = TextEngine::shape(&mut stack, "Hg", &style, None);

    assert!(
        !shaped.lines.is_empty() && !shaped.lines[0].glyphs.is_empty(),
        "排版 `\"Hg\"` 后一个字形都没有 —— 字形来源不可用"
    );

    let (_handle, quads) = stack
        .glyph_quads(&shaped, Point::new(10.0, 40.0), &style)
        .expect("有字体时 glyph_quads 不该返回 None");
    assert_eq!(
        quads.len(),
        shaped.lines[0].glyphs.len(),
        "\"Hg\" 是两个非空白字形，图集里应当各有一个四边形"
    );

    // SDF 按固定基准档位栅格化，绘制时按 `显示字号 / 基准字号` 缩放。
    let display = style.effective_size();
    let tier = crate::text::sdf_tier(display);
    let scale = display / tier as f32;
    let atlas_pixels = stack.atlas().pixels();
    for (i, q) in quads.iter().enumerate() {
        assert!(
            q.uv.x >= 0.0 && q.uv.y >= 0.0,
            "第 {i} 个字形的 uv 原点 ({}, {}) 为负 —— 归一化时符号搞反了",
            q.uv.x,
            q.uv.y
        );
        assert!(
            q.uv.right() <= 1.0 && q.uv.bottom() <= 1.0,
            "第 {i} 个字形的 uv 右下 ({}, {}) 越过了 1.0",
            q.uv.right(),
            q.uv.bottom()
        );
        assert!(
            q.uv.width > 0.0 && q.uv.height > 0.0,
            "第 {i} 个字形的 uv 面积为零"
        );

        let px_w = q.uv.width * ATLAS_SIZE as f32 * scale;
        let px_h = q.uv.height * ATLAS_SIZE as f32 * scale;
        assert!(
            (px_w - q.dst.width).abs() < 0.5,
            "第 {i} 个字形：uv 反推宽度 {px_w}（scale={scale:.3}）与 dst 宽度 {} 不符",
            q.dst.width
        );
        assert!(
            (px_h - q.dst.height).abs() < 0.5,
            "第 {i} 个字形：uv 反推高度 {px_h} 与 dst 高度 {} 不符",
            q.dst.height
        );
    }

    let mut inked = 0;
    for (i, q) in quads.iter().enumerate() {
        let (ux0, uy0) = (q.uv.x, q.uv.y);
        let (ux1, uy1) = (q.uv.right(), q.uv.bottom());
        let mut max_cov = 0u8;
        for sy in 1..4 {
            for sx in 1..4 {
                let u = ux0 + (ux1 - ux0) * (sx as f32 + 0.5) / 4.0;
                let v = uy0 + (uy1 - uy0) * (sy as f32 + 0.5) / 4.0;
                max_cov = max_cov.max(sample_atlas(atlas_pixels, u, v));
            }
        }
        if max_cov > 0 {
            inked += 1;
        } else {
            eprintln!("[t] 第 {i} 个字形采样全为 0：uv=({ux0:.4},{uy0:.4})-({ux1:.4},{uy1:.4})");
        }
    }
    assert_eq!(
        inked,
        quads.len(),
        "{inked}/{} 个字形按 uv 采样全透明 —— 栅格化/打包/uv 这条链断了",
        quads.len()
    );

    let mut list = DisplayList::new();
    list.push(
        SortKey {
            layer: Layer::Content,
            z: 0,
            order: 0,
        },
        Command::Text {
            atlas: _handle.clone(),
            glyphs: quads.clone().into(),
            offset: Point::new(0.0, 0.0),
            color: Color::rgba(1.0, 1.0, 1.0, 1.0),
            filter: FilterChain::none(),
            blend: BlendMode::default(),
        },
    );
    let batch = expand(&list);
    assert_eq!(batch.solid.len(), quads.len() * 4);

    for (i, chunk) in batch.solid.chunks_exact(4).enumerate() {
        let [a, b, _c, d] = chunk else { unreachable!() };
        assert!(
            (a.position[0] - d.position[0]).abs() < 1e-5
                && (a.position[1] - b.position[1]).abs() < 1e-5,
            "第 {i} 个字形的顶点不是轴对齐矩形"
        );
        assert!(
            (a.kind - quad::kind::GLYPH).abs() < f32::EPSILON,
            "第 {i} 个字形的 kind = {}，应为 GLYPH",
            a.kind
        );
        assert!(
            (a.uv[0] - quads[i].uv.x).abs() < 1e-6,
            "第 {i} 个字形的 uv.x 被 expand 改写了"
        );
        for v in chunk {
            assert!(
                (-1.0..=1.0).contains(&v.position[0]) && (-1.0..=1.0).contains(&v.position[1]),
                "第 {i} 个字形的顶点位置 {:?} 超出 NDC [-1,1]",
                v.position
            );
        }
    }
}

#[test]
fn space_produces_no_quads() {
    let Some(mut stack) = stack() else { return };
    let style = TextStyle {
        size: SIZE,
        ..Default::default()
    };
    let shaped = TextEngine::shape(&mut stack, "a a", &style, None);
    let (_h, quads) = stack
        .glyph_quads(&shaped, Point::new(0.0, 50.0), &style)
        .expect("有字体时不该是 None");
    assert_eq!(
        quads.len(),
        2,
        "\"a a\" 应当只有 2 个四边形，实际 {} 个 —— 空格被当成了可见字形",
        quads.len()
    );
}

#[test]
fn atlas_usage_is_sane_and_does_not_overflow() {
    let Some(mut stack) = stack() else { return };
    let style = TextStyle {
        size: SIZE,
        ..Default::default()
    };
    let shaped = TextEngine::shape(&mut stack, "Hello, Zephyra!", &style, None);
    let _ = stack.glyph_quads(&shaped, Point::new(0.0, 50.0), &style);

    let stats = stack.atlas_stats();
    assert!(stats.usage() <= 1.0, "图集用量 {} 超过 100%", stats.usage());
    assert!(stats.glyphs >= 8, "图集里只记了 {} 个条目", stats.glyphs);
    let before = stack.atlas_stats().glyphs;
    let _ = stack.glyph_quads(&shaped, Point::new(0.0, 50.0), &style);
    assert_eq!(
        stack.atlas_stats().glyphs,
        before,
        "第二次排版同一段文本新增了图集条目 —— 缓存没命中"
    );
}

#[test]
fn vertex_uv_matches_atlas_entry_verbatim() {
    let Some(mut stack) = stack() else { return };
    let style = TextStyle {
        size: SIZE,
        ..Default::default()
    };
    let shaped = TextEngine::shape(&mut stack, "M", &style, None);
    let (_h, quads) = stack
        .glyph_quads(&shaped, Point::new(0.0, 50.0), &style)
        .expect("有字体时不该是 None");
    let q = quads.first().expect("\"M\" 应当有一个四边形");

    let pixels = stack.atlas().pixels();
    let px_x = (q.uv.x * ATLAS_SIZE as f32).round() as usize;
    let px_y = (q.uv.y * ATLAS_SIZE as f32).round() as usize;
    let any_ink = (0..8).any(|dy| {
        (0..8).any(|dx| {
            let x = px_x + dx;
            let y = px_y + dy;
            x < ATLAS_SIZE as usize
                && y < ATLAS_SIZE as usize
                && pixels[(y * ATLAS_SIZE as usize + x) * 4] > 0
        })
    });
    assert!(
        any_ink,
        "按 uv 反推的图集位置 ({px_x}, {px_y}) 附近 8×8 内没有墨"
    );
}

#[test]
fn vertex_position_matches_dst_rect() {
    let Some(mut stack) = stack() else { return };
    let style = TextStyle {
        size: SIZE,
        ..Default::default()
    };
    let origin = Point::new(10.0, 60.0);
    let shaped = TextEngine::shape(&mut stack, "H", &style, None);
    let (_h, quads) = stack.glyph_quads(&shaped, origin, &style).unwrap();
    let q = quads[0];

    let nx = |x: f32| x / TARGET.0 as f32 * 2.0 - 1.0;
    let ny = |y: f32| 1.0 - y / TARGET.1 as f32 * 2.0;
    let expect = Vertex::from_uv_rect(
        q.dst,
        (TARGET.0 as f32, TARGET.1 as f32),
        [1.0; 4],
        q.uv,
        quad::kind::GLYPH,
    );
    assert!((expect[0].position[0] - nx(q.dst.x)).abs() < 1e-5);
    assert!((expect[0].position[1] - ny(q.dst.y)).abs() < 1e-5);
    assert!((expect[2].position[1] - ny(q.dst.y + q.dst.height)).abs() < 1e-5);
}
