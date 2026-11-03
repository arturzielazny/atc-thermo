/*
 * Consensus calibration of co-located temperature and humidity sensors; a port of
 * `crates/calibration` (algorithm in `docs/calibration.md` §4).
 *
 * Sensors placed together measure the same air, so their disagreement is sensor error.
 * [calibrate] fits a linear correction `gain · reported + offset` per sensor such that
 * all sensors agree, anchored so the corrections average to zero over the sensors that
 * are not outliers. Durations are in seconds throughout.
 */
package dev.artur.thermo.calibration

import kotlin.math.abs
import kotlin.math.floor
import kotlin.math.max
import kotlin.math.min
import kotlin.math.sqrt

/** One reading of one sensor; [at] in seconds on any common clock. */
data class Reading(val at: Double, val temperature: Double, val humidity: Double)

/** Tuning knobs; the defaults suit LYWSD03MMC-class thermometers. */
data class Options(
    /** Width of a resampling bin. Wide because BLE reception is lossy. */
    val bin: Double = 5 * 60.0,
    /** Window around a bin over which readings must be steady for the bin to be used. */
    val steadyWindow: Double = 30 * 60.0,
    /** Largest temperature range within the window, for the group and each sensor, °C. */
    val maxTemperatureChange: Double = 0.75,
    val maxHumidityChange: Double = 3.5,
    /** After a big change readings are ignored for this long, while cases catch up. */
    val settleTime: Double = 60 * 60.0,
    val bigTemperatureChange: Double = 2.0,
    val bigHumidityChange: Double = 10.0,
    /** Model case lag only with at least this drift (°C/h, 10th to 90th percentile). */
    val minTemperatureDrift: Double = 0.3,
    val minHumidityDrift: Double = 1.5,
    /** Fit a gain only if steady data spans at least this many °C / %RH. */
    val minTemperatureSpan: Double = 5.0,
    val minHumiditySpan: Double = 15.0,
    /** Humidity readings above this are left out (sensor saturation and creep). */
    val maxHumidity: Double = 95.0,
    /** Robust z-score above which a sensor counts as an outlier. */
    val outlierThreshold: Double = 3.5,
    /** Smallest correction spread assumed when scoring outliers. */
    val temperatureOutlierFloor: Double = 0.15,
    val humidityOutlierFloor: Double = 1.5,
    /** Least steady temperature data needed for a result (and per sensor to count). */
    val minSteady: Double = 60 * 60.0,
) {
    /** Least time readings must span: half the steady window on both sides never counts. */
    val shortestSpan: Double get() = minSteady + steadyWindow
}

/** A fitted correction: `corrected = gain · reported + offset`. */
data class Fit(
    val gain: Double = 1.0,
    val offset: Double = 0.0,
    /** Whether a gain was estimated; otherwise [gain] is 1. */
    val gainFitted: Boolean = false,
    val gainStderr: Double = 0.0,
    /** RMS difference to the consensus after correction, on steady bins. */
    val residualRms: Double = 0.0,
    /** Seconds the sensor responds later than the group, if the data drifted enough to tell. */
    val lag: Double? = null,
    /** Left out of the consensus and the anchor. Its fit still pulls it to the others. */
    val outlier: Boolean = false,
    /** Seconds of steady data this fit is based on. */
    val steady: Double = 0.0,
) {
    fun apply(value: Double) = gain * value + offset

    companion object {
        val IDENTITY = Fit()
    }
}

data class SensorReport(val temperature: Fit, val humidity: Fit)

/** Mean across bins of the standard deviation across non-outlier sensors. */
data class Spread(val before: Double, val after: Double)

data class QuantityReport(
    /** Time with at least two steady sensors. */
    val steady: Double,
    /** 5th to 95th percentile of the steady consensus. */
    val range: Pair<Double, Double>,
    val spread: Spread,
)

/** A stretch of steady conditions, e.g. "inside" or "outside". */
data class Phase(val start: Double, val end: Double, val steady: Double, val temperature: Double)

data class Report(
    val sensors: List<SensorReport>,
    val temperature: QuantityReport,
    val humidity: QuantityReport,
    /** Steady temperature stretches, split where the level changes. */
    val phases: List<Phase>,
)

