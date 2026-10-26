use std::pin::Pin;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bluer::gatt::remote::Characteristic;
use bluer::{Adapter, Address, Device, Uuid};
use futures::{Stream, StreamExt};
use tokio::time::{Instant, timeout, timeout_at};

use crate::coefficients::COEFFICIENTS_LEN;
use crate::config::CONFIG_LEN;
use crate::log::LOG_RECORD_LEN;
use crate::{ClockReading, Coefficients, Config, Error, LogRecord};

/// The firmware's command/notify characteristic `0x1F1F`.
const COMMAND_UUID: Uuid = Uuid::from_u128(0x0000_1f1f_0000_1000_8000_0080_5f9b_34fb);

const CMD_DEV_ID: u8 = 0x00;
const CMD_NAME: u8 = 0x01;
const CMD_CLOCK: u8 = 0x23;
const CMD_COEFFICIENTS: u8 = 0x25;
const CMD_COEFFICIENTS_DEFAULT: u8 = 0x26;
const CMD_LOG: u8 = 0x35;
const CMD_CONFIG: u8 = 0x55;

/// Oldest firmware whose config layout this crate knows (BCD, v4.7).
const MIN_VERSION: u16 = 0x47;
/// Longest name the firmware stores.
pub const MAX_NAME_LEN: usize = 20;

const CONNECT_ATTEMPTS: u32 = 4;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(25);
const CONNECT_RETRY_PAUSE: Duration = Duration::from_secs(1);
/// How long to wait for a device BlueZ has forgotten to advertise again.
const DEVICE_APPEAR_TIMEOUT: Duration = Duration::from_secs(30);
/// Writes are unacknowledged, so a command is resent if no answer arrives in time.
const COMMAND_ATTEMPTS: u32 = 5;
const REPLY_TIMEOUT: Duration = Duration::from_millis(1500);
/// Longest pause between two log records before the download counts as stalled.
const LOG_RECORD_TIMEOUT: Duration = Duration::from_secs(5);

type Notifications = Pin<Box<dyn Stream<Item = Vec<u8>> + Send>>;

/// Identity reported by the firmware (`CMD_ID_DEV_ID`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Hardware revision code (for LYWSD03MMC: 4 = B1.6, ...).
    pub hardware: u16,
    /// Firmware version, BCD: `0x59` is v5.9.
    pub firmware: u16,
}

impl DeviceInfo {
    /// Firmware version as text, e.g. `"5.9"`.
    #[must_use]
    pub fn firmware_version(&self) -> String {
        format!("{}.{}", self.firmware >> 4, self.firmware & 0xf)
    }
}

/// An open connection to a thermometer running pvvx firmware.
///
/// Only one command runs at a time; methods take `&mut self`.
pub struct Thermometer {
    device: Device,
    command: Characteristic,
    notifications: Notifications,
    info: DeviceInfo,
}

impl Thermometer {
    /// Connects to a device BlueZ already knows (i.e. one seen while scanning).
    ///
    /// # Errors
    ///
    /// Fails if the connection cannot be made, the device is not running pvvx
    /// firmware, or the firmware is older than v4.7.
    pub async fn connect(adapter: &Adapter, address: Address) -> Result<Self, Error> {
        let device = adapter.device(address)?;
        let mut attempt = 0;
        loop {
            wait_until_known(adapter, address).await?;
            if device.is_connected().await? {
                break;
            }
            attempt += 1;
            let error = match timeout(CONNECT_TIMEOUT, device.connect()).await {
                Ok(Ok(())) => continue,
                Ok(Err(err)) => Error::Bluetooth(err),
                Err(_) => Error::Timeout { command: 0 },
            };
            // A timed-out or concurrent attempt may still be pending inside BlueZ, which
            // makes the next one fail with "in progress"; cancelling it clears that.
            let _ = device.disconnect().await;
            if attempt >= CONNECT_ATTEMPTS {
                return Err(error);
            }
            tokio::time::sleep(CONNECT_RETRY_PAUSE).await;
        }

        let mut command = None;
        for service in device.services().await? {
            for characteristic in service.characteristics().await? {
                if characteristic.uuid().await? == COMMAND_UUID {
                    command = Some(characteristic);
                }
            }
        }
        let command = command.ok_or(Error::NotPvvx)?;
        let notifications: Notifications = Box::pin(command.notify().await?);
        let mut thermometer = Self {
            device,
            command,
            notifications,
            info: DeviceInfo {
                hardware: 0,
                firmware: 0,
            },
        };

        let reply = thermometer.command(&[CMD_DEV_ID], 6).await?;
        thermometer.info = DeviceInfo {
            hardware: u16::from_le_bytes([reply[2], reply[3]]),
            firmware: u16::from_le_bytes([reply[4], reply[5]]),
        };
        if thermometer.info.firmware < MIN_VERSION {
            return Err(Error::UnsupportedFirmware {
                version: thermometer.info.firmware,
            });
        }
        Ok(thermometer)
    }

    #[must_use]
    pub fn address(&self) -> Address {
        self.device.address()
    }

