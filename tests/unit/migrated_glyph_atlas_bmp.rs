//! 从主仓迁移来的图集可视化测试：把字形图集导成 BMP，人眼确认栅格化是否正确。
//!
//! 默认 `#[ignore]`：要读系统字体并写临时文件。
//! 手动跑：`cargo test -p zephyra-vector migrated_glyph_atlas_bmp -- --ignored --nocapture`

use crate::core::Point;
use crate::render::pixels::{ChannelOrder, PixelRead};
use crate::text::atlas::ATLAS_SIZE;
use crate::text::{from_system_font, TextEngine, TextRender, TextStyle};

const SAMPLE: &str = "ZephyraUI 渲染 DisplayList";
const SIZE: f32 = 32.0;
const REGION: u32 = 360;
const SCALE: u32 = 3;

#[test]
#[ignore = "需要系统字体、且会写文件；手动跑：--ignored --nocapture"]
fn glyph_atlas_contains_correct_glyphs() {
    let (mut stack, note) = from_system_font();
    println!("{note}");

    let style = TextStyle {
        size: SIZE,
        ..Default::default()
    };
    let shaped = TextEngine::shape(&mut stack, SAMPLE, &style, None);
    assert!(
        !shaped.lines.is_empty() && !shaped.lines[0].glyphs.is_empty(),
        "排版 `{SAMPLE}` 后一个字形都没有 —— 这台机器上没有可用字体"
    );

    let (_handle, quads) = stack
        .glyph_quads(&shaped, Point::new(0.0, 40.0), &style)
        .expect("有字体时 glyph_quads 不该返回 None");
    assert!(!quads.is_empty(), "没有产出任何字形四边形");

    let stats = stack.atlas_stats();
    println!(
        "字形 {} 个，四边形 {} 个，用量 {:.1}%",
        stats.glyphs,
        quads.len(),
        stats.usage() * 100.0
    );

    // SDF 图集四通道都写了距离值；取 red 判断是否有内容（背景是 0）。
    let inked = stack
        .atlas()
        .pixels()
        .chunks_exact(4)
        .filter(|p| p[0] > 0)
        .count();
    assert!(inked > 0, "图集里一个非零像素都没有 —— 栅格化或打包断了");
    println!("图集中有内容的像素：{inked}");

    for (i, q) in quads.iter().enumerate() {
        assert!(
            q.uv.x >= 0.0 && q.uv.y >= 0.0 && q.uv.right() <= 1.0 && q.uv.bottom() <= 1.0,
            "第 {i} 个字形的 uv ({}, {})-({}, {}) 越界",
            q.uv.x,
            q.uv.y,
            q.uv.right(),
            q.uv.bottom()
        );
    }

    let region = REGION.min(ATLAS_SIZE);
    let out_w = region * SCALE;
    let out_h = region * SCALE;
    let pixels = stack.atlas().pixels();
    let mut out = vec![0u8; (out_w * out_h * 4) as usize];
    for y in 0..out_h {
        for x in 0..out_w {
            let sx = x / SCALE;
            let sy = y / SCALE;
            let si = ((sy * ATLAS_SIZE + sx) * 4) as usize;
            let di = ((y * out_w + x) * 4) as usize;
            let v = pixels[si];
            out[di] = if v > 0 { v } else { 40 };
            out[di + 1] = v;
            out[di + 2] = v;
            out[di + 3] = 255;
        }
    }

    let path = std::env::temp_dir()
        .join("zephyra-atlas-preview.bmp")
        .display()
        .to_string();
    crate::write_bmp(
        &path,
        &PixelRead {
            order: ChannelOrder::Rgba,
            width: out_w,
            height: out_h,
            row_pitch: out_w * 4,
            declared_width: out_w,
            declared_height: out_h,
            data: out,
        },
        false,
    )
    .expect("写 BMP 失败");
    println!("已写出 {path}（{out_w}×{out_h}，放大 {SCALE}×）");
}
