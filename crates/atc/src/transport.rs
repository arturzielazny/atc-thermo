//! [`Transport`]: what [`Thermometer`](crate::Thermometer) needs from a BLE stack.

use std::pin::Pin;

use futures::Stream;
use uuid::Uuid;

use crate::Error;

/// The firmware's command/notify characteristic `0x1F1F` (`src/ble.c`).
pub const COMMAND_UUID: Uuid = Uuid::from_u128(0x0000_1f1f_0000_1000_8000_0080_5f9b_34fb);

/// Notifications from the command characteristic in order of arrival; the stream ends
/// when the connection drops.
pub type Notifications = Pin<Box<dyn Stream<Item = Vec<u8>> + Send>>;

/// An open connection to a device's command characteristic ([`COMMAND_UUID`]), as a
/// platform's BLE backend provides it. Commands, retries and byte layouts sit above
/// this and are the same everywhere.
pub trait Transport: Send {
    /// Writes `payload` to the command characteristic.
    fn write(&mut self, payload: &[u8]) -> impl Future<Output = Result<(), Error>> + Send;

    /// Closes the connection.
    fn disconnect(self) -> impl Future<Output = Result<(), Error>> + Send;
}
