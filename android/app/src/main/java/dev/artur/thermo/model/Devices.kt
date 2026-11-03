/*
 * Latest readings and history of every BTHome device heard so far; a port of
 * `crates/thermo/src/devices.rs`. Times are Unix milliseconds, addresses `AA:BB:..` text.
 */
package dev.artur.thermo.model

import dev.artur.thermo.bthome.BtObject
import dev.artur.thermo.bthome.Payload
import dev.artur.thermo.bthome.parse
import java.util.TreeMap

/** How far back history is kept in memory: the longest calibration window. */
const val HISTORY_SPAN_MS = 48 * 3600_000L
/** Signal strengths (dBm) from which reception counts as reliable, then as marginal. */
const val RSSI_FINE = -75
const val RSSI_MARGINAL = -85
/** Adverts within this span before the latest one make up [Device.typicalRssi]. */
private const val RSSI_SPAN_MS = 5 * 60_000L

/** Where a sample came from. */
enum class Source {
    /** Received in an advert. */
    Live,
    /** Downloaded from the device's own log: an average over a longer period. */
    Log,
}

/** One temperature and humidity measurement. */
data class Sample(
    val at: Long,
    val temperature: Float,
    val humidity: Float,
    /** Signal strength of the advert that carried it; `null` for log records. */
    val rssi: Int?,
    val source: Source,
)

/** A record from a device's log, already mapped to host time. */
data class LoggedSample(
    /** Middle of the averaging period. */
    val at: Long,
    val temperature: Float,
    val humidity: Float,
    val voltage: Float,
    /** Length of the averaging period. */
    val spanMs: Long,
)

/** A sample together with the device context it was taken in; the unit of persistence. */
data class Measurement(
    val address: String,
    val name: String,
    val sample: Sample,
    val battery: Int?,
    val voltage: Float?,
)

/** One BTHome advertisement, still undecoded. */
class Advert(
    val address: String,
    val name: String,
    val rssi: Int?,
    val payload: ByteArray,
    val received: Long,
)

/** A thermometer and its most recent values. */
class Device internal constructor(val address: String, var name: String, seen: Long) {
    var rssi: Int? = null
        internal set
    var temperature: Float? = null
        internal set
    var humidity: Float? = null
        internal set
    var battery: Int? = null
        internal set
    var voltage: Float? = null
        internal set
    var lastSeen: Long = seen
        internal set
    /** The device sends encrypted adverts, which need a bind key to read. */
    var encrypted = false
        internal set
    private var lastSamplePacket: Int? = null
    /** Measurements, oldest first. */
    val history: ArrayList<Sample> = ArrayList()
    /** Every advert's signal strength within [RSSI_SPAN_MS], oldest first. */
    private val recentRssi = ArrayDeque<Pair<Long, Int>>()

    /**
     * Median signal strength of the adverts in the last 5 minutes the device was heard,
     * or the latest one known. Single adverts jump by several dB; the median shows the usual level.
     */
    val typicalRssi: Int? get() = medianRssi(recentRssi.map { it.second }) ?: rssi

    internal fun noteRssi(at: Long, value: Int) {
        recentRssi.addLast(at to value)
        while (recentRssi.isNotEmpty() && at - recentRssi.first().first > RSSI_SPAN_MS) recentRssi.removeFirst()
    }

    /** Applies one packet's objects; returns the new sample if this was a new measurement. */
    internal fun apply(objects: List<BtObject>, at: Long, rssi: Int?): Sample? {
        var packetId: Int? = null
        var t: Float? = null
        var h: Float? = null
        for (o in objects) when (o) {
            is BtObject.PacketId -> packetId = o.id
            is BtObject.Temperature -> t = o.celsius
            is BtObject.Humidity -> h = o.percent
            is BtObject.Battery -> battery = o.percent
            is BtObject.Voltage -> voltage = o.volts
            else -> {}
        }
        temperature = t ?: temperature
        humidity = h ?: humidity
        if (t == null || h == null) return null
        // Devices repeat each measurement in several adverts with the same packet id.
        if (packetId != null && packetId == lastSamplePacket) return null
        lastSamplePacket = packetId
        val sample = Sample(at, t, h, rssi, Source.Live)
        insert(sample)
        return sample
    }

