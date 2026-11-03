/*
 * Port of `crates/calibration/src/tests.rs`: simulated sessions with known sensor errors.
 */
package dev.artur.thermo

import dev.artur.thermo.calibration.CalibrationError
import dev.artur.thermo.calibration.Fit
import dev.artur.thermo.calibration.Reading
import dev.artur.thermo.calibration.Report
import dev.artur.thermo.calibration.calibrate
import dev.artur.thermo.calibration.median
import dev.artur.thermo.calibration.percentile
import dev.artur.thermo.calibration.robustFit
import dev.artur.thermo.calibration.stdDev
import kotlin.math.PI
import kotlin.math.abs
import kotlin.math.cos
import kotlin.math.ln
import kotlin.math.sin
import kotlin.math.sqrt
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNull
import kotlin.test.assertTrue

private const val TAU = 2 * PI
private const val HOUR = 3600.0

/** Deterministic xorshift generator so tests are reproducible. */
class Rng(private var state: Long) {
    fun uniform(): Double {
        state = state xor (state shl 13)
        state = state xor (state ushr 7)
        state = state xor (state shl 17)
        return (state ushr 11).toDouble() / (1L shl 53).toDouble()
    }

    /** Standard normal via Box–Muller. */
    fun normal(): Double {
        val u = uniform().coerceAtLeast(1e-12)
        return sqrt(-2.0 * ln(u)) * cos(TAU * uniform())
    }
}

/** How a simulated sensor misreports: `reported = gain · actual + offset`; [lag] in seconds. */
data class Sensor(val temperature: Pair<Double, Double>, val humidity: Pair<Double, Double>, val lag: Double)

/** Air conditions over time: an indoor day, optionally outside for a while, then back in. */
fun air(t: Double, outside: Pair<Double, Double>?): Pair<Double, Double> {
    if (outside != null && t >= outside.first && t < outside.second) return 8.0 to 85.0
    return (21.0 + 0.4 * sin(TAU * t / (3.0 * HOUR))) to (55.0 + 2.0 * sin(TAU * t / (4.0 * HOUR)))
}

fun simulate(sensors: List<Sensor>, hours: Double, outside: Pair<Double, Double>?, seed: Long) =
    simulateAir(sensors, hours, { air(it, outside) }, seed)

fun simulateAir(sensors: List<Sensor>, hours: Double, air: (Double) -> Pair<Double, Double>, seed: Long): List<List<Reading>> {
    val step = 10.0
    val rng = Rng(seed)
    return sensors.map { s ->
        val start = rng.uniform() * step
        var (tCase, hCase) = air(0.0)
        val readings = mutableListOf<Reading>()
        var t = start
        while (t < hours * HOUR) {
            val (ta, ha) = air(t)
            val k = step / s.lag
            tCase += (ta - tCase) * k
            hCase += (ha - hCase) * k
            readings += Reading(
                1_793_000_000.0 + t,
                s.temperature.first * tCase + s.temperature.second + 0.02 * rng.normal(),
                s.humidity.first * hCase + s.humidity.second + 0.15 * rng.normal(),
            )
            t += step
        }
        readings
    }
}

fun sensors() = listOf(
    Sensor(1.000 to 0.20, 1.00 to 2.0, 300.0),
    Sensor(1.015 to -0.25, 0.96 to -1.5, 400.0),
    Sensor(0.990 to 0.05, 1.04 to 0.5, 540.0),
    Sensor(1.005 to -0.10, 1.02 to -3.0, 360.0),
    Sensor(0.995 to 0.30, 0.98 to 1.0, 480.0),
)

class CalibrationTest {
    @Test
    fun statsBasics() {
        assertTrue(abs(median(listOf(3.0, 1.0, 2.0)) - 2.0) < 1e-12)
        assertTrue(abs(median(listOf(4.0, 1.0, 2.0, 3.0)) - 2.5) < 1e-12)
        assertTrue(abs(percentile(listOf(0.0, 10.0), 0.25) - 2.5) < 1e-12)
        assertTrue(median(emptyList()).isNaN())
        assertTrue(abs(stdDev(listOf(1.0, 3.0)) - sqrt(2.0)) < 1e-12)
    }

    @Test
    fun robustLineIgnoresOutliers() {
        val rows = (0 until 50).map { doubleArrayOf(it.toDouble(), 1.0) }
        val ys = rows.map { 1.05 * it[0] - 2.0 }.toMutableList()
        ys[10] += 30.0
        ys[40] -= 25.0
        val model = robustFit(rows, ys)!!
        assertTrue(abs(model.coef[0] - 1.05) < 1e-3, "$model")
        assertTrue(abs(model.coef[1] + 2.0) < 0.05, "$model")
    }

    @Test
    fun robustThreeRegressorsAndCollinear() {
        val rows = (0 until 40).map { doubleArrayOf(it.toDouble(), 1.0, sin(it * 0.7)) }
        val ys = rows.mapIndexed { i, r -> 0.9 * r[0] + 3.0 + 0.5 * r[2] + 0.001 * cos(i.toDouble()) }
        val model = robustFit(rows, ys)!!
        model.coef.zip(listOf(0.9, 3.0, 0.5)).forEach { (got, want) -> assertTrue(abs(got - want) < 0.01) }
        assertTrue(model.stderr.all { it < 0.01 })
        assertNull(robustFit((0 until 10).map { doubleArrayOf(it.toDouble(), 2.0 * it) }, List(10) { 1.0 }))
    }

