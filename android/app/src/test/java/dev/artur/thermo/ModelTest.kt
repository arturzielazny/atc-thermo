/*
 * Ports of the tests of `devices.rs`, `store.rs`, `level.rs`, `calibrate.rs` and `app.rs`.
 */
package dev.artur.thermo

import dev.artur.thermo.atc.Config
import dev.artur.thermo.atc.Coefficients
import dev.artur.thermo.atc.Correction
import dev.artur.thermo.atc.TxPower
import dev.artur.thermo.calibration.Fit
import dev.artur.thermo.calibration.SensorReport
import dev.artur.thermo.model.Action
import dev.artur.thermo.model.Advert
import dev.artur.thermo.model.CalibrationRecord
import dev.artur.thermo.model.DEFAULT_TARGET
import dev.artur.thermo.model.Devices
import dev.artur.thermo.model.Job
import dev.artur.thermo.model.JobEvent
import dev.artur.thermo.model.Leveling
import dev.artur.thermo.model.Loaded
import dev.artur.thermo.model.LoggedSample
import dev.artur.thermo.model.MIN_ADVERTS
import dev.artur.thermo.model.Measurement
import dev.artur.thermo.model.Metric
import dev.artur.thermo.model.Outcome
import dev.artur.thermo.model.RateTrim
import dev.artur.thermo.model.Sample
import dev.artur.thermo.model.Settings
import dev.artur.thermo.model.Source
import dev.artur.thermo.model.Store
import dev.artur.thermo.model.TARGET_RANGE
import dev.artur.thermo.model.ThermoModel
import dev.artur.thermo.model.WINDOW_STEP_MS
import dev.artur.thermo.model.corrections
import dev.artur.thermo.model.dayOf
import dev.artur.thermo.model.describeRate
import dev.artur.thermo.model.fileName
import dev.artur.thermo.model.formatLine
import dev.artur.thermo.model.formatOffset
import dev.artur.thermo.model.parseLine
import dev.artur.thermo.model.snapToStep
import dev.artur.thermo.model.upgradeHeader
import dev.artur.thermo.model.windowStart
import java.io.File
import java.nio.file.Files
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

private const val ADDR = "A4:C1:38:84:7E:FC"
private const val A = "A4:C1:38:00:00:01"
private const val B = "A4:C1:38:00:00:02"
private const val C = "A4:C1:38:00:00:03"

// pvvx sends measurement and voltage packets alternately, each repeated, sharing a packet id.
private val MEASUREMENT = bytes(0x40, 0x00, 0xa4, 0x01, 0x57, 0x02, 0xc4, 0x08, 0x03, 0x3e, 0x16)
private val VOLTAGE = bytes(0x40, 0x00, 0xa4, 0x0c, 0x91, 0x0b, 0x10, 0x00, 0x11, 0x01)

private fun advert(payload: ByteArray, at: Long, address: String = ADDR, rssi: Int = -60, name: String = "ATC_847EFC") =
    Advert(address, name, rssi, payload, at)

class DevicesTest {
    @Test
    fun typicalRssiIsTheRecentMedian() {
        val devices = Devices()
        val start = 1_793_000_000_000L
        devices.record(advert(VOLTAGE, start, rssi = -40))
        listOf(-80, -82, -95, -81, -79).forEachIndexed { i, rssi ->
            devices.record(advert(VOLTAGE, start + (400 + 10 * i) * 1000L, rssi = rssi))
        }
        val d = devices[ADDR]!!
        assertEquals(-79, d.rssi)
        assertEquals(-81, d.typicalRssi)
    }

    @Test
    fun mergesAlternatingPackets() {
        val devices = Devices()
        val now = 1_793_000_000_000L
        devices.record(advert(VOLTAGE, now))
        val m = devices.record(advert(MEASUREMENT, now))!!
        assertEquals(2.961f, m.voltage)
        assertEquals(87, m.battery)
        assertEquals(22.44f, devices[ADDR]!!.temperature)
        assertEquals(56.94f, devices[ADDR]!!.humidity)
    }

    @Test
    fun repeatedPacketIdAddsOneSample() {
        val devices = Devices()
        val now = 1_793_000_000_000L
        assertNotNull(devices.record(advert(MEASUREMENT, now)))
        assertNull(devices.record(advert(MEASUREMENT, now + 2000)))
        assertEquals(1, devices[ADDR]!!.history.size)
        val next = MEASUREMENT.copyOf().also { it[2] = 0xa5.toByte() }
        devices.record(advert(next, now + 10_000))
        assertEquals(2, devices[ADDR]!!.history.size)
    }