sealed class CalibrationError(message: String) : Exception(message) {
    data class TooFewSensors(val count: Int) :
        CalibrationError("need at least 2 sensors with data, have $count")

    data class TooShort(val span: Double, val needed: Double) : CalibrationError(
        "data covers only ${(span / 60).toLong()} minutes, a fit needs at least ${(needed / 60).toLong()}"
    )

    data class NotEnoughSteadyData(val steady: Double, val needed: Double) : CalibrationError(
        "only ${(steady / 60).toLong()} steady minutes, need ${(needed / 60).toLong()}; " +
            "keep the sensors together and undisturbed longer"
    )
}

private enum class Quantity {
    Temperature, Humidity;

    fun of(r: Reading) = if (this == Temperature) r.temperature else r.humidity
    fun maxChange(o: Options) = if (this == Temperature) o.maxTemperatureChange else o.maxHumidityChange
    fun minDriftPerSecond(o: Options) =
        (if (this == Temperature) o.minTemperatureDrift else o.minHumidityDrift) / 3600.0
    fun bigChange(o: Options) = if (this == Temperature) o.bigTemperatureChange else o.bigHumidityChange
    fun minSpan(o: Options) = if (this == Temperature) o.minTemperatureSpan else o.minHumiditySpan
    fun outlierFloor(o: Options) =
        if (this == Temperature) o.temperatureOutlierFloor else o.humidityOutlierFloor
}

/** Per-bin mean of one quantity: `grid[bin][sensor]`. */
private typealias Grid = List<List<Double?>>

/**
 * Fits corrections for co-located sensors; `sensors[i]` are sensor i's readings in any order.
 *
 * @throws CalibrationError with fewer than two sensors with readings, readings spanning
 * less than [Options.shortestSpan], or without [Options.minSteady] of steady temperature.
 */
fun calibrate(sensors: List<List<Reading>>, options: Options = Options()): Report {
    val withData = sensors.count { it.isNotEmpty() }
    if (withData < 2) throw CalibrationError.TooFewSensors(withData)
    val bin = options.bin
    val all = sensors.flatten()
    val t0 = all.minOf { it.at }
    val t1 = all.maxOf { it.at }
    val span = t1 - t0
    if (span < options.shortestSpan) throw CalibrationError.TooShort(span, options.shortestSpan)

    val rawT = resample(sensors, t0, bin, Quantity.Temperature)
    val temperature = settled(rawT, Quantity.Temperature, options)
    val temperatureRates = rates(rawT, bin)
    val steady = bin * steadyBinCount(temperature)
    if (steady < options.minSteady) throw CalibrationError.NotEnoughSteadyData(steady, options.minSteady)
    val rawH = resample(sensors, t0, bin, Quantity.Humidity)
    val humidityRates = rates(rawH, bin)
    val humidity = settled(rawH, Quantity.Humidity, options).map { row ->
        row.map { cell -> cell?.takeIf { it <= options.maxHumidity } }
    }

    val (tFits, tReport) = fitQuantity(temperature, temperatureRates, Quantity.Temperature, options)
    val humiditySteady = bin * steadyBinCount(humidity)
    val (hFits, hReport) = if (humiditySteady >= options.minSteady) {
        fitQuantity(humidity, humidityRates, Quantity.Humidity, options)
    } else {
        val identity = List(sensors.size) { Fit.IDENTITY }
        identity to quantityReport(humidity, identity, bin)
    }
    val goodT = tFits.map { !it.outlier }
    return Report(
        sensors = tFits.zip(hFits) { t, h -> SensorReport(t, h) },
        temperature = tReport,
        humidity = hReport,
        phases = phases(temperature, goodT, t0, bin),
    )
}