    #[must_use]
    pub fn info(&self) -> DeviceInfo {
        self.info
    }

    /// # Errors
    ///
    /// Fails if the device does not answer.
    pub async fn name(&mut self) -> Result<String, Error> {
        let reply = self.command(&[CMD_NAME], 1).await?;
        Ok(decode_name(&reply[1..]))
    }

    /// Stores a new name; the device restarts and uses it once disconnected.
    ///
    /// Returns the name as stored. An empty name restores the default `ATC_xxxxxx`.
    ///
    /// # Errors
    ///
    /// Fails if the name is longer than [`MAX_NAME_LEN`] bytes or the device does not answer.
    pub async fn set_name(&mut self, name: &str) -> Result<String, Error> {
        if name.len() > MAX_NAME_LEN {
            return Err(Error::InvalidValue(format!(
                "name is {} bytes, max {MAX_NAME_LEN}",
                name.len()
            )));
        }
        let mut payload = vec![CMD_NAME];
        if name.is_empty() {
            payload.push(0);
        } else {
            payload.extend_from_slice(name.as_bytes());
        }
        let reply = self.command(&payload, 1).await?;
        Ok(decode_name(&reply[1..]))
    }

    /// # Errors
    ///
    /// Fails if the device does not answer.
    pub async fn config(&mut self) -> Result<Config, Error> {
        let reply = self.command(&[CMD_CONFIG], 2 + CONFIG_LEN).await?;
        Ok(config_from_reply(&reply))
    }

    /// Writes the config and returns it as the device accepted it (it may clamp values).
    ///
    /// # Errors
    ///
    /// Fails if the device does not answer.
    pub async fn set_config(&mut self, config: &Config) -> Result<Config, Error> {
        let mut payload = vec![CMD_CONFIG];
        payload.extend_from_slice(config.as_bytes());
        let reply = self.command(&payload, 2 + CONFIG_LEN).await?;
        Ok(config_from_reply(&reply))
    }

    /// # Errors
    ///
    /// Fails if the device does not answer.
    pub async fn coefficients(&mut self) -> Result<Coefficients, Error> {
        let reply = self
            .command(&[CMD_COEFFICIENTS], 1 + COEFFICIENTS_LEN)
            .await?;
        Ok(coefficients_from_reply(&reply))
    }

    /// Writes new sensor coefficients (persisted in flash) and returns them as read back.
    ///
    /// # Errors
    ///
    /// Fails if the device does not answer.
    pub async fn set_coefficients(
        &mut self,
        coefficients: &Coefficients,
    ) -> Result<Coefficients, Error> {
        let mut payload = vec![CMD_COEFFICIENTS];
        payload.extend_from_slice(&coefficients.to_bytes());
        let reply = self.command(&payload, 1 + COEFFICIENTS_LEN).await?;
        Ok(coefficients_from_reply(&reply))
    }

    /// Restores the factory coefficients for the sensor type.
    ///
    /// # Errors
    ///
    /// Fails if the device does not answer.
    pub async fn reset_coefficients(&mut self) -> Result<Coefficients, Error> {
        let reply = self
            .command(&[CMD_COEFFICIENTS_DEFAULT], 1 + COEFFICIENTS_LEN)
            .await?;
        Ok(coefficients_from_reply(&reply))
    }

    /// # Errors
    ///
    /// Fails if the device does not answer.
    pub async fn clock(&mut self) -> Result<ClockReading, Error> {
        let reply = self.command(&[CMD_CLOCK], 5).await?;
        Ok(clock_from_reply(&reply, SystemTime::now()))
    }

    /// Sets the device clock to `time` and returns a reading taken right after.
    ///
    /// # Errors
    ///
    /// Fails if the device does not answer.
    pub async fn set_clock(&mut self, time: SystemTime) -> Result<ClockReading, Error> {
        let secs = time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let secs =
            u32::try_from(secs).map_err(|_| Error::InvalidValue("time beyond 2106".into()))?;
        let mut payload = vec![CMD_CLOCK];
        payload.extend_from_slice(&secs.to_le_bytes());
        let reply = self.command(&payload, 5).await?;
        Ok(clock_from_reply(&reply, SystemTime::now()))
    }

