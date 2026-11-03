//! Background worker that connects to devices one at a time and runs commands on them.

use std::collections::VecDeque;
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime};

use atc::{
    Adapter, ClockReading, Coefficients, Config, Correction, DeviceId, Firmware, ScreenMessage,
    Smiley, Thermometer, TxPower,
};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::ble::{Ble, PAUSE_SCAN_FOR_JOBS};
use crate::devices::LoggedSample;

/// Longest a single job may take, including connecting.
const JOB_TIMEOUT: Duration = Duration::from_secs(180);
/// Where scanning goes on during jobs, a job starts only once its device advertised
/// within this window, and fails as unheard once it was due this long without one.
/// The connect request itself waits for the next advert, so the window only has to
/// show the device is around; `THERMO_ADVERT_WINDOW` (seconds) overrides it.
const ADVERT_WINDOW: Duration = Duration::from_secs(60);
/// ATC devices are heard less reliably by CoreBluetooth while it connects to others,
/// so they get this many windows.
const ATC_WINDOWS: u32 = 2;
/// Status hint for THB2 devices whose connect timed out: a short press switches
/// them to 1.5 s adverts for 60 s (`increase_advertising_frequency` in `thb2_main.c`).
const THB_CONNECT_HINT: &str = "press the device button to connect faster";
/// How often the worker checks for fresh adverts while jobs wait.
const POLL: Duration = Duration::from_millis(200);
/// Tries per job whose connection failed. In between, other devices' jobs run.
const JOB_ATTEMPTS: u32 = 3;
/// Wait before the second and third try.
const RETRY_BACKOFF: [Duration; 2] = [Duration::from_secs(10), Duration::from_secs(30)];
/// Wait after a failed connect before checking for a late, unwanted connection.
const LATE_CONNECT_GRACE: Duration = Duration::from_secs(3);
/// How long a device shows its number; long enough for a whole row of devices to be
/// reached one connection after another.
pub const IDENTIFY_DURATION: Duration = Duration::from_secs(30 * 60);

/// Settings changes; `None` leaves a value as it is on the device.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Settings {
    pub advertising_interval: Option<Duration>,
    pub measure_interval: Option<u8>,
    /// In dBm; each device takes its firmware's nearest level.
    pub tx_power: Option<f32>,
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
    /// Read the settings, to show them.
    ReadConfig,
    Configure(Settings),
    /// Change the TX power by about `change_db`, to the nearest documented level.
    AdjustTxPower {
        change_db: f32,
    },
    /// Set the TX power the firmware starts with, [`TxPower::factory`].
    ResetTxPower,
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
            Self::ReadConfig => "reading settings",
            Self::Configure(_) => "configuring",
            Self::AdjustTxPower { .. } => "adjusting TX power",
            Self::ResetTxPower => "restoring TX power",
            Self::Calibrate { .. } => "calibrating",
            Self::ResetCalibration => "resetting calibration",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub address: DeviceId,
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
    ConfigRead(Config),
    Configured(Config),
    /// The TX power changed from `before`; `config` is the device's config now.
    TxPowerSet {
        before: TxPower,
        config: Config,
    },
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
        address: DeviceId,
        label: &'static str,
    },
    Finished {
        address: DeviceId,
        outcome: Result<Outcome, String>,
    },
    /// Connecting failed; the job stays queued and is tried again after `retry_in`,
    /// while other devices' jobs go ahead.
    Retrying {
        address: DeviceId,
        error: String,
        retry_in: Duration,
    },
}

/// Starts the worker; jobs sent to the returned sender run in order.
pub fn spawn(ble: Ble, events: UnboundedSender<JobEvent>) -> UnboundedSender<Job> {
    let (jobs_tx, jobs_rx) = mpsc::unbounded_channel();
    tokio::spawn(run(ble, jobs_rx, events));
    jobs_tx
}

/// A job waiting in the worker's queue.
#[derive(Debug)]
struct Pending {
    job: Job,
    /// Tries made so far.
    tries: u32,
    /// Not to be started before this, after a failed try.
    not_before: Instant,
}

