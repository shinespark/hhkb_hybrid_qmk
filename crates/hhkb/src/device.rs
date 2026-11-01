use std::collections::BTreeMap;
use std::fmt;

pub const VENDOR_ID: u16 = 0x04fe;
pub const PRODUCT_ID: u16 = 0x0021;
pub const VENDOR_USAGE_PAGE: u16 = 0xff00;

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
                "ベンダー定義のインターフェース (Usage Page 0xFF00) が見つかりません。USB を接続し直してください"
            ),
            Self::Multiple(count) => write!(
                f,
                "USB 接続の HHKB が {count} 台見つかりました。対象の 1 台だけを接続してください"
            ),
        }
    }
}

impl std::error::Error for SelectError {}

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
        .filter(|usages| usages.iter().any(|(page, _)| *page == VENDOR_USAGE_PAGE))
        .count();
    match keyboards {
        0 => return Err(SelectError::MissingVendorInterface),
        1 => {}
        count => return Err(SelectError::Multiple(count)),
    }

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

    #[test]
    fn usages_are_grouped_by_interface() {
        let target = select_target(&keyboard("a")).unwrap();
        let cc = target.interfaces.iter().find(|i| i.path == "a-cc").unwrap();
        assert_eq!(cc.usages, vec![(0x000c, 0x0001), (0x0001, 0x0006)]);
    }
}
