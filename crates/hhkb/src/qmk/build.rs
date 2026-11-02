use std::fmt;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use crate::hfb::to_hex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Keymap {
    Default,
    Via,
}

impl Keymap {
    pub fn name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Via => "via",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Platform {
    Arm64,
    Amd64,
}

impl Platform {
    fn docker_name(self) -> &'static str {
        match self {
            Self::Arm64 => "linux/arm64",
            Self::Amd64 => "linux/amd64",
        }
    }

    fn short_name(self) -> &'static str {
        match self {
            Self::Arm64 => "arm64",
            Self::Amd64 => "amd64",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub root: PathBuf,
    pub uid: u32,
    pub gid: u32,
}

const REFERENCE_COMMIT: &str = "0099ac64d30a0530714844fe1fcbaff230fc65fa";
const QMK_COMMIT: &str = "08c662f286ddfd12a985f57b584b02eca5af0ae6";

const REFERENCE_DIRECTORY: &str = "third_party/hhkb_hybrid";
const QMK_DIRECTORY: &str = "third_party/hhkb_hybrid/qmk_firmware";
const KEYBOARD_DIRECTORY: &str = "third_party/hhkb_hybrid/keyboards/hhkb_hybrid";
const OUTPUT_DIRECTORY: &str = "local/qmk-build";
const IMAGE: &str =
    "ghcr.io/qmk/qmk_cli@sha256:b7d7fa8fb4432b569931de5ad59098cb788f440ed61a62c5126746b71aee0f4a";
const KEYBOARD: &str = "hhkb_hybrid";

#[derive(Debug, PartialEq, Eq)]
pub enum BuildError {
    Project(String),
    CommitMismatch { directory: PathBuf, actual: String },
    Git(String),
    LocalChanges { directory: PathBuf },
    Output(String),
    Docker(String),
    Artifact(String),
    HashMismatch { expected: String, actual: String },
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Project(reason) => write!(f, "{reason}"),
            Self::CommitMismatch { directory, actual } => write!(
                f,
                "Not building: commit {actual} of {} differs from the pinned value",
                directory.display()
            ),
            Self::Git(reason) => write!(f, "Not building: the Git check failed: {reason}"),
            Self::LocalChanges { directory } => write!(
                f,
                "Not building: {} has uncommitted changes",
                directory.display()
            ),
            Self::Output(reason) => write!(f, "Cannot prepare the output directory: {reason}"),
            Self::Docker(reason) => write!(f, "Docker build failed: {reason}"),
            Self::Artifact(reason) => write!(f, "Cannot read the artifact: {reason}"),
            Self::HashMismatch { expected, actual } => write!(
                f,
                "The .bin SHA-256 does not match the expected value (expected {expected}, actual {actual})"
            ),
        }
    }
}

impl std::error::Error for BuildError {}

trait Host {
    fn git_head(&self, directory: &Path) -> Result<String, String>;
    fn git_status(&self, directory: &Path, include_untracked: bool) -> Result<String, String>;
    fn prepare_output(&self, directory: &Path) -> Result<(), String>;
    fn run_docker(&self, arguments: &[String]) -> Result<(), String>;
    fn read(&self, path: &Path) -> Result<Vec<u8>, String>;
}

fn docker_arguments(project: &Project, keymap: Keymap, platform: Platform) -> Vec<String> {
    let qmk = project.root.join(QMK_DIRECTORY);
    let keyboard = project.root.join(KEYBOARD_DIRECTORY);
    let output = output_directory(project, keymap, platform);
    vec![
        "run".to_owned(),
        "--rm".to_owned(),
        "--platform".to_owned(),
        platform.docker_name().to_owned(),
        "-u".to_owned(),
        format!("{}:{}", project.uid, project.gid),
        "-v".to_owned(),
        format!("{}:/qmk_firmware", qmk.display()),
        "-v".to_owned(),
        format!(
            "{}:/qmk_firmware/keyboards/{KEYBOARD}:ro",
            keyboard.display()
        ),
        "-v".to_owned(),
        format!("{}:/qmk_firmware/.build", output.display()),
        "-e".to_owned(),
        "SKIP_GIT=1".to_owned(),
        "-e".to_owned(),
        "SKIP_VERSION=1".to_owned(),
        "-w".to_owned(),
        "/qmk_firmware".to_owned(),
        IMAGE.to_owned(),
        "qmk".to_owned(),
        "compile".to_owned(),
        "-kb".to_owned(),
        KEYBOARD.to_owned(),
        "-km".to_owned(),
        keymap.name().to_owned(),
    ]
}

