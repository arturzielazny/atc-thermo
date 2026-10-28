//! Consensus calibration of co-located temperature and humidity sensors.
//!
//! Sensors placed together measure the same air, so their disagreement is
//! sensor error. [`calibrate`] fits a linear correction `gain · reported + offset`
//! per sensor such that all sensors agree, anchored so the corrections average to
//! zero over the sensors that are not outliers.
//!
//! Temperature and humidity are handled independently, each in these steps:
//!
//! 1. resample every sensor onto a common time grid ([`Options::bin`]);
//! 2. keep only readings taken while both the group and the sensor itself were
//!    steady, so transients after moving the sensors (which differ per case) are ignored;
//! 3. fit each sensor against the median of the others with a robust (Huber)
//!    regression, iterating a few times;
//! 4. use a gain only where the data spans enough range and the gain is
//!    significantly different from 1, otherwise just an offset;
//! 5. flag sensors whose correction is far from the others' as outliers, refit
//!    without them, and anchor the result to the mean of the rest.

mod robust;
mod stats;

use std::fmt;
use std::time::Duration;

use robust::Model;
use stats::{median, percentile, std_dev};

/// One reading of one sensor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reading {
    /// Seconds on any common clock (e.g. Unix time).
    pub at: f64,
    pub temperature: f64,
    pub humidity: f64,
}

/// Tuning knobs; [`Options::default`] suits LYWSD03MMC-class thermometers.
#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    /// Width of a resampling bin.
    pub bin: Duration,
    /// Window around a bin over which readings must be steady for the bin to be used.
    pub steady_window: Duration,
    /// Largest temperature range within the window, for the group median and for each
    /// sensor itself, in °C. Sensors with slower cases lag behind while conditions change,
    /// so only nearly constant periods compare like with like.
    pub max_temperature_change: f64,
    /// Same for humidity, in %RH.
    pub max_humidity_change: f64,
    /// After a big change (see below) readings are ignored for this long, while the
    /// sensors' cases catch up at their different speeds.
    pub settle_time: Duration,
    /// A consensus temperature change of at least this many °C within
    /// [`Options::settle_time`] counts as big, e.g. moving the sensors outside.
    pub big_temperature_change: f64,
    /// Same for humidity, in %RH.
    pub big_humidity_change: f64,
    /// Model case lag only if the temperature drifts by at least this many °C per hour
    /// (10th to 90th percentile of rates), otherwise the lag term is just noise.
    pub min_temperature_drift: f64,
    /// Same for humidity, in %RH per hour.
    pub min_humidity_drift: f64,
    /// Fit a temperature gain only if steady data spans at least this many °C.
    pub min_temperature_span: f64,
    /// Fit a humidity gain only if steady data spans at least this many %RH.
    pub min_humidity_span: f64,
    /// Humidity readings above this are left out (sensor saturation and creep).
    pub max_humidity: f64,
    /// Robust z-score above which a sensor counts as an outlier.
    pub outlier_threshold: f64,
    /// Smallest correction spread assumed when scoring outliers, in °C,
    /// so a group that agrees very well doesn't flag tiny differences.
    pub temperature_outlier_floor: f64,
    /// Same for humidity, in %RH.
    pub humidity_outlier_floor: f64,
    /// Least steady temperature data needed for a result (and per sensor, for its fit
    /// to count as reliable).
    pub min_steady: Duration,
}

impl Options {
    /// Least time readings must span: a bin counts as steady only with half the
    /// steady window on both sides, so the window's ends never count.
    #[must_use]
    pub fn shortest_span(&self) -> Duration {
        self.min_steady + self.steady_window
    }
}

