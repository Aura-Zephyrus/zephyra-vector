// 离屏规划的压力测试：确定性伪随机遍历「滤镜链 × 内容矩形 × 命令序列」的
// 组合空间，断言 plan_segments / plan_offscreens / pass_for_chain 的**结构
// 不变量**。
//
// # 为什么是压力测试而不是更多用例
//
// 已有用例钉的是**单点**行为（某条链算出某张表）。而本文件钉的是**任意**输入
// 下都成立的关系 —— 那些关系一旦被破坏，症状是"某个元素的阴影/折射莫名其妙
// 不对"，在 demo 上要靠肉眼一帧帧比对，而在这里是一次 assert。
//
// # 为什么用自制 PRNG 而不加 rand 依赖
//
// 本 crate `unsafe_code = "forbid"` 且零依赖（见 lib.rs 顶部）。xorshift64*
// 十二行，固定 seed 完全确定 —— 测试的可复现性比"分布质量"重要得多。

use super::*;
use crate::core::{Color, CornerRadius, Point, Rect};
use crate::filter::{BlendMode, Filter};
use crate::paint::{Layer, SortKey};

// ── 确定性伪随机 ──

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // xorshift 的状态不能为 0；固定常量保证任何 seed 都进入满周期。
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
    /// [lo, hi) 上的 f32。24 位分辨率对像素几何足够。
    fn f32_in(&mut self, lo: f32, hi: f32) -> f32 {
        let t = (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32;
        lo + t * (hi - lo)
    }
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u32) as usize]
    }
}

// ── 随机输入构造 ──

/// 随机滤镜链：0..=4 个滤镜，σ / offset / 折射参数都取**合法但跨度大**的值。
///
/// 刻意包含 σ=0（恒等趟）、负 σ（必须被 push_blur 挡掉）、以及极大 σ
/// （离屏矩形膨胀到 u32 饱和）—— 这三类在真机上分别对应"白付两次 draw"、
/// "矩形反而缩小"和"程序卡死"，都必须是良构的。
fn random_chain(rng: &mut Rng) -> FilterChain {
    let n = rng.below(5); // 0..=4
    let mut v = Vec::new();
    for _ in 0..n {
        let f = match rng.below(6) {
            0 | 1 => Filter::Blur {
                sigma_x: rng.f32_in(0.0, 40.0),
                sigma_y: rng.f32_in(0.0, 40.0),
            },
            2 => Filter::Blur {
                sigma_x: 0.0,
                sigma_y: rng.f32_in(0.0, 5.0),
            },
            3 => Filter::DropShadow {
                color: Color::rgba(rng.f32_in(0.0, 1.0), 0.0, 0.0, 0.5),
                offset: Point::new(rng.f32_in(-30.0, 30.0), rng.f32_in(-30.0, 30.0)),
                sigma: rng.f32_in(0.0, 20.0),
            },
            4 => Filter::Refraction {
                strength: rng.f32_in(1.0, 60.0),
                bezel: rng.f32_in(1.0, 40.0),
                dispersion: rng.f32_in(0.0, 1.0),
                ior: rng.f32_in(1.0, 2.2),
            },
            _ => Filter::Blur {
                // 负 σ：outsets 必须 clamp 到 0，否则离屏矩形比内容还小。
                sigma_x: -rng.f32_in(0.0, 10.0),
                sigma_y: rng.f32_in(0.0, 10.0),
            },
        };
        v.push(f);
    }
    FilterChain::new(v)
}

