/*
 * What device jobs do and report, without the Bluetooth part (that is `ble/JobRunner.kt`);
 * the types of `crates/thermo/src/jobs.rs` plus the settings presets of `forms.rs`.
 */
package dev.artur.thermo.model

import dev.artur.thermo.atc.Coefficients
import dev.artur.thermo.atc.Config
import dev.artur.thermo.atc.Correction
import dev.artur.thermo.atc.ScreenMessage
import dev.artur.thermo.atc.Smiley
import dev.artur.thermo.atc.TxPower

/**
 * How long a device shows its number; long enough for a whole row of devices to be
 * reached one connection after another.
 */
const val IDENTIFY_DURATION_SECS = 30 * 60

/** Settings changes; `null` leaves a value as it is on the device. Times in seconds. */
data class Settings(
    val advertisingInterval: Double? = null,
    val measureInterval: Int? = null,
    val txPower: TxPower? = null,
    /** Desired time per on-device log record. */
    val logPeriod: Double? = null,
) {
    val isEmpty: Boolean get() = this == Settings()

    /** The config with these changes; the log period last, since it counts measurements. */
    fun applyTo(config: Config): Config {
        var c = config
        advertisingInterval?.let { c = c.withAdvertisingInterval(it) }
        measureInterval?.let { c = c.withMeasureInterval(it) }
        txPower?.let { c = c.withTxPower(it) }
        logPeriod?.let { c = c.withLogAveraging(Math.round(it / c.measurementPeriod).toInt().coerceIn(1, 255)) }
        return c
    }

    companion object {
        /** Seconds between adverts offered in the settings form. */
        val ADVERTISING = listOf(1.0, 2.5, 5.0, 10.0)
        /** Adverts per measurement offered in the settings form. */
        val MEASURE_EVERY = listOf(2, 3, 4, 6, 8, 12)
        /** Seconds per log record offered in the settings form. */
        val LOG_EVERY = listOf(60.0, 5 * 60.0, 10 * 60.0, 30 * 60.0, 60 * 60.0)
    }
}

sealed interface Action {
    val label: String

    /** Download log records newer than [since] (host time). */
    data class Download(val since: Long) : Action {
        override val label get() = "downloading log"
    }

    /**
     * Download log records newer than [downloadSince] (they can only be dated with the
     * old clock), trim the clock rate by the drift since the last sync, then set the clock.
     */
    data class SyncClock(val downloadSince: Long) : Action {
        override val label get() = "syncing clock"
    }

    /** Show [number] on the device's screen for [IDENTIFY_DURATION_SECS]. */
    data class Identify(val number: Int) : Action {
        override val label get() = "identifying"
    }

    data class Rename(val name: String) : Action {
        override val label get() = "renaming"
    }

    /** Read the settings, to show them. */
    data object ReadConfig : Action {
        override val label get() = "reading settings"
    }

    data class Configure(val settings: Settings) : Action {
        override val label get() = "configuring"
    }

    /** Change the TX power by about [changeDb], to the nearest documented level. */
    data class AdjustTxPower(val changeDb: Float) : Action {
        override val label get() = "adjusting TX power"
    }

    /** Set the TX power the firmware starts with, [TxPower.FACTORY]. */
    data object ResetTxPower : Action {
        override val label get() = "restoring TX power"
    }

    /** Compose a correction onto the device's current coefficients. */
    data class Calibrate(val temperature: Correction, val humidity: Correction) : Action {
        override val label get() = "calibrating"
    }

    data object ResetCalibration : Action {
        override val label get() = "resetting calibration"
    }

    /** Read firmware, coefficients and clock, to show them. */
    data object ReadInfo : Action {
        override val label get() = "reading device"
    }
}

data class Job(val address: String, val action: Action)

/** What a clock sync did about the rate the clock drifts at. */
sealed interface RateTrim {
    /** The clock was never synced, or too recently to measure its drift. */
    data object Unknown : RateTrim
    /** [drift] (fraction, positive = fast) is corrected from now on. */
    data class Trimmed(val drift: Double) : RateTrim
    /** [drift] is more than the firmware can correct. */
    data class TooLarge(val drift: Double) : RateTrim
    /** The firmware has no rate adjustment. */
    data object Unsupported : RateTrim
}

sealed interface Outcome {
    data class Downloaded(val samples: List<LoggedSample>, val clockOffset: Double) : Outcome
    data class ClockSynced(val offsetBefore: Double, val rate: RateTrim, val samples: List<LoggedSample>) : Outcome
    data class Identified(val number: Int) : Outcome
    data class Renamed(val name: String) : Outcome
    data class ConfigRead(val config: Config) : Outcome
    data class Configured(val config: Config) : Outcome
    /** The TX power changed from [before]; [config] is the device's config now. */
    data class TxPowerSet(val before: TxPower, val config: Config) : Outcome
    data class Calibrated(
        val before: Coefficients,
        val after: Coefficients,
        val temperature: Correction,
        val humidity: Correction,
    ) : Outcome
    data class CalibrationReset(val before: Coefficients, val after: Coefficients) : Outcome
    data class InfoRead(
        val firmware: String,
        val hardware: Int,
        val coefficients: Coefficients,
        val config: Config,
        val clockOffset: Double,
    ) : Outcome
}

sealed interface JobEvent {
    data class Started(val address: String, val label: String) : JobEvent
    data class Finished(val address: String, val outcome: Result<Outcome>) : JobEvent
}

/**
 * [number] big and small with a happy face. Single digits are repeated (`333`) so they
 * show without the decimal point a reading has and can't pass for one.
 */
fun identifyMessage(number: Int): ScreenMessage {
    val bigTenths = when (number) {
        in 1..9 -> number * 111 * 10
        in 10..19 -> (number * 100 + number) * 10
        else -> number * 10
    }
    return ScreenMessage(bigTenths, number.coerceAtMost(99), Smiley.Happy, IDENTIFY_DURATION_SECS)
}
