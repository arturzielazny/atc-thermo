/*
 * Decoder for BTHome v2 advertisement payloads; a port of `crates/bthome`.
 *
 * BTHome sensors put their readings in the service data of the 16-bit UUID
 * [SERVICE_UUID16]. The payload is one device-information byte followed by a list of
 * `(object id, value)` pairs, little-endian. See <https://bthome.io/format/>.
 */
package dev.artur.thermo.bthome

/** 16-bit service UUID under which BTHome payloads are advertised. */
const val SERVICE_UUID16: Int = 0xfcd2

private const val FLAG_ENCRYPTED = 0x01
private const val FLAG_TRIGGER_BASED = 0x04
private const val VERSION_SHIFT = 5
private const val SUPPORTED_VERSION = 2

/** A decoded BTHome advertisement. */
data class Packet(
    /** The device sends only on events rather than at a fixed interval. */
    val triggerBased: Boolean,
    val payload: Payload,
)

sealed interface Payload {
    /** Decoded objects in the order they appear in the packet. */
    data class Plain(val objects: List<BtObject>) : Payload

    /** AES-CCM encrypted objects; decrypting them needs the device's bind key. */
    class Encrypted(val bytes: ByteArray) : Payload {
        override fun equals(other: Any?) = other is Encrypted && other.bytes.contentEquals(bytes)
        override fun hashCode() = bytes.contentHashCode()
    }
}

/** One measurement from a packet. Values are in the units BTHome defines. */
sealed interface BtObject {
    /** Rolling counter; repeated adverts of one measurement share it. */
    data class PacketId(val id: Int) : BtObject
    /** Battery level in %. */
    data class Battery(val percent: Int) : BtObject
    /** Temperature in °C. */
    data class Temperature(val celsius: Float) : BtObject
    /** Relative humidity in %. */
    data class Humidity(val percent: Float) : BtObject
    data class DewPoint(val celsius: Float) : BtObject
    data class Pressure(val hpa: Float) : BtObject
    data class Illuminance(val lux: Float) : BtObject
    /** Voltage in V. */
    data class Voltage(val volts: Float) : BtObject
    data class Co2(val ppm: Int) : BtObject
    data class Power(val on: Boolean) : BtObject
    data class Opening(val open: Boolean) : BtObject

    /** A valid object this decoder does not interpret, with its raw value bytes. */
    class Other(val id: Int, val raw: ByteArray) : BtObject {
        override fun equals(other: Any?) =
            other is Other && other.id == id && other.raw.contentEquals(raw)
        override fun hashCode() = 31 * id + raw.contentHashCode()
        override fun toString() = "Other(id=$id, raw=${raw.toList()})"
    }
}

/** Why a payload could not be decoded. */
sealed class ParseError(message: String) : Exception(message) {
    data object Empty : ParseError("empty payload")
    data class UnsupportedVersion(val version: Int) :
        ParseError("unsupported BTHome version $version")
    data class UnknownObject(val id: Int, val offset: Int) :
        ParseError("unknown object id 0x%02x at offset %d".format(id, offset))
    data class Truncated(val id: Int, val offset: Int) :
        ParseError("object 0x%02x at offset %d is truncated".format(id, offset))
}

/**
 * Decodes a BTHome v2 service-data payload.
 *
 * @throws ParseError if the payload is empty, not BTHome v2, contains an object id
 * outside the spec, or ends in the middle of an object.
 */
fun parse(data: ByteArray): Packet {
    if (data.isEmpty()) throw ParseError.Empty
    val info = data[0].toInt() and 0xff
    val version = info shr VERSION_SHIFT
    if (version != SUPPORTED_VERSION) throw ParseError.UnsupportedVersion(version)
    val triggerBased = info and FLAG_TRIGGER_BASED != 0
    if (info and FLAG_ENCRYPTED != 0) {
        return Packet(triggerBased, Payload.Encrypted(data.copyOfRange(1, data.size)))
    }
    val objects = mutableListOf<BtObject>()
    var offset = 1
    while (offset < data.size) {
        val id = data[offset].toInt() and 0xff
        val start = offset + 1
        val len = when (val n = valueLen(id)) {
            null -> throw ParseError.UnknownObject(id, offset)
            PREFIXED -> {
                if (start >= data.size) throw ParseError.Truncated(id, offset)
                (data[start].toInt() and 0xff) + 1
            }
            else -> n
        }
        if (start + len > data.size) throw ParseError.Truncated(id, offset)
        objects += decodeObject(id, data.copyOfRange(start, start + len))
        offset = start + len
    }
    return Packet(triggerBased, Payload.Plain(objects))
}

/** First value byte is the length of the rest (text and raw objects). */
private const val PREFIXED = -1

/** Value length of each object id in the BTHome v2 spec. */
private fun valueLen(id: Int): Int? = when (id) {
    0x00, 0x01, 0x09, in 0x0f..0x11, in 0x15..0x2f, 0x3a, 0x46, in 0x57..0x59, 0x60 -> 1
    0x02, 0x03, in 0x06..0x08, in 0x0c..0x0e, in 0x12..0x14, 0x3c, 0x3d, in 0x3f..0x41,
    in 0x43..0x45, in 0x47..0x4a, 0x51, 0x52, 0x56, 0x5a, in 0x5d..0x5f, 0xf0 -> 2
    0x04, 0x05, 0x0a, 0x0b, 0x42, 0x4b, 0xf2 -> 3
    0x3e, in 0x4c..0x50, 0x55, 0x5b, 0x5c, 0xf1 -> 4
    0x53, 0x54 -> PREFIXED
    else -> null
}

private fun decodeObject(id: Int, raw: ByteArray): BtObject {
    // Dividing (rather than multiplying by 0.01) rounds 2244 / 100 to exactly the float nearest 22.44.
    fun u(divisor: Float) = unsigned(raw).toFloat() / divisor
    fun s(divisor: Float) = signed(raw).toFloat() / divisor
    return when (id) {
        0x00 -> BtObject.PacketId(raw[0].toInt() and 0xff)
        0x01 -> BtObject.Battery(raw[0].toInt() and 0xff)
        0x02 -> BtObject.Temperature(s(100f))
        0x45 -> BtObject.Temperature(s(10f))
        0x57 -> BtObject.Temperature(s(1f))
        0x58 -> BtObject.Temperature(signed(raw) * 0.35f)
        0x03 -> BtObject.Humidity(u(100f))
        0x2e -> BtObject.Humidity(u(1f))
        0x08 -> BtObject.DewPoint(s(100f))
        0x04 -> BtObject.Pressure(u(100f))
        0x05 -> BtObject.Illuminance(u(100f))
        0x0c -> BtObject.Voltage(u(1000f))
        0x4a -> BtObject.Voltage(u(10f))
        0x12 -> BtObject.Co2(unsigned(raw).toInt())
        0x10 -> BtObject.Power(raw[0].toInt() != 0)
        0x11 -> BtObject.Opening(raw[0].toInt() != 0)
        else -> BtObject.Other(id, raw)
    }
}

/** Little-endian unsigned integer of up to 4 bytes. */
private fun unsigned(raw: ByteArray): Long =
    raw.foldRight(0L) { b, acc -> (acc shl 8) or (b.toLong() and 0xff) }

/** Little-endian two's-complement integer of up to 4 bytes. */
private fun signed(raw: ByteArray): Int {
    val bits = 32 - 8 * raw.size
    return (unsigned(raw).toInt() shl bits) shr bits
}
