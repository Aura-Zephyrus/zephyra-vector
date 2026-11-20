// `expand` / `RenderReport` 记账的压力测试。
//
// # 守的是什么
//
// 两个计数器（`unimplemented_filters` / `unimplemented_filters` 的搭档
// `unimplemented_blends`）是「效果静默消失」的唯一诊断手段，而它们按**命令**
// 计数、后端按**段**执行 —— 这个口径差正是 2026-10-24 冻结链 bug 的根因族
// （redraw 分支少减一次账，投影/折射每帧恒报 1）。单点用例钉不住这类问题，
// 因为它在「一帧一条命令」上完全正常，只在组合里现形。
//
// 所以这里用确定性伪随机列表钉两条**守恒律**：
//
// 1. 逐命令计数：整表 expand 的两个计数必须等于手工逐命令数的结果；
// 2. 分段回放守恒：把所有段区间分别 expand 再 absorb，计数字段必须与
//    整表 expand **逐一相等** —— 区间漏一段/叠一段都会当场红。

use super::*;
use crate::core::{Color, CornerRadius, Point, Rect};
use crate::filter::{BlendMode, Filter, FilterChain};
use crate::offscreen::{plan_segments, Segment};
use crate::paint::{command_filter, Layer, SortKey};

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E3779B97F4A7C15) | 1)
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn below(&mut self, n: u32) -> u32 {
        (self.next_u64() % n as u64) as u32
    }
    fn f32_in(&mut self, lo: f32, hi: f32) -> f32 {
        let t = (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32;
        lo + t * (hi - lo)
    }
}

/// 与 `offscreen::stress_tests` 同款随机链（本文件自足，不跨模块借私有助手）。
fn random_chain(rng: &mut Rng) -> FilterChain {
    let n = rng.below(5);
    let mut v = Vec::new();
    for _ in 0..n {
        v.push(match rng.below(5) {
            0 => Filter::Blur {
                sigma_x: rng.f32_in(0.0, 30.0),
                sigma_y: rng.f32_in(0.0, 30.0),
            },
            1 => Filter::Blur {
                sigma_x: 0.0,
                sigma_y: 0.0,
            },
            2 => Filter::DropShadow {
                color: Color::rgba(0.0, 0.0, 0.0, 0.5),
                offset: Point::new(rng.f32_in(-20.0, 20.0), rng.f32_in(-20.0, 20.0)),
                sigma: rng.f32_in(0.0, 12.0),
            },
            3 => Filter::Refraction {
                strength: rng.f32_in(1.0, 40.0),
                bezel: rng.f32_in(1.0, 30.0),
                dispersion: rng.f32_in(0.0, 1.0),
                ior: rng.f32_in(1.0, 2.2),
            },
            _ => Filter::Blur {
                sigma_x: -1.0, // 负 σ：恒等，不进计数
                sigma_y: rng.f32_in(0.0, 5.0),
            },
        });
    }
    FilterChain::new(v)
}

fn random_blend(rng: &mut Rng) -> BlendMode {
    match rng.below(6) {
        0 => BlendMode::SrcOver,
        1 => BlendMode::Multiply,
        2 => BlendMode::Screen,
        3 => BlendMode::SoftLight,
        4 => BlendMode::Hue,
        _ => BlendMode::Plus,
    }
}

fn random_list(rng: &mut Rng) -> DisplayList {
    let mut l = DisplayList::new();
    let n = rng.below(40);
    for i in 0..n {
        l.push(
            SortKey {
                layer: Layer::Content,
                z: 0,
                order: i,
            },
            Command::Quad {
                rect: Rect::new(
                    rng.f32_in(-200.0, 900.0),
                    rng.f32_in(-200.0, 700.0),
                    rng.f32_in(0.5, 300.0),
                    rng.f32_in(0.5, 300.0),
                ),
                radius: CornerRadius {
                    radius: rng.f32_in(0.0, 16.0),
                },
                shape: crate::core::CornerShape::ROUND,
                fill: Color::rgba(rng.f32_in(0.0, 1.0), 0.5, 0.5, rng.f32_in(0.0, 1.0)),
                stroke: None,
                filter: random_chain(rng),
                blend: random_blend(rng),
                decorations: crate::decoration::Decorations::none(),
            },
        );
    }
    l.sort();
    l
}

const SIZE: (u32, u32) = (1024, 768);
const REG: fn(&str) -> bool = |_| true;

