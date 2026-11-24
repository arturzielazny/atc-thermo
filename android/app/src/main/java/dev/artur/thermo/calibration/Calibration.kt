/*
 * Consensus calibration of co-located temperature and humidity sensors; a port of
 * `crates/calibration` (algorithm in `docs/calibration.md` §4).
 *
 * Sensors placed together measure the same air, so their disagreement is sensor error.
 * [calibrate] fits a linear correction `gain · reported + offset` per sensor such that
 * all sensors agree, anchored so the corrections average to zero over the sensors that
 * are not outliers, or to one reference sensor ([Options.reference]) declared correct.
 * Durations are in seconds throughout.
 *
 * Unlike the Rust crate, this port also estimates how precise each correction is
 * ([Fit.stderr], [QuantityReport.inflation]), for the app's live guidance; the fit itself is
 * the same.
 */
package dev.artur.thermo.calibration

import kotlin.math.abs
import kotlin.math.exp
import kotlin.math.floor
import kotlin.math.ln
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
    /**
     * Android only: after a change of half a big change or more within 30 minutes, also
     * hold back each sensor until its offset to the others is within [settledTemperature] /
     * [settledHumidity] of where it is heading ([Settling]). The Rust crate has
     * [settleTime] alone.
     */
    val holdUntilSettled: Boolean = false,
    val settledTemperature: Double = 0.03,
    val settledHumidity: Double = 0.15,
    /** Slowest a sensor's offset is taken to catch up with the others: its time constant, s. */
    val slowestTemperatureSettling: Double = 40 * 60.0,
    val slowestHumiditySettling: Double = 3 * 3600.0,
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
    /**
     * Index of a sensor declared correct: the others are corrected to read like it, and its
     * own correction is the identity. `null` anchors to the group average.
     */
    val reference: Int? = null,
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
    /**
     * Standard error of the correction at the mean of the steady readings, from how much
     * the residuals scatter and how long they stay correlated ([QuantityReport.inflation]);
     * `null` without at least two steady bins.
     */
    val stderr: Double? = null,
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
    /**
     * How much residuals correlated over time inflate the variance of a mean, compared with
     * independent bins (1 if they are independent), pooled over the non-outlier sensors.
     * A room drifting slowly leaves long runs of residuals of the same sign, so each bin
     * tells less than it seems. It also applies to [Fit.gainStderr], which assumes
     * independent bins: multiply that by its square root.
     */
    val inflation: Double = 1.0,
    /** Start times of the bins with at least two steady sensors, the readings fitted. */
    val steadyBins: List<Double> = emptyList(),
    /** Per sensor, whether it is held back at the end of the readings after a change; `null` if not. */
    val settling: List<Settling?> = emptyList(),
)

/**
 * A sensor whose readings don't count yet after a big change ([Options.settleTime]), or
 * whose offset to the others is still on its way to a new level ([Options.holdUntilSettled]):
 * cases catch up with the air at their own speed, humidity sensors much slower than
 * temperature ones.
 */
