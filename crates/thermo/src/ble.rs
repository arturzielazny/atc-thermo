//! The Bluetooth adapter handle shared by the scanner and the device jobs.
//!
//! On macOS a device can only be connected through the `CBCentralManager` that
//! discovered it, so scanning and connecting must use the same adapter object.

use std::sync::Arc;

use atc::Adapter;
use tokio::sync::Mutex;

#[derive(Clone, Default)]
pub struct Ble(Arc<Mutex<Option<Adapter>>>);

impl Ble {
    /// The shared adapter, opened on first use.
    pub async fn adapter(&self) -> Result<Adapter, atc::Error> {
        let mut slot = self.0.lock().await;
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
        *self.0.lock().await = None;
    }
}
