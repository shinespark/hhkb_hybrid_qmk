use std::fmt;

use sha2::{Digest, Sha256};

use crate::hfb::{
    Check, Determination, FILE_SIZE, HfbError, Report, VERSION_FIELD, inspect, to_hex,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HfbKind {
    Stock,
    QmkDefault,
    QmkVia,
}

impl HfbKind {
    pub const fn is_qmk(self) -> bool {
        matches!(self, Self::QmkDefault | Self::QmkVia)
    }
}

impl fmt::Display for HfbKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Stock => "公式ファームウェア",
            Self::QmkDefault => "QMK の default",
            Self::QmkVia => "QMK の via",
        })
    }
}

const ALLOWED: &[(&str, HfbKind)] = &[
    (
        "635b995eb5a15aa50c2cedc60da7d8998fcca8285a09d41d984c07fffcaf9d43",
        HfbKind::Stock,
    ),
    (
        "abc86297c4f0d4d967fe9e1bfa8b5491b4a99873afb58e57d93d2f3ebf1cdcf4",
        HfbKind::QmkDefault,
    ),
    (
        "1393125de1bd6b24c64b424b1398e1fe1aa1e104f26609fc045130a2042e223e",
        HfbKind::QmkVia,
    ),
];

const BODY_OFFSET: usize = 2;
const CHUNK_SIZE: usize = 57;

#[derive(Debug, PartialEq, Eq)]
pub enum VerifyError {
    Structure(HfbError),
    NotAllowed { sha256: String },
    Integrity { failed: Vec<&'static str> },
    Undetermined,
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Structure(error) => write!(f, "{error}"),
            Self::NotAllowed { sha256 } => write!(
                f,
                "許可リストにない HFB です。書き込みません（SHA-256: {sha256}）"
            ),
            Self::Integrity { failed } => write!(
                f,
                "整合性の検査に失敗しました。書き込みません（{}）",
                failed.join("、")
            ),
            Self::Undetermined => write!(f, "機種か版を確定できません。書き込みません"),
        }
    }
}

impl std::error::Error for VerifyError {}

#[derive(Debug)]
pub struct VerifiedHfb {
    data: Vec<u8>,
    report: Report,
    sha256: String,
    version: String,
    kind: HfbKind,
}

impl VerifiedHfb {
    pub fn verify(data: Vec<u8>) -> Result<Self, VerifyError> {
        Self::verify_against(data, ALLOWED)
    }

    pub(super) fn verify_against(
        data: Vec<u8>,
        allowed: &[(&str, HfbKind)],
    ) -> Result<Self, VerifyError> {
        let sha256 = to_hex(&Sha256::digest(&data));
        let Some((_, kind)) = allowed.iter().find(|(hash, _)| *hash == sha256) else {
            return Err(VerifyError::NotAllowed { sha256 });
        };
        let kind = *kind;
        let report = inspect(&data).map_err(VerifyError::Structure)?;
        let failed: Vec<&'static str> = report
            .checks
            .iter()
            .filter(|check| !check.is_match())
            .map(|check: &Check| check.name)
            .collect();
        if !failed.is_empty() {
            return Err(VerifyError::Integrity { failed });
        }
        if !report.is_valid() {
            return Err(VerifyError::Undetermined);
        }
        let Determination::Known(version) = &report.version else {
            return Err(VerifyError::Undetermined);
        };
        let version = version.clone();
        Ok(Self {
            data,
            report,
            sha256,
            version,
            kind,
        })
    }

    pub fn report(&self) -> &Report {
        &self.report
    }

    pub fn kind(&self) -> HfbKind {
        self.kind
    }

    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    pub fn size(&self) -> usize {
        self.data.len()
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn packet_count(&self) -> usize {
        (FILE_SIZE - BODY_OFFSET).div_ceil(CHUNK_SIZE)
    }

    pub(super) fn version_bytes(&self) -> [u8; 2] {
        [self.data[VERSION_FIELD], self.data[VERSION_FIELD + 1]]
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.data
    }

    pub(super) fn stored_crc(&self) -> [u8; 2] {
        [self.data[0], self.data[1]]
    }

    pub(super) fn chunk(&self, packet: u16) -> &[u8] {
        let start = BODY_OFFSET + usize::from(packet) * CHUNK_SIZE;
        let end = (start + CHUNK_SIZE).min(self.data.len());
        &self.data[start..end]
    }
}

#[cfg(test)]
pub(super) mod testing {
    use md5::{Digest, Md5};
    use sha2::Sha256;

    use crate::hfb::{FILE_SIZE, crc16, to_hex};

    const PADDING: usize = 0x10004;
    const MD5_FIELD: usize = 0x10010;
    const REGION_B: usize = 0x10020;
    const FOOTER: usize = 0x45020;
    const VERSION: usize = 0x45028;

    pub(in crate::flash) fn seal(data: &mut [u8]) {
        let digest = Md5::digest(&data[REGION_B..FOOTER]);
        data[MD5_FIELD..REGION_B].copy_from_slice(&digest);
        let region_a = crc16(&data[4..PADDING]);
        data[2..4].copy_from_slice(&region_a.to_le_bytes());
        let whole = crc16(&data[2..]);
        data[0..2].copy_from_slice(&whole.to_le_bytes());
    }