    @Test
    fun threePhaseSessionRecoversGainsAndOffsets() {
        val truth = sensors()
        val report = calibrate(simulate(truth, 14.0, 6.0 * HOUR to 10.0 * HOUR, 7))
        val t = report.temperature.spread
        val h = report.humidity.spread
        assertTrue(t.before > 0.15 && t.after < 0.03, "temperature $t")
        assertTrue(h.before > 1.5 && h.after < 0.3, "humidity $h")
        assertEquals(3, report.phases.size, "${report.phases}")
        assertTrue(report.sensors.all { !it.temperature.outlier && !it.humidity.outlier })
        assertTrue(report.temperature.range.second - report.temperature.range.first > 10.0)

        // Gains are identifiable up to a common factor: fitted · true is the same for all.
        fun check(name: String, fitted: List<Fit>, trueGains: List<Double>) {
            assertTrue(fitted.count { it.gainFitted } >= 3, "$name $fitted")
            val products = fitted.zip(trueGains) { f, g -> f.gain * g }
            assertTrue(products.max() - products.min() < 0.005, "$name gain products $products")
        }
        check("temperature", report.sensors.map { it.temperature }, truth.map { it.temperature.first })
        check("humidity", report.sensors.map { it.humidity }, truth.map { it.humidity.first })
        assertTrue(abs(report.sensors.sumOf { it.temperature.gain } / 5 - 1.0) < 1e-9)
        assertTrue(abs(report.sensors.sumOf { it.temperature.offset } / 5) < 1e-9)
    }

    @Test
    fun indoorOnlyFitsOffsets() {
        val report = calibrate(simulate(sensors(), 6.0, null, 11))
        assertEquals(1, report.phases.size)
        assertTrue(report.sensors.none { it.temperature.gainFitted || it.humidity.gainFitted })
        assertTrue(report.temperature.spread.after < 0.04)
        assertTrue(report.humidity.spread.after < 0.4)
    }

    @Test
    fun outlierIsFlaggedAndExcludedFromAnchor() {
        val truth = sensors() + Sensor(1.0 to 2.5, 1.0 to 0.0, 400.0)
        val report = calibrate(simulate(truth, 6.0, null, 3))
        assertEquals(listOf(false, false, false, false, false, true), report.sensors.map { it.temperature.outlier })
        assertTrue(report.sensors.none { it.humidity.outlier })
        val goodMean = truth.take(5).sumOf { it.temperature.second } / 5
        val outlier = report.sensors[5].temperature
        assertTrue(abs(outlier.offset - (goodMean - 2.5)) < 0.05, "$outlier")
    }

    @Test
    fun transientsAreNotSteady() {
        val moves = listOf(6.0 * HOUR, 10.0 * HOUR)
        val data = simulate(sensors(), 14.0, moves[0] to moves[1], 5)
        val report = calibrate(data)
        val start = data[0][0].at - data[0][0].at % 60.0
        report.phases.drop(1).zip(moves).forEach { (phase, moved) ->
            val settleMinutes = (phase.start - (start + moved)) / 60.0
            assertTrue(settleMinutes > 20.0, "$phase settled after $settleMinutes min")
        }
    }

    @Test
    fun copesWithLossyReception() {
        val rng = Rng(99)
        val data = simulate(sensors(), 6.0, null, 13).map { r -> r.filter { rng.uniform() < 0.3 } }
        val report = calibrate(data)
        assertTrue(report.temperature.steady >= 3600.0, "${report.temperature}")
        assertTrue(report.temperature.spread.after < 0.04)
    }

    @Test
    fun slowDriftCountsAsSteady() {
        val report = calibrate(simulateAir(sensors(), 5.0, { (22.0 - it / HOUR) to (55.0 + it / HOUR) }, 21))
        assertTrue(report.temperature.steady >= 3 * 3600.0, "${report.temperature}")
        val t = report.temperature.spread
        assertTrue(t.before > 0.1 && t.after < 0.04, "temperature $t")
    }

    @Test
    fun lagDuringDriftDoesNotBiasOffsets() {
        val offsets = listOf(0.20, -0.25, 0.05, -0.10, 0.30)
        val lags = listOf(120.0, 300.0, 900.0, 480.0, 600.0)
        val truth = offsets.zip(lags) { o, lag -> Sensor(1.0 to o, 1.0 to 0.0, lag) }
        val room = { t: Double -> (22.0 - 1.2 * (t / HOUR - 2.0).coerceIn(0.0, 3.0)) to 55.0 }
        val report: Report = calibrate(simulateAir(truth, 8.0, room, 31))
        val meanOffset = offsets.sum() / 5
        val meanLag = lags.sum() / 5
        val fittedLags = report.sensors.map { it.temperature.lag!! }
        val meanFittedLag = fittedLags.sum() / 5
        report.sensors.forEachIndexed { i, sensor ->
            val expected = -(offsets[i] - meanOffset)
            assertTrue(abs(sensor.temperature.offset - expected) < 0.03, "sensor $i: ${sensor.temperature}, want $expected")
            assertTrue(abs((fittedLags[i] - meanFittedLag) - (lags[i] - meanLag)) < 90.0, "sensor $i lag")
        }
    }

    @Test
    fun errors() {
        assertFailsWith<CalibrationError.TooShort> { calibrate(simulate(sensors(), 0.3, null, 1)) }
        val warming = simulateAir(sensors(), 3.0, { (20.0 + it / 1200.0) to 50.0 }, 1)
        assertFailsWith<CalibrationError.NotEnoughSteadyData> { calibrate(warming) }
        assertEquals(
            CalibrationError.TooFewSensors(1),
            assertFailsWith<CalibrationError> { calibrate(simulate(sensors().take(1), 6.0, null, 1)) },
        )
    }
}
