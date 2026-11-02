use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    PrepareNotSent,
    QueryOnly,
    ModeChangeAttempted,
    QmkPrepareNotSent,
    QmkModeChangeAttempted {
        command: u8,
    },
    QmkRevertRefused,
    WriteNotSent,
    TransferAttempted {
        last_acked: Option<u16>,
        total: usize,
    },
    EndAttempted,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Abort {
    pub stage: Stage,
    pub reason: String,
}

impl fmt::Display for Abort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.reason)
    }
}

impl std::error::Error for Abort {}

const QMK_BOOTMAGIC: &str = "If QMK does not respond, references say that holding Esc while connecting USB enters update mode. This procedure is unverified";

const CHECK_BY_KEY_INPUT: &str = "The HHKB state cannot be determined. Check it by key input (the USB listing cannot tell the states apart)";

impl Abort {
    pub fn guidance(&self) -> String {
        match self.stage {
            Stage::PrepareNotSent => {
                "Stage reached: before the mode change. The report was not sent. Fix the cause, then rerun hhkb flash prepare"
                    .to_owned()
            }
            Stage::QueryOnly => {
                "Stage reached: version check. Only 02 was sent; the mode-change report (e0) was not sent. Check the state with key input and hhkb status before deciding"
                    .to_owned()
            }
            Stage::ModeChangeAttempted => format!(
                "Stage reached: after sending the mode-change report (e0). {CHECK_BY_KEY_INPUT}\n\
                 - Can type: it may be running stock firmware. Decide whether to rerun hhkb flash prepare\n\
                 - Cannot type: it may be in update mode. Replug USB, check with hhkb list, and decide whether to run hhkb flash write"
            ),
            Stage::QmkPrepareNotSent => format!(
                "Stage reached: before the QMK mode change. The report was not sent. Fix the cause, then rerun hhkb flash prepare\n{QMK_BOOTMAGIC}"
            ),
            Stage::QmkModeChangeAttempted { command } => format!(
                "Stage reached: after sending the mode-change report ({command:02x}) to QMK. {CHECK_BY_KEY_INPUT}\n\
                 - Can type: it may be running QMK. Check with hhkb list and decide whether to rerun hhkb flash prepare\n\
                 - Cannot type: it may be in update mode. Replug USB, check with hhkb list, and decide whether to run hhkb flash write\n\
                 {QMK_BOOTMAGIC}"
            ),
            Stage::QmkRevertRefused => format!(
                "Stage reached: after sending f0 to QMK. f0 was refused and e0 was not sent. The mode change was stopped so that stock firmware is not restored with VIA settings left behind. {CHECK_BY_KEY_INPUT}\n\
                 Rerunning hhkb flash prepare will likely give the same result, so decide what to do instead of repeating it\n\
                 {QMK_BOOTMAGIC}"
            ),
            Stage::WriteNotSent => {
                "Stage reached: before the write. The report was not sent. Fix the cause, confirm that the HHKB cannot type, then rerun hhkb flash write"
                    .to_owned()
            }
            Stage::TransferAttempted { last_acked, total } => {
                let progress = last_acked.map_or_else(
                    || format!("No packet was acknowledged ({total} in total)"),
                    |packet| format!("The last acknowledged packet is {packet} (numbered from 0, {total} in total)"),
                );
                format!(
                    "Stage reached: firmware transfer. {progress}. {CHECK_BY_KEY_INPUT}\n\
                     - Cannot type: replug USB, confirm that there is no Bluetooth connection, and decide whether to rerun hhkb flash write once. Resuming midway is unverified\n\
                     - Can type: do not run hhkb flash write; check with hhkb status"
                )
            }
            Stage::EndAttempted => format!(
                "Stage reached: after sending the end report (e3). All firmware packets were acknowledged. {CHECK_BY_KEY_INPUT}\n\
                 - Can type: check the version and settings with hhkb status and a read in the official tool\n\
                 - Cannot type: replug USB, check again, and decide whether to rerun hhkb flash write"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guidance(stage: Stage) -> String {
        Abort {
            stage,
            reason: String::new(),
        }
        .guidance()
    }

    const QMK_STAGES: [Stage; 3] = [
        Stage::QmkPrepareNotSent,
        Stage::QmkModeChangeAttempted { command: 0xe0 },
        Stage::QmkRevertRefused,
    ];

    const ALL_STAGES: [Stage; 9] = [
        Stage::PrepareNotSent,
        Stage::QueryOnly,
        Stage::ModeChangeAttempted,
        Stage::QmkPrepareNotSent,
        Stage::QmkModeChangeAttempted { command: 0xf0 },
        Stage::QmkRevertRefused,
        Stage::WriteNotSent,
        Stage::TransferAttempted {
            last_acked: Some(10),
            total: 4960,
        },
        Stage::EndAttempted,
    ];

    #[test]
    fn every_qmk_stage_shows_the_esc_procedure_as_unverified() {
        for stage in QMK_STAGES {
            let text = guidance(stage);
            assert!(text.contains("holding Esc"), "{stage:?}: {text}");
            assert!(text.contains("unverified"), "{stage:?}: {text}");
        }
    }

    #[test]
    fn stock_stages_do_not_mention_the_qmk_procedure() {
        for stage in [
            Stage::PrepareNotSent,
            Stage::QueryOnly,
            Stage::ModeChangeAttempted,
        ] {
            assert!(!guidance(stage).contains("holding Esc"), "{stage:?}");
        }
    }

    #[test]
    fn qmk_mode_change_stage_names_the_command() {
        let text = guidance(Stage::QmkModeChangeAttempted { command: 0xf0 });
        assert!(text.contains("f0"), "{text}");
    }

    #[test]
    fn revert_refused_stage_says_e0_was_not_sent() {
        let text = guidance(Stage::QmkRevertRefused);
        assert!(text.contains("e0 was not sent"), "{text}");
    }

    #[test]
    fn nothing_sent_before_mode_change_says_so() {
        assert!(guidance(Stage::PrepareNotSent).contains("was not sent"));
    }

    #[test]
    fn version_query_stage_says_the_mode_change_was_not_sent() {
        let text = guidance(Stage::QueryOnly);
        assert!(text.contains("e0"), "{text}");
        assert!(text.contains("was not sent"), "{text}");
    }

    #[test]
    fn mode_change_stage_explains_both_key_input_outcomes() {
        let text = guidance(Stage::ModeChangeAttempted);
        assert!(text.contains("key input"), "{text}");
        assert!(text.contains("hhkb flash prepare"), "{text}");
        assert!(text.contains("hhkb flash write"), "{text}");
    }

    #[test]
    fn write_not_sent_stage_says_nothing_was_sent() {
        let text = guidance(Stage::WriteNotSent);
        assert!(text.contains("was not sent"), "{text}");
    }

    #[test]
    fn transfer_stage_names_the_last_acknowledged_packet() {
        let text = guidance(Stage::TransferAttempted {
            last_acked: Some(10),
            total: 4960,
        });
        assert!(text.contains("10"), "{text}");
        assert!(text.contains("4960"), "{text}");
        assert!(text.contains("hhkb flash write"), "{text}");
    }

    #[test]
    fn transfer_stage_without_an_acknowledged_packet_says_none() {
        let text = guidance(Stage::TransferAttempted {
            last_acked: None,
            total: 4960,
        });
        assert!(text.contains("No packet was acknowledged"), "{text}");
    }

    #[test]
    fn end_stage_asks_to_check_before_retrying() {
        let text = guidance(Stage::EndAttempted);
        assert!(text.contains("e3"), "{text}");
        assert!(text.contains("key input"), "{text}");
    }

    #[test]
    fn stages_after_sending_never_assert_the_device_state() {
        for stage in ALL_STAGES {
            let text = guidance(stage);
            for assertion in [
                "is in update mode",
                "is still in update mode",
                "is running stock firmware",
                "broken",
            ] {
                assert!(!text.contains(assertion), "{stage:?}: {text}");
            }
        }
    }

    #[test]
    fn no_stage_points_to_linux() {
        for stage in ALL_STAGES {
            let text = guidance(stage);
            assert!(!text.contains("Linux"), "{stage:?}: {text}");
            assert!(!text.contains("--resume"), "{stage:?}: {text}");
        }
    }
}