/// What the worker does next.
#[derive(Debug, PartialEq, Eq)]
enum Pick {
    /// Start the job at this queue index.
    Run(usize),
    /// The job at this index was due for its device's advert window without hearing
    /// the device in it.
    Unheard(usize),
    /// Nothing can start yet.
    Wait,
}

/// [`ADVERT_WINDOW`], or `THERMO_ADVERT_WINDOW` seconds if set.
fn base_advert_window() -> Duration {
    static WINDOW: OnceLock<Duration> = OnceLock::new();
    *WINDOW.get_or_init(|| {
        let Ok(value) = std::env::var("THERMO_ADVERT_WINDOW") else {
            return ADVERT_WINDOW;
        };
        match value.trim().parse::<u64>() {
            Ok(secs) if secs > 0 => {
                log::info!("advert window {secs} s (THERMO_ADVERT_WINDOW)");
                Duration::from_secs(secs)
            }
            _ => {
                log::warn!("ignoring THERMO_ADVERT_WINDOW={value:?}: not a positive number");
                ADVERT_WINDOW
            }
        }
    })
}

/// How recent an advert must be for a job on a device of `firmware` to start, and
/// how long the job waits for one.
fn advert_window(base: Duration, firmware: Option<Firmware>) -> Duration {
    match firmware {
        Some(Firmware::Atc) => base * ATC_WINDOWS,
        _ => base,
    }
}

/// Picks the next job: the first in the queue that is due, whose device has no
/// earlier job waiting (jobs for one device keep their order), and, when scanning
/// goes on during jobs, whose device advertised within its window. A job whose
/// device stayed silent for a whole window after it fell due is reported unheard;
/// time spent on other devices' jobs counts only if the device was silent then too.
fn pick(
    queue: &VecDeque<Pending>,
    now: Instant,
    scan_during_jobs: bool,
    heard: impl Fn(DeviceId) -> (Option<Instant>, Duration),
) -> Pick {
    let mut seen = Vec::new();
    let mut unheard = None;
    for (i, pending) in queue.iter().enumerate() {
        let address = pending.job.address;
        if seen.contains(&address) {
            continue;
        }
        seen.push(address);
        if pending.not_before > now {
            continue;
        }
        if !scan_during_jobs {
            return Pick::Run(i);
        }
        let (last, window) = heard(address);
        if last.is_some_and(|at| now.saturating_duration_since(at) <= window) {
            return Pick::Run(i);
        }
        if unheard.is_none() && now.saturating_duration_since(pending.not_before) >= window {
            unheard = Some(i);
        }
    }
    unheard.map_or(Pick::Wait, Pick::Unheard)
}

/// Why a job did not finish.
#[derive(Debug)]
struct Failure {
    message: String,
    /// Failed before the action wrote anything, so trying again is safe.
    retryable: bool,
    /// The connect request was never taken up.
    connect_timeout: bool,
}

fn queued(job: Job) -> Pending {
    log::info!("{}: queued {}", job.address, job.action.label());
    Pending {
        job,
        tries: 0,
        not_before: Instant::now(),
    }
}

async fn run(ble: Ble, mut jobs: UnboundedReceiver<Job>, events: UnboundedSender<JobEvent>) {
    let mut queue: VecDeque<Pending> = VecDeque::new();
    let mut open = true;
    loop {
        while let Ok(job) = jobs.try_recv() {
            queue.push_back(queued(job));
        }
        if queue.is_empty() {
            match jobs.recv().await {
                Some(job) => queue.push_back(queued(job)),
                None => return,
            }
            continue;
        }
        let base = base_advert_window();
        let picked = pick(&queue, Instant::now(), !PAUSE_SCAN_FOR_JOBS, |id| {
            (ble.last_heard(id), advert_window(base, ble.firmware(id)))
        });
        let (index, unheard) = match picked {
            Pick::Run(i) => (i, false),
            Pick::Unheard(i) => (i, true),
            Pick::Wait if open => {
                tokio::select! {
                    job = jobs.recv() => match job {
                        Some(job) => queue.push_back(queued(job)),
                        None => open = false,
                    },
                    () = tokio::time::sleep(POLL) => {}
                }
                continue;
            }
            Pick::Wait => {
                tokio::time::sleep(POLL).await;
                continue;
            }
        };
        let Some(pending) = queue.remove(index) else {
            continue;
        };
        let Some(event) = attempt(&ble, &events, &mut queue, index, pending, unheard).await else {
            return;
        };
        if events.send(event).is_err() {
            return;
        }
    }
}

