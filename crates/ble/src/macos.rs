//! CoreBluetooth backend (macOS), through `btleplug`.
//!
//! Three CoreBluetooth habits shape this file:
//!
//! - A peripheral can only be connected through the central that discovered it, and
//!   once it disconnects it is listed again only when a scan sees it. So scan and
//!   connect share one adapter, and scanning goes on while jobs run.
//! - A connect request never times out by itself: CoreBluetooth keeps it pending and
//!   sends it on the device's next connectable advert. Cancelling and asking again
//!   only restarts that wait.
//! - MACs are hidden behind UUIDs that differ per host. The scan maps each UUID back
//!   to the device's MAC by connecting once and reading it ([`Thermometer::mac`]), and
//!   from then on reports the MAC, as BlueZ does (AGENTS.md, "Porting to another OS").

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use atc::{COMMAND_UUID, DeviceId, Notifications, Transport};
use btleplug::api::{
    Central as _, CentralEvent, CentralState, CharPropFlags, Characteristic, Manager as _,
    Peripheral as _, ScanFilter, WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral, PeripheralId};
use futures::StreamExt;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::time::{Instant, timeout};
use uuid::Uuid;

use crate::{Advert, BTHOME_UUID, ScanControl, ScanEvent, Thermometer};

/// Wait before starting over after an error, e.g. while Bluetooth is switched off.
const RETRY_PAUSE: Duration = Duration::from_secs(5);
/// A device counts as in range if it advertised this recently; otherwise `connect`
/// first waits up to [`DEVICE_APPEAR_TIMEOUT`] for an advert.
const ADVERT_WINDOW: Duration = Duration::from_secs(60);
/// Several advert intervals (ATC 2.5 s by default), since weak devices are heard only
/// now and then.
const DEVICE_APPEAR_TIMEOUT: Duration = Duration::from_secs(30);
/// How long one connect request stays pending. At weak signal many connectable adverts
/// are lost, so it gets several; on macOS the first request after a while often runs
/// out and the second one connects.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_ATTEMPTS: u32 = 2;
const DISCOVER_TIMEOUT: Duration = Duration::from_secs(15);
const SUBSCRIBE_TIMEOUT: Duration = Duration::from_secs(8);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// `disconnect` waits for the stack's confirmation, which never comes for a peripheral
/// that is not connected.
const DISCONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Longest an identification may hold the connection lock: two connect attempts and
/// one command.
const IDENTIFY_TIMEOUT: Duration = Duration::from_secs(120);
/// Waits before identifying a device again after a failure, growing with each one.
const IDENTIFY_RETRY: [Duration; 3] = [
    Duration::from_secs(60),
    Duration::from_secs(5 * 60),
    Duration::from_secs(30 * 60),
];
/// The UUID → MAC map, under `$HOME`. Losing it only costs one connection per device.
const IDS_FILE: &str = "Library/Caches/thermo/macos-ids.csv";
const IDS_HEADER: &str = "uuid,mac";

/// The CoreBluetooth backend: one shared central, when each device was last heard, and
/// the UUID → MAC map.
#[derive(Clone, Default)]
pub struct Bluetooth {
    shared: Arc<Shared>,
}

#[derive(Default)]
struct Shared {
    /// Opened on first use and shared by scan and connect (see the module docs).
    adapter: tokio::sync::Mutex<Option<Adapter>>,
    /// When each peripheral last sent a BTHome advert.
    heard: Mutex<HashMap<Uuid, Instant>>,
    /// Read from the cache file on first use.
    ids: OnceLock<Mutex<Ids>>,
    /// Held while a job connects and through a whole identification, so the two never
    /// connect at once.
    connecting: tokio::sync::Mutex<()>,
}

