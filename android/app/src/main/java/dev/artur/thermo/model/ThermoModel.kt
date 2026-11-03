/*
 * Application state and actions, independent of how they are shown and of Android; the
 * touch counterpart of `crates/thermo/src/app.rs`. It receives adverts and job events,
 * and hands device work to [jobs]. Everything runs on one thread (the main thread in the
 * app); [io] moves file writes off it.
 */
package dev.artur.thermo.model

import dev.artur.thermo.atc.Config
import dev.artur.thermo.atc.Correction
import dev.artur.thermo.atc.MAX_NAME_LEN
import dev.artur.thermo.atc.TxPower
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import java.util.TreeSet
import kotlin.math.abs

/** Chart time spans offered, in milliseconds. */
val WINDOWS = listOf(10 * 60_000L, 3600_000L, 6 * 3600_000L, 24 * 3600_000L, 48 * 3600_000L)
const val DEFAULT_WINDOW = 1

/** How far back missing history is fetched from device logs. */
const val DOWNLOAD_SPAN_MS = 24 * 3600_000L
/** A hole in history longer than this triggers a log download. */
const val MAX_HISTORY_GAP_MS = 15 * 60_000L
/**
 * Devices heard within this span get their settings read when levelling starts; others
 * are probably out of range, and a job would wait for them in vain.
 */
const val RECENTLY_HEARD_MS = 2 * 60_000L
/** Rows of devices not heard from for this long are shown as stale. */
const val STALE_AFTER_MS = 2 * 60_000L

/** Calibration window when the screen opens: the last 6 h. */
const val DEFAULT_CALIBRATION_SPAN_MS = 6 * 3600_000L
/** Calibration window edges move in steps of this much. */
const val WINDOW_STEP_MS = 15 * 60_000L
/** How often the calibration screen refits as new data arrives. */
const val CALIBRATION_REFRESH_MS = 60_000L

/** Which reading the chart shows. */
enum class Metric(val label: String, val unit: String, val minSpan: Double, val minGridStep: Double) {
    Temperature("Temperature", "°C", 0.1, 0.02),
    Humidity("Humidity", "%", 1.0, 0.1),
    /** Signal strength of received adverts. */
    Rssi("Signal", "dBm", 10.0, 1.0);

    /** The sample's value; `null` for signal strength of log records. */
    fun of(sample: Sample): Float? = when (this) {
        Temperature -> sample.temperature
        Humidity -> sample.humidity
        Rssi -> sample.rssi?.toFloat()
    }
}

/** State of the background scan. */
sealed interface ScanStatus {
    data object Starting : ScanStatus
    data object Scanning : ScanStatus
    data class Failed(val reason: String) : ScanStatus
}

/** What device jobs are doing and have done, per device. */
data class DeviceState(
    val queued: Int = 0,
    val busy: String? = null,
    /** Message of the last finished job, and whether it succeeded. */
    val last: Pair<Boolean, String>? = null,
    /** Device minus host clock, in seconds, when last measured. */
    val clockOffset: Double? = null,
    /** Settings last read from or written to the device. */
    val config: Config? = null,
    /** Firmware version, once read. */
    val firmware: String? = null,
    val coefficients: dev.artur.thermo.atc.Coefficients? = null,
)

/** The calibration screen: which devices, over which window, and the latest fit. */
class CalibrationSession(
    /** The devices picked, or `null` for every known device. */
    var chosen: List<String>?,
    private val all: () -> List<String>,
    /** First moment of data to fit; never before the devices' last calibration. */
    var start: Long,
    /** Last moment of data to fit; `null` follows the current time. */
    var end: Long?,
) {
    /** The devices compared, in list order. */
    val devices: List<String> get() = chosen ?: all()
    var result: Result<CalibrationRun>? = null
    var computing = false
    var computedAt: Long? = null
    /** The devices of the last fit; a change (e.g. history loaded) refits right away. */
    internal var fitted: List<String>? = null
}

