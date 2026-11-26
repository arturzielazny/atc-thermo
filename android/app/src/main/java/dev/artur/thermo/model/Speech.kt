/*
 * What the app says aloud during a calibration, for a phone lying next to the thermometers
 * while nobody looks at it: news as it comes (a level reached, a device astray, the writes
 * done), otherwise the progress about four times an hour. Plain words for text-to-speech:
 * no symbols, a decimal point, devices by their number. [ThermoModel.takeSpeech] decides
 * when; the app speaks it.
 */
package dev.artur.thermo.model

import kotlin.math.abs

/** Progress is said this long after the last thing said: about four times an hour, as the user asked. */
const val SPEAK_EVERY_MS = 15 * 60_000L

/** News waits until this long after the last thing said, so that a burst of it comes as one. */
const val SPEAK_GAP_MS = 2 * 60_000L

/** A device astray is said once it has been so this long: picking one up for a moment isn't news. */
const val STRAY_SAID_AFTER_MS = 2 * 60_000L

/** The same device astray the same way is said again after this long at the earliest. */
const val STRAY_SAID_AGAIN_MS = 3600_000L

/** What was said about a session, and the news waiting to be said. */
internal class Spoken(
    /** When something was last said; `null` before anything. */
    var at: Long?,
    /** The stage of the last fit that worked: the first one after starting, and after writing, is news. */
    var fitted: Stage? = null,
) {
    val news = mutableListOf<String>()
    /** By [Stray.key]: since when each device is astray, and when that was last said. */
    val strayFrom = HashMap<String, Long>()
    val straySaid = HashMap<String, Long>()
    /** Whether devices were held back after the move, and whether their catching up was said. */
    var settling = false
    var caughtUpSaid = false
    /** Corrections sent; how it went is said once none is pending. */
    var writes = 0
    /** Whether the salt container's settling was said. */
    var saltSaid = false
}

internal fun sayStarted(devices: Int): String =
    "Calibrating ${sayCount(devices)}. Progress will be spoken about four times an hour; the speaker button turns it off."

/** The session's progress in a sentence or three, or `null` while there is nothing to tell. */
internal fun sayProgress(session: CalibrationSession, number: (String) -> Int?): String? {
    if (session.pending.isNotEmpty()) return "Writing the corrections to ${sayCount(session.pending.size)}."
    val result = session.result ?: return null
    val run = result.getOrElse { error ->
        val first = if (session.stage == Stage.Checking) "check" else "estimate"
        return firstEstimateIn(error)?.let { "Collecting readings: a first $first in about ${sayDuration(it)}." }
    }
    return when (session.stage) {
        Stage.FirstPlace -> sayFirstPlace(session, run)
        Stage.SecondPlace -> saySecondPlace(session, run, number)
        Stage.Checking -> sayChecking(session, run, number)
    }
}

private fun sayFirstPlace(session: CalibrationSession, run: CalibrationRun): String? {
    val precision = sayPrecision(precision(run)) ?: return null
    val best = Level.entries.lastOrNull { eta(run, it) == Eta.Reached }
    val now = if (best != null) "${best.label} so far: $precision." else "So far $precision."
    val target = session.target.label
    if (saltPending(session, run)) return now + " " + saySaltSettling(run.salt)
    val next = when (val eta = eta(run, session.target)) {
        Eta.Reached -> " Ready to write, or to move them to a second place for the gains."
        is Eta.In -> " $target in about ${sayDuration(eta.ms)}."
        Eta.Later -> " $target would take over ${sayDuration(ETA_SURE_MS)}, if it comes at all."
        Eta.Unlikely -> " $target is unlikely here."
        null -> ""
    }
    return now + next
}

private fun saySecondPlace(session: CalibrationSession, run: CalibrationRun, number: (String) -> Int?): String {
    val steady = run.places.getOrNull(1)?.steady ?: 0
    val parts = mutableListOf(
        if (steady >= 60_000) "Second place: ${sayDuration(steady, roundUp = false)} of steady readings." else "Second place: no steady readings yet.",
    )
    for (temperature in listOf(true, false)) {
        val held = heldBack(run, temperature)?.mapNotNull { (i, _) -> run.devices.getOrNull(i)?.first?.let(number) } ?: continue
        if (held.isNotEmpty()) parts += "${if (temperature) "Temperature" else "Humidity"} of ${sayNumbers(held)} still settling."
    }
    val gain = gainStatus(run).uncertainty
    if (saltPending(session, run)) parts += saySaltSettling(run.salt)
    parts += when {
        targetReached(session, run) -> "Gains ready" + (gain?.let { ", plus or minus ${sayPercent(it * 100)}" } ?: "") + ": write the corrections."
        gain != null -> "Gains so far plus or minus ${sayPercent(gain * 100)}."
        else -> "No gains yet."
    }
    return parts.joinToString(" ")
}

