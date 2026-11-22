//! `range_has_lcd_text` 的门禁：三个 GPU 后端（D3D11/D3D12/Vulkan）共用它
//! 决定要不要为这一趟取目标副本。误判为 false ⇒ LCD alpha 退化成 max（彩边）。

use crate::core::{Color, Point, Rect, UvRect};
use crate::filter::{BlendMode, FilterChain};
use crate::paint::{Command, DisplayList, GlyphMask, GlyphQuad, Layer, LcdOrder, SortKey, TextureRef};

fn text_command(mask: GlyphMask) -> Command {
    Command::Text {
        atlas: TextureRef::new("glyph-atlas"),
        glyphs: vec![GlyphQuad {
            uv: UvRect::new(0.0, 0.0, 0.1, 0.1),
            dst: Rect::new(0.0, 0.0, 10.0, 10.0),
            color_glyph: false,
            mask,
            rotation: 0.0,
            color: None,
            transform: None,
            clip: None,
            kind: crate::render::quad::kind::GLYPH,
        }]
        .into(),
        offset: Point::new(0.0, 0.0),
        color: Color::rgb(1.0, 1.0, 1.0),
        filter: FilterChain::none(),
        blend: BlendMode::default(),
    }
}

fn key(order: u32) -> SortKey {
    SortKey {
        layer: Layer::Content,
        z: 0,
        order,
    }
}

fn lcd() -> GlyphMask {
    GlyphMask::LcdMsdf {
        w: 0.5,
        embolden: 0.0,
        order: LcdOrder::Rgb,
    }
}

/// 三条文本命令，LCD 只在中间：[coverage, LCD, msdf]。
fn three_rows() -> DisplayList {
    let mut list = DisplayList::new();
    list.push(key(0), text_command(GlyphMask::Coverage));
    list.push(key(1), text_command(lcd()));
    list.push(
        key(2),
        text_command(GlyphMask::Msdf {
            w: 0.5,
            embolden: 0.0,
        }),
    );
    list
}

#[test]
fn detects_lcd_in_the_whole_range() {
    let list = three_rows();
    assert!(crate::paint::range_has_lcd_text(&list, &(0..3)));
}

#[test]
fn respects_range_boundaries() {
    let list = three_rows();
    assert!(!crate::paint::range_has_lcd_text(&list, &(0..1)));
    assert!(crate::paint::range_has_lcd_text(&list, &(1..2)));
    assert!(!crate::paint::range_has_lcd_text(&list, &(2..3)));
}

#[test]
fn non_lcd_text_does_not_trigger() {
    let mut list = DisplayList::new();
    list.push(key(0), text_command(GlyphMask::Coverage));
    list.push(
        key(1),
        text_command(GlyphMask::Msdf {
            w: 0.5,
            embolden: 0.0,
        }),
    );
    assert!(!crate::paint::range_has_lcd_text(&list, &(0..2)));
}

#[test]
fn empty_and_out_of_range_are_false() {
    let list = DisplayList::new();
    assert!(!crate::paint::range_has_lcd_text(&list, &(0..0)));
    assert!(!crate::paint::range_has_lcd_text(&list, &(3..5)));
}
