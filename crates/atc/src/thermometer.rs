//! [`Thermometer`]: one open connection, over `btleplug`, to a device running pvvx
//! ATC or THB2 firmware.

use std::pin::Pin;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use btleplug::api::{
    BDAddr, Central as _, CharPropFlags, Characteristic, Manager as _, Peripheral as _, WriteType,
};
pub use btleplug::platform::{Adapter, Peripheral};
use futures::{FutureExt, Stream, StreamExt};
use tokio::time::{Instant, timeout, timeout_at};
use uuid::Uuid;

use crate::clock::ticks_delta;
use crate::coefficients::COEFFICIENTS_LEN;
use crate::firmware::DEV_ID_MIN_LEN;
use crate::log::LOG_RECORD_LEN;
use crate::screen::SCREEN_MESSAGE_LEN;
use crate::{
    ATC_MAX_GAIN, ClockReading, Coefficients, Config, DeviceId, DeviceInfo, Error, Firmware,
    LogRecord, ScreenMessage, SensorInfo,
};

const CMD_DEV_ID: u8 = 0x00;
const CMD_NAME: u8 = 0x01;
const CMD_SCREEN: u8 = 0x22;
const CMD_CLOCK: u8 = 0x23;
const CMD_SECOND_TICKS: u8 = 0x24;
const CMD_COEFFICIENTS: u8 = 0x25;
const CMD_COEFFICIENTS_DEFAULT: u8 = 0x26;
const CMD_LOG: u8 = 0x35;
const CMD_CONFIG: u8 = 0x55;

/// Longest name either firmware stores (THB2: `GAP_DEVICE_NAME_LEN` 21 incl. the 0).
pub const MAX_NAME_LEN: usize = 20;

/// Connection attempts per [`Thermometer::connect`]; callers retry whole jobs later.
const CONNECT_ATTEMPTS: u32 = 2;
/// CoreBluetooth never gives up on a connect by itself: it waits for the next advert.
/// Devices advertise every few seconds, so this covers several of them.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(12);
/// Pause before the second attempt.
const CONNECT_RETRY_PAUSE: Duration = Duration::from_secs(2);
const DISCOVER_TIMEOUT: Duration = Duration::from_secs(15);
const SUBSCRIBE_TIMEOUT: Duration = Duration::from_secs(8);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// `disconnect` waits for the stack's confirmation, which may never come for a
/// peripheral that is not (or not yet) connected.
const DISCONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long to wait for a device the BLE stack has forgotten to advertise again.
const DEVICE_APPEAR_TIMEOUT: Duration = Duration::from_secs(15);
/// Writes are unacknowledged, so a command is resent if no answer arrives in time.
const COMMAND_ATTEMPTS: u32 = 5;
const REPLY_TIMEOUT: Duration = Duration::from_millis(1500);
/// Longest pause between two log records before the download counts as stalled.
const LOG_RECORD_TIMEOUT: Duration = Duration::from_secs(5);

type Notifications = Pin<Box<dyn Stream<Item = Vec<u8>> + Send>>;

/// The first Bluetooth adapter of this machine.
///
/// On macOS every call opens a new `CBCentralManager`, which only knows the devices
/// it has scanned itself: create one and share it between scanning and connecting.
///
/// # Errors
///
/// Fails if the BLE stack is unavailable or there is no adapter.
pub async fn default_adapter() -> Result<Adapter, Error> {
    let manager = btleplug::platform::Manager::new().await?;
    manager
        .adapters()
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| Error::NotFound("bluetooth adapter".into()))
}

/// How this machine names `peripheral`: its MAC where the stack shows it (BlueZ,
/// WinRT), else the CoreBluetooth UUID.
#[must_use]
pub fn device_id(peripheral: &Peripheral) -> DeviceId {
    let address = peripheral.address();
    if address != BDAddr::default() {
        return DeviceId::Mac(address.into_inner());
    }
    Uuid::parse_str(&peripheral.id().to_string())
        .map_or(DeviceId::Mac(address.into_inner()), DeviceId::Uuid)
}

/// The peripheral the adapter lists under `id`, if any.
///
/// # Errors
///
/// Fails if the adapter cannot list peripherals.
pub async fn find_peripheral(adapter: &Adapter, id: DeviceId) -> Result<Option<Peripheral>, Error> {
    Ok(adapter
        .peripherals()
        .await?
        .into_iter()
        .find(|p| device_id(p) == id))
}

/// Drops a connection to `id` that may be left over, e.g. from a cancelled connect.
///
/// # Errors
///
/// Fails if the BLE stack does.
pub async fn disconnect(adapter: &Adapter, id: DeviceId) -> Result<(), Error> {
    if let Some(peripheral) = find_peripheral(adapter, id).await? {
        let connected = step("checking the connection", DISCONNECT_TIMEOUT, async {
            Ok(peripheral.is_connected().await?)
        })
        .await?;
        if connected {
            close(&peripheral, id).await?;
        }
    }
    Ok(())
}

