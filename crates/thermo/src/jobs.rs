//! Background worker that connects to devices one at a time and runs commands on them.

use std::time::{Duration, SystemTime};

use atc::{
    ClockReading, Coefficients, Config, Correction, ScreenMessage, Smiley, Thermometer, TxPower,
};
use bluer::{Adapter, Address};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::devices::LoggedSample;

/// Longest a single job may take, including connecting.
const JOB_TIMEOUT: Duration = Duration::from_secs(180);
/// Wait after a failed connect before checking for a late, unwanted connection.
const LATE_CONNECT_GRACE: Duration = Duration::from_secs(3);
/// How long a device shows its number; long enough for a whole row of devices to be
/// reached one connection after another.
pub const IDENTIFY_DURATION: Duration = Duration::from_secs(10 * 60);

/// Settings changes; `None` leaves a value as it is on the device.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Settings {
    pub advertising_interval: Option<Duration>,
    pub measure_interval: Option<u8>,
    pub tx_power: Option<TxPower>,
    /// Desired time per on-device log record.
    pub log_period: Option<Duration>,
}

impl Settings {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Download log records newer than `since` (host time).
    Download {
        since: SystemTime,
    },
    /// Download log records newer than `download_since` (they can only be dated with
    /// the old clock), trim the clock rate by the drift since the last sync, then set
    /// the clock to host time.
    SyncClock {
        download_since: SystemTime,
    },
    /// Show `number` on the device's screen for [`IDENTIFY_DURATION`].
    Identify {
        number: u8,
    },
    Rename(String),
    Configure(Settings),
    /// Compose a correction onto the device's current coefficients.
    Calibrate {
        temperature: Correction,
        humidity: Correction,
    },
    ResetCalibration,
}

impl Action {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Download { .. } => "downloading log",
            Self::SyncClock { .. } => "syncing clock",
            Self::Identify { .. } => "identifying",
            Self::Rename(_) => "renaming",
            Self::Configure(_) => "configuring",
            Self::Calibrate { .. } => "calibrating",
            Self::ResetCalibration => "resetting calibration",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub address: Address,
    pub action: Action,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Downloaded {
        samples: Vec<LoggedSample>,
        clock_offset: f64,
    },
    ClockSynced {
        offset_before: f64,
        rate: RateTrim,
        samples: Vec<LoggedSample>,
    },
    Identified {
        number: u8,
    },
    Renamed(String),
    Configured(Config),
    Calibrated {
        before: Coefficients,
        after: Coefficients,
        temperature: Correction,
        humidity: Correction,
    },
    CalibrationReset {
        before: Coefficients,
        after: Coefficients,
    },
}

/// What a clock sync did about the rate the clock drifts at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RateTrim {
    /// The clock was never synced, or too recently to measure its drift.
    Unknown,
    /// `drift` (fraction, positive = fast) is corrected from now on.
    Trimmed { drift: f64 },
    /// `drift` is more than the firmware can correct, which suggests the clock was
    /// last set to something other than this computer's time.
    TooLarge { drift: f64 },
    /// The firmware has no rate adjustment.
    Unsupported,
}

#[derive(Debug, Clone, PartialEq)]
pub enum JobEvent {
    Started {
        address: Address,
        label: &'static str,
    },
    Finished {
        address: Address,
        outcome: Result<Outcome, String>,
    },
}

/// Starts the worker; jobs sent to the returned sender run in order.
pub fn spawn(events: UnboundedSender<JobEvent>) -> UnboundedSender<Job> {
    let (jobs_tx, jobs_rx) = mpsc::unbounded_channel();
    tokio::spawn(run(jobs_rx, events));
    jobs_tx
}

