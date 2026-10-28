//! Runs the consensus calibration on recorded history.

use std::collections::HashMap;
use std::fmt::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use atc::Correction;
use bluer::Address;
use calibration::{Fit, Options, Reading, Report, SensorReport};

use crate::devices::{Devices, Source};

/// A fit for some devices over a time window.
#[derive(Debug, Clone)]
pub struct Run {
    /// `(address, name)` per sensor, in [`Report::sensors`] order.
    pub devices: Vec<(Address, String)>,
    pub since: SystemTime,
    pub until: SystemTime,
    pub report: Report,
}

/// The window start: `requested`, but never before the last coefficient change of a
/// participating device, since older data was recorded with other coefficients.
pub fn window_start(
    requested: SystemTime,
    devices: &[Address],
    last_calibrations: &HashMap<Address, SystemTime>,
) -> SystemTime {
    devices
        .iter()
        .filter_map(|a| last_calibrations.get(a))
        .fold(requested, |start, &changed| start.max(changed))
}

/// Fits corrections for `addresses` using their live samples in `since..until`.
///
/// # Errors
///
/// Fails if there are too few devices or too little steady data.
pub fn run(
    devices: &Devices,
    addresses: &[Address],
    since: SystemTime,
    until: SystemTime,
) -> Result<Run, calibration::Error> {
    let mut names = Vec::new();
    let mut readings: Vec<Vec<Reading>> = Vec::new();
    for &address in addresses {
        let Some(device) = devices.get(address) else {
            continue;
        };
        let samples: Vec<Reading> = device
            .history()
            .iter()
            .filter(|s| s.source == Source::Live && s.at >= since && s.at <= until)
            .map(|s| Reading {
                at: s
                    .at
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64(),
                temperature: f64::from(s.temperature),
                humidity: f64::from(s.humidity),
            })
            .collect();
        names.push((address, device.name.clone()));
        readings.push(samples);
    }
    let slices: Vec<&[Reading]> = readings.iter().map(Vec::as_slice).collect();
    let report = calibration::calibrate(&slices, &Options::default())?;
    Ok(Run {
        devices: names,
        since,
        until,
        report,
    })
}

/// The temperature and humidity corrections to write for a device; `None` for one
/// that should not be applied.
///
/// An outlier in either quantity skips both: the sensor reads temperature and
/// humidity in the same spot, so whatever set it apart (a warmer, moister place, a
/// hand) skews the other fit too.
pub fn corrections(sensor: &SensorReport) -> (Option<Correction>, Option<Correction>) {
    let (t, h) = (&sensor.temperature, &sensor.humidity);
    if t.outlier || h.outlier {
        return (None, None);
    }
    (correction(t), correction(h))
}

/// The correction for one fit, or `None` with too little data for this device.
fn correction(fit: &Fit) -> Option<Correction> {
    (fit.steady >= Options::default().min_steady).then_some(Correction {
        gain: fit.gain,
        offset: fit.offset,
    })
}

/// Human-readable summary, as printed by `thermo calibrate`.
pub fn format(run: &Run) -> String {
    let r = &run.report;
    let mut out = String::new();
    let hours = run
        .until
        .duration_since(run.since)
        .unwrap_or_default()
        .as_secs_f64()
        / 3600.0;
    let _ = writeln!(
        out,
        "Window: last {hours:.1} h, {} devices",
        run.devices.len()
    );
    let _ = writeln!(
        out,
        "Steady data: {} min temperature, {} min humidity; range {:.1}–{:.1} °C, {:.0}–{:.0} %RH",
        r.temperature.steady.as_secs() / 60,
        r.humidity.steady.as_secs() / 60,
        r.temperature.range.0,
        r.temperature.range.1,
        r.humidity.range.0,
        r.humidity.range.1
    );
    let _ = writeln!(
        out,
        "Spread between devices: {:.3} → {:.3} °C, {:.2} → {:.2} %RH",
        r.temperature.spread.before,
        r.temperature.spread.after,
        r.humidity.spread.before,
        r.humidity.spread.after
    );
    let _ = writeln!(out, "Phases: {}", r.phases.len());
    let _ = writeln!(
        out,
        "\n{:<14} {:>9} {:>8} {:>7}  {:>9} {:>8} {:>7}  {:>6}  note",
        "device", "T offset", "T gain", "T rms", "H offset", "H gain", "H rms", "lag"
    );
    for ((_, name), sensor) in run.devices.iter().zip(&r.sensors) {
        let (t, h) = (sensor.temperature, sensor.humidity);
        let _ = writeln!(
            out,
            "{name:<14} {} {}  {:>6}  {}",
            format_fit(&t, 2),
            format_fit(&h, 1),
            format_lag(t.lag),
            notes(&t, &h)
        );
    }
    out
}

/// Columns `offset gain rms` for one quantity.
pub fn format_fit(fit: &Fit, decimals: usize) -> String {
    let gain = if fit.gain_fitted {
        format!("{:.4}", fit.gain)
    } else {
        "–".to_owned()
    };
    format!(
        "{:>+9.decimals$} {gain:>8} {:>7.decimals$}",
        fit.offset,
        fit.residual_rms,
        decimals = decimals + 1
    )
}

/// Temperature lag behind the group, e.g. `"+2.5m"`, or `"–"` if not estimated.
pub fn format_lag(lag: Option<f64>) -> String {
    lag.map_or_else(|| "–".to_owned(), |secs| format!("{:+.1}m", secs / 60.0))
}

pub fn notes(t: &Fit, h: &Fit) -> String {
    let mut notes = Vec::new();
    if t.outlier {
        notes.push("temperature outlier");
    }
    if h.outlier {
        notes.push("humidity outlier");
    }
    if correction(t).is_none() && !t.outlier {
        notes.push("too little data");
    }
    notes.join(", ")
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn an_outlier_in_one_quantity_skips_both() {
        let fit = Fit {
            gain: 1.0,
            offset: 0.1,
            gain_fitted: false,
            gain_stderr: 0.0,
            residual_rms: 0.05,
            lag: None,
            outlier: false,
            steady: Duration::from_secs(2 * 3600),
        };
        let outlier = Fit {
            outlier: true,
            ..fit
        };
        let both = |temperature, humidity| {
            let (t, h) = corrections(&SensorReport {
                temperature,
                humidity,
            });
            (t.is_some(), h.is_some())
        };
        assert_eq!(both(fit, fit), (true, true));
        assert_eq!(both(outlier, fit), (false, false));
        assert_eq!(both(fit, outlier), (false, false));
        let short = Fit {
            steady: Duration::from_secs(600),
            ..fit
        };
        assert_eq!(both(fit, short), (true, false));
    }

    #[test]
    fn window_starts_after_last_calibration() {
        let now = UNIX_EPOCH + Duration::from_secs(1_793_000_000);
        let a = Address([1; 6]);
        let b = Address([2; 6]);
        let mut changes = HashMap::new();
        changes.insert(b, now - Duration::from_secs(3600));
        assert_eq!(
            window_start(now - Duration::from_secs(6 * 3600), &[a], &changes),
            now - Duration::from_secs(6 * 3600)
        );
        assert_eq!(
            window_start(now - Duration::from_secs(6 * 3600), &[a, b], &changes),
            now - Duration::from_secs(3600)
        );
    }
}
