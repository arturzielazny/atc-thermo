//! Client for the BLE command interface of the pvvx `ATC_MiThermometer` and `THB2`
//! firmwares.
//!
//! Each firmware exposes one GATT characteristic (ATC `0x1F1F`, THB2 `0xFFF4`) that
//! takes commands as writes and answers with notifications whose first byte echoes the
//! command id. [`Thermometer::connect`] detects the family by that characteristic; see
//! [`Firmware`] for where the two differ. The transport is `btleplug`, so the crate
//! works with BlueZ (Linux), CoreBluetooth (macOS) and WinRT.
//! [`Thermometer`] wraps a connection; the value types ([`Config`], [`Coefficients`],
//! [`ClockReading`], [`LogRecord`], [`ScreenMessage`]) are plain data with the firmware's binary layouts.
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

pub use clock::{ClockReading, NOMINAL_SECOND_TICKS, trimmed_second_ticks};
pub use coefficients::{
    ATC_MAX_GAIN, Coefficients, Correction, SensorInfo, THB_AHT_MAX_GAIN, THB_MAX_GAIN,
};
pub use config::{CONFIG_LEN, Config, THB_CONFIG_LEN, TX_POWER_PRESETS_DBM, TxPower};
pub use device_id::DeviceId;
pub use error::Error;
pub use firmware::{DeviceInfo, Firmware};
pub use log::LogRecord;
pub use screen::{ScreenMessage, Smiley};
pub use thermometer::{
    Adapter, MAX_NAME_LEN, Peripheral, Thermometer, default_adapter, device_id, disconnect,
    find_peripheral,
};
