use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use super::protocol::{QmkCommand, Step};
use crate::hfb::to_hex;

const START_HEAD_SIZE: usize = 11;
const BODY_HEAD_SIZE: usize = 7;
const COMMAND_HEAD_SIZE: usize = 3;

pub(super) struct Log {
    out: Box<dyn Write>,
    warned: bool,
}

fn seconds_since_epoch() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64())
}

fn step_fields(step: Step) -> String {
    match step {
        Step::KeyboardInfo => r#""step":"02""#.to_owned(),
        Step::ModeChange => r#""step":"e0""#.to_owned(),
        Step::Start => r#""step":"e1""#.to_owned(),
        Step::Body(packet) => format!(r#""step":"e2","packet":{packet}"#),
        Step::End => r#""step":"e3""#.to_owned(),
        Step::Qmk(QmkCommand::ModeChange) => r#""step":"qmk-e0""#.to_owned(),
        Step::Qmk(QmkCommand::StockRevert) => r#""step":"qmk-f0""#.to_owned(),
        Step::Qmk(QmkCommand::ViaProbe) => r#""step":"qmk-via-probe""#.to_owned(),
    }
}

fn head_size(step: Step) -> usize {
    match step {
        Step::Start => START_HEAD_SIZE,
        Step::Body(_) => BODY_HEAD_SIZE,
        Step::KeyboardInfo | Step::ModeChange | Step::End | Step::Qmk(_) => COMMAND_HEAD_SIZE,
    }
}

impl Log {
    pub(super) fn new(out: Box<dyn Write>) -> Self {
        Self { out, warned: false }
    }

    // 転送の途中でログに書けなくても通信は止めない。止めると実機が中途半端な状態で残る。
    fn append(&mut self, fields: &str) {
        let line = format!("{{\"t\":{:.3},{fields}}}\n", seconds_since_epoch());
        if let Err(error) = self.out.write_all(line.as_bytes())
            && !self.warned
        {
            eprintln!("警告: ログに書き込めません。通信は続けます: {error}");
            self.warned = true;
        }
    }

    pub(super) fn record_send(&mut self, step: Step, frame: &[u8]) {
        let head = to_hex(&frame[..head_size(step)]);
        self.append(&format!(
            r#""dir":"out",{},"head":"{head}""#,
            step_fields(step)
        ));
    }

    pub(super) fn record_receive(&mut self, step: Step, bytes: &[u8]) {
        self.append(&format!(
            r#""dir":"in",{},"bytes":"{}""#,
            step_fields(step),
            to_hex(bytes)
        ));
    }

    pub(super) fn record_event(&mut self, event: &str) {
        self.append(&format!(r#""event":"{event}""#));
    }

    #[cfg(test)]
    pub(super) fn warned(&self) -> bool {
        self.warned
    }
}

pub(super) fn create_log_file(parent: &Path, stage: &str) -> io::Result<File> {
    if !parent.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} がありません", parent.display()),
        ));
    }
    let directory = parent.join("logs");
    fs::create_dir_all(&directory)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(directory.join(format!("{stamp}-flash-{stage}.jsonl")))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use super::*;
    use crate::device::REPORT_SIZE;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("hhkb-flash-log-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("書けません"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn log_file_is_created_under_logs_with_owner_only_permissions() {
        let dir = TempDir::new("mode");
        let file = create_log_file(&dir.0, "enter").unwrap();
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        let entries: Vec<_> = fs::read_dir(dir.0.join("logs")).unwrap().collect();
        assert_eq!(entries.len(), 1);
        let name = entries[0].as_ref().unwrap().file_name();
        assert!(name.to_string_lossy().ends_with("-flash-enter.jsonl"));
    }

    #[test]
    fn missing_parent_is_rejected_instead_of_created() {
        let dir = TempDir::new("missing");
        let parent = dir.0.join("local");
        assert!(create_log_file(&parent, "enter").is_err());
        assert!(!parent.exists());
    }

    #[test]
    fn log_write_failure_warns_and_does_not_panic() {
        let mut log = Log::new(Box::new(FailingWriter));
        log.record_send(Step::ModeChange, &[0; REPORT_SIZE]);
        log.record_send(Step::ModeChange, &[0; REPORT_SIZE]);
        assert!(log.warned());
    }
}