/// 随机内容矩形：正常小矩形为主，偶发退化矩形（0 宽 / 负宽 / 极小）。
fn random_rect(rng: &mut Rng) -> Rect {
    match rng.below(16) {
        0 => Rect::new(
            rng.f32_in(-500.0, 500.0),
            rng.f32_in(-500.0, 500.0),
            0.0,
            10.0,
        ),
        1 => Rect::new(
            rng.f32_in(-500.0, 500.0),
            rng.f32_in(-500.0, 500.0),
            10.0,
            0.0,
        ),
        2 => Rect::new(
            rng.f32_in(-500.0, 500.0),
            rng.f32_in(-500.0, 500.0),
            -5.0,
            10.0,
        ),
        3 => Rect::new(0.0, 0.0, 0.001, 0.001),
        _ => Rect::new(
            rng.f32_in(-500.0, 1500.0),
            rng.f32_in(-500.0, 1500.0),
            rng.f32_in(0.5, 400.0),
            rng.f32_in(0.5, 400.0),
        ),
    }
}

fn random_blend(rng: &mut Rng) -> BlendMode {
    *rng.pick(&[
        BlendMode::SrcOver,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::SoftLight,
        BlendMode::Plus,
        BlendMode::Xor,
        BlendMode::Hue,
        BlendMode::Luminosity,
    ])
}

fn random_command(rng: &mut Rng) -> Command {
    match rng.below(9) {
        0..=3 => Command::Quad {
            rect: random_rect(rng),
            radius: CornerRadius {
                radius: rng.f32_in(0.0, 24.0),
            },
            shape: crate::core::CornerShape::ROUND,
            fill: Color::rgb(1.0, 1.0, 1.0),
            stroke: None,
            filter: random_chain(rng),
            blend: random_blend(rng),
            decorations: crate::decoration::Decorations::none(),
        },
        4 => Command::Path {
            vertices: std::sync::Arc::from(
                (0..3 + rng.below(9))
                    .map(|_| crate::paint::Vertex {
                        position: Point::new(rng.f32_in(0.0, 300.0), rng.f32_in(0.0, 300.0)),
                        uv: Point::new(0.0, 0.0),
                    })
                    .collect::<Vec<_>>(),
            ),
            fill: Color::rgb(1.0, 1.0, 1.0),
            filter: random_chain(rng),
            blend: random_blend(rng),
        },
        5 => Command::Image {
            dst: random_rect(rng),
            src: None,
            tint: Color::rgb(1.0, 1.0, 1.0),
            texture: crate::paint::TextureRef(std::borrow::Cow::Borrowed("atlas")),
            radius: CornerRadius { radius: 0.0 },
            shape: crate::core::CornerShape::ROUND,
            filter: random_chain(rng),
            blend: random_blend(rng),
        },
        6 => Command::PushClip(random_rect(rng)),
        7 => Command::PopClip,
        // 外部渲染源：随机滤镜（恒等 → 单独成 Custom 段；非恒等 → 进离屏段）。
        // kind 非空 = 语义上的「有源」（空 kind 是渲染降级的「无」）。
        _ => Command::Custom {
            bounds: random_rect(rng),
            source: crate::paint::RenderSource::new("stress:custom", rng.below(u32::MAX)),
            resource_key: None,
            filter: random_chain(rng),
            blend: random_blend(rng),
        },
    }
}

fn random_list(rng: &mut Rng) -> DisplayList {
    let mut l = DisplayList::new();
    let n = rng.below(48);
    for i in 0..n {
        l.push(
            SortKey {
                layer: Layer::Content,
                z: 0,
                order: i,
            },
            random_command(rng),
        );
    }
    l.sort();
    l
}

/// 本文件自带的 Quad 构造助手（`tests` 模块里的同名助手是它的私有成员，
/// 跨模块取不到，也不想为此把生产可见性放宽）。
fn quad(filter: FilterChain, blend: BlendMode, rect: Rect) -> Command {
    Command::Quad {
        rect,
        radius: CornerRadius { radius: 0.0 },
        shape: crate::core::CornerShape::ROUND,
        fill: Color::rgb(1.0, 1.0, 1.0),
        stroke: None,
        filter,
        blend,
        decorations: crate::decoration::Decorations::none(),
    }
}

fn list_of(cmds: Vec<Command>) -> DisplayList {
    let mut l = DisplayList::new();
    for (i, c) in cmds.into_iter().enumerate() {
        l.push(
            SortKey {
                layer: Layer::Content,
                z: 0,
                order: i as u32,
            },
            c,
        );
    }
    l.sort();
    l
}

