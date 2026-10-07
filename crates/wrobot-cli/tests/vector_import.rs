//! 命令行矢量导入（DXF / SVG）端到端测试：直接驱动编译出的 wrobot-cli 二进制，
//! 验证「导入 → 笔迹 → G-code / SVG」整条链路在真实进程里跑通。

use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_wrobot-cli"))
}

/// 一条从 (0,0) 到 (0,100) 的竖线（DXF 中 Y 向上）。
fn minimal_dxf() -> String {
    [
        "0", "SECTION", "2", "ENTITIES", "0", "LINE", "10", "0", "20", "0", "11", "0", "21", "100",
        "0", "ENDSEC", "0", "EOF",
    ]
    .join("\n")
}

fn minimal_svg() -> &'static str {
    r#"<svg><rect x="10" y="20" width="100" height="50"/><line x1="0" y1="0" x2="30" y2="40"/></svg>"#
}

#[test]
fn dxf_import_produces_gcode_and_svg() {
    let dir = std::env::temp_dir().join("wrobot_cli_dxf");
    let _ = std::fs::create_dir_all(&dir);
    let dxf = dir.join("t.dxf");
    std::fs::write(&dxf, minimal_dxf()).unwrap();

    // 1) DXF → G-code（写文件）
    let nc = dir.join("t.nc");
    let out = bin()
        .arg("dxf")
        .arg(&dxf)
        .arg("-o")
        .arg(&nc)
        .output()
        .expect("应成功运行 wrobot-cli dxf");
    assert!(out.status.success(), "dxf→nc 应成功：{}", String::from_utf8_lossy(&out.stderr));
    let g = std::fs::read_to_string(&nc).unwrap();
    assert!(g.contains("G0") || g.contains("G1"), "G-code 应为非空且含运动指令：\n{g}");

    // 2) DXF → SVG（写文件）
    let svg = dir.join("t.svg");
    let out = bin()
        .arg("dxf")
        .arg(&dxf)
        .arg("--svg")
        .arg(&svg)
        .output()
        .expect("应成功运行 wrobot-cli dxf --svg");
    assert!(out.status.success(), "dxf→svg 应成功：{}", String::from_utf8_lossy(&out.stderr));
    let s = std::fs::read_to_string(&svg).unwrap();
    assert!(s.contains("<svg") && s.contains("polyline"), "SVG 缺失：\n{s}");
}

#[test]
fn svgi_import_produces_gcode() {
    let dir = std::env::temp_dir().join("wrobot_cli_svg");
    let _ = std::fs::create_dir_all(&dir);
    let svg = dir.join("t.svg");
    std::fs::write(&svg, minimal_svg()).unwrap();

    let nc = dir.join("t.nc");
    let out = bin()
        .arg("svgi")
        .arg(&svg)
        .arg("-o")
        .arg(&nc)
        .output()
        .expect("应成功运行 wrobot-cli svgi");
    assert!(out.status.success(), "svgi→nc 应成功：{}", String::from_utf8_lossy(&out.stderr));
    let g = std::fs::read_to_string(&nc).unwrap();
    assert!(g.contains("G0") || g.contains("G1"), "SVG 导入应产出 G-code：\n{g}");
}

#[test]
fn vector_import_rejects_unknown_extension() {
    let dir = std::env::temp_dir().join("wrobot_cli_bad");
    let _ = std::fs::create_dir_all(&dir);
    let f = dir.join("t.txt");
    std::fs::write(&f, "not a vector").unwrap();
    let out = bin()
        .arg("dxf")
        .arg(&f)
        .output()
        .expect("应成功运行 wrobot-cli dxf");
    // .txt 不是 dxf/svg，import_dxf 解析应失败 → 退出码非 0
    assert!(!out.status.success(), "不支持的格式应失败");
}
