use std::f64::consts::TAU;

use super::*;

/// Deterministic xorshift generator so tests are reproducible without dependencies.
struct Rng(u64);

impl Rng {
    fn uniform(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1_u64 << 53) as f64
    }

    /// Standard normal via Box–Muller.
    fn normal(&mut self) -> f64 {
        let u = self.uniform().max(1e-12);
        (-2.0 * u.ln()).sqrt() * (TAU * self.uniform()).cos()
    }
}

/// How a simulated sensor misreports: `reported = gain · actual + offset`.
#[derive(Clone, Copy)]
struct Sensor {
    temperature: (f64, f64),
    humidity: (f64, f64),
    /// Thermal time constant of the case, in seconds.
    lag: f64,
}

const HOUR: f64 = 3600.0;

/// Air conditions over time: an indoor day, optionally `outside` for a while, then back in.
fn air(t: f64, outside: Option<(f64, f64)>) -> (f64, f64) {
    let indoor = (
        21.0 + 0.4 * (TAU * t / (3.0 * HOUR)).sin(),
        55.0 + 2.0 * (TAU * t / (4.0 * HOUR)).sin(),
    );
    match outside {
        Some((from, to)) if t >= from && t < to => (8.0, 85.0),
        _ => indoor,
    }
}

fn simulate(
    sensors: &[Sensor],
    hours: f64,
    outside: Option<(f64, f64)>,
    seed: u64,
) -> Vec<Vec<Reading>> {
    const STEP: f64 = 10.0;
    let mut rng = Rng(seed);
    sensors
        .iter()
        .map(|s| {
            let start = rng.uniform() * STEP;
            let (mut t_case, mut h_case) = air(0.0, outside);
            let mut readings = Vec::new();
            let mut t = start;
            while t < hours * HOUR {
                let (ta, ha) = air(t, outside);
                let k = STEP / s.lag;
                t_case += (ta - t_case) * k;
                h_case += (ha - h_case) * k;
                readings.push(Reading {
                    at: 1_793_000_000.0 + t,
                    temperature: s.temperature.0 * t_case + s.temperature.1 + 0.02 * rng.normal(),
                    humidity: s.humidity.0 * h_case + s.humidity.1 + 0.15 * rng.normal(),
                });
                t += STEP;
            }
            readings
        })
        .collect()
}

fn sensors() -> Vec<Sensor> {
    vec![
        Sensor {
            temperature: (1.000, 0.20),
            humidity: (1.00, 2.0),
            lag: 300.0,
        },
        Sensor {
            temperature: (1.015, -0.25),
            humidity: (0.96, -1.5),
            lag: 400.0,
        },
        Sensor {
            temperature: (0.990, 0.05),
            humidity: (1.04, 0.5),
            lag: 540.0,
        },
        Sensor {
            temperature: (1.005, -0.10),
            humidity: (1.02, -3.0),
            lag: 360.0,
        },
        Sensor {
            temperature: (0.995, 0.30),
            humidity: (0.98, 1.0),
            lag: 480.0,
        },
    ]
}

fn run(data: &[Vec<Reading>]) -> Result<Report, Error> {
    let slices: Vec<&[Reading]> = data.iter().map(Vec::as_slice).collect();
    calibrate(&slices, &Options::default())
}

