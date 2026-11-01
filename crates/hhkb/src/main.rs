use std::process::ExitCode;

use clap::{Parser, Subcommand};
use hhkb::device::{HidEntry, HidEnumerator, Target, Transport, select_target};
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

fn main() -> ExitCode {
    let cli = Cli::parse();
    let hid = match SystemHid::new() {
        Ok(hid) => hid,
        Err(error) => {
            eprintln!("エラー: HID の初期化に失敗しました: {error}");
            return ExitCode::FAILURE;
        }
    };
    match cli.command {
        Command::List => list(&hid),
    }
}
