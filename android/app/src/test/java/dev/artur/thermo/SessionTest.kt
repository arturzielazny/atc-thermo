/*
 * Calibration sessions: their flow from together to written, persistence, writes that
 * fail and are retried, the announcement, and the precision and time estimates.
 */
package dev.artur.thermo

import dev.artur.thermo.atc.Coefficients
import dev.artur.thermo.atc.Correction
import dev.artur.thermo.calibration.Fit
import dev.artur.thermo.calibration.QuantityReport
import dev.artur.thermo.calibration.Report
import dev.artur.thermo.calibration.SensorReport
import dev.artur.thermo.calibration.Spread
import dev.artur.thermo.model.Action
import dev.artur.thermo.model.CalibrationRun
import dev.artur.thermo.model.CalibrationSession
import dev.artur.thermo.model.ErrorParts
import dev.artur.thermo.model.Eta
import dev.artur.thermo.model.Job
import dev.artur.thermo.model.JobEvent
import dev.artur.thermo.model.Level
import dev.artur.thermo.model.Measurement
import dev.artur.thermo.model.Outcome
import dev.artur.thermo.model.Sample
import dev.artur.thermo.model.Source
import dev.artur.thermo.model.Stage
import dev.artur.thermo.model.TEMPERATURE_FLOOR
import dev.artur.thermo.model.ThermoModel
import dev.artur.thermo.model.deviceError
import dev.artur.thermo.model.eta
import dev.artur.thermo.model.formatEta
import dev.artur.thermo.model.gainStatus
import dev.artur.thermo.model.precision
import dev.artur.thermo.model.secondPlaceAdvice
import dev.artur.thermo.model.togetherSince
import java.util.Random
import kotlin.math.abs
import kotlin.math.sqrt
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

private const val A = "A4:C1:38:00:00:01"
private const val B = "A4:C1:38:00:00:02"
private const val C = "A4:C1:38:00:00:03"
private const val D = "A4:C1:38:00:00:04"
private const val HOUR = 3_600_000L

class SessionTest {
    private var now = 1_793_000_000_000L
    private val jobs = mutableListOf<Job>()
    private var saved: String? = "untouched"

    private fun model() = ThermoModel(jobs = { jobs += it; true }, clock = { now }, saveCalibration = { saved = it })

    private fun drain(): List<Job> = jobs.toList().also { jobs.clear() }

    /**
     * Readings every 30 s from [from] to [to] of devices reading [truth] plus their offset
     * and noise; humidity 50 %RH plus ten times the offset.
     */
    private fun ThermoModel.record(
        offsets: Map<String, Double>,
        from: Long,
        to: Long,
        truth: (Long) -> Double = { 21.0 },
        noise: Double = 0.02,
        seed: Long = 1,
    ) {
        val random = Random(seed)
        var at = from
        while (at < to) {
            for ((address, offset) in offsets) {
                val t = truth(at) + offset + noise * random.nextGaussian()
                val h = 50.0 + 10 * offset + 5 * noise * random.nextGaussian()
                devices.restore(Measurement(address, "T$address".takeLast(6), Sample(at, t.toFloat(), h.toFloat(), -60, Source.Live), 90, 3.0f))
            }
            at += 30_000
        }
    }

    /** Fits the session's input here, as the app does in the background. */
    private fun ThermoModel.refit() {
        val input = calibrationInput(force = true)!!
        onCalibrationResult(input, fit(input))
    }

