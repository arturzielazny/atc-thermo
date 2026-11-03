/*
 * Ports of the unit tests of `crates/bthome` and `crates/atc`, with the same fixtures
 * captured from LYWSD03MMC thermometers on pvvx firmware v5.9.
 */
package dev.artur.thermo

import dev.artur.thermo.atc.Coefficients
import dev.artur.thermo.atc.Config
import dev.artur.thermo.atc.ClockReading
import dev.artur.thermo.atc.Correction
import dev.artur.thermo.atc.LogRecord
import dev.artur.thermo.atc.NOMINAL_SECOND_TICKS
import dev.artur.thermo.atc.ScreenMessage
import dev.artur.thermo.atc.Smiley
import dev.artur.thermo.atc.TX_POWER_TABLE
import dev.artur.thermo.atc.TxPower
import dev.artur.thermo.atc.clockFromReply
import dev.artur.thermo.atc.decodeName
import dev.artur.thermo.atc.ticksDelta
import dev.artur.thermo.atc.trimmedSecondTicks
import dev.artur.thermo.bthome.BtObject
import dev.artur.thermo.bthome.ParseError
import dev.artur.thermo.bthome.Payload
import dev.artur.thermo.bthome.parse
import dev.artur.thermo.model.identifyMessage
import kotlin.math.abs
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNull
import kotlin.test.assertTrue

fun hex(s: String): ByteArray = ByteArray(s.length / 2) { s.substring(2 * it, 2 * it + 2).toInt(16).toByte() }

fun bytes(vararg values: Int) = ByteArray(values.size) { values[it].toByte() }

class BtHomeTest {
    private fun objects(s: String) = (parse(hex(s)).payload as Payload.Plain).objects

    @Test
    fun pvvxMeasurementPacket() {
        assertEquals(
            listOf(BtObject.PacketId(0xa4), BtObject.Battery(87), BtObject.Temperature(22.44f), BtObject.Humidity(56.94f)),
            objects("4000a4015702c408033e16"),
        )
    }

    @Test
    fun pvvxVoltageAndSwitchPacket() {
        assertEquals(
            listOf(BtObject.PacketId(0xcc), BtObject.Voltage(2.961f), BtObject.Power(false), BtObject.Opening(true)),
            objects("4000cc0c910b10001101"),
        )
    }

    @Test
    fun negativeTemperature() {
        assertEquals(listOf(BtObject.Temperature(-5.25f)), objects("4002f3fd"))
        assertEquals(listOf(BtObject.Temperature(-5.0f)), objects("4057fb"))
    }

    @Test
    fun uninterpretedObjectsKeepTheirBytes() {
        assertEquals(listOf(BtObject.Other(0x42, bytes(0xa0, 0x86, 0x01))), objects("4042a08601"))
        assertEquals(listOf(BtObject.Other(0x53, bytes(3, 'a'.code, 'b'.code, 'c'.code))), objects("405303616263"))
    }

    @Test
    fun triggerFlagAndEncryption() {
        assertTrue(parse(hex("443a01")).triggerBased)
        assertEquals(Payload.Encrypted(bytes(0xaa, 0xbb, 0xcc)), parse(hex("41aabbcc")).payload)
    }

    @Test
    fun errors() {
        assertFailsWith<ParseError.Empty> { parse(ByteArray(0)) }
        assertEquals(ParseError.UnsupportedVersion(1), assertFailsWith<ParseError> { parse(hex("2000")) })
        assertEquals(ParseError.UnknownObject(0x7f, 3), assertFailsWith<ParseError> { parse(hex("4000017f01")) })
        assertEquals(ParseError.Truncated(0x02, 3), assertFailsWith<ParseError> { parse(hex("400001020a")) })
        assertEquals(ParseError.Truncated(0x53, 1), assertFailsWith<ParseError> { parse(hex("4053")) })
    }
}

