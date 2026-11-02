mod abort;
mod image;
mod log;
mod protocol;

use std::ffi::CString;
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::time::Duration;

use hidapi::{HidApi, HidDevice};
use log::{Log, create_log_file};
use protocol::{
    ExchangeError, Frame, RESPONSE_TIMEOUT, ReportChannel, Step, exchange, frame_for, transfer,
};

pub use abort::{Abort, Stage};
pub use image::{VerifiedHfb, VerifyError};

use crate::device::{HidEntry, HidEnumerator, SelectError, Target, select_target};
use crate::status::{
    Pgrep, ProcessCheck, REPORT_SIZE, StatusError, output_buffer, vendor_interface_path,
};

pub const ENTER_PHRASE: &str = "enter update mode";
pub const WRITE_PHRASE: &str = "write firmware";

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

fn update_interface(entries: &[HidEntry]) -> Result<String, String> {
    let target = select_target(entries).map_err(|error| error.to_string())?;
    vendor_interface_path(&target)
        .map(str::to_owned)
        .map_err(|error| error.to_string())
}

fn precheck(platform: &mut impl Platform, stage: Stage) -> Result<String, Abort> {
    let fail = abort_at(stage);
    let running = platform
        .official_tool_running()
        .map_err(|reason| fail(StatusError::ProcessCheckFailed(reason).to_string()))?;
    if running {
        return Err(fail(StatusError::OfficialToolRunning.to_string()));
    }
    update_interface(&platform.enumerate()).map_err(&fail)
}

fn confirm(
    platform: &mut impl Platform,
    stage: Stage,
    prompt: &str,
    phrase: &str,
) -> Result<(), Abort> {
    let fail = abort_at(stage);
    let line = platform
        .ask(&format!(
            "{prompt}\n進めるには次の文を入力してください: {phrase}\n> "
        ))
        .map_err(|reason| fail(format!("確認文を読めません: {reason}")))?;
    if line.trim_end_matches(['\n', '\r']) != phrase {
        return Err(fail("確認文が一致しないので中止しました".to_owned()));
    }
    Ok(())
}

fn version_text(bytes: [u8; 2]) -> String {
    if bytes.iter().all(|digit| *digit <= 9) {
        format!("A0.{}{}", bytes[0], bytes[1])
    } else {
        "（数字として読めません）".to_owned()
    }
}

fn check_version(response: &[u8], hfb: &VerifiedHfb) -> Result<(), String> {
    let bytes = response
        .get(APP_VERSION_OFFSET..APP_VERSION_OFFSET + 2)
        .ok_or_else(|| "応答から実機の版を読み取れません".to_owned())?;
    let actual = [bytes[0], bytes[1]];
    if actual != hfb.version_bytes() {
        return Err(format!(
            "実機の版 {} が HFB の版 {} と異なります",
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
                "更新用のインターフェースが見つからないまま待ち時間が切れました。送信していません: {reason}"
            ));
        }
        if !guided {
            println!(
                "更新用のインターフェースが見つかりません。USB を抜き差ししてください（最大 {} 秒待ちます）",
                REPLUG_WAIT.as_secs()
            );
            guided = true;
        }
        platform.sleep(POLL_INTERVAL);
        waited += POLL_INTERVAL;
    }
}

fn enter_with(platform: &mut impl Platform, hfb: &VerifiedHfb) -> Result<(), Abort> {
    let path = precheck(platform, Stage::EnterNotSent)?;
    confirm(
        platform,
        Stage::EnterNotSent,
        "HHKB を更新モードへ移行します。実機の版を確認した後、移行のレポートを送信します。本体は書き込みません。",
        ENTER_PHRASE,
    )?;
    let not_sent = abort_at(Stage::EnterNotSent);
    let mut log = Log::new(platform.open_log("enter").map_err(&not_sent)?);
    let mut device = platform
        .open(&path)
        .map_err(|reason| not_sent(format!("デバイスを開けません: {reason}")))?;

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
    println!("実機の版が HFB の版 {} と一致しました", hfb.version());

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
    println!(
        "移行のレポートの応答を確認しました。再列挙を {} 秒待ちます",
        SETTLE_WAIT.as_secs()
    );

    let new_path = wait_for_update_interface(platform, &mut log).map_err(&attempted)?;
    let reopened = platform.open(&new_path).map_err(|reason| {
        attempted(format!(
            "再列挙されたインターフェースを開けません。送信していません: {reason}"
        ))
    })?;
    drop(reopened);
    log.record_event("interface_opened");
    println!("再列挙されたインターフェースを開けました。本体は送信していません");
    println!("次の手順:");
    println!("  1. HHKB からキー入力できないことを確かめます（USB の表示では区別できません）");
    println!(
        "  2. caffeinate -i cargo run -p hhkb -- flash write <HFB のパス> で本体を書き込みます"
    );
    println!("     HHKB から入力できないので、確認文は別のキーボードで入力します");
    Ok(())
}

