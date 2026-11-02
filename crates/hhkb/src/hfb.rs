use std::fmt;

use md5::{Digest, Md5};

pub const HEADER_SIZE: usize = 4;
pub const FILE_SIZE: usize = 0x45030;

pub(crate) const WHOLE_CRC_FIELD: usize = 0x0;
pub(crate) const REGION_A_CRC_FIELD: usize = 0x2;
pub(crate) const REGION_A: usize = HEADER_SIZE;
pub(crate) const PADDING: usize = 0x10004;
const MD5_FIELD: usize = 0x10010;
const REGION_B: usize = 0x10020;
const FOOTER: usize = 0x45020;
pub(crate) const VERSION_FIELD: usize = 0x45028;

const IDENTIFIER_SIZE: usize = 6;
const IDENTIFIER_TERMINATOR: [u8; 2] = *b"\n\0";
const KNOWN_MODELS: &[(&str, &str)] = &[("AHUX01", "observed in the stock HFB for PD-KB800BNS")];

const CRC16_ALGORITHM: &str = "CRC-16 (polynomial 0x1021, reflected, initial 0xffff)";

#[derive(Debug, PartialEq, Eq)]
pub enum HfbError {
    TooShort { actual: usize },
    UnexpectedSize { actual: usize },
}

impl fmt::Display for HfbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort { actual } => write!(
                f,
                "File is too short ({actual} bytes, header is {HEADER_SIZE} bytes)"
            ),
            Self::UnexpectedSize { actual } => write!(
                f,
                "Unexpected structure (0x{actual:x} bytes, expected 0x{FILE_SIZE:x} bytes)"
            ),
        }
    }
}

impl std::error::Error for HfbError {}

#[derive(Debug, PartialEq, Eq)]
pub struct Region {
    pub name: &'static str,
    pub offset: usize,
    pub size: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Check {
    pub name: &'static str,
    pub algorithm: &'static str,
    pub start: usize,
    pub end: usize,
    pub stored: String,
    pub computed: String,
}

impl Check {
    pub fn is_match(&self) -> bool {
        self.stored == self.computed
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Determination {
    Known(String),
    Undetermined,
}

impl Determination {
    fn is_known(&self) -> bool {
        matches!(self, Self::Known(_))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Report {
    pub file_size: usize,
    pub regions: Vec<Region>,
    pub checks: Vec<Check>,
    pub model: Determination,
    pub version: Determination,
}

impl Report {
    pub fn is_valid(&self) -> bool {
        self.checks.iter().all(Check::is_match) && self.model.is_known() && self.version.is_known()
    }
}

pub fn crc16(data: &[u8]) -> u16 {
    data.iter().fold(0xffff, |crc, byte| {
        (0..8).fold(crc ^ u16::from(*byte), |crc, _| {
            if crc & 1 == 1 {
                (crc >> 1) ^ 0x8408
            } else {
                crc >> 1
            }
        })
    })
}

pub fn inspect(data: &[u8]) -> Result<Report, HfbError> {
    if data.len() < HEADER_SIZE {
        return Err(HfbError::TooShort { actual: data.len() });
    }
    if data.len() != FILE_SIZE {
        return Err(HfbError::UnexpectedSize { actual: data.len() });
    }
    Ok(Report {
        file_size: data.len(),
        regions: regions(),
        checks: vec![
            crc_check(
                data,
                "Whole CRC",
                WHOLE_CRC_FIELD,
                REGION_A_CRC_FIELD,
                FILE_SIZE,
            ),
            crc_check(data, "Region A CRC", REGION_A_CRC_FIELD, REGION_A, PADDING),
            md5_check(data),
        ],
        model: read_model(data),
        version: read_version(data),
    })
}

fn regions() -> Vec<Region> {
    [
        ("Whole CRC", WHOLE_CRC_FIELD, REGION_A_CRC_FIELD),
        ("Region A CRC", REGION_A_CRC_FIELD, REGION_A),
        ("Region A", REGION_A, PADDING),
        ("Padding", PADDING, MD5_FIELD),
        ("Region B MD5", MD5_FIELD, REGION_B),
        ("Region B", REGION_B, FOOTER),
        ("Footer", FOOTER, FILE_SIZE),
    ]
    .into_iter()
    .map(|(name, start, end)| Region {
        name,
        offset: start,
        size: end - start,
    })
    .collect()
}

fn crc_check(data: &[u8], name: &'static str, field: usize, start: usize, end: usize) -> Check {
    let stored = u16::from_le_bytes([data[field], data[field + 1]]);
    Check {
        name,
        algorithm: CRC16_ALGORITHM,
        start,
        end,
        stored: format!("{stored:04x}"),
        computed: format!("{:04x}", crc16(&data[start..end])),
    }
}

fn md5_check(data: &[u8]) -> Check {
    Check {
        name: "Region B MD5",
        algorithm: "MD5",
        start: REGION_B,
        end: FOOTER,
        stored: to_hex(&data[MD5_FIELD..REGION_B]),
        computed: to_hex(&Md5::digest(&data[REGION_B..FOOTER])),
    }
}

pub(crate) fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn read_model(data: &[u8]) -> Determination {
    let (identifier, terminator) = data[FOOTER..VERSION_FIELD].split_at(IDENTIFIER_SIZE);
    if terminator != IDENTIFIER_TERMINATOR {
        return Determination::Undetermined;
    }
    KNOWN_MODELS
        .iter()
        .find(|(known, _)| known.as_bytes() == identifier)
        .map_or(Determination::Undetermined, |(known, note)| {
            Determination::Known(format!("{known} ({note})"))
        })
}

fn read_version(data: &[u8]) -> Determination {
    let (upper, lower) = (data[VERSION_FIELD], data[VERSION_FIELD + 1]);
    if upper > 9 || lower > 9 {
        return Determination::Undetermined;
    }
    Determination::Known(format!("A0.{upper}{lower}"))
}
