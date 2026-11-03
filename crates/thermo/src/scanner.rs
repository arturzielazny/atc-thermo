//! Background BLE scan that forwards BTHome adverts, through `btleplug` (BlueZ on
//! Linux, CoreBluetooth on macOS).

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail, ensure};
use btleplug::api::{Central as _, CentralEvent, CentralState, Peripheral as _, ScanFilter};
use btleplug::platform::PeripheralId;
use futures::StreamExt;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use uuid::Uuid;

use crate::ble::{Ble, PAUSE_SCAN_FOR_JOBS};
use atc::DeviceId;

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
    /// Device jobs are about to connect: stop scanning until [`ScanControl::Resume`].
    Pause,
    /// Device jobs are over: restart scanning. BlueZ may have resumed discovery by
    /// itself with duplicate filtering, after which adverts trickle in only rarely;
    /// restarting re-applies the filter.
    Resume,
}

/// One BTHome advertisement, still undecoded.
#[derive(Debug, Clone)]
pub struct Advert {
    pub address: DeviceId,
    pub name: String,
    pub rssi: Option<i16>,
    pub payload: Vec<u8>,
    pub received: SystemTime,
}

/// Scans until the app is gone. A failure is reported as [`ScanEvent::Failed`], and
/// scanning starts over with a fresh adapter after [`RETRY_PAUSE`].
pub async fn run(
    ble: Ble,
    events: UnboundedSender<ScanEvent>,
    mut controls: UnboundedReceiver<ScanControl>,
) {
    let mut paused = false;
    while let Err(err) = scan(&ble, &events, &mut controls, &mut paused).await {
        log::error!("scan failed: {err:#}");
        if events.send(ScanEvent::Failed(format!("{err:#}"))).is_err() {
            return;
        }
        ble.reset().await;
        tokio::time::sleep(RETRY_PAUSE).await;
    }
}

/// Scans everything: CoreBluetooth matches a service filter against advertised
/// service UUIDs, which BTHome devices may only send as service data.
fn scan_filter() -> ScanFilter {
    ScanFilter::default()
}

/// Scans until the app is gone (`Ok`) or the BLE stack fails.
async fn scan(
    ble: &Ble,
    events: &UnboundedSender<ScanEvent>,
    controls: &mut UnboundedReceiver<ScanControl>,
    paused: &mut bool,
) -> Result<()> {
    let adapter = ble.adapter().await.context("finding a Bluetooth adapter")?;
    let state = adapter.adapter_state().await?;
    ensure!(
        state == CentralState::PoweredOn,
        "Bluetooth is not available ({state:?}); on macOS, allow this terminal under \
         System Settings › Privacy & Security › Bluetooth"
    );
    let name = adapter
        .adapter_info()
        .await
        .unwrap_or_else(|_| "Bluetooth".to_owned());
    let mut central_events = adapter.events().await.context("watching the adapter")?;
    if !*paused {
        adapter
            .start_scan(scan_filter())
            .await
            .context("starting discovery")?;
    }
    log::info!("scanning on {name}");
    if events.send(ScanEvent::Started { adapter: name }).is_err() {
        return Ok(());
    }

    let mut known: HashMap<PeripheralId, Known> = HashMap::new();
    loop {
        tokio::select! {
            control = controls.recv() => match control {
                None => return Ok(()),
                Some(ScanControl::Pause | ScanControl::Resume) if !PAUSE_SCAN_FOR_JOBS => {}
                Some(ScanControl::Pause) => {
                    log::debug!("scan paused for device jobs");
                    *paused = true;
                    let _ = adapter.stop_scan().await;
                }
                Some(ScanControl::Resume) => {
                    log::debug!("scan restarted after device jobs");
                    *paused = false;
                    let _ = adapter.stop_scan().await;
                    tokio::time::sleep(DISCOVERY_RESTART_PAUSE).await;
                    adapter.start_scan(scan_filter()).await.context("restarting discovery")?;
                }
            },
            event = central_events.next() => match event {
                None => bail!("the Bluetooth adapter stopped sending events"),
                Some(CentralEvent::StateUpdate(state)) if state != CentralState::PoweredOn => {
                    log::warn!("adapter state {state:?}");
                    bail!("Bluetooth adapter switched off");
                }
                Some(CentralEvent::RssiUpdate { id, rssi }) => {
                    known.entry(id).or_default().rssi = Some(rssi);
                }
                Some(CentralEvent::DeviceDisconnected(id)) => {
                    log::debug!("stack reports {id} disconnected");
                    // A connection may have changed the name; read it again.
                    known.remove(&id);
                }
                Some(CentralEvent::ServiceDataAdvertisement { id, mut service_data }) => {
                    let Some(payload) = service_data.remove(&BTHOME_UUID) else { continue };
                    let Ok(peripheral) = adapter.peripheral(&id).await else { continue };
                    let properties = peripheral.properties().await.ok().flatten().unwrap_or_default();
                    let address = atc::device_id(&peripheral);
                    let entry = known.entry(id).or_default();
                    if let Some(rssi) = properties.rssi {
                        entry.rssi = Some(rssi);
                    }
                    if let Some(local) = properties.local_name.or(properties.advertisement_name) {
                        entry.name = Some(local);
                    }
                    let name = entry.name.clone().unwrap_or_else(|| address.short());
                    ble.heard(address, &name);
                    let advert = Advert {
                        address,
                        name,
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

/// What the scanner remembers about one peripheral between its adverts.
#[derive(Debug, Default)]
struct Known {
    name: Option<String>,
    rssi: Option<i16>,
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
