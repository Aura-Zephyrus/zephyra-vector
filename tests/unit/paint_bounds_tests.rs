//! `bounds_of_vertices` 的直接单测（从 ZephyraCore 移植到 vector）。
//!
//! 框架侧那条端到端测试走真实图标的三角化网格，而那个网格的**首顶点恰好是
//! 极值角点** —— 于是「忘了更新 max 边」的变异算出的结果与正确答案**恰好
//! 相同**，测试一直显示绿色却什么也没验证。这里把那条直接单测搬过来，保留
//! 刻意选的顶点顺序，让 min 与 max 必须由不同顶点得出。

use crate::core::{Point, Rect};
use crate::paint::{bounds_of_vertices, Vertex};

/// `bounds_of_vertices` 的直接单测。
///
/// ⚠ 顶点顺序是刻意选的：**首顶点在内部**，后两个才是包围盒的对角。
///
/// 为什么端到端那条（`blurred_vector_icon_yields_offscreen_requirement`）替代不了这条：它走真实图标
/// 的三角化网格，而那个网格的**首顶点恰好是极值角点** —— 于是「忘了更新 max 边」
/// 这个变异算出来的结果与正确答案**恰好相同**，测试一直显示绿色却什么也没验证。
/// 这个洞是**变异测试抓出来的**，不是看代码想出来的。
#[test]
fn vertex_bounds_when_first_vertex_is_interior() {
    let v = |x: f32, y: f32| Vertex {
        position: Point::new(x, y),
        uv: Point::new(0.0, 0.0),
    };
    assert_eq!(
        bounds_of_vertices(&[v(20.0, 30.0), v(10.0, 20.0), v(33.0, 47.0)]),
        Rect::new(10.0, 20.0, 23.0, 27.0),
        "min 与 max 必须分别由不同顶点得出；首顶点在内部时这条才有意义"
    );
    // x 与 y 刻意不是整数对齐：x/w 与 y/h 必须**分别**来自 min 与 max，混了会露馅
    assert_eq!(
        bounds_of_vertices(&[v(20.0, 30.0), v(10.0, 20.0), v(33.0, 47.0)]).width,
        23.0,
        "宽 = max.x − min.x，不能取反"
    );
    assert_eq!(
        bounds_of_vertices(&[v(7.0, 9.0)]),
        Rect::new(7.0, 9.0, 0.0, 0.0),
        "单顶点 = 零尺寸矩形，原点在该顶点"
    );
    assert_eq!(
        bounds_of_vertices(&[]),
        Rect::new(0.0, 0.0, 0.0, 0.0),
        "空列表 = 零尺寸（与 union_of_quads 同一约定，不用 Option）"
    );
}
