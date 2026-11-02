mod abort;
mod image;
mod log;
mod protocol;

use std::io::{self, BufRead, Write};
use std::path::Path;
use std::time::Duration;

use hidapi::HidApi;
use log::{Log, create_log_file};
use protocol::{
    ExchangeError, RESPONSE_TIMEOUT, RevertAnswer, Step, exchange, frame_for, qmk_mode_change,
    qmk_stock_revert, qmk_via_answers, transfer,
};

pub use abort::{Abort, Stage};
pub use image::{HfbKind, VerifiedHfb, VerifyError};

use crate::device::{
    DeviceState, HidEntry, HidEnumerator, ReportChannel, SelectError, SystemDevice, Target,
    open_device, select_target,
};
use crate::status::{Pgrep, ProcessCheck, StatusError, qmk_interface_path, vendor_interface_path};

const QMK_WARNING: &str = "Writing QMK may void your warranty. If you cannot revert, you may need to disassemble the keyboard. Recovery from an interrupted write is unverified.";
const UNDETERMINED: &str =
    "Not sending: the product name and the interfaces disagree, so the state cannot be determined";

const VIA_QUERY_TIMEOUT: Duration = Duration::from_secs(1);
const VERSION_QUERY_TIMEOUT: Duration = Duration::from_secs(1);
const SETTLE_WAIT: Duration = Duration::from_secs(10);
const REPLUG_WAIT: Duration = Duration::from_secs(120);
const POLL_INTERVAL: Duration = Duration::from_secs(1);
const APP_VERSION_OFFSET: usize = 6 + 42;
const PROGRESS_INTERVAL: usize = 128;
const LOG_PARENT: &str = "local";

#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    pub packets: usize,
    pub connection: Result<Target, SelectError>,
}

pub fn plan(hfb: &VerifiedHfb, hid: &impl HidEnumerator) -> Plan {
    Plan {
        packets: hfb.packet_count(),
        connection: select_target(&hid.enumerate()),
    }
}

trait Platform {
    type Device: ReportChannel;
    fn official_tool_running(&self) -> Result<bool, String>;
    fn enumerate(&mut self) -> Vec<HidEntry>;
    fn open(&mut self, path: &str) -> Result<Self::Device, String>;
    fn ask(&mut self, prompt: &str) -> Result<String, String>;
    fn sleep(&mut self, duration: Duration);
    fn open_log(&mut self, stage: &str) -> Result<Box<dyn Write>, String>;
}

fn abort_at(stage: Stage) -> impl Fn(String) -> Abort {
    move |reason| Abort { stage, reason }
}

const fn enter_phrase(kind: HfbKind) -> &'static str {
    match kind {
        HfbKind::Stock => "enter stock",
        HfbKind::QmkDefault => "enter default",
        HfbKind::QmkVia => "enter via",
    }
}

const fn write_phrase(kind: HfbKind) -> &'static str {
    match kind {
        HfbKind::Stock => "write stock",
        HfbKind::QmkDefault => "write default",
        HfbKind::QmkVia => "write via",
    }
}

fn kind_warning(kind: HfbKind) -> &'static str {
    if kind.is_qmk() { QMK_WARNING } else { "" }
}

fn open_session<P: Platform>(
    platform: &mut P,
    log_name: &str,
    path: &str,
    not_sent: Stage,
) -> Result<(Log, P::Device), Abort> {
    let fail = abort_at(not_sent);
    let log = Log::new(platform.open_log(log_name).map_err(&fail)?);
    let device = platform
        .open(path)
        .map_err(|reason| fail(format!("Cannot open the device: {reason}")))?;
    Ok((log, device))
}

fn update_interface(entries: &[HidEntry]) -> Result<String, String> {
    let target = select_target(entries).map_err(|error| error.to_string())?;
    match target.state {
        DeviceState::StockOrUpdate => vendor_interface_path(&target)
            .map(str::to_owned)
            .map_err(|error| error.to_string()),
        DeviceState::QmkLikely => {
            Err("QMK appears to be running, so this is not treated as update mode".to_owned())
        }
        DeviceState::Undetermined => Err(UNDETERMINED.to_owned()),
    }
}

fn precheck(platform: &mut impl Platform, stage: Stage) -> Result<Target, Abort> {
    let fail = abort_at(stage);
    let running = platform
        .official_tool_running()
        .map_err(|reason| fail(StatusError::ProcessCheckFailed(reason).to_string()))?;
    if running {
        return Err(fail(StatusError::OfficialToolRunning.to_string()));
    }
    select_target(&platform.enumerate()).map_err(|error| fail(error.to_string()))
}

fn confirm(
    platform: &mut impl Platform,
    stage: Stage,
    prompt: &str,
    phrase: &str,
) -> Result<(), Abort> {
    let fail = abort_at(stage);
    let line = platform
        .ask(&format!("{prompt}\nTo proceed, type: {phrase}\n> "))
        .map_err(|reason| fail(format!("Cannot read the confirmation: {reason}")))?;
    if line.trim_end_matches(['\n', '\r']) != phrase {
        return Err(fail("Aborted: the confirmation did not match".to_owned()));
    }
    Ok(())
}

fn version_text(bytes: [u8; 2]) -> String {
    if bytes.iter().all(|digit| *digit <= 9) {
        format!("A0.{}{}", bytes[0], bytes[1])
    } else {
        "(not numeric)".to_owned()
    }
}

fn check_version(response: &[u8], hfb: &VerifiedHfb) -> Result<(), String> {
    let bytes = response
        .get(APP_VERSION_OFFSET..APP_VERSION_OFFSET + 2)
        .ok_or_else(|| "Cannot read the device version from the response".to_owned())?;
    let actual = [bytes[0], bytes[1]];
    if actual != hfb.version_bytes() {
        return Err(format!(
            "Device version {} differs from HFB version {}",
            version_text(actual),
            hfb.version()
        ));
    }
    Ok(())
}

