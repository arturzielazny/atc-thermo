//! [`Transport`]: what [`Thermometer`](crate::Thermometer) needs from a BLE stack.

use std::pin::Pin;

use futures::Stream;

use crate::Error;

/// Notifications from the command characteristic in order of arrival; the stream ends
/// when the connection drops.
pub type Notifications = Pin<Box<dyn Stream<Item = Vec<u8>> + Send>>;

/// An open connection to a device's command characteristic
/// ([`ATC_COMMAND_UUID`](crate::ATC_COMMAND_UUID) or
/// [`THB_COMMAND_UUID`](crate::THB_COMMAND_UUID)), as a platform's BLE backend
/// provides it. Commands, retries and byte layouts sit above
/// this and are the same everywhere.
pub trait Transport: Send {
    /// Writes `payload` to the command characteristic.
    fn write(&mut self, payload: &[u8]) -> impl Future<Output = Result<(), Error>> + Send;

    /// Closes the connection.
    fn disconnect(self) -> impl Future<Output = Result<(), Error>> + Send;
}