private fun fitQuantity(
    grid: Grid,
    rates: Grid,
    quantity: Quantity,
    options: Options,
): Pair<List<Fit>, QuantityReport> {
    val sensors = grid.firstOrNull()?.size ?: 0
    var good = List(sensors) { true }
    var fits = fitAll(grid, rates, good, quantity, options)
    if (sensors >= 4) {
        val outliers = findOutliers(grid, fits, quantity, options)
        if (outliers.any { it }) {
            good = outliers.map { !it }
            fits = fitAll(grid, rates, good, quantity, options)
        }
    }
    var corrected = correctedGrid(grid, fits)
    fits = fits.mapIndexed { i, fit ->
        val p = pairs(grid, rates, corrected, good, i)
        fit.copy(outlier = !good[i], steady = options.bin * p.xs.size)
    }
    // Sensors with too little data neither define nor shift the group average.
    val anchors = fits.map { !it.outlier && it.steady >= options.minSteady }
    fits = anchor(fits, if (anchors.any { it }) anchors else good)
    corrected = correctedGrid(grid, fits)
    fits = fits.mapIndexed { i, fit ->
        fit.copy(residualRms = residualRms(pairs(grid, rates, corrected, good, i), fit))
    }
    return fits to quantityReport(grid, fits, options.bin)
}

private fun quantityReport(grid: Grid, fits: List<Fit>, bin: Double): QuantityReport {
    val good = fits.map { !it.outlier }
    val consensus = grid.mapNotNull { consensus(it, good) }
    return QuantityReport(
        steady = bin * steadyBinCount(grid),
        range = percentile(consensus, 0.05) to percentile(consensus, 0.95),
        spread = spread(grid, good, fits),
    )
}

/** Averages one quantity into fixed-width bins starting at `t0`. */
private fun resample(sensors: List<List<Reading>>, t0: Double, bin: Double, quantity: Quantity): Grid {
    val t1 = sensors.flatten().maxOf { it.at }
    val bins = binIndex(t1 - t0, bin) + 1
    val sums = Array(bins) { DoubleArray(sensors.size) }
    val counts = Array(bins) { IntArray(sensors.size) }
    sensors.forEachIndexed { i, readings ->
        for (r in readings) {
            val b = binIndex(r.at - t0, bin)
            sums[b][i] += quantity.of(r)
            counts[b][i]++
        }
    }
    return List(bins) { b -> List(sensors.size) { i -> if (counts[b][i] > 0) sums[b][i] / counts[b][i] else null } }
}

private fun binIndex(offset: Double, bin: Double): Int = max(floor(offset / bin), 0.0).toInt()

private fun steadyBinCount(grid: Grid): Int = grid.count { row -> row.count { it != null } >= 2 }

/** Median over the included sensors present in a bin, if at least half of them are. */
private fun consensus(row: List<Double?>, include: List<Boolean>): Double? {
    val values = row.filterIndexed { i, _ -> include[i] }.filterNotNull()
    val needed = max((include.count { it } + 1) / 2, 2)
    return if (values.size >= needed) median(values) else null
}

/**
 * Keeps only readings taken while both the group median and the sensor itself stayed
 * within [Quantity.maxChange] over the steady window, and not shortly after a big
 * change of the group median.
 */
private fun settled(grid: Grid, quantity: Quantity, options: Options): Grid {
    val bin = options.bin
    val half = binIndex(options.steadyWindow / 2.0, bin)
    val settle = binIndex(options.settleTime, bin)
    val limit = quantity.maxChange(options)
    val sensors = grid.firstOrNull()?.size ?: 0
    val allIncluded = List(sensors) { true }
    val group = grid.map { consensus(it, allIncluded) }
    val settling = grid.indices.map { b ->
        range(group.subList(max(b - settle, 0), b + 1)) >= quantity.bigChange(options)
    }
    val calmCells = (0 until sensors).map { i ->
        val own = grid.map { it[i] }
        grid.indices.map { b -> !settling[b] && calm(group, b, half, limit) && calm(own, b, half, limit) }
    }
    return grid.mapIndexed { b, row ->
        val masked = row.mapIndexed { i, cell -> cell?.takeIf { calmCells[i][b] } }
        // A bin needs at least two sensors to compare anything.
        if (masked.count { it != null } >= 2) masked else List(row.size) { null }
    }
}

/** Whether `series` around bin `b` is mostly observed and varies by at most `limit`. */
private fun calm(series: List<Double?>, b: Int, half: Int, limit: Double): Boolean {
    if (b < half || b + half >= series.size) return false
    val window = series.subList(b - half, b + half + 1)
    val present = window.count { it != null }
    return present * 2 >= window.size && range(window) <= limit
}