impl Bluetooth {
    /// Scans until the app is gone. A failure is reported as [`ScanEvent::Failed`],
    /// and scanning starts over with a new central after [`RETRY_PAUSE`].
    ///
    /// Scanning goes on during [`ScanControl::Pause`], since CoreBluetooth connects on
    /// adverts; the pause only keeps identifications from starting.
    pub async fn scan(
        self,
        events: UnboundedSender<ScanEvent>,
        mut controls: UnboundedReceiver<ScanControl>,
    ) {
        let mut identifier = Identifier::new();
        while let Err(Failure(reason)) =
            scan(&self.shared, &events, &mut controls, &mut identifier).await
        {
            if events.send(ScanEvent::Failed(reason)).is_err() {
                return;
            }
            *self.shared.adapter.lock().await = None;
            tokio::time::sleep(RETRY_PAUSE).await;
        }
    }

    /// Connects to a device heard while scanning and checks that it runs pvvx
    /// firmware. A MAC is looked up in the UUID → MAC map.
    ///
    /// # Errors
    ///
    /// Fails if the device was never identified or is not in range, no connection is
    /// made in [`CONNECT_ATTEMPTS`] tries, or the device is not a supported pvvx device.
    pub async fn connect(&self, id: DeviceId) -> Result<Thermometer, atc::Error> {
        let uuid = self.shared.uuid(id)?;
        let _connecting = self.shared.connecting.lock().await;
        open(&self.shared, uuid, id).await
    }

    /// Drops a connection to `id`, or a connect still pending, that a cancelled job
    /// may have left behind.
    pub async fn release(&self, id: DeviceId) {
        if let Ok(uuid) = self.shared.uuid(id)
            && let Ok(adapter) = self.shared.adapter().await
            && let Ok(peripheral) = adapter.peripheral(&PeripheralId::from(uuid)).await
        {
            // Also cancels a pending request, which would otherwise connect on the
            // next advert and leave the device silent.
            let _ = close(&self.shared, &peripheral).await;
        }
    }
}

impl Shared {
    async fn adapter(&self) -> Result<Adapter, atc::Error> {
        let mut slot = self.adapter.lock().await;
        if let Some(adapter) = slot.as_ref() {
            return Ok(adapter.clone());
        }
        let manager = Manager::new().await.map_err(bluetooth)?;
        let adapter = manager
            .adapters()
            .await
            .map_err(bluetooth)?
            .into_iter()
            .next()
            .ok_or_else(|| atc::Error::Bluetooth("no Bluetooth adapter".into()))?;
        *slot = Some(adapter.clone());
        Ok(adapter)
    }

    fn ids(&self) -> &Mutex<Ids> {
        self.ids
            .get_or_init(|| Mutex::new(Ids::load(ids_path().as_deref())))
    }

    fn mac(&self, uuid: Uuid) -> Option<[u8; 6]> {
        self.ids().lock().ok()?.mac(uuid)
    }

    /// The peripheral behind `id`.
    fn uuid(&self, id: DeviceId) -> Result<Uuid, atc::Error> {
        match id {
            DeviceId::Uuid(uuid) => Ok(uuid),
            DeviceId::Mac(mac) => self
                .ids()
                .lock()
                .ok()
                .and_then(|ids| ids.uuid(mac))
                .ok_or(atc::Error::NotInRange(id)),
        }
    }

    fn heard(&self, uuid: Uuid) {
        if let Ok(mut heard) = self.heard.lock() {
            heard.insert(uuid, Instant::now());
        }
    }

    /// CoreBluetooth forgets a peripheral when it disconnects, and a connect to the old
    /// object fails ("peripheral no longer available") until a scan sees it again.
    fn forget(&self, uuid: Uuid) {
        if let Ok(mut heard) = self.heard.lock() {
            heard.remove(&uuid);
        }
    }

    fn heard_within(&self, uuid: Uuid, window: Duration) -> bool {
        self.heard
            .lock()
            .ok()
            .and_then(|heard| heard.get(&uuid).copied())
            .is_some_and(|at| at.elapsed() <= window)
    }
}

/// A connection's command characteristic.
pub struct Link {
    shared: Arc<Shared>,
    peripheral: Peripheral,
    command: Characteristic,
    write_type: WriteType,
}

impl Transport for Link {
    async fn write(&mut self, payload: &[u8]) -> Result<(), atc::Error> {
        let (peripheral, command, write_type) = (&self.peripheral, &self.command, self.write_type);
        step("writing a command", WRITE_TIMEOUT, async {
            peripheral
                .write(command, payload, write_type)
                .await
                .map_err(bluetooth)
        })
        .await
    }