#[test]
fn three_phase_session_recovers_gains_and_offsets() {
    let truth = sensors();
    let data = simulate(&truth, 14.0, Some((6.0 * HOUR, 10.0 * HOUR)), 7);
    let report = run(&data).expect("enough data");

    let (t, h) = (report.temperature.spread, report.humidity.spread);
    assert!(t.before > 0.15 && t.after < 0.03, "temperature {t:?}");
    assert!(h.before > 1.5 && h.after < 0.3, "humidity {h:?}");
    assert_eq!(report.phases.len(), 3, "{:?}", report.phases);
    assert!(
        report
            .sensors
            .iter()
            .all(|s| !s.temperature.outlier && !s.humidity.outlier)
    );
    assert!(report.temperature.range.1 - report.temperature.range.0 > 10.0);

    // Gains are identifiable up to a common factor: fitted · true is the same for all.
    // A gain equal to the group's average is correctly left unfitted (1.0).
    let check = |name: &str, fitted: Vec<Fit>, true_gains: Vec<f64>| {
        assert!(
            fitted.iter().filter(|f| f.gain_fitted).count() >= 3,
            "{name} {fitted:?}"
        );
        let products: Vec<f64> = fitted
            .iter()
            .zip(&true_gains)
            .map(|(f, g)| f.gain * g)
            .collect();
        let max = products.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let min = products.iter().copied().fold(f64::INFINITY, f64::min);
        assert!(max - min < 0.005, "{name} gain products {products:?}");
    };
    check(
        "temperature",
        report.sensors.iter().map(|s| s.temperature).collect(),
        truth.iter().map(|s| s.temperature.0).collect(),
    );
    check(
        "humidity",
        report.sensors.iter().map(|s| s.humidity).collect(),
        truth.iter().map(|s| s.humidity.0).collect(),
    );

    // Anchored to the average: gains average 1, offsets 0.
    let mean = |f: fn(&SensorReport) -> f64| report.sensors.iter().map(f).sum::<f64>() / 5.0;
    assert!((mean(|s| s.temperature.gain) - 1.0).abs() < 1e-9);
    assert!(mean(|s| s.temperature.offset).abs() < 1e-9);
}

#[test]
fn indoor_only_fits_offsets() {
    let data = simulate(&sensors(), 6.0, None, 11);
    let report = run(&data).expect("enough data");
    assert_eq!(report.phases.len(), 1);
    for s in &report.sensors {
        assert!(!s.temperature.gain_fitted, "{s:?}");
        assert!(!s.humidity.gain_fitted, "{s:?}");
    }
    let (t, h) = (report.temperature.spread, report.humidity.spread);
    assert!(t.after < 0.04, "temperature {t:?}");
    assert!(h.after < 0.4, "humidity {h:?}");
}

#[test]
fn outlier_is_flagged_and_excluded_from_anchor() {
    let mut truth = sensors();
    truth.push(Sensor {
        temperature: (1.0, 2.5),
        humidity: (1.0, 0.0),
        lag: 400.0,
    });
    let data = simulate(&truth, 6.0, None, 3);
    let report = run(&data).expect("enough data");
    let flags: Vec<bool> = report
        .sensors
        .iter()
        .map(|s| s.temperature.outlier)
        .collect();
    assert_eq!(flags, [false, false, false, false, false, true]);
    assert!(report.sensors.iter().all(|s| !s.humidity.outlier));

    // The good sensors' offsets average zero, so the outlier is pulled to them.
    let good_mean = truth[..5].iter().map(|s| s.temperature.1).sum::<f64>() / 5.0;
    let outlier = report.sensors[5].temperature;
    assert!(
        (outlier.offset - (good_mean - 2.5)).abs() < 0.05,
        "{outlier:?}"
    );
}

#[test]
fn transients_are_not_steady() {
    let moves = [6.0 * HOUR, 10.0 * HOUR];
    let data = simulate(&sensors(), 14.0, Some((moves[0], moves[1])), 5);
    let report = run(&data).expect("enough data");
    let start = data[0][0].at - data[0][0].at % 60.0;
    // Phases after each move start only once every case has settled (lags are 5-9 min).
    for (phase, moved) in report.phases[1..].iter().zip(moves) {
        let settle_minutes = (phase.start - (start + moved)) / 60.0;
        assert!(
            settle_minutes > 20.0,
            "{phase:?} settled after {settle_minutes} min"
        );
    }
}

#[test]
fn errors() {
    let data = simulate(&sensors(), 0.3, None, 1);
    assert!(matches!(run(&data), Err(Error::NotEnoughSteadyData { .. })));
    let one = simulate(&sensors()[..1], 6.0, None, 1);
    assert_eq!(run(&one), Err(Error::TooFewSensors(1)));
}
