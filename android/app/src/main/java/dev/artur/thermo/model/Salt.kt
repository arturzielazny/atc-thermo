/*
 * Saturated salt as a humidity reference. In a sealed container over damp kitchen salt (NaCl,
 * with crystals left undissolved), the air settles at a humidity the salt fixes, about 75 %RH
 * and nearly the same from 0 to 50 °C. A session can say in which place the thermometers sat in
 * such a container; once its humidity has stopped drifting there, the humidity corrections are
 * shifted so the group reads the salt's value instead of its own average. The consensus fit
 * still decides how the thermometers differ from each other, and the gains.
 */
package dev.artur.thermo.model

import dev.artur.thermo.calibration.Fit
import dev.artur.thermo.calibration.Reading
import dev.artur.thermo.calibration.Report
import dev.artur.thermo.calibration.median
import kotlin.math.abs

/**
 * Saturated NaCl, %RH, every 5 °C from 0 to 50 °C: Greenspan, "Humidity fixed points of binary
 * saturated aqueous solutions", J. Res. NBS 81A (1977), table 2.
 */
private val NACL = doubleArrayOf(75.51, 75.65, 75.67, 75.61, 75.47, 75.29, 75.09, 74.87, 74.68, 74.52, 74.43)

/** Their uncertainty in the same table, ± %RH: 0.34 at 0 °C, 0.18 at 15 °C, 0.12 at 25 °C. */
private val NACL_UNCERTAINTY = doubleArrayOf(0.34, 0.27, 0.22, 0.18, 0.14, 0.12, 0.11, 0.12, 0.13, 0.16, 0.19)

/** The humidity over saturated NaCl at [temperature] °C, interpolated, held at the table's ends. */
fun naclHumidity(temperature: Double): Double = interpolate(NACL, temperature)

/** Its uncertainty, ± %RH. */
fun naclUncertainty(temperature: Double): Double = interpolate(NACL_UNCERTAINTY, temperature)

private fun interpolate(table: DoubleArray, temperature: Double): Double {
    val x = (temperature / 5).coerceIn(0.0, table.size - 1.0)
    val i = x.toInt().coerceAtMost(table.size - 2)
    return table[i] + (table[i + 1] - table[i]) * (x - i)
}

/**
 * The drift over [SALT_RATE_MS] below which the container counts as settled, %RH per hour.
 * A box of 7 thermometers at 15 °C (Nov 25–26) still rose 0.1 %RH an hour after 21 h, reading
 * 74.2 %RH against the salt's 75.6, and its rise had slowed to a time constant of about 10 h:
 * at 0.03 an hour some 0.3 %RH would still have been to come. Fitting a curve to guess the end
 * didn't work there: its end kept moving up as more hours came in, from 71 %RH after 6 h to 75
 * after 21 h.
 */
const val SALT_SETTLED_RATE = 0.03

/** The stretch the drift is measured over: with 10-minute medians of 7 devices, ±0.005 %RH an hour. */
const val SALT_RATE_MS = 3 * 3600_000L

/** The group's humidity in the container is the median of this last stretch while it was settled. */
const val SALT_LEVEL_MS = 3600_000L

/** Bins of the group's humidity for its drift. */
private const val SALT_BIN_MS = 10 * 60_000L

/** A thermometer this many °C warmer than the salt reads this much lower at 75 %RH and 15 °C, %RH per 0.1 °C. */
const val SALT_PER_TENTH_DEGREE = 0.5

/** Decimals to show a drift of [rate] %RH an hour with: three near [SALT_SETTLED_RATE], so 0.034 isn't "0.03, under 0.03". */
fun rateDecimals(rate: Double): Int = if (abs(rate) < 0.1) 3 else 2