/** Readings handed to a background fit; see [ThermoModel.calibrationInput]. */
class CalibrationInput(val devices: List<String>, val since: Long, val until: Long, val end: Long?)

class ThermoModel(
    /** Runs a job on a device; `false` if the worker is gone. `null` disables device commands. */
    private val jobs: ((Job) -> Boolean)?,
    /** Runs file writes off the main thread. */
    private val io: (() -> Unit) -> Unit = { it() },
    /** Runs a problem report from [io] back on the main thread. */
    private val main: (() -> Unit) -> Unit = { it() },
    private val clock: () -> Long = System::currentTimeMillis,
) {
    val devices = Devices()
    var metric = Metric.Temperature
        set(value) { field = value; changed() }
    var windowIndex = DEFAULT_WINDOW
        set(value) { field = value.coerceIn(WINDOWS.indices); changed() }
    var scanStatus: ScanStatus = ScanStatus.Starting
        private set

    /** Most recent problem worth showing, with a counter so the same text shows again. */
    var warning: String? = null
        private set
    var warningId = 0
        private set

    /** Devices picked in selection mode; actions apply to them. */
    val selected: Set<String> get() = marked
    private val marked = TreeSet<String>()

    var calibration: CalibrationSession? = null
        private set
    var leveling: Leveling? = null
        private set
    /** The devices levelled, or `null` for every known device. */
    private var levelingChosen: Set<String>? = null
    /** Devices whose settings were asked for since levelling started. */
    private val levelingReads = HashSet<String>()

    /** The devices signal levelling applies to. */
    var levelingDevices: Set<String>
        get() = levelingChosen ?: devices.all().map { it.address }.toSet()
        set(value) {
            levelingChosen = value.takeIf { it.size < devices.size }
            changed()
        }

    var store: Store? = null
        private set
    private val states = HashMap<String, DeviceState>()
    private val gapChecked = HashSet<String>()
    private val lastCalibrations = HashMap<String, Long>()
    /** When each device's log was last downloaded; older history needs no new download. */
    private val lastDownloads = HashMap<String, Long>()

    private val _revision = MutableStateFlow(0L)
    /** Bumped after every change, for the UI to redraw. */
    val revision: StateFlow<Long> = _revision

    private fun changed() {
        _revision.value++
    }

    private fun warn(message: String) {
        warning = message
        warningId++
        changed()
    }

    val window: Long get() = WINDOWS[windowIndex]

    fun state(address: String): DeviceState = states[address] ?: DeviceState()

    private fun update(address: String, f: (DeviceState) -> DeviceState) {
        states[address] = f(state(address))
    }

    /** Loads recent history from [store] (on the calling thread) and saves new measurements to it. */
    fun useStore(store: Store, loaded: Loaded, calibrations: Map<String, Long>, downloads: Map<String, Long>, clocks: Map<String, Double>) {
        for (m in loaded.measurements) devices.restore(m)
        if (loaded.skippedLines > 0) warn("skipped ${loaded.skippedLines} unreadable history lines")
        lastCalibrations.putAll(calibrations)
        lastDownloads.putAll(downloads)
        for ((address, offset) in clocks) update(address) { it.copy(clockOffset = offset) }
        this.store = store
        changed()
    }

    /** Adds history and logs imported into the store, e.g. from the desktop app. */
    fun importHistory(measurements: List<Measurement>, calibrations: Map<String, Long>, downloads: Map<String, Long>) {
        for (m in measurements.sortedBy { it.sample.at }) {
            if (clock() - m.sample.at <= HISTORY_SPAN_MS) devices.restore(m)
        }
        for ((address, at) in calibrations) lastCalibrations[address] = maxOf(lastCalibrations[address] ?: at, at)
        for ((address, at) in downloads) lastDownloads[address] = maxOf(lastDownloads[address] ?: at, at)
        changed()
    }

    // ------------------------------------------------------------ events

    fun onScanStarted() {
        scanStatus = ScanStatus.Scanning
        changed()
    }

    fun onScanFailed(reason: String) {
        scanStatus = ScanStatus.Failed(reason)
        changed()
    }

    fun onAdvert(advert: Advert) {
        if (advert.rssi != null) leveling?.record(advert.address, advert.rssi)
        if (leveling != null) readConfigForLeveling(advert.address)
        try {
            devices.record(advert)?.let { save(listOf(it)) }
        } catch (e: Exception) {
            warn("${advert.name}: ${e.message}")
        }
        if (gapChecked.add(advert.address)) downloadIfGaps(advert.address)
        changed()
    }

    /** The first time a device is heard, fetches its log if recent history has holes. */
    private fun downloadIfGaps(address: String) {
        val now = clock()
        val hasGap = devices[address]?.hasGap(downloadSince(address), now, MAX_HISTORY_GAP_MS) ?: false
        if (hasGap) enqueue(address, Action.Download(now - DOWNLOAD_SPAN_MS))
    }

    fun onJobEvent(event: JobEvent) {
        when (event) {
            is JobEvent.Started -> update(event.address) { it.copy(busy = event.label) }
            is JobEvent.Finished -> {
                val message = event.outcome.fold(
                    onSuccess = { true to applyOutcome(event.address, it) },
                    onFailure = { false to (it.message ?: it.toString()) },
                )
                update(event.address) { it.copy(queued = (it.queued - 1).coerceAtLeast(0), busy = null, last = message) }
            }
        }
        changed()
    }

    /** Updates the model with a job's result; returns a short message for the device. */
    private fun applyOutcome(address: String, outcome: Outcome): String = when (outcome) {
        is Outcome.Downloaded -> {
            noteClockOffset(address, outcome.clockOffset)
            val added = mergeDownloaded(address, outcome.samples)
            "$added of ${outcome.samples.size} log records added"
        }
        is Outcome.ClockSynced -> {
            noteClockOffset(address, 0.0)
            val added = mergeDownloaded(address, outcome.samples)
            "clock synced, was ${formatOffset(outcome.offsetBefore)}${describeRate(outcome.rate)}; $added log records added"
        }
        is Outcome.Identified -> "showing ${outcome.number} for ${IDENTIFY_DURATION_SECS / 60} min"
        is Outcome.Renamed -> {
            devices.rename(address, outcome.name)
            "renamed to ${outcome.name}"
        }
        is Outcome.ConfigRead -> {
            update(address) { it.copy(config = outcome.config) }
            "settings read"
        }
        is Outcome.Configured -> {
            update(address) { it.copy(config = outcome.config) }
            "settings saved"
        }
        is Outcome.TxPowerSet -> {
            update(address) { it.copy(config = outcome.config) }
            val after = outcome.config.txPower
            if (outcome.before == after) {
                "TX power $after unchanged"
            } else {
                // Signal strengths heard before the change no longer apply.
                leveling?.forget(address)
                "TX power ${outcome.before} → $after"
            }
        }
        is Outcome.Calibrated -> {
            recordCalibration(address, outcome.temperature to outcome.humidity, outcome)
            update(address) { it.copy(coefficients = outcome.after) }
            "calibrated: T %+.2f °C, H %+.1f %%".fmt(outcome.temperature.offset, outcome.humidity.offset)
        }
        is Outcome.CalibrationReset -> {
            recordCalibration(address, null, outcome)
            update(address) { it.copy(coefficients = outcome.after) }
            "factory calibration restored"
        }
        is Outcome.InfoRead -> {
            update(address) {
                it.copy(
                    config = outcome.config,
                    firmware = outcome.firmware,
                    coefficients = outcome.coefficients,
                    clockOffset = outcome.clockOffset,
                )
            }
            noteClockOffset(address, outcome.clockOffset)
            "firmware v${outcome.firmware}"
        }
    }

    /** Shows a freshly measured clock offset and keeps it for the next start. */
    private fun noteClockOffset(address: String, offset: Double) {
        update(address) { it.copy(clockOffset = offset) }
        val at = clock()
        persist("clock log") { it.recordClockOffset(address, at, offset) }
    }

    /** Adds downloaded log records to history and notes the download; returns how many were new. */
    private fun mergeDownloaded(address: String, samples: List<LoggedSample>): Int {
        val added = devices.mergeLog(address, samples)
        save(added)
        val now = clock()
        lastDownloads[address] = now
        persist("download log") { it.recordDownload(address, now) }
        return added.size
    }

    /** Start of the history still worth downloading: since the last download, at most a day. */
    fun downloadSince(address: String): Long {
        val dayAgo = clock() - DOWNLOAD_SPAN_MS
        return maxOf(lastDownloads[address] ?: dayAgo, dayAgo)
    }

    private fun recordCalibration(address: String, corrections: Pair<Correction, Correction>?, outcome: Outcome) {
        val (before, after) = when (outcome) {
            is Outcome.Calibrated -> outcome.before to outcome.after
            is Outcome.CalibrationReset -> outcome.before to outcome.after
            else -> return
        }
        val at = clock()
        lastCalibrations[address] = at
        val record = CalibrationRecord(at, address, devices[address]?.name ?: "", corrections, before, after)
        persist("calibration log") { it.recordCalibration(record) }
        // Data before the change is now in other units; refit from scratch.
        calibration?.let {
            it.start = windowStart(it.start, it.devices, lastCalibrations)
            it.computedAt = null
        }
    }

    private fun save(measurements: List<Measurement>) {
        if (measurements.isNotEmpty()) persist("history") { it.append(measurements) }
    }

    /** Writes to the store off the main thread; a failure shows as a warning. */
    private fun persist(what: String, write: (Store) -> Unit) {
        val store = store ?: return
        io {
            try {
                write(store)
            } catch (e: Exception) {
                main { warn("saving $what failed: ${e.message}") }
            }
        }
    }

    private fun enqueue(address: String, action: Action) {
        val jobs = jobs ?: return warn("device commands are unavailable")
        if (jobs(Job(address, action))) {
            update(address) { it.copy(queued = it.queued + 1) }
        } else {
            warn("device command worker stopped")
        }
        changed()
    }

    /** Whether any device job is queued or running. */
    val busy: Boolean get() = states.values.any { it.queued > 0 }

    // ------------------------------------------------------------ selection

    fun toggleSelected(address: String) {
        if (!marked.remove(address)) marked += address
        changed()
    }

    fun selectAll() {
        if (marked.size == devices.size) marked.clear() else marked.addAll(devices.all().map { it.address })
        changed()
    }

    fun clearSelection() {
        marked.clear()
        changed()
    }

    /** The device's position in the list, from 1; what identify shows on its screen. */
    fun deviceNumber(address: String): Int? =
        devices.all().indexOfFirst { it.address == address }.takeIf { it >= 0 }?.plus(1)

    // ------------------------------------------------------------ device actions

    fun identify(targets: Collection<String>) {
        for (address in targets) {
            val number = deviceNumber(address) ?: continue
            enqueue(address, Action.Identify(number))
        }
    }

    fun syncClocks(targets: Collection<String>) {
        for (address in targets) enqueue(address, Action.SyncClock(downloadSince(address)))
    }

    fun download(targets: Collection<String>) {
        val since = clock() - DOWNLOAD_SPAN_MS
        for (address in targets) enqueue(address, Action.Download(since))
    }

    fun readConfig(targets: Collection<String>) {
        for (address in targets) enqueue(address, Action.ReadConfig)
    }

    fun readInfo(address: String) = enqueue(address, Action.ReadInfo)

    fun configure(targets: Collection<String>, settings: Settings) {
        if (settings.isEmpty) return
        for (address in targets) enqueue(address, Action.Configure(settings))
    }

    /** Problem with [name] as a new device name, or `null` if it can be stored. */
    fun nameProblem(name: String): String? = when {
        name.toByteArray(Charsets.UTF_8).size > MAX_NAME_LEN -> "at most $MAX_NAME_LEN bytes"
        ',' in name -> "no commas"
        else -> null
    }

    fun rename(address: String, name: String) {
        nameProblem(name)?.let { return warn(it) }
        if (devices[address]?.name == name) return
        enqueue(address, Action.Rename(name))
    }

    // ------------------------------------------------------------ calibration

    fun openCalibration(): CalibrationSession {
        // Compared by default: the selected devices if at least two, else all.
        val chosen = if (marked.size >= 2) marked.toList() else null
        val session = CalibrationSession(chosen, { devices.all().map { it.address } }, start = 0, end = null)
        session.start = windowStart(clock() - DEFAULT_CALIBRATION_SPAN_MS, session.devices, lastCalibrations)
        calibration = session
        changed()
        return session
    }

    fun closeCalibration() {
        calibration = null
        changed()
    }

    /** Where the calibration window may start at the earliest: the devices' last calibration. */
    fun earliestCalibrationStart(devices: List<String>): Long = windowStart(0, devices, lastCalibrations)

    /** Earliest moment with history of any of [devices], for the window slider. */
    fun historyStart(devices: List<String>): Long? =
        devices.mapNotNull { this.devices[it]?.history?.firstOrNull()?.at }.minOrNull()

    /** Sets the window, snapped to [WINDOW_STEP_MS] and kept after the last calibration. */
    fun setCalibrationWindow(start: Long, end: Long?) {
        val session = calibration ?: return
        val now = clock()
        val earliest = earliestCalibrationStart(session.devices)
        val snappedEnd = end?.let { snapToStep(it) }?.takeIf { it < now }
        val limit = (snappedEnd ?: now) - WINDOW_STEP_MS
        session.start = maxOf(earliest, minOf(snapToStep(start), limit))
        session.end = snappedEnd?.coerceAtLeast(session.start + WINDOW_STEP_MS)
        session.computedAt = null
        changed()
    }

    fun setCalibrationDevices(devices: List<String>) {
        val session = calibration ?: return
        session.chosen = devices.takeIf { it.size < this.devices.size }
        session.start = windowStart(session.start, devices, lastCalibrations)
        session.computedAt = null
        changed()
    }

    /** What to fit, if the calibration screen's result is due for a refresh. */
    fun calibrationInput(force: Boolean = false): CalibrationInput? {
        val session = calibration ?: return null
        val now = clock()
        val stale = session.computedAt?.let { now - it >= CALIBRATION_REFRESH_MS } ?: true ||
            session.fitted != session.devices
        if (!force && (!stale || session.computing)) return null
        session.computing = true
        session.fitted = session.devices
        session.start = windowStart(session.start, session.devices, lastCalibrations)
        return CalibrationInput(session.devices, session.start, session.end ?: now, session.end)
    }

    /** Fits [input] here; the app fits [calibrationReadings] off the main thread instead. */
    fun fit(input: CalibrationInput): Result<CalibrationRun> =
        runCatching { runCalibration(devices, input.devices, input.since, input.until) }

    fun onCalibrationResult(input: CalibrationInput, result: Result<CalibrationRun>) {
        val session = calibration ?: return
        session.computing = false
        // A fit of a window or devices changed since is stale.
        if (session.devices != input.devices || session.start != input.since || session.end != input.end) return
        session.result = result
        session.computedAt = clock()
        changed()
    }

    /** Corrections that "apply" writes: `(address, temperature, humidity)`. */
    fun calibrationWrites(): List<Triple<String, Correction, Correction>> {
        val run = calibration?.result?.getOrNull() ?: return emptyList()
        return run.devices.zip(run.report.sensors).mapNotNull { (device, sensor) ->
            val (t, h) = corrections(sensor)
            if (t == null && h == null) null
            else Triple(device.first, t ?: Correction.IDENTITY, h ?: Correction.IDENTITY)
        }
    }

    fun applyCalibration() {
        for ((address, t, h) in calibrationWrites()) enqueue(address, Action.Calibrate(t, h))
    }

    fun resetCalibration(devices: Collection<String>) {
        for (address in devices) enqueue(address, Action.ResetCalibration)
    }

    // ------------------------------------------------------------ signal levelling

    fun openLeveling() {
        leveling = Leveling(clock())
        // Levelled by default: the selected devices, or else all.
        levelingChosen = marked.toSet().takeIf { it.isNotEmpty() }
        levelingReads.clear()
        metric = Metric.Rssi
        readUnknownConfigs()
        changed()
    }

    fun closeLeveling() {
        leveling = null
        changed()
    }

    fun restartLeveling() {
        leveling?.restart(clock())
        changed()
    }

    fun setLevelingTarget(target: Int) {
        leveling?.target = target
        changed()
    }

    /** Reads the settings of recently heard devices whose settings are not known yet. */
    private fun readUnknownConfigs() {
        val now = clock()
        devices.all().filter { now - it.lastSeen < RECENTLY_HEARD_MS }.forEach { readConfigForLeveling(it.address) }
    }

    /** Reads a device's settings once per levelling session, unless known or busy. */
    private fun readConfigForLeveling(address: String) {
        if (state(address).config != null || state(address).queued > 0 || !levelingReads.add(address)) return
        enqueue(address, Action.ReadConfig)
    }

    fun levelingPlan(): Plan? = leveling?.plan(devices.all().map { it.address })

    /**
     * The TX power changes "apply" queues, in dB. Devices with a job waiting are left
     * out: a queued change was planned from the same samples.
     */
    fun levelingChanges(): List<Pair<String, Int>> {
        val plan = leveling?.plan(levelingDevices.toList()) ?: return emptyList()
        return plan.rows.mapNotNull { row -> row.change?.let { row.address to it } }
            .filter { state(it.first).queued == 0 }
    }

    fun applyLeveling() {
        for ((address, change) in levelingChanges()) enqueue(address, Action.AdjustTxPower(change.toFloat()))
    }

    /** The devices a factory TX power reset applies to: those not known to have it already. */
    fun factoryResets(): List<String> =
        levelingDevices.filter { state(it).config?.txPower != TxPower.FACTORY }

    fun resetTxPower() {
        for (address in factoryResets()) enqueue(address, Action.ResetTxPower)
    }

    /** Periodic housekeeping, called about once a second. */
    fun onTick() {
        changed()
    }
}