/** Largest minus smallest observed value; 0 if none. */
private fun range(values: List<Double?>): Double {
    var lo = Double.POSITIVE_INFINITY
    var hi = Double.NEGATIVE_INFINITY
    for (v in values) if (v != null) {
        lo = min(lo, v)
        hi = max(hi, v)
    }
    return if (lo <= hi) hi - lo else 0.0
}

/** Iteratively fits every sensor against the median of the other included sensors. */
private fun fitAll(grid: Grid, rates: Grid, include: List<Boolean>, quantity: Quantity, options: Options): List<Fit> {
    var fits = List(include.size) { Fit.IDENTITY }
    repeat(4) {
        val corrected = correctedGrid(grid, fits)
        fits = include.indices.map { fitSensor(grid, rates, corrected, include, it, quantity, options) }
    }
    return fits
}

/** Rate of change per second of every sensor's raw series, from neighbouring bins. */
private fun rates(raw: Grid, bin: Double): Grid {
    val sensors = raw.firstOrNull()?.size ?: 0
    return raw.indices.map { b ->
        (0 until sensors).map { i ->
            val before = raw.getOrNull(b - 1)?.get(i)
            val here = raw[b][i]
            val after = raw.getOrNull(b + 1)?.get(i)
            when {
                before != null && after != null -> (after - before) / (2.0 * bin)
                before != null && here != null -> (here - before) / bin
                here != null && after != null -> (after - here) / bin
                else -> null
            }
        }
    }
}

private fun correctedGrid(grid: Grid, fits: List<Fit>): Grid =
    grid.map { row -> row.mapIndexed { i, cell -> cell?.let { fits[i].apply(it) } } }

/** Readings of one sensor paired with the median of the other included sensors. */
private class Pairs(val xs: List<Double>, val ys: List<Double>, val rates: List<Double?>)

private fun pairs(grid: Grid, rates: Grid, reference: Grid, include: List<Boolean>, i: Int): Pairs {
    var others = include.toMutableList().also { it[i] = false }
    if (others.none { it }) {
        // An excluded sensor in a group of two: compare with everyone else.
        others = MutableList(include.size) { it != i }
    }
    val xs = mutableListOf<Double>()
    val ys = mutableListOf<Double>()
    val rs = mutableListOf<Double?>()
    for (b in grid.indices) {
        val x = grid[b][i] ?: continue
        val values = reference[b].filterIndexed { j, _ -> others[j] }.filterNotNull()
        if (values.isEmpty()) continue
        xs += x
        ys += median(values)
        rs += rates[b][i]
    }
    return Pairs(xs, ys, rs)
}

private fun fitSensor(
    grid: Grid,
    rates: Grid,
    corrected: Grid,
    include: List<Boolean>,
    i: Int,
    quantity: Quantity,
    options: Options,
): Fit {
    var p = pairs(grid, rates, corrected, include, i)
    if (p.xs.isEmpty()) return Fit.IDENTITY
    // Model lag only with enough drift; then use just the pairs with a known rate.
    val known = p.rates.filterNotNull()
    val withLag = known.size * 5 >= p.xs.size * 4 &&
        percentile(known, 0.9) - percentile(known, 0.1) >= quantity.minDriftPerSecond(options)
    if (withLag) {
        val keep = p.rates.map { it != null }
        p = Pairs(
            p.xs.filterIndexed { j, _ -> keep[j] },
            p.ys.filterIndexed { j, _ -> keep[j] },
            p.rates.filterNotNull(),
        )
    }
    val rate = { j: Int -> p.rates[j] ?: 0.0 }

    // y − x ≈ offset (+ k·rate)
    val offsetOnly = run {
        val rows = p.xs.indices.map { j -> if (withLag) doubleArrayOf(1.0, rate(j)) else doubleArrayOf(1.0) }
        val diffs = p.ys.zip(p.xs) { y, x -> y - x }
        val model = robustFit(rows, diffs)
        if (model != null) Fit(offset = model.coef[0], lag = model.coef.getOrNull(1))
        else Fit(offset = median(diffs))
    }
    if (percentile(p.xs, 0.95) - percentile(p.xs, 0.05) < quantity.minSpan(options)) return offsetOnly
    // y ≈ gain·x + offset (+ k·rate), with lag = k / gain
    val rows = p.xs.indices.map { j ->
        if (withLag) doubleArrayOf(p.xs[j], 1.0, rate(j)) else doubleArrayOf(p.xs[j], 1.0)
    }
    val model = robustFit(rows, p.ys) ?: return offsetOnly
    val gain = model.coef[0]
    val gainStderr = model.stderr[0]
    // A gain indistinguishable from 1 only adds noise.
    if (abs(gain - 1.0) < 2.0 * gainStderr) return offsetOnly
    return Fit(
        gain = gain,
        offset = model.coef[1],
        gainFitted = true,
        gainStderr = gainStderr,
        lag = model.coef.getOrNull(2)?.let { it / gain },
    )
}