private fun sayChecking(session: CalibrationSession, run: CalibrationRun, number: (String) -> Int?): String {
    val (t, h) = leftToCorrect(run)
    val left = listOfNotNull(t?.let { "%.2f degrees".fmt(it) }, h?.let { "${sayPercent(it)} humidity" }).joinToString(" and ")
    val parts = mutableListOf(if (left.isEmpty()) "Corrections written." else "Left to correct after writing: up to $left.")
    val failed = session.failed.keys.mapNotNull(number)
    if (failed.isNotEmpty()) parts += "Not written to ${sayNumbers(failed)} yet."
    return parts.joinToString(" ")
}

/** When the session's target was reached: the moment to write, or to move them for the gains. */
internal fun sayReached(session: CalibrationSession, run: CalibrationRun): String {
    val salt = run.salt?.takeIf { session.salt == currentPlace(session) && it.shift != null }?.let { " " + saySaltSettled(it) } ?: ""
    return if (session.stage == Stage.SecondPlace) {
        val gain = gainStatus(run).uncertainty?.let { ", plus or minus ${sayPercent(it * 100)}" } ?: ""
        "Gains ready$gain.$salt Write the corrections."
    } else {
        val precision = sayPrecision(precision(run))?.let { ": $it" } ?: ""
        "${session.target.label} reached$precision.$salt Write the corrections, or move the thermometers to a second place for the gains."
    }
}

/** The salt container still settling: how fast its humidity drifts. */
internal fun saySaltSettling(salt: Salt?): String {
    val rate = salt?.rate ?: return "Waiting for the salt: it counts after ${sayDuration(SALT_RATE_MS)} of readings in the box."
    return "Waiting for the salt: humidity still %s %.${rateDecimals(rate)}f percent an hour.".fmt(if (rate >= 0) "rising" else "falling", abs(rate))
}

/** The salt container settled: what that does to the humidity. */
internal fun saySaltSettled(salt: Salt): String {
    val shift = salt.shift ?: return ""
    return "The salt has settled: humidity corrections %s by %.1f percent.".fmt(if (shift >= 0) "raised" else "lowered", abs(shift))
}

/** After the writes: how many went through. */
internal fun sayWritten(writes: Int, failed: Int, failedNumbers: List<Int>): String {
    val written = writes - failed
    val which = if (failedNumbers.size == failed) sayNumbers(failedNumbers) else sayCount(failed)
    return when {
        failed == 0 -> "Corrections written to ${sayCount(written)}. Leave them together a while to check."
        written == 0 -> "The corrections were not written. Retry when the thermometers are in range."
        else -> "Corrections written to ${sayCount(written)}, not to $which: retry when ${if (failed == 1) "it is" else "they are"} in range."
    }
}

/** Devices not heard lately, and devices apart from the others. */
internal fun sayStrays(strays: List<Stray>, number: (String) -> Int?): List<String> {
    val silent = strays.filter { it.offset == null }.mapNotNull { number(it.address) }
    val said = mutableListOf<String>()
    if (silent.isNotEmpty()) said += "${sayNumbers(silent).replaceFirstChar(Char::uppercase)} not heard for a while: still in range?"
    for (stray in strays) {
        val offset = stray.offset ?: continue
        val n = number(stray.address) ?: continue
        said += "Number $n reads %.1f degrees %s than the others: still next to them?".fmt(abs(offset), if (offset > 0) "warmer" else "colder")
    }
    return said
}

/** "plus or minus 0.04 degrees and 0.4 percent humidity", or `null` with neither. */
internal fun sayPrecision(p: Precision): String? =
    listOfNotNull(p.temperature?.let { "%.2f degrees".fmt(it) }, p.humidity?.let { "${sayPercent(it)} humidity" })
        .ifEmpty { null }
        ?.let { "plus or minus " + it.joinToString(" and ") }

/** "0.2 percent", "0.05 percent": two significant digits below 0.1. */
private fun sayPercent(percent: Double): String = (if (percent < 0.095) "%.2f percent" else "%.1f percent").fmt(percent)

/** "1 thermometer", "5 thermometers". */
internal fun sayCount(n: Int): String = if (n == 1) "1 thermometer" else "$n thermometers"

/** "number 3", "numbers 2, 4 and 7". */
internal fun sayNumbers(numbers: List<Int>): String =
    if (numbers.size == 1) "number ${numbers[0]}" else "numbers " + numbers.dropLast(1).joinToString(", ") + " and " + numbers.last()

/** "25 minutes", "1 hour 40 minutes": rounded up and beyond an hour to 5 minutes like [formatEta], or down. */
internal fun sayDuration(ms: Long, roundUp: Boolean = true): String {
    val minutes = (if (roundUp) (ms + 59_999) / 60_000 else ms / 60_000).coerceAtLeast(1)
    if (minutes < 60) return plural(minutes, "minute")
    val rounded = if (roundUp) (minutes + 4) / 5 * 5 else minutes / 5 * 5
    return plural(rounded / 60, "hour") + if (rounded % 60 != 0L) " " + plural(rounded % 60, "minute") else ""
}

private fun plural(n: Long, unit: String) = if (n == 1L) "1 $unit" else "$n ${unit}s"
