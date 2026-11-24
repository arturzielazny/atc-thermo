/*
 * Holding devices back after a move until they have caught up with the others
 * ([dev.artur.thermo.calibration.Options.holdUntilSettled], Android only), on simulated
 * sessions whose sensors have separate temperature and humidity lags.
 */
package dev.artur.thermo

import dev.artur.thermo.calibration.Options
import dev.artur.thermo.calibration.Reading
import dev.artur.thermo.calibration.Report
import dev.artur.thermo.calibration.calibrate
import dev.artur.thermo.model.SESSION_OPTIONS
import kotlin.math.abs
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

private const val HOUR = 3600.0
private const val START = 1_793_000_000.0

class SettlingTest {
    /** Offsets in °C and %RH; lags (time constants) in seconds. */
    private class Lagging(val t: Double, val h: Double, val tLag: Double, val hLag: Double)

    /** Inside for [movedAt] s, then [after] (°C, %RH) for the rest of [hours]. */
    private fun session(sensors: List<Lagging>, hours: Double, movedAt: Double, after: Pair<Double, Double>, seed: Long): List<List<Reading>> {
        val rng = Rng(seed)
        val step = 30.0
        return sensors.map { s ->
            var tCase = 21.0
            var hCase = 55.0
            val readings = mutableListOf<Reading>()
            var t = rng.uniform() * step
            while (t < hours * HOUR) {
                val (ta, ha) = if (t < movedAt) 21.0 + 0.1 * kotlin.math.sin(t / HOUR) to 55.0 else after
                tCase += (ta - tCase) * step / s.tLag
                hCase += (ha - hCase) * step / s.hLag
                readings += Reading(START + t, tCase + s.t + 0.01 * rng.normal(), hCase + s.h + 0.05 * rng.normal())
                t += step
            }
            readings
        }
    }

    private val sensors = listOf(
        Lagging(0.10, 1.0, 180.0, 300.0),
        Lagging(-0.05, -0.5, 240.0, 240.0),
        Lagging(0.00, 0.3, 300.0, 360.0),
        // Its humidity follows an hour behind.
        Lagging(-0.05, -0.8, 240.0, HOUR),
        Lagging(0.00, 0.0, 200.0, 300.0),
    )

    /** Humidity correction error of each sensor against its true offset, relative to the mean. */
    private fun humidityErrors(report: Report): List<Double> {
        val mean = sensors.map { it.h }.average()
        return report.sensors.indices.map { i -> -report.sensors[i].humidity.apply(60.0) + 60.0 - (sensors[i].h - mean) }
    }

    @Test
    fun aSlowHumiditySensorIsHeldBackUntilItCatchesUp() {
        // 3 h inside, then 2.5 h 6 °C colder and 12 %RH more humid: below the humidity gain span.
        val data = session(sensors, 5.5, 3 * HOUR, 15.0 to 67.0, seed = 7)
        val held = calibrate(data, SESSION_OPTIONS)
        val fixed = calibrate(data, SESSION_OPTIONS.copy(holdUntilSettled = false))
        val slow = 3
        // The hour after the move left out, its humidity still counts in the second place: it
        // lags by a few %RH, which the robust fit mostly ignores here, but not creep of a few
        // tenths on real data. Held back, only its readings inside count; the others' don't change.
        val minutes = { r: Report -> r.sensors.map { (it.humidity.steady / 60).toInt() } }
        assertTrue(minutes(fixed)[slow] > 3.5 * 60, "${minutes(fixed)}")
        assertTrue(minutes(held)[slow] < 3 * 60, "${minutes(held)}")
        for (i in sensors.indices) if (i != slow) assertTrue(abs(minutes(held)[i] - minutes(fixed)[i]) <= 15, "${minutes(held)} ${minutes(fixed)}")
        assertTrue(humidityErrors(held).all { abs(it) < 0.15 }, "held back until settled: ${humidityErrors(held)}")
        // Still on its way 2.5 h after the move, the others long settled.
        val settling = held.humidity.settling
        assertEquals(listOf(slow), settling.indices.filter { settling[it] != null })
        // 12 %RH with an hour's time constant is within 0.15 about 4.4 h after the move.
        val until = assertNotNull(settling[slow]!!.until) - (START + 3 * HOUR)
        assertTrue(until in 3.5 * HOUR..6 * HOUR, "expected to count %.1f h after the move".format(until / HOUR))
        assertTrue(held.temperature.settling.all { it == null })
    }

    @Test
    fun rightAfterAMoveAllAreSettlingWithoutAnEstimateYet() {
        val data = session(sensors, 3.3, 3 * HOUR, 15.0 to 67.0, seed = 8)
        val report = calibrate(data, SESSION_OPTIONS)
        for (settling in listOf(report.temperature.settling, report.humidity.settling)) {
            assertEquals(sensors.size, settling.size)
            assertTrue(settling.all { it != null && it.until == null }, "$settling")
        }
        // Without holding back, only the hour after a big change: all of them, until a known time.
        val fixed = calibrate(data, SESSION_OPTIONS.copy(holdUntilSettled = false)).temperature.settling
        assertTrue(fixed.all { it?.until != null && it.until!! > START + 3.3 * HOUR })
    }

    @Test
    fun withoutAMoveNothingIsHeldBack() {
        val data = simulate(sensors(), 6.0, null, seed = 3)
        val held = calibrate(data, SESSION_OPTIONS)
        val fixed = calibrate(data, SESSION_OPTIONS.copy(holdUntilSettled = false))
        assertEquals(fixed.sensors, held.sensors)
        assertTrue(held.temperature.settling.all { it == null } && held.humidity.settling.all { it == null })
    }

    @Test
    fun aSmallMoveBelowTheBigChangeStillHoldsTheSlowOneBack() {
        // 7 %RH: no hour skipped, but the slow sensor lags for hours.
        val data = session(sensors, 6.0, 3 * HOUR, 20.0 to 62.0, seed = 9)
        val fixed = calibrate(data, Options(steadyWindow = 20 * 60.0, minSteady = 20 * 60.0))
        val held = calibrate(data, SESSION_OPTIONS)
        val minutes = { r: Report -> r.sensors.map { (it.humidity.steady / 60).toInt() } }
        assertTrue(minutes(held)[3] < minutes(fixed)[3] - 60, "${minutes(held)} ${minutes(fixed)}")
        assertTrue(humidityErrors(held).all { abs(it) < 0.15 }, "${humidityErrors(held)}")
        assertNull(held.temperature.settling.firstOrNull { it != null })
    }
}
