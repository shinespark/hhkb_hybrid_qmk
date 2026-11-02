use std::collections::BTreeMap;
use std::ffi::CString;
use std::fmt;
use std::time::Duration;

use hidapi::{BusType, DeviceInfo, HidApi, HidDevice};

pub const VENDOR_ID: u16 = 0x04fe;
pub const PRODUCT_ID: u16 = 0x0021;
pub const VENDOR_USAGE_PAGE: u16 = 0xff00;
pub const QMK_USAGE_PAGE: u16 = 0xff60;
const QMK_NAME_MARK: &str = "QMK";
pub(crate) const REPORT_SIZE: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    StockOrUpdate,
    QmkLikely,
    Undetermined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Usb,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HidEntry {
    pub path: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub transport: Transport,
    pub product: Option<String>,
    pub release: u16,
    pub usage_page: u16,
    pub usage: u16,
}

impl From<&DeviceInfo> for HidEntry {
    fn from(info: &DeviceInfo) -> Self {
        Self {
            path: info.path().to_string_lossy().into_owned(),
            vendor_id: info.vendor_id(),
            product_id: info.product_id(),
            transport: match info.bus_type() {
                BusType::Usb => Transport::Usb,
                _ => Transport::Other,
            },
            product: info.product_string().map(str::to_owned),
            release: info.release_number(),
            usage_page: info.usage_page(),
            usage: info.usage(),
        }
    }
}

pub trait HidEnumerator {
    fn enumerate(&self) -> Vec<HidEntry>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interface {
    pub path: String,
    pub usages: Vec<(u16, u16)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub product: Option<String>,
    pub release: u16,
    pub state: DeviceState,
    pub interfaces: Vec<Interface>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectError {
    NotFound,
    MissingVendorInterface,
    Multiple(usize),
}

impl fmt::Display for SelectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => write!(
                f,
                "USB 接続の HHKB (04fe:0021) が見つかりません。ケーブルと接続を確認してください"
            ),
            Self::MissingVendorInterface => write!(
                f,
                "ベンダー定義のインターフェース (Usage Page 0xFF00 または 0xFF60) が見つかりません。USB を接続し直してください"
            ),
            Self::Multiple(count) => write!(
                f,
                "USB 接続の HHKB が {count} 台見つかりました。対象の 1 台だけを接続してください"
            ),
        }
    }
}

impl std::error::Error for SelectError {}

