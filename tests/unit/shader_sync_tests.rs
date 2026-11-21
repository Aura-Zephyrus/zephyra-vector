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
    assert!(src.contains("duv_dx * dir / 3.0"));
}
