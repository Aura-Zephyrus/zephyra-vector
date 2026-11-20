//! COLR v0 / CPAL 彩色字形（emoji）解析。
//!
//! # 为什么不引 skrifa / ttf-parser 的高层 API
//!
//! ttf-parser 0.25 的 `colr` 表 API 只暴露 v1 的 paint graph，`LayerRecord` 是私有的；
//! 而 COLR v0（绝大多数 emoji 字体，包括 Segoe UI Emoji 与 Noto Color Emoji）
//! 的 baseglyph/layer 记录只有 6/4 字节，直接读表更短、更可控，也不引入新依赖。
//!
//! # 绘制策略：逐层 quad
//!
//! 每个 layer 是字体里的一个普通 glyph id（有自己的轮廓），配一个 CPAL 调色板颜色。
//! 于是每层就是一片 `GlyphQuad`，颜色走 A4 加的 `GlyphQuad::color` 覆盖，
//! 按层序由正常 alpha 合成叠出最终彩色字形 —— 与 Skia 渲染 COLR v0 的方式一致，
//! 且**不需要新的 shader 模式或 RGBA 图集**：层 glyph 用已有的 MSDF 路径。

use std::collections::HashMap;

use rustybuzz::ttf_parser::Tag;

use crate::core::Color;

/// COLR 的一层：一个字形 id + 它在当前调色板里的颜色。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorLayer {
    /// 该层的字形 id（在字体里是普通 glyph id，可独立栅格化）。
    pub glyph_id: u32,
    /// 该层颜色（来自 CPAL palette 0）。
    pub color: Color,
}

fn u16at(data: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_be_bytes(data.get(off..off + 2)?.try_into().ok()?))
}

fn u32at(data: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_be_bytes(data.get(off..off + 4)?.try_into().ok()?))
}

/// 解析字体里的 COLR v0 彩色字形表。无相关表、版本不支持或表损坏时返回空表。
pub fn parse_color_layers(font_bytes: &[u8]) -> HashMap<u32, Vec<ColorLayer>> {
    let mut out = HashMap::new();
    let Ok(face) = rustybuzz::ttf_parser::Face::parse(font_bytes, 0) else {
        return out;
    };
    let Some(colr) = face.raw_face().table(Tag::from_bytes(b"COLR")) else {
        return out;
    };
    let Some(cpal) = face.raw_face().table(Tag::from_bytes(b"CPAL")) else {
        return out;
    };
    // 只做 v0：v1 的 paint graph（渐变/合成）是独立的一大块，本轮不支持。
    if u16at(colr, 0) != Some(0) {
        return out;
    }
    let Some(num_base) = u16at(colr, 2) else {
        return out;
    };
    let Some(base_off) = u32at(colr, 4) else {
        return out;
    };
    let Some(layer_off) = u32at(colr, 8) else {
        return out;
    };

    let Some(color_records_off) = u32at(cpal, 8) else {
        return out;
    };
    let color = |idx: u16| -> Option<Color> {
        let off = color_records_off as usize + idx as usize * 4;
        let b = *cpal.get(off)? as f32 / 255.0;
        let g = *cpal.get(off + 1)? as f32 / 255.0;
        let r = *cpal.get(off + 2)? as f32 / 255.0;
        let a = *cpal.get(off + 3)? as f32 / 255.0;
        Some(Color::rgba(r, g, b, a))
    };

    for i in 0..num_base as usize {
        let o = base_off as usize + i * 6;
        let Some(gid) = u16at(colr, o) else { break };
        let Some(first) = u16at(colr, o + 2) else { break };
        let Some(count) = u16at(colr, o + 4) else { break };
        let mut layers = Vec::with_capacity(count as usize);
        for j in 0..count as usize {
            let lo = layer_off as usize + (first as usize + j) * 4;
            let Some(lgid) = u16at(colr, lo) else { break };
            let Some(palette_index) = u16at(colr, lo + 2) else {
                break;
            };
            layers.push(ColorLayer {
                glyph_id: lgid as u32,
                // 无对应调色板项时用不透明白色，至少让字形可见，而不是整层消失。
                color: color(palette_index).unwrap_or(Color::rgb(1.0, 1.0, 1.0)),
            });
        }
        if !layers.is_empty() {
            out.insert(gid as u32, layers);
        }
    }
    out
}
