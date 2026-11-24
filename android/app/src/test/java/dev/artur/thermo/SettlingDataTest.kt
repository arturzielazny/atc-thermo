/*
 * Shows how [dev.artur.thermo.calibration.Options.holdUntilSettled] treats a real move:
 * fits from before the move to several moments after it, with and without holding devices
 * back, and prints who is still settling and how far each humidity offset is from a fit of
 * the first place alone (unaffected by the move). Runs only with THERMO_DATA_DIR and
 * THERMO_MOVE set: `from-moved-to` in Unix seconds.
 */
package dev.artur.thermo

import dev.artur.thermo.calibration.Reading
import dev.artur.thermo.calibration.calibrate
import dev.artur.thermo.model.SESSION_OPTIONS
import dev.artur.thermo.model.Source
import dev.artur.thermo.model.parseLine
import org.junit.Assume.assumeTrue
import java.io.File
import java.util.Locale
import kotlin.math.sqrt
import kotlin.test.Test

class SettlingDataTest {
    @Test
    fun printsWhoIsHeldBackAfterARealMove() {
        val dir = System.getenv("THERMO_DATA_DIR")
        val spec = System.getenv("THERMO_MOVE")
        assumeTrue(dir != null && spec != null)
        val (from, moved, to) = spec!!.split('-').map { it.toDouble() }
        val readings = sortedMapOf<String, MutableList<Reading>>()
        for (file in File(dir!!).listFiles { f -> f.name.startsWith("2026-") }!!.sortedBy { it.name }) file.forEachLine { line ->
            val m = parseLine(line) ?: return@forEachLine
            val at = m.sample.at / 1000.0
            if (at < from || at >= to || m.sample.source == Source.Log) return@forEachLine
            val c = line.split(',')
            readings.getOrPut(m.address) { mutableListOf() } += Reading(at, c[3].toDouble(), c[4].toDouble())
        }
        val names = readings.keys.map { it.takeLast(5) }
        val sensors = readings.values.toList()
        val first = calibrate(sensors.map { s -> s.filter { it.at < moved } }, SESSION_OPTIONS)
        val out = StringBuilder("devices ${names.joinToString(" ")}\n")
        for (minutes in listOf(60, 75, 90, 120, 150, 180, 210)) {
            val until = moved + minutes * 60
            if (until > to) break
            val window = sensors.map { s -> s.filter { it.at < until } }
            for (hold in listOf(false, true)) {
                val report = calibrate(window, SESSION_OPTIONS.copy(holdUntilSettled = hold))
                val dh = report.sensors.indices.map { report.sensors[it].humidity.offset - first.sensors[it].humidity.offset }
                val dt = report.sensors.indices.map { report.sensors[it].temperature.offset - first.sensors[it].temperature.offset }
                fun rms(v: List<Double>) = sqrt(v.sumOf { it * it } / v.size)
                out.appendLine(
                    "+%3d min %-5s ΔT rms %.3f  ΔH rms %.3f  ΔH %s".fmt(
                        minutes, if (hold) "hold" else "fixed", rms(dt), rms(dh), dh.joinToString(" ") { "%6.2f".fmt(it) },
                    ),
                )
                if (hold) for ((label, settling) in listOf("T" to report.temperature.settling, "H" to report.humidity.settling)) {
                    val held = settling.withIndex().filter { it.value != null }.joinToString(" ") { (i, s) ->
                        names[i] + "→" + (s!!.until?.let { "%.0f".fmt((it - moved) / 60) } ?: "?")
                    }
                    if (held.isNotEmpty()) out.appendLine("          $label settling (min after the move): $held")
                }
            }
        }
        println(out)
        System.getenv("THERMO_OUT")?.let { File(it).writeText(out.toString()) }
    }
}

private fun String.fmt(vararg args: Any?) = String.format(Locale.ROOT, this, *args)