fn wait_for_update_interface(
    platform: &mut impl Platform,
    log: &mut Log,
) -> Result<String, String> {
    platform.sleep(SETTLE_WAIT);
    let mut waited = Duration::ZERO;
    let mut guided = false;
    loop {
        let reason = match update_interface(&platform.enumerate()) {
            Ok(path) => {
                log.record_event("interface_found");
                return Ok(path);
            }
            Err(reason) => reason,
        };
        if waited >= REPLUG_WAIT {
            log.record_event("interface_not_found");
            return Err(format!(
                "Timed out waiting for the update interface. Nothing was sent: {reason}"
            ));
        }
        if !guided {
            println!(
                "Update interface not found. Replug the USB cable (waiting up to {} seconds)",
                REPLUG_WAIT.as_secs()
            );
            guided = true;
        }
        platform.sleep(POLL_INTERVAL);
        waited += POLL_INTERVAL;
    }
}

fn enter_with(platform: &mut impl Platform, hfb: &VerifiedHfb) -> Result<(), Abort> {
    let target = precheck(platform, Stage::EnterNotSent)?;
    match target.state {
        DeviceState::StockOrUpdate => enter_from_stock(platform, hfb, &target),
        DeviceState::QmkLikely => enter_from_qmk(platform, hfb, &target),
        DeviceState::Undetermined => Err(abort_at(Stage::EnterNotSent)(UNDETERMINED.to_owned())),
    }
}

fn enter_from_stock(
    platform: &mut impl Platform,
    hfb: &VerifiedHfb,
    target: &Target,
) -> Result<(), Abort> {
    let path = vendor_interface_path(target)
        .map_err(|error| abort_at(Stage::EnterNotSent)(error.to_string()))?;
    let kind = hfb.kind();
    confirm(
        platform,
        Stage::EnterNotSent,
        &format!(
            "Switching the HHKB to update mode (HFB to write: {kind}). Checks the device version, then sends the mode-change report. Does not write the firmware. {}",
            kind_warning(kind)
        ),
        enter_phrase(kind),
    )?;
    let (mut log, mut device) = open_session(platform, "enter", path, Stage::EnterNotSent)?;

    let query_only = abort_at(Stage::QueryOnly);
    let response = exchange(
        &mut device,
        &mut log,
        Step::KeyboardInfo,
        &frame_for(Step::KeyboardInfo, hfb),
        VERSION_QUERY_TIMEOUT,
    )
    .map_err(|error: ExchangeError| query_only(error.to_string()))?;
    check_version(&response, hfb).map_err(&query_only)?;
    println!("Device version matches HFB version {}", hfb.version());

    let attempted = abort_at(Stage::ModeChangeAttempted);
    exchange(
        &mut device,
        &mut log,
        Step::ModeChange,
        &frame_for(Step::ModeChange, hfb),
        RESPONSE_TIMEOUT,
    )
    .map_err(|error| attempted(error.to_string()))?;
    drop(device);
    finish_enter(platform, &mut log, Stage::ModeChangeAttempted)
}

fn enter_from_qmk(
    platform: &mut impl Platform,
    hfb: &VerifiedHfb,
    target: &Target,
) -> Result<(), Abort> {
    let path = qmk_interface_path(target)
        .map_err(|error| abort_at(Stage::QmkEnterNotSent)(error.to_string()))?;
    let kind = hfb.kind();
    let plan = if kind.is_qmk() {
        "Sends the mode-change report (e0). "
    } else {
        "Sends the mode-change report that reverts to stock firmware (f0). VIA settings will be erased. If f0 is refused, sends a VIA query (01), and sends e0 only if there is no answer. "
    };
    confirm(
        platform,
        Stage::QmkEnterNotSent,
        &format!(
            "QMK appears to be running (HFB to write: {kind}). {plan}Does not write the firmware. {}",
            kind_warning(kind)
        ),
        enter_phrase(kind),
    )?;
    let (mut log, mut device) = open_session(platform, "enter", path, Stage::QmkEnterNotSent)?;

    let stage = if kind.is_qmk() {
        qmk_enter_with_e0(&mut device, &mut log)?
    } else {
        qmk_enter_for_stock(&mut device, &mut log)?
    };
    drop(device);
    finish_enter(platform, &mut log, stage)
}

fn qmk_enter_with_e0(device: &mut impl ReportChannel, log: &mut Log) -> Result<Stage, Abort> {
    let stage = Stage::QmkModeChangeAttempted { command: 0xe0 };
    qmk_mode_change(device, log, RESPONSE_TIMEOUT)
        .map_err(|error| abort_at(stage)(error.to_string()))?;
    Ok(stage)
}

fn qmk_enter_for_stock(device: &mut impl ReportChannel, log: &mut Log) -> Result<Stage, Abort> {
    let stage = Stage::QmkModeChangeAttempted { command: 0xf0 };
    let answer = qmk_stock_revert(device, log, RESPONSE_TIMEOUT)
        .map_err(|error| abort_at(stage)(error.to_string()))?;
    if answer == RevertAnswer::Accepted {
        return Ok(stage);
    }
    println!("f0 was refused. Sending a VIA query");
    let refused = abort_at(Stage::QmkRevertRefused);
    let answered = qmk_via_answers(device, log, VIA_QUERY_TIMEOUT)
        .map_err(|error| refused(error.to_string()))?;
    if answered {
        return Err(refused(
            "Not switching: f0 was refused and VIA answered the query".to_owned(),
        ));
    }
    qmk_enter_with_e0(device, log)
}

fn finish_enter(platform: &mut impl Platform, log: &mut Log, stage: Stage) -> Result<(), Abort> {
    let attempted = abort_at(stage);
    println!(
        "Mode-change report acknowledged. Waiting {} seconds for re-enumeration",
        SETTLE_WAIT.as_secs()
    );

    let new_path = wait_for_update_interface(platform, log).map_err(&attempted)?;
    let reopened = platform.open(&new_path).map_err(|reason| {
        attempted(format!(
            "Cannot open the re-enumerated interface. Nothing was sent: {reason}"
        ))
    })?;
    drop(reopened);
    log.record_event("interface_opened");
    println!("Opened the re-enumerated interface. The firmware was not sent");
    println!("Next steps:");
    println!(
        "  1. Confirm that the HHKB cannot type (the USB listing cannot tell the states apart)"
    );
    println!("  2. Write the firmware: caffeinate -i cargo run -p hhkb -- flash write <HFB path>");
    println!("     The HHKB cannot type, so enter the confirmation on another keyboard");
    Ok(())
}