impl Default for Options {
    fn default() -> Self {
        Self {
            // Bins and window are wide because BLE reception is lossy: often only every
            // second or third measurement arrives.
            bin: Duration::from_secs(5 * 60),
            steady_window: Duration::from_secs(30 * 60),
            // Slow drifts like a room cooling in the evening (up to 1.5 °C/h) are fine:
            // a lag difference of a minute or two between cases costs only a few
            // hundredths of a degree, and the robust fit averages it out.
            max_temperature_change: 0.75,
            max_humidity_change: 3.5,
            settle_time: Duration::from_secs(60 * 60),
            big_temperature_change: 2.0,
            big_humidity_change: 10.0,
            min_temperature_drift: 0.3,
            min_humidity_drift: 1.5,
            min_temperature_span: 5.0,
            min_humidity_span: 15.0,
            max_humidity: 95.0,
            outlier_threshold: 3.5,
            temperature_outlier_floor: 0.15,
            humidity_outlier_floor: 1.5,
            min_steady: Duration::from_secs(60 * 60),
        }
    }
}

/// A fitted correction: `corrected = gain · reported + offset`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fit {
    pub gain: f64,
    pub offset: f64,
    /// Whether a gain was estimated; otherwise `gain` is 1.
    pub gain_fitted: bool,
    /// Standard error of the gain estimate (0 if not fitted).
    pub gain_stderr: f64,
    /// RMS difference to the consensus after correction, on steady bins.
    pub residual_rms: f64,
    /// How many seconds the sensor responds later than the group (negative: earlier),
    /// if the data drifted enough to tell. The firmware cannot correct this; it is
    /// modelled only so that drifting periods don't bias gain and offset.
    pub lag: Option<f64>,
    /// Disagreed so much with the others that it was left out of the consensus
    /// and the anchor. Its fit still pulls it to the others.
    pub outlier: bool,
    /// Amount of steady data this fit is based on.
    pub steady: Duration,
}

impl Fit {
    const IDENTITY: Self = Self {
        gain: 1.0,
        offset: 0.0,
        gain_fitted: false,
        gain_stderr: 0.0,
        residual_rms: 0.0,
        lag: None,
        outlier: false,
        steady: Duration::ZERO,
    };

    #[must_use]
    pub fn apply(&self, value: f64) -> f64 {
        self.gain * value + self.offset
    }
}

/// Result for one sensor, in input order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SensorReport {
    pub temperature: Fit,
    pub humidity: Fit,
}

/// Agreement between sensors before and after correction, on steady bins.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spread {
    /// Mean across bins of the standard deviation across non-outlier sensors.
    pub before: f64,
    pub after: f64,
}

/// Summary for temperature or humidity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QuantityReport {
    /// Time with at least two steady sensors.
    pub steady: Duration,
    /// 5th to 95th percentile of the steady consensus, `(low, high)`.
    pub range: (f64, f64),
    pub spread: Spread,
}

