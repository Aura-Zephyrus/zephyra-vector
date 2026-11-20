//! A1：字体度量 / StrutStyle 的回归测试。
//!
//! 对应 Skia 的 `SkFontMetrics` 与 `SkStrutStyle`：真实字体要给出真实升部/降部，
//! strut 能把它覆盖成统一行框。

use crate::text::ab_glyph_source::AbGlyphSource;
use crate::text::stack::resolved_line_height;
use crate::text::{FontMetrics, GlyphSource, StrutStyle, TextStyle};

fn source() -> Option<AbGlyphSource> {
    AbGlyphSource::from_bytes(0, &std::fs::read(r"C:\Windows\Fonts\segoeui.ttf").ok()?).ok()
}

#[test]
fn real_font_reports_hhea_and_underline_metrics() {
    let Some(src) = source() else {
        eprintln!("系统字体不可用，跳过");
        return;
    };
    let m = src.metrics(20.0).expect("真实字体应给出度量");
    assert!(m.ascent > 0.0, "升部应为正: {m:?}");
    assert!(m.descent < 0.0, "降部应为负: {m:?}");
    assert!(m.ascent > m.x_height && m.x_height > 0.0, "升部应高于 x 高度: {m:?}");
    assert!(m.cap_height > m.x_height, "大写高度应高于 x 高度: {m:?}");
    assert!(m.underline_thickness > 0.0, "应有下划线粗细: {m:?}");
    assert!(m.underline_position > 0.0, "下划线应在基线下方: {m:?}");
    assert!(m.line_height() > 20.0, "20px 字体的自然行高应大于字号: {m:?}");
}

#[test]
fn strut_overrides_font_metrics_and_explicit_height_wins() {
    let metrics = FontMetrics {
        ascent: 16.0,
        descent: -4.0,
        leading: 2.0,
        ..Default::default()
    };
    // 无 strut：用真实度量。
    assert_eq!(resolved_line_height(&TextStyle::default(), 20.0, Some(metrics)), 22.0);

    // 显式倍数最高优先级。
    let mut style = TextStyle {
        line_height: Some(1.5),
        ..Default::default()
    };
    style.strut = Some(StrutStyle {
        ascent: Some(99.0),
        force: true,
        ..Default::default()
    });
    assert_eq!(resolved_line_height(&style, 20.0, Some(metrics)), 30.0);

    // strut 覆盖：force=true 时未指定字段取半个经验行高，不回落字体度量。
    let style = TextStyle {
        strut: Some(StrutStyle {
            ascent: Some(12.0),
            descent: Some(3.0),
            leading: Some(1.0),
            force: true,
        }),
        ..Default::default()
    };
    assert_eq!(resolved_line_height(&style, 20.0, Some(metrics)), 16.0);

    // 非 force：只覆盖指定字段，其余取字体度量。
    let style = TextStyle {
        strut: Some(StrutStyle {
            leading: Some(5.0),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(resolved_line_height(&style, 20.0, Some(metrics)), 25.0);
}

#[test]
fn fallback_when_metrics_absent() {
    let style = TextStyle::default();
    assert_eq!(resolved_line_height(&style, 20.0, None), 24.0); // 1.2 × 20
}
