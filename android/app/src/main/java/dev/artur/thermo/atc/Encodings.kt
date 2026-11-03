/*
 * Byte layouts and maths of the pvvx ATC_MiThermometer command interface, with no I/O;
 * a port of `crates/atc/src/{config,coefficients,clock,log,screen}.rs`. Layouts follow
 * `src/app.h`, `src/sensor.h`, `src/logger.h` and `src/cmd_parser.c` of
 * <https://github.com/pvvx/ATC_MiThermometer>, firmware v4.7 and newer.
 */
package dev.artur.thermo.atc

import java.util.Locale
import kotlin.math.abs
import kotlin.math.round

/** A requested value cannot be represented by the firmware, or a reply made no sense. */
class InvalidValue(message: String) : Exception(message)

internal fun ByteArray.u8(at: Int) = this[at].toInt() and 0xff
internal fun ByteArray.u16(at: Int) = u8(at) or (u8(at + 1) shl 8)
internal fun ByteArray.i16(at: Int) = u16(at).toShort().toInt()
internal fun ByteArray.u32(at: Int): Long =
    (u16(at).toLong()) or (u16(at + 2).toLong() shl 16)

internal fun le16(value: Int) = byteArrayOf(value.toByte(), (value shr 8).toByte())
internal fun le32(value: Long) =
    byteArrayOf(value.toByte(), (value shr 8).toByte(), (value shr 16).toByte(), (value shr 24).toByte())

// ---------------------------------------------------------------- config

/** Size of `cfg_t` for firmware v4.7+. */
const val CONFIG_LEN = 11

/** Advertising interval steps are 62.5 ms. */
private const val ADV_INTERVAL_STEP = 0.0625
private const val ADV_INTERVAL_MAX = 160
private const val MEASURE_INTERVAL_MIN = 2
private const val MEASURE_INTERVAL_MAX = 25

// Byte offsets in `cfg_t`.
private const val ADV_INTERVAL = 4
private const val MEASURE_INTERVAL = 5
private const val TX_POWER = 6
private const val AVERAGING = 10

/**
 * Device configuration (`cfg_t`). Keeps all bytes so that writing back a modified
 * config leaves fields this type does not interpret unchanged.
 */
class Config(bytes: ByteArray) {
    private val b: ByteArray = bytes.copyOf(CONFIG_LEN)

    fun bytes(): ByteArray = b.copyOf()

    /** Seconds between two advertisements. */
    val advertisingInterval: Double get() = ADV_INTERVAL_STEP * b.u8(ADV_INTERVAL)

    /** A measurement is taken every this many advertisements. */
    val measureInterval: Int get() = b.u8(MEASURE_INTERVAL)

    /** Seconds between two sensor measurements. */
    val measurementPeriod: Double get() = advertisingInterval * measureInterval

    val txPower: TxPower get() = TxPower(b.u8(TX_POWER))

    /** Measurements averaged into one on-device log record; 0 disables the log. */
    val logAveraging: Int get() = b.u8(AVERAGING)

    /** Seconds covered by one log record, or `null` if logging is off. */
    val logPeriod: Double? get() = if (logAveraging == 0) null else measurementPeriod * logAveraging

    /** Sets the advertising interval, rounded to 62.5 ms steps; 62.5 ms ..= 10 s. */
    fun withAdvertisingInterval(seconds: Double): Config {
        val steps = round(seconds / ADV_INTERVAL_STEP)
        if (steps < 1 || steps > ADV_INTERVAL_MAX) {
            throw InvalidValue("advertising interval $seconds s outside 62.5 ms..10 s")
        }
        return with(ADV_INTERVAL, steps.toInt())
    }

    /** 2 ..= 25 advertisements per measurement. */
    fun withMeasureInterval(adverts: Int): Config {
        if (adverts !in MEASURE_INTERVAL_MIN..MEASURE_INTERVAL_MAX) {
            throw InvalidValue("measure interval $adverts outside $MEASURE_INTERVAL_MIN..=$MEASURE_INTERVAL_MAX")
        }
        return with(MEASURE_INTERVAL, adverts)
    }

    fun withTxPower(power: TxPower) = with(TX_POWER, power.code)

    fun withLogAveraging(measurements: Int) = with(AVERAGING, measurements.coerceIn(0, 255))

    private fun with(index: Int, value: Int) = Config(b.copyOf().also { it[index] = value.toByte() })

    override fun equals(other: Any?) = other is Config && other.b.contentEquals(b)
    override fun hashCode() = b.contentHashCode()
    override fun toString() = "Config(${b.joinToString(" ") { "%02x".format(it) }})"
}

