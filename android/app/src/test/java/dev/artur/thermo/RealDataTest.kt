/*
 * Fits a window of real history, to compare with the Rust crate on the same files. Runs
 * only with THERMO_DATA_DIR, THERMO_FROM and THERMO_TO (Unix seconds) set.
 */
package dev.artur.thermo

import dev.artur.thermo.calibration.Reading
import dev.artur.thermo.calibration.calibrate
import dev.artur.thermo.model.Source
import dev.artur.thermo.model.parseLine
import org.junit.Assume.assumeTrue
import java.io.File
import java.util.Locale
import kotlin.test.Test

class RealDataTest {
    @Test
    fun printsFitOfRealWindow() {
        val dir = System.getenv("THERMO_DATA_DIR")
        assumeTrue(dir != null)
        val from = System.getenv("THERMO_FROM").toDouble()
        val to = System.getenv("THERMO_TO").toDouble()
        val readings = sortedMapOf<String, MutableList<Reading>>()
        File(dir!!).listFiles { f -> f.name.startsWith("2026-") }!!.sortedBy { it.name }.forEach { file ->
            file.forEachLine { line ->
                val m = parseLine(line) ?: return@forEachLine
                val at = m.sample.at / 1000.0
                if (at < from || at > to || m.sample.source == Source.Log) return@forEachLine
                // Parse the text again as double, as the Rust side does, rather than widen the float.
                val c = line.split(',')
                readings.getOrPut(m.address) { mutableListOf() } += Reading(at, c[3].toDouble(), c[4].toDouble())
            }
        }
        val r = calibrate(readings.values.toList())
        val out = StringBuilder()
        out.appendLine(String.format(Locale.ROOT, "steady %.0f %.0f phases %d spreadT %.4f %.4f spreadH %.3f %.3f",
            r.temperature.steady, r.humidity.steady, r.phases.size, r.temperature.spread.before, r.temperature.spread.after,
            r.humidity.spread.before, r.humidity.spread.after))
        readings.keys.zip(r.sensors).forEach { (n, s) ->
            out.appendLine(String.format(Locale.ROOT, "%s T %+.4f %.5f %b %.4f H %+.4f %.5f %b %.4f lag %s", n,
                s.temperature.offset, s.temperature.gain, s.temperature.outlier, s.temperature.residualRms,
                s.humidity.offset, s.humidity.gain, s.humidity.outlier, s.humidity.residualRms, s.temperature.lag))
        }
        File(System.getenv("THERMO_OUT") ?: "/dev/stdout").writeText(out.toString())
    }
}