async fn run(mut jobs: UnboundedReceiver<Job>, events: UnboundedSender<JobEvent>) {
    let adapter = match bluer::Session::new().await {
        Ok(session) => session.default_adapter().await.map_err(|e| e.to_string()),
        Err(err) => Err(err.to_string()),
    };
    while let Some(job) = jobs.recv().await {
        let address = job.address;
        if events
            .send(JobEvent::Started {
                address,
                label: job.action.label(),
            })
            .is_err()
        {
            return;
        }
        let outcome = match &adapter {
            Ok(adapter) => execute_with_timeout(adapter, job).await,
            Err(err) => Err(format!("no Bluetooth adapter: {err}")),
        };
        if events
            .send(JobEvent::Finished { address, outcome })
            .is_err()
        {
            return;
        }
    }
}

async fn execute_with_timeout(adapter: &Adapter, job: Job) -> Result<Outcome, String> {
    let address = job.address;
    if let Ok(result) = tokio::time::timeout(JOB_TIMEOUT, execute(adapter, job)).await {
        return result;
    }
    // The cancelled job may have left the connection open.
    if let Ok(device) = adapter.device(address) {
        let _ = device.disconnect().await;
    }
    Err("timed out".to_owned())
}

async fn execute(adapter: &Adapter, job: Job) -> Result<Outcome, String> {
    let mut thermometer = match Thermometer::connect(adapter, job.address).await {
        Ok(thermometer) => thermometer,
        Err(err) => {
            // BlueZ can finish a connection after we gave up on it; a connected
            // thermometer stops advertising, so make sure none is left behind.
            tokio::time::sleep(LATE_CONNECT_GRACE).await;
            if let Ok(device) = adapter.device(job.address)
                && device.is_connected().await.unwrap_or(false)
            {
                let _ = device.disconnect().await;
            }
            return Err(err.to_string());
        }
    };
    let result = perform(&mut thermometer, job.action).await;
    // The device restarts or carries on by itself; a failed disconnect changes nothing.
    let _ = thermometer.disconnect().await;
    result.map_err(|e| e.to_string())
}

async fn perform(t: &mut Thermometer, action: Action) -> Result<Outcome, atc::Error> {
    match action {
        Action::Download { since } => download(t, since).await,
        Action::SyncClock { download_since } => {
            let before = t.clock().await?;
            let samples = read_samples(t, &before, download_since).await?;
            // Before setting the clock, so the new rate starts with the new time.
            let rate = trim_rate(t, &before).await?;
            t.set_clock(SystemTime::now()).await?;
            Ok(Outcome::ClockSynced {
                offset_before: before.offset_secs(),
                rate,
                samples,
            })
        }
        Action::Identify { number } => {
            t.show(&identify_message(number)).await?;
            Ok(Outcome::Identified { number })
        }
        Action::Rename(name) => Ok(Outcome::Renamed(t.set_name(&name).await?)),
        Action::Configure(settings) => {
            let mut config = t.config().await?;
            apply_settings(&mut config, &settings)?;
            Ok(Outcome::Configured(t.set_config(&config).await?))
        }
        Action::Calibrate {
            temperature,
            humidity,
        } => {
            let before = t.coefficients().await?;
            let wanted = before.corrected(temperature, humidity)?;
            let after = t.set_coefficients(&wanted).await?;
            if after != wanted {
                return Err(atc::Error::InvalidValue(format!(
                    "device stored {after:?} instead of {wanted:?}"
                )));
            }
            Ok(Outcome::Calibrated {
                before,
                after,
                temperature,
                humidity,
            })
        }
        Action::ResetCalibration => {
            let before = t.coefficients().await?;
            let after = t.reset_coefficients().await?;
            Ok(Outcome::CalibrationReset { before, after })
        }
    }
}

/// `number` big and small with a happy face. Single digits are repeated (`333`) so
/// they show without the decimal point a reading has and can't pass for one.
fn identify_message(number: u8) -> ScreenMessage {
    let n = i16::from(number);
    let big_tenths = match n {
        1..=9 => n * 111 * 10,
        10..=19 => (n * 100 + n) * 10,
        _ => n * 10,
    };
    ScreenMessage {
        big_tenths,
        small: i8::try_from(number).unwrap_or(i8::MAX).min(99),
        smiley: Smiley::Happy,
        duration: IDENTIFY_DURATION,
    }
}

