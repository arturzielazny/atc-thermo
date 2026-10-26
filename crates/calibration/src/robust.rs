//! Huber M-estimators: least squares that down-weights outlying points.

use crate::stats::median;

/// Huber tuning constant: 95 % efficiency for Gaussian noise.
const HUBER_K: f64 = 1.345;
const ITERATIONS: usize = 30;

/// `y ≈ gain · x + offset`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Line {
    pub gain: f64,
    pub offset: f64,
    pub gain_stderr: f64,
}

/// Robust estimate of `b` in `y ≈ x + b`.
pub fn fit_offset(xs: &[f64], ys: &[f64]) -> f64 {
    let diffs: Vec<f64> = ys.iter().zip(xs).map(|(y, x)| y - x).collect();
    let mut b = median(&diffs);
    for _ in 0..ITERATIONS {
        let residuals: Vec<f64> = diffs.iter().map(|d| d - b).collect();
        let weights = huber_weights(&residuals);
        let total: f64 = weights.iter().sum();
        if total == 0.0 {
            break;
        }
        let next = diffs.iter().zip(&weights).map(|(d, w)| d * w).sum::<f64>() / total;
        if (next - b).abs() < 1e-9 {
            b = next;
            break;
        }
        b = next;
    }
    b
}

/// Robust linear fit with iteratively reweighted least squares.
pub fn fit_line(xs: &[f64], ys: &[f64]) -> Line {
    let mut weights = vec![1.0; xs.len()];
    let mut line = weighted_least_squares(xs, ys, &weights);
    for _ in 0..ITERATIONS {
        let residuals: Vec<f64> = xs
            .iter()
            .zip(ys)
            .map(|(x, y)| y - (line.gain * x + line.offset))
            .collect();
        weights = huber_weights(&residuals);
        let next = weighted_least_squares(xs, ys, &weights);
        let converged =
            (next.gain - line.gain).abs() < 1e-9 && (next.offset - line.offset).abs() < 1e-9;
        line = next;
        if converged {
            break;
        }
    }
    line
}

fn weighted_least_squares(xs: &[f64], ys: &[f64], weights: &[f64]) -> Line {
    let total: f64 = weights.iter().sum();
    let mean_x = xs.iter().zip(weights).map(|(x, w)| x * w).sum::<f64>() / total;
    let mean_y = ys.iter().zip(weights).map(|(y, w)| y * w).sum::<f64>() / total;
    let sxx: f64 = xs
        .iter()
        .zip(weights)
        .map(|(x, w)| w * (x - mean_x).powi(2))
        .sum();
    let sxy: f64 = xs
        .iter()
        .zip(ys)
        .zip(weights)
        .map(|((x, y), w)| w * (x - mean_x) * (y - mean_y))
        .sum();
    if sxx <= f64::EPSILON {
        return Line {
            gain: 1.0,
            offset: mean_y - mean_x,
            gain_stderr: f64::INFINITY,
        };
    }
    let gain = sxy / sxx;
    let offset = mean_y - gain * mean_x;
    let residual_var = xs
        .iter()
        .zip(ys)
        .zip(weights)
        .map(|((x, y), w)| w * (y - gain * x - offset).powi(2))
        .sum::<f64>()
        / (total - 2.0).max(1.0);
    Line {
        gain,
        offset,
        gain_stderr: (residual_var / sxx).sqrt(),
    }
}

fn huber_weights(residuals: &[f64]) -> Vec<f64> {
    let deviations: Vec<f64> = residuals.iter().map(|r| r.abs()).collect();
    let scale = 1.4826 * median(&deviations);
    if scale <= f64::EPSILON {
        return vec![1.0; residuals.len()];
    }
    let limit = HUBER_K * scale;
    residuals
        .iter()
        .map(|r| {
            if r.abs() <= limit {
                1.0
            } else {
                limit / r.abs()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_ignores_outliers() {
        let xs: Vec<f64> = (0..50).map(f64::from).collect();
        let mut ys: Vec<f64> = xs.iter().map(|x| 1.05 * x - 2.0).collect();
        ys[10] += 30.0;
        ys[40] -= 25.0;
        let line = fit_line(&xs, &ys);
        assert!((line.gain - 1.05).abs() < 1e-3, "{line:?}");
        assert!((line.offset + 2.0).abs() < 0.05, "{line:?}");
    }

    #[test]
    fn offset_ignores_outliers() {
        let xs = [20.0, 21.0, 22.0, 23.0, 24.0, 25.0];
        let ys = [20.3, 21.3, 22.3, 33.0, 24.3, 25.3];
        assert!((fit_offset(&xs, &ys) - 0.3).abs() < 0.01);
    }
}