    @Test
    fun invalidPayloadStillMarksDeviceSeen() {
        val devices = Devices()
        val now = 1_793_000_000_000L
        assertTrue(runCatching { devices.record(advert(bytes(0x20), now)) }.isFailure)
        assertEquals(now, devices[ADDR]!!.lastSeen)
        assertNull(devices[ADDR]!!.temperature)
    }

    @Test
    fun restoreFillsHistoryAndLatestValues() {
        val devices = Devices()
        val start = 1_793_000_000_000L
        listOf(21.0f, 21.5f).forEachIndexed { i, t ->
            devices.restore(Measurement(ADDR, "Kitchen", Sample(start + 10_000L * i, t, 50f, null, Source.Live), 90, null))
        }
        val d = devices[ADDR]!!
        assertEquals(2, d.history.size)
        assertEquals(21.5f, d.temperature)
        assertEquals("Kitchen", d.name)
        assertEquals(start + 10_000, d.lastSeen)
    }

    @Test
    fun logRecordsFillOnlyGaps() {
        val devices = Devices()
        val start = 1_793_000_000_000L
        devices.record(advert(MEASUREMENT, start))
        val half = 1_800_000L
        val records = (0 until 4).map { LoggedSample(start + half * it, 20f, 50f, 2.9f, half) }
        assertTrue(devices[ADDR]!!.hasGap(start, start + half * 3, 900_000))
        assertEquals(3, devices.mergeLog(ADDR, records).size)
        val d = devices[ADDR]!!
        assertEquals(d.history.map { it.at }.sorted(), d.history.map { it.at })
        assertEquals(Source.Log, d.history[1].source)
        assertEquals(22.44f, d.temperature)
    }
}

class StoreTest {
    private fun measurement(at: Long, temperature: Float) =
        Measurement(ADDR, "Living, room", Sample(at, temperature, 56.94f, -61, Source.Live), 87, null)

    private fun tempDir(): File = Files.createTempDirectory("thermo-test").toFile()

    @Test
    fun lineRoundTrip() {
        val at = 1_793_046_191_123L
        val line = formatLine(measurement(at, -3.5f))
        assertEquals("1793046191123,A4:C1:38:84:7E:FC,Living  room,-3.50,56.94,87,,,-61", line)
        val parsed = parseLine(line)!!
        assertEquals(Sample(at, -3.5f, 56.94f, -61, Source.Live), parsed.sample)
        assertEquals(87, parsed.battery)
        assertNull(parsed.voltage)
        assertNull(parseLine("1793046191123,A4:C1:38:84:7E:FC,x,21."))
        assertEquals(Source.Live, parseLine("1793046191123,A4:C1:38:84:7E:FC,x,21.00,50.00,,")!!.sample.source)
        val logged = parseLine("1793046191123,A4:C1:38:84:7E:FC,x,21.00,50.00,,2.900,log")!!
        assertEquals(Source.Log, logged.sample.source)
        assertNull(logged.sample.rssi)
    }

    @Test
    fun dates() {
        assertEquals("1970-01-01.csv", fileName(0))
        assertEquals("2024-02-29.csv", fileName(19_782))
        assertEquals("2026-10-26.csv", fileName(20_752))
    }

    @Test
    fun oldHeaderGainsNewColumns() {
        val file = File(tempDir(), "day.csv")
        file.writeText("a,b\n1,2\n")
        upgradeHeader(file, "a,b,c")
        assertEquals("a,b,c\n1,2\n", file.readText())
        file.writeText("x,y\n1,2\n")
        upgradeHeader(file, "a,b,c")
        assertEquals("x,y\n1,2\n", file.readText())
        file.writeText("a,bc\n")
        upgradeHeader(file, "a,b")
        assertEquals("a,bc\n", file.readText())
    }

    @Test
    fun appendThenLoad() {
        val dir = tempDir()
        val now = System.currentTimeMillis()
        Store(dir).append(listOf(measurement(now - 2 * 86_400_000L, 20f), measurement(now, 21f)))
        File(dir, fileName(dayOf(now))).appendText("17930461911")
        // A fresh store (as after a restart) must not glue its first line onto the torn one.
        val store = Store(dir)
        store.append(listOf(measurement(now, 22f)))
        val loaded = store.load(now - 60_000)
        assertEquals(listOf(21f, 22f), loaded.measurements.map { it.sample.temperature })
        assertEquals(1, loaded.skippedLines)
    }