/** Snaps a time to the nearest multiple of [WINDOW_STEP_MS] on the local clock. */
fun snapToStep(time: Long, zoneOffsetMs: (Long) -> Long = { java.util.TimeZone.getDefault().getOffset(it).toLong() }): Long {
    val offset = zoneOffsetMs(time)
    val local = time + offset
    return Math.round(local.toDouble() / WINDOW_STEP_MS) * WINDOW_STEP_MS - offset
}

/** Status text after the clock offset, e.g. `", drift +19.0 s/day trimmed"`. */
fun describeRate(rate: RateTrim): String {
    fun perDay(drift: Double) = "%+.1f s/day".fmt(drift * 86_400.0)
    return when (rate) {
        RateTrim.Unknown -> ""
        is RateTrim.Trimmed -> ", drift ${perDay(rate.drift)} trimmed"
        is RateTrim.TooLarge -> ", drift ${perDay(rate.drift)} too large to trim"
        RateTrim.Unsupported -> ", rate not adjustable"
    }
}

/** `"ok"`, `"+12s"`, `"-3m"`, `"+2.4h"`, `"-20753d"`. */
fun formatOffset(secs: Double): String {
    val a = abs(secs)
    return when {
        a < 1.5 -> "ok"
        a < 90.0 -> "%+.0fs".fmt(secs)
        a < 90.0 * 60.0 -> "%+.0fm".fmt(secs / 60.0)
        a < 48.0 * 3600.0 -> "%+.1fh".fmt(secs / 3600.0)
        else -> "%+.0fd".fmt(secs / 86_400.0)
    }
}
