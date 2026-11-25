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

/** The start of a calibration's readings moves in steps of this much. */
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

/** A finished job: whether it succeeded, its message, and when it finished (epoch ms). */
data class JobResult(val ok: Boolean, val message: String, val at: Long) {
    /**
     * Whether it is recent enough to keep its color. Later it is dimmed: the device may
     * have changed since (Seen tells whether it is still heard), but the job didn't rerun.
     */
    fun isFresh(now: Long): Boolean = now - at < JOB_RESULT_FRESH_MS
}

/** A finished job's result keeps its color this long; as `JOB_RESULT_FRESH` in `ui/table.rs`. */
const val JOB_RESULT_FRESH_MS = 10 * 60_000L

/** What device jobs are doing and have done, per device. */
data class DeviceState(
    val queued: Int = 0,
    val busy: String? = null,
    /** The last finished job. */
    val last: JobResult? = null,
    /** Device minus host clock, in seconds, when last measured. */
    val clockOffset: Double? = null,
    /** Settings last read from or written to the device. */
    val config: Config? = null,
    /** Firmware family and version (`ATC v5.9`, `THB1 · THB v2.1`), once read. */
    val firmware: String? = null,
    val coefficients: dev.artur.thermo.atc.Coefficients? = null,
)

/** Readings handed to a background fit; see [ThermoModel.calibrationInput]. */
class CalibrationInput(val devices: List<String>, val since: Long, val until: Long, val reference: String?, val moved: Long?)