// ── 不变量 1：plan_segments 的结构 ──

/// 段序列必须**恰好**是「Plain, Offscreen/Custom, Plain, …, Plain」：
/// 段数 = 滤镜命令数 × 2 + 自绘源段数 × 2 + 1，且所有区间严严实实铺满 [0, len)。
///
/// # 变异目标
///
/// 漏 push 空段（`segments.len()` 关系破坏）、`start` 更新写成 `i` 而不是
/// `i + 1`（区间重叠 → 同一条命令画两遍）、末段忘 push（尾部命令消失）时
/// 本条必须失败。三类症状都是"画面多一层 / 少一块且不报错"。
#[test]
fn stress_plan_segments_tile_the_list_exactly() {
    for seed in 0..300u64 {
        let mut rng = Rng::new(seed);
        let list = random_list(&mut rng);
        let segs = plan_segments(&list);

        let filtered: Vec<usize> = list
            .iter()
            .enumerate()
            .filter(|(_, item)| plan_pass(&item.command).is_some())
            .map(|(i, _)| i)
            .collect();
        // 恒等滤镜的自绘源单独成 `Custom` 段（见 `Segment::Custom`）。
        let customs: Vec<usize> = list
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                matches!(
                    &item.command,
                    Command::Custom { source, .. }
                        if !source.is_none()
                            && crate::paint::command_filter(&item.command)
                                .is_some_and(|f| f.is_identity())
                )
            })
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            segs.len(),
            2 * filtered.len() + 2 * customs.len() + 1,
            "seed {seed}: 段数必须等于滤镜命令数 × 2 + 自绘源段数 × 2 + 1（严格交替，首尾 Plain）"
        );

        let mut cursor = 0usize;
        for (k, seg) in segs.iter().enumerate() {
            match (k % 2, seg) {
                (0, Segment::Plain(r)) => {
                    assert_eq!(r.start, cursor, "seed {seed}: Plain 段必须紧接上一段");
                    cursor = r.end;
                }
                (
                    1,
                    Segment::Offscreen {
                        index,
                        pass,
                        redraw,
                        key,
                    },
                ) => {
                    assert_eq!(*index, cursor, "seed {seed}: 离屏命令必须紧跟 Plain 段");
                    cursor = index + 1;
                    // 段里缓存的那份计划必须与现算的一致（缓存过期 = 画面与规划脱节）。
                    let fresh = plan_pass(&list.iter().nth(*index).unwrap().command)
                        .expect("进段了的命令必须能算计划");
                    assert_eq!(
                        **pass, fresh,
                        "seed {seed}: 段内 pass 必须与 plan_pass 一致"
                    );
                    assert_eq!(
                        *redraw, fresh.redraw_original,
                        "seed {seed}: redraw 标志必须与 pass 一致"
                    );
                    assert_eq!(
                        *key,
                        list.iter().nth(*index).unwrap().key,
                        "seed {seed}: 排序键必须取原命令的"
                    );
                }
                (1, Segment::Custom { index }) => {
                    assert_eq!(*index, cursor, "seed {seed}: 自绘源命令必须紧跟 Plain 段");
                    cursor = index + 1;
                    // 进了 Custom 段的命令必须是「有源 + 恒等滤镜」的自绘——
                    // 带滤镜的 Custom 该进离屏段（v1 离屏不支持自绘，展开层计数）。
                    match &list.iter().nth(*index).unwrap().command {
                        Command::Custom { source, filter, .. } => {
                            assert!(!source.is_none(), "seed {seed}: 空源不该进段");
                            assert!(
                                filter.is_identity(),
                                "seed {seed}: 带非恒等滤镜的 Custom 该进离屏段"
                            );
                        }
                        other => panic!("seed {seed}: Custom 段里是 {other:?}"),
                    }
                }
                _ => panic!("seed {seed}: 段序列必须严格交替，第 {k} 段是 {seg:?}"),
            }
        }
        assert_eq!(cursor, list.len(), "seed {seed}: 末段必须收在 list.len()");
    }
}

