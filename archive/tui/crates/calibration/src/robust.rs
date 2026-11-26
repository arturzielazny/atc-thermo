//! Huber M-estimation: least squares that down-weights outlying points.

use crate::stats::median;

/// Huber tuning constant: 95 % efficiency for Gaussian noise.
const HUBER_K: f64 = 1.345;
const ITERATIONS: usize = 30;

/// Coefficients of `y ≈ Σ coef[j] · row[j]` with their standard errors.
#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    pub coef: Vec<f64>,
    pub stderr: Vec<f64>,
}

/// Robust linear regression by iteratively reweighted least squares.
///
/// `rows[i]` are the regressors of observation `i` (include a constant 1 for an
/// intercept). Returns `None` if the regressors are collinear or there are fewer
/// observations than coefficients.
pub fn fit(rows: &[Vec<f64>], ys: &[f64]) -> Option<Model> {
    let k = rows.first()?.len();
    if rows.len() <= k {
        return None;
    }
    let mut weights = vec![1.0; rows.len()];
    let (mut coef, _) = weighted_least_squares(rows, ys, &weights)?;
    for _ in 0..ITERATIONS {
        weights = huber_weights(&residuals(rows, ys, &coef));
        let (next, _) = weighted_least_squares(rows, ys, &weights)?;
        let converged = next.iter().zip(&coef).all(|(a, b)| (a - b).abs() < 1e-10);
        coef = next;
        if converged {
            break;
        }
    }
    let (coef, inverse) = weighted_least_squares(rows, ys, &weights)?;
    let r = residuals(rows, ys, &coef);
    let total: f64 = weights.iter().sum();
    let variance =
        r.iter().zip(&weights).map(|(r, w)| w * r * r).sum::<f64>() / (total - k as f64).max(1.0);
    let stderr = (0..k).map(|j| (variance * inverse[j][j]).sqrt()).collect();
    Some(Model { coef, stderr })
}

fn residuals(rows: &[Vec<f64>], ys: &[f64], coef: &[f64]) -> Vec<f64> {
    rows.iter()
        .zip(ys)
        .map(|(row, y)| y - row.iter().zip(coef).map(|(x, c)| x * c).sum::<f64>())
        .collect()
}

/// Solves the weighted normal equations; returns the coefficients and `(XᵀWX)⁻¹`.
fn weighted_least_squares(
    rows: &[Vec<f64>],
    ys: &[f64],
    weights: &[f64],
) -> Option<(Vec<f64>, Vec<Vec<f64>>)> {
    let k = rows[0].len();
    // Normal equations: (XᵀWX) coef = XᵀWy.
    let mut normal = vec![vec![0.0; k]; k];
    let mut rhs = vec![0.0; k];
    for ((row, y), w) in rows.iter().zip(ys).zip(weights) {
        for a in 0..k {
            rhs[a] += w * row[a] * y;
            for b in 0..k {
                normal[a][b] += w * row[a] * row[b];
            }
        }
    }
    let inverse = invert(normal)?;
    let coef = (0..k)
        .map(|a| (0..k).map(|b| inverse[a][b] * rhs[b]).sum())
        .collect();
    Some((coef, inverse))
}

/// Gauss–Jordan inversion with partial pivoting; `None` if (nearly) singular.
fn invert(mut m: Vec<Vec<f64>>) -> Option<Vec<Vec<f64>>> {
    let n = m.len();
    let scale = m
        .iter()
        .flatten()
        .fold(0.0_f64, |acc, v| acc.max(v.abs()))
        .max(f64::MIN_POSITIVE);
    let mut inv: Vec<Vec<f64>> = (0..n)
        .map(|i| (0..n).map(|j| if i == j { 1.0 } else { 0.0 }).collect())
        .collect();
    for col in 0..n {
        let pivot = (col..n).max_by(|&a, &b| m[a][col].abs().total_cmp(&m[b][col].abs()))?;
        if m[pivot][col].abs() <= scale * 1e-12 {
            return None;
        }
        m.swap(col, pivot);
        inv.swap(col, pivot);
        let p = m[col][col];
        for j in 0..n {
            m[col][j] /= p;
            inv[col][j] /= p;
        }
        for row in 0..n {
            if row != col {
                let factor = m[row][col];
                for j in 0..n {
                    m[row][j] -= factor * m[col][j];
                    inv[row][j] -= factor * inv[col][j];
                }
            }
        }
    }
    Some(inv)
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
        let rows: Vec<Vec<f64>> = (0..50).map(|x| vec![f64::from(x), 1.0]).collect();
        let mut ys: Vec<f64> = rows.iter().map(|r| 1.05 * r[0] - 2.0).collect();
        ys[10] += 30.0;
        ys[40] -= 25.0;
        let model = fit(&rows, &ys).expect("well posed");
        assert!((model.coef[0] - 1.05).abs() < 1e-3, "{model:?}");
        assert!((model.coef[1] + 2.0).abs() < 0.05, "{model:?}");
    }

    #[test]
    fn three_regressors() {
        // y = 0.9 a + 3 + 0.5 b with a bit of deterministic noise.
        let rows: Vec<Vec<f64>> = (0..40)
            .map(|i| {
                let i = f64::from(i);
                vec![i, 1.0, (i * 0.7).sin()]
            })
            .collect();
        let ys: Vec<f64> = rows
            .iter()
            .enumerate()
            .map(|(i, r)| 0.9 * r[0] + 3.0 + 0.5 * r[2] + 0.001 * (i as f64).cos())
            .collect();
        let model = fit(&rows, &ys).expect("well posed");
        for (got, want) in model.coef.iter().zip([0.9, 3.0, 0.5]) {
            assert!((got - want).abs() < 0.01, "{model:?}");
        }
        assert!(model.stderr.iter().all(|s| *s < 0.01));
    }

    #[test]
    fn collinear_is_rejected() {
        let rows: Vec<Vec<f64>> = (0..10)
            .map(|x| vec![f64::from(x), 2.0 * f64::from(x)])
            .collect();
        let ys = vec![1.0; 10];
        assert_eq!(fit(&rows, &ys), None);
    }
}
