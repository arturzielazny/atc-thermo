/*
 * Replays a calibration session on real history as the app would have run it: a fake clock
 * steps through the readings, the session is refitted every few minutes, and what the screen
 * would say is printed. Then checks the times it gave ("Fine in ~25 min") against when each
 * level was actually reached. Runs only with THERMO_DATA_DIR and THERMO_REPLAY set:
 * `from-to` in Unix seconds, optionally `@moved` and `#address|address…`.
 */
package dev.artur.thermo

import dev.artur.thermo.model.CalibrationRun
import dev.artur.thermo.model.Eta
import dev.artur.thermo.model.Level
import dev.artur.thermo.model.Source
import dev.artur.thermo.model.ThermoModel
import dev.artur.thermo.model.eta
import dev.artur.thermo.model.gainStatus
import dev.artur.thermo.model.groupError
import dev.artur.thermo.model.heldBack
import dev.artur.thermo.model.parseLine
import dev.artur.thermo.model.precision
import org.junit.Assume.assumeTrue
import java.io.File
import java.util.Locale
import kotlin.math.abs
import kotlin.math.max
import kotlin.test.Test

class SessionReplayTest {
    @Test
    fun printsWhatTheAppWouldHaveSaidAndHowItsTimesHeldUp() {
        val dir = System.getenv("THERMO_DATA_DIR")
        val spec = System.getenv("THERMO_REPLAY")
        assumeTrue(dir != null && spec != null)
        val only = spec!!.substringAfter('#', "").split('|').filter { it.isNotEmpty() }.toSet()
        val moved = spec.substringBefore('#').substringAfter('@', "").toLongOrNull()?.times(1000)
        val (from, to) = spec.substringBefore('#').substringBefore('@').split('-').map { it.toLong() * 1000 }
        var now = from
        val model = ThermoModel(jobs = { true }, clock = { now })
        for (file in File(dir!!).listFiles { f -> f.name.startsWith("2026-") }!!.sortedBy { it.name }) file.forEachLine { line ->
            val m = parseLine(line) ?: return@forEachLine
            if (m.sample.at < from - 3600_000 || m.sample.at >= to || m.sample.source != Source.Live) return@forEachLine
            if (only.isNotEmpty() && m.address !in only) return@forEachLine
            model.devices.restore(m)
        }
        val addresses = model.devices.all().map { it.address }
        model.startCalibration(addresses, start = from)
        val out = StringBuilder("replay of ${addresses.size} devices, %.1f h%s\n".fmt((to - from) / 3.6e6, moved?.let { ", moved after %.1f h".fmt((it - from) / 3.6e6) } ?: ""))
        out.appendLine("   min   ±°C    ±%RH  " + Level.entries.joinToString(" ") { "%7s".fmt(it.label) } + "  notes")
        // Per level and step: what the app said (null before a first fit).
        val said = Level.entries.associateWith { mutableListOf<Pair<Long, Eta?>>() }
        while (now <= to) {
            if (moved != null && now >= moved && model.calibration?.moved == null) model.markMoved(moved)
            val input = model.calibrationInput(force = true)!!
            model.onCalibrationResult(input, model.fit(input))
            val minutes = (now - from) / 60_000
            val run = model.calibration!!.result!!.getOrNull()
            if (run != null) {
                val p = precision(run)
                val cells = Level.entries.map { level ->
                    val e = eta(run, level)
                    said.getValue(level) += now to uncapped(run, level)
                    when (e) {
                        Eta.Reached -> "now"
                        is Eta.In -> "%dm".fmt(e.ms / 60_000)
                        Eta.Later -> ">2h"
                        Eta.Unlikely -> "–"
                        null -> "?"
                    }
                }
                val notes = listOfNotNull(
                    model.takeCalibrationAnnouncement()?.let { "NOTIFY: $it" },
                    heldBack(run, temperature = false)?.let { "H held: ${it.size}" },
                    heldBack(run, temperature = true)?.let { "T held: ${it.size}" },
                    if (moved != null && now >= moved) gainStatus(run).uncertainty?.let { "gain ±%.2f%%".fmt(it * 100) } else null,
                )
                out.appendLine(
                    "%6d %6.3f %6.2f  ".fmt(minutes, p.temperature ?: Double.NaN, p.humidity ?: Double.NaN) +
                        cells.joinToString(" ") { "%7s".fmt(it) } + "  " + notes.joinToString(" · "),
                )
            } else {
                out.appendLine("%6d  %s".fmt(minutes, model.calibrationStatus()))
            }
            now += STEP_MS
        }
        // Each time given, against the next time the level was actually reached.
        out.appendLine("\nTimes given, by how far ahead, against the next actual reach (minutes):")
        out.appendLine("  ahead     given  came true  never  median |error|  within ±50 %")
        val buckets = listOf(0L to 30L, 30L to 60L, 60L to 120L, 120L to 240L, 240L to 720L)
        val samples = Level.entries.flatMap { level ->
            val steps = said.getValue(level)
            steps.mapNotNull { (at, e) ->
                if (e !is Eta.In) return@mapNotNull null
                val next = steps.firstOrNull { it.first >= at && it.second == Eta.Reached }?.first
                Triple(e.ms / 60_000, next?.let { (it - at) / 60_000 }, level)
            }
        }
        for ((lo, hi) in buckets) {
            val inBucket = samples.filter { it.first in lo until hi }
            if (inBucket.isEmpty()) continue
            val true_ = inBucket.filter { it.second != null }
            val errors = true_.map { abs(it.first - it.second!!) }.sorted()
            val close = true_.count { abs(it.first - it.second!!) <= maxOf(it.second!!, it.first) / 2 }
            out.appendLine(
                "  %3d–%-4d %6d %9d %6d %10s %12d".fmt(
                    lo, hi, inBucket.size, true_.size, inBucket.size - true_.size, errors.getOrNull(errors.size / 2)?.toString() ?: "–", close,
                ),
            )
        }
        out.appendLine("  (\"said unlikely\" but reached later: %d)".fmt(Level.entries.sumOf { level ->
            val steps = said.getValue(level)
            steps.count { (at, e) -> e == Eta.Unlikely && steps.any { it.first > at && it.second == Eta.Reached } }
        }))
        println(out)
        System.getenv("THERMO_OUT")?.let { File(it).writeText(out.toString()) }
    }

    private companion object {
        const val STEP_MS = 5 * 60_000L

        /** [eta] without [dev.artur.thermo.model.ETA_SURE_MS], to see how the longer times held up. */
        fun uncapped(run: CalibrationRun, level: Level): Eta? {
            val elapsed = (run.until - run.since) / 1000.0
            val results = listOf(true, false).mapNotNull { temperature ->
                val parts = groupError(run, temperature) ?: return@mapNotNull null
                eta(parts, if (temperature) level.temperature else level.humidity, parts.steadySeconds / max(elapsed, 1.0))
            }
            return when {
                results.isEmpty() -> null
                results.any { it == Eta.Unlikely } -> Eta.Unlikely
                results.all { it == Eta.Reached } -> Eta.Reached
                else -> Eta.In(results.filterIsInstance<Eta.In>().maxOf { it.ms })
            }
        }
    }
}

private fun String.fmt(vararg args: Any?) = String.format(Locale.ROOT, this, *args)