// ── 守恒律 1：逐命令计数 ──

/// 整表 expand 的两个「未实现」计数必须等于**逐命令手数**的结果。
///
/// # 变异目标
///
/// 某个命令变体漏数（Path/Text 不走同一行）、把计数挪到几何裁掉之后
/// （透明命令不计数 → 诊断数字偏小）、或把 blend 计数成「非默认」而不是
/// 「系数不可表达」（15 个可分离模式虚报）时本条失败。
#[test]
fn stress_expand_counts_every_filtered_command_exactly_once() {
    for seed in 0..400u64 {
        let mut rng = Rng::new(seed);
        let list = random_list(&mut rng);
        let batch = expand(&list, SIZE, REG);

        let mut exp_filters = 0usize;
        let mut exp_blends = 0usize;
        for item in list.iter() {
            if let Some(f) = command_filter(&item.command) {
                if !f.is_identity() {
                    exp_filters += 1;
                }
            }
            let b = match &item.command {
                Command::Quad { blend, .. }
                | Command::Path { blend, .. }
                | Command::Image { blend, .. }
                | Command::Text { blend, .. }
                | Command::Custom { blend, .. } => *blend,
                Command::PushClip(_) | Command::PopClip => BlendMode::default(),
            };
            if blend_is_unimplemented(b) {
                exp_blends += 1;
            }
        }
        assert_eq!(
            batch.report.unimplemented_filters, exp_filters,
            "seed {seed}: unimplemented_filters 必须逐命令计"
        );
        assert_eq!(
            batch.report.unimplemented_blends, exp_blends,
            "seed {seed}: unimplemented_blends 必须逐命令计"
        );
        // 透明填充的 Quad 也计数（计数在几何 early-return 之前）—— 上面手数
        // 已经覆盖了这种命令，这里再钉一次「不得因透明而少计」。
    }
}

// ── 守恒律 2：分段回放不丢不加 ──

/// 按 `plan_segments` 的区间逐段 expand 再 absorb，**所有 absorb 求和的字段**
/// 必须与整表 expand 相等。
///
/// # 为什么这条能冻住 bug
///
/// 后端正是这样分段回放的（Plain 段一次 expand、Offscreen 内容一次 expand）。
/// 区间漏一段 → 某命令的滤镜计数消失 →「效果静默消失」诊断失灵；区间叠一段
/// → 同一命令计两次 → 计数器虚高。两者在单段帧上都看不出来，只在多滤镜帧上
/// 现形 —— 与 10-24 冻结链同族。
#[test]
fn stress_segmented_replay_conserves_every_counted_field() {
    for seed in 0..400u64 {
        let mut rng = Rng::new(seed ^ 0xFEEDFACE);
        let list = random_list(&mut rng);
        let whole = expand(&list, SIZE, REG).report;

        let mut sum = RenderReport::default();
        for seg in plan_segments(&list) {
            let range = match seg {
                Segment::Plain(r) => r,
                Segment::Offscreen { index, .. } | Segment::Custom { index } => index..index + 1,
            };
            let mut scratch = Scratch::new();
            let rep = expand_range_translated_into(
                &list,
                SIZE,
                &REG,
                Point::ZERO,
                Some(range),
                &mut scratch,
            );
            sum.absorb(rep);
        }
        assert_eq!(
            sum.unimplemented_filters, whole.unimplemented_filters,
            "seed {seed}: 分段回放丢了/重了滤镜计数"
        );
        assert_eq!(
            sum.unimplemented_blends, whole.unimplemented_blends,
            "seed {seed}: 分段回放丢了/重了混合计数"
        );
        assert_eq!(
            sum.quads, whole.quads,
            "seed {seed}: 分段回放丢了/重了四边形"
        );
        assert_eq!(
            sum.skipped, whole.skipped,
            "seed {seed}: 分段回放丢了/重了跳过计数"
        );
    }
}

// ── 守恒律 3：absorb 的合并契约 ──

