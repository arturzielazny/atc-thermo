/*
 * A calibration session of chosen devices, from putting them side by side to writing and
 * checking corrections: how precise corrections written now would be, how long a better
 * precision takes, and what to do next (stay, write, or move them for the gains).
 */
package dev.artur.thermo.model

import dev.artur.thermo.atc.Correction
import dev.artur.thermo.calibration.CalibrationError
import dev.artur.thermo.calibration.Fit
import dev.artur.thermo.calibration.Reading
import dev.artur.thermo.calibration.median
import kotlin.math.abs
import kotlin.math.max
import kotlin.math.sqrt

/**
 * Error that more time in one place doesn't remove, one standard error, in °C and %RH.
 * `PrecisionDataTest` fitted windows of two indoor sessions (8 and 6 devices, 10 and 18 h)
 * and compared each with the rest of its session at a similar temperature: the differences
 * shrank little with longer windows and needed 0.007–0.011 °C and 0.014–0.017 %RH on top of
 * the statistical error. Slow drifts, heaters cycling and the cases' different lags.
 */
const val TEMPERATURE_FLOOR = 0.009
const val HUMIDITY_FLOOR = 0.02

/**
 * How much [Fit.stderr] underestimates the statistical error: 0.6–1.8 across those sessions
 * and both quantities, as correlations longer than its 30 minutes are missed.
 */
const val STDERR_SCALE = 1.5

/** Temperature gains are fitted only with steady data at least this far apart, °C (`Options.minTemperatureSpan`). */
const val GAIN_SPAN_T = 5.0
/** A second place this far from the first gives a gain twice as precise as the minimum does. */
const val GOOD_GAIN_SPAN_T = 10.0
/** Same for humidity, %RH (`Options.minHumiditySpan`). */
const val GAIN_SPAN_H = 15.0
/** Humidity above this is left out of fits (`Options.maxHumidity`), so a second place should stay below it. */
const val MAX_FITTED_HUMIDITY = 95.0

/**
 * Typical difference of these sensors' temperature gains from each other: the 8 LYWSD03MMC
 * fitted here on Oct 29 had gains of 0.989–1.011. With offsets only, readings this many °C
 * per °C away from the calibration temperature can drift apart.
 */
const val TYPICAL_GAIN_ERROR = 0.01

/** Steady readings worth having in the second place before a gain is trusted. */
const val SECOND_PLACE_STEADY_MS = 30 * 60_000L
/** A device not heard for this long during a session is probably out of range. */
const val SESSION_SILENT_MS = 5 * 60_000L
/** A device this far from the others' median is probably not next to them. */
const val STRAY_TEMPERATURE = 1.5
/** Devices read alike if within this of each other's median, °C and %RH; see [togetherSince]. */
const val TOGETHER_TEMPERATURE = 1.0
const val TOGETHER_HUMIDITY = 6.0
/** Bins of [togetherSince]: the fit's. */
private const val TOGETHER_BIN_MS = 5 * 60_000L

/** Precision to aim for: about ± two standard errors of each correction, °C and %RH. */
enum class Level(val label: String, val temperature: Double, val humidity: Double) {
    Rough("Rough", 0.10, 1.0),
    Good("Good", 0.05, 0.5),
    Fine("Fine", 0.03, 0.3),
    Best("Best", 0.02, 0.2),
}

/** Where a session is. */
enum class Stage {
    /** Together in one place: offsets. */
    FirstPlace,
    /** Moved to a second, different place: gains. */
    SecondPlace,
    /** Corrections written; the devices stay together to show they now agree. */
    Checking,
}

