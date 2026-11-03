/*
 * Measurement history on disk: append-only CSV, one file per UTC day; a port of
 * `crates/thermo/src/store.rs` with the same file formats, so a phone's files can be
 * copied into `~/.local/share/thermo/` and back.
 */
package dev.artur.thermo.model

import dev.artur.thermo.atc.Coefficients
import dev.artur.thermo.atc.Correction
import java.io.File
import java.io.RandomAccessFile
import java.time.Instant
import java.time.ZoneOffset
import java.util.Locale

private const val HEADER =
    "time_ms,address,name,temperature_c,humidity_pct,battery_pct,voltage_v,source,rssi_dbm"
private const val CALIBRATION_FILE = "calibrations.csv"
private const val DOWNLOAD_FILE = "downloads.csv"
private const val DOWNLOAD_HEADER = "time_ms,address"
private const val CLOCK_FILE = "clocks.csv"
private const val CLOCK_HEADER = "time_ms,address,offset_s"
private const val CALIBRATION_HEADER =
    "time_ms,address,name,action,temperature_gain,temperature_offset,humidity_gain,humidity_offset,coefficients_before,coefficients_after"
private const val MS_PER_DAY = 86_400_000L

/** `String.format` with `.` decimals whatever the phone's language, as the files need. */
internal fun String.fmt(vararg args: Any?): String = String.format(Locale.ROOT, this, *args)

/** Measurements read back from disk. */
class Loaded(
    /** Oldest first. */
    val measurements: List<Measurement>,
    /** Lines that could not be parsed, e.g. cut off by a crash mid-write. */
    val skippedLines: Int,
)

/** One change of a device's sensor coefficients. */
data class CalibrationRecord(
    val at: Long,
    val address: String,
    val name: String,
    /** The applied `(temperature, humidity)` corrections; `null` for a factory reset. */
    val corrections: Pair<Correction, Correction>?,
    val before: Coefficients,
    val after: Coefficients,
)

class Store(val dir: File) {
    init {
        dir.mkdirs()
    }

    /** Reads every stored measurement taken at or after [since]. */
    fun load(since: Long, now: Long = System.currentTimeMillis()): Loaded {
        val measurements = mutableListOf<Measurement>()
        var skipped = 0
        for (day in dayOf(since)..dayOf(now)) {
            val file = File(dir, fileName(day))
            if (!file.exists()) continue
            file.forEachLine { line ->
                // Files written before the `source` column have a shorter header.
                if (line.isEmpty() || line.startsWith("time_ms,")) return@forEachLine
                val m = parseLine(line)
                when {
                    m == null -> skipped++
                    m.sample.at >= since -> measurements += m
                }
            }
        }
        measurements.sortBy { it.sample.at }
        return Loaded(measurements, skipped)
    }

    /** Appends measurements, each to the file of the day it was taken. */
    fun append(measurements: List<Measurement>) {
        for ((day, group) in measurements.groupBy { dayOf(it.sample.at) }) {
            val file = openForAppend(File(dir, fileName(day)), HEADER)
            // One line per measurement, so a crash can at worst leave one partial line.
            file.appendText(group.joinToString("") { formatLine(it) + "\n" })
        }
    }

    /** Appends a coefficient change to `calibrations.csv`. */
    fun recordCalibration(record: CalibrationRecord) {
        val file = openForAppend(File(dir, CALIBRATION_FILE), CALIBRATION_HEADER)
        val (action, corrections) = when (val c = record.corrections) {
            null -> "reset" to ",,,"
            else -> "apply" to "%.6f,%.4f,%.6f,%.4f".fmt(c.first.gain, c.first.offset, c.second.gain, c.second.offset)
        }
        file.appendText(
            "${record.at},${record.address},${sanitize(record.name)},$action,$corrections," +
                "${record.before.format()},${record.after.format()}\n"
        )
    }

    /** Time of the most recent coefficient change per device. */
    fun lastCalibrations(): Map<String, Long> = latestRows(File(dir, CALIBRATION_FILE)).mapValues { it.value.first }

    /** Notes that a device's log was downloaded up to [at]. */
    fun recordDownload(address: String, at: Long) {
        openForAppend(File(dir, DOWNLOAD_FILE), DOWNLOAD_HEADER).appendText("$at,$address\n")
    }

    /** Time of the most recent log download per device. */
    fun lastDownloads(): Map<String, Long> = latestRows(File(dir, DOWNLOAD_FILE)).mapValues { it.value.first }

    /** Notes a device's clock offset (device minus host, seconds) measured at [at]. */
    fun recordClockOffset(address: String, at: Long, offset: Double) {
        openForAppend(File(dir, CLOCK_FILE), CLOCK_HEADER).appendText("$at,$address,${"%.1f".fmt(offset)}\n")
    }

    /** The most recently measured clock offset per device. */
    fun lastClockOffsets(): Map<String, Double> =
        latestRows(File(dir, CLOCK_FILE)).mapNotNull { (address, row) ->
            row.second.toDoubleOrNull()?.let { address to it }
        }.toMap()