// ── 不变量 2：plan_offscreens 的包含关系与去重契约 ──

/// 每条离屏规划的 `bounds` 必须**包含** `content`，且 (bounds, filter)
/// 二元组不得重复 —— 重合矩形换滤镜是允许的（那是两张不同的 RT）。
///
/// # 变异目标
///
/// outsets 某一边写成负数（bounds 不含 content → 模糊在边缘被硬切）、
/// 去重判据从「矩形相等」放宽成「重叠」（两张 RT 复用错 → 画面错位）时
/// 本条必须失败。
#[test]
fn stress_plan_offscreens_bounds_contain_content_without_bad_merges() {
    const EPS: f32 = 1e-3;
    for seed in 0..300u64 {
        let mut rng = Rng::new(seed ^ 0x5DEECE66D);
        let list = random_list(&mut rng);
        let plans = plan_offscreens(&list);

        for p in &plans {
            assert!(
                p.bounds.x <= p.content.x + EPS
                    && p.bounds.y <= p.content.y + EPS
                    && p.bounds.right() >= p.content.right() - EPS
                    && p.bounds.bottom() >= p.content.bottom() - EPS,
                "seed {seed}: bounds {:?} 必须包含 content {:?}",
                p.bounds,
                p.content
            );
            assert!(
                p.bounds.width >= p.content.width - EPS
                    && p.bounds.height >= p.content.height - EPS,
                "seed {seed}: bounds 不得比 content 小"
            );
            // origin 与 plan_pass 的口径一致（两个函数各算一遍 origin 的话，
            // 漂移的症状是"元素左上角一小段空白"）。
            //
            // ⚠ 空/非有限内容**刻意**只进规划不进执行计划 —— 见
            // `offscreen_tests::empty_path_gives_zero_sized_bounds` 立下的设计：
            // 零尺寸护栏归执行侧（绑 RT 时），规划层只如实报告。所以这里分两支：
            // 有 pass 的必须原点一致；没 pass 的必须确实是退化内容，不能是别的理由。
            let cmd = list.iter().nth(p.command_index).unwrap();
            match plan_pass(&cmd.command) {
                Some(pass) => {
                    assert_eq!(
                        (p.bounds.x, p.bounds.y),
                        pass.origin,
                        "seed {seed}: OffscreenPlan.origin 与 OffscreenPass.origin 必须一致"
                    );
                }
                None => {
                    let degenerate = !(p.content.width > 0.0
                        && p.content.height > 0.0
                        && p.content.x.is_finite()
                        && p.content.y.is_finite()
                        && p.content.width.is_finite()
                        && p.content.height.is_finite());
                    assert!(
                        degenerate,
                        "seed {seed}: 内容 {:?} 既不退化又算不出 pass，规划层口径不一致",
                        p.content
                    );
                }
            }
        }
        for i in 0..plans.len() {
            for j in i + 1..plans.len() {
                assert!(
                    plans[i].bounds != plans[j].bounds || plans[i].filter != plans[j].filter,
                    "seed {seed}: (bounds, filter) 都相同的两条规划必须被合并"
                );
            }
        }
    }
}

// ── 不变量 3：pass_for_chain 的趟表数学 ──