/** The calibration of a set of devices. Times in epoch ms. */
class CalibrationSession(
    /** The devices calibrated, in list order. */
    devices: List<String>,
    /** The device the others are corrected to match, or `null` for their average. */
    var reference: String?,
    /** First moment of data to fit; never before the devices' last calibration. */
    var start: Long,
    /** When the session began. */
    val began: Long = start,
) {
    var devices: List<String> = devices
        internal set
    /** When the devices were moved to a second place, for gains; `null` while in the first. */
    var moved: Long? = null
        internal set
    /** The precision aimed for; reaching it is announced once. */
    var target: Level = Level.Fine
        internal set
    /** When corrections were last written. */
    var written: Long? = null
        internal set
    /** Whether reaching [target] was announced. */
    internal var announced = false
    /**
     * The place the devices are or were in a sealed container over saturated salt: 0 the
     * first, 1 the second; `null` if neither. Its humidity is then corrected to the salt's.
     */
    var salt: Int? = null
        internal set

    var result: Result<CalibrationRun>? = null
        internal set
    var computing = false
        internal set
    var computedAt: Long? = null
        internal set
    /** The devices of the last fit; a change (e.g. history loaded) refits right away. */
    internal var fitted: List<String>? = null

    /** Corrections queued and not yet confirmed, and those that failed, by address. */
    val pending = LinkedHashMap<String, Pair<Correction, Correction>>()
    val failed = LinkedHashMap<String, Pair<Correction, Correction>>()

    val stage: Stage get() = when {
        written != null -> Stage.Checking
        moved != null -> Stage.SecondPlace
        else -> Stage.FirstPlace
    }

    /** The settings that outlive the app, as `key=value` lines. */
    fun encode(): String = buildString {
        appendLine("devices=${devices.joinToString(",")}")
        appendLine("reference=${reference ?: ""}")
        appendLine("start=$start")
        appendLine("began=$began")
        appendLine("moved=${moved ?: ""}")
        appendLine("target=${target.name}")
        appendLine("written=${written ?: ""}")
        appendLine("announced=$announced")
        appendLine("salt=${salt ?: ""}")
    }

    companion object {
        /** A session from [encode], or `null` if [text] isn't one. */
        fun decode(text: String): CalibrationSession? {
            val values = text.lines().mapNotNull { line ->
                val i = line.indexOf('=')
                if (i < 0) null else line.substring(0, i) to line.substring(i + 1)
            }.toMap()
            val devices = values["devices"]?.split(',')?.filter { it.isNotBlank() } ?: return null
            if (devices.size < 2) return null
            val start = values["start"]?.toLongOrNull() ?: return null
            return CalibrationSession(
                devices,
                values["reference"]?.takeIf { it in devices },
                start,
                values["began"]?.toLongOrNull() ?: start,
            ).apply {
                moved = values["moved"]?.toLongOrNull()
                target = Level.entries.firstOrNull { it.name == values["target"] } ?: Level.Fine
                written = values["written"]?.toLongOrNull()
                announced = values["announced"] == "true"
                salt = values["salt"]?.toIntOrNull()?.takeIf { it in 0..1 }
            }
        }
    }
}

/** Conditions in one place: group medians, and how much of it was steady enough to fit. */
data class Place(
    val since: Long,
    val until: Long,
    /** Median of all readings there, °C and %RH. */
    val temperature: Double,
    val humidity: Double,
    /** Median of the last 30 min there, as they still settle after a move. */
    val latestTemperature: Double,
    val latestHumidity: Double,
    /** Time with steady temperature readings fitted, ms. */
    val steady: Long,
)

/** When a precision is reached. */
sealed interface Eta {
    data object Reached : Eta
    data class In(val ms: Long) : Eta
    /** Further off than [ETA_SURE_MS]: it may come, or not. */
    data object Later : Eta
    /** The error that time doesn't remove is larger, or it would take more than [ETA_LIMIT_MS]. */
    data object Unlikely : Eta
}

/** Estimates further out than this aren't worth stating. */
const val ETA_LIMIT_MS = 12 * 3600_000L

/**
 * Estimates further out than this are only "later": replaying two real sessions
 * (`SessionReplayTest`), every time under 2 h given for a level came true, mostly within
 * ±50 %, but of longer ones all came true in one session and 1 of 140 in the other: the
 * square-root law assumes the next hours are as steady as the last, and rooms often aren't.
 */
const val ETA_SURE_MS = 2 * 3600_000L

/** ± two standard errors of the corrections that would be written, typical over the devices. */
data class Precision(val temperature: Double?, val humidity: Double?)

/** The error of one quantity, one standard error, split into what time reduces and what it doesn't. */
internal class ErrorParts(val floor: Double, val statistical: Double, val steadySeconds: Double) {
    val total: Double get() = sqrt(floor * floor + statistical * statistical)
}

private fun floorOf(temperature: Boolean) = if (temperature) TEMPERATURE_FLOOR else HUMIDITY_FLOOR

private fun fitOf(run: CalibrationRun, i: Int, temperature: Boolean): Fit? =
    run.report.sensors.getOrNull(i)?.let { if (temperature) it.temperature else it.humidity }

