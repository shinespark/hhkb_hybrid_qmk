use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use hhkb::device::{HidEntry, HidEnumerator, Target, Transport, select_target};
use hhkb::hfb::{Determination, Report, inspect};
use hidapi::{BusType, HidApi, HidError};

#[derive(Parser)]
#[command(name = "hhkb")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    List,
    Inspect { path: PathBuf },
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
        self.api
            .device_list()
            .map(|d| HidEntry {
                path: d.path().to_string_lossy().into_owned(),
                vendor_id: d.vendor_id(),
                product_id: d.product_id(),
                transport: match d.bus_type() {
                    BusType::Usb => Transport::Usb,
                    _ => Transport::Other,
                },
                product: d.product_string().map(str::to_owned),
                release: d.release_number(),
                usage_page: d.usage_page(),
                usage: d.usage(),
            })
            .collect()
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

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::List => match SystemHid::new() {
            Ok(hid) => list(&hid),
            Err(error) => {
                eprintln!("エラー: HID の初期化に失敗しました: {error}");
                ExitCode::FAILURE
            }
        },
        Command::Inspect { path } => inspect_file(&path),
    }
}
