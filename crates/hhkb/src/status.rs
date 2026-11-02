use std::fmt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use hidapi::HidApi;

use crate::device::{
    DeviceState, HidEntry, HidEnumerator, QMK_USAGE_PAGE, REPORT_SIZE, ReportChannel, SelectError,
    SystemDevice, Target, VENDOR_USAGE_PAGE, open_device, select_target,
};

const COMMAND_OFFSET: usize = 2;
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(1);
const OFFICIAL_TOOL_PROCESS: &str = "hhkb-keymap-tool";

pub enum ApprovedReport {
    GetKeyboardInformation,
}

impl ApprovedReport {
    const fn bytes(&self) -> [u8; REPORT_SIZE] {
        match self {
            Self::GetKeyboardInformation => {
                let mut report = [0; REPORT_SIZE];
                report[0] = 0xaa;
                report[1] = 0xaa;
                report[COMMAND_OFFSET] = 0x02;
                report
            }
        }
    }
}

const APPROVED_REPORTS: &[ApprovedReport] = &[ApprovedReport::GetKeyboardInformation];

#[derive(Debug, PartialEq, Eq)]
pub struct Received {
    pub command: u8,
    pub byte3: u8,
    pub data_len: u8,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Status {
    pub target: Target,
    pub received: Vec<Received>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum StatusError {
    OfficialToolRunning,
    ProcessCheckFailed(String),
    Select(SelectError),
    VendorInterfaceNotIsolated,
    QmkInterfaceNotIsolated,
    Open(String),
    Send(String),
    Receive(String),
    Timeout { command: u8 },
}

impl fmt::Display for StatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OfficialToolRunning => write!(
                f,
                "The official tool is running. Close it and retry (the device was not opened)"
            ),
            Self::ProcessCheckFailed(reason) => write!(
                f,
                "Aborted: cannot check whether the official tool is running (the device was not opened): {reason}"
            ),
            Self::Select(error) => write!(f, "{error}"),
            Self::VendorInterfaceNotIsolated => write!(
                f,
                "Not opening: the Usage Page 0xFF00 interface is not isolated from other usages"
            ),
            Self::QmkInterfaceNotIsolated => write!(
                f,
                "Not opening: the Usage Page 0xFF60 interface is not isolated from other usages"
            ),
            Self::Open(reason) => write!(f, "Cannot open the device: {reason}"),
            Self::Send(reason) => write!(f, "Send failed. Not retrying: {reason}"),
            Self::Receive(reason) => write!(f, "Receive failed. Not retrying: {reason}"),
            Self::Timeout { command } => write!(
                f,
                "No response to command 0x{command:02x} within the time limit. Not retrying"
            ),
        }
    }
}

impl std::error::Error for StatusError {}

pub(crate) trait ProcessCheck {
    fn official_tool_running(&self) -> Result<bool, String>;
}

trait DeviceOpener {
    type Device: ReportChannel;
    fn open(&self, path: &str) -> Result<Self::Device, String>;
}

pub(crate) struct Pgrep;

impl ProcessCheck for Pgrep {
    fn official_tool_running(&self) -> Result<bool, String> {
        let status = Command::new("/usr/bin/pgrep")
            .args(["-x", OFFICIAL_TOOL_PROCESS])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|error| error.to_string())?;
        interpret_pgrep(status.code())
    }
}

fn interpret_pgrep(code: Option<i32>) -> Result<bool, String> {
    match code {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        Some(code) => Err(format!("pgrep exited with code {code}")),
        None => Err("pgrep was terminated by a signal".to_owned()),
    }
}

struct SystemOpener<'a> {
    api: &'a HidApi,
}

impl DeviceOpener for SystemOpener<'_> {
    type Device = SystemDevice;

    fn open(&self, path: &str) -> Result<SystemDevice, String> {
        open_device(self.api, path)
    }
}

fn parse_response(bytes: &[u8], command: u8) -> Option<Received> {
    match bytes {
        [0x55, 0x55, received, byte3, _, data_len, ..] if *received == command => Some(Received {
            command,
            byte3: *byte3,
            data_len: *data_len,
        }),
        _ => None,
    }
}

