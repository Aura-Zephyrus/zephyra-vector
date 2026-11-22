//! 着色器同步门禁：提交的 `.bin` 必须与 `.hlsl` 源逐字节一致。
//!
//! 跨 crate 后主仓的 mtime 门禁失效（文件不在主仓了），所以在这里用最可靠的
//! 办法：调 fxc 重编全部入口，与提交产物逐字节比对。只改 `.hlsl` 忘记重编
//! `.bin` 时这条会红 —— 而那种错误在 GPU 上表现为"改了没反应"，没有任何报错。

use std::path::PathBuf;
use std::process::Command;

fn fxc() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("FXC_PATH") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    // Windows SDK 的常见安装位置（本机实测 10.0.26100.0）。
    let root = PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10\bin");
    if let Ok(dirs) = std::fs::read_dir(root) {
        let mut candidates: Vec<PathBuf> = dirs
            .filter_map(Result::ok)
            .map(|e| e.path().join("x64").join("fxc.exe"))
            .filter(|p| p.exists())
            .collect();
        candidates.sort();
        if let Some(p) = candidates.pop() {
            return Some(p);
        }
    }
    // 最后试 PATH。
    let out = Command::new("where").arg("fxc.exe").output().ok()?;
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout);
        let first = s.lines().next()?.trim();
        let p = PathBuf::from(first);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

