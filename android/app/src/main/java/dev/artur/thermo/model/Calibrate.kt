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

/** A fit for some devices over a time window. */
data class CalibrationRun(
    /** `(address, name)` per sensor, in [Report.sensors] order. */
    val devices: List<Pair<String, String>>,
    val since: Long,
    val until: Long,
    val report: Report,
)

/**
 * The window start: [requested], but never before the last coefficient change of a
 * participating device, since older data was recorded with other coefficients.
 */
fun windowStart(requested: Long, devices: List<String>, lastCalibrations: Map<String, Long>): Long =
    devices.mapNotNull { lastCalibrations[it] }.fold(requested) { start, changed -> maxOf(start, changed) }

/**
 * Fits corrections for [addresses] using their live samples in `since..until`.
 *
 * @throws dev.artur.thermo.calibration.CalibrationError with too few devices or too
 * little steady data.
 */
fun runCalibration(devices: Devices, addresses: List<String>, since: Long, until: Long): CalibrationRun {
    val (names, readings) = calibrationReadings(devices, addresses, since, until)
    return CalibrationRun(names, since, until, calibrate(readings, Options()))
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
    if (fit.steady >= Options().minSteady) Correction(fit.gain, fit.offset) else null

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
