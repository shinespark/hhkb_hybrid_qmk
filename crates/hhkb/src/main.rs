use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use hhkb::device::{HidEntry, HidEnumerator, Target, select_target};
use hhkb::flash::{self, Abort, VerifiedHfb};
use hhkb::hfb::{Determination, Report, inspect};
use hhkb::status::check_status;
use hidapi::{HidApi, HidError};

#[derive(Parser)]
#[command(name = "hhkb")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    List,
    Status,
    Inspect {
        path: PathBuf,
    },
    /// 許可した純正 HFB を段階を分けて書き込む
    Flash {
        #[command(subcommand)]
        stage: FlashStage,
    },
}

#[derive(Subcommand)]
enum FlashStage {
    /// HFB を検査し、パケット数と接続状況を表示する。実機には送信しない
    Plan { path: PathBuf },
    /// 実機の版を確認して更新モードへ移行する。本体は書き込まない
    Enter { path: PathBuf },
    /// 更新モードの実機に本体を書き込む
    Write { path: PathBuf },
}

struct SystemHid {
    api: HidApi,
}

impl SystemHid {
    fn new() -> Result<Self, HidError> {
        Ok(Self {
            api: HidApi::new()?,
        })
    }
}

impl HidEnumerator for SystemHid {
    fn enumerate(&self) -> Vec<HidEntry> {
        self.api.device_list().map(HidEntry::from).collect()
    }
}

fn print_target(target: &Target) {
    println!("製品名: {}", target.product.as_deref().unwrap_or("(不明)"));
    println!("バージョン: 0x{:04x}", target.release);
    println!("インターフェース:");
    for interface in &target.interfaces {
        let usages: Vec<String> = interface
            .usages
            .iter()
            .map(|(page, usage)| format!("0x{page:04x}/0x{usage:04x}"))
            .collect();
        println!("  {}  {}", interface.path, usages.join(", "));
    }
    println!(
        "状態: 純正 FW または更新モード（この情報では区別できません。キー入力の可否で確認してください）"
    );
}

fn list(hid: &impl HidEnumerator) -> ExitCode {
    match select_target(&hid.enumerate()) {
        Ok(target) => {
            print_target(&target);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("エラー: {error}");
            ExitCode::FAILURE
        }
    }
}

fn status(hid: &SystemHid) -> ExitCode {
    match check_status(hid, &hid.api) {
        Ok(status) => {
            print_target(&status.target);
            if status.received.is_empty() {
                println!("承認済みのレポートがないので、デバイスを開かず、何も送信していません");
            }
            for received in &status.received {
                println!(
                    "応答: コマンド 0x{:02x}、4 バイト目 0x{:02x}、データ長 {}",
                    received.command, received.byte3, received.data_len
                );
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("エラー: {error}");
            ExitCode::FAILURE
        }
    }
}

fn determination_text(determination: &Determination) -> &str {
    match determination {
        Determination::Known(value) => value,
        Determination::Undetermined => "未確定",
    }
}

fn print_report(report: &Report) {
    println!("ファイルサイズ: 0x{:x} バイト", report.file_size);
    println!("領域:");
    for region in &report.regions {
        println!(
            "  0x{:05x}  サイズ 0x{:05x}  {}",
            region.offset, region.size, region.name
        );
    }
    println!("整合性:");
    for check in &report.checks {
        let result = if check.is_match() {
            "一致"
        } else {
            "不一致"
        };
        println!(
            "  {}: {result}（0x{:05x}〜0x{:05x}、{}、ファイル内 {}、再計算 {}）",
            check.name, check.start, check.end, check.algorithm, check.stored, check.computed
        );
    }
    println!("機種: {}", determination_text(&report.model));
    println!("版: {}", determination_text(&report.version));
}

fn inspect_file(path: &Path) -> ExitCode {
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(error) => {
            eprintln!("エラー: {} を読めません: {error}", path.display());
            return ExitCode::FAILURE;
        }
    };
    match inspect(&data) {
        Ok(report) => {
            print_report(&report);
            if report.is_valid() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("エラー: {error}");
            ExitCode::FAILURE
        }
    }
}

fn load_verified(path: &Path) -> Result<VerifiedHfb, String> {
    let data =
        fs::read(path).map_err(|error| format!("{} を読めません: {error}", path.display()))?;
    VerifiedHfb::verify(data).map_err(|error| error.to_string())
}

fn flash_plan(hid: &SystemHid, hfb: &VerifiedHfb) -> ExitCode {
    print_report(hfb.report());
    println!("SHA-256: {}（許可リストと一致）", hfb.sha256());
    let plan = flash::plan(hfb, hid);
    println!("本体の送信: {} パケット", plan.packets);
    println!("接続状況:");
    match &plan.connection {
        Ok(target) => print_target(target),
        Err(error) => println!("  {error}"),
    }
    println!("実機には何も送信していません");
    ExitCode::SUCCESS
}

fn finish_stage(result: Result<(), Abort>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(abort) => {
            eprintln!("エラー: {abort}");
            eprintln!("{}", abort.guidance());
            ExitCode::FAILURE
        }
    }
}

fn flash_stage(stage: FlashStage) -> ExitCode {
    let path = match &stage {
        FlashStage::Plan { path } | FlashStage::Enter { path } | FlashStage::Write { path } => path,
    };
    let hfb = match load_verified(path) {
        Ok(hfb) => hfb,
        Err(reason) => {
            eprintln!("エラー: {reason}");
            return ExitCode::FAILURE;
        }
    };
    match stage {
        FlashStage::Plan { .. } => with_hid(|hid| flash_plan(hid, &hfb)),
        FlashStage::Enter { .. } => finish_stage(flash::enter(&hfb)),
        FlashStage::Write { .. } => finish_stage(flash::write(&hfb)),
    }
}

fn with_hid(run: impl FnOnce(&SystemHid) -> ExitCode) -> ExitCode {
    match SystemHid::new() {
        Ok(hid) => run(&hid),
        Err(error) => {
            eprintln!("エラー: HID の初期化に失敗しました: {error}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::List => with_hid(list),
        Command::Status => with_hid(status),
        Command::Inspect { path } => inspect_file(&path),
        Command::Flash { stage } => flash_stage(stage),
    }
}
