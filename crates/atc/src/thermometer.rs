//! [`Thermometer`]: the firmware's command protocol over one open [`Transport`].

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::{FutureExt, StreamExt};
use tokio::time::{Instant, timeout_at};

use crate::clock::ticks_delta;
use crate::coefficients::COEFFICIENTS_LEN;
use crate::config::CONFIG_LEN;
use crate::log::LOG_RECORD_LEN;
use crate::screen::SCREEN_MESSAGE_LEN;
use crate::{
    ClockReading, Coefficients, Config, DeviceId, Error, LogRecord, Notifications, ScreenMessage,
    Transport,
};

const CMD_DEV_ID: u8 = 0x00;
const CMD_NAME: u8 = 0x01;
/// `CMD_ID_DEV_MAC`. With parameters it writes a new MAC to flash; only ever send it bare.
const CMD_MAC: u8 = 0x10;
const CMD_SCREEN: u8 = 0x22;
const CMD_CLOCK: u8 = 0x23;
const CMD_SECOND_TICKS: u8 = 0x24;
const CMD_COEFFICIENTS: u8 = 0x25;
const CMD_COEFFICIENTS_DEFAULT: u8 = 0x26;
const CMD_LOG: u8 = 0x35;
const CMD_CONFIG: u8 = 0x55;

/// Oldest firmware whose config layout this crate knows (BCD, v4.7).
const MIN_VERSION: u16 = 0x47;
/// Bytes in a MAC address.
const MAC_LEN: usize = 6;
/// Longest name the firmware stores.
pub const MAX_NAME_LEN: usize = 20;

/// Writes are unacknowledged, so a command is resent if no answer arrives in time.
const COMMAND_ATTEMPTS: u32 = 5;
const REPLY_TIMEOUT: Duration = Duration::from_millis(1500);
/// Longest pause between two log records before the download counts as stalled.
const LOG_RECORD_TIMEOUT: Duration = Duration::from_secs(5);

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
pub struct Thermometer<T> {
    transport: T,
    notifications: Notifications,
    info: DeviceInfo,
}

