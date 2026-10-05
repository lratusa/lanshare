//! 文件名清理：迁移 v1 的全部用例，并补上双向控制符与更多 Windows 设备名。

use lanshare_proto::sanitize_filename as s;

#[test]
fn keeps_normal_names() {
    assert_eq!(s("照片 2026.jpg"), "照片 2026.jpg");
}

#[test]
fn drops_directories() {
    assert_eq!(s("../../etc/passwd"), "passwd");
    assert_eq!(s("C:\\Windows\\win.ini"), "win.ini");
}

#[test]
fn removes_illegal_characters() {
    assert_eq!(s("a<b>c:d\"e|f?g*h.txt"), "abcdefgh.txt");
    assert_eq!(s("tab\there\n.txt"), "tabhere.txt");
}

#[test]
fn strips_dots_and_spaces_at_both_ends() {
    assert_eq!(s("  report.pdf. . "), "report.pdf");
    assert_eq!(s(".hidden"), "hidden");
    assert_eq!(s(".lanshare-abc.part"), "lanshare-abc.part");
}

#[test]
fn empty_becomes_unnamed() {
    for raw in ["", "..", "...", "/", "  ", "<>"] {
        assert_eq!(s(raw), "unnamed", "{raw:?}");
    }
}

#[test]
fn prefixes_windows_reserved_names() {
    assert_eq!(s("CON"), "_CON");
    assert_eq!(s("nul.txt"), "_nul.txt");
    assert_eq!(s("com1.tar.gz"), "_com1.tar.gz");
    assert_eq!(s("console.txt"), "console.txt");
    assert_eq!(s("CONIN$"), "_CONIN$");
    assert_eq!(s("conout$.log"), "_conout$.log");
    assert_eq!(s("COM0"), "_COM0");
    assert_eq!(s("lpt0.txt"), "_lpt0.txt");
    assert_eq!(s("COM\u{00B9}"), "_COM\u{00B9}");
    assert_eq!(s("lpt\u{00B3}.txt"), "_lpt\u{00B3}.txt");
}

#[test]
fn removes_bidi_and_invisible_format_characters() {
    // “a\u{202E}gpj.exe” 在资源管理器里显示成 “aexe.jpg”，用来把程序伪装成图片
    assert_eq!(s("a\u{202E}gpj.exe"), "agpj.exe");
    for c in ['\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
              '\u{200E}', '\u{200F}', '\u{200B}', '\u{FEFF}'] {
        assert_eq!(s(&format!("re{c}port.pdf")), "report.pdf", "U+{:04X}", c as u32);
    }
}

#[test]
fn truncates_long_names_keeping_extension() {
    let result = s(&format!("{}.mp4", "中".repeat(150)));
    assert!(result.ends_with(".mp4"));
    assert!(result.starts_with('中'));
    assert!(result.len() <= 200, "按 UTF-8 字节不超过 200，实际 {}", result.len());
}

#[test]
fn absurd_extension_is_treated_as_part_of_name() {
    let result = s(&format!("a.{}", "x".repeat(300)));
    assert!(result.len() <= 200);
    assert!(result.starts_with("a."));
}
