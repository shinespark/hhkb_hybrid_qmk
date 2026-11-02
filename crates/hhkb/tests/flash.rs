use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use hhkb::hfb::{FILE_SIZE, crc16};
use md5::{Digest, Md5};

const STAGES: [&str; 3] = ["plan", "enter", "write"];

struct TempFile(PathBuf);

impl TempFile {
    fn new(name: &str, data: &[u8]) -> Self {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("flash-{name}-{}.hfb", std::process::id()));
        fs::write(&path, data).unwrap();
        Self(path)
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn valid_but_not_allowed() -> Vec<u8> {
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

fn flash(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_hhkb"))
        .arg("flash")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn every_stage_rejects_a_file_outside_the_allowlist_before_touching_the_device() {
    let file = TempFile::new("not-allowed", &valid_but_not_allowed());
    for stage in STAGES {
        let output = flash(&[stage, file.0.to_str().unwrap()]);
        assert!(!output.status.success(), "{stage}");
        assert!(text(&output.stderr).contains("許可リスト"), "{stage}");
        assert!(
            !text(&output.stdout).contains("入力してください"),
            "{stage}"
        );
    }
}

#[test]
fn every_stage_rejects_a_missing_file() {
    for stage in STAGES {
        let output = flash(&[stage, "/nonexistent/hhkb-test.hfb"]);
        assert!(!output.status.success(), "{stage}");
        assert!(text(&output.stderr).contains("読めません"), "{stage}");
    }
}

#[test]
fn no_stage_accepts_an_option_to_skip_checks() {
    let file = TempFile::new("options", &valid_but_not_allowed());
    for stage in STAGES {
        for option in ["--yes", "--force", "--skip-confirm", "--no-verify"] {
            let output = flash(&[stage, option, file.0.to_str().unwrap()]);
            assert_eq!(output.status.code(), Some(2), "{stage} {option}");
        }
        let help = flash(&[stage, "--help"]);
        assert!(help.status.success(), "{stage}");
        let options: Vec<String> = text(&help.stdout)
            .lines()
            .filter(|line| line.trim_start().starts_with('-'))
            .map(str::to_owned)
            .collect();
        assert_eq!(options.len(), 1, "{stage}: {options:?}");
        assert!(options[0].contains("--help"), "{stage}: {options:?}");
    }
}

#[test]
fn flash_help_lists_the_three_stages() {
    let help = flash(&["--help"]);
    let body = text(&help.stdout);
    for stage in STAGES {
        assert!(body.contains(stage), "{body}");
    }
}