impl<T: Transport> Thermometer<T> {
    /// Starts talking to a device over a fresh connection: reads its identity and
    /// checks the firmware version. `notifications` must already be subscribed, so no
    /// reply is missed.
    ///
    /// # Errors
    ///
    /// Fails if the device does not answer or its firmware is older than v4.7.
    pub async fn open(transport: T, notifications: Notifications) -> Result<Self, Error> {
        let mut thermometer = Self {
            transport,
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
    pub fn info(&self) -> DeviceInfo {
        self.info
    }

    /// Reads the device's own MAC address, which CoreBluetooth hides from scans.
    ///
    /// # Errors
    ///
    /// Fails if the device does not answer.
    pub async fn mac(&mut self) -> Result<DeviceId, Error> {
        let reply = self.command(&[CMD_MAC], 2 + MAC_LEN).await?;
        Ok(mac_from_reply(&reply))
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

    /// Sets the device clock to this computer's time and returns a reading taken right
    /// after.
    ///
    /// The time is read again for every attempt, so a resent command doesn't set the
    /// time of the first one.
    ///
    /// # Errors
    ///
    /// Fails if the device does not answer.
    pub async fn set_clock_to_now(&mut self) -> Result<ClockReading, Error> {
        let reply = self
            .command_with(CMD_CLOCK, 5, || {
                let secs = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs());
                let secs = u32::try_from(secs)
                    .map_err(|_| Error::InvalidValue("time beyond 2106".into()))?;
                Ok(secs.to_le_bytes().to_vec())
            })
            .await?;
        Ok(clock_from_reply(&reply, SystemTime::now()))
    }

    /// Shows `message` on the LCD, alternating with the readings.
    ///
    /// # Errors
    ///
    /// Fails if the message cannot be displayed, or the device does not answer or
    /// stores something else.
    pub async fn show(&mut self, message: &ScreenMessage) -> Result<(), Error> {
        let bytes = message.to_bytes()?;
        let mut payload = vec![CMD_SCREEN];
        payload.extend_from_slice(&bytes);
        let reply = self.command(&payload, 1 + SCREEN_MESSAGE_LEN).await?;
        if reply[1..=SCREEN_MESSAGE_LEN] != bytes {
            return Err(Error::InvalidValue(format!(
                "device stored screen data {:02x?} instead of {bytes:02x?}",
                &reply[1..=SCREEN_MESSAGE_LEN]
            )));
        }
        Ok(())
    }

    /// System timer ticks the device counts as one second of its clock, nominally
    /// [`NOMINAL_SECOND_TICKS`](crate::NOMINAL_SECOND_TICKS).
    ///
    /// # Errors
    ///
    /// Fails if the device does not answer, e.g. firmware built without time adjustment.
    pub async fn second_ticks(&mut self) -> Result<u32, Error> {
        let reply = self.command(&[CMD_SECOND_TICKS], 5).await?;
        Ok(u32::from_le_bytes([reply[1], reply[2], reply[3], reply[4]]))
    }

    /// Sets the length of a clock second; the device keeps it in flash.
    ///
    /// # Errors
    ///
    /// Fails if `ticks` is outside nominal ± 32767, or the device does not answer or
    /// stores something else.
    pub async fn set_second_ticks(&mut self, ticks: u32) -> Result<u32, Error> {
        let delta = ticks_delta(ticks).ok_or_else(|| {
            Error::InvalidValue(format!("{ticks} ticks per second is out of range"))
        })?;
        let [lo, hi] = delta.to_le_bytes();
        let reply = self.command(&[CMD_SECOND_TICKS, lo, hi], 5).await?;
        let stored = u32::from_le_bytes([reply[1], reply[2], reply[3], reply[4]]);
        if stored != ticks {
            return Err(Error::InvalidValue(format!(
                "device stored {stored} ticks per second instead of {ticks}"
            )));
        }
        Ok(stored)
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
        self.discard_pending();
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
    /// Fails if the BLE stack cannot disconnect.
    pub async fn disconnect(self) -> Result<(), Error> {
        self.transport.disconnect().await
    }

    async fn write(&mut self, payload: &[u8]) -> Result<(), Error> {
        // `&mut` keeps the futures `Send` without requiring `Thermometer: Sync`.
        self.transport.write(payload).await
    }

    /// Sends a command and returns its reply, which must be at least `min_len` bytes.
    async fn command(&mut self, payload: &[u8], min_len: usize) -> Result<Vec<u8>, Error> {
        self.command_with(payload[0], min_len, || Ok(payload[1..].to_vec()))
            .await
    }

    /// Like [`Self::command`], with the arguments after the command id built anew for
    /// every attempt.
    async fn command_with(
        &mut self,
        command: u8,
        min_len: usize,
        mut arguments: impl FnMut() -> Result<Vec<u8>, Error>,
    ) -> Result<Vec<u8>, Error> {
        self.discard_pending();
        for attempt in 1..=COMMAND_ATTEMPTS {
            let mut payload = vec![command];
            payload.extend(arguments()?);
            self.write(&payload).await?;
            if let Some(reply) = self.next_reply(command, REPLY_TIMEOUT).await? {
                if attempt > 1 {
                    // The earlier writes may be answered too, and such an answer
                    // would pass for the reply to the next command with this id
                    // (e.g. reading coefficients, then writing them).
                    while self.next_reply(command, REPLY_TIMEOUT).await?.is_some() {}
                }
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

    /// Drops notifications that arrived after their command had given up waiting.
    fn discard_pending(&mut self) {
        while let Some(Some(_)) = self.notifications.next().now_or_never() {}
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

/// MAC replies are `[0x10, 8, MAC least significant byte first, random static[2]]`.
fn mac_from_reply(reply: &[u8]) -> DeviceId {
    let mut mac: [u8; MAC_LEN] = reply[2..2 + MAC_LEN]
        .try_into()
        .expect("caller checked the length");
    mac.reverse();
    DeviceId::Mac(mac)
}

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

        // Recorded from A4:C1:38:A0:C9:52 on firmware v5.9.
        let mac = mac_from_reply(&[0x10, 0x08, 0x52, 0xc9, 0xa0, 0x38, 0xc1, 0xa4, 0x20, 0x34]);
        assert_eq!(mac.to_string(), "A4:C1:38:A0:C9:52");
    }
}
