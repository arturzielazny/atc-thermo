//! Client for the BLE command interface of the pvvx `ATC_MiThermometer` firmware.
//!
//! The firmware exposes one GATT characteristic (UUID `0x1F1F`) that takes commands as
//! writes and answers with notifications whose first byte echoes the command id.
//! [`Thermometer`] wraps a connection; the value types ([`Config`], [`Coefficients`],
//! [`ClockReading`], [`LogRecord`], [`ScreenMessage`]) are plain data with the firmware's binary layouts.
//!
//! Layouts follow `src/app.h`, `src/sensor.h`, `src/logger.h` and `src/cmd_parser.c` of
//! <https://github.com/pvvx/ATC_MiThermometer>, firmware v4.7 and newer.

mod clock;
mod coefficients;
mod config;
mod error;
mod log;
mod screen;
mod thermometer;

pub use clock::{ClockReading, NOMINAL_SECOND_TICKS, trimmed_second_ticks};
pub use coefficients::{Coefficients, Correction};
pub use config::{Config, TxPower};
pub use error::Error;
pub use log::LogRecord;
pub use screen::{ScreenMessage, Smiley};
pub use thermometer::{DeviceInfo, MAX_NAME_LEN, Thermometer};