/// A stretch of steady conditions, e.g. "inside" or "outside".
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Phase {
    pub start: f64,
    pub end: f64,
    pub steady: Duration,
    pub temperature: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub sensors: Vec<SensorReport>,
    pub temperature: QuantityReport,
    pub humidity: QuantityReport,
    /// Steady temperature stretches, split where the level changes.
    pub phases: Vec<Phase>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// Consensus needs at least two sensors with data.
    TooFewSensors(usize),
    /// The readings span less time than [`Options::shortest_span`], so they cannot hold
    /// enough steady data however calm they are.
    TooShort { span: Duration, needed: Duration },
    /// Not enough steady, co-observed data yet.
    NotEnoughSteadyData { steady: Duration, needed: Duration },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooFewSensors(n) => write!(f, "need at least 2 sensors with data, have {n}"),
            Self::TooShort { span, needed } => write!(
                f,
                "data covers only {} minutes, a fit needs at least {}",
                span.as_secs() / 60,
                needed.as_secs() / 60
            ),
            Self::NotEnoughSteadyData { steady, needed } => write!(
                f,
                "only {} steady minutes, need {}; keep the sensors together and undisturbed longer",
                steady.as_secs() / 60,
                needed.as_secs() / 60
            ),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quantity {
    Temperature,
    Humidity,
}

impl Quantity {
    fn of(self, reading: &Reading) -> f64 {
        match self {
            Self::Temperature => reading.temperature,
            Self::Humidity => reading.humidity,
        }
    }

    fn max_change(self, options: &Options) -> f64 {
        match self {
            Self::Temperature => options.max_temperature_change,
            Self::Humidity => options.max_humidity_change,
        }
    }

    fn min_drift_per_second(self, options: &Options) -> f64 {
        match self {
            Self::Temperature => options.min_temperature_drift / 3600.0,
            Self::Humidity => options.min_humidity_drift / 3600.0,
        }
    }

    fn big_change(self, options: &Options) -> f64 {
        match self {
            Self::Temperature => options.big_temperature_change,
            Self::Humidity => options.big_humidity_change,
        }
    }

    fn min_span(self, options: &Options) -> f64 {
        match self {
            Self::Temperature => options.min_temperature_span,
            Self::Humidity => options.min_humidity_span,
        }
    }

    fn outlier_floor(self, options: &Options) -> f64 {
        match self {
            Self::Temperature => options.temperature_outlier_floor,
            Self::Humidity => options.humidity_outlier_floor,
        }
    }
}

/// Per-bin mean of one quantity: `grid[bin][sensor]`.
type Grid = Vec<Vec<Option<f64>>>;

/// Fits corrections for co-located sensors; `sensors[i]` are sensor i's readings in any order.
///
/// # Errors
///
/// Fails with fewer than two sensors with readings, readings spanning less than
/// [`Options::shortest_span`], or without [`Options::min_steady`] of steady
/// temperature data.
pub fn calibrate(sensors: &[&[Reading]], options: &Options) -> Result<Report, Error> {
    let with_data = sensors.iter().filter(|s| !s.is_empty()).count();
    if with_data < 2 {
        return Err(Error::TooFewSensors(with_data));
    }
    let bin = options.bin.as_secs_f64();
    let (t0, t1) = sensors
        .iter()
        .flat_map(|s| s.iter().map(|r| r.at))
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), at| {
            (lo.min(at), hi.max(at))
        });
    let span = Duration::from_secs_f64(t1 - t0);
    if span < options.shortest_span() {
        return Err(Error::TooShort {
            span,
            needed: options.shortest_span(),
        });
    }

    let raw_t = resample(sensors, t0, bin, Quantity::Temperature);
    let temperature = settled(&raw_t, Quantity::Temperature, options);
    let temperature_rates = rates(&raw_t, bin);
    let steady = options.bin * u32::try_from(steady_bin_count(&temperature)).unwrap_or(u32::MAX);
    if steady < options.min_steady {
        return Err(Error::NotEnoughSteadyData {
            steady,
            needed: options.min_steady,
        });
    }
    let raw_h = resample(sensors, t0, bin, Quantity::Humidity);
    let humidity_rates = rates(&raw_h, bin);
    let mut humidity = settled(&raw_h, Quantity::Humidity, options);
    for cell in humidity.iter_mut().flatten() {
        if cell.is_some_and(|h| h > options.max_humidity) {
            *cell = None;
        }
    }

    let (t_fits, t_report) = fit_quantity(
        &temperature,
        &temperature_rates,
        Quantity::Temperature,
        options,
    );
    let humidity_steady =
        options.bin * u32::try_from(steady_bin_count(&humidity)).unwrap_or(u32::MAX);
    let (h_fits, h_report) = if humidity_steady >= options.min_steady {
        fit_quantity(&humidity, &humidity_rates, Quantity::Humidity, options)
    } else {
        let identity = vec![Fit::IDENTITY; sensors.len()];
        let report = quantity_report(&humidity, &identity, options.bin);
        (identity, report)
    };
    let good_t: Vec<bool> = t_fits.iter().map(|f| !f.outlier).collect();
    Ok(Report {
        sensors: t_fits
            .into_iter()
            .zip(h_fits)
            .map(|(temperature, humidity)| SensorReport {
                temperature,
                humidity,
            })
            .collect(),
        temperature: t_report,
        humidity: h_report,
        phases: phases(&temperature, &good_t, t0, bin),
    })
}