fn write_with(platform: &mut impl Platform, hfb: &VerifiedHfb) -> Result<(), Abort> {
    let path = precheck(platform, Stage::WriteNotSent)?;
    confirm(
        platform,
        Stage::WriteNotSent,
        "本体を書き込みます。HHKB からキー入力できないこと（更新モードの可能性が高いこと）を確かめてください。入力できるときは実行しないでください。確認文は別のキーボードで入力します。",
        WRITE_PHRASE,
    )?;
    let not_sent = abort_at(Stage::WriteNotSent);
    let mut log = Log::new(platform.open_log("write").map_err(&not_sent)?);
    let mut device = platform
        .open(&path)
        .map_err(|reason| not_sent(format!("デバイスを開けません: {reason}")))?;

    transfer(
        &mut device,
        &mut log,
        hfb,
        RESPONSE_TIMEOUT,
        &mut |sent, total| {
            if sent % PROGRESS_INTERVAL == 0 || sent == total {
                println!("送信 {sent}/{total}");
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
    println!("終了のレポートの応答を確認しました");
    println!("次の手順: USB の表示（hhkb list）、キー入力、hhkb status の順に確認します");
    Ok(())
}

struct SystemDevice(HidDevice);

impl ReportChannel for SystemDevice {
    fn send(&mut self, frame: &Frame) -> Result<(), String> {
        self.0
            .write(&output_buffer(frame))
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn receive(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, String> {
        let mut buffer = [0; REPORT_SIZE];
        let timeout_ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
        let size = self
            .0
            .read_timeout(&mut buffer, timeout_ms)
            .map_err(|error| error.to_string())?;
        Ok((size > 0).then(|| buffer[..size].to_vec()))
    }
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
            eprintln!("警告: デバイスの一覧を更新できません: {error}");
            return Vec::new();
        }
        self.api.device_list().map(HidEntry::from).collect()
    }

    fn open(&mut self, path: &str) -> Result<SystemDevice, String> {
        let path = CString::new(path).map_err(|error| error.to_string())?;
        #[cfg(target_os = "macos")]
        self.api.set_open_exclusive(false);
        self.api
            .open_path(&path)
            .map(SystemDevice)
            .map_err(|error| error.to_string())
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
                format!(
                    "ログを作れないので中止しました。リポジトリのルートで実行してください: {error}"
                )
            })
    }
}

fn system_platform(stage: Stage) -> Result<SystemPlatform, Abort> {
    HidApi::new()
        .map(|api| SystemPlatform { api })
        .map_err(|error| Abort {
            stage,
            reason: format!("HID の初期化に失敗しました: {error}"),
        })
}

pub fn enter(hfb: &VerifiedHfb) -> Result<(), Abort> {
    enter_with(&mut system_platform(Stage::EnterNotSent)?, hfb)
}

pub fn write(hfb: &VerifiedHfb) -> Result<(), Abort> {
    write_with(&mut system_platform(Stage::WriteNotSent)?, hfb)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::{HashMap, VecDeque};
    use std::rc::Rc;

    use super::image::testing::{sha256_hex, synthetic};
    use super::protocol::testing::correct_reply;
    use super::*;
    use crate::device::{PRODUCT_ID, Transport, VENDOR_ID, VENDOR_USAGE_PAGE};

    const SERIAL_MARKER: u8 = 0xcd;

    fn hfb() -> VerifiedHfb {
        let data = synthetic();
        let sha256 = sha256_hex(&data);
        VerifiedHfb::verify_against(data, &[sha256.as_str()]).unwrap()
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
        sent: Vec<Frame>,
    }

    struct MockDevice {
        shared: Rc<RefCell<Shared>>,
        replies: HashMap<u8, Reply>,
    }

    impl ReportChannel for MockDevice {
        fn send(&mut self, frame: &Frame) -> Result<(), String> {
            self.shared.borrow_mut().sent.push(*frame);
            Ok(())
        }

        fn receive(&mut self, _timeout: Duration) -> Result<Option<Vec<u8>>, String> {
            let shared = self.shared.borrow();
            let frame = shared.sent.last().expect("送信の前に受信しない");
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
                return Err("開けません".to_owned());
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
                return Err("ログを作れません".to_owned());
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
        platform.tool = Err("pgrep 失敗".to_owned());
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
            "ENTER UPDATE MODE\n",
            "enter update mode \n",
            "write firmware\n",
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
        platform.answer = Err("読めません".to_owned());
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
    fn write_rejects_the_enter_phrase() {
        let mut platform = MockPlatform::new(vec![keyboard("a")], ENTER_PHRASE);
        let abort = write_with(&mut platform, &hfb()).unwrap_err();
        assert_eq!(abort.stage, Stage::WriteNotSent);
        assert!(platform.opened.is_empty());
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

    #[test]
    fn rejected_confirmation_creates_no_log() {
        let mut platform = MockPlatform::new(vec![keyboard("a")], "no");
        assert!(enter_with(&mut platform, &hfb()).is_err());
        assert_eq!(platform.logs_opened, 0);
    }
}
