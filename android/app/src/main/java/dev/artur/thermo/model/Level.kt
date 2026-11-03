/*
 * Signal levelling: TX power changes that bring every device to about the same signal
 * strength at this phone, by default the weakest that is still reliable, so no device
 * spends more battery on its radio than it needs; a port of `crates/thermo/src/level.rs`.
 */
package dev.artur.thermo.model

import kotlin.math.abs

/** Fewer adverts than this give too rough a median: single adverts scatter by several dB. */
const val MIN_ADVERTS = 20
/** Smaller changes are skipped: medians differ by about this much, and every change is a flash write. */
const val TOLERANCE_DB = 2
/** Median to aim for by default: a margin above where reception stops counting as reliable. */
const val DEFAULT_TARGET = RSSI_FINE + 5
/** Targets beyond these are not useful. */
val TARGET_RANGE = -95..-30

/** Signal strengths heard while levelling, and the level to aim for. */
class Leveling(var startedAt: Long) {
    /** Every advert's signal strength since [startedAt] or the device's last TX power change. */
    private val samples = HashMap<String, MutableList<Int>>()

    /** Median signal strength to aim for, in dBm. */
    var target: Int = DEFAULT_TARGET
        set(value) {
            field = value.coerceIn(TARGET_RANGE)
        }

    fun record(address: String, rssi: Int) {
        samples.getOrPut(address) { mutableListOf() } += rssi
    }

    /** Drops a device's samples after its TX power changed; they no longer apply. */
    fun forget(address: String) {
        samples.remove(address)
    }

    /** Starts observing anew, keeping the target. */
    fun restart(now: Long) {
        startedAt = now
        samples.clear()
    }

    /** Proposed TX power changes for [devices], in that order. */
    fun plan(devices: List<String>): Plan = Plan(devices.map { address ->
        val heard = samples[address].orEmpty()
        val median = if (heard.size >= MIN_ADVERTS) medianRssi(heard) else null
        Plan.Row(
            address = address,
            adverts = heard.size,
            median = median,
            change = median?.let { target - it }?.takeIf { abs(it) >= TOLERANCE_DB },
        )
    })
}

/** What levelling would do to the devices. */
data class Plan(val rows: List<Row>) {
    /** Whether some device has been heard often enough to have a median. */
    val hasMedians: Boolean get() = rows.any { it.median != null }

    data class Row(
        val address: String,
        val adverts: Int,
        val median: Int?,
        /** TX power change in dB; `null` while heard too rarely or already within tolerance. */
        val change: Int?,
    )
}
