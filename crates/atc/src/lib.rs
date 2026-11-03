//! Client for the BLE command interface of the pvvx `ATC_MiThermometer` firmware.
//!
//! The firmware exposes one GATT characteristic (UUID `0x1F1F`) that takes commands as
//! writes and answers with notifications whose first byte echoes the command id.
//! [`Thermometer`] speaks that protocol over a [`Transport`], which a platform's BLE
//! backend provides (the `ble` crate); this crate does no Bluetooth I/O itself. The
//! value types ([`Config`], [`Coefficients`], [`ClockReading`], [`LogRecord`],
//! [`ScreenMessage`]) are plain data with the firmware's binary layouts, and
//! [`DeviceId`] names a device across BLE stacks.
//!
//! Layouts follow `src/app.h`, `src/sensor.h`, `src/logger.h` and `src/cmd_parser.c` of
//! <https://github.com/pvvx/ATC_MiThermometer>, firmware v4.7 and newer.

mod clock;
mod coefficients;
mod config;
mod device_id;
mod error;
mod log;
mod screen;
mod thermometer;
mod transport;

pub use clock::{ClockReading, NOMINAL_SECOND_TICKS, trimmed_second_ticks};
pub use coefficients::{Coefficients, Correction};
pub use config::{CONFIG_LEN, Config, TxPower};
pub use device_id::DeviceId;
pub use error::Error;
pub use log::LogRecord;
pub use screen::{ScreenMessage, Smiley};
pub use thermometer::{DeviceInfo, MAX_NAME_LEN, Thermometer};
pub use transport::{COMMAND_UUID, Notifications, Transport};