    /**
     * Merges [text], the contents of a file named [name] from another copy of this store
     * (e.g. the desktop app's), into the file of the same name; lines already there are
     * skipped. Returns the measurements that were new, or `null` for a file that isn't
     * part of a store.
     */
    fun import(name: String, text: String): List<Measurement>? {
        val header = when {
            DAY_FILE.matches(name) -> HEADER
            name == CALIBRATION_FILE -> CALIBRATION_HEADER
            name == DOWNLOAD_FILE -> DOWNLOAD_HEADER
            name == CLOCK_FILE -> CLOCK_HEADER
            else -> return null
        }
        val file = File(dir, name)
        val existing = if (file.exists()) file.readLines().toHashSet() else HashSet()
        val added = text.lineSequence()
            .filter { it.isNotEmpty() && !it.startsWith("time_ms,") && it !in existing }
            .distinct()
            .toList()
        if (added.isEmpty()) return emptyList()
        openForAppend(file, header).appendText(added.joinToString("") { "$it\n" })
        return if (header == HEADER) added.mapNotNull(::parseLine) else emptyList()
    }

    /** Every file of the store, for sharing. */
    fun files(): List<File> = dir.listFiles()?.filter { it.isFile && it.name.endsWith(".csv") }?.sortedBy { it.name }
        ?: emptyList()
}

/**
 * Reads a CSV whose first two columns are `time_ms,address`; returns the newest line per
 * address as its time and the columns after the address.
 */
private fun latestRows(file: File): Map<String, Pair<Long, String>> {
    if (!file.exists()) return emptyMap()
    val latest = HashMap<String, Pair<Long, String>>()
    file.forEachLine { line ->
        val fields = line.split(',', limit = 3)
        val at = fields.getOrNull(0)?.toLongOrNull() ?: return@forEachLine
        val address = fields.getOrNull(1)?.takeIf { isAddress(it) } ?: return@forEachLine
        val rest = fields.getOrElse(2) { "" }
        if ((latest[address]?.first ?: Long.MIN_VALUE) <= at) latest[address] = at to rest
    }
    return latest
}

private val DAY_FILE = Regex("^\\d{4}-\\d{2}-\\d{2}\\.csv$")
private val ADDRESS = Regex("^[0-9A-Fa-f]{2}(:[0-9A-Fa-f]{2}){5}$")

internal fun isAddress(text: String) = ADDRESS.matches(text)

/** Opens a CSV file for appending, writing [header] if it is new. */
private fun openForAppend(file: File, header: String): File {
    upgradeHeader(file, header)
    if (!file.exists() || file.length() == 0L) {
        file.writeText("$header\n")
    } else if (lastByte(file) != '\n'.code) {
        // Terminate a line torn by a crash so the next record starts cleanly.
        file.appendText("\n")
    }
    return file
}

/**
 * Replaces a file's header with [header] when that only adds columns at the end, so
 * lines written with the new columns still match the header.
 */
internal fun upgradeHeader(file: File, header: String) {
    if (!file.exists()) return
    val contents = file.readText()
    val first = contents.substringBefore('\n')
    val rest = if ('\n' in contents) contents.substringAfter('\n') else ""
    val outdated = first != header && header.startsWith(first) && header.removePrefix(first).startsWith(",")
    if (!outdated) return
    // Write beside it and rename, so a crash leaves either the old or the new file.
    val temporary = File(file.parentFile, file.name + ".tmp")
    temporary.writeText("$header\n$rest")
    temporary.renameTo(file)
}

private fun lastByte(file: File): Int = RandomAccessFile(file, "r").use {
    it.seek(it.length() - 1)
    it.read()
}

private fun sanitize(name: String) = name.map { if (it == ',' || it.isISOControl()) ' ' else it }.joinToString("")

internal fun formatLine(m: Measurement): String {
    val battery = m.battery?.toString() ?: ""
    val voltage = m.voltage?.let { "%.3f".fmt(it) } ?: ""
    val source = if (m.sample.source == Source.Log) "log" else ""
    val rssi = m.sample.rssi?.toString() ?: ""
    return "${m.sample.at},${m.address},${sanitize(m.name)},${"%.2f".fmt(m.sample.temperature)}," +
        "${"%.2f".fmt(m.sample.humidity)},$battery,$voltage,$source,$rssi"
}

internal fun parseLine(line: String): Measurement? {
    val f = line.split(',')
    if (f.size < 7) return null
    val at = f[0].toLongOrNull() ?: return null
    val address = f[1].takeIf { isAddress(it) } ?: return null
    val temperature = f[3].toFloatOrNull() ?: return null
    val humidity = f[4].toFloatOrNull() ?: return null
    val battery = optional(f[5]) { it.toIntOrNull() } ?: return null
    val voltage = optional(f[6]) { it.toFloatOrNull() } ?: return null
    // Columns added later are missing from older lines.
    val source = when (f.getOrNull(7)) {
        null, "" -> Source.Live
        "log" -> Source.Log
        else -> return null
    }
    val rssi = optional(f.getOrElse(8) { "" }) { it.toIntOrNull() } ?: return null
    return Measurement(address, f[2], Sample(at, temperature, humidity, rssi.value, source), battery.value, voltage.value)
}

/** A field that may be left empty for "unknown"; `null` if it doesn't parse. */
private class Optional<T>(val value: T?)

private fun <T> optional(field: String, parse: (String) -> T?): Optional<T>? =
    if (field.isEmpty()) Optional(null) else parse(field)?.let { Optional(it) }

internal fun dayOf(ms: Long): Long = Math.floorDiv(ms, MS_PER_DAY)

internal fun fileName(day: Long): String {
    val date = Instant.ofEpochMilli(day * MS_PER_DAY).atOffset(ZoneOffset.UTC).toLocalDate()
    return "%04d-%02d-%02d.csv".fmt(date.year, date.monthValue, date.dayOfMonth)
}