fn write_with(platform: &mut impl Platform, hfb: &VerifiedHfb) -> Result<(), Abort> {
    let not_sent = abort_at(Stage::WriteNotSent);
    let target = precheck(platform, Stage::WriteNotSent)?;
    match target.state {
        DeviceState::StockOrUpdate => {}
        DeviceState::QmkLikely => {
            return Err(not_sent(
                "Not writing: QMK appears to be running. Run hhkb flash enter first to switch to update mode"
                    .to_owned(),
            ));
        }
        DeviceState::Undetermined => return Err(not_sent(UNDETERMINED.to_owned())),
    }
    let path = vendor_interface_path(&target).map_err(|error| not_sent(error.to_string()))?;
    let kind = hfb.kind();
    confirm(
        platform,
        Stage::WriteNotSent,
        &format!(
            "Writing the firmware (HFB to write: {kind}). Confirm that the HHKB cannot type (it is likely in update mode). Do not proceed if it can type. Enter the confirmation on another keyboard. {}",
            kind_warning(kind)
        ),
        write_phrase(kind),
    )?;
    let (mut log, mut device) = open_session(platform, "write", path, Stage::WriteNotSent)?;

    transfer(
        &mut device,
        &mut log,
        hfb,
        RESPONSE_TIMEOUT,
        &mut |sent, total| {
            if sent % PROGRESS_INTERVAL == 0 || sent == total {
                println!("Sent {sent}/{total}");
            }
        },
    )
    .map_err(|failure| {
        let stage = match failure.error.step {
            Step::End => Stage::EndAttempted,
            _ => Stage::TransferAttempted {
                last_acked: failure.last_acked,
                total: hfb.packet_count(),
            },
        };
        Abort {
            stage,
            reason: failure.error.to_string(),
        }
    })?;
    println!("End report acknowledged");
    if kind.is_qmk() {
        println!(
            "Next: check the USB listing (hhkb list) and key input. Do not start the official tool while QMK is running"
        );
    } else {
        println!("Next: check the USB listing (hhkb list), key input, then hhkb status");
    }
    Ok(())
}

struct SystemPlatform {
    api: HidApi,
}

impl Platform for SystemPlatform {
    type Device = SystemDevice;

    fn official_tool_running(&self) -> Result<bool, String> {
        Pgrep.official_tool_running()
    }

    fn enumerate(&mut self) -> Vec<HidEntry> {
        if let Err(error) = self.api.refresh_devices() {
            eprintln!("Warning: cannot refresh the device list: {error}");
            return Vec::new();
        }
        self.api.device_list().map(HidEntry::from).collect()
    }

    fn open(&mut self, path: &str) -> Result<SystemDevice, String> {
        open_device(&self.api, path)
    }

    fn ask(&mut self, prompt: &str) -> Result<String, String> {
        print!("{prompt}");
        io::stdout().flush().map_err(|error| error.to_string())?;
        let mut line = String::new();
        io::stdin()
            .lock()
            .read_line(&mut line)
            .map_err(|error| error.to_string())?;
        Ok(line)
    }

    fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }

    fn open_log(&mut self, stage: &str) -> Result<Box<dyn Write>, String> {
        create_log_file(Path::new(LOG_PARENT), stage)
            .map(|file| Box::new(file) as Box<dyn Write>)
            .map_err(|error| {
                format!("Aborted: cannot create the log. Run from the repository root: {error}")
            })
    }
}

fn system_platform(stage: Stage) -> Result<SystemPlatform, Abort> {
    HidApi::new()
        .map(|api| SystemPlatform { api })
        .map_err(|error| Abort {
            stage,
            reason: format!("Failed to initialize HID: {error}"),
        })
}

pub fn enter(hfb: &VerifiedHfb) -> Result<(), Abort> {
    enter_with(&mut system_platform(Stage::EnterNotSent)?, hfb)
}

pub fn write(hfb: &VerifiedHfb) -> Result<(), Abort> {
    write_with(&mut system_platform(Stage::WriteNotSent)?, hfb)
}

#[cfg(test)]
pub(crate) mod testing {
    use super::image::testing::{sha256_hex, synthetic};
    use super::{HfbKind, VerifiedHfb};

    pub(crate) fn synthetic_bytes() -> Vec<u8> {
        synthetic()
    }

    pub(crate) fn verified_synthetic() -> VerifiedHfb {
        verified_synthetic_as(HfbKind::Stock)
    }

