/*
 * Runs the consensus calibration on recorded history; a port of
 * `crates/thermo/src/calibrate.rs`.
 */
package dev.artur.thermo.model

import dev.artur.thermo.atc.Correction
import dev.artur.thermo.calibration.Fit
import dev.artur.thermo.calibration.Options
import dev.artur.thermo.calibration.Reading
import dev.artur.thermo.calibration.Report
import dev.artur.thermo.calibration.SensorReport
import dev.artur.thermo.calibration.calibrate

/**
 * Fit options for a calibration session: a steadiness window of 20 min instead of 30 and
 * 20 min of steady data instead of an hour, so a first result comes after about 40
 * minutes and the precision estimate, not a fixed minimum, says whether it is good enough.
 * On real data (indoor sessions of 8 and 6 devices) 45-minute windows came within 0.011
 * and 0.024 °C RMS of fits over 10 and 18 hours.
 *
 * After a move each device also waits until it has caught up with the others
 * ([Options.holdUntilSettled]). On the Oct 29 move, the hour after it was enough for
 * temperatures (offsets in the next 15 min were within 0.005 °C of the following half hour,
 * as without a move) but not for humidity: 0.31 %RH RMS and 0.8 at worst, against 0.07 with
 * each device held back until settled, and nothing held back in 26 h without a move.
 */
val SESSION_OPTIONS = Options(steadyWindow = 20 * 60.0, minSteady = 20 * 60.0, holdUntilSettled = true)

/** A fit for some devices over a time window. */
data class CalibrationRun(
    /** `(address, name)` per sensor, in [Report.sensors] order. */
    val devices: List<Pair<String, String>>,
    val since: Long,
    val until: Long,
    val report: Report,
    /** The device the others were corrected to match, or `null` for the group average. */
    val reference: String? = null,
    /** Conditions in each place of the session: one, or two once the devices were moved. */
    val places: List<Place> = emptyList(),
) {
    fun isReference(index: Int): Boolean = reference != null && devices.getOrNull(index)?.first == reference

    /** The corrections to write to the device at [index]: none for the reference, which defines what is correct. */
    fun corrections(index: Int): Pair<Correction?, Correction?> =
        if (isReference(index)) null to null else report.sensors.getOrNull(index)?.let(::corrections) ?: (null to null)
}

/**
 * The window start: [requested], but never before the last coefficient change of a
 * participating device, since older data was recorded with other coefficients.
 */
fun windowStart(requested: Long, devices: List<String>, lastCalibrations: Map<String, Long>): Long =
    devices.mapNotNull { lastCalibrations[it] }.fold(requested) { start, changed -> maxOf(start, changed) }

/**
 * Fits corrections for [addresses] using their live samples in `since..until`, so they
 * agree with their average or, with a [reference] among them, with that device.
 *
 * @throws dev.artur.thermo.calibration.CalibrationError with too few devices or too
 * little steady data, also from the reference.
 */
fun runCalibration(
    devices: Devices,
    addresses: List<String>,
    since: Long,
    until: Long,
    reference: String? = null,
    moved: Long? = null,
): CalibrationRun {
    val (names, readings) = calibrationReadings(devices, addresses, since, until)
    return fitCalibration(names, readings, since, until, reference, moved)
}

/** Fits [readings] from [calibrationReadings]; safe on any thread. */
fun fitCalibration(
    names: List<Pair<String, String>>,
    readings: List<List<Reading>>,
    since: Long,
    until: Long,
    reference: String?,
    moved: Long? = null,
): CalibrationRun {
    // A reference that isn't among the devices (never heard) leaves the average.
    val index = names.indexOfFirst { it.first == reference }.takeIf { it >= 0 }
    val report = calibrate(readings, SESSION_OPTIONS.copy(reference = index))
    val places = places(readings, report.temperature.steadyBins, SESSION_OPTIONS.bin, since, until, moved)
    return CalibrationRun(names, since, until, report, reference.takeIf { index != null }, places)
}

/**
 * `(address, name)` and live readings in `since..until` of each of [addresses]: a copy
 * that can be fitted on another thread.
 */
fun calibrationReadings(
    devices: Devices,
    addresses: List<String>,
    since: Long,
    until: Long,
): Pair<List<Pair<String, String>>, List<List<Reading>>> {
    val names = mutableListOf<Pair<String, String>>()
    val readings = mutableListOf<List<Reading>>()
    for (address in addresses) {
        val device = devices[address] ?: continue
        val from = device.partitionPoint { it.at < since }
        val to = device.partitionPoint { it.at <= until }
        readings += device.history.subList(from, to)
            .filter { it.source == Source.Live }
            .map { Reading(it.at / 1000.0, it.temperature.toDouble(), it.humidity.toDouble()) }
        names += address to device.name
    }
    return names to readings
}

/**
 * The temperature and humidity corrections to write for a device; `null` for one that
 * should not be applied. An outlier in either quantity skips both: the sensor reads
 * both in the same spot, so whatever set it apart skews the other fit too.
 */
fun corrections(sensor: SensorReport): Pair<Correction?, Correction?> {
    if (sensor.temperature.outlier || sensor.humidity.outlier) return null to null
    return correction(sensor.temperature) to correction(sensor.humidity)
}

/** The correction for one fit, or `null` with too little data for this device. */
private fun correction(fit: Fit): Correction? =
    if (fit.steady >= SESSION_OPTIONS.minSteady) Correction(fit.gain, fit.offset) else null

/** Why a device's corrections are not (all) written, or empty. */
fun notes(t: Fit, h: Fit): String = buildList {
    if (t.outlier) add("temperature outlier")
    if (h.outlier) add("humidity outlier")
    if (correction(t) == null && !t.outlier) add("too little data")
}.joinToString(", ")

/** Temperature lag behind the group, e.g. `"+2.5 min"`, or `"–"` if not estimated. */
fun formatLag(lag: Double?): String = lag?.let { "%+.1f min".fmt(it / 60.0) } ?: "–"

/**
 * What a fit adds at [at], typically the middle of the covered range: with a gain the
 * offset alone can mislead (gain 1.01 and offset −0.22 nearly cancel at 22 °C).
 */
fun netCorrection(fit: Fit, at: Double): Double = fit.apply(at) - at