/// 对**任意**随机链，模糊趟数 / 顺序 / redraw / tint / translate / rt_size
/// 六个量必须与「按 filters 从头推一遍」的期望值相等。
///
/// # 变异目标
///
/// 这是本文件对 `pass_for_chain` 最全面的一条锁：排序 blurs（σ 序列乱序 →
/// 模糊方向错）、把 DropShadow 的 offset 加到 translate 时漏加或多加一次
/// （影子画在原位 / 偏两倍）、tint 取第一个投影而不是最后一个（链语义反了）、
/// rt_size 的 `.max(1)` 删掉（0×0 视口下 D3D11 报错）—— 全部在这里失败。
#[test]
fn stress_pass_for_chain_step_table_matches_the_filters() {
    for seed in 0..500u64 {
        let mut rng = Rng::new(seed ^ 0xABCDEF01);
        let chain = random_chain(&mut rng);
        let content = Rect::new(
            rng.f32_in(-1000.0, 1000.0),
            rng.f32_in(-1000.0, 1000.0),
            rng.f32_in(0.001, 800.0),
            rng.f32_in(0.001, 800.0),
        );
        let p = pass_for_chain(
            &chain,
            content,
            &crate::decoration::Decorations::none(),
            0.0,
        );

        // 期望值：按 filters[0] 起依次推。
        let mut exp_blurs = 0usize;
        let mut exp_tint: Option<Color> = None;
        let mut exp_offset = Point::new(0.0, 0.0);
        let mut exp_redraw = false;
        for f in chain.filters() {
            match f {
                Filter::Blur { sigma_x, sigma_y } => {
                    if *sigma_x > 0.0 && sigma_x.is_finite() {
                        exp_blurs += 1;
                    }
                    if *sigma_y > 0.0 && sigma_y.is_finite() {
                        exp_blurs += 1;
                    }
                }
                Filter::DropShadow {
                    color,
                    offset,
                    sigma,
                } => {
                    if *sigma > 0.0 && sigma.is_finite() {
                        exp_blurs += 2;
                    }
                    exp_offset.x += offset.x;
                    exp_offset.y += offset.y;
                    exp_tint = Some(*color);
                    exp_redraw = true;
                }
                Filter::Refraction { .. } => exp_redraw = true,
                // 透镜与折射同：合成输出的是背景，原命令要之后再画一遍。
                Filter::Lens { .. } => exp_redraw = true,
                // 背景模糊与折射同：合成输出的是背景，原命令要之后再画一遍。
                Filter::BackdropBlur { .. } => exp_redraw = true,
                // 像素偏移玻璃**不占离屏的任何一环**（位移是解析的）⇒
                // 既不产模糊趟，也不重画原图。
                Filter::PixelOffset { .. } => {}
            }
        }
        assert_eq!(p.blurs.len(), exp_blurs, "seed {seed}: 趟数对不上");
        assert_eq!(p.tint, exp_tint, "seed {seed}: tint 必须是最后一个投影的色");
        assert_eq!(
            p.redraw_original, exp_redraw,
            "seed {seed}: 投影/折射必须重画原图，纯模糊不重画"
        );
        for b in &p.blurs {
            assert!(
                b.sigma > 0.0 && b.sigma.is_finite(),
                "seed {seed}: σ≤0 的趟必须被丢掉"
            );
        }

        // bounds / rt_size / translate 三个几何量。
        let exp_bounds = chain.bounds_for(content);
        assert_eq!(
            p.bounds, exp_bounds,
            "seed {seed}: bounds 必须等于 bounds_for"
        );
        assert_eq!(p.origin, (exp_bounds.x, exp_bounds.y));
        assert_eq!(
            p.rt_size,
            (
                (exp_bounds.width.ceil() as u32).max(1),
                (exp_bounds.height.ceil() as u32).max(1)
            ),
            "seed {seed}: rt_size 必须向上取整且 ≥1"
        );
        assert!(
            p.rt_size.0 >= 1 && p.rt_size.1 >= 1,
            "seed {seed}: RT 尺寸永不塌成 0"
        );
        assert_eq!(
            p.translate,
            (-exp_bounds.x + exp_offset.x, -exp_bounds.y + exp_offset.y),
            "seed {seed}: translate = -origin + Σoffset"
        );
    }
}

// ── 不变量 4：退化输入不 panic ──