/** Indices of the devices whose correction would be written. */
private fun CalibrationRun.writtenIndices(): List<Int> = devices.indices.filter { i ->
    val (t, h) = corrections(i)
    t != null || h != null
}

/**
 * One device's error for one quantity, one standard error: its own, and with a reference
 * also the reference's, which the correction is relative to. `null` if not fitted.
 */
internal fun deviceError(run: CalibrationRun, i: Int, temperature: Boolean): ErrorParts? {
    val fit = fitOf(run, i, temperature) ?: return null
    val se = fit.stderr ?: return null
    val floor = floorOf(temperature)
    val referenceIndex = run.devices.indexOfFirst { it.first == run.reference }.takeIf { it >= 0 && it != i }
    val referenceSe = referenceIndex?.let { fitOf(run, it, temperature)?.stderr } ?: 0.0
    val copies = if (referenceIndex != null) 2.0 else 1.0
    val steady = if (temperature) run.report.temperature.steady else run.report.humidity.steady
    return ErrorParts(
        floor = floor * sqrt(copies),
        statistical = STDERR_SCALE * sqrt(se * se + referenceSe * referenceSe),
        steadySeconds = steady,
    )
}

/** The errors of the written devices combined: root mean square of each part. */
internal fun groupError(run: CalibrationRun, temperature: Boolean): ErrorParts? {
    val parts = run.writtenIndices().mapNotNull { deviceError(run, it, temperature) }
    if (parts.isEmpty()) return null
    fun rms(f: (ErrorParts) -> Double) = sqrt(parts.sumOf { f(it) * f(it) } / parts.size)
    return ErrorParts(rms { it.floor }, rms { it.statistical }, parts.first().steadySeconds)
}

/** ± two standard errors of the device at [i]'s correction, or `null` if not fitted. */
fun deviceUncertainty(run: CalibrationRun, i: Int, temperature: Boolean): Double? =
    deviceError(run, i, temperature)?.let { 2 * it.total }

fun precision(run: CalibrationRun): Precision =
    Precision(groupError(run, true)?.let { 2 * it.total }, groupError(run, false)?.let { 2 * it.total })

/**
 * When ± [wanted] (two standard errors) is reached: the statistical error shrinks with the
 * square root of the steady time, which grows by [steadyFraction] of the time that passes.
 */
internal fun eta(parts: ErrorParts, wanted: Double, steadyFraction: Double): Eta {
    val half = wanted / 2
    if (parts.total <= half) return Eta.Reached
    val room = half * half - parts.floor * parts.floor
    if (room <= 0) return Eta.Unlikely
    val needed = parts.statistical * parts.statistical * parts.steadySeconds / room
    val ms = ((needed - parts.steadySeconds) / steadyFraction.coerceIn(0.25, 1.0) * 1000).toLong()
    return if (ms > ETA_LIMIT_MS) Eta.Unlikely else Eta.In(ms)
}

/** When [level] is reached in both quantities, or `null` before a first fit. */
fun eta(run: CalibrationRun, level: Level): Eta? {
    val elapsed = (run.until - run.since) / 1000.0
    val results = listOf(true, false).mapNotNull { temperature ->
        val parts = groupError(run, temperature) ?: return@mapNotNull null
        eta(parts, if (temperature) level.temperature else level.humidity, parts.steadySeconds / max(elapsed, 1.0))
    }
    if (results.isEmpty()) return null
    return when {
        results.any { it == Eta.Unlikely } -> Eta.Unlikely
        results.all { it == Eta.Reached } -> Eta.Reached
        else -> results.filterIsInstance<Eta.In>().maxOf { it.ms }.let { if (it > ETA_SURE_MS) Eta.Later else Eta.In(it) }
    }
}

/** How long until a first estimate, from why a fit failed; `null` if more time won't help. */
fun firstEstimateIn(error: Throwable): Long? = when (error) {
    // No readings since the start yet: all of it.
    is CalibrationError.TooFewSensors -> (SESSION_OPTIONS.shortestSpan * 1000).toLong()
    is CalibrationError.TooShort -> ((error.needed - error.span) * 1000).toLong().coerceAtLeast(60_000)
    is CalibrationError.NotEnoughSteadyData -> ((error.needed - error.steady) * 1000).toLong().coerceAtLeast(60_000)
    is CalibrationError.ReferenceNotSteady -> ((error.needed - error.steady) * 1000).toLong().coerceAtLeast(60_000)
    else -> null
}

