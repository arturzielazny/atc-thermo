//! BlueZ backend (Linux), through `bluer`.
//!
//! Nothing is kept between calls: the scan and every connection open their own BlueZ
//! session, so both work again once `bluetoothd` restarts or the adapter comes back.

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use atc::{DeviceId, Firmware, Notifications, Transport};
use bluer::gatt::remote::Characteristic;
use bluer::{
    Adapter, AdapterEvent, Address, Device, DeviceEvent, DeviceProperty, DiscoveryFilter,
    DiscoveryTransport,
};
use futures::stream::{self, SelectAll};
use futures::{Stream, StreamExt};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::time::{Instant, timeout};

use crate::{Advert, BTHOME_UUID, ScanControl, ScanEvent, Thermometer};

const DISCOVERY_RESTART_PAUSE: Duration = Duration::from_millis(500);
/// Wait before starting over after an error, e.g. while the adapter is switched off
/// or `bluetoothd` restarts.
const RETRY_PAUSE: Duration = Duration::from_secs(5);
const CONNECT_ATTEMPTS: u32 = 4;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(25);
const CONNECT_RETRY_PAUSE: Duration = Duration::from_secs(1);
/// How long to wait for a device BlueZ has forgotten to advertise again.
const DEVICE_APPEAR_TIMEOUT: Duration = Duration::from_secs(30);
/// Wait after a failed connect before checking for a late, unwanted connection.
const LATE_CONNECT_GRACE: Duration = Duration::from_secs(3);

/// The BlueZ backend. It holds no state (see the module documentation); the private
/// field keeps it created as `Bluetooth::default()`, like backends that do.
#[derive(Debug, Clone, Default)]
pub struct Bluetooth {
    _private: (),
}

impl Bluetooth {
    /// Scans until the app is gone. A failure is reported as [`ScanEvent::Failed`],
    /// and scanning starts over with a new BlueZ session after [`RETRY_PAUSE`].
    ///
    /// Discovery stops for [`ScanControl::Pause`]: a connection stops it anyway, and
    /// BlueZ connects more reliably without it.
    pub async fn scan(
        self,
        events: UnboundedSender<ScanEvent>,
        mut controls: UnboundedReceiver<ScanControl>,
    ) {
        let mut paused = false;
        while let Err(Failure(reason)) = scan(&events, &mut controls, &mut paused).await {
            if events.send(ScanEvent::Failed(reason)).is_err() {
                return;
            }
            tokio::time::sleep(RETRY_PAUSE).await;
        }
    }

    /// Connects to a device BlueZ knows (i.e. one heard while scanning) and checks
    /// that it runs pvvx firmware.
    ///
    /// # Errors
    ///
    /// Fails if the device is not in range, no connection is made in
    /// [`CONNECT_ATTEMPTS`] tries, or the device is not a supported pvvx device.
    pub async fn connect(&self, id: DeviceId) -> Result<Thermometer, atc::Error> {
        let address = address(id)?;
        let adapter = default_adapter().await?;
        let result = connect(&adapter, address).await;
        if result.is_err() {
            // BlueZ can finish a connection after we gave up on it; a connected
            // thermometer stops advertising, so make sure none is left behind.
            tokio::time::sleep(LATE_CONNECT_GRACE).await;
            if let Ok(device) = adapter.device(address)
                && device.is_connected().await.unwrap_or(false)
            {
                let _ = device.disconnect().await;
            }
        }
        result
    }

    /// Drops a connection to `id`, or a connect still pending, that a cancelled job
    /// may have left behind.
    pub async fn release(&self, id: DeviceId) {
        if let Ok(address) = address(id)
            && let Ok(adapter) = default_adapter().await
            && let Ok(device) = adapter.device(address)
        {
            let _ = device.disconnect().await;
        }
    }
}

/// A BlueZ connection's command characteristic.
pub struct Link {
    device: Device,
    command: Characteristic,
}

impl Transport for Link {
    async fn write(&mut self, payload: &[u8]) -> Result<(), atc::Error> {
        self.command.write(payload).await.map_err(bluetooth)
    }

    async fn disconnect(self) -> Result<(), atc::Error> {
        self.device.disconnect().await.map_err(bluetooth)
    }
}

/// BlueZ names devices by MAC address; an id from another stack (a CoreBluetooth
/// UUID in history copied from a Mac) is never in range here.
fn address(id: DeviceId) -> Result<Address, atc::Error> {
    match id {
        DeviceId::Mac(bytes) => Ok(Address(bytes)),
        DeviceId::Uuid(_) => Err(atc::Error::NotInRange(id)),
    }
}