    @Test
    fun aSessionSurvivesARestart() {
        val session = CalibrationSession(listOf(A, B, C), B, now - HOUR, began = now)
        val model = model()
        model.record(mapOf(A to 0.0, B to 0.0, C to 0.0), now - HOUR, now)
        model.restoreCalibration(session.encode())
        model.markMoved()
        model.setCalibrationTarget(Level.Best)
        val restored = CalibrationSession.decode(saved!!)!!
        assertEquals(listOf(A, B, C), restored.devices)
        assertEquals(B, restored.reference)
        assertEquals(now - HOUR, restored.start)
        assertEquals(now, restored.began)
        assertEquals(now, restored.moved)
        assertEquals(Level.Best, restored.target)
        assertEquals(Stage.SecondPlace, restored.stage)
        assertNull(CalibrationSession.decode("not a session"))
        assertNull(CalibrationSession.decode("devices=$A\nstart=1"), "one device is no calibration")
        model.endCalibration()
        assertNull(saved)
        assertNull(model.calibration)
    }

    @Test
    fun fromTogetherToWrittenWithAFailedWriteRetried() {
        val model = model()
        model.record(mapOf(A to 0.2, B to -0.2), now - 2 * HOUR, now)
        model.startCalibration(listOf(B, A), start = now - 2 * HOUR)
        val session = model.calibration!!
        assertEquals(listOf(A, B), session.devices, "in list order")
        assertTrue(saved!!.contains("devices=$A,$B"))
        model.refit()
        val run = session.result!!.getOrThrow()
        val p = precision(run)
        assertTrue(p.temperature!! in 0.018..0.05, "±${p.temperature} °C from 2 h of steady readings")
        assertTrue(abs(run.report.sensors[0].temperature.offset + 0.2) < 0.02)

        model.markMoved()
        assertEquals(Stage.SecondPlace, session.stage)
        model.unmarkMoved()
        assertEquals(Stage.FirstPlace, session.stage)
        model.refit()

        model.applyCalibration()
        assertEquals(listOf(A, B), drain().map { it.address })
        assertEquals(setOf(A, B), session.pending.keys)
        val (t, h) = session.pending.getValue(A)
        val coefficients = Coefficients(17500, 12500, -4500, 0)
        now += 60_000
        model.onJobEvent(JobEvent.Finished(A, Result.success(Outcome.Calibrated(coefficients, coefficients, t, h))))
        assertEquals(Stage.Checking, session.stage)
        assertEquals(now, session.written)
        assertEquals(now, session.start, "readings before the write are in other units")
        assertTrue(model.calibrationWrites().isEmpty(), "nothing more to write while checking")
        model.onJobEvent(JobEvent.Finished(B, Result.failure(Exception("timeout"))))
        assertEquals(setOf(B), session.failed.keys)
        assertTrue(session.pending.isEmpty())

        model.retryCalibrationWrites()
        val retry = drain().single()
        assertEquals(B, retry.address)
        assertEquals(Action.Calibrate(session.pending.getValue(B).first, session.pending.getValue(B).second), retry.action)
        assertTrue(model.calibrationStatus()!!.contains("writing to 1"))
    }

    @Test
    fun theTargetIsAnnouncedOnce() {
        val model = model()
        model.record(mapOf(A to 0.1, B to -0.1, C to 0.0), now - 2 * HOUR, now)
        model.startCalibration(listOf(A, B, C), start = now - 2 * HOUR)
        model.setCalibrationTarget(Level.Rough)
        model.refit()
        val said = model.takeCalibrationAnnouncement()
        assertNotNull(said)
        assertTrue(said.contains("reached for 3 thermometers"), said)
        assertNull(model.takeCalibrationAnnouncement())
        model.refit()
        assertNull(model.takeCalibrationAnnouncement(), "said once")
        // Choosing a level already reached isn't news either.
        model.setCalibrationTarget(Level.Good)
        model.refit()
        assertNull(model.takeCalibrationAnnouncement())
    }

    @Test
    fun aShortSessionSaysWhenAFirstEstimateComes() {
        val model = model()
        model.record(mapOf(A to 0.0, B to 0.0), now - 15 * 60_000, now)
        model.startCalibration(listOf(A, B), start = now - 15 * 60_000)
        model.refit()
        assertTrue(model.calibration!!.result!!.isFailure)
        assertTrue(model.calibrationStatus()!!.contains("first estimate in"), model.calibrationStatus())
    }