    /// Downloads log records, newest first, while `wanted` returns `true` for them.
    ///
    /// Reads at most `max_records`. The first record `wanted` rejects is not included
    /// and ends the download.
    ///
    /// # Errors
    ///
    /// Fails if the device stops sending before the end of the log.
    pub async fn read_log(
        &mut self,
        max_records: u16,
        mut wanted: impl FnMut(&LogRecord) -> bool,
    ) -> Result<Vec<LogRecord>, Error> {
        let [lo, hi] = max_records.to_le_bytes();
        let mut records = Vec::new();
        let mut attempts = 0;
        'start: loop {
            attempts += 1;
            self.write(&[CMD_LOG, lo, hi]).await?;
            loop {
                let Some(reply) = self.next_reply(CMD_LOG, LOG_RECORD_TIMEOUT).await? else {
                    // The start request can get lost like any write; ask again.
                    if records.is_empty() && attempts < COMMAND_ATTEMPTS {
                        continue 'start;
                    }
                    return Err(Error::Timeout { command: CMD_LOG });
                };
                let bytes = reply
                    .get(3..3 + LOG_RECORD_LEN)
                    .and_then(|b| <&[u8; LOG_RECORD_LEN]>::try_from(b).ok());
                let Some(bytes) = bytes else {
                    // `[0x35, 0, 0]` marks the end of the log.
                    return Ok(records);
                };
                let record = LogRecord::from_bytes(bytes);
                if !wanted(&record) {
                    self.write(&[CMD_LOG, 0, 0]).await?;
                    // Swallow the end marker so it doesn't confuse the next command.
                    while let Some(reply) = self.next_reply(CMD_LOG, REPLY_TIMEOUT).await? {
                        if reply.len() < 3 + LOG_RECORD_LEN {
                            break;
                        }
                    }
                    return Ok(records);
                }
                records.push(record);
            }
        }
    }

    /// # Errors
    ///
    /// Fails if BlueZ cannot disconnect.
    pub async fn disconnect(self) -> Result<(), Error> {
        Ok(self.device.disconnect().await?)
    }

    async fn write(&mut self, payload: &[u8]) -> Result<(), Error> {
        // `&mut` keeps the futures `Send` without requiring `Thermometer: Sync`.
        Ok(self.command.write(payload).await?)
    }

    /// Sends a command and returns its reply, which must be at least `min_len` bytes.
    async fn command(&mut self, payload: &[u8], min_len: usize) -> Result<Vec<u8>, Error> {
        let command = payload[0];
        for _ in 0..COMMAND_ATTEMPTS {
            self.write(payload).await?;
            if let Some(reply) = self.next_reply(command, REPLY_TIMEOUT).await? {
                if reply.len() < min_len {
                    return Err(Error::ShortReply {
                        command,
                        len: reply.len(),
                    });
                }
                return Ok(reply);
            }
        }
        Err(Error::Timeout { command })
    }

    /// Waits for the next notification answering `command`, skipping unrelated ones.
    async fn next_reply(&mut self, command: u8, wait: Duration) -> Result<Option<Vec<u8>>, Error> {
        let deadline = Instant::now() + wait;
        loop {
            match timeout_at(deadline, self.notifications.next()).await {
                Ok(Some(data)) if data.first() == Some(&command) => return Ok(Some(data)),
                Ok(Some(_)) => {}
                Ok(None) => return Err(Error::Disconnected),
                Err(_) => return Ok(None),
            }
        }
    }
}

/// Waits for BlueZ to list the device. It drops devices it has not heard from in a
/// while (e.g. after discovery restarts) and lists them again on their next advert.
async fn wait_until_known(adapter: &Adapter, address: Address) -> Result<(), Error> {
    let deadline = Instant::now() + DEVICE_APPEAR_TIMEOUT;
    while !adapter.device_addresses().await?.contains(&address) {
        if Instant::now() >= deadline {
            return Err(Error::InvalidValue(format!(
                "{address} is not in range (not seen by BlueZ)"
            )));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Ok(())
}

// Connections are handed to background tasks, so they must stay `Send`.
const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<Thermometer>();
};

fn decode_name(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Config replies are `[0x55, firmware version, cfg_t...]`.
fn config_from_reply(reply: &[u8]) -> Config {
    let bytes: [u8; CONFIG_LEN] = reply[2..2 + CONFIG_LEN]
        .try_into()
        .expect("caller checked the length");
    Config::from_bytes(bytes)
}

fn coefficients_from_reply(reply: &[u8]) -> Coefficients {
    let bytes: &[u8; COEFFICIENTS_LEN] = reply[1..=COEFFICIENTS_LEN]
        .try_into()
        .expect("caller checked the length");
    Coefficients::from_bytes(bytes)
}

/// Clock replies are `[0x23, now u32, (last set u32)]`.
fn clock_from_reply(reply: &[u8], host_now: SystemTime) -> ClockReading {
    let word = |at: usize| {
        reply
            .get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    ClockReading {
        device_now: word(1).expect("caller checked the length"),
        last_set: word(5),
        host_now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_replies() {
        let config = config_from_reply(&[
            0x55, 0x59, 0x87, 0x10, 0x0a, 0x06, 0x28, 0x04, 0xa9, 0x31, 0x31, 0x04, 0xb4, 0x00,
        ]);
        assert_eq!(config.measure_interval(), 4);

        let clock = clock_from_reply(&[0x23, 1, 0, 0, 0, 2, 0, 0, 0], UNIX_EPOCH);
        assert_eq!(clock.device_now, 1);
        assert_eq!(clock.last_set, Some(2));
        let clock = clock_from_reply(&[0x23, 1, 0, 0, 0], UNIX_EPOCH);
        assert_eq!(clock.last_set, None);

        assert_eq!(decode_name(b"Kitchen\0\0"), "Kitchen");
    }
}
