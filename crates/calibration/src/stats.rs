//! Small statistics helpers on unsorted samples.

/// Median; `NaN` for an empty slice.
pub fn median(values: &[f64]) -> f64 {
    percentile(values, 0.5)
}

/// Linear-interpolated percentile, `q` in 0..=1; `NaN` for an empty slice.
pub fn percentile(values: &[f64], q: f64) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let position = q.clamp(0.0, 1.0) * (sorted.len() - 1) as f64;
    let below = sorted[floor_index(position)];
    let above = sorted[floor_index(position.ceil())];
    below + (above - below) * position.fract()
}

/// Sample standard deviation; 0 for fewer than two values.
pub fn std_dev(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let n = values.len() as f64;
    let mean = values.iter().sum::<f64>() / n;
    (values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1.0)).sqrt()
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "callers pass in-range non-negative positions"
)]
fn floor_index(position: f64) -> usize {
    position as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basics() {
        assert!((median(&[3.0, 1.0, 2.0]) - 2.0).abs() < 1e-12);
        assert!((median(&[4.0, 1.0, 2.0, 3.0]) - 2.5).abs() < 1e-12);
        assert!((percentile(&[0.0, 10.0], 0.25) - 2.5).abs() < 1e-12);
        assert!(median(&[]).is_nan());
        assert!((std_dev(&[1.0, 3.0]) - 2.0_f64.sqrt()).abs() < 1e-12);
    }
}