/** What the gains are like in a fit: whether fitted, and how precise, as a fraction of 1. */
data class GainStatus(
    /** Devices whose temperature gain was fitted, of those written. */
    val fitted: Int,
    val of: Int,
    /** Two standard errors of the fitted temperature gains, typical; `null` if none fitted. */
    val uncertainty: Double?,
    /** Whether humidity gains were fitted for any device. */
    val humidity: Boolean,
    /** Range of the steady temperatures, 5th to 95th percentile, °C. */
    val span: Double,
)

fun gainStatus(run: CalibrationRun): GainStatus {
    val written = run.writtenIndices().filterNot(run::isReference)
    val fits = written.mapNotNull { fitOf(run, it, true) }
    val range = run.report.temperature.range
    val span = range.second - range.first
    val inflation = sqrt(run.report.temperature.inflation)
    val fitted = fits.filter { it.gainFitted }
    val uncertainty = if (fitted.isEmpty() || span <= 0) null else {
        // Statistical, and the offset floor at both ends of the span.
        val parts = fitted.map {
            val statistical = STDERR_SCALE * it.gainStderr * inflation
            val floor = sqrt(2.0) * TEMPERATURE_FLOOR / span
            statistical * statistical + floor * floor
        }
        2 * sqrt(parts.average())
    }
    val humidity = written.any { fitOf(run, it, false)?.gainFitted == true }
    return GainStatus(fitted.size, written.size, uncertainty, humidity, span)
}

/**
 * Whether [session]'s target is reached: precision, in a second place also the gains, and in
 * a salt container also the salt settled.
 */
internal fun targetReached(session: CalibrationSession, run: CalibrationRun): Boolean {
    if (eta(run, session.target) != Eta.Reached) return false
    if (saltPending(session, run)) return false
    if (session.stage != Stage.SecondPlace) return true
    val second = run.places.getOrNull(1) ?: return false
    return second.steady >= SECOND_PLACE_STEADY_MS && gainStatus(run).fitted > 0
}

/** The index of the place the devices are in: 0 or 1; `null` once written. */
fun currentPlace(session: CalibrationSession): Int? = when (session.stage) {
    Stage.FirstPlace -> 0
    Stage.SecondPlace -> 1
    Stage.Checking -> null
}

/** Whether the devices are in the salt container and it hasn't settled yet: writing now would miss it. */
fun saltPending(session: CalibrationSession, run: CalibrationRun): Boolean =
    session.salt != null && session.salt == currentPlace(session) && run.salt?.shift == null

/**
 * The largest correction any device still needs in the middle of the range fitted, °C and
 * %RH: after writing, what is left should stay small.
 */
fun leftToCorrect(run: CalibrationRun): Pair<Double?, Double?> {
    val t = run.report.temperature.range.let { (it.first + it.second) / 2 }
    val h = run.report.humidity.range.let { (it.first + it.second) / 2 }
    return run.report.sensors.maxOfOrNull { abs(netCorrection(it.temperature, t)) } to
        run.report.sensors.maxOfOrNull { abs(netCorrection(it.humidity, h)) }
}

/** Readings that would take the devices to a place different enough for gains. */
data class SecondPlaceAdvice(
    val colderThan: Double,
    val warmerThan: Double,
    val betterColderThan: Double,
    val betterWarmerThan: Double,
    /** Humidity limits for its gain; `null` where out of the range fitted. */
    val drierThan: Double?,
    val moreHumidThan: Double?,
)

fun secondPlaceAdvice(first: Place): SecondPlaceAdvice = SecondPlaceAdvice(
    colderThan = first.temperature - GAIN_SPAN_T,
    warmerThan = first.temperature + GAIN_SPAN_T,
    betterColderThan = first.temperature - GOOD_GAIN_SPAN_T,
    betterWarmerThan = first.temperature + GOOD_GAIN_SPAN_T,
    drierThan = (first.humidity - GAIN_SPAN_H).takeIf { it > 5 },
    moreHumidThan = (first.humidity + GAIN_SPAN_H).takeIf { it < MAX_FITTED_HUMIDITY - 3 },
)