class ConfigTest {
    // Factory config read from a LYWSD03MMC B1.6 on firmware v5.9.
    private val factory = bytes(0x87, 0x10, 0x0a, 0x06, 0x28, 0x04, 0xa9, 0x31, 0x31, 0x04, 0xb4)

    @Test
    fun decodesFactoryConfig() {
        val config = Config(factory)
        assertEquals(2.5, config.advertisingInterval)
        assertEquals(4, config.measureInterval)
        assertEquals(10.0, config.measurementPeriod)
        assertEquals("+0.0 dBm", config.txPower.toString())
        assertEquals(TxPower.FACTORY, config.txPower)
        assertEquals(1800.0, config.logPeriod)
    }

    @Test
    fun settersTouchOnlyTheirByte() {
        val config = Config(factory).withAdvertisingInterval(5.0).withMeasureInterval(6).withTxPower(TxPower(191))
        val expected = factory.copyOf().also {
            it[4] = 80
            it[5] = 6
            it[6] = 191.toByte()
        }
        assertContentEquals(expected, config.bytes())
    }

    @Test
    fun rejectsOutOfRange() {
        val config = Config(factory)
        assertFailsWith<Exception> { config.withAdvertisingInterval(11.0) }
        assertFailsWith<Exception> { config.withAdvertisingInterval(0.0) }
        assertFailsWith<Exception> { config.withMeasureInterval(1) }
    }

    @Test
    fun txPowers() {
        assertTrue(TxPower.PRESETS.all { it.dbm != null })
        assertEquals("code 7", TxPower(7).toString())
        assertEquals(TxPower(169), TxPower.nearest(0f))
        assertEquals(TxPower(150), TxPower.nearest(-5.2f))
        assertEquals(TxPower(63), TxPower.nearest(40f))
        assertEquals(TxPower(130), TxPower.nearest(-40f))
        for ((code, _) in TX_POWER_TABLE) assertEquals(TxPower(code), TxPower.nearest(TxPower(code).dbm!!))
    }
}

class CoefficientsTest {
    private val sht4x = Coefficients(17_500, 12_500, -4_500, -600)

    /** What the firmware would report for a raw reading, in °C / %RH. */
    private fun firmware(gain: Long, offset: Int, raw: Int) = (((raw.toLong() * gain) shr 16) + offset) / 100.0

    @Test
    fun byteRoundTrip() {
        val b = sht4x.toBytes()
        assertContentEquals(bytes(0x5c, 0x44, 0, 0, 0xd4, 0x30, 0, 0, 0x6c, 0xee, 0xa8, 0xfd), b)
        assertEquals(sht4x, Coefficients.fromBytes(b))
        assertEquals(sht4x, sht4x.corrected(Correction.IDENTITY, Correction.IDENTITY))
    }

    @Test
    fun correctionChangesFirmwareOutputAsRequested() {
        val t = Correction(1.02, -0.35)
        val h = Correction(0.97, 2.1)
        val new = sht4x.corrected(t, h)
        for (raw in listOf(20_000, 26_000, 32_000, 40_000)) {
            val wantT = t.gain * firmware(sht4x.temperatureGain, sht4x.temperatureOffset, raw) + t.offset
            assertTrue(abs(firmware(new.temperatureGain, new.temperatureOffset, raw) - wantT) < 0.02)
            val wantH = h.gain * firmware(sht4x.humidityGain, sht4x.humidityOffset, raw) + h.offset
            assertTrue(abs(firmware(new.humidityGain, new.humidityOffset, raw) - wantH) < 0.02)
        }
    }

    @Test
    fun rejectsUnrepresentable() {
        assertFailsWith<Exception> { sht4x.corrected(Correction(2.0, 0.0), Correction.IDENTITY) }
        assertFailsWith<Exception> { sht4x.corrected(Correction(1.0, 400.0), Correction.IDENTITY) }
    }
}

class ClockTest {
    private val hostNow = 1_793_046_000L

    @Test
    fun constantOffsetWhenNeverSet() {
        val clock = ClockReading(1000, 0, hostNow * 1000)
        assertEquals((hostNow - 600) * 1000, clock.toHostMs(400))
    }