    pub(in crate::flash) fn synthetic() -> Vec<u8> {
        let mut data = vec![0xffu8; FILE_SIZE];
        for (i, byte) in data[4..PADDING].iter_mut().enumerate() {
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

    pub(in crate::flash) fn sha256_hex(data: &[u8]) -> String {
        to_hex(&Sha256::digest(data))
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{seal, sha256_hex, synthetic};
    use super::*;
    use crate::hfb::FILE_SIZE;

    fn verify_allowing_itself(data: Vec<u8>) -> Result<VerifiedHfb, VerifyError> {
        let sha256 = sha256_hex(&data);
        VerifiedHfb::verify_against(data, &[(sha256.as_str(), HfbKind::Stock)])
    }

    #[test]
    fn allowed_intact_file_is_accepted() {
        assert!(verify_allowing_itself(synthetic()).is_ok());
    }

    #[test]
    fn file_outside_the_allowlist_is_rejected() {
        let data = synthetic();
        let sha256 = sha256_hex(&data);
        let result = VerifiedHfb::verify_against(data, &[("00", HfbKind::Stock)]);
        assert_eq!(result.unwrap_err(), VerifyError::NotAllowed { sha256 });
    }

    #[test]
    fn crc_mismatch_is_rejected_even_when_the_hash_is_allowed() {
        let mut data = synthetic();
        data[0] ^= 0x01;
        assert_eq!(
            verify_allowing_itself(data).unwrap_err(),
            VerifyError::Integrity {
                failed: vec!["全体 CRC"]
            }
        );
    }

    #[test]
    fn md5_mismatch_is_rejected_even_when_the_hash_is_allowed() {
        let mut data = synthetic();
        data[0x10010] ^= 0x01;
        let whole = crate::hfb::crc16(&data[2..]);
        data[0..2].copy_from_slice(&whole.to_le_bytes());
        assert_eq!(
            verify_allowing_itself(data).unwrap_err(),
            VerifyError::Integrity {
                failed: vec!["領域 B MD5"]
            }
        );
    }

    #[test]
    fn unknown_model_is_rejected_even_when_the_hash_is_allowed() {
        let mut data = synthetic();
        data[0x45020..0x45026].copy_from_slice(b"ZZZZ99");
        seal(&mut data);
        assert_eq!(
            verify_allowing_itself(data).unwrap_err(),
            VerifyError::Undetermined
        );
    }

    #[test]
    fn unexpected_size_is_rejected_even_when_the_hash_is_allowed() {
        let mut data = synthetic();
        data.pop();
        assert_eq!(
            verify_allowing_itself(data).unwrap_err(),
            VerifyError::Structure(HfbError::UnexpectedSize {
                actual: FILE_SIZE - 1
            })
        );
    }

    #[test]
    fn the_shipped_allowlist_holds_exactly_the_three_hashes_with_their_kinds() {
        assert_eq!(
            super::ALLOWED,
            [
                (
                    "635b995eb5a15aa50c2cedc60da7d8998fcca8285a09d41d984c07fffcaf9d43",
                    HfbKind::Stock
                ),
                (
                    "abc86297c4f0d4d967fe9e1bfa8b5491b4a99873afb58e57d93d2f3ebf1cdcf4",
                    HfbKind::QmkDefault
                ),
                (
                    "1393125de1bd6b24c64b424b1398e1fe1aa1e104f26609fc045130a2042e223e",
                    HfbKind::QmkVia
                ),
            ]
        );
    }

    #[test]
    fn the_kind_comes_from_the_matching_entry() {
        let data = synthetic();
        let sha256 = sha256_hex(&data);
        let hfb = VerifiedHfb::verify_against(
            data,
            &[("00", HfbKind::Stock), (sha256.as_str(), HfbKind::QmkVia)],
        )
        .unwrap();
        assert_eq!(hfb.kind(), HfbKind::QmkVia);
    }

    #[test]
    #[ignore = "local/ の 3 つの実物の HFB が必要"]
    fn the_real_files_have_the_expected_kinds() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../local");
        for (name, kind) in [
            ("HHKB800_FW_A048.hfb", HfbKind::Stock),
            ("hhkb_hybrid_default.hfb", HfbKind::QmkDefault),
            ("hhkb_hybrid_via.hfb", HfbKind::QmkVia),
        ] {
            let data = std::fs::read(format!("{root}/{name}")).unwrap();
            let hfb = VerifiedHfb::verify(data).unwrap();
            assert_eq!(hfb.kind(), kind, "{name}");
            assert_eq!(hfb.version(), "A0.48", "{name}");
        }
    }

    #[test]
    fn the_public_constructor_rejects_a_file_outside_the_shipped_allowlist() {
        assert!(matches!(
            VerifiedHfb::verify(synthetic()),
            Err(VerifyError::NotAllowed { .. })
        ));
    }

    #[test]
    fn verified_file_exposes_its_size_version_and_packet_count() {
        let hfb = verify_allowing_itself(synthetic()).unwrap();
        assert_eq!(hfb.size(), 282_672);
        assert_eq!(hfb.version(), "A0.48");
        assert_eq!(hfb.packet_count(), 4960);
    }
}