fn fit_quantity(
    grid: &Grid,
    rates: &Grid,
    quantity: Quantity,
    options: &Options,
) -> (Vec<Fit>, QuantityReport) {
    let sensors = grid.first().map_or(0, Vec::len);
    let mut good = vec![true; sensors];
    let mut fits = fit_all(grid, rates, &good, quantity, options);
    if sensors >= 4 {
        let outliers = find_outliers(grid, &fits, quantity, options);
        if outliers.iter().any(|&o| o) {
            good = outliers.iter().map(|o| !o).collect();
            fits = fit_all(grid, rates, &good, quantity, options);
        }
    }
    let corrected = corrected_grid(grid, &fits);
    for (i, fit) in fits.iter_mut().enumerate() {
        let p = pairs(grid, rates, &corrected, &good, i);
        fit.outlier = !good[i];
        fit.steady = options.bin * u32::try_from(p.xs.len()).unwrap_or(u32::MAX);
    }
    // Sensors with too little data neither define nor shift the group average.
    let anchors: Vec<bool> = fits
        .iter()
        .map(|f| !f.outlier && f.steady >= options.min_steady)
        .collect();
    let anchors = if anchors.iter().any(|&a| a) {
        anchors
    } else {
        good.clone()
    };
    anchor(&mut fits, &anchors);
    let corrected = corrected_grid(grid, &fits);
    for (i, fit) in fits.iter_mut().enumerate() {
        let p = pairs(grid, rates, &corrected, &good, i);
        fit.residual_rms = residual_rms(&p, fit);
    }
    let report = quantity_report(grid, &fits, options.bin);
    (fits, report)
}

fn quantity_report(grid: &Grid, fits: &[Fit], bin: Duration) -> QuantityReport {
    let good: Vec<bool> = fits.iter().map(|f| !f.outlier).collect();
    let consensus: Vec<f64> = grid
        .iter()
        .filter_map(|row| consensus(row, &good))
        .collect();
    QuantityReport {
        steady: bin * u32::try_from(steady_bin_count(grid)).unwrap_or(u32::MAX),
        range: (percentile(&consensus, 0.05), percentile(&consensus, 0.95)),
        spread: spread(grid, &good, fits),
    }
}

/// Averages one quantity into fixed-width bins starting at `t0`.
fn resample(sensors: &[&[Reading]], t0: f64, bin: f64, quantity: Quantity) -> Grid {
    let t1 = sensors
        .iter()
        .flat_map(|s| s.iter().map(|r| r.at))
        .fold(f64::NEG_INFINITY, f64::max);
    let bins = bin_index(t1 - t0, bin) + 1;
    let mut sums = vec![vec![(0.0, 0_u32); sensors.len()]; bins];
    for (i, readings) in sensors.iter().enumerate() {
        for r in *readings {
            let cell = &mut sums[bin_index(r.at - t0, bin)][i];
            cell.0 += quantity.of(r);
            cell.1 += 1;
        }
    }
    sums.into_iter()
        .map(|row| {
            row.into_iter()
                .map(|(sum, n)| (n > 0).then(|| sum / f64::from(n)))
                .collect()
        })
        .collect()
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "offsets are non-negative and bin counts small"
)]
fn bin_index(offset: f64, bin: f64) -> usize {
    (offset / bin).floor().max(0.0) as usize
}

fn steady_bin_count(grid: &Grid) -> usize {
    grid.iter()
        .filter(|row| row.iter().flatten().count() >= 2)
        .count()
}

/// Median over the included sensors present in a bin, if at least half of them are.
fn consensus(row: &[Option<f64>], include: &[bool]) -> Option<f64> {
    let values: Vec<f64> = row
        .iter()
        .zip(include)
        .filter_map(|(cell, &inc)| cell.filter(|_| inc))
        .collect();
    let needed = include.iter().filter(|&&i| i).count().div_ceil(2).max(2);
    (values.len() >= needed).then(|| median(&values))
}

