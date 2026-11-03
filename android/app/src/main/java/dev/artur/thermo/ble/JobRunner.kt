/*
 * Background worker that connects to devices one at a time and runs commands on them;
 * the Android counterpart of `crates/thermo/src/jobs.rs`.
 */
package dev.artur.thermo.ble

import android.bluetooth.BluetoothAdapter
import android.content.Context
import dev.artur.thermo.atc.ClockReading
import dev.artur.thermo.atc.CommandTimeout
import dev.artur.thermo.atc.Config
import dev.artur.thermo.atc.InvalidValue
import dev.artur.thermo.atc.LogRecord
import dev.artur.thermo.atc.Thermometer
import dev.artur.thermo.atc.TxPower
import dev.artur.thermo.atc.trimmedSecondTicks
import dev.artur.thermo.model.Action
import dev.artur.thermo.model.Job
import dev.artur.thermo.model.JobEvent
import dev.artur.thermo.model.LoggedSample
import dev.artur.thermo.model.Outcome
import dev.artur.thermo.model.RateTrim
import dev.artur.thermo.model.identifyMessage
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import kotlin.math.ceil

/** Longest a single job may take, including connecting. */
private const val JOB_TIMEOUT_MS = 180_000L

/** Runs jobs in order; [events] are delivered on the main thread. */
class JobRunner(
    private val context: Context,
    private val adapter: BluetoothAdapter,
    scope: CoroutineScope,
    private val events: (JobEvent) -> Unit,
) {
    private val jobs = Channel<Job>(Channel.UNLIMITED)

    init {
        scope.launch(Dispatchers.Default) {
            for (job in jobs) {
                withContext(Dispatchers.Main) { events(JobEvent.Started(job.address, job.action.label)) }
                val outcome = run(job)
                withContext(Dispatchers.Main) { events(JobEvent.Finished(job.address, outcome)) }
            }
        }
    }

    /** Queues a job; `false` if the worker is gone. */
    fun submit(job: Job): Boolean = jobs.trySend(job).isSuccess

    private suspend fun run(job: Job): Result<Outcome> {
        if (!adapter.isEnabled) return Result.failure(InvalidValue("Bluetooth is off"))
        var thermometer: Thermometer? = null
        val result = withTimeoutOrNull(JOB_TIMEOUT_MS) {
            runCatching {
                val t = Thermometer.connect(context, adapter, job.address)
                thermometer = t
                perform(t, job.action)
            }
        }
        // The device restarts or carries on by itself; a failed disconnect changes nothing.
        withContext(NonCancellable) { thermometer?.disconnect() }
        return result ?: Result.failure(InvalidValue("timed out"))
    }
}

private suspend fun perform(t: Thermometer, action: Action): Outcome = when (action) {
    is Action.Download -> {
        val clock = t.clock()
        if (t.config().logPeriod == null) throw InvalidValue("logging is disabled on the device")
        Outcome.Downloaded(readSamples(t, clock, action.since), clock.offsetSecs)
    }
    is Action.SyncClock -> {
        val before = t.clock()
        val samples = readSamples(t, before, action.downloadSince)
        // Before setting the clock, so the new rate starts with the new time.
        val rate = trimRate(t, before)
        t.setClockToNow()
        Outcome.ClockSynced(before.offsetSecs, rate, samples)
    }
    is Action.Identify -> {
        t.show(identifyMessage(action.number))
        Outcome.Identified(action.number)
    }
    is Action.Rename -> Outcome.Renamed(t.setName(action.name))
    Action.ReadConfig -> Outcome.ConfigRead(t.config())
    is Action.Configure -> Outcome.Configured(t.setConfig(action.settings.applyTo(t.config())))
    is Action.AdjustTxPower -> setTxPower(t) { before ->
        val dbm = before.dbm ?: throw InvalidValue("unknown TX power $before")
        TxPower.nearest(dbm + action.changeDb)
    }
    Action.ResetTxPower -> setTxPower(t) { TxPower.FACTORY }
    is Action.Calibrate -> {
        val before = t.coefficients()
        val wanted = before.corrected(action.temperature, action.humidity)
        val after = t.setCoefficients(wanted)
        if (after != wanted) throw InvalidValue("device stored $after instead of $wanted")
        Outcome.Calibrated(before, after, action.temperature, action.humidity)
    }
    Action.ResetCalibration -> {
        val before = t.coefficients()
        Outcome.CalibrationReset(before, t.resetCoefficients())
    }
    Action.ReadInfo -> {
        val clock = t.clock()
        Outcome.InfoRead(t.info.firmwareVersion, t.info.hardware, t.coefficients(), t.config(), clock.offsetSecs)
    }
}

/** Sets the TX power [wanted] returns for the current one; writes nothing if they match. */
private suspend fun setTxPower(t: Thermometer, wanted: (TxPower) -> TxPower): Outcome {
    val config = t.config()
    val before = config.txPower
    val target = wanted(before)
    if (target == before) return Outcome.TxPowerSet(before, config)
    val stored: Config = t.setConfig(config.withTxPower(target))
    if (stored.txPower != target) throw InvalidValue("device stored TX power ${stored.txPower} instead of $target")
    return Outcome.TxPowerSet(before, stored)
}

private suspend fun trimRate(t: Thermometer, clock: ClockReading): RateTrim {
    val drift = clock.drift() ?: return RateTrim.Unknown
    val current = try {
        t.secondTicks()
    } catch (e: CommandTimeout) {
        return RateTrim.Unsupported
    }
    val ticks = trimmedSecondTicks(current, drift) ?: return RateTrim.TooLarge(drift)
    t.setSecondTicks(ticks)
    return RateTrim.Trimmed(drift)
}

/**
 * The [Thermometer.readLog] filter of [readSamples]: records newer than [since], up to
 * the first one stamped before the clock was set back.
 *
 * Records arrive newest first, so their device times only go down; one that goes up
 * was logged under an older time base, and so is everything before it. Without this,
 * a device that does not report when its clock was set (pvvx THB2) would have records
 * logged up to a whole clock offset ago dated that much too late.
 */
internal fun wantedRecords(clock: ClockReading, since: Long): (LogRecord) -> Boolean {
    var newer: Long? = null
    return { r ->
        val sameBase = newer.let { it == null || r.deviceTime <= it }
        newer = r.deviceTime
        sameBase && (clock.toHostMs(r.deviceTime)?.let { it >= since } ?: false)
    }
}

/** Log records newer than [since], dated with [clock]; empty if logging is off. */
private suspend fun readSamples(t: Thermometer, clock: ClockReading, since: Long): List<LoggedSample> {
    val period = t.config().logPeriod ?: return emptyList()
    val wantedSpan = (System.currentTimeMillis() - since).coerceAtLeast(0) / 1000.0
    val maxRecords = (ceil(wantedSpan / period) + 5).coerceIn(0.0, 65_535.0).toInt()
    val records = t.readLog(maxRecords, wantedRecords(clock, since))
    val periodMs = (period * 1000).toLong()
    return records.mapNotNull { r ->
        val end = clock.toHostMs(r.deviceTime) ?: return@mapNotNull null
        LoggedSample(end - periodMs / 2, r.temperature, r.humidity, r.voltage, periodMs)
    }
}