/** How the salt container of a session went: what it should read, what the group read, and whether that counts. */
data class Salt(
    /** The place the thermometers were in the container: 0 the first, 1 the second. */
    val place: Int,
    /** Drift of the group's humidity over the last [SALT_RATE_MS] there, %RH per hour; `null` with less than that. */
    val rate: Double?,
    /** End of the latest stretch there that was settled, ms; `null` if none was. */
    val settledAt: Long? = null,
    /** The group's corrected temperature and humidity over [SALT_LEVEL_MS] up to [settledAt], or lately if not settled. */
    val temperature: Double? = null,
    val read: Double? = null,
) {
    /** What the salt holds at [temperature], %RH. */
    val expected: Double? get() = temperature?.let(::naclHumidity)

    /** Added to every humidity correction once settled, %RH. */
    val shift: Double? get() = if (settledAt != null && read != null) expected?.let { it - read } else null
}

/**
 * The salt status of [place] ([readings] per device in s, [report] their fit) and the latest
 * stretch there whose group humidity drifted less than [SALT_SETTLED_RATE]. Outliers don't count.
 */
fun saltStatus(readings: List<List<Reading>>, report: Report, place: Place, index: Int): Salt {
    val included = readings.indices.filter { i -> report.sensors.getOrNull(i)?.let { !it.humidity.outlier && !it.temperature.outlier } ?: false }
    val from = place.since
    val bins = ((place.until - from) / SALT_BIN_MS).toInt()
    // The group's median humidity per bin: of each device's mean.
    val sums = Array(included.size) { DoubleArray(maxOf(bins, 0)) }
    val counts = Array(included.size) { IntArray(maxOf(bins, 0)) }
    for ((k, i) in included.withIndex()) {
        for (r in readings[i]) {
            val ms = r.at * 1000 - from
            if (ms < 0) continue
            val b = (ms / SALT_BIN_MS).toInt()
            if (b >= bins) continue
            sums[k][b] += r.humidity
            counts[k][b]++
        }
    }
    val humidity = Array(maxOf(bins, 0)) { b ->
        val means = included.indices.mapNotNull { k -> if (counts[k][b] > 0) sums[k][b] / counts[k][b] else null }
        if (means.size >= 2) median(means) else null
    }
    val perWindow = (SALT_RATE_MS / SALT_BIN_MS).toInt()
    fun rateEndingAt(last: Int): Double? = slopePerHour(humidity, last - perWindow + 1, last)
    val rate = rateEndingAt(bins - 1)
    // The latest settled window, also before the box was opened and the move noted.
    val settled = (bins - 1 downTo perWindow - 1).firstOrNull { last -> rateEndingAt(last)?.let { abs(it) <= SALT_SETTLED_RATE } ?: false }
    val until = settled?.let { from + (it + 1) * SALT_BIN_MS } ?: place.until
    val levelFrom = (until - SALT_LEVEL_MS) / 1000.0
    fun corrected(temperature: Boolean): Double? = included.mapNotNull { i ->
        val sensor = report.sensors[i]
        val fit = if (temperature) sensor.temperature else sensor.humidity
        readings[i].filter { it.at >= levelFrom && it.at < until / 1000.0 }
            .map { fit.apply(if (temperature) it.temperature else it.humidity) }
            .ifEmpty { null }?.let(::median)
    }.ifEmpty { null }?.average()
    return Salt(index, rate, settled?.let { until }, corrected(true), corrected(false))
}

/** Least-squares slope of [values] from [first] to [last] (bins of [SALT_BIN_MS]), %RH per hour; `null` with fewer than 80 % of them. */
private fun slopePerHour(values: Array<Double?>, first: Int, last: Int): Double? {
    if (first < 0) return null
    val points = (first..last).mapNotNull { b -> values[b]?.let { b.toDouble() to it } }
    if (points.size * 5 < (last - first + 1) * 4) return null
    val mx = points.sumOf { it.first } / points.size
    val my = points.sumOf { it.second } / points.size
    val sxx = points.sumOf { (it.first - mx) * (it.first - mx) }
    if (sxx == 0.0) return null
    val perBin = points.sumOf { (it.first - mx) * (it.second - my) } / sxx
    return perBin * 3600_000.0 / SALT_BIN_MS
}

/** [report] with [shift] added to every humidity correction. */
fun shiftHumidity(report: Report, shift: Double): Report =
    report.copy(sensors = report.sensors.map { it.copy(humidity = it.humidity.shifted(shift)) })

private fun Fit.shifted(by: Double) = copy(offset = offset + by)
