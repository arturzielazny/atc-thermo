/*
 * Small statistics helpers on unsorted samples, and Huber M-estimation: least squares
 * that down-weights outlying points. Ports of `crates/calibration/src/{stats,robust}.rs`.
 */
package dev.artur.thermo.calibration

import kotlin.math.abs
import kotlin.math.ceil
import kotlin.math.floor
import kotlin.math.sqrt

/** Median; `NaN` for an empty list. */
fun median(values: List<Double>): Double = percentile(values, 0.5)

/** Linear-interpolated percentile, [q] in 0..1; `NaN` for an empty list. */
fun percentile(values: List<Double>, q: Double): Double {
    if (values.isEmpty()) return Double.NaN
    val sorted = values.sorted()
    val position = q.coerceIn(0.0, 1.0) * (sorted.size - 1)
    val below = sorted[floor(position).toInt()]
    val above = sorted[ceil(position).toInt()]
    return below + (above - below) * (position - floor(position))
}

/** Sample standard deviation; 0 for fewer than two values. */
fun stdDev(values: List<Double>): Double {
    if (values.size < 2) return 0.0
    val n = values.size.toDouble()
    val mean = values.sum() / n
    return sqrt(values.sumOf { (it - mean) * (it - mean) } / (n - 1.0))
}

/** Huber tuning constant: 95 % efficiency for Gaussian noise. */
private const val HUBER_K = 1.345
private const val ROBUST_ITERATIONS = 30

/** Coefficients of `y ≈ Σ coef[j] · row[j]` with their standard errors. */
data class Model(val coef: List<Double>, val stderr: List<Double>)

/**
 * Robust linear regression by iteratively reweighted least squares.
 *
 * `rows[i]` are the regressors of observation `i` (include a constant 1 for an
 * intercept). Returns `null` if the regressors are collinear or there are fewer
 * observations than coefficients.
 */
fun robustFit(rows: List<DoubleArray>, ys: List<Double>): Model? {
    val k = rows.firstOrNull()?.size ?: return null
    if (rows.size <= k) return null
    var weights = DoubleArray(rows.size) { 1.0 }
    var coef = weightedLeastSquares(rows, ys, weights)?.first ?: return null
    for (iteration in 0 until ROBUST_ITERATIONS) {
        weights = huberWeights(residuals(rows, ys, coef))
        val next = weightedLeastSquares(rows, ys, weights)?.first ?: return null
        val converged = next.indices.all { abs(next[it] - coef[it]) < 1e-10 }
        coef = next
        if (converged) break
    }
    val (final, inverse) = weightedLeastSquares(rows, ys, weights) ?: return null
    val r = residuals(rows, ys, final)
    val total = weights.sum()
    var sum = 0.0
    for (i in r.indices) sum += weights[i] * r[i] * r[i]
    val variance = sum / (total - k).coerceAtLeast(1.0)
    return Model(final.toList(), (0 until k).map { sqrt(variance * inverse[it][it]) })
}

private fun residuals(rows: List<DoubleArray>, ys: List<Double>, coef: DoubleArray): DoubleArray =
    DoubleArray(rows.size) { i ->
        var fitted = 0.0
        for (j in coef.indices) fitted += rows[i][j] * coef[j]
        ys[i] - fitted
    }

/** Solves the weighted normal equations; returns the coefficients and `(XᵀWX)⁻¹`. */
private fun weightedLeastSquares(
    rows: List<DoubleArray>,
    ys: List<Double>,
    weights: DoubleArray,
): Pair<DoubleArray, Array<DoubleArray>>? {
    val k = rows[0].size
    val normal = Array(k) { DoubleArray(k) }
    val rhs = DoubleArray(k)
    for (i in rows.indices) {
        val row = rows[i]
        val w = weights[i]
        for (a in 0 until k) {
            rhs[a] += w * row[a] * ys[i]
            for (b in 0 until k) normal[a][b] += w * row[a] * row[b]
        }
    }
    val inverse = invert(normal) ?: return null
    val coef = DoubleArray(k) { a -> (0 until k).sumOf { b -> inverse[a][b] * rhs[b] } }
    return coef to inverse
}

/** Gauss–Jordan inversion with partial pivoting; `null` if (nearly) singular. */
private fun invert(input: Array<DoubleArray>): Array<DoubleArray>? {
    val n = input.size
    val m = Array(n) { input[it].copyOf() }
    val scale = m.maxOf { row -> row.maxOf { abs(it) } }.coerceAtLeast(Double.MIN_VALUE)
    val inv = Array(n) { i -> DoubleArray(n) { j -> if (i == j) 1.0 else 0.0 } }
    for (col in 0 until n) {
        val pivot = (col until n).maxBy { abs(m[it][col]) }
        if (abs(m[pivot][col]) <= scale * 1e-12) return null
        m[col] = m[pivot].also { m[pivot] = m[col] }
        inv[col] = inv[pivot].also { inv[pivot] = inv[col] }
        val p = m[col][col]
        for (j in 0 until n) {
            m[col][j] /= p
            inv[col][j] /= p
        }
        for (row in 0 until n) {
            if (row == col) continue
            val factor = m[row][col]
            for (j in 0 until n) {
                m[row][j] -= factor * m[col][j]
                inv[row][j] -= factor * inv[col][j]
            }
        }
    }
    return inv
}

private fun huberWeights(residuals: DoubleArray): DoubleArray {
    val scale = 1.4826 * median(residuals.map { abs(it) })
    if (scale <= Math.ulp(1.0)) return DoubleArray(residuals.size) { 1.0 }
    val limit = HUBER_K * scale
    return DoubleArray(residuals.size) { if (abs(residuals[it]) <= limit) 1.0 else limit / abs(residuals[it]) }
}
