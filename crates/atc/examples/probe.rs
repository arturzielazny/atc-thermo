//! Prints everything readable from one thermometer: `cargo run -p atc --example probe -- A4:C1:38:xx:xx:xx`.

use std::time::Duration;

use atc::Thermometer;
use bluer::{Address, DiscoveryFilter, DiscoveryTransport};
use futures::StreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let address: Address = std::env::args().nth(1).ok_or("usage: probe MAC")?.parse()?;
    let session = bluer::Session::new().await?;
    let adapter = session.default_adapter().await?;

    // The device must be known to BlueZ; scan until it shows up.
    adapter
        .set_discovery_filter(DiscoveryFilter {
            transport: DiscoveryTransport::Le,
            ..Default::default()
        })
        .await?;
    {
        // Discovery stops when the stream is dropped at the end of this block.
        let discovery = adapter.discover_devices().await?;
        tokio::pin!(discovery);
        tokio::time::timeout(Duration::from_secs(30), async {
            while let Some(event) = discovery.next().await {
                if matches!(event, bluer::AdapterEvent::DeviceAdded(a) if a == address) {
                    break;
                }
            }
        })
        .await?;
    }

    let mut thermometer = Thermometer::connect(&adapter, address).await?;
    let info = thermometer.info();
    println!("firmware v{} hw {}", info.firmware_version(), info.hardware);
    println!("name       {}", thermometer.name().await?);
    let config = thermometer.config().await?;
    println!(
        "config     adv {:?}, measure every {:?}, tx {}, log every {:?}",
        config.advertising_interval(),
        config.measurement_period(),
        config.tx_power(),
        config.log_period()
    );
    println!("coeffs     {:?}", thermometer.coefficients().await?);
    let clock = thermometer.clock().await?;
    println!(
        "clock      {clock:?} (device {:+.1} s vs host)",
        clock.offset_secs()
    );
    let mut count = 0;
    let records = thermometer
        .read_log(1000, |_| {
            count += 1;
            count <= 5
        })
        .await?;
    for r in &records {
        println!(
            "log        {:?} -> host {:?}",
            r,
            clock.to_host(r.device_time)
        );
    }
    thermometer.disconnect().await?;
    Ok(())
}