/// Cancels a connection or a pending connect, waiting at most [`DISCONNECT_TIMEOUT`].
async fn close(peripheral: &Peripheral, id: DeviceId) -> Result<(), Error> {
    let started = Instant::now();
    let result = step("disconnecting", DISCONNECT_TIMEOUT, async {
        Ok(peripheral.disconnect().await?)
    })
    .await;
    match &result {
        Ok(()) => log::debug!("{id}: disconnected in {:?}", started.elapsed()),
        Err(err) => log::warn!("{id}: {err}"),
    }
    result
}

/// Runs one BLE stack call with a deadline, so a lost callback cannot hang a job.
async fn step<T>(
    name: &'static str,
    limit: Duration,
    call: impl Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    timeout(limit, call)
        .await
        .unwrap_or(Err(Error::StepTimeout {
            step: name,
            after: limit,
        }))
}

/// An open connection to a thermometer running pvvx firmware.
///
/// Only one command runs at a time; methods take `&mut self`.
pub struct Thermometer {
    peripheral: Peripheral,
    id: DeviceId,
    command: Characteristic,
    write_type: WriteType,
    notifications: Notifications,
    info: DeviceInfo,
    /// THB2 only: learned from the coefficient reply.
    sensor: Option<SensorInfo>,
}

impl Thermometer {
    /// Connects to a device the adapter already knows (i.e. one seen while scanning)
    /// and detects its firmware family by its command characteristic.
    ///
    /// # Errors
    ///
    /// Fails if the connection cannot be made, the device is not running pvvx
    /// firmware, or the firmware is older than [`Firmware::min_version`].
    pub async fn connect(adapter: &Adapter, id: DeviceId) -> Result<Self, Error> {
        let started = Instant::now();
        let peripheral = open(adapter, id).await?;
        log::debug!("{id}: connected after {:?}", started.elapsed());
        match Self::set_up(peripheral.clone(), id).await {
            Ok(thermometer) => {
                log::info!(
                    "{id}: ready in {:?}: {} {} v{:#x}, {:?}",
                    started.elapsed(),
                    thermometer.info.family,
                    thermometer.info.model().unwrap_or("unknown model"),
                    thermometer.info.firmware,
                    thermometer.write_type
                );
                Ok(thermometer)
            }
            Err(err) => {
                log::warn!("{id}: setup failed after {:?}: {err}", started.elapsed());
                let _ = close(&peripheral, id).await;
                Err(err)
            }
        }
    }

    /// Finds the command characteristic, subscribes to it (before any write, so no
    /// reply is missed) and reads the device id.
    async fn set_up(peripheral: Peripheral, id: DeviceId) -> Result<Self, Error> {
        let started = Instant::now();
        step("discovering services", DISCOVER_TIMEOUT, async {
            Ok(peripheral.discover_services().await?)
        })
        .await?;
        log::debug!("{id}: services discovered in {:?}", started.elapsed());
        let (family, command) = peripheral
            .characteristics()
            .into_iter()
            .find_map(|c| Firmware::from_command_uuid(c.uuid).map(|f| (f, c)))
            .ok_or(Error::NotPvvx)?;
        let write_type = write_type_for(command.properties);
        log::debug!(
            "{id}: {family} command characteristic {}, properties {:?}, writing {write_type:?}",
            command.uuid,
            command.properties
        );
        let uuid = command.uuid;
        let notifications: Notifications = Box::pin(
            peripheral
                .notifications()
                .await?
                .filter_map(move |n| futures::future::ready((n.uuid == uuid).then_some(n.value))),
        );
        let subscribing = Instant::now();
        step("subscribing to notifications", SUBSCRIBE_TIMEOUT, async {
            Ok(peripheral.subscribe(&command).await?)
        })
        .await?;
        log::debug!("{id}: subscribed in {:?}", subscribing.elapsed());
        let mut thermometer = Self {
            peripheral,
            id,
            command,
            write_type,
            notifications,
            info: DeviceInfo {
                family,
                hardware: 0,
                firmware: 0,
                services: None,
            },
            sensor: None,
        };

        let reply = thermometer.command(&[CMD_DEV_ID], DEV_ID_MIN_LEN).await?;
        thermometer.info = DeviceInfo::from_reply(family, &reply);
        if thermometer.info.firmware < family.min_version() {
            return Err(Error::UnsupportedFirmware {
                firmware: family,
                version: thermometer.info.firmware,
            });
        }
        Ok(thermometer)
    }

    #[must_use]
    pub fn id(&self) -> DeviceId {
        self.id
    }

