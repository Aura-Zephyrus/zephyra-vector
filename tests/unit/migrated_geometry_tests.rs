//! 从主仓 `ZephyraUI/tests/unit/catalog_tests.rs` 迁移来的几何测试。
//! 原测试挂在控件状态测试文件里，但它验的是本仓的三角化能力。

use crate::core::Point;
use crate::geom::{triangulate, Contour, FillRule};

#[test]
fn dynamic_arc_annulus_triangulates_to_vertices() {
    // 与 gallery 的 catalog_busy_path 同一套几何：中线半径 25、线宽 6、圆端。
    let start = 45.0_f32.to_radians();
    let sweep = 120.0_f32.to_radians();
    let steps = 20usize;
    let (ro, ri, cap) = (28.0f32, 22.0f32, 3.0f32);
    let mut ring: Vec<Point> = Vec::new();
    for i in 0..=steps {
        let a = start + sweep * (i as f32 / steps as f32);
        ring.push(Point::new(28.0 + ro * a.cos(), 28.0 + ro * a.sin()));
    }
    let end = start + sweep;
    let ec = Point::new(28.0 + 25.0 * end.cos(), 28.0 + 25.0 * end.sin());
    for i in 1..8 {
        let t = end + std::f32::consts::PI * i as f32 / 8.0;
        ring.push(Point::new(ec.x + cap * t.cos(), ec.y + cap * t.sin()));
    }
    for i in (0..=steps).rev() {
        let a = start + sweep * (i as f32 / steps as f32);
        ring.push(Point::new(28.0 + ri * a.cos(), 28.0 + ri * a.sin()));
    }
    let sc = Point::new(28.0 + 25.0 * start.cos(), 28.0 + 25.0 * start.sin());
    for i in 1..8 {
        let t = start + std::f32::consts::PI + std::f32::consts::PI * i as f32 / 8.0;
        ring.push(Point::new(sc.x + cap * t.cos(), sc.y + cap * t.sin()));
    }
    let mesh = triangulate(&[Contour::new(ring)], FillRule::NonZero).expect("弧线应能三角化");
    assert!(
        mesh.vertices.len() > 6,
        "顶点数 {} 太少",
        mesh.vertices.len()
    );
}
