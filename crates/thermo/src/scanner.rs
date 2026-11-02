//! Background BLE scan that forwards BTHome adverts from BlueZ.

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, ensure};
use bluer::{
    Adapter, AdapterEvent, Address, DeviceEvent, DeviceProperty, DiscoveryFilter,
    DiscoveryTransport, Uuid,
};
use futures::stream::{self, SelectAll};
use futures::{Stream, StreamExt};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

/// BTHome's 16-bit UUID expanded with the Bluetooth base UUID.
const BTHOME_UUID: Uuid = Uuid::from_u128(
    ((bthome::SERVICE_UUID16 as u128) << 96) | 0x0000_0000_0000_1000_8000_0080_5f9b_34fb,
);

const DISCOVERY_RESTART_PAUSE: Duration = Duration::from_millis(500);
/// Wait before starting over after an error, e.g. while the adapter is switched off
/// or `bluetoothd` restarts.
const RETRY_PAUSE: Duration = Duration::from_secs(5);

/// Something the scanner wants the app to know.
#[derive(Debug)]
pub enum ScanEvent {
    /// Discovery is running on the named adapter.
    Started { adapter: String },
    /// A device sent new BTHome service data.
    Advert(Advert),
    /// Scanning stopped; it starts over every few seconds until it works again.
    Failed(String),
}

/// What the app tells the scanner about device connections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanControl {
    /// Device jobs are about to connect. A connection stops discovery, and it is not
    /// restarted until [`ScanControl::Resume`].
    Pause,
    /// Device jobs are over: restart discovery. BlueZ may have resumed it by itself
    /// with duplicate filtering, after which adverts trickle in only rarely;
    /// restarting re-applies the filter.
    Resume,
}

/// One BTHome advertisement, still undecoded.
#[derive(Debug, Clone)]
pub struct Advert {
    pub address: Address,
    pub name: String,
    pub rssi: Option<i16>,
    pub payload: Vec<u8>,
    pub received: SystemTime,
}

/// Scans until the app is gone. A failure is reported as [`ScanEvent::Failed`], and
/// scanning starts over with a new BlueZ session after [`RETRY_PAUSE`].
pub async fn run(events: UnboundedSender<ScanEvent>, mut controls: UnboundedReceiver<ScanControl>) {
    let mut paused = false;
    while let Err(err) = scan(&events, &mut controls, &mut paused).await {
        if events.send(ScanEvent::Failed(format!("{err:#}"))).is_err() {
            return;
        }
        tokio::time::sleep(RETRY_PAUSE).await;
    }
}

async fn start_discovery(adapter: &Adapter) -> Result<impl Stream<Item = AdapterEvent> + use<>> {
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
) -> Result<()> {
    let session = bluer::Session::new().await.context("connecting to BlueZ")?;
    let adapter = session
        .default_adapter()
        .await
        .context("finding a Bluetooth adapter")?;
    ensure!(
        adapter.is_powered().await?,
        "Bluetooth adapter {} is powered off",
        adapter.name()
    );
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
                    // Dropping the stream stops discovery; give BlueZ a moment
                    // before it starts again at the top of the loop.
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
                            address,
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
) -> Result<impl Stream<Item = (Address, u64, Option<DeviceEvent>)> + use<>> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bthome_uuid_matches_spec() {
        assert_eq!(
            BTHOME_UUID.to_string(),
            "0000fcd2-0000-1000-8000-00805f9b34fb"
        );
    }
}
