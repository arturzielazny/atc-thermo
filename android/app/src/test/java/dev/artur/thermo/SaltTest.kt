/*
 * Calibrating humidity over saturated salt: thermometers in a sealed box over damp NaCl,
 * whose air creeps up to 75.6 %RH (at 15 °C) for hours, then taken out to a warm room. The
 * salt counts only once the box has stopped drifting, and then sets their humidity.
 */
package dev.artur.thermo

import dev.artur.thermo.atc.Correction
import dev.artur.thermo.model.CalibrationRun
import dev.artur.thermo.model.CalibrationSession
import dev.artur.thermo.model.Measurement
import dev.artur.thermo.model.SALT_SETTLED_RATE
import dev.artur.thermo.model.Sample
import dev.artur.thermo.model.Source
import dev.artur.thermo.model.ThermoModel
import dev.artur.thermo.model.naclHumidity
import dev.artur.thermo.model.naclUncertainty
import dev.artur.thermo.model.saltPending
import java.util.Random
import kotlin.math.abs
import kotlin.math.exp
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

private const val A = "A4:C1:38:00:00:01"
private const val B = "A4:C1:38:00:00:02"
private const val C = "A4:C1:38:00:00:03"
private const val D = "A4:C1:38:00:00:04"
private const val MINUTE = 60_000L
private const val HOUR = 3_600_000L

class SaltTest {
    private var now = 1_793_000_000_000L
    private val random = Random(11)
    private val model = ThermoModel(jobs = { true }, clock = { now })
    private val all = listOf(A, B, C, D)

    /** Each device's own errors, °C and %RH: on average they read 0.3 %RH low. */
    private val errorT = mapOf(A to 0.1, B to -0.1, C to 0.05, D to -0.05)
    private val errorH = mapOf(A to -1.5, B to -0.5, C to 0.0, D to 0.8)

    /** The box: closed at [closed] on air of 60 %RH, creeping to the salt's with a time constant of 3 h. */
    private fun box(closed: Long): (Long) -> Pair<Double, Double> = { at ->
        val t = 15.0
        t to naclHumidity(t) - (naclHumidity(t) - 60.0) * exp(-(at - closed) / (3.0 * HOUR))
    }

    private val room: (Long) -> Pair<Double, Double> = { 21.0 to 45.0 }

    /** Readings every minute for [minutes] of the true air given by [air]. */
    private fun live(minutes: Int, air: (Long) -> Pair<Double, Double>) {
        repeat(minutes) {
            now += MINUTE
            val (t, h) = air(now)
            for (address in all) {
                val sample = Sample(
                    now, (t + errorT.getValue(address) + 0.02 * random.nextGaussian()).toFloat(),
                    (h + errorH.getValue(address) + 0.05 * random.nextGaussian()).toFloat(), -60, Source.Live,
                )
                model.devices.restore(Measurement(address, address.takeLast(5), sample, 90, 3f))
            }
        }
    }

    private fun refit(): CalibrationRun {
        val input = model.calibrationInput(force = true)!!
        model.onCalibrationResult(input, model.fit(input))
        return model.calibration!!.result!!.getOrThrow()
    }

    /** What [address] reads at [h] %RH after its humidity correction. */
    private fun CalibrationRun.correctedHumidity(address: String, h: Double): Double {
        val i = devices.indexOfFirst { it.first == address }
        val correction = corrections(i).second ?: Correction.IDENTITY
        return correction.gain * (h + errorH.getValue(address)) + correction.offset
    }

    @Test
    fun theSaltSetsTheHumidityOnceTheBoxHasSettledAndAfterTheyAreTakenOut() {
        val closed = now
        live(1, box(closed))
        model.startCalibration(all, start = closed)
        model.setCalibrationSalt(0)
        // 4 h in: the box still creeps up, over 1 %RH an hour.
        live(4 * 60, box(closed))
        var run = refit()
        val session = model.calibration!!
        var salt = assertNotNull(run.salt)
        assertTrue(salt.rate!! > 0.5, "rising ${salt.rate}")
        assertNull(salt.shift, "not settled")
        assertTrue(saltPending(session, run))
        assertTrue(model.calibrationStatus()!!.contains("salt settling"), model.calibrationStatus())
        assertNull(model.takeCalibrationAnnouncement(), "nothing to write yet")

        // 20 h in: settled. The group, 0.3 %RH low on average, gets 0.3 %RH more.
        live(16 * 60, box(closed))
        run = refit()
        salt = assertNotNull(run.salt)
        assertTrue(abs(salt.rate!!) <= SALT_SETTLED_RATE, "rate ${salt.rate}")
        assertTrue(now - salt.settledAt!! <= 10 * MINUTE, "settled lately")
        assertEquals(0.3, salt.shift!!, 0.05)
        assertFalse(saltPending(session, run))
        for (address in all) assertEquals(naclHumidity(15.0), run.correctedHumidity(address, naclHumidity(15.0)), 0.06, address)
        assertNotNull(model.takeCalibrationAnnouncement(), "Fine reached with the salt")

        // Out of the box into a warm room; the move is noted 20 minutes later.
        val opened = now
        live(20, room)
        model.markMoved()
        live(3 * 60, room)
        run = refit()
        salt = assertNotNull(run.salt)
        assertTrue(salt.settledAt!! <= opened, "from before the box was opened")
        assertEquals(0.3, salt.shift!!, 0.05)
        assertFalse(saltPending(session, run), "the salt was the first place")
        // The humidity gain comes from both places, the level from the salt.
        for (address in all) {
            assertEquals(45.0, run.correctedHumidity(address, 45.0), 0.15, "$address in the room")
            assertEquals(naclHumidity(15.0), run.correctedHumidity(address, naclHumidity(15.0)), 0.15, "$address in the salt")
        }
    }