pub(crate) trait ReportChannel {
    fn send(&mut self, report: &[u8]) -> Result<(), String>;
    fn receive(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, String>;
}

pub(crate) struct SystemDevice(HidDevice);

impl ReportChannel for SystemDevice {
    fn send(&mut self, report: &[u8]) -> Result<(), String> {
        self.0
            .write(&output_buffer(report))
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

pub(crate) fn open_device(api: &HidApi, path: &str) -> Result<SystemDevice, String> {
    let path = CString::new(path).map_err(|error| error.to_string())?;
    #[cfg(target_os = "macos")]
    api.set_open_exclusive(false);
    api.open_path(&path)
        .map(SystemDevice)
        .map_err(|error| error.to_string())
}

// hid_write は先頭バイトをレポート ID として扱う。公式ツールの送信はレポート ID 0 の 64 バイトなので、0 を前置する。
fn output_buffer(report: &[u8]) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(report.len() + 1);
    buffer.push(0);
    buffer.extend_from_slice(report);
    buffer
}

fn has_page(usages: &[(u16, u16)], wanted: u16) -> bool {
    usages.iter().any(|(page, _)| *page == wanted)
}

fn has_qmk_name(entry: &&HidEntry) -> bool {
    entry
        .product
        .as_deref()
        .is_some_and(|name| name.contains(QMK_NAME_MARK))
}

pub fn select_target(entries: &[HidEntry]) -> Result<Target, SelectError> {
    let matching: Vec<&HidEntry> = entries
        .iter()
        .filter(|e| {
            e.vendor_id == VENDOR_ID && e.product_id == PRODUCT_ID && e.transport == Transport::Usb
        })
        .collect();

    let Some(first) = matching.first() else {
        return Err(SelectError::NotFound);
    };

    let mut by_path: BTreeMap<&str, Vec<(u16, u16)>> = BTreeMap::new();
    for entry in &matching {
        by_path
            .entry(&entry.path)
            .or_default()
            .push((entry.usage_page, entry.usage));
    }

    let keyboards = by_path
        .values()
        .filter(|usages| has_page(usages, VENDOR_USAGE_PAGE) || has_page(usages, QMK_USAGE_PAGE))
        .count();
    let any_qmk_name = matching.iter().any(has_qmk_name);
    let all_qmk_name = matching.iter().all(has_qmk_name);
    match keyboards {
        0 if !any_qmk_name => return Err(SelectError::MissingVendorInterface),
        0 | 1 => {}
        count => return Err(SelectError::Multiple(count)),
    }

    let has_vendor = by_path.values().any(|u| has_page(u, VENDOR_USAGE_PAGE));
    let has_qmk = by_path.values().any(|u| has_page(u, QMK_USAGE_PAGE));
    let state = match (any_qmk_name, all_qmk_name, has_vendor, has_qmk) {
        (false, _, true, false) => DeviceState::StockOrUpdate,
        (_, true, false, true) => DeviceState::QmkLikely,
        _ => DeviceState::Undetermined,
    };

    let interfaces = by_path
        .into_iter()
        .map(|(path, usages)| Interface {
            path: path.to_owned(),
            usages,
        })
        .collect();

    Ok(Target {
        product: first.product.clone(),
        release: first.release,
        state,
        interfaces,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, transport: Transport, usage_page: u16, usage: u16) -> HidEntry {
        HidEntry {
            path: path.to_owned(),
            vendor_id: VENDOR_ID,
            product_id: PRODUCT_ID,
            transport,
            product: Some("HHKB-Hybrid".to_owned()),
            release: 0x0001,
            usage_page,
            usage,
        }
    }

    fn keyboard(prefix: &str) -> Vec<HidEntry> {
        vec![
            entry(&format!("{prefix}-kbd"), Transport::Usb, 0x0001, 0x0006),
            entry(&format!("{prefix}-cc"), Transport::Usb, 0x000c, 0x0001),
            entry(&format!("{prefix}-cc"), Transport::Usb, 0x0001, 0x0006),
            entry(
                &format!("{prefix}-vendor"),
                Transport::Usb,
                VENDOR_USAGE_PAGE,
                0x0001,
            ),
        ]
    }

    #[test]
    fn bluetooth_entries_are_excluded() {
        let mut entries = keyboard("a");
        entries.push(entry("ble", Transport::Other, 0x0001, 0x0006));
        entries.push(entry(
            "ble-vendor",
            Transport::Other,
            VENDOR_USAGE_PAGE,
            0x0001,
        ));

        let target = select_target(&entries).unwrap();

        assert_eq!(target.interfaces.len(), 3);
        assert!(target.interfaces.iter().all(|i| !i.path.starts_with("ble")));
    }

    #[test]
    fn bluetooth_only_is_not_found() {
        let entries = vec![entry("ble", Transport::Other, 0x0001, 0x0006)];
        assert_eq!(select_target(&entries), Err(SelectError::NotFound));
    }

    #[test]
    fn no_entries_is_not_found() {
        assert_eq!(select_target(&[]), Err(SelectError::NotFound));
    }

    #[test]
    fn two_keyboards_are_rejected() {
        let mut entries = keyboard("a");
        entries.extend(keyboard("b"));
        assert_eq!(select_target(&entries), Err(SelectError::Multiple(2)));
    }

    #[test]
    fn other_devices_are_ignored() {
        let mut entries = keyboard("a");
        let mut other = entry("other", Transport::Usb, VENDOR_USAGE_PAGE, 0x0001);
        other.vendor_id = 0x1234;
        entries.push(other);
        assert!(select_target(&entries).is_ok());
    }

    #[test]
    fn missing_vendor_interface_is_rejected() {
        let entries = vec![entry("kbd", Transport::Usb, 0x0001, 0x0006)];
        assert_eq!(
            select_target(&entries),
            Err(SelectError::MissingVendorInterface)
        );
    }

    fn named(mut entry: HidEntry, product: Option<&str>) -> HidEntry {
        entry.product = product.map(str::to_owned);
        entry
    }

    fn qmk_keyboard(prefix: &str) -> Vec<HidEntry> {
        [
            entry(&format!("{prefix}-kbd"), Transport::Usb, 0x0001, 0x0006),
            entry(
                &format!("{prefix}-raw"),
                Transport::Usb,
                QMK_USAGE_PAGE,
                0x0061,
            ),
        ]
        .into_iter()
        .map(|e| named(e, Some("HHKB Hybrid QMK")))
        .collect()
    }

    #[test]
    fn output_buffer_prepends_report_id_zero_for_both_report_sizes() {
        for size in [REPORT_SIZE, 32] {
            let mut report = vec![0; size];
            report[..3].copy_from_slice(&[0xaa, 0xaa, 0x02]);
            let buffer = output_buffer(&report);
            assert_eq!(buffer.len(), size + 1);
            assert_eq!(buffer[..4], [0x00, 0xaa, 0xaa, 0x02]);
            assert!(buffer[4..].iter().all(|byte| *byte == 0));
        }
    }

    #[test]
    fn stock_name_with_the_vendor_interface_is_stock_or_update() {
        let target = select_target(&keyboard("a")).unwrap();
        assert_eq!(target.state, DeviceState::StockOrUpdate);
    }

    #[test]
    fn qmk_name_with_the_raw_interface_is_likely_qmk() {
        let target = select_target(&qmk_keyboard("a")).unwrap();
        assert_eq!(target.state, DeviceState::QmkLikely);
    }

    #[test]
    fn mismatched_name_and_interface_are_undetermined() {
        let raw = |product| {
            vec![named(
                entry("raw", Transport::Usb, QMK_USAGE_PAGE, 0x0061),
                product,
            )]
        };
        let cases = [
            raw(Some("HHKB-Hybrid")),
            raw(None),
            vec![named(
                entry("vendor", Transport::Usb, VENDOR_USAGE_PAGE, 0x0001),
                Some("HHKB Hybrid QMK"),
            )],
            vec![named(
                entry("kbd", Transport::Usb, 0x0001, 0x0006),
                Some("HHKB Hybrid QMK"),
            )],
        ];
        for entries in cases {
            let target = select_target(&entries).unwrap();
            assert_eq!(target.state, DeviceState::Undetermined, "{entries:?}");
        }
    }

    #[test]
    fn partly_qmk_names_are_undetermined() {
        let mut entries = qmk_keyboard("a");
        entries[0].product = Some("HHKB-Hybrid".to_owned());
        let target = select_target(&entries).unwrap();
        assert_eq!(target.state, DeviceState::Undetermined);
    }

    #[test]
    fn both_pages_on_one_interface_are_undetermined() {
        let entries = vec![
            entry("mixed", Transport::Usb, VENDOR_USAGE_PAGE, 0x0001),
            entry("mixed", Transport::Usb, QMK_USAGE_PAGE, 0x0061),
        ];
        let target = select_target(&entries).unwrap();
        assert_eq!(target.state, DeviceState::Undetermined);
    }

    #[test]
    fn vendor_and_raw_interfaces_count_as_two() {
        let mut entries = keyboard("a");
        entries.extend(qmk_keyboard("b"));
        assert_eq!(select_target(&entries), Err(SelectError::Multiple(2)));
    }

    #[test]
    fn two_qmk_keyboards_are_rejected() {
        let mut entries = qmk_keyboard("a");
        entries.extend(qmk_keyboard("b"));
        assert_eq!(select_target(&entries), Err(SelectError::Multiple(2)));
    }

    #[test]
    fn bluetooth_raw_interface_is_excluded() {
        let mut entries = keyboard("a");
        entries.push(entry("ble-raw", Transport::Other, QMK_USAGE_PAGE, 0x0061));
        let target = select_target(&entries).unwrap();
        assert_eq!(target.state, DeviceState::StockOrUpdate);
    }

    #[test]
    fn usages_are_grouped_by_interface() {
        let target = select_target(&keyboard("a")).unwrap();
        let cc = target.interfaces.iter().find(|i| i.path == "a-cc").unwrap();
        assert_eq!(cc.usages, vec![(0x000c, 0x0001), (0x0001, 0x0006)]);
    }
}