fn output_directory(project: &Project, keymap: Keymap, platform: Platform) -> PathBuf {
    project
        .root
        .join(OUTPUT_DIRECTORY)
        .join(format!("{}-{}", keymap.name(), platform.short_name()))
}

fn check_checkout(
    host: &impl Host,
    directory: &Path,
    commit: &str,
    include_untracked: bool,
) -> Result<(), BuildError> {
    let actual = host.git_head(directory).map_err(BuildError::Git)?;
    if actual != commit {
        return Err(BuildError::CommitMismatch {
            directory: directory.to_owned(),
            actual,
        });
    }
    let status = host
        .git_status(directory, include_untracked)
        .map_err(BuildError::Git)?;
    if !status.trim().is_empty() {
        return Err(BuildError::LocalChanges {
            directory: directory.to_owned(),
        });
    }
    Ok(())
}

fn build_with_expected(
    host: &impl Host,
    project: &Project,
    keymap: Keymap,
    platform: Platform,
    expected: &str,
) -> Result<PathBuf, BuildError> {
    check_checkout(
        host,
        &project.root.join(REFERENCE_DIRECTORY),
        REFERENCE_COMMIT,
        true,
    )?;
    // Docker がマウントのために QMK 本体の中に作る keyboards/hhkb_hybrid を変更扱いにしない。
    check_checkout(host, &project.root.join(QMK_DIRECTORY), QMK_COMMIT, false)?;
    let output = output_directory(project, keymap, platform);
    host.prepare_output(&output).map_err(BuildError::Output)?;
    host.run_docker(&docker_arguments(project, keymap, platform))
        .map_err(BuildError::Docker)?;
    let artifact = output.join(format!("{KEYBOARD}_{}.bin", keymap.name()));
    let bytes = host.read(&artifact).map_err(BuildError::Artifact)?;
    let actual = to_hex(&Sha256::digest(&bytes));
    if actual != expected {
        return Err(BuildError::HashMismatch {
            expected: expected.to_owned(),
            actual,
        });
    }
    Ok(artifact)
}

struct SystemHost;