    @Test
    fun importMergesNewLinesOnly() {
        val store = Store(tempDir())
        val now = System.currentTimeMillis()
        val name = fileName(dayOf(now))
        store.append(listOf(measurement(now, 21f)))
        val desktop = "time_ms,address,name,temperature_c,humidity_pct,battery_pct,voltage_v,source,rssi_dbm\n" +
            formatLine(measurement(now, 21f)) + "\n" + formatLine(measurement(now + 10_000, 22f)) + "\n"
        assertEquals(listOf(22f), store.import(name, desktop)!!.map { it.sample.temperature })
        assertEquals(emptyList(), store.import(name, desktop))
        assertEquals(listOf(21f, 22f), store.load(now - 1000).measurements.map { it.sample.temperature })
        assertNull(store.import("notes.txt", "x"))
        store.import("calibrations.csv", "1793000000000,$ADDR,ATC,reset,,,,,1;2;3;4,5;6;7;8\n")
        assertEquals(1_793_000_000_000L, store.lastCalibrations()[ADDR])
    }

    @Test
    fun logsKeepTheNewest() {
        val store = Store(tempDir())
        val c = Coefficients(17_500, 12_500, -4_500, -600)
        val early = 1_793_000_000_000L
        val late = early + 3_600_000
        for (at in listOf(late, early)) {
            store.recordCalibration(CalibrationRecord(at, ADDR, "ATC", Correction.IDENTITY to Correction.IDENTITY, c, c))
            store.recordDownload(ADDR, at)
        }
        store.recordClockOffset(ADDR, late, 31.0)
        store.recordClockOffset(ADDR, early, -2.5)
        assertEquals(late, store.lastCalibrations()[ADDR])
        assertEquals(late, store.lastDownloads()[ADDR])
        assertEquals(31.0, store.lastClockOffsets()[ADDR])
        assertTrue(File(store.dir, "calibrations.csv").readText().contains(",apply,1.000000,0.0000,1.000000,0.0000,17500;-4500;12500;-600,"))
    }
}

class LevelTest {
    private fun screenWith(vararg levels: Pair<String, Int>) = Leveling(0).also { screen ->
        for ((address, rssi) in levels) {
            // Scatter of ±3 dB around the level must not move the median.
            for (i in 0 until MIN_ADVERTS) screen.record(address, rssi + listOf(-3, 0, 3)[i % 3])
        }
    }

    @Test
    fun aimsJustAboveTheMarginalSignal() {
        assertEquals(-70, DEFAULT_TARGET)
        val changes = screenWith(A to -50, B to -81, C to -71).plan(listOf(A, B, C)).rows.map { it.change }
        assertEquals(listOf(-20, 11, null), changes)
    }

    @Test
    fun devicesHeardTooRarelyAreLeftAlone() {
        val screen = Leveling(0)
        screen.record(C, -90)
        val row = screen.plan(listOf(C)).rows[0]
        assertEquals(1, row.adverts)
        assertNull(row.median)
        assertNull(row.change)
        val loud = screenWith(A to -50)
        assertTrue(loud.plan(listOf(A)).hasMedians)
        loud.forget(A)
        assertTrue(!loud.plan(listOf(A)).hasMedians)
    }

    @Test
    fun targetMovesWithinRangeAndSurvivesARestart() {
        val screen = screenWith(A to -50)
        screen.target -= 1
        assertEquals(-21, screen.plan(listOf(A)).rows[0].change)
        screen.target = 100
        assertEquals(TARGET_RANGE.last, screen.target)
        screen.restart(1)
        assertEquals(TARGET_RANGE.last, screen.target)
        assertTrue(!screen.plan(listOf(A)).hasMedians)
    }
}

class CalibrateTest {
    @Test
    fun anOutlierInOneQuantitySkipsBoth() {
        val fit = Fit(offset = 0.1, residualRms = 0.05, steady = 2 * 3600.0)
        val outlier = fit.copy(outlier = true)
        fun both(t: Fit, h: Fit) = corrections(SensorReport(t, h)).let { (a, b) -> (a != null) to (b != null) }
        assertEquals(true to true, both(fit, fit))
        assertEquals(false to false, both(outlier, fit))
        assertEquals(false to false, both(fit, outlier))
        assertEquals(true to false, both(fit, fit.copy(steady = 600.0)))
    }