    async fn disconnect(self) -> Result<(), atc::Error> {
        close(&self.shared, &self.peripheral).await
    }
}

fn bluetooth(err: btleplug::Error) -> atc::Error {
    atc::Error::Bluetooth(Box::new(err))
}

/// Runs one BLE stack call with a deadline, so a lost callback cannot hang a job.
async fn step<T>(
    name: &str,
    limit: Duration,
    call: impl Future<Output = Result<T, atc::Error>>,
) -> Result<T, atc::Error> {
    timeout(limit, call).await.unwrap_or_else(|_| {
        Err(atc::Error::Bluetooth(
            format!("{name} timed out after {} s", limit.as_secs()).into(),
        ))
    })
}

/// Cancels a connection or a pending connect. The next connect then waits for an
/// advert (see [`Shared::forget`]).
async fn close(shared: &Shared, peripheral: &Peripheral) -> Result<(), atc::Error> {
    let result = step("disconnecting", DISCONNECT_TIMEOUT, async {
        peripheral.disconnect().await.map_err(bluetooth)
    })
    .await;
    if let Some(uuid) = uuid_of(&peripheral.id()) {
        shared.forget(uuid);
    }
    result
}

/// Connects to `uuid` and opens its command characteristic. Errors name the device as
/// `id`, the way the caller asked for it. The caller holds [`Shared::connecting`].
async fn open(shared: &Arc<Shared>, uuid: Uuid, id: DeviceId) -> Result<Thermometer, atc::Error> {
    let adapter = shared.adapter().await?;
    let mut attempt = 0;
    let peripheral = loop {
        attempt += 1;
        let peripheral = wait_until_heard(shared, &adapter, uuid, id).await?;
        let result = timeout(CONNECT_TIMEOUT, peripheral.connect()).await;
        let error = match result {
            Ok(Ok(())) => break peripheral,
            Ok(Err(err)) => bluetooth(err),
            Err(_) => atc::Error::ConnectTimeout,
        };
        // The request is still pending inside CoreBluetooth; cancel it, or the device
        // connects later with nobody using the connection.
        let _ = close(shared, &peripheral).await;
        if attempt >= CONNECT_ATTEMPTS {
            return Err(error);
        }
    };
    let result = set_up(shared, &peripheral).await;
    if result.is_err() {
        let _ = close(shared, &peripheral).await;
    }
    result
}