/// `absorb` 对每个字段的合并方式（求和 / 取 max / 取或）必须逐字段钉死。
///
/// # 变异目标
///
/// 把 `vertex_bytes` 从 max 改成 sum（自相矛盾：报告写了 3 倍缓冲容量却没
/// 触发扩容）、把 `reallocated` 从或改成覆盖（后一段 false 抹掉前一段 true）
/// 时本条失败。
#[test]
fn stress_absorb_merges_each_field_by_its_documented_rule() {
    let mut rng = Rng::new(7);
    for _ in 0..200 {
        let a = RenderReport {
            quads: rng.below(50) as usize,
            skipped: rng.below(5) as usize,
            unimplemented_filters: rng.below(10) as usize,
            unimplemented_blends: rng.below(10) as usize,
            vertex_bytes: rng.below(10_000) as u64,
            reallocated: rng.below(2) == 0,
            image_batches: rng.below(8) as usize,
            missing_textures: rng.below(3) as usize,
            path_triangles: rng.below(30) as usize,
            offscreen_passes: rng.below(6) as usize,
            refraction_passes: rng.below(4) as usize,
            unhandled_custom: rng.below(4) as usize,
            custom_passes: rng.below(4) as usize,
            ..RenderReport::default()
        };
        let b = RenderReport {
            quads: rng.below(50) as usize,
            skipped: rng.below(5) as usize,
            unimplemented_filters: rng.below(10) as usize,
            unimplemented_blends: rng.below(10) as usize,
            vertex_bytes: rng.below(10_000) as u64,
            reallocated: rng.below(2) == 0,
            image_batches: rng.below(8) as usize,
            missing_textures: rng.below(3) as usize,
            path_triangles: rng.below(30) as usize,
            offscreen_passes: rng.below(6) as usize,
            refraction_passes: rng.below(4) as usize,
            unhandled_custom: rng.below(4) as usize,
            custom_passes: rng.below(4) as usize,
            ..RenderReport::default()
        };
        let mut m = a;
        m.absorb(b);
        assert_eq!(m.quads, a.quads + b.quads);
        assert_eq!(m.skipped, a.skipped + b.skipped);
        assert_eq!(
            m.unimplemented_filters,
            a.unimplemented_filters + b.unimplemented_filters
        );
        assert_eq!(
            m.unimplemented_blends,
            a.unimplemented_blends + b.unimplemented_blends
        );
        assert_eq!(m.offscreen_passes, a.offscreen_passes + b.offscreen_passes);
        assert_eq!(
            m.refraction_passes,
            a.refraction_passes + b.refraction_passes
        );
        assert_eq!(m.unhandled_custom, a.unhandled_custom + b.unhandled_custom);
        assert_eq!(m.custom_passes, a.custom_passes + b.custom_passes);
        assert_eq!(m.image_batches, a.image_batches + b.image_batches);
        assert_eq!(m.missing_textures, a.missing_textures + b.missing_textures);
        assert_eq!(m.path_triangles, a.path_triangles + b.path_triangles);
        assert_eq!(m.vertex_bytes, a.vertex_bytes.max(b.vertex_bytes));
        assert_eq!(m.reallocated, a.reallocated || b.reallocated);
    }
}

// ── 压力：一帧 4000 条命令的展开不炸且计数自洽 ──

/// 4000 条命令（一半带滤镜）整表 expand：计数自洽 + `runs` 恰好铺满顶点流。
///
/// # 为什么连 runs 一起查
///
/// `DrawRun` 的「不留缝、不重叠」是 draw call 切分的唯一保证；段数随命令数
/// 增长时，`push_run` 的相邻合并逻辑最容易在长序列上出错。
#[test]
fn stress_a_four_thousand_command_batch_is_internally_consistent() {
    let mut rng = Rng::new(0xD1CE);
    let mut l = DisplayList::new();
    for i in 0..4000u32 {
        let filter = if i % 2 == 0 {
            random_chain(&mut rng)
        } else {
            FilterChain::none()
        };
        l.push(
            SortKey {
                layer: Layer::Content,
                z: 0,
                order: i,
            },
            Command::Quad {
                rect: Rect::new((i % 64) as f32 * 16.0, (i / 64) as f32 * 16.0, 12.0, 12.0),
                radius: CornerRadius { radius: 0.0 },
                shape: crate::core::CornerShape::ROUND,
                fill: Color::rgb(1.0, 1.0, 1.0),
                stroke: None,
                filter,
                blend: BlendMode::SrcOver,
                decorations: crate::decoration::Decorations::none(),
            },
        );
    }
    l.sort();
    let batch = expand(&l, SIZE, REG);

    let exp_filters = l
        .iter()
        .filter(|i| command_filter(&i.command).is_some_and(|f| !f.is_identity()))
        .count();
    assert_eq!(batch.report.unimplemented_filters, exp_filters);
    assert_eq!(batch.report.unimplemented_blends, 0, "本场景全是默认混合");
    assert_eq!(batch.report.missing_textures, 0, "本场景无图像命令");
    assert!(batch.report.skipped == 0, "全部矩形可见，不该有跳过");

    // runs 铺满 solid 顶点流：**按 `first_vertex` 排序后**必须首尾相接。
    //
    // ⚠ 不能直接按 `runs` 的顺序断言 —— 段的先后是 **DisplayList 的顺序**，
    // 而 `first_vertex` 是它在合并流里的位置，两者的序**不必**一致
    // （实心的顶点在流头、图像的在中，但图像可能排在实心之前）。
    let mut by_pos: Vec<&crate::quad::DrawRun> = batch.runs.iter().collect();
    by_pos.sort_by_key(|r| r.first_vertex);
    let mut cursor = 0u32;
    for run in by_pos {
        assert_eq!(run.first_vertex, cursor, "runs 之间不留缝");
        cursor += run.vertex_count;
    }
    assert_eq!(
        cursor as usize,
        batch.solid.len(),
        "runs 必须恰好覆盖整条 solid 顶点流"
    );
}

