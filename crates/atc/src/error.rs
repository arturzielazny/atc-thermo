use std::error;
use std::fmt;

use crate::Firmware;

/// Why talking to a thermometer failed.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The BLE stack reported an error (not connected, connection failed, ...).
    Bluetooth(btleplug::Error),
    /// The device did not answer a command, even after retries.
    Timeout { command: u8 },
    /// The connection dropped while waiting for an answer.
    Disconnected,
    /// The device has no pvvx command characteristic.
    NotPvvx,
    /// The BLE stack does not list the device (out of range or not seen yet).
    NotFound(String),
    /// The firmware is older than the layouts this crate understands.
    UnsupportedFirmware { firmware: Firmware, version: u16 },
    /// The firmware has no such feature, e.g. clock rate trimming on THB2.
    Unsupported(&'static str),
    /// An answer was shorter than its layout requires.
    ShortReply { command: u8, len: usize },
    /// A requested value cannot be represented by the firmware.
    InvalidValue(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bluetooth(err) => write!(f, "bluetooth: {err}"),
            Self::Timeout { command } => write!(f, "no answer to command {command:#04x}"),
            Self::Disconnected => write!(f, "device disconnected"),
            Self::NotPvvx => write!(
                f,
                "not a pvvx ATC (0x1F1F) or THB2 (0xFFF4) firmware device"
            ),
            Self::NotFound(id) => write!(f, "{id} is not in range (not seen while scanning)"),
            Self::UnsupportedFirmware { firmware, version } => {
                let min = firmware.min_version();
                write!(
                    f,
                    "{firmware} firmware v{}.{} is too old (need v{}.{}+)",
                    version >> 4,
                    version & 0xf,
                    min >> 4,
                    min & 0xf
                )
            }
            Self::Unsupported(what) => write!(f, "firmware does not support {what}"),
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
            Self::Bluetooth(err) => Some(err),
            _ => None,
        }
    }
}

impl From<btleplug::Error> for Error {
    fn from(err: btleplug::Error) -> Self {
        Self::Bluetooth(err)
    }
}