/// Puts a job to be retried back where it was taken from, at `index`. [`pick`] skips
/// it until it is due and holds back its device's later jobs meanwhile, so those keep
/// their order while other devices go first.
fn requeue(queue: &mut VecDeque<Pending>, index: usize, pending: Pending) {
    queue.insert(index.min(queue.len()), pending);
}

/// Tries `pending`, taken from `queue` at `index`, once; a retryable failure puts it
/// back there. Returns the event to report, or `None` if the app is gone.
async fn attempt(
    ble: &Ble,
    events: &UnboundedSender<JobEvent>,
    queue: &mut VecDeque<Pending>,
    index: usize,
    mut pending: Pending,
    unheard: bool,
) -> Option<JobEvent> {
    pending.tries += 1;
    let address = pending.job.address;
    let label = pending.job.action.label();
    let started = Instant::now();
    let result = if unheard {
        let silent = ble.last_heard(address).map_or_else(
            || "never heard".to_owned(),
            |at| format!("not heard for {} s", at.elapsed().as_secs()),
        );
        Err(Failure {
            message: format!(
                "{silent}: out of range, or connected to another app (Home Assistant?)"
            ),
            retryable: true,
            connect_timeout: false,
        })
    } else {
        log::info!(
            "{address}: {label}, try {}/{JOB_ATTEMPTS}{}",
            pending.tries,
            ble.last_heard(address)
                .map_or_else(String::new, |at| format!(
                    ", last advert {} ms ago",
                    at.elapsed().as_millis()
                ))
        );
        events.send(JobEvent::Started { address, label }).ok()?;
        match ble.adapter().await {
            Ok(adapter) => execute_with_timeout(ble, &adapter, pending.job.clone()).await,
            Err(err) => Err(Failure {
                message: format!("no Bluetooth adapter: {err}"),
                retryable: false,
                connect_timeout: false,
            }),
        }
    };
    let result = result.map_err(|mut failure| {
        if failure.connect_timeout && ble.firmware(address) == Some(Firmware::Thb) {
            failure.message = format!("{}; {THB_CONNECT_HINT}", failure.message);
        }
        failure
    });
    let elapsed = started.elapsed();
    Some(match result {
        Ok(outcome) => {
            log::info!("{address}: {label} done in {elapsed:?}");
            JobEvent::Finished {
                address,
                outcome: Ok(outcome),
            }
        }
        Err(failure) if failure.retryable && pending.tries < JOB_ATTEMPTS => {
            let step = usize::try_from(pending.tries - 1).unwrap_or(usize::MAX);
            let retry_in = RETRY_BACKOFF[step.min(RETRY_BACKOFF.len() - 1)];
            log::warn!(
                "{address}: {label} failed after {elapsed:?}: {}; retrying in {retry_in:?}",
                failure.message
            );
            pending.not_before = Instant::now() + retry_in;
            requeue(queue, index, pending);
            JobEvent::Retrying {
                address,
                error: failure.message,
                retry_in,
            }
        }
        Err(failure) => {
            log::warn!(
                "{address}: {label} failed after {elapsed:?}: {}",
                failure.message
            );
            JobEvent::Finished {
                address,
                outcome: Err(failure.message),
            }
        }
    })
}

async fn execute_with_timeout(ble: &Ble, adapter: &Adapter, job: Job) -> Result<Outcome, Failure> {
    let address = job.address;
    if let Ok(result) = tokio::time::timeout(JOB_TIMEOUT, execute(ble, adapter, job)).await {
        return result;
    }
    // The cancelled job may have left the connection open.
    let _ = atc::disconnect(adapter, address).await;
    Err(Failure {
        message: format!("timed out after {} s", JOB_TIMEOUT.as_secs()),
        retryable: false,
        connect_timeout: false,
    })
}

