//! `Rect` 的几何不变量。
//!
//! 每条断言针对的是**契约**而不是实现细节；注释写明"为什么值得钉住"，
//! 因为这里正是变异测试最容易漏掉的地方。

use crate::core::{Rect, Thickness};

#[test]
fn intersect_returns_none_when_disjoint() {
    let a = Rect::new(0.0, 0.0, 10.0, 10.0);
    let b = Rect::new(20.0, 20.0, 5.0, 5.0);
    assert_eq!(a.intersect(&b), None, "不相交必须返回 None 而非空矩形");
    // 空矩形会在后续布局里变成难以追踪的零尺寸。
    assert_eq!(b.intersect(&a), None);
}

#[test]
fn intersect_touching_edges_is_empty_not_none() {
    let a = Rect::new(0.0, 0.0, 10.0, 10.0);
    let b = Rect::new(10.0, 0.0, 10.0, 10.0);
    assert_eq!(a.intersect(&b), None);
}

#[test]
fn intersect_keeps_overlap() {
    let a = Rect::new(0.0, 0.0, 10.0, 10.0);
    let b = Rect::new(5.0, 5.0, 10.0, 10.0);
    assert_eq!(a.intersect(&b), Some(Rect::new(5.0, 5.0, 5.0, 5.0)));
}

#[test]
fn inset_never_produces_negative_size() {
    let r = Rect::new(0.0, 0.0, 4.0, 4.0);
    let inner = r.inset(Thickness::uniform(10.0));
    assert_eq!(inner.width, 0.0);
    assert_eq!(inner.height, 0.0);
}