/// NaN / Inf / 0 / 负 / 极值矩形 × 含 NaN/负 σ 的链：plan_pass 要么干净
/// 返回 None，要么给出 rt_size ≥ 1 的良构计划 —— **任何输入都不许 panic**。
///
/// # 为什么单测这条
///
/// 非有限值穿过 `ceil` 变天文数字是真实事故模式（症状是分配失败/卡死，
/// 不是"某个元素少画了"）。本 crate 零 unsafe，panic 只会来自显式断言，
/// 但后续任何重构引入 unwrap/index 时，这条就是第一道网。
#[test]
fn stress_degenerate_inputs_never_panic() {
    let rects = [
        Rect::new(f32::NAN, 0.0, 10.0, 10.0),
        Rect::new(0.0, f32::NAN, 10.0, 10.0),
        Rect::new(0.0, 0.0, f32::NAN, 10.0),
        Rect::new(0.0, 0.0, 10.0, f32::NAN),
        Rect::new(f32::INFINITY, 0.0, 10.0, 10.0),
        Rect::new(0.0, 0.0, f32::INFINITY, 10.0),
        Rect::new(f32::NEG_INFINITY, 0.0, 10.0, 10.0),
        Rect::new(0.0, 0.0, -10.0, 10.0),
        Rect::new(0.0, 0.0, 10.0, -10.0),
        Rect::new(0.0, 0.0, 0.0, 0.0),
        Rect::new(0.0, 0.0, 0.001, 0.001),
        Rect::new(-1e30, -1e30, 1e30, 1e30),
    ];
    let chains = [
        FilterChain::single(Filter::Blur {
            sigma_x: f32::NAN,
            sigma_y: f32::NAN,
        }),
        FilterChain::single(Filter::Blur {
            sigma_x: -5.0,
            sigma_y: -5.0,
        }),
        FilterChain::single(Filter::Blur {
            sigma_x: 1e30,
            sigma_y: 1e30,
        }),
        FilterChain::single(Filter::DropShadow {
            color: Color::rgba(0.0, 0.0, 0.0, 0.5),
            offset: Point::new(f32::NAN, 0.0),
            sigma: 4.0,
        }),
        FilterChain::single(Filter::Refraction {
            strength: 1e30,
            bezel: f32::NAN,
            dispersion: 0.5,
            ior: 1.5,
        }),
        FilterChain::new([
            Filter::Blur {
                sigma_x: 2.0,
                sigma_y: 2.0,
            },
            Filter::DropShadow {
                color: Color::rgba(0.0, 0.0, 0.0, 1.0),
                offset: Point::new(-1e30, -1e30),
                sigma: 1e30,
            },
        ]),
    ];
    for (ri, r) in rects.iter().enumerate() {
        for (ci, c) in chains.iter().enumerate() {
            // plan_pass 的守卫路径：非有限 / 空内容 → None，不许 panic。
            let cmd = quad(c.clone(), BlendMode::SrcOver, *r);
            let plan = plan_pass(&cmd);
            if let Some(p) = plan {
                assert!(
                    p.rt_size.0 >= 1 && p.rt_size.1 >= 1,
                    "rect {ri} chain {ci}: RT 塌成 0"
                );
            }
            // 内核路径（绕过守卫）：同样不许 panic。
            let direct = pass_for_chain(c, *r, &crate::decoration::Decorations::none(), 0.0);
            assert!(
                direct.rt_size.0 >= 1 && direct.rt_size.1 >= 1,
                "rect {ri} chain {ci}: 内核 RT 塌成 0"
            );
            // 段规划整表跑一遍也不许 panic。
            let list = list_of(vec![
                quad(c.clone(), BlendMode::SrcOver, *r),
                quad(
                    c.clone(),
                    BlendMode::Multiply,
                    Rect::new(0.0, 0.0, 50.0, 50.0),
                ),
                Command::PopClip,
            ]);
            let _ = plan_segments(&list);
            let _ = plan_offscreens(&list);
        }
    }
}

// ── 不变量 5：count_non_default_blends 与逐命令手数一致 ──

