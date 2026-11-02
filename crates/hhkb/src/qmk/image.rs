use std::fmt;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use sha2::{Digest, Sha256};

use super::build::Keymap;
use crate::flash::{HfbKind, VerifiedHfb};
use crate::hfb::{
    FILE_SIZE, PADDING, REGION_A, REGION_A_CRC_FIELD, WHOLE_CRC_FIELD, crc16, inspect, to_hex,
};

const APP_SIZE: usize = PADDING - REGION_A;
const FILL: u8 = 0xff;

#[derive(Debug, PartialEq, Eq)]
pub enum GenerateError {
    TooLarge { actual: usize },
    EmptyBinary,
    BinaryNotAllowed { sha256: String },
    TemplateNotStock,
    Write(String),
    OutsideLocal,
    Invalid,
}

impl fmt::Display for GenerateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { actual } => write!(
                f,
                ".bin が領域 A より大きいので生成しません（0x{actual:x} バイト、上限 0x{APP_SIZE:x} バイト）"
            ),
            Self::EmptyBinary => write!(f, ".bin が空なので生成しません"),
            Self::BinaryNotAllowed { sha256 } => {
                write!(
                    f,
                    "許可リストにない .bin なので生成しません（SHA-256: {sha256}）"
                )
            }
            Self::TemplateNotStock => {
                write!(f, "テンプレートが純正の HFB ではないので生成しません")
            }
            Self::Write(reason) => write!(f, "出力先に書き込めません: {reason}"),
            Self::OutsideLocal => write!(f, "出力先は local/ の中を指定してください"),
            Self::Invalid => write!(f, "生成した内容が検査に通らないので、書き出しません"),
        }
    }
}

impl std::error::Error for GenerateError {}

pub fn replace_app_region(template: &[u8], app: &[u8]) -> Result<Vec<u8>, GenerateError> {
    if template.len() != FILE_SIZE {
        return Err(GenerateError::Invalid);
    }
    if app.is_empty() {
        return Err(GenerateError::EmptyBinary);
    }
    if app.len() > APP_SIZE {
        return Err(GenerateError::TooLarge { actual: app.len() });
    }
    let mut output = template.to_vec();
    let region = &mut output[REGION_A..PADDING];
    region.fill(FILL);
    region[..app.len()].copy_from_slice(app);
    let region_crc = crc16(&output[REGION_A..PADDING]);
    output[REGION_A_CRC_FIELD..REGION_A].copy_from_slice(&region_crc.to_le_bytes());
    let whole_crc = crc16(&output[REGION_A_CRC_FIELD..]);
    output[WHOLE_CRC_FIELD..REGION_A_CRC_FIELD].copy_from_slice(&whole_crc.to_le_bytes());
    let report = inspect(&output).map_err(|_| GenerateError::Invalid)?;
    if !report.is_valid() {
        return Err(GenerateError::Invalid);
    }
    Ok(output)
}

fn check_inside_local(local: &Path, output: &Path) -> Result<(), GenerateError> {
    if output.file_name().is_none() {
        return Err(GenerateError::OutsideLocal);
    }
    let parent = match output.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let parent = parent
        .canonicalize()
        .map_err(|_| GenerateError::OutsideLocal)?;
    let local = local
        .canonicalize()
        .map_err(|_| GenerateError::OutsideLocal)?;
    if parent.starts_with(&local) {
        Ok(())
    } else {
        Err(GenerateError::OutsideLocal)
    }
}

fn generate_with(
    stock: &VerifiedHfb,
    bin: &[u8],
    allowed_bin_sha256: &str,
    local: &Path,
    output: &Path,
) -> Result<(), GenerateError> {
    if stock.kind() != HfbKind::Stock {
        return Err(GenerateError::TemplateNotStock);
    }
    let sha256 = to_hex(&Sha256::digest(bin));
    if sha256 != allowed_bin_sha256 {
        return Err(GenerateError::BinaryNotAllowed { sha256 });
    }
    check_inside_local(local, output)?;
    let image = replace_app_region(stock.bytes(), bin)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(output)
        .map_err(|error| GenerateError::Write(format!("{}: {error}", output.display())))?;
    file.write_all(&image)
        .map_err(|error| GenerateError::Write(format!("{}: {error}", output.display())))
}