    @Test
    fun windowStartsAfterLastCalibration() {
        val now = 1_793_000_000_000L
        val changes = mapOf("B" to now - 3_600_000)
        assertEquals(now - 6 * 3_600_000, windowStart(now - 6 * 3_600_000, listOf("A"), changes))
        assertEquals(now - 3_600_000, windowStart(now - 6 * 3_600_000, listOf("A", "B"), changes))
    }

    @Test
    fun snapsToQuarterHoursOfTheLocalClock() {
        val plusTwo = { _: Long -> 2 * 3_600_000L }
        val t = 1_793_000_000_000L + 7 * 60_000
        val snapped = snapToStep(t, plusTwo)
        assertEquals(0, (snapped + 2 * 3_600_000L) % WINDOW_STEP_MS)
        assertTrue(kotlin.math.abs(snapped - t) <= WINDOW_STEP_MS / 2)
    }
}

class ThermoModelTest {
    private val now = 1_793_000_000_000L
    private val jobs = mutableListOf<Job>()

    private fun model(): ThermoModel {
        val model = ThermoModel(jobs = { jobs += it; true }, clock = { now })
        for ((address, name) in listOf(A to "ATC_000001", B to "ATC_000002")) {
            model.onAdvert(advert(MEASUREMENT, now, address, -50, name))
        }
        return model
    }

    private fun drain(): List<Job> = jobs.toList().also { jobs.clear() }

    private fun finish(model: ThermoModel, address: String, outcome: Result<Outcome>) =
        model.onJobEvent(JobEvent.Finished(address, outcome))

    /** The factory config of a LYWSD03MMC on firmware v5.9 with another TX power code. */
    private fun configWithPower(code: Int) =
        Config(bytes(0x87, 0x10, 0x0a, 0x06, 0x28, 0x04, code, 0x31, 0x31, 0x04, 0xb4))

    @Test
    fun newDevicesWithoutHistoryGetTheirLogDownloaded() {
        model()
        val jobs = drain()
        assertEquals(2, jobs.size)
        assertTrue(jobs.all { it.action is Action.Download })
    }

    @Test
    fun actionsApplyToTheirTargetsAndIdentifyShowsListNumbers() {
        val model = model()
        drain()
        model.syncClocks(listOf(A))
        assertEquals(listOf(A), drain().map { it.address })
        model.selectAll()
        assertEquals(setOf(A, B), model.selected)
        model.identify(model.selected)
        assertEquals(listOf(Action.Identify(1), Action.Identify(2)), drain().map { it.action })
        model.selectAll()
        assertTrue(model.selected.isEmpty())
    }

    @Test
    fun renameChecksTheName() {
        val model = model()
        drain()
        model.rename(A, "Kitchen")
        assertEquals(listOf(Job(A, Action.Rename("Kitchen"))), drain())
        model.rename(A, "a,b")
        model.rename(A, "x".repeat(21))
        model.rename(B, "ATC_000002")
        assertEquals(emptyList(), drain())
        assertEquals("at most 20 bytes", model.warning)
    }

    @Test
    fun settingsSendOnlyChangedFields() {
        val model = model()
        drain()
        model.configure(listOf(A, B), Settings())
        assertEquals(emptyList(), drain())
        val settings = Settings(advertisingInterval = 1.0)
        model.configure(listOf(A, B), settings)
        assertEquals(listOf(Action.Configure(settings), Action.Configure(settings)), drain().map { it.action })
        val config = settings.applyTo(configWithPower(169))
        assertEquals(1.0, config.advertisingInterval)
        assertEquals(1800.0, Settings(logPeriod = 1800.0).applyTo(configWithPower(169)).logPeriod)
    }