/// 计数器必须逐命令数，且**不受滤镜影响**（混合模式不产生离屏需求）。
///
/// # 变异目标
///
/// 漏数某个命令变体（Path / Text 忘了算）、把 PushClip/PopClip 也数进去、
/// 或者把「滤镜命令」误判成「非默认混合」时本条失败。
#[test]
fn stress_blend_counter_matches_a_manual_per_command_count() {
    for seed in 0..300u64 {
        let mut rng = Rng::new(seed ^ 0x13579BDF);
        let list = random_list(&mut rng);
        let expected = list
            .iter()
            .filter(|i| {
                let b = match &i.command {
                    Command::Quad { blend, .. }
                    | Command::Path { blend, .. }
                    | Command::Image { blend, .. }
                    | Command::Text { blend, .. }
                    | Command::Custom { blend, .. } => *blend,
                    Command::PushClip(_) | Command::PopClip => BlendMode::default(),
                };
                b != BlendMode::default()
            })
            .count();
        assert_eq!(
            count_non_default_blends(&list),
            expected,
            "seed {seed}: 混合模式计数必须逐命令"
        );
    }
}

/// 带装饰的玻璃：**外投影与滤镜是两层**，不是二选一。
///
/// `hollow_shadow` 是装饰的外投影（内侧要挖掉），滤镜是玻璃本体。两者同时
/// 成立时计划里必须**都**在。曾经 D3D12 / D3D11 的合成趟写成
/// `if hollow_shadow { 投影 } else { 滤镜 }`，于是所有带装饰的玻璃
/// （开关把手、材质页玻璃卡片）把折射 / 透镜 / 磨砂整趟丢掉 —— 只剩高光与
/// 内阴影。而「没有折射的玻璃」看起来完全正常，只能靠材质页「开/关装饰」的
/// 对照看出来。Vulkan 侧一直是两层（先 `hollow_composite`，再按 `dst_effect`
/// 画），这条把三后端钉到同一个语义上。
#[test]
fn a_decorated_glass_plans_both_the_outer_shadow_and_the_filter() {
    let chain = FilterChain::single(Filter::Lens {
        refraction_height: 5.0,
        refraction_amount: 10.0,
    });
    let decorations = crate::decoration::Decorations {
        highlight: None,
        shadow: Some(crate::decoration::OuterShadow::glass()),
        inner_shadow: None,
    };
    let content = Rect::new(10.0, 10.0, 40.0, 24.0);

    let p = pass_for_chain(&chain, content, &decorations, 12.0);

    assert!(p.hollow_shadow, "外投影要单独占一趟合成（内侧要挖掉）");
    assert!(
        p.lens.is_some(),
        "装饰不能把透镜挤掉：外投影在玻璃背后，两者叠加"
    );
    assert!(p.redraw_original, "透镜合成之后元素自己的内容要再画一遍");
}

// ── 不变量 6：巨型帧不炸 ──

/// 2000 条命令（其中过半带滤镜）一整帧：规划必须秒出且结构不变量仍成立。
/// 这条同时是**性能哨兵** —— 规划是 O(n²) 的话（如每条命令扫全表），
/// 这里会以秒计的耗时暴露。
#[test]
fn stress_a_two_thousand_command_frame_plans_instantly() {
    let mut rng = Rng::new(0xC0FFEE);
    let mut l = DisplayList::new();
    for i in 0..2000u32 {
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
                rect: Rect::new((i % 100) as f32 * 8.0, (i / 100) as f32 * 8.0, 6.0, 6.0),
                radius: CornerRadius { radius: 2.0 },
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
    let segs = plan_segments(&l);
    let n_filtered = l.iter().filter(|i| plan_pass(&i.command).is_some()).count();
    assert_eq!(
        segs.len(),
        2 * n_filtered + 1,
        "巨型帧的段数不变量（2×滤镜数+1）"
    );
    let plans = plan_offscreens(&l);
    assert_eq!(
        plans.len(),
        n_filtered.min(plans.len()),
        "规划数不得越过命令数"
    );
}
