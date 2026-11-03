//! Bluetooth for thermo: the scan for BTHome adverts and connections to thermometers,
//! with one backend per platform behind the same API.
//!
//! A backend provides [`Bluetooth`], a handle made with `Bluetooth::default()` and
//! cheap to clone, with
//!
//! - `scan(events, controls)`: forwards adverts as [`ScanEvent`]s until the receiver
//!   is gone, following [`ScanControl`]s, and recovers from stack failures by itself;
//! - `connect(id)`: a [`Thermometer`] ready for commands;
//! - `release(id)`: drops a connection a cancelled job may have left open;
//!
//! and [`Link`], its [`atc::Transport`]. Each platform's quirks stay in its backend,
//! so the app and its job worker never ask which platform they run on.
//!
//! Backends: BlueZ on Linux (`bluez.rs`).

use std::time::SystemTime;

use atc::DeviceId;
use uuid::Uuid;

#[cfg(target_os = "linux")]
mod bluez;
#[cfg(target_os = "linux")]
pub use bluez::{Bluetooth, Link};

#[cfg(not(target_os = "linux"))]
compile_error!(
    "no Bluetooth backend for this platform yet: see \"Porting to another OS\" in AGENTS.md"
);

/// A connection to a thermometer through this platform's backend.
pub type Thermometer = atc::Thermometer<Link>;

// Connections are handed to background tasks, so they must stay `Send`.
const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<Thermometer>();
};

/// BTHome's 16-bit UUID expanded with the Bluetooth base UUID.
const BTHOME_UUID: Uuid = Uuid::from_u128(
    ((bthome::SERVICE_UUID16 as u128) << 96) | 0x0000_0000_0000_1000_8000_0080_5f9b_34fb,
);

/// Something the scanner wants the app to know.
#[derive(Debug)]
pub enum ScanEvent {
    /// Scanning is running on the named adapter.
    Started { adapter: String },
    /// A device sent new BTHome service data.
    Advert(Advert),
    /// Scanning stopped; it starts over every few seconds until it works again.
    Failed(String),
}

/// What the app tells the scanner about device connections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanControl {
    /// Device jobs are about to connect. Where the stack connects badly while
    /// scanning (BlueZ), scanning stops until [`ScanControl::Resume`].
    Pause,
    /// Device jobs are over.
    Resume,
}

/// One BTHome advertisement, still undecoded.
#[derive(Debug, Clone)]
pub struct Advert {
    pub address: DeviceId,
    pub name: String,
    pub rssi: Option<i16>,
    pub payload: Vec<u8>,
    pub received: SystemTime,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bthome_uuid_matches_spec() {
        assert_eq!(
            BTHOME_UUID.to_string(),
            "0000fcd2-0000-1000-8000-00805f9b34fb"
        );
    }
}