/// 交错命令（矩形 / 图像）的展开：段的先后必须等于**命令的先后**。
///
/// # 为什么全矩形的老压力测试看不见顺序问题
///
/// 全是矩形时，「按图元类型分组」与「按 DisplayList 顺序」**恰好一致** ——
/// 那条老压力测试因此一直是绿的，而真实缺陷（图像盖住后面的矩形）就藏在
/// 它看不见的那半边。这条用**交错**的两种命令把两者分开。
///
/// 判据是「段的类别序列 = 命令类别序列的游程编码」：段能合并的只有
/// **紧邻**的同类命令，所以段序列与命令序列必须逐项吻合。
#[test]
fn stress_mixed_commands_keep_displaylist_order() {
    let mut l = DisplayList::new();
    const N: u32 = 900;
    for i in 0..N {
        let cmd = if i % 2 == 0 {
            Command::Quad {
                rect: Rect::new((i % 30) as f32 * 16.0, (i / 30) as f32 * 16.0, 12.0, 12.0),
                radius: CornerRadius { radius: 0.0 },
                shape: crate::core::CornerShape::ROUND,
                fill: Color::rgb(1.0, 1.0, 1.0),
                stroke: None,
                filter: FilterChain::none(),
                blend: BlendMode::SrcOver,
                decorations: crate::decoration::Decorations::none(),
            }
        } else {
            Command::Image {
                texture: crate::paint::TextureRef::new("t"),
                src: None,
                dst: Rect::new((i % 30) as f32 * 16.0, (i / 30) as f32 * 16.0, 12.0, 12.0),
                radius: CornerRadius { radius: 0.0 },
                shape: crate::core::CornerShape::ROUND,
                tint: Color::rgb(1.0, 1.0, 1.0),
                filter: FilterChain::none(),
                blend: BlendMode::SrcOver,
            }
        };
        l.push(
            SortKey {
                layer: Layer::Content,
                z: 0,
                order: i,
            },
            cmd,
        );
    }
    l.sort();
    let batch = expand(&l, SIZE, REG);

    // 段序列 = 命令类别序列的游程编码。
    //
    // 类别：0 = 实心（不换纹理绑定），1 = 图像（换绑定）。
    let mut expected: Vec<u8> = Vec::new();
    for i in 0..N {
        let kind = if i % 2 == 0 { 0u8 } else { 1 };
        if expected.last() != Some(&kind) {
            expected.push(kind);
        }
    }
    let actual: Vec<u8> = batch
        .runs
        .iter()
        .map(|r| if r.texture.is_some() { 1u8 } else { 0 })
        .collect();
    assert_eq!(
        actual, expected,
        "段的先后必须与命令的先后一致（类型分组会把图像全推到实心后面）"
    );

    // 段的顶点必须恰好铺满整条流（按位置排序后首尾相接）。
    let mut by_pos: Vec<&crate::quad::DrawRun> = batch.runs.iter().collect();
    by_pos.sort_by_key(|r| r.first_vertex);
    let mut cursor = 0u32;
    for run in by_pos {
        assert_eq!(run.first_vertex, cursor, "runs 之间不留缝");
        cursor += run.vertex_count;
    }
    assert_eq!(
        cursor as usize,
        batch.solid.len() + batch.images.len(),
        "runs 必须恰好覆盖整条顶点流"
    );
}
