use std::error;
use std::fmt;

use crate::DeviceId;

/// Why talking to a thermometer failed.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The BLE stack reported an error (not connected, connection failed, ...).
    Bluetooth(Box<dyn error::Error + Send + Sync>),
    /// The BLE stack has not heard the device recently, so it cannot connect to it.
    NotInRange(DeviceId),
    /// No connection was made in time.
    ConnectTimeout,
    /// The device did not answer a command, even after retries.
    Timeout { command: u8 },
    /// The connection dropped while waiting for an answer.
    Disconnected,
    /// The device has no pvvx command characteristic.
    NotPvvx,
    /// The firmware is older than the layouts this crate understands.
    UnsupportedFirmware { version: u16 },
    /// An answer was shorter than its layout requires.
    ShortReply { command: u8, len: usize },
    /// A requested value cannot be represented by the firmware.
    InvalidValue(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bluetooth(err) => write!(f, "bluetooth: {err}"),
            Self::NotInRange(id) => write!(f, "{id} is not in range"),
            Self::ConnectTimeout => write!(f, "connecting timed out"),
            Self::Timeout { command } => write!(f, "no answer to command {command:#04x}"),
            Self::Disconnected => write!(f, "device disconnected"),
            Self::NotPvvx => write!(f, "not a pvvx ATC firmware device"),
            Self::UnsupportedFirmware { version } => write!(
                f,
                "firmware v{}.{} is too old (need v4.7+)",
                version >> 4,
                version & 0xf
            ),
            Self::ShortReply { command, len } => {
                write!(f, "reply to command {command:#04x} too short ({len} bytes)")
            }
            Self::InvalidValue(msg) => write!(f, "{msg}"),
        }
    }
}

impl error::Error for Error {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            Self::Bluetooth(err) => Some(err.as_ref()),
            _ => None,
        }
    }
}