fn exchange(
    device: &mut impl ReportChannel,
    report: &[u8; REPORT_SIZE],
    timeout: Duration,
) -> Result<Received, StatusError> {
    let command = report[COMMAND_OFFSET];
    device.send(report).map_err(StatusError::Send)?;
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(StatusError::Timeout { command });
        }
        let Some(bytes) = device.receive(remaining).map_err(StatusError::Receive)? else {
            return Err(StatusError::Timeout { command });
        };
        if let Some(received) = parse_response(&bytes, command) {
            return Ok(received);
        }
    }
}

fn isolated_interface_path(target: &Target, wanted: u16) -> Option<&str> {
    target
        .interfaces
        .iter()
        .find(|interface| interface.usages.iter().any(|(page, _)| *page == wanted))
        .filter(|interface| interface.usages.iter().all(|(page, _)| *page == wanted))
        .map(|interface| interface.path.as_str())
}

pub(crate) fn vendor_interface_path(target: &Target) -> Result<&str, StatusError> {
    isolated_interface_path(target, VENDOR_USAGE_PAGE)
        .ok_or(StatusError::VendorInterfaceNotIsolated)
}

pub(crate) fn qmk_interface_path(target: &Target) -> Result<&str, StatusError> {
    isolated_interface_path(target, QMK_USAGE_PAGE).ok_or(StatusError::QmkInterfaceNotIsolated)
}

fn check_with(
    process: &impl ProcessCheck,
    entries: &[HidEntry],
    opener: &impl DeviceOpener,
    reports: &[[u8; REPORT_SIZE]],
    timeout: Duration,
) -> Result<Status, StatusError> {
    let running = process
        .official_tool_running()
        .map_err(StatusError::ProcessCheckFailed)?;
    if running {
        return Err(StatusError::OfficialToolRunning);
    }
    let target = select_target(entries).map_err(StatusError::Select)?;
    if reports.is_empty() || target.state != DeviceState::StockOrUpdate {
        return Ok(Status {
            target,
            received: Vec::new(),
        });
    }
    let path = vendor_interface_path(&target)?;
    let mut device = opener.open(path).map_err(StatusError::Open)?;
    let mut received = Vec::with_capacity(reports.len());
    for report in reports {
        received.push(exchange(&mut device, report, timeout)?);
    }
    Ok(Status { target, received })
}

