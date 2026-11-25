/*
 * What a calibration says aloud, and when: news at once and only once, the progress about
 * four times an hour, in words a text-to-speech voice reads well.
 */
package dev.artur.thermo

import dev.artur.thermo.atc.Coefficients
import dev.artur.thermo.model.CalibrationSession
import dev.artur.thermo.model.Job
import dev.artur.thermo.model.JobEvent
import dev.artur.thermo.model.Measurement
import dev.artur.thermo.model.Outcome
import dev.artur.thermo.model.Precision
import dev.artur.thermo.model.SPEAK_EVERY_MS
import dev.artur.thermo.model.SPEAK_GAP_MS
import dev.artur.thermo.model.Sample
import dev.artur.thermo.model.Source
import dev.artur.thermo.model.ThermoModel
import dev.artur.thermo.model.sayDuration
import dev.artur.thermo.model.sayNumbers
import dev.artur.thermo.model.sayPrecision
import java.util.Random
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

private const val A = "A4:C1:38:00:00:01"
private const val B = "A4:C1:38:00:00:02"
private const val C = "A4:C1:38:00:00:03"
private const val D = "A4:C1:38:00:00:04"
private const val MINUTE = 60_000L
private const val HOUR = 3_600_000L

/** Symbols and decimal commas a voice would read out oddly. */
private val UNSPEAKABLE = Regex("""[±°%#]|\d,\d""")

class SpeechTest {
    private var now = 1_793_000_000_000L
    private val jobs = mutableListOf<Job>()
    private val random = Random(7)
    /** What was said, with when. */
    private val said = mutableListOf<Pair<Long, String>>()

    private fun model() = ThermoModel(jobs = { jobs += it; true }, clock = { now })

    /** A reading of each device every 30 s from [from] to [to]: `(°C, %RH)`, or `null` when not heard. */
    private fun ThermoModel.record(addresses: List<String>, from: Long, to: Long, read: (String, Long) -> Pair<Double, Double>?) {
        var at = from
        while (at < to) {
            for (address in addresses) {
                val (t, h) = read(address, at) ?: continue
                devices.restore(Measurement(address, address.takeLast(5), Sample(at, t.toFloat(), h.toFloat(), -60, Source.Live), 90, 3f))
            }
            at += 30_000
        }
    }

    /** Devices reading 21 °C and 50 %RH plus their [offset] (ten times it in %RH) and noise. */
    private fun steady(offset: Map<String, Double>): (String, Long) -> Pair<Double, Double> = { address, _ ->
        val o = offset.getValue(address)
        21.0 + o + 0.02 * random.nextGaussian() to 50.0 + 10 * o + 0.1 * random.nextGaussian()
    }

    /** The app's loop for [minutes]: readings, a refit every 5 minutes, and what is said. */
    private fun ThermoModel.live(addresses: List<String>, minutes: Int, read: (String, Long) -> Pair<Double, Double>?) {
        repeat(minutes) { minute ->
            record(addresses, now, now + MINUTE, read)
            now += MINUTE
            if (minute % 5 == 0) refit()
            takeSpeech()?.let { said += now to it }
        }
    }

    private fun ThermoModel.refit() {
        val input = calibrationInput(force = true)!!
        onCalibrationResult(input, fit(input))
    }

    private fun transcript() = said.joinToString("\n") { (at, text) -> "%4d min  %s".format((at - said.first().first) / MINUTE, text) }

    @Test
    fun aSessionSaysItStartedThenItsProgressAboutFourTimesAnHour() {
        val model = model()
        val offsets = mapOf(A to 0.1, B to -0.1, C to 0.0)
        model.record(offsets.keys.toList(), now - HOUR, now, steady(offsets))
        model.startCalibration(offsets.keys.toList(), start = now)
        model.live(offsets.keys.toList(), 4 * 60, steady(offsets))
        println(transcript())
        assertTrue(said.first().second.startsWith("Calibrating 3 thermometers."), said.first().second)
        assertTrue(said.first().first - (now - 4 * HOUR) <= MINUTE, "at once")
        for ((at, text) in said) assertNull(UNSPEAKABLE.find(text), "${(at - said.first().first) / MINUTE} min: $text")
        val gaps = said.zipWithNext { a, b -> b.first - a.first }
        assertTrue(gaps.all { it in SPEAK_GAP_MS..SPEAK_EVERY_MS + MINUTE }, "gaps ${gaps.map { it / MINUTE }}")
        // About four an hour, a bit more with news.
        assertTrue(said.size in 16..20, "${said.size} in 4 h")
        assertTrue(said.any { it.second.startsWith("Collecting readings: a first estimate in about") })
        // The first estimate is news, unless the target is reached at once: that is the news then.
        val first = said.indexOfFirst { "First estimate." in it.second || "Fine reached: plus or minus" in it.second }
        assertTrue(first > 0 && said.subList(0, first).all { it.second.startsWith("Calibrating") || it.second.startsWith("Collecting") }, transcript())
        assertEquals(1, said.count { "First estimate." in it.second || "Fine reached" in it.second }, "once")
    }