    #[must_use]
    pub fn info(&self) -> DeviceInfo {
        self.info
    }

    #[must_use]
    pub fn firmware(&self) -> Firmware {
        self.info.family
    }

    /// The sensor a THB2 device reported with its last coefficient reply.
    #[must_use]
    pub fn sensor(&self) -> Option<SensorInfo> {
        self.sensor
    }

    /// Largest gain coefficient this device takes, for
    /// [`Coefficients::corrected_within`]. Reads the coefficients on THB2 if the
    /// sensor is not known yet.
    ///
    /// # Errors
    ///
    /// Fails if the device does not answer.
    pub async fn max_gain(&mut self) -> Result<u32, Error> {
        match self.firmware() {
            Firmware::Atc => Ok(ATC_MAX_GAIN),
            Firmware::Thb => {
                if self.sensor.is_none() {
                    self.coefficients().await?;
                }
                Ok(self
                    .sensor
                    .map_or(crate::THB_AHT_MAX_GAIN, |s| s.max_gain()))
            }
        }
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
    /// Returns the name as stored. An empty name restores the default (`ATC_xxxxxx`, `THB1-xxxxxx`, ...).
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
        let family = self.firmware();
        let reply = self
            .command(&[CMD_CONFIG], config_reply_len(family))
            .await?;
        Ok(config_from_reply(family, &reply))
    }

    /// Writes the config and returns it as the device accepted it (it may clamp values).
    ///
    /// # Errors
    ///
    /// Fails if `config` is for the other firmware family or the device does not answer.
    pub async fn set_config(&mut self, config: &Config) -> Result<Config, Error> {
        let family = self.firmware();
        if config.firmware() != family {
            return Err(Error::InvalidValue(format!(
                "{} config for a {family} device",
                config.firmware()
            )));
        }
        let mut payload = vec![CMD_CONFIG];
        payload.extend_from_slice(config.as_bytes());
        let reply = self.command(&payload, config_reply_len(family)).await?;
        Ok(config_from_reply(family, &reply))
    }

    /// # Errors
    ///
    /// Fails if the device does not answer.
    pub async fn coefficients(&mut self) -> Result<Coefficients, Error> {
        let reply = self
            .command(&[CMD_COEFFICIENTS], 1 + COEFFICIENTS_LEN)
            .await?;
        Ok(self.coefficients_from_reply(&reply))
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
        Ok(self.coefficients_from_reply(&reply))
    }

