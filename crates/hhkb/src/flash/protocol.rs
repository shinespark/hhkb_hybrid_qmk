use std::fmt;
use std::time::Duration;

use super::image::VerifiedHfb;
use super::log::Log;
use crate::hfb::to_hex;
use crate::status::REPORT_SIZE;

pub(super) type Frame = [u8; REPORT_SIZE];

pub(super) const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    KeyboardInfo,
    ModeChange,
    Start,
    Body(u16),
    End,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ExchangeFailure {
    Send(String),
    Receive(String),
    Timeout,
    Unexpected { head: Vec<u8> },
    PacketMismatch { got: u16 },
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct ExchangeError {
    pub(super) step: Step,
    pub(super) failure: ExchangeFailure,
}

impl fmt::Display for ExchangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.step {
            Step::Body(packet) => write!(f, "コマンド 0xe2（パケット {packet}）: ")?,
            step => write!(f, "コマンド 0x{:02x}: ", command(step))?,
        }
        match &self.failure {
            ExchangeFailure::Send(reason) => write!(f, "送信に失敗しました: {reason}")?,
            ExchangeFailure::Receive(reason) => write!(f, "受信に失敗しました: {reason}")?,
            ExchangeFailure::Timeout => write!(f, "応答が制限時間内に届きませんでした")?,
            ExchangeFailure::Unexpected { head } => {
                write!(f, "想定と異なる応答です（先頭 {}）", to_hex(head))?;
            }
            ExchangeFailure::PacketMismatch { got } => {
                write!(f, "応答のパケット番号が {got} で、送信と異なります")?;
            }
        }
        write!(f, "。再送しません")
    }
}