/// Waits until the device advertised within [`ADVERT_WINDOW`], at most
/// [`DEVICE_APPEAR_TIMEOUT`], and returns its peripheral.
async fn wait_until_heard(
    shared: &Shared,
    adapter: &Adapter,
    uuid: Uuid,
    id: DeviceId,
) -> Result<Peripheral, atc::Error> {
    let deadline = Instant::now() + DEVICE_APPEAR_TIMEOUT;
    loop {
        if shared.heard_within(uuid, ADVERT_WINDOW)
            && let Ok(peripheral) = adapter.peripheral(&PeripheralId::from(uuid)).await
        {
            return Ok(peripheral);
        }
        if Instant::now() >= deadline {
            return Err(atc::Error::NotInRange(id));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Finds the command characteristic, subscribes to it (before any write, so no reply
/// is missed) and reads the device id.
async fn set_up(shared: &Arc<Shared>, peripheral: &Peripheral) -> Result<Thermometer, atc::Error> {
    step("discovering services", DISCOVER_TIMEOUT, async {
        peripheral.discover_services().await.map_err(bluetooth)
    })
    .await?;
    let command = peripheral
        .characteristics()
        .into_iter()
        .find(|c| c.uuid == COMMAND_UUID)
        .ok_or(atc::Error::NotPvvx)?;
    let notifications: Notifications = Box::pin(
        peripheral
            .notifications()
            .await
            .map_err(bluetooth)?
            .filter_map(|n| futures::future::ready((n.uuid == COMMAND_UUID).then_some(n.value))),
    );
    step("subscribing to notifications", SUBSCRIBE_TIMEOUT, async {
        peripheral.subscribe(&command).await.map_err(bluetooth)
    })
    .await?;
    let link = Link {
        shared: Arc::clone(shared),
        peripheral: peripheral.clone(),
        write_type: write_type_for(command.properties),
        command,
    };
    Thermometer::open(link, notifications).await
}

/// Writing without response is quicker and is what the pvvx web tools use; the
/// firmware accepts both.
fn write_type_for(properties: CharPropFlags) -> WriteType {
    if properties.contains(CharPropFlags::WRITE_WITHOUT_RESPONSE) {
        WriteType::WithoutResponse
    } else {
        WriteType::WithResponse
    }
}

/// Why scanning stopped, as shown to the user.
struct Failure(String);

impl From<atc::Error> for Failure {
    fn from(err: atc::Error) -> Self {
        Self(err.to_string())
    }
}

trait Context<T> {
    /// Prefixes an error with what was being done.
    fn context(self, doing: &str) -> Result<T, Failure>;
}

impl<T> Context<T> for btleplug::Result<T> {
    fn context(self, doing: &str) -> Result<T, Failure> {
        self.map_err(|err| Failure(format!("{doing}: {err}")))
    }
}

/// Scans until the app is gone (`Ok`) or CoreBluetooth fails.
async fn scan(
    shared: &Arc<Shared>,
    events: &UnboundedSender<ScanEvent>,
    controls: &mut UnboundedReceiver<ScanControl>,
    identifier: &mut Identifier,
) -> Result<(), Failure> {
    let adapter = shared.adapter().await?;
    let state = adapter
        .adapter_state()
        .await
        .context("reading the adapter state")?;
    if state != CentralState::PoweredOn {
        return Err(Failure(format!(
            "Bluetooth is not available ({state:?}); allow this terminal under System \
             Settings › Privacy & Security › Bluetooth"
        )));
    }
    let mut central_events = adapter.events().await.context("watching the adapter")?;
    // No service filter: CoreBluetooth matches it against advertised service UUIDs,
    // and BTHome devices may send theirs only as service data.
    adapter
        .start_scan(ScanFilter::default())
        .await
        .context("starting the scan")?;
    let name = adapter
        .adapter_info()
        .await
        .unwrap_or_else(|_| "CoreBluetooth".to_owned());
    if events.send(ScanEvent::Started { adapter: name }).is_err() {
        return Ok(());
    }

    let mut known: HashMap<Uuid, Known> = HashMap::new();
    loop {
        tokio::select! {
            control = controls.recv() => match control {
                None => return Ok(()),
                Some(ScanControl::Pause) => identifier.paused = true,
                Some(ScanControl::Resume) => identifier.paused = false,
            },
            Some((uuid, result)) = identifier.results.recv() => {
                identifier.finished(shared, uuid, result, Instant::now());
            }
            event = central_events.next() => match event {
                None => return Err(Failure("CoreBluetooth stopped sending events".into())),
                Some(CentralEvent::StateUpdate(state)) if state != CentralState::PoweredOn => {
                    return Err(Failure(format!("Bluetooth switched off ({state:?})")));
                }
                Some(CentralEvent::DeviceDisconnected(id)) => {
                    // A connection may have changed the name; read it again.
                    if let Some(uuid) = uuid_of(&id) {
                        known.remove(&uuid);
                        shared.forget(uuid);
                    }
                }
                Some(CentralEvent::ServiceDataAdvertisement { id, mut service_data }) => {
                    let Some(payload) = service_data.remove(&BTHOME_UUID) else { continue };
                    let Some(uuid) = uuid_of(&id) else { continue };
                    shared.heard(uuid);
                    let entry = known.entry(uuid).or_default();
                    if let Ok(peripheral) = adapter.peripheral(&id).await
                        && let Ok(Some(properties)) = peripheral.properties().await
                    {
                        entry.rssi = properties.rssi.or(entry.rssi);
                        if let Some(name) = properties.local_name {
                            entry.name = Some(name);
                        }
                    }
                    let address = if let Some(mac) = shared.mac(uuid) {
                        DeviceId::Mac(mac)
                    } else {
                        identifier.heard(shared, uuid, Instant::now());
                        if !identifier.sends_adverts(uuid) {
                            continue;
                        }
                        DeviceId::Uuid(uuid)
                    };
                    let advert = Advert {
                        address,
                        name: entry.name.clone().unwrap_or_else(|| address.short()),
                        rssi: entry.rssi,
                        payload,
                        received: SystemTime::now(),
                    };
                    if events.send(ScanEvent::Advert(advert)).is_err() {
                        return Ok(());
                    }
                }
                Some(_) => {}
            },
        }
    }
}

/// btleplug names CoreBluetooth peripherals by their UUID.
fn uuid_of(id: &PeripheralId) -> Option<Uuid> {
    Uuid::parse_str(&id.to_string()).ok()
}

/// What the scan remembers about one peripheral between its adverts.
#[derive(Debug, Default)]
struct Known {
    name: Option<String>,
    rssi: Option<i16>,
}

/// Maps UUIDs to MACs, one device at a time, from inside the scan.
struct Identifier {
    /// Unmapped peripherals heard since the scan started.
    states: HashMap<Uuid, Identification>,
    /// The peripheral being identified now.
    running: Option<Uuid>,
    /// Set while jobs run; no identification starts then.
    paused: bool,
    results_tx: UnboundedSender<(Uuid, Result<[u8; 6], atc::Error>)>,
    results: UnboundedReceiver<(Uuid, Result<[u8; 6], atc::Error>)>,
}

impl Identifier {
    fn new() -> Self {
        let (results_tx, results) = mpsc::unbounded_channel();
        Self {
            states: HashMap::new(),
            running: None,
            paused: false,
            results_tx,
            results,
        }
    }

    /// An unmapped peripheral advertised: identify it if it is due and nothing else
    /// is being identified.
    fn heard(&mut self, shared: &Arc<Shared>, uuid: Uuid, now: Instant) {
        let state = self.states.entry(uuid).or_insert(Identification::Untried);
        if self.paused || self.running.is_some() || !state.due(now) {
            return;
        }
        *state = state.started();
        self.running = Some(uuid);
        let (shared, results) = (Arc::clone(shared), self.results_tx.clone());
        tokio::spawn(async move {
            let result = timeout(IDENTIFY_TIMEOUT, identify(&shared, uuid))
                .await
                .unwrap_or(Err(atc::Error::ConnectTimeout));
            let _ = results.send((uuid, result));
        });
    }

    fn sends_adverts(&self, uuid: Uuid) -> bool {
        self.states
            .get(&uuid)
            .is_some_and(|state| state.sends_adverts())
    }

    fn finished(
        &mut self,
        shared: &Shared,
        uuid: Uuid,
        result: Result<[u8; 6], atc::Error>,
        now: Instant,
    ) {
        self.running = None;
        match result {
            Ok(mac) => {
                self.states.remove(&uuid);
                if let Ok(mut ids) = shared.ids().lock()
                    && ids.insert(uuid, mac)
                {
                    // Losing the file only costs another identification.
                    let _ = ids.save();
                }
            }
            Err(err) => {
                if let Some(state) = self.states.get_mut(&uuid) {
                    *state = state.failed(can_retry(&err), now);
                }
            }
        }
    }
}

/// Connects to `uuid` and reads its MAC, holding the connection lock throughout.
async fn identify(shared: &Arc<Shared>, uuid: Uuid) -> Result<[u8; 6], atc::Error> {
    let _connecting = shared.connecting.lock().await;
    let mut thermometer = open(shared, uuid, DeviceId::Uuid(uuid)).await?;
    let mac = thermometer.mac().await;
    let _ = thermometer.disconnect().await;
    match mac? {
        DeviceId::Mac(mac) => Ok(mac),
        DeviceId::Uuid(_) => Err(atc::Error::InvalidValue("no MAC in the reply".into())),
    }
}

/// Whether identifying again might work: not for devices this crate can't talk to.
fn can_retry(err: &atc::Error) -> bool {
    !matches!(
        err,
        atc::Error::NotPvvx
            | atc::Error::UnsupportedFirmware { .. }
            | atc::Error::ShortReply { .. }
            | atc::Error::InvalidValue(_)
    )
}

/// Where identifying one unmapped peripheral stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Identification {
    /// Not tried yet; its adverts are held back.
    Untried,
    /// The first attempt runs; its adverts are held back.
    First,
    /// A later attempt runs, after `failures`; adverts go out under the UUID.
    Again { failures: usize },
    /// Failed `failures` times; adverts go out under the UUID until it works.
    Failed { failures: usize, retry_at: Instant },
    /// The device can't be asked for its MAC (e.g. other firmware); adverts go out
    /// under the UUID.
    Never,
}

impl Identification {
    fn due(self, now: Instant) -> bool {
        match self {
            Self::Untried => true,
            Self::Failed { retry_at, .. } => now >= retry_at,
            Self::First | Self::Again { .. } | Self::Never => false,
        }
    }

    fn started(self) -> Self {
        match self {
            Self::Failed { failures, .. } => Self::Again { failures },
            _ => Self::First,
        }
    }

    fn failed(self, can_retry: bool, now: Instant) -> Self {
        if !can_retry {
            return Self::Never;
        }
        let failures = match self {
            Self::Again { failures } | Self::Failed { failures, .. } => failures + 1,
            _ => 1,
        };
        let wait = IDENTIFY_RETRY[(failures - 1).min(IDENTIFY_RETRY.len() - 1)];
        Self::Failed {
            failures,
            retry_at: now + wait,
        }
    }

    fn sends_adverts(self) -> bool {
        !matches!(self, Self::Untried | Self::First)
    }
}

/// UUID → MAC pairs, with the file they are kept in.
#[derive(Debug, Default)]
struct Ids {
    macs: HashMap<Uuid, [u8; 6]>,
    path: Option<PathBuf>,
}

impl Ids {
    /// Reads `path`; a missing or unreadable file gives an empty map.
    fn load(path: Option<&Path>) -> Self {
        let macs = path
            .and_then(|path| fs::read_to_string(path).ok())
            .map(|text| parse_ids(&text))
            .unwrap_or_default();
        Self {
            macs,
            path: path.map(Path::to_path_buf),
        }
    }

    fn mac(&self, uuid: Uuid) -> Option<[u8; 6]> {
        self.macs.get(&uuid).copied()
    }

    fn uuid(&self, mac: [u8; 6]) -> Option<Uuid> {
        self.macs
            .iter()
            .find_map(|(uuid, m)| (*m == mac).then_some(*uuid))
    }

    /// Records that `uuid` is `mac`, dropping any other UUID for that MAC (macOS hands
    /// out a new one after a Bluetooth reset). Returns whether anything changed.
    fn insert(&mut self, uuid: Uuid, mac: [u8; 6]) -> bool {
        if self.mac(uuid) == Some(mac) && self.macs.values().filter(|m| **m == mac).count() == 1 {
            return false;
        }
        self.macs.retain(|_, m| *m != mac);
        self.macs.insert(uuid, mac);
        true
    }

    /// Writes the file whole, through a temporary file so a crash can't truncate it.
    fn save(&self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let temporary = path.with_extension("csv.new");
        fs::write(&temporary, format_ids(&self.macs))?;
        fs::rename(temporary, path)
    }
}

/// `$HOME/`[`IDS_FILE`].
fn ids_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(IDS_FILE))
}

/// Reads `uuid,mac` lines after the header, skipping any it can't parse.
fn parse_ids(text: &str) -> HashMap<Uuid, [u8; 6]> {
    text.lines()
        .skip_while(|line| line.trim() == IDS_HEADER)
        .filter_map(|line| {
            let (uuid, mac) = line.split_once(',')?;
            let uuid = Uuid::parse_str(uuid.trim()).ok()?;
            match mac.parse() {
                Ok(DeviceId::Mac(mac)) => Some((uuid, mac)),
                _ => None,
            }
        })
        .collect()
}

/// The map as `uuid,mac` lines under a header, sorted so the file only changes when
/// the map does.
fn format_ids(macs: &HashMap<Uuid, [u8; 6]>) -> String {
    let mut lines: Vec<String> = macs
        .iter()
        .map(|(uuid, mac)| format!("{},{}", DeviceId::Uuid(*uuid), DeviceId::Mac(*mac)))
        .collect();
    lines.sort();
    let mut text = format!("{IDS_HEADER}\n");
    for line in lines {
        text.push_str(&line);
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Uuid = Uuid::from_u128(0xa42d_df15_06d6_3a4c_f547_da9b_e9d8_ebb7);
    const B: Uuid = Uuid::from_u128(0xffbc_2e88_a02d_4a7e_2cbf_0754_f80b_1bf6);
    const MAC: [u8; 6] = [0xa4, 0xc1, 0x38, 0x27, 0xc6, 0x22];

    #[test]
    fn ids_file_round_trips() {
        let macs = HashMap::from([(A, MAC), (B, [1, 2, 3, 4, 5, 6])]);
        let text = format_ids(&macs);
        assert_eq!(
            text,
            "uuid,mac\n\
             A42DDF15-06D6-3A4C-F547-DA9BE9D8EBB7,A4:C1:38:27:C6:22\n\
             FFBC2E88-A02D-4A7E-2CBF-0754F80B1BF6,01:02:03:04:05:06\n"
        );
        assert_eq!(parse_ids(&text), macs);
    }

    #[test]
    fn ids_file_skips_bad_lines() {
        let text = "uuid,mac\n\
                    garbage\n\
                    A42DDF15-06D6-3A4C-F547-DA9BE9D8EBB7,A4:C1:38:27:C6:22\n\
                    FFBC2E88-A02D-4A7E-2CBF-0754F80B1BF6,FFBC2E88-A02D-4A7E-2CBF-0754F80B1BF6\n";
        assert_eq!(parse_ids(text), HashMap::from([(A, MAC)]));
        assert!(parse_ids("").is_empty());
    }

    #[test]
    fn a_mac_keeps_only_its_newest_uuid() {
        let mut ids = Ids::default();
        assert!(ids.insert(A, MAC));
        assert!(!ids.insert(A, MAC));
        assert_eq!(ids.uuid(MAC), Some(A));
        // macOS gave the device a new UUID.
        assert!(ids.insert(B, MAC));
        assert_eq!(ids.mac(A), None);
        assert_eq!(ids.mac(B), Some(MAC));
        assert_eq!(ids.uuid(MAC), Some(B));
    }

    #[test]
    fn ids_save_and_load() {
        let dir = std::env::temp_dir().join(format!("thermo-ids-test-{}", std::process::id()));
        let path = dir.join("nested").join("macos-ids.csv");
        let mut ids = Ids::load(Some(&path));
        assert!(ids.macs.is_empty());
        ids.insert(A, MAC);
        ids.save().expect("temp dir is writable");
        assert_eq!(Ids::load(Some(&path)).mac(A), Some(MAC));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn adverts_wait_for_the_first_identification() {
        let now = Instant::now();
        let state = Identification::Untried;
        assert!(!state.sends_adverts());
        assert!(state.due(now));
        let state = state.started();
        assert_eq!(state, Identification::First);
        assert!(!state.sends_adverts());
        assert!(!state.due(now));
        let state = state.failed(true, now);
        assert!(state.sends_adverts());
        assert!(!state.due(now));
        assert!(state.due(now + IDENTIFY_RETRY[0]));
    }

    #[test]
    fn retries_back_off_and_stop_for_other_firmware() {
        let now = Instant::now();
        let mut state = Identification::Untried;
        for wait in IDENTIFY_RETRY.iter().chain(&[IDENTIFY_RETRY[2]]) {
            state = state.started().failed(true, now);
            assert!(!state.due(now + *wait - Duration::from_secs(1)));
            assert!(state.due(now + *wait));
            assert!(state.started().sends_adverts());
        }
        let state = state.started().failed(can_retry(&atc::Error::NotPvvx), now);
        assert_eq!(state, Identification::Never);
        assert!(state.sends_adverts());
        assert!(!state.due(now + Duration::from_secs(86_400)));
        assert!(can_retry(&atc::Error::ConnectTimeout));
    }
}