fn git_output(directory: &Path, arguments: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(arguments)
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "{}: {}",
            directory.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

impl Host for SystemHost {
    fn git_head(&self, directory: &Path) -> Result<String, String> {
        git_output(directory, &["rev-parse", "HEAD"])
    }

    fn git_status(&self, directory: &Path, include_untracked: bool) -> Result<String, String> {
        let untracked = if include_untracked {
            "--untracked-files=all"
        } else {
            "--untracked-files=no"
        };
        git_output(directory, &["status", "--porcelain", untracked])
    }

    fn prepare_output(&self, directory: &Path) -> Result<(), String> {
        if directory.exists() {
            fs::remove_dir_all(directory).map_err(|error| error.to_string())?;
        }
        fs::create_dir_all(directory).map_err(|error| error.to_string())
    }

    fn run_docker(&self, arguments: &[String]) -> Result<(), String> {
        let status = Command::new("docker")
            .args(arguments)
            .status()
            .map_err(|error| format!("Cannot run docker: {error}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("docker exited with {status}"))
        }
    }

    fn read(&self, path: &Path) -> Result<Vec<u8>, String> {
        fs::read(path).map_err(|error| format!("{}: {error}", path.display()))
    }
}

impl Project {
    pub fn from_current_directory() -> Result<Self, BuildError> {
        let root =
            std::env::current_dir().map_err(|error| BuildError::Project(error.to_string()))?;
        if !root.join("local").is_dir() || !root.join(REFERENCE_DIRECTORY).is_dir() {
            return Err(BuildError::Project(
                "local/ and third_party/hhkb_hybrid not found. Run from the repository root"
                    .to_owned(),
            ));
        }
        let metadata =
            fs::metadata(&root).map_err(|error| BuildError::Project(error.to_string()))?;
        Ok(Self {
            root,
            uid: metadata.uid(),
            gid: metadata.gid(),
        })
    }
}

pub fn build(project: &Project, keymap: Keymap, platform: Platform) -> Result<PathBuf, BuildError> {
    build_with_expected(
        &SystemHost,
        project,
        keymap,
        platform,
        keymap.expected_sha256(),
    )
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use super::*;

    const REFERENCE: &str = "third_party/hhkb_hybrid";
    const QMK: &str = "third_party/hhkb_hybrid/qmk_firmware";

    struct MockHost {
        heads: HashMap<PathBuf, Result<String, String>>,
        statuses: HashMap<PathBuf, String>,
        include_untracked: RefCell<HashMap<PathBuf, bool>>,
        docker_result: Result<(), String>,
        bin: Result<Vec<u8>, String>,
        prepared: RefCell<Vec<PathBuf>>,
        docker_calls: RefCell<Vec<Vec<String>>>,
    }

    fn project() -> Project {
        Project {
            root: PathBuf::from("/repo"),
            uid: 501,
            gid: 20,
        }
    }

    impl MockHost {
        fn good() -> Self {
            let root = project().root;
            Self {
                heads: HashMap::from([
                    (root.join(REFERENCE), Ok(REFERENCE_COMMIT.to_owned())),
                    (root.join(QMK), Ok(QMK_COMMIT.to_owned())),
                ]),
                statuses: HashMap::new(),
                include_untracked: RefCell::default(),
                docker_result: Ok(()),
                bin: Ok(b"bin".to_vec()),
                prepared: RefCell::default(),
                docker_calls: RefCell::default(),
            }
        }
    }

    impl Host for MockHost {
        fn git_head(&self, directory: &Path) -> Result<String, String> {
            self.heads[directory].clone()
        }

        fn git_status(&self, directory: &Path, include_untracked: bool) -> Result<String, String> {
            self.include_untracked
                .borrow_mut()
                .insert(directory.to_owned(), include_untracked);
            Ok(self.statuses.get(directory).cloned().unwrap_or_default())
        }

        fn prepare_output(&self, directory: &Path) -> Result<(), String> {
            self.prepared.borrow_mut().push(directory.to_owned());
            Ok(())
        }

        fn run_docker(&self, arguments: &[String]) -> Result<(), String> {
            self.docker_calls.borrow_mut().push(arguments.to_vec());
            self.docker_result.clone()
        }

        fn read(&self, _path: &Path) -> Result<Vec<u8>, String> {
            self.bin.clone()
        }
    }

    fn run(host: &MockHost) -> Result<PathBuf, BuildError> {
        build_with_expected(host, &project(), Keymap::Via, Platform::Arm64, &sha(b"bin"))
    }

    fn sha(data: &[u8]) -> String {
        to_hex(&Sha256::digest(data))
    }

    #[test]
    fn matching_inputs_build_once_and_return_the_artifact() {
        let host = MockHost::good();
        let path = run(&host).unwrap();
        assert_eq!(host.docker_calls.borrow().len(), 1);
        assert_eq!(
            path,
            PathBuf::from("/repo/local/qmk-build/via-arm64/hhkb_hybrid_via.bin")
        );
    }

    #[test]
    fn the_pinned_commits_are_the_recorded_ones() {
        assert_eq!(REFERENCE_COMMIT, "0099ac64d30a0530714844fe1fcbaff230fc65fa");
        assert_eq!(QMK_COMMIT, "08c662f286ddfd12a985f57b584b02eca5af0ae6");
    }

    #[test]
    fn different_reference_commit_stops_before_docker() {
        let mut host = MockHost::good();
        host.heads
            .insert(project().root.join(REFERENCE), Ok("other".to_owned()));
        assert!(matches!(run(&host), Err(BuildError::CommitMismatch { .. })));
        assert!(host.docker_calls.borrow().is_empty());
    }

    #[test]
    fn different_qmk_commit_stops_before_docker() {
        let mut host = MockHost::good();
        host.heads
            .insert(project().root.join(QMK), Ok("other".to_owned()));
        assert!(matches!(run(&host), Err(BuildError::CommitMismatch { .. })));
        assert!(host.docker_calls.borrow().is_empty());
    }

    #[test]
    fn unreadable_commit_stops_before_docker() {
        let mut host = MockHost::good();
        host.heads
            .insert(project().root.join(REFERENCE), Err("git failed".to_owned()));
        assert!(matches!(run(&host), Err(BuildError::Git(_))));
        assert!(host.docker_calls.borrow().is_empty());
    }

    #[test]
    fn local_changes_in_either_checkout_stop_before_docker() {
        for directory in [REFERENCE, QMK] {
            let mut host = MockHost::good();
            host.statuses
                .insert(project().root.join(directory), " M file".to_owned());
            assert!(
                matches!(run(&host), Err(BuildError::LocalChanges { .. })),
                "{directory}"
            );
            assert!(host.docker_calls.borrow().is_empty(), "{directory}");
        }
    }

    #[test]
    fn untracked_files_count_in_the_reference_but_not_in_the_qmk_tree() {
        let host = MockHost::good();
        run(&host).unwrap();
        let seen = host.include_untracked.borrow();
        assert!(seen[&project().root.join(REFERENCE)]);
        assert!(!seen[&project().root.join(QMK)]);
    }

    #[test]
    fn output_is_prepared_under_local_before_docker() {
        let host = MockHost::good();
        run(&host).unwrap();
        assert_eq!(
            *host.prepared.borrow(),
            [PathBuf::from("/repo/local/qmk-build/via-arm64")]
        );
    }

    #[test]
    fn docker_failure_is_reported() {
        let mut host = MockHost::good();
        host.docker_result = Err("exit code 1".to_owned());
        assert!(matches!(run(&host), Err(BuildError::Docker(_))));
    }

    #[test]
    fn missing_artifact_is_reported() {
        let mut host = MockHost::good();
        host.bin = Err("missing".to_owned());
        assert!(matches!(run(&host), Err(BuildError::Artifact(_))));
    }

    #[test]
    fn hash_mismatch_reports_both_values() {
        let mut host = MockHost::good();
        host.bin = Ok(b"other".to_vec());
        let error = run(&host).unwrap_err();
        let text = error.to_string();
        assert!(text.contains(&sha(b"bin")), "{text}");
        assert!(text.contains(&sha(b"other")), "{text}");
        assert!(matches!(error, BuildError::HashMismatch { .. }));
    }

    #[test]
    fn docker_arguments_are_exactly_the_fixed_command() {
        let arguments = docker_arguments(&project(), Keymap::Via, Platform::Arm64);
        let expected = [
            "run",
            "--rm",
            "--platform",
            "linux/arm64",
            "-u",
            "501:20",
            "-v",
            "/repo/third_party/hhkb_hybrid/qmk_firmware:/qmk_firmware",
            "-v",
            "/repo/third_party/hhkb_hybrid/keyboards/hhkb_hybrid:/qmk_firmware/keyboards/hhkb_hybrid:ro",
            "-v",
            "/repo/local/qmk-build/via-arm64:/qmk_firmware/.build",
            "-e",
            "SKIP_GIT=1",
            "-e",
            "SKIP_VERSION=1",
            "-w",
            "/qmk_firmware",
            "ghcr.io/qmk/qmk_cli@sha256:b7d7fa8fb4432b569931de5ad59098cb788f440ed61a62c5126746b71aee0f4a",
            "qmk",
            "compile",
            "-kb",
            "hhkb_hybrid",
            "-km",
            "via",
        ];
        assert_eq!(arguments, expected);
    }

    #[test]
    fn keymap_and_platform_select_only_their_own_arguments() {
        let arguments = docker_arguments(&project(), Keymap::Default, Platform::Amd64);
        assert!(arguments.contains(&"linux/amd64".to_owned()));
        assert!(
            arguments
                .contains(&"/repo/local/qmk-build/default-amd64:/qmk_firmware/.build".to_owned())
        );
        assert_eq!(arguments.last().unwrap(), "default");
    }

    #[test]
    fn expected_hashes_are_the_measured_values_of_the_pinned_commit() {
        assert_eq!(
            Keymap::Default.expected_sha256(),
            "913daf19ee8ac0a4e86f0d17b2d99a773797b1489bc77cfaae32dcad4b59aaff"
        );
        assert_eq!(
            Keymap::Via.expected_sha256(),
            "4a12b64b99c25d877def3ff59432e25217b66f9191661a8d7b890ce9f9f652e0"
        );
    }
}
