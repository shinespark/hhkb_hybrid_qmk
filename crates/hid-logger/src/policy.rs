pub const VENDOR_USAGE_PAGE: i64 = 0xff00;

const FRAME_HEADER: [u8; 2] = [0xaa, 0xaa];
const READ_COMMANDS: &[u8] = &[0x01, 0x02, 0x05, 0x06, 0x87, 0x09];
const KEYMAP_WRITE_COMMANDS: &[u8] = &[0x86, 0x08, 0x04, 0x07];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Probe,
    Read,
    KeymapWrite,
    FirmwareUpdate,
}

impl Mode {
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("read") => Self::Read,
            Some("keymap-write") => Self::KeymapWrite,
            Some("firmware-update") => Self::FirmwareUpdate,
            _ => Self::Probe,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Probe => "probe",
            Self::Read => "read",
            Self::KeymapWrite => "keymap-write",
            Self::FirmwareUpdate => "firmware-update",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Forward,
    Block,
}

pub fn decide(
    mode: Mode,
    command_offset: Option<usize>,
    usage_page: Option<i64>,
    report: &[u8],
) -> Decision {
    if usage_page != Some(VENDOR_USAGE_PAGE) {
        return Decision::Block;
    }
    let allowed = match mode {
        Mode::Probe => false,
        Mode::FirmwareUpdate => true,
        Mode::Read | Mode::KeymapWrite if !report.starts_with(&FRAME_HEADER) => false,
        Mode::Read | Mode::KeymapWrite => {
            let command = command_offset.and_then(|offset| report.get(offset));
            command.is_some_and(|c| {
                READ_COMMANDS.contains(c)
                    || (mode == Mode::KeymapWrite && KEYMAP_WRITE_COMMANDS.contains(c))
            })
        }
    };
    if allowed {
        Decision::Forward
    } else {
        Decision::Block
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: Option<i64> = Some(VENDOR_USAGE_PAGE);

    #[test]
    fn unknown_mode_falls_back_to_probe() {
        assert_eq!(Mode::parse(None), Mode::Probe);
        assert_eq!(Mode::parse(Some("typo")), Mode::Probe);
    }

    #[test]
    fn probe_blocks_everything() {
        assert_eq!(decide(Mode::Probe, Some(0), PAGE, &[0x01]), Decision::Block);
    }

    #[test]
    fn read_allows_only_read_commands() {
        for command in [0x01, 0x02, 0x05, 0x06, 0x87, 0x09] {
            assert_eq!(
                decide(Mode::Read, Some(2), PAGE, &[0xaa, 0xaa, command]),
                Decision::Forward
            );
        }
        for command in [0x86, 0x08, 0x04, 0x07, 0xe0, 0xe1, 0xe2, 0xe3, 0x00, 0xff] {
            assert_eq!(
                decide(Mode::Read, Some(2), PAGE, &[0xaa, 0xaa, command]),
                Decision::Block
            );
        }
    }

    #[test]
    fn missing_header_is_blocked() {
        assert_eq!(
            decide(Mode::Read, Some(2), PAGE, &[0xaa, 0xab, 0x02]),
            Decision::Block
        );
        assert_eq!(
            decide(Mode::KeymapWrite, Some(0), PAGE, &[0x02]),
            Decision::Block
        );
    }

    #[test]
    fn keymap_write_adds_only_keymap_commands() {
        for command in [0x02, 0x86, 0x08, 0x04, 0x07] {
            assert_eq!(
                decide(Mode::KeymapWrite, Some(2), PAGE, &[0xaa, 0xaa, command]),
                Decision::Forward
            );
        }
        for command in [0xe0, 0xe1, 0xe2, 0xe3] {
            assert_eq!(
                decide(Mode::KeymapWrite, Some(2), PAGE, &[0xaa, 0xaa, command]),
                Decision::Block
            );
        }
    }

    #[test]
    fn unknown_command_position_is_blocked() {
        let report = [0xaa, 0xaa, 0x02];
        assert_eq!(decide(Mode::Read, None, PAGE, &report), Decision::Block);
        assert_eq!(decide(Mode::Read, Some(5), PAGE, &report), Decision::Block);
    }

    #[test]
    fn other_interfaces_are_blocked_in_every_mode() {
        for mode in [
            Mode::Probe,
            Mode::Read,
            Mode::KeymapWrite,
            Mode::FirmwareUpdate,
        ] {
            assert_eq!(decide(mode, Some(0), Some(1), &[0x02]), Decision::Block);
            assert_eq!(decide(mode, Some(0), None, &[0x02]), Decision::Block);
        }
    }

    #[test]
    fn firmware_update_allows_vendor_interface() {
        assert_eq!(
            decide(Mode::FirmwareUpdate, None, PAGE, &[0xe0]),
            Decision::Forward
        );
    }
}