/** Radio transmit power, as the firmware's register code. */
@JvmInline
value class TxPower(val code: Int) {
    /** Output power in dBm, if the code is one the firmware documents. */
    val dbm: Float? get() = TX_POWER_TABLE.firstOrNull { it.first == code }?.let { it.second / 100f }

    override fun toString(): String = dbm?.let { String.format(Locale.ROOT, "%+.1f dBm", it) } ?: "code $code"

    companion object {
        /** What the firmware starts with (`RF_POWER_P0p04dBm` in pvvx `def_cfg`), +0.0 dBm. */
        val FACTORY = TxPower(169)

        /** A sensible subset of the table for menus, weakest first. */
        val PRESETS = listOf(140, 150, 160, 169, 182, 191, 33, 63).map(::TxPower)

        /** The documented code closest to [dbm], clamped to the radio's range; weaker on a tie. */
        fun nearest(dbm: Float): TxPower {
            fun distance(centi: Int) = abs(centi / 100f - dbm)
            var best = TX_POWER_TABLE[0]
            for (entry in TX_POWER_TABLE) {
                val closer = distance(entry.second).compareTo(distance(best.second))
                    .let { if (it != 0) it else entry.second.compareTo(best.second) }
                if (closer < 0) best = entry
            }
            return TxPower(best.first)
        }
    }
}

/**
 * `(code, dBm × 100)` from the pvvx flasher page. Codes with bit 7 set use the antenna
 * supply, the others the battery supply (stronger, more current).
 */
internal val TX_POWER_TABLE: List<Pair<Int, Int>> = listOf(
    63 to 1046, 61 to 1029, 58 to 1001, 56 to 981, 53 to 948, 51 to 924, 49 to 897, 47 to 873,
    45 to 844, 43 to 813, 41 to 779, 39 to 741, 37 to 702, 35 to 660, 33 to 614, 31 to 565,
    29 to 513, 27 to 457, 25 to 394, 23 to 323, 191 to 301, 189 to 281, 187 to 261, 185 to 239,
    182 to 199, 180 to 173, 178 to 145, 176 to 117, 174 to 90, 172 to 58, 169 to 4, 168 to -14,
    164 to -97, 162 to -142, 160 to -189, 158 to -248, 156 to -303, 154 to -361, 152 to -426,
    150 to -503, 148 to -581, 146 to -667, 144 to -765, 142 to -865, 140 to -989, 138 to -1140,
    136 to -1329, 134 to -1588, 132 to -1927, 130 to -2518,
)

// ---------------------------------------------------------------- coefficients

internal const val COEFFICIENTS_LEN = 12

/** Largest gain the firmware can apply without overflowing `raw * k` in an i32. */
private const val MAX_GAIN_CODE = 32_767.0

/** A linear correction of reported values: `corrected = gain * reported + offset` (°C or %RH). */
data class Correction(val gain: Double, val offset: Double) {
    companion object {
        val IDENTITY = Correction(1.0, 0.0)
    }
}

/**
 * Sensor conversion coefficients (`sensor_coef_t`). The firmware computes, in
 * hundredths of °C and %RH: `value = (raw * gain) >> 16 + offset`.
 */
data class Coefficients(
    val temperatureGain: Long,
    val humidityGain: Long,
    val temperatureOffset: Int,
    val humidityOffset: Int,
) {
    fun toBytes(): ByteArray =
        le32(temperatureGain) + le32(humidityGain) + le16(temperatureOffset) + le16(humidityOffset)

    /**
     * Coefficients that make the device report the corrections applied to what it
     * reports now: `k' = a·k`, `z' = a·z + 100·b`.
     */
    fun corrected(temperature: Correction, humidity: Correction): Coefficients {
        val (tk, tz) = applyCorrection(temperatureGain, temperatureOffset, temperature)
        val (hk, hz) = applyCorrection(humidityGain, humidityOffset, humidity)
        return Coefficients(tk, hk, tz, hz)
    }

    /** `k;z;k;z` as `calibrations.csv` stores it. */
    fun format() = "$temperatureGain;$temperatureOffset;$humidityGain;$humidityOffset"

    companion object {
        fun fromBytes(b: ByteArray, at: Int = 0) =
            Coefficients(b.u32(at), b.u32(at + 4), b.i16(at + 8), b.i16(at + 10))
    }
}

private fun applyCorrection(gain: Long, offset: Int, c: Correction): Pair<Long, Int> {
    val newGain = round(c.gain * gain)
    val newOffset = round(c.gain * offset + 100.0 * c.offset)
    if (newGain < 1.0 || newGain > MAX_GAIN_CODE) {
        throw InvalidValue("gain %.4f makes coefficient %.0f (max %.0f)".format(c.gain, newGain, MAX_GAIN_CODE))
    }
    if (newOffset < Short.MIN_VALUE || newOffset > Short.MAX_VALUE) {
        throw InvalidValue("offset %.2f makes coefficient %.0f (outside i16)".format(c.offset, newOffset))
    }
    return newGain.toLong() to newOffset.toInt()
}

// ---------------------------------------------------------------- clock

/** Below this the device clock was never set to real time (it counts from boot). */
private const val PLAUSIBLE_SET_TIME = 1_600_000_000L // 2020-09-13
/** Shorter spans since the last set are too short to estimate drift from. */
private const val MIN_DRIFT_SPAN = 3600L
/** Offsets have a resolution of about a second, which over 12 h is 3 s/day. */
private const val MIN_RATE_SPAN = 12.0 * 3600.0

