use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs::{File, OpenOptions, Permissions};
use std::io::{self, Write as _};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::policy::{Decision, Mode, VENDOR_USAGE_PAGE, decide};

pub trait LogSink: Send + Sync {
    fn append(&self, line: &[u8]) -> io::Result<()>;
}

pub struct FileSink(File);

impl FileSink {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)?;
        file.set_permissions(Permissions::from_mode(0o600))?;
        Ok(Self(file))
    }
}

impl LogSink for FileSink {
    fn append(&self, line: &[u8]) -> io::Result<()> {
        (&self.0).write(line).map(|_| ())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceInfo {
    pub transport: Option<String>,
    pub usage_page: Option<i64>,
    pub location: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendOutcome {
    Forwarded(i32),
    Blocked,
}

pub struct Session<S> {
    mode: Mode,
    command_offset: Option<usize>,
    sink: Option<S>,
    sequence: AtomicU64,
}

impl Session<FileSink> {
    pub fn from_values(path: Option<&OsStr>, mode: Option<&str>, offset: Option<&str>) -> Self {
        let sink = path.and_then(|p| FileSink::open(Path::new(p)).ok());
        Self::new(Mode::parse(mode), offset.and_then(|o| o.parse().ok()), sink)
    }
}

impl<S: LogSink> Session<S> {
    pub fn new(mode: Mode, command_offset: Option<usize>, sink: Option<S>) -> Self {
        let session = Self {
            mode,
            command_offset,
            sink,
            sequence: AtomicU64::new(0),
        };
        session.write_event(
            "session",
            &format!(
                "\"pid\":{},\"mode\":\"{}\",\"command_offset\":{}",
                std::process::id(),
                mode.name(),
                command_offset.map_or("null".to_owned(), |o| o.to_string())
            ),
        );
        session
    }

    pub fn send(
        &self,
        device: &DeviceInfo,
        report_type: u32,
        report_id: isize,
        report: &[u8],
        forward: impl FnOnce() -> i32,
    ) -> SendOutcome {
        let decision = if self.sink.is_none() {
            Decision::Block
        } else {
            decide(self.mode, self.command_offset, device.usage_page, report)
        };
        let direction = match decision {
            Decision::Forward => "out",
            Decision::Block => "out-blocked",
        };
        let sequence = self.write_report(direction, device, report_type, report_id as i64, report);
        match decision {
            Decision::Block => SendOutcome::Blocked,
            Decision::Forward => {
                let result = forward();
                self.write_event(
                    "out-result",
                    &format!("\"of\":{sequence},\"result\":{result}"),
                );
                SendOutcome::Forwarded(result)
            }
        }
    }

    pub fn receive(&self, device: &DeviceInfo, report_type: u32, report_id: u32, report: &[u8]) {
        self.write_report("in", device, report_type, i64::from(report_id), report);
    }

    pub fn device_event(&self, kind: &str, device: &DeviceInfo, value: u32, result: Option<i32>) {
        let result = result.map_or("null".to_owned(), |r| r.to_string());
        self.write_event(
            kind,
            &format!(
                "{},\"value\":{value},\"result\":{result}",
                device_fields(device)
            ),
        );
    }

    fn write_report(
        &self,
        direction: &str,
        device: &DeviceInfo,
        report_type: u32,
        report_id: i64,
        report: &[u8],
    ) -> u64 {
        let mut fields = format!(
            "{},\"report_type\":{report_type},\"report_id\":{report_id},\"len\":{}",
            device_fields(device),
            report.len()
        );
        if device.usage_page == Some(VENDOR_USAGE_PAGE) {
            fields.push_str(",\"bytes\":\"");
            for byte in report {
                let _ = write!(fields, "{byte:02x}");
            }
            fields.push('"');
        }
        self.write_event(direction, &fields)
    }

    fn write_event(&self, kind: &str, fields: &str) -> u64 {
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        if let Some(sink) = &self.sink {
            let time_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_millis());
            let line =
                format!("{{\"seq\":{sequence},\"t_ms\":{time_ms},\"kind\":\"{kind}\",{fields}}}\n");
            let _ = sink.append(line.as_bytes());
        }
        sequence
    }
}

fn device_fields(device: &DeviceInfo) -> String {
    let transport: Option<String> = device.transport.as_ref().map(|t| {
        t.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect()
    });
    format!(
        "\"transport\":{},\"usage_page\":{},\"location\":{}",
        transport.map_or("null".to_owned(), |t| format!("\"{t}\"")),
        device
            .usage_page
            .map_or("null".to_owned(), |v| v.to_string()),
        device.location.map_or("null".to_owned(), |v| v.to_string())
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::fs;
    use std::os::unix::fs::MetadataExt;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemorySink {
        lines: Mutex<Vec<String>>,
        fail: bool,
    }

    impl LogSink for &MemorySink {
        fn append(&self, line: &[u8]) -> io::Result<()> {
            if self.fail {
                return Err(io::Error::other("full"));
            }
            self.lines
                .lock()
                .unwrap()
                .push(String::from_utf8(line.to_vec()).unwrap());
            Ok(())
        }
    }

    fn vendor() -> DeviceInfo {
        DeviceInfo {
            transport: Some("USB".to_owned()),
            usage_page: Some(VENDOR_USAGE_PAGE),
            location: Some(1),
        }
    }

    fn keyboard() -> DeviceInfo {
        DeviceInfo {
            usage_page: Some(1),
            ..vendor()
        }
    }

    #[test]
    fn allowed_report_forwards_and_returns_original_result() {
        let sink = MemorySink::default();
        let session = Session::new(Mode::Read, Some(2), Some(&sink));
        let outcome = session.send(&vendor(), 1, 0, &[0xaa, 0xaa, 0x02, 0xab], || -42);

        assert_eq!(outcome, SendOutcome::Forwarded(-42));
        let lines = sink.lines.lock().unwrap();
        assert!(lines.iter().any(|l| l.contains("\"bytes\":\"aaaa02ab\"")));
        assert!(lines.iter().any(|l| l.contains("\"result\":-42")));
    }

    #[test]
    fn blocked_report_does_not_forward_and_is_recorded() {
        let sink = MemorySink::default();
        let session = Session::new(Mode::Probe, None, Some(&sink));
        let called = Cell::new(false);
        let outcome = session.send(&vendor(), 1, 0, &[0x01, 0xcd], || {
            called.set(true);
            0
        });

        assert_eq!(outcome, SendOutcome::Blocked);
        assert!(!called.get());
        let lines = sink.lines.lock().unwrap();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("out-blocked") && l.contains("01cd"))
        );
    }

    #[test]
    fn missing_log_blocks_every_send() {
        let session = Session::<&MemorySink>::new(Mode::FirmwareUpdate, Some(0), None);
        let outcome = session.send(&vendor(), 1, 0, &[0x02], || 0);
        assert_eq!(outcome, SendOutcome::Blocked);
    }

    #[test]
    fn keyboard_input_records_metadata_only() {
        let sink = MemorySink::default();
        let session = Session::new(Mode::Read, Some(2), Some(&sink));
        session.receive(&keyboard(), 0, 0, &[0x00, 0x00, 0x04]);

        let lines = sink.lines.lock().unwrap();
        let line = lines
            .iter()
            .find(|l| l.contains("\"kind\":\"in\""))
            .unwrap();
        assert!(line.contains("\"len\":3"));
        assert!(!line.contains("bytes"));
    }

    #[test]
    fn vendor_input_records_all_bytes() {
        let sink = MemorySink::default();
        let session = Session::new(Mode::Read, Some(2), Some(&sink));
        session.receive(&vendor(), 0, 0, &[0x12, 0x34]);
        assert!(
            sink.lines
                .lock()
                .unwrap()
                .iter()
                .any(|l| l.contains("\"bytes\":\"1234\""))
        );
    }

    #[test]
    fn log_failure_does_not_stop_traffic_and_leaves_sequence_gap() {
        let sink = MemorySink {
            fail: true,
            ..Default::default()
        };
        let session = Session::new(Mode::Read, Some(2), Some(&sink));
        let outcome = session.send(&vendor(), 1, 0, &[0xaa, 0xaa, 0x02], || 7);
        assert_eq!(outcome, SendOutcome::Forwarded(7));
        assert!(session.sequence.load(Ordering::Relaxed) >= 3);
    }

    #[test]
    fn file_sink_uses_owner_only_permissions() {
        let path = std::env::temp_dir().join(format!("hid-logger-test-{}", std::process::id()));
        fs::write(&path, b"").unwrap();
        fs::set_permissions(&path, Permissions::from_mode(0o644)).unwrap();

        let sink = FileSink::open(&path).unwrap();
        sink.append(b"x\n").unwrap();
        let mode = fs::metadata(&path).unwrap().mode() & 0o777;
        fs::remove_file(&path).unwrap();

        assert_eq!(mode, 0o600);
    }
}
