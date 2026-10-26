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

use robust::{Line, fit_line, fit_offset};
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
    /// Fewest steady temperature bins needed for a result.
    pub min_steady_bins: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            bin: Duration::from_secs(60),
            steady_window: Duration::from_secs(15 * 60),
            max_temperature_change: 0.1,
            max_humidity_change: 0.5,
            min_temperature_span: 5.0,
            min_humidity_span: 15.0,
            max_humidity: 95.0,
            outlier_threshold: 3.5,
            temperature_outlier_floor: 0.15,
            humidity_outlier_floor: 1.5,
            min_steady_bins: 30,
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
    /// Disagreed so much with the others that it was left out of the consensus
    /// and the anchor. Its fit still pulls it to the others.
    pub outlier: bool,
    /// Steady bins this fit is based on.
    pub bins: usize,
}

impl Fit {
    const IDENTITY: Self = Self {
        gain: 1.0,
        offset: 0.0,
        gain_fitted: false,
        gain_stderr: 0.0,
        residual_rms: 0.0,
        outlier: false,
        bins: 0,
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
    /// Bins with at least two steady sensors.
    pub steady_bins: usize,
    /// 5th to 95th percentile of the steady consensus, `(low, high)`.
    pub range: (f64, f64),
    pub spread: Spread,
}

/// A stretch of steady conditions, e.g. "inside" or "outside".
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Phase {
    pub start: f64,
    pub end: f64,
    pub steady_bins: usize,
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
    /// Not enough steady, co-observed data yet.
    NotEnoughSteadyData { steady_bins: usize, needed: usize },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooFewSensors(n) => write!(f, "need at least 2 sensors with data, have {n}"),
            Self::NotEnoughSteadyData {
                steady_bins,
                needed,
            } => write!(
                f,
                "only {steady_bins} steady minutes, need {needed}; keep the sensors together and undisturbed longer"
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
/// Fails with fewer than two sensors with readings, or without
/// [`Options::min_steady_bins`] steady temperature bins.
pub fn calibrate(sensors: &[&[Reading]], options: &Options) -> Result<Report, Error> {
    let with_data = sensors.iter().filter(|s| !s.is_empty()).count();
    if with_data < 2 {
        return Err(Error::TooFewSensors(with_data));
    }
    let bin = options.bin.as_secs_f64();
    let t0 = sensors
        .iter()
        .flat_map(|s| s.iter().map(|r| r.at))
        .fold(f64::INFINITY, f64::min);

    let raw_t = resample(sensors, t0, bin, Quantity::Temperature);
    let temperature = settled(&raw_t, Quantity::Temperature, options);
    let temperature_bins = steady_bin_count(&temperature);
    if temperature_bins < options.min_steady_bins {
        return Err(Error::NotEnoughSteadyData {
            steady_bins: temperature_bins,
            needed: options.min_steady_bins,
        });
    }
    let mut humidity = settled(
        &resample(sensors, t0, bin, Quantity::Humidity),
        Quantity::Humidity,
        options,
    );
    for cell in humidity.iter_mut().flatten() {
        if cell.is_some_and(|h| h > options.max_humidity) {
            *cell = None;
        }
    }

    let (t_fits, t_report) = fit_quantity(&temperature, Quantity::Temperature, options);
    let (h_fits, h_report) = if steady_bin_count(&humidity) >= options.min_steady_bins {
        fit_quantity(&humidity, Quantity::Humidity, options)
    } else {
        let identity = vec![Fit::IDENTITY; sensors.len()];
        let report = quantity_report(&humidity, &identity);
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

fn fit_quantity(grid: &Grid, quantity: Quantity, options: &Options) -> (Vec<Fit>, QuantityReport) {
    let sensors = grid.first().map_or(0, Vec::len);
    let mut good = vec![true; sensors];
    let mut fits = fit_all(grid, &good, quantity, options);
    if sensors >= 4 {
        let outliers = find_outliers(grid, &fits, quantity, options);
        if outliers.iter().any(|&o| o) {
            good = outliers.iter().map(|o| !o).collect();
            fits = fit_all(grid, &good, quantity, options);
        }
    }
    anchor(&mut fits, &good);
    let corrected = corrected_grid(grid, &fits);
    for (i, fit) in fits.iter_mut().enumerate() {
        let (xs, ys) = pairs(grid, &corrected, &good, i);
        fit.residual_rms = residual_rms(&xs, &ys, fit);
        fit.outlier = !good[i];
        fit.bins = xs.len();
    }
    let report = quantity_report(grid, &fits);
    (fits, report)
}

fn quantity_report(grid: &Grid, fits: &[Fit]) -> QuantityReport {
    let good: Vec<bool> = fits.iter().map(|f| !f.outlier).collect();
    let consensus: Vec<f64> = grid
        .iter()
        .filter_map(|row| consensus(row, &good))
        .collect();
    QuantityReport {
        steady_bins: steady_bin_count(grid),
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
/// within [`Quantity::max_change`] over the steady window.
fn settled(grid: &Grid, quantity: Quantity, options: &Options) -> Grid {
    let half = bin_index(
        options.steady_window.as_secs_f64() / 2.0,
        options.bin.as_secs_f64(),
    );
    let limit = quantity.max_change(options);
    let sensors = grid.first().map_or(0, Vec::len);
    let all = vec![true; sensors];
    let group: Vec<Option<f64>> = grid.iter().map(|row| consensus(row, &all)).collect();
    let calm_cells: Vec<Vec<bool>> = (0..sensors)
        .map(|i| {
            let own: Vec<Option<f64>> = grid.iter().map(|row| row[i]).collect();
            (0..grid.len())
                .map(|b| calm(&group, b, half, limit) && calm(&own, b, half, limit))
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
    let present: Vec<f64> = window.iter().flatten().copied().collect();
    if present.len() * 4 < window.len() * 3 {
        return false;
    }
    let (lo, hi) = present
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &v| {
            (lo.min(v), hi.max(v))
        });
    hi - lo <= limit
}

/// Iteratively fits every sensor against the median of the other included sensors.
fn fit_all(grid: &Grid, include: &[bool], quantity: Quantity, options: &Options) -> Vec<Fit> {
    const ITERATIONS: usize = 4;
    let mut fits = vec![Fit::IDENTITY; include.len()];
    for _ in 0..ITERATIONS {
        let corrected = corrected_grid(grid, &fits);
        fits = (0..include.len())
            .map(|i| fit_sensor(grid, &corrected, include, i, quantity, options))
            .collect();
    }
    fits
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

/// Pairs of (raw reading of sensor `i`, median of the other included sensors in `reference`).
fn pairs(grid: &Grid, reference: &Grid, include: &[bool], i: usize) -> (Vec<f64>, Vec<f64>) {
    let mut others = include.to_vec();
    others[i] = false;
    if !others.iter().any(|&o| o) {
        // An excluded sensor in a group of two: compare with everyone else.
        others = vec![true; include.len()];
        others[i] = false;
    }
    let mut xs = Vec::new();
    let mut ys = Vec::new();
    for (row, ref_row) in grid.iter().zip(reference) {
        let Some(x) = row[i] else { continue };
        let values: Vec<f64> = ref_row
            .iter()
            .zip(&others)
            .filter_map(|(c, &o)| c.filter(|_| o))
            .collect();
        if values.is_empty() {
            continue;
        }
        xs.push(x);
        ys.push(median(&values));
    }
    (xs, ys)
}

fn fit_sensor(
    grid: &Grid,
    corrected: &Grid,
    include: &[bool],
    i: usize,
    quantity: Quantity,
    options: &Options,
) -> Fit {
    let (xs, ys) = pairs(grid, corrected, include, i);
    if xs.is_empty() {
        return Fit::IDENTITY;
    }
    let offset_only = Fit {
        offset: fit_offset(&xs, &ys),
        ..Fit::IDENTITY
    };
    if percentile(&xs, 0.95) - percentile(&xs, 0.05) < quantity.min_span(options) {
        return offset_only;
    }
    let Line {
        gain,
        offset,
        gain_stderr,
    } = fit_line(&xs, &ys);
    // A gain indistinguishable from 1 only adds noise.
    if (gain - 1.0).abs() < 2.0 * gain_stderr {
        return offset_only;
    }
    Fit {
        gain,
        offset,
        gain_fitted: true,
        gain_stderr,
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

fn residual_rms(xs: &[f64], ys: &[f64], fit: &Fit) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let sum: f64 = xs
        .iter()
        .zip(ys)
        .map(|(x, y)| (fit.apply(*x) - y).powi(2))
        .sum();
    (sum / xs.len() as f64).sqrt()
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
            steady_bins: ts.len(),
            temperature: median(&ts),
        })
        .collect()
}

#[cfg(test)]
mod tests;