fn compile(fxc: &PathBuf, profile: &str, entry: &str, src: &PathBuf, dst: &PathBuf) {
    let out = Command::new(fxc)
        .args([
            "/nologo",
            "/T",
            profile,
            "/E",
            entry,
            "/Fo",
            dst.to_str().expect("非 UTF-8 路径"),
            src.to_str().expect("非 UTF-8 路径"),
        ])
        .output()
        .expect("fxc 调用失败");
    assert!(
        out.status.success(),
        "fxc 编译 {entry} 失败：{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn committed_dxbc_matches_hlsl_sources() {
    let Some(fxc) = fxc() else {
        eprintln!("跳过：找不到 fxc.exe（设置 FXC_PATH 可指定）");
        return;
    };
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("shaders");
    let src = dir.join("hlsl").join("zephyra.hlsl");
    let dxbc = dir.join("dxbc");
    let tmp = std::env::temp_dir().join(format!("zephyra_shader_gate_{}", std::process::id()));
    std::fs::create_dir_all(&tmp).expect("建临时目录失败");

    let mut checked = 0usize;
    let vs_out = tmp.join("zephyra.vs.bin");
    compile(&fxc, "vs_5_0", "VSMain", &src, &vs_out);
    assert_bytes_equal(&dxbc.join("zephyra.vs.bin"), &vs_out, "VSMain");
    checked += 1;

    for (name, _) in crate::shaders::PS_VARIANTS {
        let file = format!("zephyra.{name}.ps.bin");
        let out = tmp.join(&file);
        compile(&fxc, "ps_5_0", name, &src, &out);
        assert_bytes_equal(&dxbc.join(&file), &out, name);
        checked += 1;
    }
    assert_eq!(checked, 25, "应有 1 个 VS + 24 个 PS 入口");
    let _ = std::fs::remove_dir_all(&tmp);
}

/// NV12 变体的 `.bin` 也必须由 `zephyra_nv12.hlsl` 逐字节重编得到。
///
/// 与默认那套分开钉住：默认入口的字节码是 RGBA 路径的回归基准（一字未改），
/// 变体那套是新增能力；任一处「改了源忘重编」都会在这里红，而不是在 GPU 上
/// 表现成「NV12 画面颜色不对」。
#[test]
fn committed_nv12_dxbc_matches_hlsl_sources() {
    let Some(fxc) = fxc() else {
        eprintln!("跳过：找不到 fxc.exe（设置 FXC_PATH 可指定）");
        return;
    };
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("shaders");
    let src = dir.join("hlsl").join("zephyra_nv12.hlsl");
    let dxbc = dir.join("dxbc");
    let tmp = std::env::temp_dir().join(format!("zephyra_nv12_shader_gate_{}", std::process::id()));
    std::fs::create_dir_all(&tmp).expect("建临时目录失败");

    for (name, _) in crate::shaders::PS_NV12_VARIANTS {
        let file = format!("zephyra.{name}.ps.bin");
        let out = tmp.join(&file);
        compile(&fxc, "ps_5_0", name, &src, &out);
        assert_bytes_equal(&dxbc.join(&file), &out, name);
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

fn assert_bytes_equal(expected: &PathBuf, actual: &PathBuf, name: &str) {
    let a = std::fs::read(expected).unwrap_or_else(|e| panic!("读 {expected:?} 失败: {e}"));
    let b = std::fs::read(actual).unwrap_or_else(|e| panic!("读 {actual:?} 失败: {e}"));
    assert!(
        a == b,
        "{name} 的 .bin 与 .hlsl 不一致 —— 改了源但忘了重编 fxc"
    );
}

#[test]
fn wgsl_parts_concatenate_to_a_valid_module_shape() {
    // 这条不依赖 naga：只钉住拼接顺序与入口存在性，真正编译在宿主 build script。
    let src = crate::shaders::wgsl_source();
    assert!(src.contains("fn vs_main"), "缺少顶点入口");
    assert!(src.contains("fn fs_main"), "缺少主片元入口");
    assert!(src.contains("fn fs_refraction"), "缺少折射入口");
    // 字形分支按 mode 分派：覆盖率(0)/SDF(1)/MSDF(2)/LCD(3,4)。
    // SDF 模式仍读 red 通道；旧覆盖率路径在 mode < 0.5 时读 alpha。
    assert!(src.contains("smoothstep(0.5 - aa, 0.5 + aa, texel.r)"));
    assert!(src.contains("mode < 0.5"));
    assert!(src.contains("coverage = texel.a"));
    // MSDF 的中位数重建与伪粗体阈值；AA 半宽改用屏幕导数自适应。
    assert!(src.contains("max(min(texel.r, texel.g)"));
    assert!(src.contains("0.5 - aa - emb"));
    assert!(src.contains("fwidth(s_msdf)"));
    // LCD 三次采样：子像素步长 = 每像素 uv 步长 / 3。
    // LCD 用 Skia 12-tap FIR，不再是 3 点等权采样。
    assert!(src.contains("FIR_R") && src.contains("FIR_G") && src.contains("FIR_B"));
    assert!(src.contains("gamma_correct_coverage"));
}

// ── WGSL 折射 / 背景模糊入口的字符串级门禁（从 wgsl_refraction_gate.rs 移植）──
//
// `fs_refraction` 与 HLSL 的 `PSRefraction` 是**两份手抄**（对应关系写在 WGSL
// 文件头），像素级对照要真 GPU（WSL2 上的 gallery），不能每次提交都跑。手抄
// 最危险的不是写错，而是**改着改着退回到旧版**—— 旧版四个坑（宽遮罩、薄棱镜
// 近似、bezel 按圆角半径夹、漏色散）每一个都「画面合法、不报错」，只有下面
// 这些断言能在提交前拦住。
//
// 每条断言钉一个已实测过的失效模式，出处见 `docs/dev-log/` 的折射三条日志。

/// 与构建期同一份源（都来自 `crate::shaders::wgsl_source()`）。
fn wgsl() -> String {
    crate::shaders::wgsl_source()
}

/// 入口必须存在 —— 漏了它 `create_entry_pipeline` 要到运行期建管线时才报错，
/// 而那时的症状是「启动即失败」，与「管线建错」无法区分。
#[test]
fn fs_refraction_entry_exists() {
    assert!(
        wgsl().contains("fn fs_refraction(i: VsOut)"),
        "WGSL 里没有 fs_refraction 入口 —— 折射在 Linux 侧会整个消失"
    );
}

/// 遮罩必须是 1px 抗锯齿带。
///
/// 钉的是「宽遮罩把最强折射乘成 0」那个实测差 6 倍的问题（10-22 日志 2.3 节）：
/// `min(half_size) * 0.5` 那一版在边缘正好是 0。
#[test]
fn refraction_mask_is_a_one_pixel_band() {
    assert!(
        wgsl().contains("let edge = 1.0;"),
        "折射遮罩的 edge 不再是 1px —— 宽遮罩会把边缘处最强的折射乘成 0（实测差 6 倍）"
    );
}

/// 倒角带**不按圆角半径夹**。
///
/// 钉的是「倒角参数上 3/4 档变空档」那个问题（10-22 日志 2.4 节）：按
/// `min(bezel, radius)` 夹时，12px 圆角的玻璃块上 26/34/48px 三档全没反应。
/// liquid-glass 网页 demo 的 WebGL 版正是被半径夹的那一版。
#[test]
fn bezel_is_not_clamped_by_the_corner_radius() {
    assert!(
        wgsl().contains("let bezel = min(max(i.color.g, 1.0), min(i.half_size.x, i.half_size.y));"),
        "折射 bezel 的夹法变了 —— 只能按半边长夹，按圆角半径夹会让倒角参数的上 3/4 变空档"
    );
}

/// 位移必须是有界的楔形偏折式。
///
/// 钉的是「薄棱镜近似在大角度下发散」那个问题（10-22 日志 2.2 节）：旧式
/// `h * (tan(Θs) - tan(Θr))` 实测位移峰值 491px，而契约标称值（厚度）是 28px。
#[test]
fn displacement_is_the_bounded_wedge_form() {
    assert!(
        wgsl().contains("sin(slope - theta_r) / max(cos(theta_r), 1e-3)"),
        "位移公式不是有界楔形偏折式 —— 薄棱镜近似会在 t→0（Θs→90°）处发散（实测 491px vs 28px）"
    );
    assert!(
        !wgsl().contains("tan(slope) - tan(theta_r)"),
        "位移公式里出现了薄棱镜近似 —— 它在大角度下发散，实测位移是厚度的 17~27 倍"
    );
}

/// 色散必须把 R 与 B 往**相反**方向拉。
///
/// 钉的是色散生效的判据（10 月折射日志的色散门禁）：只拉一个通道时红蓝交替
/// 背景上与无色散逐像素相同，看着像「色散没接上」。
#[test]
fn dispersion_offsets_red_and_blue_in_opposite_directions() {
    assert!(
        wgsl().contains("off_uv * (1.0 + dispersion)")
            && wgsl().contains("off_uv * (1.0 - dispersion)"),
        "色散的两个方向系数不完整 —— R/B 必须沿法线反向拉开，否则色散永不生效"
    );
}

/// 背景模糊入口必须存在，且采样的是 `g_dst`（背后）而不是 `g_tex`（元素自己）。
///
/// 与 `fs_refraction` 同一条手抄纪律：漏了入口，`fs_backdrop_blur` 在 Linux 侧
/// 整个消失；采样错了纹理，磨砂糊的是元素自己而不是背后，症状是「磨砂像普通模糊」。
#[test]
fn fs_backdrop_blur_exists_and_samples_the_backdrop() {
    assert!(
        wgsl().contains("fn fs_backdrop_blur(i: VsOut)"),
        "WGSL 里没有 fs_backdrop_blur 入口 —— 磨砂在 Linux 侧会整个消失"
    );
    // 在这个入口的源码段里，采样必须是 g_dst（元素背后），不是 g_tex（元素自己）。
    let src = wgsl();
    let start = src
        .find("fn fs_backdrop_blur(i: VsOut)")
        .expect("上面刚断言存在");
    let body = &src[start..];
    let end = body.find("\n}").map(|e| e + 2).unwrap_or(body.len());
    let body = &body[..end];
    assert!(
        body.contains("textureSampleLevel(g_dst"),
        "fs_backdrop_blur 必须采样 g_dst（背后画面），否则磨砂糊的是元素自己"
    );
    assert!(
        body.contains("gauss_weight"),
        "fs_backdrop_blur 必须用高斯权重做卷积"
    );
    assert!(
        body.contains("let mask = saturate(-d / 1.0);"),
        "fs_backdrop_blur 的遮罩必须是 1px 抗锯齿带（与折射同一条纪律）"
    );
}