    /** Inserts in time order and drops samples older than [HISTORY_SPAN_MS] before the newest. */
    internal fun insert(sample: Sample) {
        history.add(partitionPoint { it.at <= sample.at }, sample)
        val newest = history.last().at
        var drop = 0
        while (drop < history.size && newest - history[drop].at > HISTORY_SPAN_MS) drop++
        if (drop > 0) history.subList(0, drop).clear()
    }

    /** Index of the first sample for which [before] is false; history is sorted by time. */
    fun partitionPoint(before: (Sample) -> Boolean): Int {
        var lo = 0
        var hi = history.size
        while (lo < hi) {
            val mid = (lo + hi) ushr 1
            if (before(history[mid])) lo = mid + 1 else hi = mid
        }
        return lo
    }

    /** Whether any sample lies within [radius] of [at]. */
    internal fun hasSampleNear(at: Long, radius: Long): Boolean {
        val first = partitionPoint { it.at < at - radius }
        return first < history.size && history[first].at <= at + radius
    }

    /** Whether history between [since] and [until] has a hole longer than [maxGap]. */
    fun hasGap(since: Long, until: Long, maxGap: Long): Boolean {
        var previous = since
        for (i in partitionPoint { it.at < since } until history.size) {
            val s = history[i]
            if (s.at > until) break
            if (s.at - previous > maxGap) return true
            previous = s.at
        }
        return until - previous > maxGap
    }
}

/** Median of signal strengths in dBm, rounded down; `null` for none. */
fun medianRssi(values: List<Int>): Int? {
    if (values.isEmpty()) return null
    val sorted = values.sorted()
    val mid = sorted.size / 2
    return if (sorted.size % 2 == 0) Math.floorDiv(sorted[mid - 1] + sorted[mid], 2) else sorted[mid]
}

/** All known devices, ordered by address so the order (and colors) are stable between runs. */
class Devices {
    private val byAddress = TreeMap<String, Device>()

    /**
     * Decodes an advert and updates the sending device; returns the new measurement when
     * the advert carried one not seen before.
     *
     * @throws dev.artur.thermo.bthome.ParseError if the payload is not BTHome v2. The
     * device's name, signal strength and last-seen time are still updated.
     */
    fun record(advert: Advert): Measurement? {
        val device = byAddress.getOrPut(advert.address) { Device(advert.address, advert.name, advert.received) }
        device.name = advert.name
        device.rssi = advert.rssi ?: device.rssi
        advert.rssi?.let { device.noteRssi(advert.received, it) }
        device.lastSeen = advert.received
        val objects = when (val payload = parse(advert.payload).payload) {
            is Payload.Encrypted -> {
                device.encrypted = true
                return null
            }
            is Payload.Plain -> payload.objects
        }
        device.encrypted = false
        val sample = device.apply(objects, advert.received, advert.rssi) ?: return null
        return Measurement(device.address, device.name, sample, device.battery, device.voltage)
    }

    /** Adds a measurement loaded from disk. Call in chronological order. */
    fun restore(m: Measurement) {
        val device = byAddress.getOrPut(m.address) { Device(m.address, m.name, m.sample.at) }
        device.name = m.name
        if (m.sample.at >= device.lastSeen) {
            device.temperature = m.sample.temperature
            device.humidity = m.sample.humidity
            device.battery = m.battery ?: device.battery
            device.voltage = m.voltage ?: device.voltage
            m.sample.rssi?.let {
                device.rssi = it
                device.noteRssi(m.sample.at, it)
            }
        }
        device.lastSeen = maxOf(device.lastSeen, m.sample.at)
        device.insert(m.sample)
    }

    /**
     * Adds downloaded log records where history has no sample within half a record's
     * span; returns the added ones so they can be saved.
     */
    fun mergeLog(address: String, records: List<LoggedSample>): List<Measurement> {
        val device = byAddress[address] ?: return emptyList()
        val added = mutableListOf<Measurement>()
        for (r in records) {
            if (device.hasSampleNear(r.at, r.spanMs / 2)) continue
            val sample = Sample(r.at, r.temperature, r.humidity, null, Source.Log)
            device.insert(sample)
            added += Measurement(address, device.name, sample, null, r.voltage)
        }
        return added
    }

    operator fun get(address: String): Device? = byAddress[address]

    fun rename(address: String, name: String) {
        byAddress[address]?.name = name
    }

    fun all(): Collection<Device> = byAddress.values

    val size: Int get() = byAddress.size
}