async fn execute(ble: &Ble, adapter: &Adapter, job: Job) -> Result<Outcome, Failure> {
    let mut thermometer = match Thermometer::connect(adapter, job.address).await {
        Ok(thermometer) => thermometer,
        Err(err) => {
            // The stack can finish a connection after we gave up on it; a connected
            // thermometer stops advertising, so make sure none is left behind.
            tokio::time::sleep(LATE_CONNECT_GRACE).await;
            let _ = atc::disconnect(adapter, job.address).await;
            // Connecting writes nothing but the read-only device id request.
            let retryable = !matches!(
                err,
                atc::Error::NotPvvx | atc::Error::UnsupportedFirmware { .. }
            );
            return Err(Failure {
                message: err.to_string(),
                retryable,
                connect_timeout: err.is_connect_timeout(),
            });
        }
    };
    ble.learnt(job.address, thermometer.firmware());
    let result = perform(&mut thermometer, job.action).await;
    if let Err(err) = &result {
        log::warn!("{}: action failed: {err}", job.address);
    }
    // The device restarts or carries on by itself; a failed disconnect changes nothing.
    let _ = thermometer.disconnect().await;
    result.map_err(|e| Failure {
        message: e.to_string(),
        retryable: false,
        connect_timeout: false,
    })
}