/// Keeps only readings taken while both the group median and the sensor itself stayed
/// within [`Quantity::max_change`] over the steady window, and not shortly after a
/// big change of the group median.
fn settled(grid: &Grid, quantity: Quantity, options: &Options) -> Grid {
    let bin = options.bin.as_secs_f64();
    let half = bin_index(options.steady_window.as_secs_f64() / 2.0, bin);
    let settle = bin_index(options.settle_time.as_secs_f64(), bin);
    let limit = quantity.max_change(options);
    let sensors = grid.first().map_or(0, Vec::len);
    let all = vec![true; sensors];
    let group: Vec<Option<f64>> = grid.iter().map(|row| consensus(row, &all)).collect();
    let settling: Vec<bool> = (0..grid.len())
        .map(|b| range(&group[b.saturating_sub(settle)..=b]) >= quantity.big_change(options))
        .collect();
    let calm_cells: Vec<Vec<bool>> = (0..sensors)
        .map(|i| {
            let own: Vec<Option<f64>> = grid.iter().map(|row| row[i]).collect();
            (0..grid.len())
                .map(|b| !settling[b] && calm(&group, b, half, limit) && calm(&own, b, half, limit))
                .collect()
        })
        .collect();
    let masked: Grid = grid
        .iter()
        .enumerate()
        .map(|(b, row)| {
            row.iter()
                .enumerate()
                .map(|(i, cell)| cell.filter(|_| calm_cells[i][b]))
                .collect()
        })
        .collect();
    // A bin needs at least two sensors to compare anything.
    masked
        .into_iter()
        .map(|row| {
            if row.iter().flatten().count() >= 2 {
                row
            } else {
                vec![None; row.len()]
            }
        })
        .collect()
}

/// Whether `series` around bin `b` is mostly observed and varies by at most `limit`.
fn calm(series: &[Option<f64>], b: usize, half: usize, limit: f64) -> bool {
    if b < half || b + half >= series.len() {
        return false;
    }
    let window = &series[b - half..=b + half];
    let present = window.iter().flatten().count();
    present * 2 >= window.len() && range(window) <= limit
}

/// Largest minus smallest observed value; 0 if none.
fn range(values: &[Option<f64>]) -> f64 {
    let (lo, hi) = values
        .iter()
        .flatten()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &v| {
            (lo.min(v), hi.max(v))
        });
    if lo <= hi { hi - lo } else { 0.0 }
}

/// Iteratively fits every sensor against the median of the other included sensors.
fn fit_all(
    grid: &Grid,
    rates: &Grid,
    include: &[bool],
    quantity: Quantity,
    options: &Options,
) -> Vec<Fit> {
    const ITERATIONS: usize = 4;
    let mut fits = vec![Fit::IDENTITY; include.len()];
    for _ in 0..ITERATIONS {
        let corrected = corrected_grid(grid, &fits);
        fits = (0..include.len())
            .map(|i| fit_sensor(grid, rates, &corrected, include, i, quantity, options))
            .collect();
    }
    fits
}

/// Rate of change per second of every sensor's raw series, from neighbouring bins.
fn rates(raw: &Grid, bin: f64) -> Grid {
    let sensors = raw.first().map_or(0, Vec::len);
    (0..raw.len())
        .map(|b| {
            (0..sensors)
                .map(|i| {
                    let at = |b: Option<usize>| b.and_then(|b| raw.get(b)).and_then(|row| row[i]);
                    let (before, here, after) = (at(b.checked_sub(1)), raw[b][i], at(Some(b + 1)));
                    match (before, here, after) {
                        (Some(x0), _, Some(x2)) => Some((x2 - x0) / (2.0 * bin)),
                        (Some(x0), Some(x1), None) => Some((x1 - x0) / bin),
                        (None, Some(x1), Some(x2)) => Some((x2 - x1) / bin),
                        _ => None,
                    }
                })
                .collect()
        })
        .collect()
}

fn corrected_grid(grid: &Grid, fits: &[Fit]) -> Grid {
    grid.iter()
        .map(|row| {
            row.iter()
                .zip(fits)
                .map(|(cell, fit)| cell.map(|v| fit.apply(v)))
                .collect()
        })
        .collect()
}

/// Readings of one sensor paired with the median of the other included sensors.
struct Pairs {
    xs: Vec<f64>,
    ys: Vec<f64>,
    /// The sensor's own rate of change at each pair, per second.
    rates: Vec<Option<f64>>,
}