    @Test
    fun aBoxOpenedBeforeItSettledLeavesTheHumidityToTheAverage() {
        val closed = now
        live(1, box(closed))
        model.startCalibration(all, start = closed)
        model.setCalibrationSalt(0)
        live(6 * 60, box(closed))
        model.markMoved()
        live(3 * 60, room)
        val run = refit()
        val salt = assertNotNull(run.salt)
        assertNull(salt.settledAt)
        assertNull(salt.shift)
        // Corrected to their average, as without salt: -0.3 %RH from the truth.
        assertEquals(44.7, all.map { run.correctedHumidity(it, 45.0) }.average(), 0.1)
    }

    @Test
    fun aBoxInTheSecondPlaceCountsThere() {
        val start = now
        live(3 * 60, room)
        model.startCalibration(all, start = start)
        model.markMoved()
        model.setCalibrationSalt(1)
        val closed = now
        live(20 * 60, box(closed))
        val run = refit()
        val salt = assertNotNull(run.salt)
        assertEquals(1, salt.place)
        assertEquals(0.3, salt.shift!!, 0.05)
        for (address in all) assertEquals(45.0, run.correctedHumidity(address, 45.0), 0.15, address)
    }

    @Test
    fun theReferenceGetsTheSaltsHumidityToo() {
        val closed = now
        live(1, box(closed))
        model.startCalibration(all, reference = A, start = closed)
        model.setCalibrationSalt(0)
        live(20 * 60, box(closed))
        val run = refit()
        val i = run.devices.indexOfFirst { it.first == A }
        val (t, h) = run.corrections(i)
        assertNull(t, "its temperature defines the others'")
        // A reads 1.5 %RH low: the salt, not A, says what is right.
        assertEquals(1.5, assertNotNull(h).offset, 0.06)
        assertEquals(1.0, h.gain, 1e-9)
        for (address in all) assertEquals(naclHumidity(15.0), run.correctedHumidity(address, naclHumidity(15.0)), 0.06, address)
    }

    @Test
    fun itIsSaidWhileSettlingAndOnceSettled() {
        val closed = now
        live(1, box(closed))
        model.startCalibration(all, start = closed)
        model.setCalibrationSalt(0)
        model.takeSpeech()
        live(4 * 60, box(closed))
        refit()
        val said = mutableListOf<String>()
        now += 20 * MINUTE
        model.takeSpeech()?.let(said::add)
        assertTrue(said.single().contains("Waiting for the salt: humidity still rising"), said.single())
        live(16 * 60, box(closed))
        refit()
        now += 3 * MINUTE
        val settled = assertNotNull(model.takeSpeech())
        assertTrue(settled.contains("The salt has settled: humidity corrections raised by 0.3 percent."), settled)
        refit()
        now += 3 * MINUTE
        assertTrue(model.takeSpeech()?.contains("salt") != true, "once")
    }

    @Test
    fun theSessionKeepsItsSaltPlace() {
        val session = CalibrationSession(all, null, now)
        assertNull(CalibrationSession.decode(session.encode())!!.salt)
        live(1, room)
        model.startCalibration(all, start = now)
        model.setCalibrationSalt(1)
        assertEquals(1, CalibrationSession.decode(model.calibration!!.encode())!!.salt)
        model.setCalibrationSalt(null)
        assertNull(CalibrationSession.decode(model.calibration!!.encode())!!.salt)
    }

    @Test
    fun saltHoldsGreenspansHumidity() {
        assertEquals(75.61, naclHumidity(15.0), 1e-9)
        assertEquals(75.54, naclHumidity(17.5), 1e-9)
        assertEquals(75.29, naclHumidity(25.0), 1e-9)
        assertEquals(75.51, naclHumidity(-5.0), 1e-9)
        assertEquals(74.43, naclHumidity(60.0), 1e-9)
        assertEquals(0.18, naclUncertainty(15.0), 1e-9)
    }
}