    @Test
    fun aSessionJustStartedIsCollectingNotFailing() {
        val model = model()
        model.record(mapOf(A to 0.0, B to 0.0), now - 3600_000, now - 60_000)
        model.startCalibration(listOf(A, B), start = now)
        model.refit()
        // No readings since the start: the whole wait, not "need at least 2 sensors".
        assertEquals("Calibrating 2 · first estimate in 40 min", model.calibrationStatus())
    }

    @Test
    fun timeShrinksTheStatisticalErrorButNotTheFloor() {
        val parts = ErrorParts(floor = 0.009, statistical = 0.02, steadySeconds = 3600.0)
        assertEquals(Eta.Reached, eta(parts, 0.10, 1.0))
        assertEquals(Eta.Reached, eta(parts, 0.05, 1.0))
        // (0.03/2)² = 0.009² + 0.02²·3600/T → T = 10 000 s of steady readings, 6 400 s more.
        assertEquals(Eta.In(6_400_000), eta(parts, 0.03, 1.0))
        assertEquals(Eta.In(12_800_000), eta(parts, 0.03, 0.5), "half the time steady takes twice as long")
        assertEquals(Eta.Unlikely, eta(parts, 0.02, 1.0), "20 h is too far to promise")
        assertEquals(Eta.Unlikely, eta(ErrorParts(0.011, 0.001, 3600.0), 0.02, 1.0), "below the floor")
    }

    @Test
    fun timesBeyondTwoHoursAreOnlyLater() {
        fun run(stderr: Double): CalibrationRun {
            val fit = Fit(offset = 0.1, steady = 3 * 3600.0, stderr = stderr)
            val quantity = QuantityReport(3 * 3600.0, 21.0 to 22.0, Spread(0.2, 0.01))
            val report = Report(listOf(SensorReport(fit, fit), SensorReport(fit, fit)), quantity, quantity, emptyList())
            return CalibrationRun(listOf(A to "A", B to "B"), now - 3 * HOUR, now, report)
        }
        // ±0.03 °C: (0.015² − 0.009²) against (1.5 · stderr)² over 3 h steady.
        val soon = eta(run(0.010), Level.Fine)
        assertTrue(soon is Eta.In && soon.ms in 3_600_000..7_200_000, "$soon")
        assertEquals(Eta.Later, eta(run(0.012), Level.Fine), "3.75 h: a room seldom stays that steady")
    }

    @Test
    fun aReferenceAddsItsOwnError() {
        val fit = Fit(offset = 0.1, steady = 3 * 3600.0, stderr = 0.01)
        val quantity = QuantityReport(3 * 3600.0, 21.0 to 22.0, Spread(0.2, 0.01))
        val report = Report(listOf(SensorReport(fit, fit), SensorReport(fit, fit)), quantity, quantity, emptyList())
        val average = CalibrationRun(listOf(A to "A", B to "B"), now - 3 * HOUR, now, report)
        val toB = average.copy(reference = B)
        assertEquals(TEMPERATURE_FLOOR, deviceError(average, 0, true)!!.floor, 1e-12)
        assertEquals(TEMPERATURE_FLOOR * sqrt(2.0), deviceError(toB, 0, true)!!.floor, 1e-12)
        assertTrue(deviceError(toB, 0, true)!!.statistical > deviceError(average, 0, true)!!.statistical)
        assertNull(deviceError(average.copy(report = report.copy(sensors = listOf(SensorReport(Fit(), Fit()), SensorReport(fit, fit)))), 0, true))
    }

