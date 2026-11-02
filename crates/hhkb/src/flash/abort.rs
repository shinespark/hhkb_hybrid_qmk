use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    EnterNotSent,
    QueryOnly,
    ModeChangeAttempted,
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

const LINUX_RECOVERY: &str = "Linux からの復旧: docs/analysis/findings.md の「Linux からの復旧手順」に従い、承認を得てから python3 tools/flash.py --resume --device /dev/hidrawN --trace-log <公開しない場所>/flash-trace.jsonl ../HHKB800_FW_A048.hfb を実行します。途中で止まった状態からの復旧は未検証です";

const CHECK_BY_KEY_INPUT: &str =
    "HHKB の状態は断定できません。キー入力の可否で確認します（USB の表示では区別できません）";

impl Abort {
    pub fn guidance(&self) -> String {
        match self.stage {
            Stage::EnterNotSent => {
                "到達した段階: 移行前。レポートは送信していません。原因を解消してから hhkb flash enter をやり直します"
                    .to_owned()
            }
            Stage::QueryOnly => {
                "到達した段階: 版の確認。02 だけを送り、移行のレポート（e0）は送信していません。キー入力と hhkb status で状態を確かめてから判断します"
                    .to_owned()
            }
            Stage::ModeChangeAttempted => format!(
                "到達した段階: 移行のレポート（e0）の送信後。{CHECK_BY_KEY_INPUT}\n\
                 - 入力できる: 純正 FW で動いている可能性があります。hhkb flash enter をやり直すかを判断します\n\
                 - 入力できない: 更新モードの可能性があります。USB を抜き差しして hhkb list で確かめ、hhkb flash write を実行するかを判断します\n\
                 {LINUX_RECOVERY}"
            ),
            Stage::WriteNotSent => format!(
                "到達した段階: 書き込みの前。レポートは送信していません。原因を解消し、キー入力できないことを確かめてから hhkb flash write をやり直します\n{LINUX_RECOVERY}"
            ),
            Stage::TransferAttempted { last_acked, total } => {
                let progress = last_acked.map_or_else(
                    || format!("応答を確認できたパケットはありません（全 {total} 件）"),
                    |packet| format!("応答を確認した最後のパケットは {packet} です（番号は 0 から、全 {total} 件）"),
                );
                format!(
                    "到達した段階: 本体の転送。{progress}。{CHECK_BY_KEY_INPUT}\n\
                     - 入力できない: USB を抜き差しして Bluetooth の接続がないことを確かめ、hhkb flash write を 1 回だけやり直すかを判断します。途中からの再開は未検証です\n\
                     - 入力できる: hhkb flash write を実行せず、hhkb status で確かめます\n\
                     {LINUX_RECOVERY}"
                )
            }
            Stage::EndAttempted => format!(
                "到達した段階: 終了のレポート（e3）の送信後。本体の全パケットの応答は確認済みです。{CHECK_BY_KEY_INPUT}\n\
                 - 入力できる: hhkb status と公式ツールの read で版と設定を確かめます\n\
                 - 入力できない: USB を抜き差しして再度確かめ、hhkb flash write をやり直すかを判断します\n\
                 {LINUX_RECOVERY}"
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

    const ALL_STAGES: [Stage; 6] = [
        Stage::EnterNotSent,
        Stage::QueryOnly,
        Stage::ModeChangeAttempted,
        Stage::WriteNotSent,
        Stage::TransferAttempted {
            last_acked: Some(10),
            total: 4960,
        },
        Stage::EndAttempted,
    ];

    #[test]
    fn nothing_sent_before_mode_change_says_so() {
        assert!(guidance(Stage::EnterNotSent).contains("送信していません"));
    }

    #[test]
    fn version_query_stage_says_the_mode_change_was_not_sent() {
        let text = guidance(Stage::QueryOnly);
        assert!(text.contains("e0"), "{text}");
        assert!(text.contains("送信していません"), "{text}");
    }

    #[test]
    fn mode_change_stage_explains_both_key_input_outcomes_and_recovery() {
        let text = guidance(Stage::ModeChangeAttempted);
        assert!(text.contains("キー入力"), "{text}");
        assert!(text.contains("hhkb flash enter"), "{text}");
        assert!(text.contains("hhkb flash write"), "{text}");
        assert!(text.contains("Linux"), "{text}");
    }

    #[test]
    fn write_not_sent_stage_says_nothing_was_sent_and_shows_recovery() {
        let text = guidance(Stage::WriteNotSent);
        assert!(text.contains("送信していません"), "{text}");
        assert!(text.contains("Linux"), "{text}");
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
        assert!(text.contains("Linux"), "{text}");
    }

    #[test]
    fn transfer_stage_without_an_acknowledged_packet_says_none() {
        let text = guidance(Stage::TransferAttempted {
            last_acked: None,
            total: 4960,
        });
        assert!(text.contains("確認できたパケットはありません"), "{text}");
    }

    #[test]
    fn end_stage_asks_to_check_before_retrying() {
        let text = guidance(Stage::EndAttempted);
        assert!(text.contains("e3"), "{text}");
        assert!(text.contains("キー入力"), "{text}");
        assert!(text.contains("Linux"), "{text}");
    }

    #[test]
    fn stages_after_sending_never_assert_the_device_state() {
        for stage in ALL_STAGES {
            let text = guidance(stage);
            for assertion in [
                "更新モードです",
                "更新モードのままです",
                "純正 FW です",
                "壊れ",
            ] {
                assert!(!text.contains(assertion), "{stage:?}: {text}");
            }
        }
    }

    #[test]
    fn linux_recovery_refers_to_the_recorded_procedure() {
        let text = guidance(Stage::ModeChangeAttempted);
        assert!(text.contains("docs/analysis/findings.md"), "{text}");
        assert!(text.contains("--resume"), "{text}");
        assert!(!text.contains("--allow-crc-mismatch"), "{text}");
    }
}