pub fn check_status(hid: &impl HidEnumerator, api: &HidApi) -> Result<Status, StatusError> {
    let reports: Vec<[u8; REPORT_SIZE]> =
        APPROVED_REPORTS.iter().map(ApprovedReport::bytes).collect();
    check_with(
        &Pgrep,
        &hid.enumerate(),
        &SystemOpener { api },
        &reports,
        RESPONSE_TIMEOUT,
    )
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    use super::*;
    use crate::device::{PRODUCT_ID, Transport, VENDOR_ID};

    const TIMEOUT: Duration = Duration::from_millis(20);

    struct FixedProcessCheck(Result<bool, String>);

    impl ProcessCheck for FixedProcessCheck {
        fn official_tool_running(&self) -> Result<bool, String> {
            self.0.clone()
        }
    }

    const TOOL_NOT_RUNNING: FixedProcessCheck = FixedProcessCheck(Ok(false));

    enum Reply {
        Data(Vec<u8>),
        Nothing,
        Fail,
    }

    #[derive(Default)]
    struct Log {
        opened: Vec<String>,
        send_attempts: Vec<Vec<u8>>,
    }

    struct MockDevice {
        log: Rc<RefCell<Log>>,
        send_fails: bool,
        replies: VecDeque<Reply>,
        repeat_last: Option<Vec<u8>>,
    }

    impl ReportChannel for MockDevice {
        fn send(&mut self, report: &[u8]) -> Result<(), String> {
            self.log.borrow_mut().send_attempts.push(report.to_vec());
            if self.send_fails {
                Err("send failed".to_owned())
            } else {
                Ok(())
            }
        }

        fn receive(&mut self, _timeout: Duration) -> Result<Option<Vec<u8>>, String> {
            if let Some(data) = &self.repeat_last {
                return Ok(Some(data.clone()));
            }
            match self.replies.pop_front() {
                Some(Reply::Data(data)) => Ok(Some(data)),
                Some(Reply::Fail) => Err("receive failed".to_owned()),
                Some(Reply::Nothing) | None => Ok(None),
            }
        }
    }

    struct MockOpener {
        log: Rc<RefCell<Log>>,
        open_fails: bool,
        device: RefCell<Option<MockDevice>>,
    }

    impl DeviceOpener for MockOpener {
        type Device = MockDevice;

        fn open(&self, path: &str) -> Result<MockDevice, String> {
            self.log.borrow_mut().opened.push(path.to_owned());
            if self.open_fails {
                return Err("cannot open".to_owned());
            }
            Ok(self.device.borrow_mut().take().expect("opened only once"))
        }
    }

    fn opener(replies: Vec<Reply>) -> (MockOpener, Rc<RefCell<Log>>) {
        let log = Rc::new(RefCell::new(Log::default()));
        let device = MockDevice {
            log: Rc::clone(&log),
            send_fails: false,
            replies: replies.into(),
            repeat_last: None,
        };
        let opener = MockOpener {
            log: Rc::clone(&log),
            open_fails: false,
            device: RefCell::new(Some(device)),
        };
        (opener, log)
    }

    fn entry(path: &str, usage_page: u16, usage: u16) -> HidEntry {
        HidEntry {
            path: path.to_owned(),
            vendor_id: VENDOR_ID,
            product_id: PRODUCT_ID,
            transport: Transport::Usb,
            product: Some("HHKB-Hybrid".to_owned()),
            release: 0x0001,
            usage_page,
            usage,
        }
    }

    fn keyboard(prefix: &str) -> Vec<HidEntry> {
        vec![
            entry(&format!("{prefix}-kbd"), 0x0001, 0x0006),
            entry(&format!("{prefix}-cc"), 0x000c, 0x0001),
            entry(&format!("{prefix}-cc"), 0x0001, 0x0006),
            entry(&format!("{prefix}-vendor"), VENDOR_USAGE_PAGE, 0x0001),
        ]
    }

    fn report(command: u8) -> [u8; REPORT_SIZE] {
        let mut bytes = [0; REPORT_SIZE];
        bytes[..3].copy_from_slice(&[0xaa, 0xaa, command]);
        bytes
    }

    fn response(command: u8, data_len: u8) -> Vec<u8> {
        let mut bytes = vec![0; REPORT_SIZE];
        bytes[..6].copy_from_slice(&[0x55, 0x55, command, 0x00, 0x00, data_len]);
        bytes
    }

    #[test]
    fn no_approved_reports_opens_nothing_and_sends_nothing() {
        let (opener, log) = opener(vec![]);
        let status = check_with(&TOOL_NOT_RUNNING, &keyboard("a"), &opener, &[], TIMEOUT).unwrap();
        assert_eq!(status.target.interfaces.len(), 3);
        assert!(status.received.is_empty());
        assert!(log.borrow().opened.is_empty());
        assert!(log.borrow().send_attempts.is_empty());
    }

    fn named(path: &str, product: &str, usage_page: u16, usage: u16) -> HidEntry {
        HidEntry {
            product: Some(product.to_owned()),
            ..entry(path, usage_page, usage)
        }
    }

    #[test]
    fn qmk_is_listed_without_opening_or_sending() {
        let entries = vec![
            named("a-kbd", "HHKB Hybrid QMK", 0x0001, 0x0006),
            named("a-raw", "HHKB Hybrid QMK", QMK_USAGE_PAGE, 0x0061),
        ];
        let (opener, log) = opener(vec![]);
        let status = check_with(
            &TOOL_NOT_RUNNING,
            &entries,
            &opener,
            &[report(0x02)],
            TIMEOUT,
        )
        .unwrap();
        assert_eq!(status.target.state, DeviceState::QmkLikely);
        assert!(status.received.is_empty());
        assert!(log.borrow().opened.is_empty());
        assert!(log.borrow().send_attempts.is_empty());
    }

    #[test]
    fn undetermined_state_is_listed_without_opening_or_sending() {
        let cases = [
            vec![named("a-raw", "HHKB-Hybrid", QMK_USAGE_PAGE, 0x0061)],
            vec![named(
                "a-vendor",
                "HHKB Hybrid QMK",
                VENDOR_USAGE_PAGE,
                0x0001,
            )],
        ];
        for entries in cases {
            let (opener, log) = opener(vec![]);
            let status = check_with(
                &TOOL_NOT_RUNNING,
                &entries,
                &opener,
                &[report(0x02)],
                TIMEOUT,
            )
            .unwrap();
            assert_eq!(status.target.state, DeviceState::Undetermined);
            assert!(log.borrow().opened.is_empty());
            assert!(log.borrow().send_attempts.is_empty());
        }
    }

    #[test]
    fn official_tool_running_is_rejected_without_opening() {
        let (opener, log) = opener(vec![]);
        let result = check_with(
            &FixedProcessCheck(Ok(true)),
            &keyboard("a"),
            &opener,
            &[report(0x02)],
            TIMEOUT,
        );
        assert_eq!(result, Err(StatusError::OfficialToolRunning));
        assert!(log.borrow().opened.is_empty());
    }

    #[test]
    fn official_tool_running_is_rejected_even_without_approved_reports() {
        let (opener, _log) = opener(vec![]);
        let result = check_with(
            &FixedProcessCheck(Ok(true)),
            &keyboard("a"),
            &opener,
            &[],
            TIMEOUT,
        );
        assert_eq!(result, Err(StatusError::OfficialToolRunning));
    }

    #[test]
    fn failed_process_check_is_rejected_without_opening() {
        let (opener, log) = opener(vec![]);
        let result = check_with(
            &FixedProcessCheck(Err("pgrep failed".to_owned())),
            &keyboard("a"),
            &opener,
            &[report(0x02)],
            TIMEOUT,
        );
        assert_eq!(
            result,
            Err(StatusError::ProcessCheckFailed("pgrep failed".to_owned()))
        );
        assert!(log.borrow().opened.is_empty());
    }

    #[test]
    fn missing_keyboard_is_rejected_without_opening() {
        let (opener, log) = opener(vec![]);
        let result = check_with(&TOOL_NOT_RUNNING, &[], &opener, &[report(0x02)], TIMEOUT);
        assert_eq!(result, Err(StatusError::Select(SelectError::NotFound)));
        assert!(log.borrow().opened.is_empty());
    }

    #[test]
    fn only_the_vendor_interface_is_opened() {
        let (opener, log) = opener(vec![Reply::Data(response(0x02, 57))]);
        check_with(
            &TOOL_NOT_RUNNING,
            &keyboard("a"),
            &opener,
            &[report(0x02)],
            TIMEOUT,
        )
        .unwrap();
        assert_eq!(log.borrow().opened, ["a-vendor"]);
    }

    #[test]
    fn interface_shared_with_keyboard_usage_is_not_opened() {
        let (opener, log) = opener(vec![]);
        let entries = vec![
            entry("a-mixed", VENDOR_USAGE_PAGE, 0x0001),
            entry("a-mixed", 0x0001, 0x0006),
        ];
        let result = check_with(
            &TOOL_NOT_RUNNING,
            &entries,
            &opener,
            &[report(0x02)],
            TIMEOUT,
        );
        assert_eq!(result, Err(StatusError::VendorInterfaceNotIsolated));
        assert!(log.borrow().opened.is_empty());
    }

    #[test]
    fn open_failure_is_reported_without_sending() {
        let (mut opener, log) = opener(vec![]);
        opener.open_fails = true;
        let result = check_with(
            &TOOL_NOT_RUNNING,
            &keyboard("a"),
            &opener,
            &[report(0x02)],
            TIMEOUT,
        );
        assert_eq!(result, Err(StatusError::Open("cannot open".to_owned())));
        assert!(log.borrow().send_attempts.is_empty());
    }

    #[test]
    fn report_is_sent_once_with_exact_bytes() {
        let (opener, log) = opener(vec![Reply::Data(response(0x02, 57))]);
        let status = check_with(
            &TOOL_NOT_RUNNING,
            &keyboard("a"),
            &opener,
            &[report(0x02)],
            TIMEOUT,
        )
        .unwrap();
        assert_eq!(log.borrow().send_attempts, [report(0x02).to_vec()]);
        assert_eq!(
            status.received,
            [Received {
                command: 0x02,
                byte3: 0x00,
                data_len: 57
            }]
        );
    }

    #[test]
    fn timeout_stops_the_remaining_reports() {
        let (opener, log) = opener(vec![Reply::Nothing]);
        let result = check_with(
            &TOOL_NOT_RUNNING,
            &keyboard("a"),
            &opener,
            &[report(0x02), report(0x05)],
            TIMEOUT,
        );
        assert_eq!(result, Err(StatusError::Timeout { command: 0x02 }));
        assert_eq!(log.borrow().send_attempts, [report(0x02).to_vec()]);
    }

    #[test]
    fn send_failure_is_not_retried() {
        let (opener, log) = opener(vec![]);
        opener.device.borrow_mut().as_mut().unwrap().send_fails = true;
        let result = check_with(
            &TOOL_NOT_RUNNING,
            &keyboard("a"),
            &opener,
            &[report(0x02)],
            TIMEOUT,
        );
        assert_eq!(result, Err(StatusError::Send("send failed".to_owned())));
        assert_eq!(log.borrow().send_attempts.len(), 1);
    }

    #[test]
    fn receive_failure_is_not_retried() {
        let (opener, log) = opener(vec![Reply::Fail, Reply::Data(response(0x02, 57))]);
        let result = check_with(
            &TOOL_NOT_RUNNING,
            &keyboard("a"),
            &opener,
            &[report(0x02)],
            TIMEOUT,
        );
        assert_eq!(
            result,
            Err(StatusError::Receive("receive failed".to_owned()))
        );
        assert_eq!(log.borrow().send_attempts.len(), 1);
    }

    #[test]
    fn unrelated_input_is_skipped_until_the_matching_response() {
        let (opener, _log) = opener(vec![
            Reply::Data(response(0x05, 12)),
            Reply::Data(vec![0x55, 0x55, 0x02]),
            Reply::Data(response(0x02, 57)),
        ]);
        let status = check_with(
            &TOOL_NOT_RUNNING,
            &keyboard("a"),
            &opener,
            &[report(0x02)],
            TIMEOUT,
        )
        .unwrap();
        assert_eq!(
            status.received,
            [Received {
                command: 0x02,
                byte3: 0x00,
                data_len: 57
            }]
        );
    }

    #[test]
    fn endless_unrelated_input_ends_with_timeout() {
        let (opener, log) = opener(vec![]);
        opener.device.borrow_mut().as_mut().unwrap().repeat_last = Some(response(0x05, 12));
        let result = check_with(
            &TOOL_NOT_RUNNING,
            &keyboard("a"),
            &opener,
            &[report(0x02)],
            TIMEOUT,
        );
        assert_eq!(result, Err(StatusError::Timeout { command: 0x02 }));
        assert_eq!(log.borrow().send_attempts.len(), 1);
    }

    #[test]
    fn only_the_recorded_get_keyboard_information_report_is_approved() {
        let mut expected = [0u8; 64];
        expected[0] = 0xaa;
        expected[1] = 0xaa;
        expected[2] = 0x02;
        let approved: Vec<[u8; REPORT_SIZE]> =
            APPROVED_REPORTS.iter().map(ApprovedReport::bytes).collect();
        assert_eq!(approved, [expected]);
    }

    #[test]
    fn fourth_byte_of_the_response_is_reported() {
        let mut bytes = response(0x02, 57);
        bytes[3] = 0x07;
        let (opener, _log) = opener(vec![Reply::Data(bytes)]);
        let status = check_with(
            &TOOL_NOT_RUNNING,
            &keyboard("a"),
            &opener,
            &[report(0x02)],
            TIMEOUT,
        )
        .unwrap();
        assert_eq!(status.received[0].byte3, 0x07);
    }

    #[test]
    fn pgrep_exit_codes_fail_closed() {
        assert_eq!(interpret_pgrep(Some(0)), Ok(true));
        assert_eq!(interpret_pgrep(Some(1)), Ok(false));
        assert!(interpret_pgrep(Some(2)).is_err());
        assert!(interpret_pgrep(None).is_err());
    }
}
