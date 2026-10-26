//! Background BLE scan that forwards BTHome adverts from BlueZ.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail, ensure};
use bluer::{
    Adapter, AdapterEvent, Address, DeviceEvent, DeviceProperty, DiscoveryFilter,
    DiscoveryTransport, Uuid,
};
use futures::stream::SelectAll;
use futures::{Stream, StreamExt};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

/// BTHome's 16-bit UUID expanded with the Bluetooth base UUID.
const BTHOME_UUID: Uuid = Uuid::from_u128(
    ((bthome::SERVICE_UUID16 as u128) << 96) | 0x0000_0000_0000_1000_8000_0080_5f9b_34fb,
);

const DISCOVERY_RESTART_PAUSE: Duration = Duration::from_millis(500);

/// Something the scanner wants the app to know.
#[derive(Debug)]
pub enum ScanEvent {
    /// Discovery is running on the named adapter.
    Started { adapter: String },
    /// A device sent new BTHome service data.
    Advert(Advert),
    /// Scanning stopped and will not resume.
    Failed(String),
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

/// Scans until the receiver is dropped, reporting a failure as [`ScanEvent::Failed`].
///
/// A message on `restarts` restarts discovery. BlueZ suspends discovery while it
/// connects to a device and may resume it with duplicate filtering, after which
/// adverts trickle in only rarely; restarting re-applies the filter.
pub async fn run(events: UnboundedSender<ScanEvent>, restarts: UnboundedReceiver<()>) {
    if let Err(err) = scan(&events, restarts).await {
        // The app may already be gone; nothing else to do with the error then.
        let _ = events.send(ScanEvent::Failed(format!("{err:#}")));
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

async fn scan(
    events: &UnboundedSender<ScanEvent>,
    mut restarts: UnboundedReceiver<()>,
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
    let mut discovery = Some(Box::pin(start_discovery(&adapter).await?));
    if events
        .send(ScanEvent::Started {
            adapter: adapter.name().to_owned(),
        })
        .is_err()
    {
        return Ok(());
    }

    let mut device_events = SelectAll::new();
    let mut watched = HashSet::new();
    let mut names: HashMap<Address, String> = HashMap::new();
    let mut rssi: HashMap<Address, i16> = HashMap::new();
    loop {
        tokio::select! {
            Some(()) = restarts.recv() => {
                // Dropping the stream stops discovery; give BlueZ a moment before restarting.
                drop(discovery.take());
                tokio::time::sleep(DISCOVERY_RESTART_PAUSE).await;
                discovery = Some(Box::pin(start_discovery(&adapter).await?));
            }
            Some(event) = next_event(&mut discovery) => match event {
                AdapterEvent::DeviceAdded(address) if watched.insert(address) => {
                    let device = adapter.device(address)?;
                    device_events.push(device.events().await?.map(move |e| (address, e)));
                }
                AdapterEvent::DeviceRemoved(address) => {
                    // BlueZ drops idle devices; subscribe again when they come back.
                    watched.remove(&address);
                }
                _ => {}
            },
            Some((address, DeviceEvent::PropertyChanged(property))) = device_events.next() => {
                match property {
                    DeviceProperty::Rssi(value) => {
                        rssi.insert(address, value);
                    }
                    DeviceProperty::Alias(alias) => {
                        names.insert(address, alias);
                    }
                    DeviceProperty::ServiceData(mut data) => {
                        let Some(payload) = data.remove(&BTHOME_UUID) else { continue };
                        let name = if let Some(name) = names.get(&address) {
                            name.clone()
                        } else {
                            let name = alias(&adapter, address).await;
                            names.insert(address, name.clone());
                            name
                        };
                        let advert = Advert {
                            address,
                            name,
                            rssi: rssi.get(&address).copied(),
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
            else => break,
        }
    }
    bail!("BlueZ ended discovery")
}

async fn next_event<S: Stream + Unpin>(stream: &mut Option<S>) -> Option<S::Item> {
    match stream {
        Some(stream) => stream.next().await,
        None => None,
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