data class Settling(
    /** When its readings are expected to count, s on the readings' clock; `null` while that can't be told yet. */
    val until: Double?,
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

    /** The reference ([Options.reference]) has too little steady data of its own. */
    data class ReferenceNotSteady(val steady: Double, val needed: Double) : CalibrationError(
        "the reference has only ${(steady / 60).toLong()} steady minutes alongside the others, " +
            "need ${(needed / 60).toLong()}"
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
 * less than [Options.shortestSpan], without [Options.minSteady] of steady temperature, or
 * with a reference sensor that has less than that itself (in either quantity that is fitted).
 * @throws IllegalArgumentException if [Options.reference] is not an index into [sensors].
 */
fun calibrate(sensors: List<List<Reading>>, options: Options = Options()): Report {
    options.reference?.let { require(it in sensors.indices) { "reference $it of ${sensors.size} sensors" } }
    val withData = sensors.count { it.isNotEmpty() }
    if (withData < 2) throw CalibrationError.TooFewSensors(withData)
    val bin = options.bin
    val all = sensors.flatten()
    val t0 = all.minOf { it.at }
    val t1 = all.maxOf { it.at }
    val span = t1 - t0
    if (span < options.shortestSpan) throw CalibrationError.TooShort(span, options.shortestSpan)

    val rawT = resample(sensors, t0, bin, Quantity.Temperature)
    val settledT = settled(rawT, Quantity.Temperature, options, t0)
    val temperature = settledT.grid
    val temperatureRates = rates(rawT, bin)
    val steady = bin * steadyBinCount(temperature)
    if (steady < options.minSteady) throw CalibrationError.NotEnoughSteadyData(steady, options.minSteady)
    val rawH = resample(sensors, t0, bin, Quantity.Humidity)
    val humidityRates = rates(rawH, bin)
    val settledH = settled(rawH, Quantity.Humidity, options, t0)
    val humidity = settledH.grid.map { row ->
        row.map { cell -> cell?.takeIf { it <= options.maxHumidity } }
    }

    val (tFits, tReport) = fitQuantity(temperature, temperatureRates, Quantity.Temperature, options, t0)
    val humidityFitted = bin * steadyBinCount(humidity) >= options.minSteady
    val (hFits, hReport) = if (humidityFitted) {
        fitQuantity(humidity, humidityRates, Quantity.Humidity, options, t0)
    } else {
        val identity = List(sensors.size) { Fit.IDENTITY }
        identity to quantityReport(humidity, identity, bin, t0)
    }
    options.reference?.let { r ->
        val referenceSteady = min(tFits[r].steady, if (humidityFitted) hFits[r].steady else Double.MAX_VALUE)
        if (referenceSteady < options.minSteady) {
            throw CalibrationError.ReferenceNotSteady(referenceSteady, options.minSteady)
        }
    }
    val goodT = tFits.map { !it.outlier }
    return Report(
        sensors = tFits.zip(hFits) { t, h -> SensorReport(t, h) },
        temperature = tReport.copy(settling = settledT.settling),
        humidity = hReport.copy(settling = settledH.settling),
        phases = phases(temperature, goodT, t0, bin),
    )
}

private fun fitQuantity(
    grid: Grid,
    rates: Grid,
    quantity: Quantity,
    options: Options,
    t0: Double,
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
        val p = pairs(grid, rates, corrected, good, i, itself = false)
        fit.copy(outlier = !good[i], steady = options.bin * p.xs.size)
    }
    // Sensors with too little data neither define nor shift the group average.
    val anchors = fits.map { !it.outlier && it.steady >= options.minSteady }
    val reference = options.reference
    fits = anchor(
        fits,
        when {
            reference != null -> List(sensors) { it == reference }
            anchors.any { it } -> anchors
            else -> good
        },
    )
    if (reference != null) {
        // Others take over the reference's gain; it reads as it does.
        val referenceGain = fits[reference].gainFitted
        fits = fits.mapIndexed { i, fit ->
            // The reference exactly the identity: `g · (1/g)` need not round to 1.
            if (i == reference) fit.copy(gain = 1.0, offset = 0.0, gainFitted = false)
            else fit.copy(gainFitted = fit.gainFitted || referenceGain)
        }
    }
    corrected = correctedGrid(grid, fits)
    val residuals = fits.mapIndexed { i, fit -> residuals(pairs(grid, rates, corrected, good, i, itself = false), fit) }
    val inflation = inflation(residuals.filterIndexed { i, _ -> good[i] })
    fits = fits.mapIndexed { i, fit ->
        val r = residuals[i]
        fit.copy(residualRms = rms(r.values), stderr = stderr(r.values, inflation))
    }
    return fits to quantityReport(grid, fits, options.bin, t0).copy(inflation = inflation)
}

private fun quantityReport(grid: Grid, fits: List<Fit>, bin: Double, t0: Double): QuantityReport {
    val good = fits.map { !it.outlier }
    val consensus = grid.mapNotNull { consensus(it, good) }
    return QuantityReport(
        steady = bin * steadyBinCount(grid),
        range = percentile(consensus, 0.05) to percentile(consensus, 0.95),
        spread = spread(grid, good, fits),
        steadyBins = grid.indices.filter { b -> grid[b].count { it != null } >= 2 }.map { t0 + it * bin },
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

/** A quantity's readings that count, and which sensors are still held back at the end. */
private class Settled(val grid: Grid, val settling: List<Settling?>)

/**
 * Keeps only readings taken while both the group median and the sensor itself stayed
 * within [Quantity.maxChange] over the steady window, and not shortly after a big
 * change of the group median, nor (with [Options.holdUntilSettled]) while the sensor's
 * offset to the others still settles after a change.
 */
private fun settled(grid: Grid, quantity: Quantity, options: Options, t0: Double): Settled {
    val bin = options.bin
    val half = binIndex(options.steadyWindow / 2.0, bin)
    val settle = binIndex(options.settleTime, bin)
    val limit = quantity.maxChange(options)
    val big = quantity.bigChange(options)
    val sensors = grid.firstOrNull()?.size ?: 0
    val allIncluded = List(sensors) { true }
    val group = grid.map { consensus(it, allIncluded) }
    val settling = grid.indices.map { b ->
        range(group.subList(max(b - settle, 0), b + 1)) >= big
    }
    val holds = if (options.holdUntilSettled) List(sensors) { i -> hold(grid, group, i, quantity, options, t0) } else null
    val calmCells = (0 until sensors).map { i ->
        val own = grid.map { it[i] }
        val held = holds?.get(i)?.held
        grid.indices.map { b ->
            !settling[b] && held?.get(b) != true && calm(group, b, half, limit) && calm(own, b, half, limit)
        }
    }
    val kept = grid.mapIndexed { b, row ->
        val masked = row.mapIndexed { i, cell -> cell?.takeIf { calmCells[i][b] } }
        // A bin needs at least two sensors to compare anything.
        if (masked.count { it != null } >= 2) masked else List(row.size) { null }
    }
    // Still settling at the end: until the last bin a big change away leaves the settle window.
    val last = grid.lastIndex
    val fixedUntil = if (last >= 0 && settling[last]) {
        val far = (last downTo max(last - settle, 0)).first { j -> range(group.subList(j, last + 1)) >= big }
        t0 + (far + settle + 1) * bin
    } else {
        null
    }
    val report = List(sensors) { i ->
        val hold = holds?.get(i)?.takeIf { it.atEnd }
        when {
            hold == null && fixedUntil == null -> null
            hold == null -> Settling(fixedUntil)
            hold.until == null -> Settling(null)
            else -> Settling(max(hold.until, fixedUntil ?: hold.until))
        }
    }
    return Settled(kept, report)
}

/** A move shows as half a big change or more within this long, s. */
private const val CHANGE_WINDOW = 30 * 60.0
/** Fastest time constant tried for a sensor's offset settling, s: a few bins. */
private const val FASTEST_SETTLING = 3 * 60.0
/** Fewest bins after a change to tell anything from; until then the sensor is held back. */
private const val SETTLING_POINTS = 4
/** Standard errors by which a decay must stand out of the noise to hold a sensor back. */
private const val SETTLING_SIGNIFICANCE = 3.0
/** The decay is fitted over this many of the slowest time constants after a change. */
private const val SETTLING_HORIZON = 3.0

private fun Quantity.settledWithin(o: Options) = if (this == Quantity.Temperature) o.settledTemperature else o.settledHumidity
private fun Quantity.slowestSettling(o: Options) =
    if (this == Quantity.Temperature) o.slowestTemperatureSettling else o.slowestHumiditySettling

/**
 * Bins [held] back for one sensor; whether it is still held back [atEnd] of the readings and
 * [until] when, if that can be told.
 */
private class Hold(val held: BooleanArray, val atEnd: Boolean, val until: Double?)

/**
 * Where sensor [i] is held back after changes ([Options.holdUntilSettled]).
 *
 * A change is half a big change or more, of the group median or of the sensor itself,
 * within [CHANGE_WINDOW]: the devices were moved, or this one alone (put next to the others,
 * or handled). After it, the sensor's offset to the group median is fitted as
 * `c + A · exp(−t / τ)`, with τ up to [Quantity.slowestSettling]: on the Oct 29 move of 8
 * LYWSD03MMC (21 → 16 °C, 60 → 72 %RH), temperature offsets settled with τ of 2–20 min,
 * humidity ones with 3–75 min, and some crept for hours (one 5 %RH behind after 15 min and
 * 0.5 after 3 h). Bins count once less than [Quantity.settledWithin] is left of the decay. Before
 * [SETTLING_POINTS] bins, the sensor is held back; a decay within [SETTLING_SIGNIFICANCE]
 * standard errors of none holds nothing. Readings before the first change are not tested:
 * offsets wander by a few hundredths over hours, which would look like slow decays.
 */
private fun hold(grid: Grid, group: List<Double?>, i: Int, quantity: Quantity, options: Options, t0: Double): Hold {
    val bin = options.bin
    val n = grid.size
    val own = grid.map { it[i] }
    val window = binIndex(CHANGE_WINDOW, bin)
    val change = quantity.bigChange(options) / 2
    val changing = BooleanArray(n) { b ->
        val from = max(b - window, 0)
        range(group.subList(from, b + 1)) >= change || range(own.subList(from, b + 1)) >= change
    }
    val left = quantity.settledWithin(options)
    val slowest = quantity.slowestSettling(options)
    val held = BooleanArray(n)
    var until: Double? = null
    var b = 1
    while (b < n) {
        if (!changing[b - 1] || changing[b]) {
            b++
            continue
        }
        val start = b
        var end = b
        while (end < n && !changing[end]) end++
        val ts = ArrayList<Double>()
        val ys = ArrayList<Double>()
        for (k in start until end) {
            val x = own[k] ?: continue
            val g = group[k] ?: continue
            val t = (k - start) * bin
            if (t <= SETTLING_HORIZON * slowest) {
                ts += t
                ys += x - g
            }
        }
        val decay = if (ts.size >= SETTLING_POINTS) fitDecay(ts, ys, slowest) else null
        for (k in start until end) {
            held[k] = decay == null || decay.left((k - start) * bin - ts[0]) > left
        }
        if (end == n) {
            // Told only once the readings span one time constant: less is guesswork.
            until = decay?.takeIf { ts.last() - ts[0] >= it.tau }?.let { t0 + start * bin + ts[0] + it.after(left) }
        }
        b = end
    }
    val atEnd = n > 0 && (changing[n - 1] || held[n - 1])
    return Hold(held, atEnd, if (atEnd && n > 0 && !changing[n - 1]) until else null)
}

/** `|amplitude| · exp(−t / tau)` left of a decay `t` s after its first point. */
private class Decay(val tau: Double, val amplitude: Double) {
    fun left(t: Double): Double = abs(amplitude) * exp(-t / tau)

    /** Seconds after the first point until less than [left] is left. */
    fun after(left: Double): Double = if (abs(amplitude) <= left) 0.0 else tau * ln(abs(amplitude) / left)
}

/**
 * Least squares `y ≈ c + A · exp(−(t − t₀) / τ)` over τ from [FASTEST_SETTLING] to [slowest]
 * in one-minute steps; `A` is 0 unless it stands out of the noise by [SETTLING_SIGNIFICANCE].
 */
private fun fitDecay(ts: List<Double>, ys: List<Double>, slowest: Double): Decay {
    val n = ts.size
    var best = Double.POSITIVE_INFINITY
    var bestTau = FASTEST_SETTLING
    var bestA = 0.0
    var bestDet = 0.0
    val sy = ys.sum()
    val syy = ys.sumOf { it * it }
    var tau = FASTEST_SETTLING
    while (tau <= slowest) {
        var se = 0.0
        var see = 0.0
        var sey = 0.0
        for (k in 0 until n) {
            val e = exp(-(ts[k] - ts[0]) / tau)
            se += e
            see += e * e
            sey += e * ys[k]
        }
        val det = n * see - se * se
        if (det > 1e-9) {
            val a = (n * sey - se * sy) / det
            val c = (sy - a * se) / n
            val sse = syy - c * sy - a * sey
            if (sse < best) {
                best = sse
                bestTau = tau
                bestA = a
                bestDet = det
            }
        }
        tau += 60.0
    }
    if (bestDet <= 0.0) return Decay(bestTau, 0.0)
    val variance = max(best, 0.0) / max(n - 3, 1)
    val stderr = sqrt(variance * n / bestDet)
    return Decay(bestTau, if (abs(bestA) >= SETTLING_SIGNIFICANCE * stderr) bestA else 0.0)
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

/**
 * Iteratively fits every sensor against the median of the included sensors, itself among
 * them, as corrected by the previous round; then once more against the others only.
 *
 * Leaving the sensor itself out makes the rounds swing with an even number of sensors:
 * two sensors each take on the other's error and swap every round, so none is ever
 * corrected. With itself in the median, two sensors meet halfway in one round, and larger
 * groups settle as fast. Both settle on the same corrections, but corrections don't
 * include lag, so a median holding the sensor's own reading shares its lag and makes it
 * look smaller (by ~150 s on real data). The last round, starting from settled
 * corrections, measures lag against the others.
 */
private fun fitAll(grid: Grid, rates: Grid, include: List<Boolean>, quantity: Quantity, options: Options): List<Fit> {
    var fits = List(include.size) { Fit.IDENTITY }
    val iterations = 4
    for (round in 0..iterations) {
        val corrected = correctedGrid(grid, fits)
        val itself = round < iterations
        fits = include.indices.map { fitSensor(pairs(grid, rates, corrected, include, it, itself), quantity, options) }
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

/** Readings of one sensor paired with the median of a group of sensors, see [pairs]; [bins] are their grid indices. */
private class Pairs(val xs: List<Double>, val ys: List<Double>, val rates: List<Double?>, val bins: List<Int> = emptyList())

/**
 * Readings of sensor [i] paired with the median of the other included sensors in
 * [reference], in bins where any of them has a value. With [itself], an included sensor's
 * own [reference] value joins that median (see [fitAll]); without it the pairs say how the
 * sensor compares with the rest, as reported.
 */
private fun pairs(
    grid: Grid,
    rates: Grid,
    reference: Grid,
    include: List<Boolean>,
    i: Int,
    itself: Boolean,
): Pairs {
    var others = include.toMutableList().also { it[i] = false }
    if (others.none { it }) {
        // An excluded sensor in a group of two: compare with everyone else.
        others = MutableList(include.size) { it != i }
    }
    val xs = mutableListOf<Double>()
    val ys = mutableListOf<Double>()
    val rs = mutableListOf<Double?>()
    val bins = mutableListOf<Int>()
    for (b in grid.indices) {
        val x = grid[b][i] ?: continue
        val values = reference[b].filterIndexed { j, _ -> others[j] }.filterNotNull().toMutableList()
        if (values.isEmpty()) continue
        val own = reference[b][i]
        if (itself && include[i] && own != null) values += own
        xs += x
        ys += median(values)
        rs += rates[b][i]
        bins += b
    }
    return Pairs(xs, ys, rs, bins)
}

/** Fits one sensor's readings to the medians they are paired with. */
private fun fitSensor(pairs: Pairs, quantity: Quantity, options: Options): Fit {
    var p = pairs
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

/** What the fit (including lag, where modelled) leaves unexplained, per bin. */
private class Residuals(val bins: List<Int>, val values: List<Double>)

private fun residuals(p: Pairs, fit: Fit): Residuals = Residuals(
    p.bins,
    p.xs.indices.map { j ->
        val rate = p.rates[j]
        val lagTerm = if (fit.lag != null && rate != null) fit.gain * fit.lag * rate else 0.0
        fit.apply(p.xs[j]) + lagTerm - p.ys[j]
    },
)

private fun rms(values: List<Double>): Double =
    if (values.isEmpty()) 0.0 else sqrt(values.sumOf { it * it } / values.size)

/**
 * Lags, in bins, over which [inflation] sums the residuals' autocorrelation: 30 min with
 * 5-minute bins, about as long as a room's small drifts and the cases' lag last.
 */
private const val CORRELATION_LAGS = 6

/**
 * Variance inflation of a mean of correlated residuals: `1 + 2 Σ w_k ρ_k` over lags of 1 to
 * [CORRELATION_LAGS] bins, with Bartlett weights `w_k = 1 − k / (K + 1)` that keep it
 * positive. The autocorrelations ρ_k are pooled over sensors, as the room's drifts are
 * shared and a single sensor has too few bins in a short window. At least 1.
 */
private fun inflation(sensors: List<Residuals>): Double {
    var variance = 0.0
    var count = 0
    val products = DoubleArray(CORRELATION_LAGS + 1)
    val pairs = IntArray(CORRELATION_LAGS + 1)
    for (r in sensors) {
        if (r.values.size < 2) continue
        val mean = r.values.average()
        val byBin = HashMap<Int, Double>()
        r.bins.zip(r.values).forEach { (b, v) -> byBin[b] = v - mean }
        for ((b, v) in byBin) {
            variance += v * v
            count++
            for (k in 1..CORRELATION_LAGS) {
                val later = byBin[b + k] ?: continue
                products[k] += v * later
                pairs[k]++
            }
        }
    }
    if (count < 2 || variance <= 0.0) return 1.0
    val perBin = variance / count
    var inflation = 1.0
    for (k in 1..CORRELATION_LAGS) {
        if (pairs[k] == 0) continue
        val rho = products[k] / pairs[k] / perBin
        inflation += 2.0 * (1.0 - k / (CORRELATION_LAGS + 1.0)) * rho
    }
    return max(inflation, 1.0)
}

/** Standard error of the mean of [values], inflated for correlation; `null` with fewer than two. */
private fun stderr(values: List<Double>, inflation: Double): Double? {
    if (values.size < 2) return null
    val mean = values.average()
    val variance = values.sumOf { (it - mean) * (it - mean) } / (values.size - 1)
    return sqrt(inflation * variance / values.size)
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
