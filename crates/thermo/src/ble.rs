//! The Bluetooth adapter handle shared by the scanner and the device jobs, and when
//! each device was last heard and which firmware it runs.
//!
//! On macOS a device can only be connected through the `CBCentralManager` that
//! discovered it, so scanning and connecting must use the same adapter object.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use atc::{Adapter, DeviceId, Firmware};
use tokio::sync::Mutex;

/// Whether scanning stops while device jobs run. BlueZ connects more reliably with
/// discovery off. CoreBluetooth (btleplug) forgets a peripheral when it disconnects
/// and lists it again only once a scan sees it, and connects best right after an
/// advert, so elsewhere scanning goes on.
pub const PAUSE_SCAN_FOR_JOBS: bool = cfg!(target_os = "linux");

#[derive(Clone, Default)]
pub struct Ble {
    adapter: Arc<Mutex<Option<Adapter>>>,
    heard: Arc<std::sync::Mutex<HashMap<DeviceId, Heard>>>,
}

#[derive(Debug, Clone, Copy)]
struct Heard {
    at: Instant,
    /// Learnt on connecting, else guessed from the advertised name.
    firmware: Option<Firmware>,
    learnt: bool,
}

/// The firmware a device name suggests: each firmware's default names start with
/// the model (THB2 `DEF_MODEL_NUMBER_STR` in `config.h`, ATC `ATC_` or the Xiaomi or
/// Qingping model), until the user renames the device.
pub fn firmware_from_name(name: &str) -> Option<Firmware> {
    const THB: [&str; 6] = ["THB", "BTH01", "TH04", "TH05", "HDP16", "TN6AT"];
    const ATC: [&str; 7] = ["ATC_", "LYWSD", "MHO", "CGG", "CGDK", "MJWSD", "XMWSDJ"];
    if THB.iter().any(|p| name.starts_with(p)) {
        Some(Firmware::Thb)
    } else if ATC.iter().any(|p| name.starts_with(p)) {
        Some(Firmware::Atc)
    } else {
        None
    }
}

impl Ble {
    /// The shared adapter, opened on first use.
    pub async fn adapter(&self) -> Result<Adapter, atc::Error> {
        let mut slot = self.adapter.lock().await;
        if let Some(adapter) = slot.as_ref() {
            return Ok(adapter.clone());
        }
        let adapter = atc::default_adapter().await?;
        *slot = Some(adapter.clone());
        Ok(adapter)
    }

    /// Drops the adapter so the next [`Self::adapter`] opens it anew, e.g. after
    /// `bluetoothd` restarted.
    pub async fn reset(&self) {
        *self.adapter.lock().await = None;
    }

    /// Records an advert from `id`, named `name`, received now.
    pub fn heard(&self, id: DeviceId, name: &str) {
        let Ok(mut heard) = self.heard.lock() else {
            return;
        };
        let now = Instant::now();
        let entry = heard.entry(id).or_insert(Heard {
            at: now,
            firmware: None,
            learnt: false,
        });
        entry.at = now;
        if !entry.learnt {
            entry.firmware = firmware_from_name(name);
        }
    }

    /// Records the firmware a connection found on `id`.
    pub fn learnt(&self, id: DeviceId, firmware: Firmware) {
        if let Ok(mut heard) = self.heard.lock()
            && let Some(entry) = heard.get_mut(&id)
        {
            entry.firmware = Some(firmware);
            entry.learnt = true;
        }
    }

    /// When `id` last advertised, if it has since this process started.
    pub fn last_heard(&self, id: DeviceId) -> Option<Instant> {
        Some(self.heard.lock().ok()?.get(&id)?.at)
    }

    /// The firmware `id` runs, as far as known.
    pub fn firmware(&self, id: DeviceId) -> Option<Firmware> {
        self.heard.lock().ok()?.get(&id)?.firmware
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_last_advert() {
        let ble = Ble::default();
        let id = DeviceId::Mac([1, 2, 3, 4, 5, 6]);
        assert_eq!(ble.last_heard(id), None);
        let before = Instant::now();
        ble.heard(id, "Kitchen");
        assert!(ble.last_heard(id).is_some_and(|at| at >= before));
        assert_eq!(ble.firmware(id), None);
        ble.heard(id, "THB1-D2D067");
        assert_eq!(ble.firmware(id), Some(Firmware::Thb));
    }

    #[test]
    fn learnt_firmware_beats_the_name() {
        let ble = Ble::default();
        let id = DeviceId::Mac([1, 2, 3, 4, 5, 6]);
        ble.heard(id, "ATC_27C622");
        assert_eq!(ble.firmware(id), Some(Firmware::Atc));
        ble.learnt(id, Firmware::Thb);
        ble.heard(id, "ATC_27C622");
        assert_eq!(ble.firmware(id), Some(Firmware::Thb));
        assert_eq!(firmware_from_name("LYWSD03MMC"), Some(Firmware::Atc));
        assert_eq!(firmware_from_name("Kitchen"), None);
    }
}