fn pairs(grid: &Grid, rates: &Grid, reference: &Grid, include: &[bool], i: usize) -> Pairs {
    let mut others = include.to_vec();
    others[i] = false;
    if !others.iter().any(|&o| o) {
        // An excluded sensor in a group of two: compare with everyone else.
        others = vec![true; include.len()];
        others[i] = false;
    }
    let mut p = Pairs {
        xs: Vec::new(),
        ys: Vec::new(),
        rates: Vec::new(),
    };
    for ((row, rate_row), ref_row) in grid.iter().zip(rates).zip(reference) {
        let Some(x) = row[i] else { continue };
        let values: Vec<f64> = ref_row
            .iter()
            .zip(&others)
            .filter_map(|(c, &o)| c.filter(|_| o))
            .collect();
        if values.is_empty() {
            continue;
        }
        p.xs.push(x);
        p.ys.push(median(&values));
        p.rates.push(rate_row[i]);
    }
    p
}

fn fit_sensor(
    grid: &Grid,
    rates: &Grid,
    corrected: &Grid,
    include: &[bool],
    i: usize,
    quantity: Quantity,
    options: &Options,
) -> Fit {
    let mut p = pairs(grid, rates, corrected, include, i);
    if p.xs.is_empty() {
        return Fit::IDENTITY;
    }
    // Model lag only with enough drift; then use just the pairs with a known rate.
    let known: Vec<f64> = p.rates.iter().flatten().copied().collect();
    let with_lag = known.len() * 5 >= p.xs.len() * 4
        && percentile(&known, 0.9) - percentile(&known, 0.1)
            >= quantity.min_drift_per_second(options);
    if with_lag {
        let keep: Vec<bool> = p.rates.iter().map(Option::is_some).collect();
        let filter = |v: &[f64]| -> Vec<f64> {
            v.iter()
                .zip(&keep)
                .filter(|&(_, &k)| k)
                .map(|(x, _)| *x)
                .collect()
        };
        p = Pairs {
            xs: filter(&p.xs),
            ys: filter(&p.ys),
            rates: p.rates.into_iter().flatten().map(Some).collect(),
        };
    }
    let rate = |j: usize| p.rates[j].unwrap_or(0.0);

    // y − x ≈ offset (+ k·rate)
    let offset_only = {
        let rows: Vec<Vec<f64>> = (0..p.xs.len())
            .map(|j| {
                if with_lag {
                    vec![1.0, rate(j)]
                } else {
                    vec![1.0]
                }
            })
            .collect();
        let diffs: Vec<f64> = p.ys.iter().zip(&p.xs).map(|(y, x)| y - x).collect();
        match robust::fit(&rows, &diffs) {
            Some(Model { coef, .. }) => Fit {
                offset: coef[0],
                lag: coef.get(1).copied(),
                ..Fit::IDENTITY
            },
            None => Fit {
                offset: median(&diffs),
                ..Fit::IDENTITY
            },
        }
    };
    if percentile(&p.xs, 0.95) - percentile(&p.xs, 0.05) < quantity.min_span(options) {
        return offset_only;
    }
    // y ≈ gain·x + offset (+ k·rate), with lag = k / gain
    let rows: Vec<Vec<f64>> = (0..p.xs.len())
        .map(|j| {
            if with_lag {
                vec![p.xs[j], 1.0, rate(j)]
            } else {
                vec![p.xs[j], 1.0]
            }
        })
        .collect();
    let Some(Model { coef, stderr }) = robust::fit(&rows, &p.ys) else {
        return offset_only;
    };
    let (gain, gain_stderr) = (coef[0], stderr[0]);
    // A gain indistinguishable from 1 only adds noise.
    if (gain - 1.0).abs() < 2.0 * gain_stderr {
        return offset_only;
    }
    Fit {
        gain,
        offset: coef[1],
        gain_fitted: true,
        gain_stderr,
        lag: coef.get(2).map(|k| k / gain),
        ..Fit::IDENTITY
    }
}