class ThermoModel(
    /** Runs a job on a device; `false` if the worker is gone. `null` disables device commands. */
    private val jobs: ((Job) -> Boolean)?,
    /** Runs file writes off the main thread. */
    private val io: (() -> Unit) -> Unit = { it() },
    /** Runs a problem report from [io] back on the main thread. */
    private val main: (() -> Unit) -> Unit = { it() },
    private val clock: () -> Long = System::currentTimeMillis,
    /** Keeps the calibration session ([CalibrationSession.encode]) for the next start; `null` when it ends. */
    private val saveCalibration: (String?) -> Unit = {},
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
    /** Said once when a session's target is reached, until [takeCalibrationAnnouncement]. */
    private var announcement: String? = null
    /** What was said aloud about the session ([takeSpeech]). */
    private var spoken: Spoken? = null
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
        calibration?.let {
            it.start = windowStart(it.start, it.devices, lastCalibrations)
            // Fitted before history arrived: refit now rather than in a minute.
            it.computedAt = null
        }
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
                    onSuccess = { JobResult(true, applyOutcome(event.address, it), clock()) },
                    onFailure = { JobResult(false, it.message ?: it.toString(), clock()) },
                )
                update(event.address) { it.copy(queued = (it.queued - 1).coerceAtLeast(0), busy = null, last = message) }
                // A correction still pending once the device's jobs are done didn't get written.
                calibration?.let { session ->
                    if (state(event.address).queued == 0) session.pending.remove(event.address)?.let { session.failed[event.address] = it }
                    spoken?.takeIf { it.writes > 0 && session.pending.isEmpty() }?.let {
                        it.news += sayWritten(it.writes, session.failed.size, session.failed.keys.mapNotNull(::deviceNumber))
                        it.writes = 0
                    }
                }
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
            "firmware ${outcome.firmware}"
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
            if (outcome is Outcome.Calibrated && it.pending.remove(address) != null) {
                it.written = at
                it.moved = null
            }
            it.start = windowStart(it.start, it.devices, lastCalibrations)
            it.computedAt = null
            it.result = null
            saveCalibration(it.encode())
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

    /** Switches the list between address order and signal order (strongest now first). */
    fun toggleOrder() {
        devices.sort(if (devices.order == Order.Address) Order.Signal else Order.Address)
        changed()
    }

    /** The selected devices in list order, so lists of them follow the `#` numbers. */
    private fun selectedInOrder(): List<String> = devices.all().map { it.address }.filter { it in marked }

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

    /**
     * Where a calibration of [devices] would start: since they have read alike, but not
     * before their last calibration or the history kept.
     */
    fun suggestedCalibrationStart(devices: List<String>): Long {
        val now = clock()
        return togetherSince(this.devices, devices, now, maxOf(earliestCalibrationStart(devices), now - HISTORY_SPAN_MS))
    }

    /** Starts calibrating [devices] (at least two), using their readings from [start]. */
    fun startCalibration(devices: List<String>, reference: String? = null, start: Long = suggestedCalibrationStart(devices)) {
        val ordered = this.devices.all().map { it.address }.filter { it in devices }
        if (ordered.size < 2) return warn("pick at least two thermometers")
        val now = clock()
        calibration = CalibrationSession(
            ordered,
            reference?.takeIf { it in ordered },
            windowStart(minOf(start, now), ordered, lastCalibrations),
            began = now,
        )
        announcement = null
        spoken = Spoken(null).apply { news += sayStarted(ordered.size) }
        marked.clear()
        calibrationChanged()
    }

    /** A session saved by an earlier run of the app. */
    fun restoreCalibration(text: String?) {
        val session = text?.let(CalibrationSession::decode) ?: return
        session.start = windowStart(session.start, session.devices, lastCalibrations)
        calibration = session
        // Said before: the next progress in a while, and no first estimate again.
        spoken = Spoken(clock(), fitted = session.stage)
        changed()
    }

    fun endCalibration() {
        calibration = null
        announcement = null
        spoken = null
        saveCalibration(null)
        changed()
    }

    private fun calibrationChanged() {
        calibration?.let { saveCalibration(it.encode()) }
        changed()
    }

    /** The devices were moved to a second place, to fit gains: [at], by default now. */
    fun markMoved(at: Long = clock()) {
        val session = calibration ?: return
        if (session.stage != Stage.FirstPlace) return
        session.moved = at.coerceIn(session.start, clock())
        session.announced = false
        spoken?.apply { settling = false; caughtUpSaid = false }
        session.computedAt = null
        calibrationChanged()
    }

    /** Back to one place: the move was a mistake. */
    fun unmarkMoved() {
        val session = calibration ?: return
        session.moved = null
        session.computedAt = null
        calibrationChanged()
    }

    /** Where the readings of [devices] may start at the earliest: their last calibration. */
    fun earliestCalibrationStart(devices: List<String>): Long = windowStart(0, devices, lastCalibrations)

    /** Earliest moment with history of any of [devices]. */
    fun historyStart(devices: List<String>): Long? =
        devices.mapNotNull { this.devices[it]?.history?.firstOrNull()?.at }.minOrNull()

    /** Uses readings from [start], snapped to [WINDOW_STEP_MS] and kept after the last calibration. */
    fun setCalibrationStart(start: Long) {
        val session = calibration ?: return
        val latest = (session.moved ?: clock()) - WINDOW_STEP_MS
        session.start = maxOf(earliestCalibrationStart(session.devices), minOf(snapToStep(start), latest))
        session.computedAt = null
        calibrationChanged()
    }

    /** Changes the devices calibrated; at least two. */
    fun setCalibrationDevices(devices: List<String>) {
        val session = calibration ?: return
        val ordered = this.devices.all().map { it.address }.filter { it in devices }
        if (ordered.size < 2) return
        session.devices = ordered
        session.start = windowStart(session.start, ordered, lastCalibrations)
        if (session.reference !in ordered) session.reference = null
        session.computedAt = null
        calibrationChanged()
    }

    /** Corrects the others to match [address], one of the devices, or `null` for their average. */
    fun setCalibrationReference(address: String?) {
        val session = calibration ?: return
        session.reference = address?.takeIf { it in session.devices }
        session.computedAt = null
        calibrationChanged()
    }

    /** Aims for [level]; one already reached isn't announced again. */
    fun setCalibrationTarget(level: Level) {
        val session = calibration ?: return
        session.target = level
        session.announced = session.result?.getOrNull()?.let { targetReached(session, it) } ?: false
        calibrationChanged()
    }

    /** What to fit, if the session's result is due for a refresh. */
    fun calibrationInput(force: Boolean = false): CalibrationInput? {
        val session = calibration ?: return null
        val now = clock()
        val stale = session.computedAt?.let { now - it >= CALIBRATION_REFRESH_MS } ?: true ||
            session.fitted != session.devices
        if (!force && (!stale || session.computing)) return null
        session.computing = true
        session.fitted = session.devices
        session.start = windowStart(session.start, session.devices, lastCalibrations)
        if (session.reference !in session.devices) session.reference = null
        return CalibrationInput(session.devices, session.start, now, session.reference, session.moved)
    }

    /** Fits [input] here; the app fits [calibrationReadings] off the main thread instead. */
    fun fit(input: CalibrationInput): Result<CalibrationRun> =
        runCatching { runCalibration(devices, input.devices, input.since, input.until, input.reference, input.moved) }

    fun onCalibrationResult(input: CalibrationInput, result: Result<CalibrationRun>) {
        val session = calibration ?: return
        session.computing = false
        // A fit of devices or settings changed since is stale.
        if (session.devices != input.devices || session.start != input.since || session.reference != input.reference ||
            session.moved != input.moved
        ) return
        session.result = result
        session.computedAt = clock()
        val run = result.getOrNull()
        val reached = run != null && !session.announced && session.stage != Stage.Checking && targetReached(session, run)
        if (reached) {
            session.announced = true
            announcement = announce(session, run!!)
            calibrationChanged()
        }
        if (run != null) spoken?.let { noteSpeech(it, session, run, reached) }
        changed()
    }

    /** News from a new fit: the target reached, a first estimate, the devices caught up after the move. */
    private fun noteSpeech(spoken: Spoken, session: CalibrationSession, run: CalibrationRun, reached: Boolean) {
        val first = spoken.fitted == null || (session.stage == Stage.Checking && spoken.fitted != Stage.Checking)
        spoken.fitted = session.stage
        if (reached) {
            spoken.news += sayReached(session, run)
        } else if (first) {
            sayProgress(session, ::deviceNumber)?.let { spoken.news += if (session.stage == Stage.Checking) it else "First estimate. $it" }
        }
        if (session.stage == Stage.SecondPlace) {
            if (heldBack(run, true) != null || heldBack(run, false) != null) {
                spoken.settling = true
            } else if (spoken.settling && !spoken.caughtUpSaid) {
                spoken.caughtUpSaid = true
                spoken.news += "All thermometers have caught up in the second place."
            }
        }
    }

    /**
     * Something to say aloud about the session now, or `null`: news (a level reached, a
     * device astray, the writes done) [SPEAK_GAP_MS] after the last thing said at the
     * earliest, otherwise its progress [SPEAK_EVERY_MS] after it. Called about once a second.
     */
    fun takeSpeech(): String? {
        val session = calibration ?: return null
        val now = clock()
        val spoken = this.spoken ?: Spoken(now).also { this.spoken = it }
        // At the start, wait for the first fit, to say both at once.
        if (spoken.at == null && session.result == null) return null
        val strays = straysToSay(spoken, now)
        val since = spoken.at?.let { now - it } ?: Long.MAX_VALUE
        val text = when {
            (spoken.news.isNotEmpty() || strays.isNotEmpty()) && since >= SPEAK_GAP_MS -> {
                for (stray in strays) spoken.straySaid[stray.key] = now
                (spoken.news + sayStrays(strays, ::deviceNumber)).joinToString(" ").also { spoken.news.clear() }
            }
            since >= SPEAK_EVERY_MS -> sayProgress(session, ::deviceNumber)
            else -> null
        } ?: return null
        spoken.at = now
        return text
    }

    /** The session's progress to say right away, as speaking is turned on; the next comes [SPEAK_EVERY_MS] later. */
    fun sayProgressNow(): String? {
        val session = calibration ?: return null
        val spoken = this.spoken ?: Spoken(null).also { this.spoken = it }
        spoken.at = clock()
        return sayProgress(session, ::deviceNumber) ?: "Progress will be spoken about four times an hour."
    }

    /** The devices astray for a while and not said lately. */
    private fun straysToSay(spoken: Spoken, now: Long): List<Stray> {
        val strays = calibrationStrays()
        val keys = strays.map { it.key }.toSet()
        spoken.strayFrom.keys.retainAll(keys)
        for (key in keys) spoken.strayFrom.putIfAbsent(key, now)
        return strays.filter { stray ->
            now - spoken.strayFrom.getValue(stray.key) >= STRAY_SAID_AFTER_MS &&
                spoken.straySaid[stray.key]?.let { now - it >= STRAY_SAID_AGAIN_MS } ?: true
        }
    }

    private fun announce(session: CalibrationSession, run: CalibrationRun): String {
        val count = "${session.devices.size} thermometers"
        return if (session.stage == Stage.SecondPlace) {
            val gain = gainStatus(run).uncertainty?.let { " (±%.2f %%)".format(it * 100) } ?: ""
            "Gains ready$gain for $count: write the corrections."
        } else {
            val t = precision(run).temperature?.let { "±%.2f °C".format(it) } ?: session.target.label
            "$t reached for $count: write the corrections, or move them to a second place for the gains."
        }
    }

    /** The text to show once when a session's target was reached, or `null`. */
    fun takeCalibrationAnnouncement(): String? = announcement.also { announcement = null }

    /** One line on the session's progress, for a notification. */
    fun calibrationStatus(): String? {
        val session = calibration ?: return null
        val head = "Calibrating ${session.devices.size}"
        if (session.pending.isNotEmpty()) return "$head · writing to ${session.pending.size}"
        if (session.stage == Stage.Checking) return "$head · written, checking"
        val result = session.result ?: return "$head · fitting"
        val run = result.getOrElse { error ->
            return head + (firstEstimateIn(error)?.let { " · first estimate in ${formatEta(it)}" } ?: " · ${error.message}")
        }
        val now = precision(run).temperature?.let { " · ±%.3f °C now".format(it) } ?: ""
        if (session.stage == Stage.SecondPlace) {
            val steady = run.places.getOrNull(1)?.steady ?: 0
            val gain = gainStatus(run).uncertainty?.let { "gain ±%.2f %%".format(it * 100) } ?: "no gain yet"
            return "$head · second place · ${formatAge(steady)} steady there · $gain"
        }
        val place = ""
        val target = when (val eta = eta(run, session.target)) {
            Eta.Reached -> " · ${session.target.label} reached"
            is Eta.In -> " · ${session.target.label} in ${formatEta(eta.ms)}"
            Eta.Later -> " · ${session.target.label} in over ${formatEta(ETA_SURE_MS)}, if at all"
            Eta.Unlikely, null -> ""
        }
        return head + place + now + target
    }

    /** Devices of the session that seem out of range or not next to the others. */
    fun calibrationStrays(): List<Stray> {
        val session = calibration ?: return emptyList()
        val now = clock()
        val strays = mutableListOf<Stray>()
        val heard = session.devices.mapNotNull { address ->
            val device = devices[address]
            if (device == null || now - device.lastSeen > SESSION_SILENT_MS) {
                val ago = device?.let { " for ${formatAge(now - it.lastSeen)}" } ?: " yet"
                strays += Stray(address, "not heard$ago: still in range?")
                null
            } else {
                device.temperature?.let { address to it.toDouble() }
            }
        }
        // Right after a move the cases catch up at their own speed.
        val settling = session.moved?.let { now - it < SESSION_OPTIONS.settleTime * 1000 } ?: false
        if (heard.size >= 2 && !settling) {
            val middle = dev.artur.thermo.calibration.median(heard.map { it.second })
            for ((address, t) in heard) {
                if (abs(t - middle) > STRAY_TEMPERATURE) strays += Stray(address, "reads %+.1f °C from the others: still next to them?".format(t - middle), t - middle)
            }
        }
        return strays
    }

    /** Corrections that "write" sends: `(address, temperature, humidity)`. */
    fun calibrationWrites(): List<Triple<String, Correction, Correction>> {
        val run = calibration?.result?.getOrNull() ?: return emptyList()
        if (calibration?.stage == Stage.Checking) return emptyList()
        return run.devices.indices.mapNotNull { i ->
            val (t, h) = run.corrections(i)
            if (t == null && h == null) null
            else Triple(run.devices[i].first, t ?: Correction.IDENTITY, h ?: Correction.IDENTITY)
        }
    }

    fun applyCalibration() {
        val session = calibration ?: return
        session.failed.clear()
        for ((address, t, h) in calibrationWrites()) {
            session.pending[address] = t to h
            enqueue(address, Action.Calibrate(t, h))
        }
        spoken?.writes = session.pending.size
        changed()
    }

    /** Writes again the corrections that failed, e.g. once the device is back in range. */
    fun retryCalibrationWrites() {
        val session = calibration ?: return
        val retry = session.failed.toMap()
        session.failed.clear()
        for ((address, correction) in retry) {
            session.pending[address] = correction
            enqueue(address, Action.Calibrate(correction.first, correction.second))
        }
        spoken?.writes = session.pending.size
        changed()
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
        levelingDevices.filter { state(it).config?.txPower?.isFactory != true }

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
