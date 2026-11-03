//! Prints everything readable from one thermometer:
//! `cargo run -p atc --example probe -- A4:C1:38:xx:xx:xx` (on macOS, the UUID that
//! `thermo` shows in its Id column).

use std::time::Duration;

use atc::{DeviceId, Thermometer};
use btleplug::api::{Central as _, ScanFilter};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let id: DeviceId = std::env::args()
        .nth(1)
        .ok_or("usage: probe MAC-or-UUID")?
        .parse()?;
    let adapter = atc::default_adapter().await?;

    // The device must be known to the adapter; scan until it shows up.
    adapter.start_scan(ScanFilter::default()).await?;
    tokio::time::timeout(Duration::from_secs(30), async {
        while atc::find_peripheral(&adapter, id).await?.is_none() {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        Ok::<_, atc::Error>(())
    })
    .await??;
    adapter.stop_scan().await?;

    let mut thermometer = Thermometer::connect(&adapter, id).await?;
    let info = thermometer.info();
    println!(
        "firmware   {} v{} hw {}{}",
        info.family,
        info.firmware_version(),
        info.hardware,
        info.model().map(|m| format!(" ({m})")).unwrap_or_default()
    );
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
    if let Some(sensor) = thermometer.sensor() {
        println!("sensor     {sensor:?}");
    }
    let clock = thermometer.clock().await?;
    println!(
        "clock      {clock:?} (device {:+.1} s vs host)",
        clock.offset_secs()
    );
    if info.has_log() {
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
    }
    thermometer.disconnect().await?;
    Ok(())
}