/// Flags sensors whose correction at the typical operating point is far from the others'.
fn find_outliers(grid: &Grid, fits: &[Fit], quantity: Quantity, options: &Options) -> Vec<bool> {
    let all = vec![true; fits.len()];
    let typical = median(
        &grid
            .iter()
            .filter_map(|row| consensus(row, &all))
            .collect::<Vec<_>>(),
    );
    let corrections: Vec<f64> = fits.iter().map(|f| f.apply(typical) - typical).collect();
    let center = median(&corrections);
    let deviations: Vec<f64> = corrections.iter().map(|c| (c - center).abs()).collect();
    let scale = (1.4826 * median(&deviations)).max(quantity.outlier_floor(options));
    deviations
        .iter()
        .map(|d| d / scale > options.outlier_threshold)
        .collect()
}

/// Rescales all fits so the included sensors' gains average 1 and offsets average 0.
fn anchor(fits: &mut [Fit], include: &[bool]) {
    let chosen: Vec<&Fit> = fits
        .iter()
        .zip(include)
        .filter(|&(_, &inc)| inc)
        .map(|(f, _)| f)
        .collect();
    let count = chosen.len() as f64;
    let mean_gain = chosen.iter().map(|f| f.gain).sum::<f64>() / count;
    let mean_offset = chosen.iter().map(|f| f.offset).sum::<f64>() / count;
    let alpha = 1.0 / mean_gain;
    for fit in fits.iter_mut() {
        fit.gain *= alpha;
        fit.offset = alpha * (fit.offset - mean_offset);
        fit.gain_stderr *= alpha;
    }
}

/// RMS of what the fit (including lag, where modelled) leaves unexplained.
fn residual_rms(p: &Pairs, fit: &Fit) -> f64 {
    if p.xs.is_empty() {
        return 0.0;
    }
    let lag_term = |j: usize| match (fit.lag, p.rates[j]) {
        (Some(lag), Some(rate)) => fit.gain * lag * rate,
        _ => 0.0,
    };
    let sum: f64 = (0..p.xs.len())
        .map(|j| (fit.apply(p.xs[j]) + lag_term(j) - p.ys[j]).powi(2))
        .sum();
    (sum / p.xs.len() as f64).sqrt()
}

fn spread(grid: &Grid, include: &[bool], fits: &[Fit]) -> Spread {
    let mut before = Vec::new();
    let mut after = Vec::new();
    for row in grid {
        let present: Vec<(f64, &Fit)> = row
            .iter()
            .zip(include)
            .zip(fits)
            .filter_map(|((cell, &inc), fit)| cell.filter(|_| inc).map(|v| (v, fit)))
            .collect();
        if present.len() < 2 {
            continue;
        }
        let raw: Vec<f64> = present.iter().map(|(v, _)| *v).collect();
        let corrected: Vec<f64> = present.iter().map(|(v, f)| f.apply(*v)).collect();
        before.push(std_dev(&raw));
        after.push(std_dev(&corrected));
    }
    let mean = |v: &[f64]| {
        if v.is_empty() {
            0.0
        } else {
            v.iter().sum::<f64>() / v.len() as f64
        }
    };
    Spread {
        before: mean(&before),
        after: mean(&after),
    }
}

/// Groups steady temperature bins into phases of similar level.
fn phases(grid: &Grid, include: &[bool], t0: f64, bin: f64) -> Vec<Phase> {
    const SAME_PHASE: f64 = 2.0;
    let mut phases: Vec<(usize, usize, Vec<f64>)> = Vec::new();
    for (b, row) in grid.iter().enumerate() {
        let Some(t) = consensus(row, include) else {
            continue;
        };
        match phases.last_mut() {
            Some((_, end, ts)) if (median(ts) - t).abs() < SAME_PHASE => {
                *end = b;
                ts.push(t);
            }
            _ => phases.push((b, b, vec![t])),
        }
    }
    phases
        .into_iter()
        .map(|(start, end, ts)| Phase {
            start: t0 + start as f64 * bin,
            end: t0 + (end + 1) as f64 * bin,
            steady: Duration::from_secs_f64(bin * ts.len() as f64),
            temperature: median(&ts),
        })
        .collect()
}

#[cfg(test)]
mod tests;