    /// Restores the factory coefficients for the sensor type.
    ///
    /// THB2 only resets them in RAM (`CMD_ID_CFS_DEF` does not write flash), so the
    /// defaults are written back to make the reset survive a reboot.
    ///
    /// # Errors
    ///
    /// Fails if the device does not answer.
    pub async fn reset_coefficients(&mut self) -> Result<Coefficients, Error> {
        let reply = self
            .command(&[CMD_COEFFICIENTS_DEFAULT], 1 + COEFFICIENTS_LEN)
            .await?;
        let defaults = self.coefficients_from_reply(&reply);
        match self.firmware() {
            Firmware::Atc => Ok(defaults),
            Firmware::Thb => self.set_coefficients(&defaults).await,
        }
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
    /// Fails if the device has no screen, the message cannot be displayed, or the
    /// device does not answer or stores something else.
    pub async fn show(&mut self, message: &ScreenMessage) -> Result<(), Error> {
        if !self.info.has_screen() {
            return Err(Error::Unsupported("a screen"));
        }
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
    /// Fails with [`Error::Unsupported`] on THB2 (built without time adjustment), or
    /// if the device does not answer.
    pub async fn second_ticks(&mut self) -> Result<u32, Error> {
        self.require_clock_rate()?;
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
        self.require_clock_rate()?;
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
        if !self.info.has_log() {
            return Err(Error::Unsupported("a measurement log"));
        }
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
        close(&self.peripheral, self.id).await
    }

    fn require_clock_rate(&self) -> Result<(), Error> {
        if self.info.has_clock_rate() {
            Ok(())
        } else {
            Err(Error::Unsupported("clock rate trimming"))
        }
    }

    fn coefficients_from_reply(&mut self, reply: &[u8]) -> Coefficients {
        if self.firmware() == Firmware::Thb {
            self.sensor = SensorInfo::from_thb_reply(reply).or(self.sensor);
        }
        coefficients_from_reply(reply)
    }

    async fn write(&mut self, payload: &[u8]) -> Result<(), Error> {
        // `&mut` keeps the futures `Send` without requiring `Thermometer: Sync`.
        log::trace!("{}: write {payload:02x?}", self.id);
        let (peripheral, command, write_type) = (&self.peripheral, &self.command, self.write_type);
        step("writing a command", WRITE_TIMEOUT, async {
            Ok(peripheral.write(command, payload, write_type).await?)
        })
        .await
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
                log::trace!("{}: reply {reply:02x?} (attempt {attempt})", self.id);
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
        log::warn!(
            "{}: no answer to {command:#04x} after {COMMAND_ATTEMPTS} attempts",
            self.id
        );
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

/// Connects to `id`, cancelling whatever a timed-out attempt left pending.
async fn open(adapter: &Adapter, id: DeviceId) -> Result<Peripheral, Error> {
    let mut attempt = 0;
    loop {
        let peripheral = wait_until_known(adapter, id).await?;
        if step("checking the connection", DISCONNECT_TIMEOUT, async {
            Ok(peripheral.is_connected().await?)
        })
        .await?
        {
            return Ok(peripheral);
        }
        attempt += 1;
        let started = Instant::now();
        let result = step("connecting", CONNECT_TIMEOUT, async {
            Ok(peripheral.connect().await?)
        })
        .await;
        match result {
            Ok(()) => return Ok(peripheral),
            Err(err) => {
                log::warn!(
                    "{id}: connect attempt {attempt}/{CONNECT_ATTEMPTS} failed after {:?}: {err}",
                    started.elapsed()
                );
                // A timed-out attempt is still pending inside the stack and would make
                // the next one fail with "in progress"; cancelling it clears that.
                let _ = close(&peripheral, id).await;
                if attempt >= CONNECT_ATTEMPTS {
                    return Err(err);
                }
            }
        }
        tokio::time::sleep(CONNECT_RETRY_PAUSE * attempt).await;
    }
}

/// THB2 declares only write-without-response; ATC both, and either works there.
/// Without response is quicker and is what the pvvx web tools use.
fn write_type_for(properties: CharPropFlags) -> WriteType {
    if properties.contains(CharPropFlags::WRITE_WITHOUT_RESPONSE) {
        WriteType::WithoutResponse
    } else {
        WriteType::WithResponse
    }
}

/// Waits for the adapter to list the device. BlueZ drops devices it has not heard
/// from in a while (e.g. after discovery restarts) and lists them again on their next
/// advert; CoreBluetooth only lists what this process has scanned.
async fn wait_until_known(adapter: &Adapter, id: DeviceId) -> Result<Peripheral, Error> {
    let deadline = Instant::now() + DEVICE_APPEAR_TIMEOUT;
    loop {
        if let Some(peripheral) = find_peripheral(adapter, id).await? {
            return Ok(peripheral);
        }
        if Instant::now() >= deadline {
            return Err(Error::NotFound(id.to_string()));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
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

/// Where `cfg_t` starts in a config reply: ATC sends `[0x55, firmware version,
/// cfg_t...]`, THB2 `[0x55, cfg_t...]`.
fn config_offset(family: Firmware) -> usize {
    match family {
        Firmware::Atc => 2,
        Firmware::Thb => 1,
    }
}

fn config_reply_len(family: Firmware) -> usize {
    config_offset(family) + Config::len_for(family)
}

/// The caller checked the reply is [`config_reply_len`] long.
fn config_from_reply(family: Firmware, reply: &[u8]) -> Config {
    Config::from_slice(family, &reply[config_offset(family)..])
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
        let reply = [
            0x55, 0x59, 0x87, 0x10, 0x0a, 0x06, 0x28, 0x04, 0xa9, 0x31, 0x31, 0x04, 0xb4, 0x00,
        ];
        assert!(reply.len() >= config_reply_len(Firmware::Atc));
        let config = config_from_reply(Firmware::Atc, &reply);
        assert_eq!(config.measure_interval(), 4);

        // THB1 `def_cfg` as `cmd_parser.c` sends it: no version byte.
        let reply = [0x55, 0x05, 0, 0, 0, 0x1f, 80, 29, 16, 2, 60, 180, 0];
        assert_eq!(reply.len(), config_reply_len(Firmware::Thb));
        let config = config_from_reply(Firmware::Thb, &reply);
        assert_eq!(config.firmware(), Firmware::Thb);
        assert_eq!(config.measure_interval(), 2);
        assert_eq!(config.advertising_interval(), Duration::from_secs(5));
        assert!(config.tx_power().is_factory());

        let clock = clock_from_reply(&[0x23, 1, 0, 0, 0, 2, 0, 0, 0], UNIX_EPOCH);
        assert_eq!(clock.device_now, 1);
        assert_eq!(clock.last_set, Some(2));
        let clock = clock_from_reply(&[0x23, 1, 0, 0, 0], UNIX_EPOCH);
        assert_eq!(clock.last_set, None);

        assert_eq!(decode_name(b"Kitchen\0\0"), "Kitchen");
    }
}