/**
 * Since when [addresses] have read alike: walking back from [now] in 5-minute bins while
 * each device present is within [TOGETHER_TEMPERATURE] and [TOGETHER_HUMIDITY] of the
 * bins' median, and at least two are present (short gaps are bridged). Never before
 * [earliest]. [now] itself if they don't read alike now.
 */
fun togetherSince(devices: Devices, addresses: List<String>, now: Long, earliest: Long): Long {
    val histories = addresses.mapNotNull { devices[it] }
    var end = now
    var since = now
    var empty = 0
    while (end - TOGETHER_BIN_MS >= earliest) {
        val begin = end - TOGETHER_BIN_MS
        val means = histories.mapNotNull { d ->
            val from = d.partitionPoint { it.at < begin }
            val to = d.partitionPoint { it.at < end }
            val live = d.history.subList(from, to).filter { it.source == Source.Live }
            if (live.isEmpty()) null else live.map { it.temperature.toDouble() }.average() to live.map { it.humidity.toDouble() }.average()
        }
        if (means.size < 2) {
            // Up to 15 min without two devices heard is bridged; longer ends the stretch.
            if (++empty > 3) break
        } else {
            empty = 0
            val t = median(means.map { it.first })
            val h = median(means.map { it.second })
            if (means.any { abs(it.first - t) > TOGETHER_TEMPERATURE || abs(it.second - h) > TOGETHER_HUMIDITY }) break
            since = begin
        }
        end = begin
    }
    return since
}

/** Medians of [readings] (time in s) by place: before [moved] and after, from [since] to [until] (ms). */
fun places(
    readings: List<List<Reading>>,
    steadyBins: List<Double>,
    bin: Double,
    since: Long,
    until: Long,
    moved: Long?,
): List<Place> {
    val bounds = if (moved != null && moved in since until until) listOf(since to moved, moved to until) else listOf(since to until)
    val all = readings.flatten()
    return bounds.mapNotNull { (from, to) ->
        val inside = all.filter { it.at * 1000 >= from && it.at * 1000 < to }
        if (inside.isEmpty()) return@mapNotNull null
        val latestFrom = to - 30 * 60_000L
        val latest = inside.filter { it.at * 1000 >= latestFrom }.ifEmpty { inside }
        Place(
            since = from,
            until = to,
            temperature = median(inside.map { it.temperature }),
            humidity = median(inside.map { it.humidity }),
            latestTemperature = median(latest.map { it.temperature }),
            latestHumidity = median(latest.map { it.humidity }),
            steady = (steadyBins.count { it * 1000 >= from && it * 1000 < to } * bin * 1000).toLong(),
        )
    }
}

/**
 * Devices of a fit still held back at the end of its readings while they catch up with the
 * others after a change ([dev.artur.thermo.calibration.Settling]): `(index in
 * [CalibrationRun.devices], when its readings are expected to count in ms or null if that
 * can't be told yet)`; `null` if none.
 */
fun heldBack(run: CalibrationRun, temperature: Boolean): List<Pair<Int, Long?>>? {
    val settling = if (temperature) run.report.temperature.settling else run.report.humidity.settling
    return settling.withIndex()
        .mapNotNull { (i, s) -> s?.let { i to it.until?.let { until -> (until * 1000).toLong() } } }
        .ifEmpty { null }
}

/**
 * A device in a session that seems not to be with the others, and why: not heard lately, or
 * reading [offset] °C from the others' median.
 */
data class Stray(val address: String, val reason: String, val offset: Double? = null) {
    /** The same device astray the same way. */
    internal val key: String get() = if (offset == null) "$address silent" else "$address apart"
}

/** `"4 min"`, `"2 h"`: how long ago, rounded down. */
fun formatAge(ms: Long): String {
    val minutes = (ms / 60_000).coerceAtLeast(0)
    return if (minutes < 90) "$minutes min" else "${minutes / 60} h"
}

/** `"5 min"`, `"1 h 40 min"`: rounded up, and to 5 minutes beyond an hour. */
fun formatEta(ms: Long): String {
    val minutes = ((ms + 59_999) / 60_000).coerceAtLeast(1)
    if (minutes < 60) return "$minutes min"
    val rounded = (minutes + 4) / 5 * 5
    return "${rounded / 60} h" + if (rounded % 60 != 0L) " ${rounded % 60} min" else ""
}
