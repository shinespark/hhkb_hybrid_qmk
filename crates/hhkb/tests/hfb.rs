use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use hhkb::hfb::{Determination, FILE_SIZE, HEADER_SIZE, HfbError, crc16, inspect};
use md5::{Digest, Md5};
use sha2::Sha256;

const REGION_A: usize = 0x4;
const PADDING: usize = 0x10004;
const MD5_FIELD: usize = 0x10010;
const REGION_B: usize = 0x10020;
const FOOTER: usize = 0x45020;
const VERSION: usize = 0x45028;

fn seal(data: &mut [u8]) {
    let digest = Md5::digest(&data[REGION_B..FOOTER]);
    data[MD5_FIELD..REGION_B].copy_from_slice(&digest);
    let region_a = crc16(&data[REGION_A..PADDING]);
    data[2..4].copy_from_slice(&region_a.to_le_bytes());
    let whole = crc16(&data[2..]);
    data[0..2].copy_from_slice(&whole.to_le_bytes());
}

fn synthetic() -> Vec<u8> {
    let mut data = vec![0xffu8; FILE_SIZE];
    for (i, byte) in data[REGION_A..PADDING].iter_mut().enumerate() {
        *byte = (i % 251) as u8;
    }
    for (i, byte) in data[REGION_B..FOOTER].iter_mut().enumerate() {
        *byte = (i % 241) as u8;
    }
    data[FOOTER..VERSION].copy_from_slice(b"AHUX01\n\0");
    data[VERSION..VERSION + 2].copy_from_slice(&[0x04, 0x08]);
    seal(&mut data);
    data
}

fn mismatched(data: &[u8]) -> Vec<&'static str> {
    inspect(data)
        .unwrap()
        .checks
        .iter()
        .filter(|check| !check.is_match())
        .map(|check| check.name)
        .collect()
}

#[test]
fn crc16_matches_the_known_check_value() {
    assert_eq!(crc16(b"123456789"), 0x6f91);
}

#[test]
fn region_boundaries_match_the_observed_layout() {
    let report = inspect(&synthetic()).unwrap();
    assert_eq!(report.file_size, FILE_SIZE);
    let layout: Vec<(usize, usize)> = report
        .regions
        .iter()
        .map(|region| (region.offset, region.size))
        .collect();
    assert_eq!(
        layout,
        [
            (0x0, 0x2),
            (0x2, 0x2),
            (0x4, 0x10000),
            (0x10004, 0xc),
            (0x10010, 0x10),
            (0x10020, 0x35000),
            (0x45020, 0x10),
        ]
    );
}

#[test]
fn intact_file_is_valid() {
    let report = inspect(&synthetic()).unwrap();
    assert!(report.checks.iter().all(|check| check.is_match()));
    assert_eq!(report.checks.len(), 3);
    assert!(report.is_valid());
}

#[test]
fn input_shorter_than_header_is_rejected() {
    assert_eq!(
        inspect(&[0u8; HEADER_SIZE - 1]),
        Err(HfbError::TooShort {
            actual: HEADER_SIZE - 1
        })
    );
}

#[test]
fn input_with_unexpected_size_is_rejected() {
    let mut data = synthetic();
    data.pop();
    assert_eq!(
        inspect(&data),
        Err(HfbError::UnexpectedSize {
            actual: FILE_SIZE - 1
        })
    );
}

#[test]
fn change_in_region_a_breaks_whole_and_region_a_crc() {
    let mut data = synthetic();
    data[0x100] ^= 0x01;
    assert_eq!(mismatched(&data), ["全体 CRC", "領域 A CRC"]);
}

#[test]
fn change_in_padding_breaks_only_whole_crc() {
    let mut data = synthetic();
    data[PADDING] ^= 0x01;
    assert_eq!(mismatched(&data), ["全体 CRC"]);
}

#[test]
fn change_in_region_b_breaks_whole_crc_and_md5() {
    let mut data = synthetic();
    data[0x20000] ^= 0x01;
    assert_eq!(mismatched(&data), ["全体 CRC", "領域 B MD5"]);
}

#[test]
fn change_in_stored_region_a_crc_breaks_both_crcs() {
    let mut data = synthetic();
    data[2] ^= 0x01;
    assert_eq!(mismatched(&data), ["全体 CRC", "領域 A CRC"]);
}