    @Test
    fun aSecondPlaceFitsTheGains() {
        val model = model()
        // Gains 1.02 and 0.98: 21 °C inside for 2 h, then 9 °C outside for 2.5 h.
        val inside = now - 4 * HOUR - 30 * 60_000
        val moved = now - (2.5 * HOUR).toLong()
        val truth = { at: Long -> if (at < moved) 21.0 else 9.0 }
        for ((address, gain) in listOf(A to 1.02, B to 0.98, C to 1.0)) {
            val random = Random(address.hashCode().toLong())
            var at = inside
            var case = 21.0
            while (at < now) {
                case += (truth(at) - case) * 30.0 / 400.0
                val t = gain * case + 0.01 * random.nextGaussian()
                model.devices.restore(Measurement(address, address.takeLast(5), Sample(at, t.toFloat(), 50f, -60, Source.Live), 90, 3f))
                at += 30_000
            }
        }
        model.startCalibration(listOf(A, B, C), start = inside)
        val advice = secondPlaceAdvice(dev.artur.thermo.model.Place(inside, moved, 21.0, 50.0, 21.0, 50.0, 0))
        assertEquals(16.0, advice.colderThan)
        assertEquals(31.0, advice.betterWarmerThan)
        assertEquals(35.0, advice.drierThan)
        model.markMoved(moved)
        model.refit()
        val run = model.calibration!!.result!!.getOrThrow()
        assertEquals(2, run.places.size)
        assertEquals(9.0, run.places[1].latestTemperature, 0.1)
        assertTrue(run.places[1].steady >= HOUR, "steady after the settling hour")
        val gains = gainStatus(run)
        assertEquals(2, gains.fitted, "A and B; C's gain is 1")
        assertTrue(gains.uncertainty!! < 0.01, "±${gains.uncertainty}")
        val a = run.report.sensors[0].temperature
        assertEquals(1 / 1.02, a.gain / run.report.sensors[2].temperature.gain, 0.003)
    }

    @Test
    fun togetherSinceFindsWhenTheyStartedToReadAlike() {
        val model = model()
        val joined = now - HOUR
        model.record(mapOf(A to 4.0, B to -3.0), now - 3 * HOUR, joined)
        model.record(mapOf(A to 0.1, B to -0.1), joined, now)
        val since = togetherSince(model.devices, listOf(A, B), now, 0)
        assertTrue(abs(since - joined) <= 5 * 60_000, "${(since - joined) / 60_000} min off")
        assertEquals(since, model.suggestedCalibrationStart(listOf(A, B)))
        // Apart right now: nothing to use.
        model.record(mapOf(A to 5.0, B to 0.0), now, now + 10 * 60_000)
        assertEquals(now + 10 * 60_000, togetherSince(model.devices, listOf(A, B), now + 10 * 60_000, 0))
    }

    @Test
    fun straysAreDevicesNotHeardOrNotWithTheOthers() {
        val model = model()
        model.record(mapOf(A to 0.0, B to 0.1, C to 3.0), now - HOUR, now)
        model.record(mapOf(D to 0.0), now - HOUR, now - 10 * 60_000)
        model.startCalibration(listOf(A, B, C, D), start = now - HOUR)
        val strays = model.calibrationStrays().associate { it.address to it.reason }
        assertEquals(setOf(C, D), strays.keys)
        // In the phone's language: a decimal point or comma.
        assertTrue(Regex("""reads \+2[.,]9 °C""").containsMatchIn(strays.getValue(C)), strays[C])
        assertTrue(strays.getValue(D).startsWith("not heard for 10 min"), strays[D])
    }

    @Test
    fun correctionsAreRecordedForTheRetry() {
        val session = CalibrationSession(listOf(A, B), null, now)
        session.pending[A] = Correction(1.0, 0.1) to Correction.IDENTITY
        assertEquals(Stage.FirstPlace, session.stage)
        assertEquals(1, session.pending.size)
    }

    @Test
    fun formatsWaits() {
        assertEquals("1 min", formatEta(1))
        assertEquals("25 min", formatEta(24 * 60_000L + 1))
        assertEquals("1 h", formatEta(59 * 60_000L + 1))
        assertEquals("1 h 40 min", formatEta(97 * 60_000L))
        assertEquals("2 h", formatEta(2 * HOUR))
    }
}