async fn perform(t: &mut Thermometer, action: Action) -> Result<Outcome, atc::Error> {
    match action {
        Action::Download { since } => download(t, since).await,
        Action::SyncClock { download_since } => {
            let before = t.clock().await?;
            let samples = read_samples(t, &before, download_since).await?;
            // Before setting the clock, so the new rate starts with the new time.
            let rate = trim_rate(t, &before).await?;
            t.set_clock_to_now().await?;
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
        Action::ReadConfig => Ok(Outcome::ConfigRead(t.config().await?)),
        Action::AdjustTxPower { change_db } => {
            set_tx_power(t, |before| {
                before
                    .changed_by(change_db)
                    .ok_or_else(|| atc::Error::InvalidValue(format!("unknown TX power {before}")))
            })
            .await
        }
        Action::ResetTxPower => {
            set_tx_power(t, |before| Ok(TxPower::factory(before.firmware()))).await
        }
        Action::Calibrate {
            temperature,
            humidity,
        } => {
            let before = t.coefficients().await?;
            let max_gain = t.max_gain().await?;
            let wanted = before.corrected_within(temperature, humidity, max_gain)?;
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

/// Sets the TX power `wanted` returns for the current one; writes nothing if they match.
async fn set_tx_power(
    t: &mut Thermometer,
    wanted: impl FnOnce(TxPower) -> Result<TxPower, atc::Error>,
) -> Result<Outcome, atc::Error> {
    let mut config = t.config().await?;
    let before = config.tx_power();
    let wanted = wanted(before)?;
    if wanted == before {
        return Ok(Outcome::TxPowerSet { before, config });
    }
    config.set_tx_power(wanted);
    let config = t.set_config(&config).await?;
    if config.tx_power() != wanted {
        return Err(atc::Error::InvalidValue(format!(
            "device stored TX power {} instead of {wanted}",
            config.tx_power()
        )));
    }
    Ok(Outcome::TxPowerSet { before, config })
}

async fn trim_rate(t: &mut Thermometer, clock: &ClockReading) -> Result<RateTrim, atc::Error> {
    if !t.info().has_clock_rate() {
        return Ok(RateTrim::Unsupported);
    }
    let Some(drift) = clock.drift() else {
        return Ok(RateTrim::Unknown);
    };
    let current = match t.second_ticks().await {
        Ok(ticks) => ticks,
        Err(atc::Error::Timeout { .. } | atc::Error::Unsupported(_)) => {
            return Ok(RateTrim::Unsupported);
        }
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
    if let Some(dbm) = settings.tx_power {
        config.set_tx_power(TxPower::nearest(config.firmware(), dbm));
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

    #[test]
    fn tx_power_setting_maps_per_firmware() {
        let settings = Settings {
            tx_power: Some(-5.03),
            ..Settings::default()
        };
        let mut atc_config = Config::from_bytes([
            0x87, 0x10, 0x0a, 0x06, 0x28, 0x04, 0xa9, 0x31, 0x31, 0x04, 0xb4,
        ]);
        apply_settings(&mut atc_config, &settings).expect("valid");
        assert_eq!(atc_config.tx_power().code(), 150);
        let mut thb_config =
            Config::from_thb_bytes([0x05, 0, 0, 0, 0x1f, 80, 29, 16, 2, 60, 180, 0]);
        apply_settings(&mut thb_config, &settings).expect("valid");
        assert_eq!(thb_config.tx_power().code(), 0x0a);
    }

    fn pending(address: DeviceId, not_before: Instant) -> Pending {
        Pending {
            job: Job {
                address,
                action: Action::ReadConfig,
            },
            tries: 0,
            not_before,
        }
    }

    const A: DeviceId = DeviceId::Mac([0xa4, 0xc1, 0x38, 0x27, 0xc6, 0x22]);
    const B: DeviceId = DeviceId::Mac([0x38, 0x1f, 0x8d, 0x26, 0x70, 0x5b]);

    const W: Duration = ADVERT_WINDOW;

    #[test]
    fn picks_a_device_heard_within_its_window() {
        let now = Instant::now();
        let queue: VecDeque<_> = [pending(A, now), pending(B, now)].into();
        let only_b = |id| ((id == B).then_some(now), W);
        assert_eq!(pick(&queue, now, true, only_b), Pick::Run(1));
        // Heard a while ago, but within the window: the connect waits for the next advert.
        let earlier = |_| (Some(now), W);
        assert_eq!(pick(&queue, now + W, true, earlier), Pick::Run(0));
        // Without adverts nothing starts, until a whole window passed.
        assert_eq!(pick(&queue, now, true, |_| (None, W)), Pick::Wait);
        assert_eq!(pick(&queue, now + W, true, |_| (None, W)), Pick::Unheard(0));
        // BlueZ: discovery is off during jobs, so adverts don't matter.
        assert_eq!(pick(&queue, now, false, |_| (None, W)), Pick::Run(0));
    }

    #[test]
    fn busy_time_counts_as_unheard_only_if_the_device_was_silent() {
        let due = Instant::now();
        let queue: VecDeque<_> = [pending(A, due)].into();
        // The worker was busy for two windows; the device spoke a moment ago.
        let now = due + 2 * W;
        let recent = |_| (Some(due + W + Duration::from_secs(48)), W);
        assert_eq!(pick(&queue, now, true, recent), Pick::Run(0));
        let long_ago = |_| (Some(due), W);
        assert_eq!(pick(&queue, now, true, long_ago), Pick::Unheard(0));
    }

    #[test]
    fn atc_devices_get_a_longer_window() {
        assert_eq!(advert_window(W, Some(Firmware::Thb)), W);
        assert_eq!(advert_window(W, None), W);
        assert_eq!(advert_window(W, Some(Firmware::Atc)), W * ATC_WINDOWS);
    }

    #[test]
    fn keeps_order_per_device_and_skips_deferred() {
        let now = Instant::now();
        let later = now + Duration::from_secs(10);
        // A's first job is deferred: its second must not overtake it, B goes ahead.
        let queue: VecDeque<_> = [pending(A, later), pending(A, now), pending(B, now)].into();
        assert_eq!(pick(&queue, now, true, |_| (Some(now), W)), Pick::Run(2));
        assert_eq!(pick(&queue, now, false, |_| (None, W)), Pick::Run(2));
        assert_eq!(pick(&queue, later, false, |_| (None, W)), Pick::Run(0));
    }

    #[test]
    fn retried_job_stays_ahead_of_its_devices_later_jobs() {
        let now = Instant::now();
        let mut queue: VecDeque<_> = [pending(A, now), pending(A, now), pending(B, now)].into();
        let mut first = queue.remove(0).expect("queued");
        first.tries = 1;
        first.not_before = now + RETRY_BACKOFF[0];
        requeue(&mut queue, 0, first);
        let heard = |_| (Some(now), W);
        // A's second job waits for the retried first one; B goes ahead.
        assert_eq!(pick(&queue, now, true, heard), Pick::Run(2));
        assert_eq!(pick(&queue, now, false, heard), Pick::Run(2));
        let due = now + RETRY_BACKOFF[0];
        let Pick::Run(i) = pick(&queue, due, true, |_| (Some(due), W)) else {
            panic!("a job is due");
        };
        assert_eq!(queue[i].tries, 1);
    }
}