    @Test
    fun aThermometerNotHeardIsSaidOnceAfterAWhile() {
        val model = model()
        val offsets = mapOf(A to 0.0, B to 0.1, C to -0.1, D to 0.0)
        val all = offsets.keys.toList()
        model.record(all, now - 2 * HOUR, now, steady(offsets))
        model.startCalibration(all, start = now - 2 * HOUR)
        model.live(all, 10, steady(offsets))
        val gone = now
        val read = steady(offsets)
        model.live(all, 60) { address, at -> if (address == D) null else read(address, at) }
        println(transcript())
        val missing = said.filter { "not heard" in it.second }
        assertEquals(1, missing.size, transcript())
        assertTrue(missing.single().second.startsWith("Number 4 not heard for a while"), missing.single().second)
        // Not heard for 5 minutes makes it astray, said 2 minutes later, or after what was being said.
        assertTrue((missing.single().first - gone) in 7 * MINUTE..10 * MINUTE, "${(missing.single().first - gone) / MINUTE} min")
    }

    @Test
    fun writesAreSaidOnceAllAreDone() {
        val model = model()
        val offsets = mapOf(A to 0.2, B to -0.2, C to 0.0)
        model.record(offsets.keys.toList(), now - 2 * HOUR, now, steady(offsets))
        model.startCalibration(offsets.keys.toList(), start = now - 2 * HOUR)
        model.refit()
        val start = assertNotNull(model.takeSpeech())
        assertTrue(start.startsWith("Calibrating 3 thermometers.") && ("First estimate." in start || "reached" in start), start)
        model.applyCalibration()
        val (t, h) = model.calibration!!.pending.getValue(A)
        val coefficients = Coefficients(17500, 12500, -4500, 0)
        now += MINUTE
        model.onJobEvent(JobEvent.Finished(A, Result.success(Outcome.Calibrated(coefficients, coefficients, t, h))))
        model.onJobEvent(JobEvent.Finished(C, Result.success(Outcome.Calibrated(coefficients, coefficients, t, h))))
        assertNull(model.takeSpeech(), "B is still being written to, and news waits a little")
        model.onJobEvent(JobEvent.Finished(B, Result.failure(Exception("timeout"))))
        now += SPEAK_GAP_MS
        assertEquals("Corrections written to 2 thermometers, not to number 2: retry when it is in range.", model.takeSpeech())
    }

    @Test
    fun aRestartedSessionGoesOnQuietly() {
        val model = model()
        val offsets = mapOf(A to 0.1, B to -0.1, C to 0.0)
        val all = offsets.keys.toList()
        model.record(all, now - 2 * HOUR, now, steady(offsets))
        val saved = CalibrationSession(all, null, now - 2 * HOUR, began = now - 2 * HOUR).apply { announced = true }
        model.restoreCalibration(saved.encode())
        val restored = now
        model.live(all, 40, steady(offsets))
        println(transcript())
        assertEquals(listOf(15L, 30L), said.map { (it.first - restored) / MINUTE }, "progress only: ${transcript()}")
        assertTrue(said.none { "First estimate" in it.second || "Calibrating" in it.second }, transcript())
    }

    @Test
    fun aSecondPlaceSaysWhoIsSettlingAndWhenAllHaveCaughtUp() {
        val model = model()
        val all = listOf(A, B, C)
        // Gains 1.02, 0.98 and 1: 2 h at 21 °C, then 9 °C, each case following with a 400 s lag.
        val case = HashMap<String, Double>()
        val gains = mapOf(A to 1.02, B to 0.98, C to 1.0)
        var air = 21.0
        val read = { address: String, _: Long ->
            val t = case.getOrPut(address) { air }
            case[address] = t + (air - t) * 30.0 / 400.0
            gains.getValue(address) * t + 0.01 * random.nextGaussian() to 50.0 + 0.1 * random.nextGaussian()
        }
        model.record(all, now - 2 * HOUR, now, read)
        model.startCalibration(all, start = now - 2 * HOUR)
        model.live(all, 10, read)
        air = 9.0
        model.markMoved()
        val moved = now
        model.live(all, 150, read)
        println(transcript())
        val after = said.filter { it.first > moved }
        assertTrue(after.any { it.second.startsWith("Second place: no steady readings yet.") }, transcript())
        assertTrue(after.any { Regex("Temperature of numbers? [0-9, and]+ still settling").containsMatchIn(it.second) }, transcript())
        val caughtUp = after.filter { "All thermometers have caught up in the second place." in it.second }
        assertEquals(1, caughtUp.size, transcript())
        assertTrue(caughtUp.single().first - moved >= HOUR, "not before the hour after a big change")
        assertTrue(after.last().second.contains("Gains"), after.last().second)
    }

    @Test
    fun wordsForTheVoice() {
        assertEquals("1 minute", sayDuration(MINUTE))
        assertEquals("1 minute", sayDuration(10_000))
        assertEquals("25 minutes", sayDuration(24 * MINUTE + 10_000))
        assertEquals("1 hour", sayDuration(HOUR))
        assertEquals("1 hour 40 minutes", sayDuration(98 * MINUTE))
        assertEquals("2 hours 5 minutes", sayDuration(2 * HOUR + MINUTE))
        assertEquals("35 minutes", sayDuration(35 * MINUTE + 50_000, roundUp = false))
        assertEquals("number 7", sayNumbers(listOf(7)))
        assertEquals("numbers 2 and 7", sayNumbers(listOf(2, 7)))
        assertEquals("numbers 2, 4 and 7", sayNumbers(listOf(2, 4, 7)))
        assertEquals("plus or minus 0.04 degrees and 0.4 percent humidity", sayPrecision(Precision(0.0412, 0.43)))
        assertEquals("plus or minus 0.04 degrees", sayPrecision(Precision(0.0412, null)))
        assertEquals("plus or minus 0.02 degrees and 0.04 percent humidity", sayPrecision(Precision(0.02, 0.041)))
        assertNull(sayPrecision(Precision(null, null)))
    }
}