pub(super) trait ReportChannel {
    fn send(&mut self, frame: &Frame) -> Result<(), String>;
    fn receive(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, String>;
}

const RESPONSE_HEAD_SIZE: usize = 6;
const PACKET_NUMBER_OFFSET: usize = RESPONSE_HEAD_SIZE;
const BODY_ACK_SIZE: usize = PACKET_NUMBER_OFFSET + 2;
const KEYBOARD_INFO_DATA_SIZE: u8 = 57;

fn command(step: Step) -> u8 {
    match step {
        Step::KeyboardInfo => 0x02,
        Step::ModeChange => 0xe0,
        Step::Start => 0xe1,
        Step::Body(_) => 0xe2,
        Step::End => 0xe3,
    }
}

fn expected_data_size(step: Step) -> u8 {
    match step {
        Step::KeyboardInfo => KEYBOARD_INFO_DATA_SIZE,
        Step::Body(_) => 2,
        Step::ModeChange | Step::Start | Step::End => 0,
    }
}

pub(super) fn frame_for(step: Step, hfb: &VerifiedHfb) -> Frame {
    let mut frame = [0; REPORT_SIZE];
    frame[..3].copy_from_slice(&[0xaa, 0xaa, command(step)]);
    match step {
        Step::KeyboardInfo | Step::ModeChange | Step::End => {}
        Step::Start => {
            let size = u32::try_from(hfb.size()).expect("HFB のサイズは検査済み");
            frame[4] = 8;
            frame[5..9].copy_from_slice(&size.to_le_bytes());
            frame[9..11].copy_from_slice(&hfb.stored_crc());
        }
        Step::Body(packet) => {
            let chunk = hfb.chunk(packet);
            frame[4] = u8::try_from(chunk.len() + 2).expect("パケットは 57 バイト以下");
            frame[5..7].copy_from_slice(&packet.to_le_bytes());
            frame[7..7 + chunk.len()].copy_from_slice(chunk);
        }
    }
    frame
}

fn validate(step: Step, bytes: &[u8]) -> Result<(), ExchangeFailure> {
    let expected = [0x55, 0x55, command(step), 0, 0, expected_data_size(step)];
    if !bytes.starts_with(&expected) {
        return Err(ExchangeFailure::Unexpected {
            head: bytes[..bytes.len().min(RESPONSE_HEAD_SIZE)].to_vec(),
        });
    }
    if let Step::Body(packet) = step {
        let Some(number) = bytes.get(PACKET_NUMBER_OFFSET..BODY_ACK_SIZE) else {
            return Err(ExchangeFailure::Unexpected {
                head: expected.to_vec(),
            });
        };
        let got = u16::from_le_bytes([number[0], number[1]]);
        if got != packet {
            return Err(ExchangeFailure::PacketMismatch { got });
        }
    }
    Ok(())
}

pub(super) fn exchange(
    device: &mut impl ReportChannel,
    log: &mut Log,
    step: Step,
    frame: &Frame,
    timeout: Duration,
) -> Result<Vec<u8>, ExchangeError> {
    let fail = |failure| ExchangeError { step, failure };
    log.record_send(step, frame);
    device
        .send(frame)
        .map_err(|reason| fail(ExchangeFailure::Send(reason)))?;
    let bytes = device
        .receive(timeout)
        .map_err(|reason| fail(ExchangeFailure::Receive(reason)))?
        .ok_or_else(|| fail(ExchangeFailure::Timeout))?;
    log.record_receive(step, &bytes);
    validate(step, &bytes).map_err(fail)?;
    Ok(bytes)
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct TransferError {
    pub(super) error: ExchangeError,
    pub(super) last_acked: Option<u16>,
}

pub(super) fn transfer(
    device: &mut impl ReportChannel,
    log: &mut Log,
    hfb: &VerifiedHfb,
    timeout: Duration,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<(), TransferError> {
    let total = hfb.packet_count();
    let mut last_acked = None;
    let step_through = |device: &mut _, log: &mut _, step, last_acked: Option<u16>| {
        exchange(device, log, step, &frame_for(step, hfb), timeout)
            .map(|_| ())
            .map_err(|error| TransferError { error, last_acked })
    };
    step_through(device, log, Step::Start, last_acked)?;
    for index in 0..total {
        let packet = u16::try_from(index).expect("パケット数は HFB のサイズで決まる");
        step_through(device, log, Step::Body(packet), last_acked)?;
        last_acked = Some(packet);
        progress(index + 1, total);
    }
    step_through(device, log, Step::End, last_acked)
}

#[cfg(test)]
pub(super) mod testing {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(in crate::flash) enum Fault {
        SendFails,
        ReceiveFails,
        NoResponse,
        WrongCommand,
        NonZeroStatus,
        WrongDataLength,
        WrongPacketNumber,
        ShortResponse,
    }

    pub(in crate::flash) fn correct_reply(frame: &Frame) -> Vec<u8> {
        let mut reply = vec![0u8; REPORT_SIZE];
        reply[..3].copy_from_slice(&[0x55, 0x55, frame[2]]);
        if frame[2] == 0xe2 {
            reply[5] = 2;
            reply[6] = frame[5];
            reply[7] = frame[6];
        }
        reply
    }

    #[derive(Default)]
    pub(in crate::flash) struct ScriptedDevice {
        pub(in crate::flash) sent: Vec<Frame>,
        pub(in crate::flash) fault: Option<(usize, Fault)>,
    }

    impl ScriptedDevice {
        fn current_fault(&self) -> Option<Fault> {
            self.fault
                .filter(|(index, _)| *index + 1 == self.sent.len())
                .map(|(_, fault)| fault)
        }
    }

    impl ReportChannel for ScriptedDevice {
        fn send(&mut self, frame: &Frame) -> Result<(), String> {
            self.sent.push(*frame);
            if self.current_fault() == Some(Fault::SendFails) {
                return Err("送信失敗".to_owned());
            }
            Ok(())
        }

        fn receive(&mut self, _timeout: Duration) -> Result<Option<Vec<u8>>, String> {
            let mut reply = correct_reply(self.sent.last().expect("送信の前に受信しない"));
            match self.current_fault() {
                Some(Fault::ReceiveFails) => return Err("受信失敗".to_owned()),
                Some(Fault::NoResponse) => return Ok(None),
                Some(Fault::WrongCommand) => reply[2] = 0x02,
                Some(Fault::NonZeroStatus) => reply[3] = 0x01,
                Some(Fault::WrongDataLength) => reply[5] = 0x09,
                Some(Fault::WrongPacketNumber) => reply[6] ^= 0x01,
                Some(Fault::ShortResponse) => reply.truncate(3),
                Some(Fault::SendFails) | None => {}
            }
            Ok(Some(reply))
        }
    }

    pub(in crate::flash) fn null_log() -> Log {
        Log::new(Box::new(std::io::sink()))
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{Fault, ScriptedDevice, null_log};
    use super::*;
    use crate::flash::image::testing::{sha256_hex, synthetic};

    fn hfb() -> VerifiedHfb {
        let data = synthetic();
        let sha256 = sha256_hex(&data);
        VerifiedHfb::verify_against(data, &[sha256.as_str()]).unwrap()
    }

    fn run(fault: Option<(usize, Fault)>) -> (ScriptedDevice, Result<(), TransferError>) {
        let mut device = ScriptedDevice {
            sent: Vec::new(),
            fault,
        };
        let result = transfer(
            &mut device,
            &mut null_log(),
            &hfb(),
            Duration::from_millis(1),
            &mut |_, _| {},
        );
        (device, result)
    }

    fn zeros_after(frame: &Frame, offset: usize) -> bool {
        frame[offset..].iter().all(|byte| *byte == 0)
    }

    fn json_string<'a>(line: &'a str, key: &str) -> &'a str {
        let prefix = format!("\"{key}\": \"");
        let start = line.find(&prefix).expect("trace の項目がありません") + prefix.len();
        let end = start
            + line[start..]
                .find('"')
                .expect("trace の項目が閉じていません");
        &line[start..end]
    }

    fn from_hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    #[ignore = "local/ の純正 HFB と、Linux から取得した local/flash-trace.jsonl が必要"]
    fn frames_and_validation_match_the_linux_trace() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../local");
        let data = std::fs::read(format!("{root}/HHKB800_FW_A048.hfb")).unwrap();
        let hfb = VerifiedHfb::verify(data).unwrap();
        let trace = std::fs::read_to_string(format!("{root}/flash-trace.jsonl")).unwrap();
        let mut steps = vec![Step::ModeChange, Step::Start];
        steps.extend((0..4960).map(Step::Body));
        steps.push(Step::End);
        let lines: Vec<&str> = trace.lines().collect();
        assert_eq!(lines.len(), steps.len());
        for (step, line) in steps.iter().zip(lines) {
            let sent = from_hex(json_string(line, "tx"));
            let frame = frame_for(*step, &hfb);
            assert_eq!(frame[..sent.len()], sent[..], "{step:?}");
            assert!(zeros_after(&frame, sent.len()), "{step:?}");
            let received = from_hex(json_string(line, "rx"));
            assert_eq!(validate(*step, &received), Ok(()), "{step:?}");
        }
    }

    #[test]
    fn keyboard_info_frame_is_the_approved_report() {
        let frame = frame_for(Step::KeyboardInfo, &hfb());
        assert_eq!(frame[..3], [0xaa, 0xaa, 0x02]);
        assert!(zeros_after(&frame, 3));
    }

    #[test]
    fn mode_change_frame_matches_the_linux_record() {
        let frame = frame_for(Step::ModeChange, &hfb());
        assert_eq!(frame[..3], [0xaa, 0xaa, 0xe0]);
        assert!(zeros_after(&frame, 3));
    }

    #[test]
    fn start_frame_matches_the_linux_record() {
        let data = synthetic();
        let frame = frame_for(Step::Start, &hfb());
        assert_eq!(frame[..5], [0xaa, 0xaa, 0xe1, 0x00, 0x08]);
        assert_eq!(frame[5..9], [0x30, 0x50, 0x04, 0x00]);
        assert_eq!(frame[9..11], data[..2]);
        assert!(zeros_after(&frame, 11));
    }

    #[test]
    fn first_body_frame_starts_at_offset_two() {
        let data = synthetic();
        let frame = frame_for(Step::Body(0), &hfb());
        assert_eq!(frame[..7], [0xaa, 0xaa, 0xe2, 0x00, 0x3b, 0x00, 0x00]);
        assert_eq!(frame[7..], data[2..59]);
    }

    #[test]
    fn packet_number_is_little_endian() {
        let data = synthetic();
        let frame = frame_for(Step::Body(0x0102), &hfb());
        assert_eq!(frame[5..7], [0x02, 0x01]);
        let start = 2 + 0x0102 * 57;
        assert_eq!(frame[7..], data[start..start + 57]);
    }

    #[test]
    fn last_body_frame_carries_seven_bytes_and_zero_padding() {
        let data = synthetic();
        let frame = frame_for(Step::Body(4959), &hfb());
        assert_eq!(frame[..7], [0xaa, 0xaa, 0xe2, 0x00, 0x09, 0x5f, 0x13]);
        assert_eq!(frame[7..14], data[data.len() - 7..]);
        assert!(zeros_after(&frame, 14));
    }

    #[test]
    fn end_frame_matches_the_linux_record() {
        let frame = frame_for(Step::End, &hfb());
        assert_eq!(frame[..3], [0xaa, 0xaa, 0xe3]);
        assert!(zeros_after(&frame, 3));
    }

    #[test]
    fn transfer_sends_start_all_packets_in_order_then_end() {
        let (device, result) = run(None);
        assert_eq!(result, Ok(()));
        assert_eq!(device.sent.len(), 4962);
        assert_eq!(device.sent[0][2], 0xe1);
        assert_eq!(device.sent[4961][2], 0xe3);
        for (index, frame) in device.sent[1..4961].iter().enumerate() {
            assert_eq!(frame[2], 0xe2);
            assert_eq!(usize::from(u16::from_le_bytes([frame[5], frame[6]])), index);
        }
    }

    #[test]
    fn transferred_payload_reproduces_the_file_after_offset_two() {
        let (device, _) = run(None);
        let mut received = Vec::new();
        for frame in &device.sent[1..4961] {
            let payload = usize::from(frame[4]) - 2;
            received.extend_from_slice(&frame[7..7 + payload]);
        }
        assert_eq!(received, synthetic()[2..]);
    }

    #[test]
    fn every_fault_stops_at_the_failing_frame_without_retry() {
        for fault in [
            Fault::SendFails,
            Fault::ReceiveFails,
            Fault::NoResponse,
            Fault::WrongCommand,
            Fault::NonZeroStatus,
            Fault::WrongDataLength,
            Fault::ShortResponse,
        ] {
            for index in [0, 1, 1150, 4961] {
                let (device, result) = run(Some((index, fault)));
                assert!(result.is_err(), "{fault:?} {index}");
                assert_eq!(device.sent.len(), index + 1, "{fault:?} {index}");
            }
        }
    }

    #[test]
    fn packet_number_mismatch_stops_before_the_next_packet() {
        let (device, result) = run(Some((11, Fault::WrongPacketNumber)));
        let error = result.unwrap_err();
        assert_eq!(device.sent.len(), 12);
        assert_eq!(error.error.step, Step::Body(10));
        assert_eq!(
            error.error.failure,
            ExchangeFailure::PacketMismatch { got: 11 }
        );
        assert_eq!(error.last_acked, Some(9));
    }

    #[test]
    fn timeout_is_reported_with_the_last_acknowledged_packet() {
        let (_, result) = run(Some((3, Fault::NoResponse)));
        let error = result.unwrap_err();
        assert_eq!(error.error.step, Step::Body(2));
        assert_eq!(error.error.failure, ExchangeFailure::Timeout);
        assert_eq!(error.last_acked, Some(1));
    }

    #[test]
    fn failure_on_start_has_no_acknowledged_packet() {
        let (_, result) = run(Some((0, Fault::NoResponse)));
        let error = result.unwrap_err();
        assert_eq!(error.error.step, Step::Start);
        assert_eq!(error.last_acked, None);
    }

    #[test]
    fn failure_on_end_reports_all_packets_acknowledged() {
        let (_, result) = run(Some((4961, Fault::NonZeroStatus)));
        let error = result.unwrap_err();
        assert_eq!(error.error.step, Step::End);
        assert_eq!(error.last_acked, Some(4959));
    }

    #[test]
    fn unexpected_response_keeps_only_the_six_byte_head() {
        let mut device = ScriptedDevice {
            sent: Vec::new(),
            fault: Some((0, Fault::WrongCommand)),
        };
        let frame = frame_for(Step::ModeChange, &hfb());
        let error = exchange(
            &mut device,
            &mut null_log(),
            Step::ModeChange,
            &frame,
            Duration::from_millis(1),
        )
        .unwrap_err();
        assert_eq!(
            error.failure,
            ExchangeFailure::Unexpected {
                head: vec![0x55, 0x55, 0x02, 0x00, 0x00, 0x00]
            }
        );
    }

    #[test]
    fn progress_reports_every_packet_total() {
        let mut device = ScriptedDevice::default();
        let mut last = None;
        transfer(
            &mut device,
            &mut null_log(),
            &hfb(),
            Duration::from_millis(1),
            &mut |sent, total| last = Some((sent, total)),
        )
        .unwrap();
        assert_eq!(last, Some((4960, 4960)));
    }
}
