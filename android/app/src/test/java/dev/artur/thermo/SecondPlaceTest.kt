/*
 * A session's second place counts for the gains after a fixed time, however long the first
 * place was ([dev.artur.thermo.calibration.Options.spanTrim], Android only).
 */
package dev.artur.thermo

import dev.artur.thermo.calibration.calibrate
import dev.artur.thermo.model.GAIN_SPAN_T
import dev.artur.thermo.model.SESSION_OPTIONS
import kotlin.math.PI
import kotlin.math.abs
import kotlin.math.sin
import kotlin.test.Test
import kotlin.test.assertTrue

private const val HOUR = 3600.0

class SecondPlaceTest {
    /** 12 h inside, then outside 13 °C colder and 20 %RH more humid for [outside] hours. */
    private fun session(outside: Double) = simulateAir(
        sensors(), 12 + outside,
        { t -> if (t < 12 * HOUR) (21.0 + 0.3 * sin(2 * PI * t / (3 * HOUR))) to 55.0 else 8.0 to 75.0 },
        seed = 11,
    )

    @Test
    fun halfAnHourInASecondPlaceGivesGainsAfterADayInTheFirst() {
        // The hour after the move doesn't count, then ~35 min of steady readings: under 5 % of all.
        val data = session(outside = 1.75)
        val trimmed = calibrate(data, SESSION_OPTIONS)
        val percentiles = calibrate(data, SESSION_OPTIONS.copy(spanTrim = null))
        assertTrue(percentiles.sensors.none { it.temperature.gainFitted }, "5 % of the steady time isn't reached yet")
        val span = trimmed.temperature.range.let { it.second - it.first }
        assertTrue(span > GAIN_SPAN_T, "span $span")
        // Gains of the fixture: 1.000, 1.015, 0.990, 1.005, 0.995, relative to their average.
        val truth = sensors().map { it.temperature.first }
        val mean = truth.average()
        val fitted = trimmed.sensors.map { it.temperature }
        assertTrue(fitted.count { it.gainFitted } >= 3, "$fitted")
        for ((fit, gain) in fitted.zip(truth)) {
            if (fit.gainFitted) assertTrue(abs(fit.gain - mean / gain) < 0.004, "gain ${fit.gain}, expected ${mean / gain}")
        }
    }

    @Test
    fun aFewStrayBinsDontMakeASpan() {
        // 10 min outside and back: never 15 min of steady readings there.
        val data = simulateAir(
            sensors(), 8.0,
            { t -> if (t in 4 * HOUR..4 * HOUR + 600) 8.0 to 75.0 else 21.0 + 0.3 * sin(2 * PI * t / (3 * HOUR)) to 55.0 },
            seed = 12,
        )
        val report = calibrate(data, SESSION_OPTIONS)
        assertTrue(report.sensors.none { it.temperature.gainFitted })
        assertTrue(report.temperature.range.let { it.second - it.first } < 1.0)
    }
}