/** Flags sensors whose correction at the typical operating point is far from the others'. */
private fun findOutliers(grid: Grid, fits: List<Fit>, quantity: Quantity, options: Options): List<Boolean> {
    val allIncluded = List(fits.size) { true }
    val typical = median(grid.mapNotNull { consensus(it, allIncluded) })
    val corrections = fits.map { it.apply(typical) - typical }
    val center = median(corrections)
    val deviations = corrections.map { abs(it - center) }
    val scale = max(1.4826 * median(deviations), quantity.outlierFloor(options))
    return deviations.map { it / scale > options.outlierThreshold }
}

/** Rescales all fits so the included sensors' gains average 1 and offsets average 0. */
private fun anchor(fits: List<Fit>, include: List<Boolean>): List<Fit> {
    val chosen = fits.filterIndexed { i, _ -> include[i] }
    val meanGain = chosen.sumOf { it.gain } / chosen.size
    val meanOffset = chosen.sumOf { it.offset } / chosen.size
    val alpha = 1.0 / meanGain
    return fits.map {
        it.copy(gain = it.gain * alpha, offset = alpha * (it.offset - meanOffset), gainStderr = it.gainStderr * alpha)
    }
}

/** RMS of what the fit (including lag, where modelled) leaves unexplained. */
private fun residualRms(p: Pairs, fit: Fit): Double {
    if (p.xs.isEmpty()) return 0.0
    var sum = 0.0
    for (j in p.xs.indices) {
        val rate = p.rates[j]
        val lagTerm = if (fit.lag != null && rate != null) fit.gain * fit.lag * rate else 0.0
        val r = fit.apply(p.xs[j]) + lagTerm - p.ys[j]
        sum += r * r
    }
    return sqrt(sum / p.xs.size)
}

private fun spread(grid: Grid, include: List<Boolean>, fits: List<Fit>): Spread {
    val before = mutableListOf<Double>()
    val after = mutableListOf<Double>()
    for (row in grid) {
        val present = row.indices.filter { include[it] && row[it] != null }
        if (present.size < 2) continue
        before += stdDev(present.map { row[it]!! })
        after += stdDev(present.map { fits[it].apply(row[it]!!) })
    }
    fun mean(v: List<Double>) = if (v.isEmpty()) 0.0 else v.sum() / v.size
    return Spread(mean(before), mean(after))
}

/** Groups steady temperature bins into phases of similar level. */
private fun phases(grid: Grid, include: List<Boolean>, t0: Double, bin: Double): List<Phase> {
    val samePhase = 2.0
    class Building(val start: Int, var end: Int, val ts: MutableList<Double>)
    val phases = mutableListOf<Building>()
    grid.forEachIndexed { b, row ->
        val t = consensus(row, include) ?: return@forEachIndexed
        val last = phases.lastOrNull()
        if (last != null && abs(median(last.ts) - t) < samePhase) {
            last.end = b
            last.ts += t
        } else {
            phases += Building(b, b, mutableListOf(t))
        }
    }
    return phases.map {
        Phase(
            start = t0 + it.start * bin,
            end = t0 + (it.end + 1) * bin,
            steady = bin * it.ts.size,
            temperature = median(it.ts),
        )
    }
}
