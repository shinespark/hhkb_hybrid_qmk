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
    /// Write an allowlisted HFB (stock firmware, QMK default, or QMK via) in separate stages
    Flash {
        #[command(subcommand)]
        stage: FlashStage,
    },
    /// Build QMK and generate HFB files
    Qmk {
        #[command(subcommand)]
        command: QmkCommand,
    },
}

#[derive(Subcommand)]
enum QmkCommand {
    /// Build QMK from pinned inputs and verify the .bin SHA-256. Does not touch the device
    Build {
        #[arg(value_enum)]
        keymap: Keymap,
        #[arg(long, value_enum, default_value = "arm64")]
        platform: Platform,
    },
    /// Generate an HFB from an allowlisted stock HFB and .bin. Does not touch the device
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
    /// Verify an HFB and show its kind, packet count, and connection. Sends nothing to the device
    Plan { path: PathBuf },
    /// Switch to update mode. Checks the device version on stock firmware. Does not write the firmware
    Prepare { path: PathBuf },
    /// Write the firmware to a device in update mode
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
    println!(
        "Product: {}",
        target.product.as_deref().unwrap_or("(unknown)")
    );
    println!("Release: 0x{:04x}", target.release);
    println!("Interfaces:");
    for interface in &target.interfaces {
        let usages: Vec<String> = interface
            .usages
            .iter()
            .map(|(page, usage)| format!("0x{page:04x}/0x{usage:04x}"))
            .collect();
        println!("  {}  {}", interface.path, usages.join(", "));
    }
    println!(
        "State: {}",
        match target.state {
            DeviceState::StockOrUpdate =>
                "Stock firmware or update mode (cannot be told apart from this information; check by key input)",
            DeviceState::QmkLikely =>
                "QMK is likely running (judged from the product name and interfaces; do not start the official tool)",
            DeviceState::Undetermined =>
                "Undetermined (the product name and interfaces disagree). Write stages will not proceed",
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
            eprintln!("Error: {error}");
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
                        "No approved report exists, so the device was not opened and nothing was sent"
                    );
                } else {
                    println!(
                        "Does not look like stock firmware, so the device was not opened and nothing was sent"
                    );
                }
            }
            for received in &status.received {
                println!(
                    "Response: command 0x{:02x}, byte 3 0x{:02x}, data length {}",
                    received.command, received.byte3, received.data_len
                );
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("Error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn determination_text(determination: &Determination) -> &str {
    match determination {
        Determination::Known(value) => value,
        Determination::Undetermined => "undetermined",
    }
}

fn print_report(report: &Report) {
    println!("File size: 0x{:x} bytes", report.file_size);
    println!("Regions:");
    for region in &report.regions {
        println!(
            "  0x{:05x}  size 0x{:05x}  {}",
            region.offset, region.size, region.name
        );
    }
    println!("Integrity:");
    for check in &report.checks {
        let result = if check.is_match() {
            "match"
        } else {
            "mismatch"
        };
        println!(
            "  {}: {result} (0x{:05x}-0x{:05x}, {}, stored {}, computed {})",
            check.name, check.start, check.end, check.algorithm, check.stored, check.computed
        );
    }
    println!("Model: {}", determination_text(&report.model));
    println!("Version: {}", determination_text(&report.version));
}

fn inspect_file(path: &Path) -> ExitCode {
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(error) => {
            eprintln!("Error: Cannot read {}: {error}", path.display());
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
            eprintln!("Error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn load_verified(path: &Path) -> Result<VerifiedHfb, String> {
    let data =
        fs::read(path).map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
    VerifiedHfb::verify(data).map_err(|error| error.to_string())
}

fn flash_plan(hid: &SystemHid, hfb: &VerifiedHfb) -> ExitCode {
    print_report(hfb.report());
    println!("SHA-256: {} (matches the allowlist)", hfb.sha256());
    println!("Kind: {}", hfb.kind());
    let plan = flash::plan(hfb, hid);
    println!("Firmware to send: {} packets", plan.packets);
    println!("Connection:");
    match &plan.connection {
        Ok(target) => print_target(target),
        Err(error) => println!("  {error}"),
    }
    println!("Nothing was sent to the device");
    ExitCode::SUCCESS
}

fn finish_stage(result: Result<(), Abort>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(abort) => {
            eprintln!("Error: {abort}");
            eprintln!("{}", abort.guidance());
            ExitCode::FAILURE
        }
    }
}

fn flash_stage(stage: FlashStage) -> ExitCode {
    let path = match &stage {
        FlashStage::Plan { path } | FlashStage::Prepare { path } | FlashStage::Write { path } => {
            path
        }
    };
    let hfb = match load_verified(path) {
        Ok(hfb) => hfb,
        Err(reason) => {
            eprintln!("Error: {reason}");
            return ExitCode::FAILURE;
        }
    };
    match stage {
        FlashStage::Plan { .. } => with_hid(|hid| flash_plan(hid, &hfb)),
        FlashStage::Prepare { .. } => finish_stage(flash::prepare(&hfb)),
        FlashStage::Write { .. } => finish_stage(flash::write(&hfb)),
    }
}

fn qmk_build(keymap: Keymap, platform: Platform) -> Result<(), String> {
    let project = Project::from_current_directory().map_err(|error| error.to_string())?;
    let artifact = qmk::build(&project, keymap, platform).map_err(|error| error.to_string())?;
    println!("SHA-256 matches the expected value");
    println!("Artifact: {}", artifact.display());
    println!("Do not publish or distribute it");
    Ok(())
}

fn qmk_hfb(keymap: Keymap, bin: &Path, stock: &Path, out: &Path) -> Result<(), String> {
    let stock = load_verified(stock)?;
    let bin = fs::read(bin).map_err(|error| format!("Cannot read {}: {error}", bin.display()))?;
    qmk::generate(keymap, &stock, &bin, Path::new("local"), out)
        .map_err(|error| error.to_string())?;
    println!("Generated: {}", out.display());
    println!("It contains PFU's nRF image, so do not publish or distribute it");
    println!("Before writing, confirm that it matches the allowlist with hhkb flash plan");
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
            eprintln!("Error: {reason}");
            ExitCode::FAILURE
        }
    }
}

fn with_hid(run: impl FnOnce(&SystemHid) -> ExitCode) -> ExitCode {
    match SystemHid::new() {
        Ok(hid) => run(&hid),
        Err(error) => {
            eprintln!("Error: Failed to initialize HID: {error}");
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