    pub(crate) fn verified_synthetic_as(kind: HfbKind) -> VerifiedHfb {
        let data = synthetic();
        let sha256 = sha256_hex(&data);
        VerifiedHfb::verify_against(data, &[(sha256.as_str(), kind)]).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::{HashMap, VecDeque};
    use std::rc::Rc;

    use super::image::testing::synthetic;
    use super::protocol::testing::correct_reply;
    use super::testing::{verified_synthetic, verified_synthetic_as};
    use super::*;
    use crate::device::{
        PRODUCT_ID, QMK_USAGE_PAGE, REPORT_SIZE, Transport, VENDOR_ID, VENDOR_USAGE_PAGE,
    };

    const SERIAL_MARKER: u8 = 0xcd;
    const ENTER_PHRASE: &str = enter_phrase(HfbKind::Stock);
    const WRITE_PHRASE: &str = write_phrase(HfbKind::Stock);
    const VIA_PROBE_KEY: u8 = 0x00;

    fn hfb() -> VerifiedHfb {
        verified_synthetic()
    }

    fn info_reply(version: [u8; 2]) -> Vec<u8> {
        let mut reply = vec![SERIAL_MARKER; REPORT_SIZE];
        reply[..6].copy_from_slice(&[0x55, 0x55, 0x02, 0x00, 0x00, 57]);
        reply[48..50].copy_from_slice(&version);
        reply
    }

    #[derive(Clone)]
    enum Reply {
        Bytes(Vec<u8>),
        Nothing,
    }

    #[derive(Default)]
    struct Shared {
        sent: Vec<Vec<u8>>,
    }

    struct MockDevice {
        shared: Rc<RefCell<Shared>>,
        replies: HashMap<u8, Reply>,
    }

    impl ReportChannel for MockDevice {
        fn send(&mut self, report: &[u8]) -> Result<(), String> {
            self.shared.borrow_mut().sent.push(report.to_vec());
            Ok(())
        }

        fn receive(&mut self, _timeout: Duration) -> Result<Option<Vec<u8>>, String> {
            let shared = self.shared.borrow();
            let frame = shared.sent.last().expect("no receive before a send");
            match self.replies.get(&frame[2]) {
                Some(Reply::Bytes(bytes)) => Ok(Some(bytes.clone())),
                Some(Reply::Nothing) => Ok(None),
                None => Ok(Some(correct_reply(frame))),
            }
        }
    }

    #[derive(Clone, Default)]
    struct SharedLog(Rc<RefCell<Vec<u8>>>);

    impl Write for SharedLog {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    struct MockPlatform {
        tool: Result<bool, String>,
        scans: VecDeque<Vec<HidEntry>>,
        last_scan: Vec<HidEntry>,
        answer: Result<String, String>,
        replies: HashMap<u8, Reply>,
        shared: Rc<RefCell<Shared>>,
        opened: Vec<String>,
        open_failures: VecDeque<bool>,
        prompts: Vec<String>,
        slept: Duration,
        sleep_before_scans: Vec<Duration>,
        log_fails: bool,
        logs_opened: usize,
        log: SharedLog,
    }

    impl MockPlatform {
        fn new(scans: Vec<Vec<HidEntry>>, answer: &str) -> Self {
            Self {
                tool: Ok(false),
                last_scan: scans.last().cloned().unwrap_or_default(),
                scans: scans.into(),
                answer: Ok(format!("{answer}\n")),
                replies: HashMap::from([(0x02, Reply::Bytes(info_reply([0x04, 0x08])))]),
                shared: Rc::default(),
                opened: Vec::new(),
                open_failures: VecDeque::new(),
                prompts: Vec::new(),
                slept: Duration::ZERO,
                sleep_before_scans: Vec::new(),
                log_fails: false,
                logs_opened: 0,
                log: SharedLog::default(),
            }
        }

        fn commands(&self) -> Vec<u8> {
            self.shared.borrow().sent.iter().map(|f| f[2]).collect()
        }

        fn log_text(&self) -> String {
            String::from_utf8(self.log.0.borrow().clone()).unwrap()
        }
    }

    impl Platform for MockPlatform {
        type Device = MockDevice;

        fn official_tool_running(&self) -> Result<bool, String> {
            self.tool.clone()
        }

        fn enumerate(&mut self) -> Vec<HidEntry> {
            self.sleep_before_scans.push(self.slept);
            self.scans
                .pop_front()
                .unwrap_or_else(|| self.last_scan.clone())
        }

        fn open(&mut self, path: &str) -> Result<MockDevice, String> {
            self.opened.push(path.to_owned());
            if self.open_failures.pop_front().unwrap_or(false) {
                return Err("cannot open".to_owned());
            }
            Ok(MockDevice {
                shared: Rc::clone(&self.shared),
                replies: self.replies.clone(),
            })
        }

        fn ask(&mut self, prompt: &str) -> Result<String, String> {
            self.prompts.push(prompt.to_owned());
            self.answer.clone()
        }

        fn sleep(&mut self, duration: Duration) {
            self.slept += duration;
        }

        fn open_log(&mut self, _stage: &str) -> Result<Box<dyn Write>, String> {
            if self.log_fails {
                return Err("cannot create log".to_owned());
            }
            self.logs_opened += 1;
            Ok(Box::new(self.log.clone()))
        }
    }

    fn entry(path: &str, transport: Transport, page: u16, usage: u16) -> HidEntry {
        HidEntry {
            path: path.to_owned(),
            vendor_id: VENDOR_ID,
            product_id: PRODUCT_ID,
            transport,
            product: Some("HHKB-Hybrid".to_owned()),
            release: 0x0001,
            usage_page: page,
            usage,
        }
    }

    fn keyboard(prefix: &str) -> Vec<HidEntry> {
        vec![
            entry(&format!("{prefix}-kbd"), Transport::Usb, 0x0001, 0x0006),
            entry(
                &format!("{prefix}-vendor"),
                Transport::Usb,
                VENDOR_USAGE_PAGE,
                0x0001,
            ),
        ]
    }

    fn enter_platform() -> MockPlatform {
        MockPlatform::new(vec![keyboard("a"), keyboard("b")], ENTER_PHRASE)
    }

    fn write_platform() -> MockPlatform {
        MockPlatform::new(vec![keyboard("a")], WRITE_PHRASE)
    }

    struct NoDevices;
    impl HidEnumerator for NoDevices {
        fn enumerate(&self) -> Vec<HidEntry> {
            Vec::new()
        }
    }

    struct OneDevice;
    impl HidEnumerator for OneDevice {
        fn enumerate(&self) -> Vec<HidEntry> {
            keyboard("a")
        }
    }

    #[test]
    fn plan_reports_packet_count_and_connection() {
        let plan = plan(&hfb(), &OneDevice);
        assert_eq!(plan.packets, 4960);
        assert!(plan.connection.is_ok());
    }

    #[test]
    fn plan_reports_a_missing_keyboard_without_failing() {
        let plan = plan(&hfb(), &NoDevices);
        assert_eq!(plan.connection, Err(SelectError::NotFound));
    }

    #[test]
    fn enter_sends_the_version_query_then_mode_change_only() {
        let mut platform = enter_platform();
        enter_with(&mut platform, &hfb()).unwrap();
        assert_eq!(platform.commands(), [0x02, 0xe0]);
    }

    #[test]
    fn enter_opens_the_reselected_interface_without_sending() {
        let mut platform = enter_platform();
        enter_with(&mut platform, &hfb()).unwrap();
        assert_eq!(platform.opened, ["a-vendor", "b-vendor"]);
        assert_eq!(platform.commands().len(), 2);
    }

    #[test]
    fn enter_waits_before_listing_devices_again() {
        let mut platform = enter_platform();
        enter_with(&mut platform, &hfb()).unwrap();
        assert_eq!(platform.sleep_before_scans, [Duration::ZERO, SETTLE_WAIT]);
    }

    #[test]
    fn official_tool_running_stops_enter_before_opening() {
        let mut platform = enter_platform();
        platform.tool = Ok(true);
        let abort = enter_with(&mut platform, &hfb()).unwrap_err();
        assert_eq!(abort.stage, Stage::EnterNotSent);
        assert!(platform.opened.is_empty());
        assert!(platform.prompts.is_empty());
    }

    #[test]
    fn failed_tool_check_stops_enter_before_opening() {
        let mut platform = enter_platform();
        platform.tool = Err("pgrep failed".to_owned());
        assert!(enter_with(&mut platform, &hfb()).is_err());
        assert!(platform.opened.is_empty());
    }

    #[test]
    fn no_keyboard_stops_enter_before_opening() {
        let mut platform = MockPlatform::new(vec![vec![]], ENTER_PHRASE);
        assert!(enter_with(&mut platform, &hfb()).is_err());
        assert!(platform.opened.is_empty());
    }

    #[test]
    fn two_keyboards_stop_enter_before_opening() {
        let mut both = keyboard("a");
        both.extend(keyboard("b"));
        let mut platform = MockPlatform::new(vec![both], ENTER_PHRASE);
        assert!(enter_with(&mut platform, &hfb()).is_err());
        assert!(platform.opened.is_empty());
    }

    #[test]
    fn shared_vendor_interface_stops_enter_before_opening() {
        let mixed = vec![
            entry("mixed", Transport::Usb, VENDOR_USAGE_PAGE, 0x0001),
            entry("mixed", Transport::Usb, 0x0001, 0x0006),
        ];
        let mut platform = MockPlatform::new(vec![mixed], ENTER_PHRASE);
        assert!(enter_with(&mut platform, &hfb()).is_err());
        assert!(platform.opened.is_empty());
    }

    #[test]
    fn wrong_confirmation_stops_enter_before_opening() {
        for answer in [
            "",
            "\n",
            "yes\n",
            "ENTER STOCK\n",
            "enter stock \n",
            "enter\n",
            "enter default\n",
            "write stock\n",
        ] {
            let mut platform = enter_platform();
            platform.answer = Ok(answer.to_owned());
            let abort = enter_with(&mut platform, &hfb()).unwrap_err();
            assert_eq!(abort.stage, Stage::EnterNotSent, "{answer:?}");
            assert!(platform.opened.is_empty(), "{answer:?}");
        }
    }

    #[test]
    fn failed_confirmation_read_stops_enter() {
        let mut platform = enter_platform();
        platform.answer = Err("cannot read".to_owned());
        assert!(enter_with(&mut platform, &hfb()).is_err());
        assert!(platform.opened.is_empty());
    }

    #[test]
    fn the_prompt_contains_its_phrase() {
        let mut platform = enter_platform();
        enter_with(&mut platform, &hfb()).unwrap();
        assert!(platform.prompts[0].contains(ENTER_PHRASE));
    }

    #[test]
    fn failing_log_stops_enter_before_opening() {
        let mut platform = enter_platform();
        platform.log_fails = true;
        assert!(enter_with(&mut platform, &hfb()).is_err());
        assert!(platform.opened.is_empty());
    }

    #[test]
    fn version_mismatch_stops_before_mode_change() {
        let mut platform = enter_platform();
        platform
            .replies
            .insert(0x02, Reply::Bytes(info_reply([0x04, 0x09])));
        let abort = enter_with(&mut platform, &hfb()).unwrap_err();
        assert_eq!(abort.stage, Stage::QueryOnly);
        assert_eq!(platform.commands(), [0x02]);
    }

    #[test]
    fn missing_version_response_stops_before_mode_change() {
        let mut platform = enter_platform();
        platform.replies.insert(0x02, Reply::Nothing);
        let abort = enter_with(&mut platform, &hfb()).unwrap_err();
        assert_eq!(abort.stage, Stage::QueryOnly);
        assert_eq!(platform.commands(), [0x02]);
    }

    #[test]
    fn truncated_version_response_stops_before_mode_change() {
        let mut platform = enter_platform();
        let mut short = info_reply([0x04, 0x08]);
        short.truncate(48);
        platform.replies.insert(0x02, Reply::Bytes(short));
        let abort = enter_with(&mut platform, &hfb()).unwrap_err();
        assert_eq!(abort.stage, Stage::QueryOnly);
        assert_eq!(platform.commands(), [0x02]);
    }

    #[test]
    fn version_error_does_not_show_other_response_bytes() {
        let mut platform = enter_platform();
        platform
            .replies
            .insert(0x02, Reply::Bytes(info_reply([0x04, 0x09])));
        let abort = enter_with(&mut platform, &hfb()).unwrap_err();
        assert!(!abort.to_string().to_lowercase().contains("cd"));
    }

    #[test]
    fn failed_mode_change_reports_the_attempted_stage() {
        let mut platform = enter_platform();
        platform.replies.insert(0xe0, Reply::Nothing);
        let abort = enter_with(&mut platform, &hfb()).unwrap_err();
        assert_eq!(abort.stage, Stage::ModeChangeAttempted);
        assert_eq!(platform.commands(), [0x02, 0xe0]);
        assert_eq!(platform.opened, ["a-vendor"]);
    }

    #[test]
    fn missing_interface_after_mode_change_ends_without_opening_or_sending() {
        let mut platform = MockPlatform::new(vec![keyboard("a"), vec![]], ENTER_PHRASE);
        let abort = enter_with(&mut platform, &hfb()).unwrap_err();
        assert_eq!(abort.stage, Stage::ModeChangeAttempted);
        assert_eq!(platform.opened, ["a-vendor"]);
        assert_eq!(platform.commands(), [0x02, 0xe0]);
        assert_eq!(platform.slept, SETTLE_WAIT + REPLUG_WAIT);
    }

    #[test]
    fn interface_appearing_after_a_replug_is_used() {
        let mut platform = MockPlatform::new(
            vec![keyboard("a"), vec![], vec![], keyboard("c")],
            ENTER_PHRASE,
        );
        enter_with(&mut platform, &hfb()).unwrap();
        assert_eq!(platform.opened, ["a-vendor", "c-vendor"]);
    }

    #[test]
    fn unusable_interfaces_after_mode_change_are_not_opened() {
        let mut two_keyboards = keyboard("b");
        two_keyboards.extend(keyboard("c"));
        let cases = [
            vec![entry("c-kbd", Transport::Usb, 0x0001, 0x0006)],
            vec![
                entry("mixed", Transport::Usb, VENDOR_USAGE_PAGE, 0x0001),
                entry("mixed", Transport::Usb, 0x0001, 0x0006),
            ],
            vec![entry("ble", Transport::Other, VENDOR_USAGE_PAGE, 0x0001)],
            two_keyboards,
        ];
        for scan in cases {
            let mut platform = MockPlatform::new(vec![keyboard("a"), scan.clone()], ENTER_PHRASE);
            assert!(enter_with(&mut platform, &hfb()).is_err(), "{scan:?}");
            assert_eq!(platform.opened, ["a-vendor"], "{scan:?}");
            assert_eq!(platform.commands(), [0x02, 0xe0], "{scan:?}");
        }
    }

    #[test]
    fn unopenable_interface_after_mode_change_is_reported() {
        let mut platform = enter_platform();
        platform.open_failures = VecDeque::from([false, true]);
        let abort = enter_with(&mut platform, &hfb()).unwrap_err();
        assert_eq!(abort.stage, Stage::ModeChangeAttempted);
        assert_eq!(platform.commands(), [0x02, 0xe0]);
    }

    #[test]
    fn write_sends_start_body_and_end_without_a_version_query() {
        let mut platform = write_platform();
        write_with(&mut platform, &hfb()).unwrap();
        let commands = platform.commands();
        assert_eq!(commands.len(), 4962);
        assert_eq!(commands[0], 0xe1);
        assert_eq!(commands[4961], 0xe3);
        assert!(!commands.contains(&0x02));
        assert!(!commands.contains(&0xe0));
    }

    #[test]
    fn official_tool_running_stops_write_before_opening() {
        let mut platform = write_platform();
        platform.tool = Ok(true);
        let abort = write_with(&mut platform, &hfb()).unwrap_err();
        assert_eq!(abort.stage, Stage::WriteNotSent);
        assert!(platform.opened.is_empty());
    }

    #[test]
    fn no_keyboard_stops_write_before_opening() {
        let mut platform = MockPlatform::new(vec![vec![]], WRITE_PHRASE);
        let abort = write_with(&mut platform, &hfb()).unwrap_err();
        assert_eq!(abort.stage, Stage::WriteNotSent);
        assert!(platform.opened.is_empty());
    }

    #[test]
    fn unopenable_interface_stops_write_without_sending() {
        let mut platform = write_platform();
        platform.open_failures = VecDeque::from([true]);
        let abort = write_with(&mut platform, &hfb()).unwrap_err();
        assert_eq!(abort.stage, Stage::WriteNotSent);
        assert!(platform.commands().is_empty());
    }

    #[test]
    fn failed_start_reports_the_transfer_stage() {
        let mut platform = write_platform();
        platform.replies.insert(0xe1, Reply::Nothing);
        let abort = write_with(&mut platform, &hfb()).unwrap_err();
        assert_eq!(
            abort.stage,
            Stage::TransferAttempted {
                last_acked: None,
                total: 4960
            }
        );
        assert_eq!(platform.commands(), [0xe1]);
    }

    #[test]
    fn failed_end_reports_the_end_stage() {
        let mut platform = write_platform();
        platform.replies.insert(0xe3, Reply::Nothing);
        let abort = write_with(&mut platform, &hfb()).unwrap_err();
        assert_eq!(abort.stage, Stage::EndAttempted);
    }

    #[test]
    fn write_log_holds_packet_numbers_but_no_body_data() {
        let mut platform = write_platform();
        write_with(&mut platform, &hfb()).unwrap();
        assert_eq!(platform.logs_opened, 1);
        let text = platform.log_text();
        assert_eq!(text.lines().count(), 2 * 4962);
        let data = synthetic();
        let body_marker: String = data[1000..1016]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert!(!text.contains(&body_marker));
        for line in text.lines().filter(|line| line.contains(r#""dir":"out""#)) {
            assert!(line.len() < 120, "{line}");
        }
        assert!(text.contains(r#""packet":4959"#));
    }

    #[test]
    fn enter_log_records_the_full_responses() {
        let mut platform = enter_platform();
        enter_with(&mut platform, &hfb()).unwrap();
        let info: String = info_reply([0x04, 0x08])
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert!(platform.log_text().contains(&info));
    }

    fn qmk_entry(path: &str, page: u16, usage: u16) -> HidEntry {
        HidEntry {
            product: Some("HHKB Hybrid QMK".to_owned()),
            ..entry(path, Transport::Usb, page, usage)
        }
    }

    fn qmk_keyboard() -> Vec<HidEntry> {
        vec![
            qmk_entry("q-kbd", 0x0001, 0x0006),
            qmk_entry("q-raw", QMK_USAGE_PAGE, 0x0061),
        ]
    }

    fn qmk_reply(head: &[u8]) -> Reply {
        let mut bytes = vec![0; 32];
        bytes[..head.len()].copy_from_slice(head);
        Reply::Bytes(bytes)
    }

    fn qmk_platform(kind: HfbKind) -> MockPlatform {
        let mut platform =
            MockPlatform::new(vec![qmk_keyboard(), keyboard("u")], enter_phrase(kind));
        platform.replies = HashMap::from([
            (0xe0, qmk_reply(&[0x55, 0x55, 0xe0, 0x00, 0x00])),
            (0xf0, qmk_reply(&[0x55, 0x55, 0xf0, 0x00, 0x00])),
            (VIA_PROBE_KEY, Reply::Nothing),
        ]);
        platform
    }

    fn reject_f0(platform: &mut MockPlatform) {
        platform
            .replies
            .insert(0xf0, qmk_reply(&[0x55, 0x55, 0xf0, 0x01, 0x00]));
    }

    const QMK_KINDS: [HfbKind; 2] = [HfbKind::QmkDefault, HfbKind::QmkVia];
    const ALL_KINDS: [HfbKind; 3] = [HfbKind::Stock, HfbKind::QmkDefault, HfbKind::QmkVia];

    #[test]
    fn qmk_to_qmk_sends_only_e0_as_32_bytes_to_the_raw_interface() {
        for kind in QMK_KINDS {
            let mut platform = qmk_platform(kind);
            enter_with(&mut platform, &verified_synthetic_as(kind)).unwrap();
            let shared = platform.shared.borrow();
            assert_eq!(shared.sent.len(), 1, "{kind:?}");
            assert_eq!(shared.sent[0].len(), 32, "{kind:?}");
            assert_eq!(shared.sent[0][..3], [0xaa, 0xaa, 0xe0], "{kind:?}");
            assert_eq!(platform.opened[0], "q-raw", "{kind:?}");
        }
    }

    #[test]
    fn qmk_to_stock_sends_f0_first_and_stops_when_accepted() {
        let mut platform = qmk_platform(HfbKind::Stock);
        enter_with(&mut platform, &hfb()).unwrap();
        assert_eq!(platform.commands(), [0xf0]);
        assert_eq!(platform.shared.borrow().sent[0].len(), 32);
        assert_eq!(platform.opened, ["q-raw", "u-vendor"]);
    }

    #[test]
    fn rejected_f0_without_a_via_answer_falls_back_to_e0() {
        let mut platform = qmk_platform(HfbKind::Stock);
        reject_f0(&mut platform);
        enter_with(&mut platform, &hfb()).unwrap();
        let shared = platform.shared.borrow();
        assert_eq!(shared.sent.len(), 3);
        assert_eq!(shared.sent[0][2], 0xf0);
        assert_eq!(shared.sent[1][0], 0x01);
        assert_eq!(shared.sent[2][2], 0xe0);
        assert!(shared.sent.iter().all(|frame| frame.len() == 32));
    }

    #[test]
    fn rejected_f0_with_any_via_answer_does_not_enter() {
        for head in [&[0x01u8, 0x00, 0x0c][..], &[0xff][..]] {
            let mut platform = qmk_platform(HfbKind::Stock);
            reject_f0(&mut platform);
            platform.replies.insert(VIA_PROBE_KEY, qmk_reply(head));
            let abort = enter_with(&mut platform, &hfb()).unwrap_err();
            assert_eq!(abort.stage, Stage::QmkRevertRefused, "{head:?}");
            assert_eq!(platform.commands().len(), 2, "{head:?}");
            assert!(!platform.commands().contains(&0xe0), "{head:?}");
            assert_eq!(platform.opened, ["q-raw"], "{head:?}");
        }
    }

    #[test]
    fn missing_qmk_answers_report_the_attempted_stage_and_show_the_esc_procedure() {
        for (kind, command) in [
            (HfbKind::QmkDefault, 0xe0),
            (HfbKind::QmkVia, 0xe0),
            (HfbKind::Stock, 0xf0),
        ] {
            let mut platform = qmk_platform(kind);
            platform.replies.insert(command, Reply::Nothing);
            let abort = enter_with(&mut platform, &verified_synthetic_as(kind)).unwrap_err();
            assert_eq!(abort.stage, Stage::QmkModeChangeAttempted { command });
            assert!(abort.guidance().contains("holding Esc"));
            assert!(abort.guidance().contains("unverified"));
            assert_eq!(platform.commands(), [command], "{kind:?}");
        }
    }

    #[test]
    fn raw_interface_shared_with_another_usage_is_not_opened() {
        let mixed = vec![
            qmk_entry("mixed", QMK_USAGE_PAGE, 0x0061),
            qmk_entry("mixed", 0x0001, 0x0006),
        ];
        let mut platform = MockPlatform::new(vec![mixed], enter_phrase(HfbKind::QmkVia));
        let abort = enter_with(&mut platform, &verified_synthetic_as(HfbKind::QmkVia)).unwrap_err();
        assert_eq!(abort.stage, Stage::QmkEnterNotSent);
        assert!(platform.opened.is_empty());
    }

    #[test]
    fn waiting_after_qmk_enter_does_not_open_while_the_raw_interface_is_visible() {
        let kind = HfbKind::QmkDefault;
        let mut platform = qmk_platform(kind);
        platform.scans = VecDeque::from([qmk_keyboard(), qmk_keyboard(), qmk_keyboard()]);
        platform.last_scan = qmk_keyboard();
        let abort = enter_with(&mut platform, &verified_synthetic_as(kind)).unwrap_err();
        assert_eq!(abort.stage, Stage::QmkModeChangeAttempted { command: 0xe0 });
        assert_eq!(platform.opened, ["q-raw"]);
        assert_eq!(platform.commands(), [0xe0]);
        assert_eq!(platform.slept, SETTLE_WAIT + REPLUG_WAIT);
    }

    #[test]
    fn update_interface_after_the_raw_interface_disappears_is_used() {
        let kind = HfbKind::QmkDefault;
        let mut platform = qmk_platform(kind);
        platform.scans = VecDeque::from([qmk_keyboard(), qmk_keyboard(), vec![], keyboard("u")]);
        enter_with(&mut platform, &verified_synthetic_as(kind)).unwrap();
        assert_eq!(platform.opened, ["q-raw", "u-vendor"]);
        assert_eq!(platform.commands(), [0xe0]);
    }

    fn undetermined_scans() -> [Vec<HidEntry>; 3] {
        [
            vec![entry("raw", Transport::Usb, QMK_USAGE_PAGE, 0x0061)],
            vec![qmk_entry("vendor", VENDOR_USAGE_PAGE, 0x0001)],
            vec![qmk_entry("kbd", 0x0001, 0x0006)],
        ]
    }

    #[test]
    fn undetermined_state_stops_enter_before_asking() {
        for scan in undetermined_scans() {
            let mut platform = MockPlatform::new(vec![scan.clone()], ENTER_PHRASE);
            let abort = enter_with(&mut platform, &hfb()).unwrap_err();
            assert_eq!(abort.stage, Stage::EnterNotSent, "{scan:?}");
            assert!(platform.opened.is_empty(), "{scan:?}");
            assert!(platform.prompts.is_empty(), "{scan:?}");
        }
    }

    #[test]
    fn undetermined_state_after_mode_change_is_not_opened() {
        for scan in undetermined_scans() {
            let mut platform = MockPlatform::new(vec![keyboard("a"), scan.clone()], ENTER_PHRASE);
            assert!(enter_with(&mut platform, &hfb()).is_err(), "{scan:?}");
            assert_eq!(platform.opened, ["a-vendor"], "{scan:?}");
        }
    }

    #[test]
    fn write_sends_nothing_while_qmk_is_running() {
        for kind in ALL_KINDS {
            let mut platform = MockPlatform::new(vec![qmk_keyboard()], write_phrase(kind));
            let abort = write_with(&mut platform, &verified_synthetic_as(kind)).unwrap_err();
            assert_eq!(abort.stage, Stage::WriteNotSent, "{kind:?}");
            assert!(abort.reason.contains("hhkb flash enter"), "{kind:?}");
            assert!(platform.opened.is_empty(), "{kind:?}");
            assert!(platform.prompts.is_empty(), "{kind:?}");
        }
    }

    #[test]
    fn write_sends_nothing_when_the_state_is_undetermined() {
        for scan in undetermined_scans() {
            let mut platform = MockPlatform::new(vec![scan.clone()], WRITE_PHRASE);
            let abort = write_with(&mut platform, &hfb()).unwrap_err();
            assert_eq!(abort.stage, Stage::WriteNotSent, "{scan:?}");
            assert!(platform.opened.is_empty(), "{scan:?}");
        }
    }

    #[test]
    fn stock_path_sends_the_same_commands_for_every_kind() {
        for kind in ALL_KINDS {
            let hfb = verified_synthetic_as(kind);
            let mut platform =
                MockPlatform::new(vec![keyboard("a"), keyboard("b")], enter_phrase(kind));
            enter_with(&mut platform, &hfb).unwrap();
            assert_eq!(platform.commands(), [0x02, 0xe0], "{kind:?}");
            assert!(
                platform.shared.borrow().sent.iter().all(|f| f.len() == 64),
                "{kind:?}"
            );

            let mut platform = MockPlatform::new(vec![keyboard("a")], write_phrase(kind));
            write_with(&mut platform, &hfb).unwrap();
            let shared = platform.shared.borrow();
            assert_eq!(shared.sent.len(), 4962, "{kind:?}");
            assert!(shared.sent.iter().all(|f| f.len() == 64), "{kind:?}");
        }
    }

    #[test]
    fn every_phrase_is_distinct_and_only_its_own_stage_and_kind_is_accepted() {
        let phrases: Vec<&str> = ALL_KINDS
            .iter()
            .flat_map(|kind| [enter_phrase(*kind), write_phrase(*kind)])
            .collect();
        for (index, phrase) in phrases.iter().enumerate() {
            assert!(!phrases[index + 1..].contains(phrase), "{phrase}");
        }
        for kind in ALL_KINDS {
            let hfb = verified_synthetic_as(kind);
            for phrase in &phrases {
                let mut platform = MockPlatform::new(vec![keyboard("a"), keyboard("b")], phrase);
                let entered = enter_with(&mut platform, &hfb);
                assert_eq!(entered.is_ok(), *phrase == enter_phrase(kind), "{phrase}");
                if entered.is_err() {
                    assert!(platform.commands().is_empty(), "{phrase}");
                }

                let mut platform = MockPlatform::new(vec![keyboard("a")], phrase);
                let written = write_with(&mut platform, &hfb);
                assert_eq!(written.is_ok(), *phrase == write_phrase(kind), "{phrase}");
                if written.is_err() {
                    assert!(platform.commands().is_empty(), "{phrase}");
                }

                let mut platform = qmk_platform(kind);
                platform.answer = Ok(format!("{phrase}\n"));
                let from_qmk = enter_with(&mut platform, &hfb);
                assert_eq!(from_qmk.is_ok(), *phrase == enter_phrase(kind), "{phrase}");
                if from_qmk.is_err() {
                    assert!(platform.commands().is_empty(), "{phrase}");
                }
            }
        }
    }

    #[test]
    fn prompts_name_the_kind_and_warn_only_for_qmk() {
        for kind in ALL_KINDS {
            let hfb = verified_synthetic_as(kind);
            let mut enter =
                MockPlatform::new(vec![keyboard("a"), keyboard("b")], enter_phrase(kind));
            enter_with(&mut enter, &hfb).unwrap();
            let mut write = MockPlatform::new(vec![keyboard("a")], write_phrase(kind));
            write_with(&mut write, &hfb).unwrap();
            for prompt in [&enter.prompts[0], &write.prompts[0]] {
                assert!(prompt.contains(&kind.to_string()), "{prompt}");
                assert_eq!(
                    prompt.contains("warranty") && prompt.contains("disassemble"),
                    kind != HfbKind::Stock,
                    "{prompt}"
                );
            }
        }
    }

    #[test]
    fn stock_prompt_from_qmk_says_via_settings_are_erased() {
        let mut platform = qmk_platform(HfbKind::Stock);
        enter_with(&mut platform, &hfb()).unwrap();
        assert!(platform.prompts[0].contains("VIA settings will be erased"));
    }

    #[test]
    fn rejected_confirmation_creates_no_log() {
        let mut platform = MockPlatform::new(vec![keyboard("a")], "no");
        assert!(enter_with(&mut platform, &hfb()).is_err());
        assert_eq!(platform.logs_opened, 0);
    }
}