async fn trim_rate(t: &mut Thermometer, clock: &ClockReading) -> Result<RateTrim, atc::Error> {
    let Some(drift) = clock.drift() else {
        return Ok(RateTrim::Unknown);
    };
    let current = match t.second_ticks().await {
        Ok(ticks) => ticks,
        Err(atc::Error::Timeout { .. }) => return Ok(RateTrim::Unsupported),
        Err(err) => return Err(err),
    };
    let Some(ticks) = atc::trimmed_second_ticks(current, drift) else {
        return Ok(RateTrim::TooLarge { drift });
    };
    t.set_second_ticks(ticks).await?;
    Ok(RateTrim::Trimmed { drift })
}

async fn download(t: &mut Thermometer, since: SystemTime) -> Result<Outcome, atc::Error> {
    let clock = t.clock().await?;
    if t.config().await?.log_period().is_none() {
        return Err(atc::Error::InvalidValue(
            "logging is disabled on the device".into(),
        ));
    }
    let samples = read_samples(t, &clock, since).await?;
    Ok(Outcome::Downloaded {
        samples,
        clock_offset: clock.offset_secs(),
    })
}

/// Log records newer than `since`, dated with `clock`; empty if logging is off.
async fn read_samples(
    t: &mut Thermometer,
    clock: &ClockReading,
    since: SystemTime,
) -> Result<Vec<LoggedSample>, atc::Error> {
    let Some(period) = t.config().await?.log_period() else {
        return Ok(Vec::new());
    };
    let wanted_span = SystemTime::now().duration_since(since).unwrap_or_default();
    let max_records = (wanted_span.as_secs_f64() / period.as_secs_f64()).ceil() + 5.0;
    let max_records = u16::try_from(clamp_to_u16(max_records)).unwrap_or(u16::MAX);
    let records = t
        .read_log(max_records, |r| {
            clock.to_host(r.device_time).is_some_and(|at| at >= since)
        })
        .await?;
    Ok(records
        .iter()
        .filter_map(|r| {
            let end = clock.to_host(r.device_time)?;
            Some(LoggedSample {
                at: end.checked_sub(period / 2)?,
                temperature: r.temperature,
                humidity: r.humidity,
                voltage: r.voltage,
                span: period,
            })
        })
        .collect())
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "value is clamped to the u16 range first"
)]
fn clamp_to_u16(value: f64) -> u32 {
    value.clamp(0.0, f64::from(u16::MAX)) as u32
}

fn apply_settings(config: &mut Config, settings: &Settings) -> Result<(), atc::Error> {
    if let Some(interval) = settings.advertising_interval {
        config.set_advertising_interval(interval)?;
    }
    if let Some(adverts) = settings.measure_interval {
        config.set_measure_interval(adverts)?;
    }
    if let Some(power) = settings.tx_power {
        config.set_tx_power(power);
    }
    // After the other changes, since the log period counts measurements.
    if let Some(period) = settings.log_period {
        let measurements = period.as_secs_f64() / config.measurement_period().as_secs_f64();
        config.set_log_averaging(
            u8::try_from(clamp_to_u16(measurements.round().max(1.0))).unwrap_or(u8::MAX),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identify_numbers_cannot_pass_for_readings() {
        let big = |n| identify_message(n).big_tenths;
        // From 1996 tenths on the display drops the point: 3330 shows "333".
        assert_eq!(big(3), 3330);
        assert_eq!(big(12), 12_120);
        assert_eq!(identify_message(3).small, 3);
        assert_eq!(identify_message(200).small, 99);
        assert_eq!(identify_message(200).duration, IDENTIFY_DURATION);
    }
}