/** System timer ticks (16 MHz) the firmware counts as one second by default. */
const val NOMINAL_SECOND_TICKS = 16_000_000L
/** The firmware takes the second length as a signed 16-bit difference from nominal. */
private const val MAX_TICKS_DELTA = Short.MAX_VALUE.toLong()

/**
 * Device clock compared with the host clock at one moment. Devices count Unix seconds
 * from whenever they were last set (or from boot), and drift by up to minutes per day.
 */
data class ClockReading(
    /** Device time (u32 seconds) when it was read. */
    val deviceNow: Long,
    /** Device time at which the clock was last set, if the firmware tracks it. */
    val lastSet: Long?,
    /** Host time in milliseconds when it was read. */
    val hostNowMs: Long,
) {
    /** Device minus host time in seconds; positive means the device is ahead. */
    val offsetSecs: Double get() = deviceNow - hostNowMs / 1000.0

    private val realSet: Long?
        get() = lastSet?.takeIf { it in PLAUSIBLE_SET_TIME..deviceNow }

    /**
     * Converts a device timestamp to host milliseconds. If the clock was set to real time,
     * the offset grew linearly from zero then to [offsetSecs] now (constant drift);
     * otherwise the current offset applies. `null` for timestamps from before the last
     * set, which belong to an older, unrelated time base.
     */
    fun toHostMs(deviceTime: Long): Long? {
        val set = realSet
        val offset = if (set != null) {
            if (deviceTime < set) return null
            val span = deviceNow - set
            if (span < MIN_DRIFT_SPAN) offsetSecs else offsetSecs * (deviceTime - set) / span
        } else {
            offsetSecs
        }
        val host = deviceTime - offset
        return if (host >= 0) round(host * 1000).toLong() else null
    }

    /**
     * Fraction by which the device clock has run fast (positive) or slow since it was
     * last set to host time; `null` if never set or too recently to tell.
     */
    fun drift(): Double? {
        val set = realSet ?: return null
        val hostSpan = (deviceNow - set) - offsetSecs
        return if (hostSpan >= MIN_RATE_SPAN) offsetSecs / hostSpan else null
    }
}

/** The second length that cancels [drift] measured at [current] ticks; `null` beyond the firmware's range. */
fun trimmedSecondTicks(current: Long, drift: Double): Long? {
    // A clock that gains `drift` per second needs each second to last that much longer.
    val ticks = round(current * (1.0 + drift))
    return if (abs(ticks - NOMINAL_SECOND_TICKS) <= MAX_TICKS_DELTA) ticks.toLong() else null
}

/** The difference from nominal that the firmware's `0x24` command takes. */
internal fun ticksDelta(ticks: Long): Int? =
    (ticks - NOMINAL_SECOND_TICKS).takeIf { it in Short.MIN_VALUE..Short.MAX_VALUE }?.toInt()

// ---------------------------------------------------------------- log

internal const val LOG_RECORD_LEN = 10

/** One record of the on-device measurement log (`memo_blk_t`). */
data class LogRecord(
    /** Device clock (Unix seconds) at the end of the averaging period. */
    val deviceTime: Long,
    val temperature: Float,
    val humidity: Float,
    val voltage: Float,
) {
    companion object {
        fun fromBytes(b: ByteArray, at: Int = 0) = LogRecord(
            deviceTime = b.u32(at),
            temperature = b.i16(at + 4) / 100f,
            humidity = b.u16(at + 6) / 100f,
            voltage = b.u16(at + 8) / 1000f,
        )
    }
}

// ---------------------------------------------------------------- screen

/** Length of the firmware's `external_data_t` for LCD models like LYWSD03MMC. */
internal const val SCREEN_MESSAGE_LEN = 7
/** Validity time that the firmware takes as "until changed". */
private const val FOREVER_SECS = 0xffff

enum class Smiley(val bits: Int) { Off(0), Happy(5), Sad(6) }

/**
 * What to show instead of the readings, alternating with them for [durationSecs].
 * [bigTenths] −995..=19995 (from 1996 on the point is dropped: 3330 shows `333`),
 * [small] −9..=99.
 */
data class ScreenMessage(val bigTenths: Int, val small: Int, val smiley: Smiley, val durationSecs: Int) {
    fun toBytes(): ByteArray {
        if (bigTenths !in -995..19_995) throw InvalidValue("big number $bigTenths is outside -995..=19995 tenths")
        if (small !in -9..99) throw InvalidValue("small number $small is outside -9..=99")
        if (durationSecs !in 0 until FOREVER_SECS) throw InvalidValue("$durationSecs s is too long to show")
        // Flag bits: smiley 0-2, percent 3, battery 4, temperature symbol 5-7 (blank).
        return le16(bigTenths) + le16(small) + le16(durationSecs) + byteArrayOf(smiley.bits.toByte())
    }
}