    @Test
    fun driftInterpolatesFromLastSet() {
        val set = hostNow - 86_400
        val clock = ClockReading(hostNow + 120, set, hostNow * 1000)
        assertTrue(abs(clock.offsetSecs - 120.0) < 1e-6)
        val halfway = set + 43_200 + 60
        assertTrue(abs(clock.toHostMs(halfway)!! - (set + 43_200) * 1000) < 10)
        assertNull(clock.toHostMs(set - 10))
    }

    @Test
    fun driftNeedsARealSetLongEnoughAgo() {
        val clock = ClockReading(hostNow + 40, hostNow - 2 * 86_400, hostNow * 1000)
        assertTrue(abs(clock.drift()!! * 86_400.0 - 20.0) < 1e-9)
        assertNull(clock.copy(lastSet = hostNow - 3600).drift())
        assertNull(clock.copy(deviceNow = 100_000, lastSet = 0).drift())
    }

    @Test
    fun trimmingLengthensSecondsOfFastClocks() {
        val drift = 20.0 / 86_400.0
        val ticks = trimmedSecondTicks(NOMINAL_SECOND_TICKS, drift)!!
        assertEquals(NOMINAL_SECOND_TICKS + 3704, ticks)
        assertEquals(3704, ticksDelta(ticks))
        assertEquals(ticks - 185, trimmedSecondTicks(ticks, -1.0 / 86_400.0))
        assertEquals(-3704, ticksDelta(trimmedSecondTicks(NOMINAL_SECOND_TICKS, -drift)!!))
        assertNull(trimmedSecondTicks(NOMINAL_SECOND_TICKS, 0.003))
    }

    @Test
    fun recentSetUsesConstantOffset() {
        val clock = ClockReading(hostNow + 5, hostNow - 100, hostNow * 1000)
        assertEquals((hostNow - 55) * 1000, clock.toHostMs(hostNow - 50))
    }
}

class RepliesTest {
    @Test
    fun parsesReplies() {
        val clock = clockFromReply(bytes(0x23, 1, 0, 0, 0, 2, 0, 0, 0), 0)
        assertEquals(1, clock.deviceNow)
        assertEquals(2L, clock.lastSet)
        assertNull(clockFromReply(bytes(0x23, 1, 0, 0, 0), 0).lastSet)
        assertEquals("Kitchen", decodeName(byteArrayOf(1) + "Kitchen\u0000\u0000".toByteArray(), 1))
    }

    @Test
    fun decodesLogRecord() {
        assertEquals(
            LogRecord(0x6adf_2e70, 22.44f, 56.94f, 2.961f),
            LogRecord.fromBytes(bytes(0x70, 0x2e, 0xdf, 0x6a, 0xc4, 0x08, 0x3e, 0x16, 0x91, 0x0b)),
        )
    }

    @Test
    fun screenMessages() {
        assertContentEquals(
            bytes(0x02, 0x0d, 0x03, 0x00, 0x58, 0x02, 0x05),
            ScreenMessage(3330, 3, Smiley.Happy, 600).toBytes(),
        )
        assertContentEquals(bytes(0xfb, 0xff, 0xff, 0xff, 0, 0, 0), ScreenMessage(-5, -1, Smiley.Off, 0).toBytes())
        val ok = ScreenMessage(0, 0, Smiley.Off, 1)
        for (bad in listOf(ok.copy(bigTenths = 20_000), ok.copy(small = 100), ok.copy(durationSecs = 65_535))) {
            assertFailsWith<Exception> { bad.toBytes() }
        }
    }

    @Test
    fun identifyNumbersCannotPassForReadings() {
        assertEquals(3330, identifyMessage(3).bigTenths)
        assertEquals(12_120, identifyMessage(12).bigTenths)
        assertEquals(3, identifyMessage(3).small)
        assertEquals(99, identifyMessage(200).small)
    }
}
