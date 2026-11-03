//! The Bluetooth adapter handle shared by the scanner and the device jobs, and when
//! each device was last heard.
//!
//! On macOS a device can only be connected through the `CBCentralManager` that
//! discovered it, so scanning and connecting must use the same adapter object.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use atc::{Adapter, DeviceId};
use tokio::sync::Mutex;

/// Whether scanning stops while device jobs run. BlueZ connects more reliably with
/// discovery off. CoreBluetooth (btleplug) forgets a peripheral when it disconnects
/// and lists it again only once a scan sees it, and connects best right after an
/// advert, so elsewhere scanning goes on.
pub const PAUSE_SCAN_FOR_JOBS: bool = cfg!(target_os = "linux");

#[derive(Clone, Default)]
pub struct Ble {
    adapter: Arc<Mutex<Option<Adapter>>>,
    heard: Arc<std::sync::Mutex<HashMap<DeviceId, Instant>>>,
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

    /// Records an advert from `id`, received now.
    pub fn heard(&self, id: DeviceId) {
        if let Ok(mut heard) = self.heard.lock() {
            heard.insert(id, Instant::now());
        }
    }

    /// When `id` last advertised, if it has since this process started.
    pub fn last_heard(&self, id: DeviceId) -> Option<Instant> {
        self.heard.lock().ok()?.get(&id).copied()
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
        ble.heard(id);
        assert!(ble.last_heard(id).is_some_and(|at| at >= before));
    }
}