    @Test
    fun levelingTurnsLoudDevicesDownToTheTarget() {
        val model = model()
        for (address in listOf(A, B)) finish(model, address, Result.failure(Exception("no log")))
        drain()
        model.openLeveling()
        assertEquals(Metric.Rssi, model.metric)
        // Opening the screen reads the settings it shows.
        assertEquals(listOf(Action.ReadConfig, Action.ReadConfig), drain().map { it.action })
        for (address in listOf(A, B)) finish(model, address, Result.success(Outcome.ConfigRead(configWithPower(169))))
        assertTrue(model.levelingChanges().isEmpty())

        repeat(MIN_ADVERTS) {
            for ((address, rssi) in listOf(A to -50, B to -70)) model.onAdvert(advert(MEASUREMENT, now, address, rssi))
        }
        model.applyLeveling()
        assertEquals(listOf(Job(A, Action.AdjustTxPower(-20f))), drain())
        // Not again while the change is queued.
        model.applyLeveling()
        assertEquals(emptyList(), drain())

        val config = configWithPower(150)
        finish(model, A, Result.success(Outcome.TxPowerSet(TxPower.FACTORY, config)))
        assertEquals(true to "TX power +0.0 dBm → -5.0 dBm", model.state(A).last)
        assertEquals(config, model.state(A).config)
        val plan = model.levelingPlan()!!
        assertEquals(0, plan.rows[0].adverts)
        assertEquals(-70, plan.rows[1].median)

        // Factory TX power, only where it isn't already.
        model.resetTxPower()
        assertEquals(listOf(Job(A, Action.ResetTxPower)), drain())
    }

    @Test
    fun downloadedRecordsFillHistoryAndAreRemembered() {
        val model = model()
        drain()
        val at = now - 3_600_000
        model.onJobEvent(JobEvent.Started(A, "downloading log"))
        finish(model, A, Result.success(Outcome.Downloaded(listOf(LoggedSample(at, 20f, 50f, 2.9f, 1_800_000)), -5.0)))
        assertEquals(2, model.devices[A]!!.history.size)
        val state = model.state(A)
        assertEquals(-5.0, state.clockOffset)
        assertNull(state.busy)
        assertEquals(true to "1 of 1 log records added", state.last)
        assertTrue(model.downloadSince(A) > at)
    }

    @Test
    fun screensOpenedBeforeDevicesAreKnownFollowAllDevices() {
        val model = ThermoModel(jobs = { jobs += it; true }, clock = { now })
        val session = model.openCalibration()
        model.openLeveling()
        assertEquals(emptyList(), session.devices)
        for (address in listOf(A, B)) model.onAdvert(advert(MEASUREMENT, now, address))
        assertEquals(listOf(A, B), session.devices)
        assertEquals(setOf(A, B), model.levelingDevices)
        // Devices heard while levelling get their settings read, once.
        assertEquals(2, drain().count { it.action == Action.ReadConfig })
        model.onAdvert(advert(MEASUREMENT, now, A))
        assertEquals(0, drain().count { it.action == Action.ReadConfig })

        model.levelingDevices = setOf(A)
        assertEquals(setOf(A), model.levelingDevices)
        model.setCalibrationDevices(listOf(A, B))
        assertEquals(listOf(A, B), session.devices)
    }

    @Test
    fun staleCalibrationResultsAreDropped() {
        val model = model()
        val session = model.openCalibration()
        val first = model.calibrationInput()!!
        assertNull(model.calibrationInput(), "a fit is already running")
        model.setCalibrationWindow(now - 3 * 3_600_000, null)
        val second = model.calibrationInput(force = true)!!
        model.onCalibrationResult(first, Result.failure(Exception("old window")))
        assertNull(session.result)
        model.onCalibrationResult(second, Result.failure(Exception("current")))
        assertEquals("current", session.result!!.exceptionOrNull()!!.message)
    }

    @Test
    fun storeLoadRestoresDevices() {
        val model = ThermoModel(jobs = null, clock = { now })
        val m = Measurement(A, "Kitchen", Sample(now - 1000, 21f, 50f, -60, Source.Live), 80, 2.9f)
        model.useStore(Store(Files.createTempDirectory("t").toFile()), Loaded(listOf(m), 0), emptyMap(), emptyMap(), mapOf(A to 12.0))
        assertEquals("Kitchen", model.devices[A]!!.name)
        assertEquals(12.0, model.state(A).clockOffset)
    }

    @Test
    fun formatting() {
        assertEquals("ok", formatOffset(0.4))
        assertEquals("-37s", formatOffset(-37.0))
        assertEquals("+10m", formatOffset(600.0))
        assertEquals("-2.5h", formatOffset(-9000.0))
        assertEquals("-20753d", formatOffset(-1_793_040_361.0))
        assertEquals(", drift +19.0 s/day trimmed", describeRate(RateTrim.Trimmed(19.0 / 86_400.0)))
        assertEquals("", describeRate(RateTrim.Unknown))
    }
}
