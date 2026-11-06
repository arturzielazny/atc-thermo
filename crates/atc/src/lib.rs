//! Client for the BLE command interface of the pvvx `ATC_MiThermometer` and `THB2`
//! firmwares.
//!
//! Each firmware exposes one GATT characteristic (ATC `0x1F1F`, THB2 `0xFFF4`) that
//! takes commands as writes and answers with notifications whose first byte echoes the
//! command id. [`Thermometer`] speaks that protocol over a [`Transport`], which a
//! platform's BLE backend provides (the `ble` crate); the backend tells the families
//! apart by their characteristic ([`Firmware::from_command_uuid`]), and [`Firmware`]
//! keeps where they differ. This crate does no Bluetooth I/O itself. The value types
//! ([`Config`], [`Coefficients`], [`ClockReading`], [`LogRecord`], [`ScreenMessage`])
//! are plain data with the firmware's binary layouts, and [`DeviceId`] names a device
//! across BLE stacks.
//!
//! Layouts follow `src/app.h`, `src/sensor.h`, `src/logger.h` and `src/cmd_parser.c` of
//! <https://github.com/pvvx/ATC_MiThermometer>, firmware v4.7 and newer, and
//! `config.h`, `sensors.h`, `logger.h` and `cmd_parser.c` of
//! <https://github.com/pvvx/THB2>, firmware v1.4 and newer.

mod clock;
mod coefficients;
mod config;
mod device_id;
mod error;
mod firmware;
mod log;
mod screen;
mod thermometer;
mod transport;

pub use clock::{ClockReading, NOMINAL_SECOND_TICKS, trimmed_second_ticks};
pub use coefficients::{
    ATC_MAX_GAIN, Coefficients, Correction, SensorInfo, THB_AHT_MAX_GAIN, THB_MAX_GAIN,
};
pub use config::{CONFIG_LEN, Config, THB_CONFIG_LEN, TX_POWER_PRESETS_DBM, TxPower};
pub use device_id::DeviceId;
pub use error::Error;
pub use firmware::{ATC_COMMAND_UUID, DeviceInfo, Firmware, THB_COMMAND_UUID};
pub use log::LogRecord;
pub use screen::{ScreenMessage, Smiley};
pub use thermometer::{MAX_NAME_LEN, Thermometer};
pub use transport::{Notifications, Transport};
