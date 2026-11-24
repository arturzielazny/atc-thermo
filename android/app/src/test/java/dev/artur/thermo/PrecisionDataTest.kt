/*
 * Checks the precision estimate ([dev.artur.thermo.calibration.Fit.stderr]) on real
 * history: fits short windows of long sessions and compares each with a fit of the rest of
 * the session, independent data, and prints whether the differences match the estimated
 * errors and how large a floor for slow effects they need. Runs only with
 * THERMO_DATA_DIR and THERMO_SESSIONS set: `from-to` in Unix seconds, optionally
 * `@address|address…`, sessions separated by `;`.
 */
package dev.artur.thermo

import dev.artur.thermo.calibration.Reading
import dev.artur.thermo.calibration.Report
import dev.artur.thermo.calibration.calibrate
import dev.artur.thermo.model.HUMIDITY_FLOOR
import dev.artur.thermo.model.SESSION_OPTIONS
import dev.artur.thermo.model.STDERR_SCALE
import dev.artur.thermo.model.TEMPERATURE_FLOOR
import dev.artur.thermo.model.Source
import dev.artur.thermo.model.parseLine
import org.junit.Assume.assumeTrue
import java.io.File
import java.util.Locale
import kotlin.math.abs
import kotlin.math.max
import kotlin.math.sqrt
import kotlin.test.Test

class PrecisionDataTest {
    private class Sample(val error: Double, val variance: Double)

    @Test
    fun printsHowWellTheEstimateMatchesRealData() {
        val dir = System.getenv("THERMO_DATA_DIR")
        val sessions = System.getenv("THERMO_SESSIONS")
        assumeTrue(dir != null && sessions != null)
        val files = File(dir!!).listFiles { f -> f.name.startsWith("2026-") }!!.sortedBy { it.name }
        val lengths = (System.getenv("THERMO_LENGTHS") ?: "0.75,1,1.5,2,3").split(',').map { it.toDouble() * 3600 }
        val options = SESSION_OPTIONS
        val out = StringBuilder()
        val byLength = sortedMapOf<Double, Pair<MutableList<Sample>, MutableList<Sample>>>()
        for (spec in sessions!!.split(';')) {
            val (span, only) = spec.split('@').let { it[0] to it.getOrNull(1)?.split('|')?.toSet() }
            val (from, to) = span.split('-').map { it.toDouble() }
            val readings = sortedMapOf<String, MutableList<Reading>>()
            for (file in files) file.forEachLine { line ->
                val m = parseLine(line) ?: return@forEachLine
                val at = m.sample.at / 1000.0
                if (at < from || at >= to || m.sample.source == Source.Log) return@forEachLine
                if (only != null && m.address !in only) return@forEachLine
                val c = line.split(',')
                readings.getOrPut(m.address) { mutableListOf() } += Reading(at, c[3].toDouble(), c[4].toDouble())
            }
            val sensors = readings.values.toList()
            // Group median temperature per 5-minute bin, to compare a window only with the rest at similar temperatures.
            val near = System.getenv("THERMO_NEAR")?.takeIf { it.isNotEmpty() }?.toDouble()
            val groupT = sensors.flatMap { s -> s.groupBy { (it.at / 300).toLong() }.map { (b, r) -> b to r.map { it.temperature }.average() } }
                .groupBy({ it.first }, { it.second }).mapValues { (_, v) -> median(v) }
            out.appendLine("session %.0f–%.0f, %d sensors, %.1f h".fmt(from, to, sensors.size, (to - from) / 3600))
            for (length in lengths) {
                var start = from
                while (start + length <= to) {
                    val end = start + length
                    val window = sensors.map { s -> s.filter { it.at >= start && it.at < end } }
                    val t = median(window.flatten().map { it.temperature })
                    val rest = sensors.map { s ->
                        s.filter { it.at < start || it.at >= end }
                            .filter { near == null || abs((groupT[(it.at / 300).toLong()] ?: 99.0) - t) <= near }
                    }
                    val w = runCatching { calibrate(window, options) }.getOrNull()
                    val c = runCatching { calibrate(rest, options) }.getOrNull()
                    if (w != null && c != null) {
                        val h = median(window.flatten().map { it.humidity })
                        val (ts, hs) = byLength.getOrPut(length) { mutableListOf<Sample>() to mutableListOf() }
                        collect(w, c, t, temperature = true, ts)
                        collect(w, c, h, temperature = false, hs)
                    }
                    start += 1800.0
                }
            }
        }
        out.appendLine("hours quantity   n   rms error  rms est   rms z  within 2σ  floor")
        val allSamples = sortedMapOf<String, MutableList<Sample>>()
        for ((length, quantities) in byLength) {
            for ((name, samples) in listOf("°C" to quantities.first, "%RH" to quantities.second)) {
                if (samples.isEmpty()) continue
                val e2 = samples.map { it.error * it.error }.average()
                val v = samples.map { it.variance }.average()
                val z = sqrt(samples.map { it.error * it.error / it.variance }.average())
                val within = samples.count { abs(it.error) <= 2 * sqrt(it.variance) }.toDouble() / samples.size
                // Both fits carry the floor, so their difference carries it twice.
                val floor = sqrt(max(0.0, (e2 - v) / 2))
                out.appendLine("%5.2f %-8s %4d %10.4f %8.4f %7.2f %9.0f%% %7.4f".fmt(
                    length / 3600, name, samples.size, sqrt(e2), sqrt(v), z, within * 100, floor))
                allSamples.getOrPut(name) { mutableListOf() } += samples
            }
        }
        // error² ≈ 2F² + k²·(σ_w² + σ_c²): least squares over all windows.
        for ((name, samples) in allSamples) {
            val xs = samples.map { it.variance }
            val ys = samples.map { it.error * it.error }
            val mx = xs.average()
            val my = ys.average()
            val k2 = xs.indices.sumOf { (xs[it] - mx) * (ys[it] - my) } / xs.sumOf { (it - mx) * (it - mx) }
            val f2 = (my - k2 * mx) / 2
            out.appendLine("%-4s k = %.2f, floor F = %.4f".fmt(name, sqrt(max(k2, 0.0)), sqrt(max(f2, 0.0))))
        }
        File(System.getenv("THERMO_OUT") ?: "/dev/stdout").writeText(out.toString())
    }

    private fun collect(w: Report, c: Report, at: Double, temperature: Boolean, into: MutableList<Sample>) {
        w.sensors.zip(c.sensors).forEach { (a, b) ->
            val fa = if (temperature) a.temperature else a.humidity
            val fb = if (temperature) b.temperature else b.humidity
            if (fa.outlier || fb.outlier) return@forEach
            val sa = fa.stderr ?: return@forEach
            val sb = fb.stderr ?: return@forEach
            // As the app estimates it (Session.kt): the floor, and the scaled statistical error.
            val floor = if (temperature) TEMPERATURE_FLOOR else HUMIDITY_FLOOR
            fun variance(se: Double) = floor * floor + STDERR_SCALE * STDERR_SCALE * se * se
            into += Sample(fa.apply(at) - fb.apply(at), if (raw) sa * sa + sb * sb else variance(sa) + variance(sb))
        }
    }

    /** THERMO_RAW: the statistical error alone, to fit the floor and the scale. */
    private val raw = System.getenv("THERMO_RAW") != null

    private fun median(values: List<Double>): Double = values.sorted()[values.size / 2]

    private fun String.fmt(vararg args: Any) = String.format(Locale.ROOT, this, *args)
}
