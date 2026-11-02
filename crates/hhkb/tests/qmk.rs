use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use hhkb::hfb::{FILE_SIZE, crc16};
use hhkb::qmk::replace_app_region;
use md5::{Digest, Md5};

struct TempFile(PathBuf);

impl TempFile {
    fn new(name: &str, data: &[u8]) -> Self {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("qmk-{name}-{}.hfb", std::process::id()));
        fs::write(&path, data).unwrap();
        Self(path)
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn template() -> Vec<u8> {
    let mut data = vec![0xffu8; FILE_SIZE];
    data[0x45020..0x45028].copy_from_slice(b"AHUX01\n\0");
    data[0x45028..0x4502a].copy_from_slice(&[0x04, 0x08]);
    let digest = Md5::digest(&data[0x10020..0x45020]);
    data[0x10010..0x10020].copy_from_slice(&digest);
    let region_a = crc16(&data[4..0x10004]);
    data[2..4].copy_from_slice(&region_a.to_le_bytes());
    let whole = crc16(&data[2..]);
    data[0..2].copy_from_slice(&whole.to_le_bytes());
    data
}

fn hhkb(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_hhkb"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn option_lines(help: &Output) -> Vec<String> {
    text(&help.stdout)
        .lines()
        .filter(|line| line.trim_start().starts_with('-'))
        .map(str::to_owned)
        .collect()
}

#[test]
fn generated_image_is_rejected_by_every_flash_stage() {
    let generated = replace_app_region(&template(), &[0x5a; 1000]).unwrap();
    let file = TempFile::new("generated", &generated);
    let inspected = hhkb(&["inspect", file.0.to_str().unwrap()]);
    assert!(inspected.status.success());
    for stage in ["plan", "prepare", "write"] {
        let output = hhkb(&["flash", stage, file.0.to_str().unwrap()]);
        assert!(!output.status.success(), "{stage}");
        assert!(text(&output.stderr).contains("allowlist"), "{stage}");
    }
}

#[test]
fn build_rejects_other_keymaps() {
    let output = hhkb(&["qmk", "build", "custom"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn build_accepts_no_compile_options() {
    let output = hhkb(&["qmk", "build", "via", "-e", "HHKB_LOW_POWER=no"]);
    assert_eq!(output.status.code(), Some(2));
    let help = hhkb(&["qmk", "build", "--help"]);
    let lines = option_lines(&help);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines.iter().any(|line| line.contains("--platform")),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("--help")),
        "{lines:?}"
    );
}

#[test]
fn hfb_rejects_a_template_outside_the_allowlist_without_creating_output() {
    let stock = TempFile::new("stock", &template());
    let bin = TempFile::new("bin", &[0x5a; 1000]);
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("qmk-out-{}.hfb", std::process::id()));
    let output = hhkb(&[
        "qmk",
        "hfb",
        "via",
        "--bin",
        bin.0.to_str().unwrap(),
        "--stock",
        stock.0.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    assert!(text(&output.stderr).contains("allowlist"));
    assert!(!out.exists());
}

#[test]
fn hfb_has_no_option_to_skip_checks() {
    let help = hhkb(&["qmk", "hfb", "--help"]);
    let lines = option_lines(&help);
    assert_eq!(lines.len(), 4, "{lines:?}");
    for name in ["--bin", "--stock", "--out", "--help"] {
        assert!(lines.iter().any(|line| line.contains(name)), "{name}");
    }
}