#[test]
fn change_in_stored_whole_crc_breaks_only_whole_crc() {
    let mut data = synthetic();
    data[0] ^= 0x01;
    assert_eq!(mismatched(&data), ["全体 CRC"]);
}

#[test]
fn model_and_version_are_read_from_the_footer() {
    let report = inspect(&synthetic()).unwrap();
    assert_eq!(report.version, Determination::Known("A0.48".to_owned()));
    let Determination::Known(model) = report.model else {
        panic!("機種を読み取れませんでした");
    };
    assert!(model.contains("AHUX01"));
}

#[test]
fn unknown_identifier_leaves_model_undetermined() {
    let mut data = synthetic();
    data[FOOTER..FOOTER + 6].copy_from_slice(b"ZZZZ99");
    seal(&mut data);
    let report = inspect(&data).unwrap();
    assert_eq!(report.model, Determination::Undetermined);
    assert!(report.checks.iter().all(|check| check.is_match()));
    assert!(!report.is_valid());
}

#[test]
fn malformed_identifier_leaves_model_undetermined() {
    let mut data = synthetic();
    data[FOOTER + 6] = 0xff;
    seal(&mut data);
    let report = inspect(&data).unwrap();
    assert_eq!(report.model, Determination::Undetermined);
    assert!(!report.is_valid());
}

#[test]
fn non_decimal_version_is_undetermined() {
    let mut data = synthetic();
    data[VERSION] = 0xff;
    seal(&mut data);
    let report = inspect(&data).unwrap();
    assert_eq!(report.version, Determination::Undetermined);
    assert!(!report.is_valid());
}

struct TempFile(PathBuf);

impl TempFile {
    fn new(name: &str, data: &[u8]) -> Self {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{name}-{}.hfb", std::process::id()));
        fs::write(&path, data).unwrap();
        Self(path)
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn run_inspect(file: &TempFile) -> Output {
    Command::new(env!("CARGO_BIN_EXE_hhkb"))
        .arg("inspect")
        .arg(&file.0)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn cli_reports_success_for_intact_file() {
    let file = TempFile::new("intact", &synthetic());
    let output = run_inspect(&file);
    let text = stdout(&output);
    assert!(output.status.success(), "{text}");
    assert!(text.contains("0x10020"), "{text}");
    assert!(text.contains("A0.48"), "{text}");
    assert!(text.contains("AHUX01"), "{text}");
    assert!(!text.contains("不一致"), "{text}");
}

#[test]
fn cli_reports_mismatch_with_failure_exit_code() {
    let mut data = synthetic();
    data[0x100] ^= 0x01;
    let file = TempFile::new("altered", &data);
    let output = run_inspect(&file);
    assert!(!output.status.success());
    assert!(stdout(&output).contains("不一致"));
}

#[test]
fn cli_reports_undetermined_with_failure_exit_code() {
    let mut data = synthetic();
    data[VERSION] = 0xff;
    data[FOOTER..FOOTER + 6].copy_from_slice(b"ZZZZ99");
    seal(&mut data);
    let file = TempFile::new("undetermined", &data);
    let output = run_inspect(&file);
    let text = stdout(&output);
    assert!(!output.status.success(), "{text}");
    assert_eq!(text.matches("未確定").count(), 2, "{text}");
}

#[test]
fn cli_rejects_short_file_with_reason() {
    let file = TempFile::new("short", &[0u8; 2]);
    let output = run_inspect(&file);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("短すぎます"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn cli_rejects_missing_file_with_reason() {
    let output = Command::new(env!("CARGO_BIN_EXE_hhkb"))
        .args(["inspect", "/nonexistent/hhkb-test.hfb"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!stderr(&output).is_empty());
}

#[test]
fn inspection_leaves_input_hash_unchanged() {
    for (name, data) in [("hash-intact", synthetic()), ("hash-short", vec![0u8; 2])] {
        let file = TempFile::new(name, &data);
        let before = Sha256::digest(fs::read(&file.0).unwrap());
        let modified_before = fs::metadata(&file.0).unwrap().modified().unwrap();
        run_inspect(&file);
        let after = Sha256::digest(fs::read(&file.0).unwrap());
        let modified_after = fs::metadata(&file.0).unwrap().modified().unwrap();
        assert_eq!(before, after, "{name}");
        assert_eq!(modified_before, modified_after, "{name}");
    }
}
