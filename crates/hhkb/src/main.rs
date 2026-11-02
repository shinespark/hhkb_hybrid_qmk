use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use hhkb::device::{DeviceState, HidEntry, HidEnumerator, Target, select_target};
use hhkb::flash::{self, Abort, VerifiedHfb};
use hhkb::hfb::{Determination, Report, inspect};
use hhkb::qmk::{self, Keymap, Platform, Project};
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
    /// 許可した HFB（純正、QMK の default と via）を段階を分けて書き込む
    Flash {
        #[command(subcommand)]
        stage: FlashStage,
    },
    /// QMK のビルドと HFB の生成
    Qmk {
        #[command(subcommand)]
        command: QmkCommand,
    },
}

#[derive(Subcommand)]
enum QmkCommand {
    /// 固定した入力で QMK をビルドし、.bin の SHA-256 を照合する。実機には触れない
    Build {
        #[arg(value_enum)]
        keymap: Keymap,
        #[arg(long, value_enum, default_value = "arm64")]
        platform: Platform,
    },
    /// 許可した純正 HFB と .bin から HFB を生成する。実機には触れない
    Hfb {
        #[arg(value_enum)]
        keymap: Keymap,
        #[arg(long)]
        bin: PathBuf,
        #[arg(long)]
        stock: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
}

#[derive(Subcommand)]
enum FlashStage {
    /// HFB を検査し、種類、パケット数、接続状況を表示する。実機には送信しない
    Plan { path: PathBuf },
    /// 更新モードへ移行する。純正 FW の動作中は実機の版を確認する。本体は書き込まない
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
        "状態: {}",
        match target.state {
            DeviceState::StockOrUpdate =>
                "純正 FW または更新モード（この情報では区別できません。キー入力の可否で確認してください）",
            DeviceState::QmkLikely =>
                "QMK の動作中の可能性が高い（製品名とインターフェースからの判断です。公式ツールを起動しないでください）",
            DeviceState::Undetermined =>
                "確定できません（製品名とインターフェースの判断が一致しません）。書き込みの段階には進みません",
        }
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
                if status.target.state == DeviceState::StockOrUpdate {
                    println!(
                        "承認済みのレポートがないので、デバイスを開かず、何も送信していません"
                    );
                } else {
                    println!("純正 FW に見えないので、デバイスを開かず、何も送信していません");
                }
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
    println!("種類: {}", hfb.kind());
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

fn qmk_build(keymap: Keymap, platform: Platform) -> Result<(), String> {
    let project = Project::from_current_directory().map_err(|error| error.to_string())?;
    let artifact = qmk::build(&project, keymap, platform).map_err(|error| error.to_string())?;
    println!("SHA-256 が期待する値と一致しました");
    println!("成果物: {}", artifact.display());
    println!("公開も配布もしないでください");
    Ok(())
}

fn qmk_hfb(keymap: Keymap, bin: &Path, stock: &Path, out: &Path) -> Result<(), String> {
    let stock = load_verified(stock)?;
    let bin = fs::read(bin).map_err(|error| format!("{} を読めません: {error}", bin.display()))?;
    qmk::generate(keymap, &stock, &bin, Path::new("local"), out)
        .map_err(|error| error.to_string())?;
    println!("生成しました: {}", out.display());
    println!("PFU 製の nRF イメージを含むので、公開も配布もしないでください");
    println!("書き込む前に hhkb flash plan で許可リストとの一致を確かめてください");
    Ok(())
}

fn qmk_command(command: QmkCommand) -> ExitCode {
    let result = match command {
        QmkCommand::Build { keymap, platform } => qmk_build(keymap, platform),
        QmkCommand::Hfb {
            keymap,
            bin,
            stock,
            out,
        } => qmk_hfb(keymap, &bin, &stock, &out),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(reason) => {
            eprintln!("エラー: {reason}");
            ExitCode::FAILURE
        }
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
        Command::Qmk { command } => qmk_command(command),
    }
}
