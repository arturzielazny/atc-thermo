//! Prints everything readable from one thermometer: `cargo run -p ble --example probe -- A4:C1:38:xx:xx:xx`.

use std::time::Duration;

use atc::DeviceId;
use ble::{Bluetooth, ScanControl, ScanEvent};
use tokio::sync::mpsc;

const HEAR_TIMEOUT: Duration = Duration::from_secs(90);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let id: DeviceId = std::env::args().nth(1).ok_or("usage: probe MAC")?.parse()?;
    let bluetooth = Bluetooth::default();

    // The BLE stack must have heard the device; scan until it advertises. On macOS a
    // device's first adverts wait for the backend to read its MAC, which can take two
    // 30 s connect attempts.
    let (events_tx, mut events) = mpsc::unbounded_channel();
    let (controls, controls_rx) = mpsc::unbounded_channel();
    tokio::spawn(bluetooth.clone().scan(events_tx, controls_rx));
    tokio::time::timeout(HEAR_TIMEOUT, async {
        while let Some(event) = events.recv().await {
            if matches!(&event, ScanEvent::Advert(advert) if advert.address == id) {
                break;
            }
        }
    })
    .await
    .map_err(|_| format!("{id} not heard in {} s", HEAR_TIMEOUT.as_secs()))?;
    controls.send(ScanControl::Pause)?;

    let mut thermometer = bluetooth.connect(id).await?;
    let info = thermometer.info();
    println!("firmware v{} hw {}", info.firmware_version(), info.hardware);
    println!("name       {}", thermometer.name().await?);
    println!("mac        {}", thermometer.mac().await?);
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