fn bluetooth(err: bluer::Error) -> atc::Error {
    atc::Error::Bluetooth(Box::new(err))
}

async fn default_adapter() -> Result<Adapter, atc::Error> {
    let session = bluer::Session::new().await.map_err(bluetooth)?;
    session.default_adapter().await.map_err(bluetooth)
}

async fn connect(adapter: &Adapter, address: Address) -> Result<Thermometer, atc::Error> {
    let device = adapter.device(address).map_err(bluetooth)?;
    let mut attempt = 0;
    loop {
        wait_until_known(adapter, address).await?;
        if device.is_connected().await.map_err(bluetooth)? {
            break;
        }
        attempt += 1;
        let error = match timeout(CONNECT_TIMEOUT, device.connect()).await {
            Ok(Ok(())) => continue,
            Ok(Err(err)) => bluetooth(err),
            Err(_) => atc::Error::ConnectTimeout,
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
    for service in device.services().await.map_err(bluetooth)? {
        for characteristic in service.characteristics().await.map_err(bluetooth)? {
            let uuid = characteristic.uuid().await.map_err(bluetooth)?;
            if let Some(family) = Firmware::from_command_uuid(uuid) {
                command = Some((family, characteristic));
            }
        }
    }
    let (family, command) = command.ok_or(atc::Error::NotPvvx)?;
    let notifications: Notifications = Box::pin(command.notify().await.map_err(bluetooth)?);
    // `write` sends without response, which both firmwares take (THB2 declares only that).
    Thermometer::open(Link { device, command }, notifications, family).await
}

/// Waits for BlueZ to list the device. It drops devices it has not heard from in a
/// while (e.g. after discovery restarts) and lists them again on their next advert.
async fn wait_until_known(adapter: &Adapter, address: Address) -> Result<(), atc::Error> {
    let deadline = Instant::now() + DEVICE_APPEAR_TIMEOUT;
    while !adapter
        .device_addresses()
        .await
        .map_err(bluetooth)?
        .contains(&address)
    {
        if Instant::now() >= deadline {
            return Err(atc::Error::NotInRange(DeviceId::Mac(address.0)));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Ok(())
}

/// Why scanning stopped, as shown to the user.
struct Failure(String);

impl From<bluer::Error> for Failure {
    fn from(err: bluer::Error) -> Self {
        Self(err.to_string())
    }
}

trait Context<T> {
    /// Prefixes an error with what was being done.
    fn context(self, doing: &str) -> Result<T, Failure>;
}

impl<T> Context<T> for bluer::Result<T> {
    fn context(self, doing: &str) -> Result<T, Failure> {
        self.map_err(|err| Failure(format!("{doing}: {err}")))
    }
}

async fn start_discovery(
    adapter: &Adapter,
) -> Result<impl Stream<Item = AdapterEvent> + use<>, Failure> {
    adapter
        .set_discovery_filter(DiscoveryFilter {
            transport: DiscoveryTransport::Le,
            // Report every advert, not only the first one per device.
            duplicate_data: true,
            ..Default::default()
        })
        .await
        .context("setting the discovery filter")?;
    adapter
        .discover_devices()
        .await
        .context("starting discovery")
}

/// Scans until the app is gone (`Ok`) or BlueZ fails.
async fn scan(
    events: &UnboundedSender<ScanEvent>,
    controls: &mut UnboundedReceiver<ScanControl>,
    paused: &mut bool,
) -> Result<(), Failure> {
    let session = bluer::Session::new().await.context("connecting to BlueZ")?;
    let adapter = session
        .default_adapter()
        .await
        .context("finding a Bluetooth adapter")?;
    if !adapter.is_powered().await? {
        return Err(Failure(format!(
            "Bluetooth adapter {} is powered off",
            adapter.name()
        )));
    }
    let mut discovery = if *paused {
        None
    } else {
        Some(Box::pin(start_discovery(&adapter).await?))
    };
    if events
        .send(ScanEvent::Started {
            adapter: adapter.name().to_owned(),
        })
        .is_err()
    {
        return Ok(());
    }

    let mut device_events = SelectAll::new();
    let mut watched: HashMap<Address, Watched> = HashMap::new();
    let mut subscriptions = 0;
    loop {
        if discovery.is_none() && !*paused {
            discovery = Some(Box::pin(start_discovery(&adapter).await?));
        }
        tokio::select! {
            control = controls.recv() => match control {
                None => return Ok(()),
                Some(ScanControl::Pause) => *paused = true,
                Some(ScanControl::Resume) => {
                    *paused = false;
                    // BlueZ may have resumed discovery by itself with duplicate
                    // filtering, after which adverts trickle in only rarely. Dropping
                    // the stream stops it; give BlueZ a moment before it starts again,
                    // with the filter, at the top of the loop.
                    drop(discovery.take());
                    tokio::time::sleep(DISCOVERY_RESTART_PAUSE).await;
                }
            },
            event = next_event(&mut discovery) => match event {
                Some(AdapterEvent::DeviceAdded(address)) if !watched.contains_key(&address) => {
                    subscriptions += 1;
                    device_events.push(watch(&adapter, address, subscriptions).await?);
                    watched.insert(address, Watched::new(subscriptions));
                }
                Some(AdapterEvent::DeviceRemoved(address)) => {
                    // BlueZ drops idle devices; subscribe again when they come back.
                    watched.remove(&address);
                }
                Some(_) => {}
                None => {
                    // BlueZ stopped discovering: for a connection, or the adapter was
                    // switched off. It starts again at the top of the loop unless
                    // jobs are running; if it can't, scanning starts over.
                    drop(discovery.take());
                    tokio::time::sleep(DISCOVERY_RESTART_PAUSE).await;
                }
            },
            Some((address, subscription, event)) = device_events.next() => {
                let Some(device) = watched
                    .get_mut(&address)
                    .filter(|device| device.subscription == subscription)
                else {
                    continue;
                };
                let Some(DeviceEvent::PropertyChanged(property)) = event else {
                    // BlueZ removed the device while discovery was paused, so no
                    // DeviceRemoved arrived; it may already be back.
                    watched.remove(&address);
                    if adapter.device_addresses().await?.contains(&address) {
                        subscriptions += 1;
                        device_events.push(watch(&adapter, address, subscriptions).await?);
                        watched.insert(address, Watched::new(subscriptions));
                    }
                    continue;
                };
                match property {
                    DeviceProperty::Rssi(value) => device.rssi = Some(value),
                    DeviceProperty::Alias(alias) => device.name = Some(alias),
                    DeviceProperty::ServiceData(mut data) => {
                        let Some(payload) = data.remove(&BTHOME_UUID) else { continue };
                        let name = match &device.name {
                            Some(name) => name.clone(),
                            None => device.name.insert(alias(&adapter, address).await).clone(),
                        };
                        let advert = Advert {
                            address: DeviceId::Mac(address.0),
                            name,
                            rssi: device.rssi,
                            payload,
                            received: SystemTime::now(),
                        };
                        if events.send(ScanEvent::Advert(advert)).is_err() {
                            return Ok(());
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// What the scanner knows about one BlueZ device object.
///
/// BlueZ removes idle devices and creates them anew when they are heard again, with
/// a name that may have changed in between without an `Alias` change being sent, so
/// nothing here outlives the object.
struct Watched {
    /// Tells this object's events apart from those of an earlier one at the same address.
    subscription: u64,
    /// Read from BlueZ on the first advert, then kept current by `Alias` changes.
    name: Option<String>,
    rssi: Option<i16>,
}

impl Watched {
    const fn new(subscription: u64) -> Self {
        Self {
            subscription,
            name: None,
            rssi: None,
        }
    }
}

/// The device's property changes, tagged with the address and `subscription` and
/// ended by `None` when BlueZ removes the device.
async fn watch(
    adapter: &Adapter,
    address: Address,
    subscription: u64,
) -> bluer::Result<impl Stream<Item = (Address, u64, Option<DeviceEvent>)> + use<>> {
    let events = adapter.device(address)?.events().await?;
    Ok(events
        .map(Some)
        .chain(stream::iter([None]))
        .map(move |event| (address, subscription, event)))
}

/// The next discovery event, `None` once BlueZ ended discovery; never ready while
/// discovery is stopped.
async fn next_event<S: Stream + Unpin>(stream: &mut Option<S>) -> Option<S::Item> {
    match stream {
        Some(stream) => stream.next().await,
        None => std::future::pending().await,
    }
}

async fn alias(adapter: &Adapter, address: Address) -> String {
    let alias = match adapter.device(address) {
        Ok(device) => device.alias().await.ok(),
        Err(_) => None,
    };
    alias.unwrap_or_else(|| address.to_string())
}