pub fn generate(
    keymap: Keymap,
    stock: &VerifiedHfb,
    bin: &[u8],
    local: &Path,
    output: &Path,
) -> Result<(), GenerateError> {
    generate_with(stock, bin, keymap.expected_sha256(), local, output)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use sha2::{Digest, Sha256};

    use super::*;
    use crate::flash::testing::verified_synthetic;
    use crate::hfb::{FILE_SIZE, crc16, inspect, to_hex};

    const APP_START: usize = 4;
    const APP_END: usize = 0x10004;

    fn sha(data: &[u8]) -> String {
        to_hex(&Sha256::digest(data))
    }

    fn app() -> Vec<u8> {
        (0..1000u32).map(|i| (i % 253) as u8).collect()
    }

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("hhkb-qmk-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(path.join("local")).unwrap();
            Self(path)
        }

        fn local(&self) -> PathBuf {
            self.0.join("local")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn stock_bytes() -> Vec<u8> {
        crate::flash::testing::synthetic_bytes()
    }

    #[test]
    fn app_region_holds_the_binary_then_ff_padding() {
        let output = replace_app_region(&stock_bytes(), &app()).unwrap();
        assert_eq!(output[APP_START..APP_START + 1000], app()[..]);
        assert!(output[APP_START + 1000..APP_END].iter().all(|b| *b == 0xff));
    }

    #[test]
    fn everything_after_the_app_region_is_unchanged() {
        let stock = stock_bytes();
        let output = replace_app_region(&stock, &app()).unwrap();
        assert_eq!(output.len(), FILE_SIZE);
        assert_eq!(output[APP_END..], stock[APP_END..]);
    }

    #[test]
    fn both_crcs_are_recomputed_and_pass_inspection() {
        let output = replace_app_region(&stock_bytes(), &app()).unwrap();
        assert_eq!(
            output[2..4],
            crc16(&output[APP_START..APP_END]).to_le_bytes()
        );
        assert_eq!(output[0..2], crc16(&output[2..]).to_le_bytes());
        assert!(inspect(&output).unwrap().is_valid());
    }

    #[test]
    fn binary_filling_the_region_exactly_is_accepted() {
        assert!(replace_app_region(&stock_bytes(), &vec![0x5a; 0x10000]).is_ok());
    }

    #[test]
    fn binary_larger_than_the_region_is_rejected() {
        assert_eq!(
            replace_app_region(&stock_bytes(), &vec![0x5a; 0x10001]),
            Err(GenerateError::TooLarge { actual: 0x10001 })
        );
    }

    #[test]
    fn empty_binary_is_rejected() {
        assert_eq!(
            replace_app_region(&stock_bytes(), &[]),
            Err(GenerateError::EmptyBinary)
        );
    }

    #[test]
    fn output_is_created_with_owner_only_permissions() {
        let dir = TempDir::new("mode");
        let output = dir.local().join("out.hfb");
        generate_with(
            &verified_synthetic(),
            &app(),
            &sha(&app()),
            &dir.local(),
            &output,
        )
        .unwrap();
        let metadata = fs::metadata(&output).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        assert!(inspect(&fs::read(&output).unwrap()).unwrap().is_valid());
    }

    #[test]
    fn generated_hfb_as_a_template_creates_no_file() {
        let dir = TempDir::new("template-kind");
        let output = dir.local().join("out.hfb");
        let bytes = app();
        let sha256 = to_hex(&Sha256::digest(&bytes));
        for kind in [HfbKind::QmkDefault, HfbKind::QmkVia] {
            let result = generate_with(
                &crate::flash::testing::verified_synthetic_as(kind),
                &bytes,
                &sha256,
                &dir.local(),
                &output,
            );
            assert_eq!(result, Err(GenerateError::TemplateNotStock), "{kind:?}");
            assert!(!output.exists(), "{kind:?}");
        }
    }

    #[test]
    fn binary_outside_the_allowlist_creates_no_file() {
        let dir = TempDir::new("bin");
        let output = dir.local().join("out.hfb");
        let result = generate_with(&verified_synthetic(), &app(), "00", &dir.local(), &output);
        assert!(matches!(
            result,
            Err(GenerateError::BinaryNotAllowed { .. })
        ));
        assert!(!output.exists());
    }

    #[test]
    fn existing_output_is_left_untouched() {
        let dir = TempDir::new("exists");
        let output = dir.local().join("out.hfb");
        fs::write(&output, b"keep").unwrap();
        let result = generate_with(
            &verified_synthetic(),
            &app(),
            &sha(&app()),
            &dir.local(),
            &output,
        );
        assert!(matches!(result, Err(GenerateError::Write(_))));
        assert_eq!(fs::read(&output).unwrap(), b"keep");
    }

    #[test]
    fn output_outside_local_is_rejected() {
        let dir = TempDir::new("outside");
        let output = dir.0.join("out.hfb");
        let result = generate_with(
            &verified_synthetic(),
            &app(),
            &sha(&app()),
            &dir.local(),
            &output,
        );
        assert_eq!(result, Err(GenerateError::OutsideLocal));
        assert!(!output.exists());
    }

    #[test]
    fn dot_dot_cannot_escape_local() {
        let dir = TempDir::new("dotdot");
        let output = dir.local().join("../out.hfb");
        let result = generate_with(
            &verified_synthetic(),
            &app(),
            &sha(&app()),
            &dir.local(),
            &output,
        );
        assert_eq!(result, Err(GenerateError::OutsideLocal));
        assert!(!dir.0.join("out.hfb").exists());
    }

    #[test]
    fn public_generate_uses_the_keymap_allowlist() {
        let dir = TempDir::new("public");
        let output = dir.local().join("out.hfb");
        let result = generate(
            Keymap::Via,
            &verified_synthetic(),
            &app(),
            &dir.local(),
            &output,
        );
        assert!(matches!(
            result,
            Err(GenerateError::BinaryNotAllowed { .. })
        ));
    }
}
