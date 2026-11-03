/*
 * Client for the BLE command interface of the pvvx firmware, on Android's BluetoothGatt;
 * the transport half of `crates/atc/src/thermometer.rs`.
 *
 * The firmware exposes one GATT characteristic (UUID `0x1F1F`) that takes commands as
 * writes and answers with notifications whose first byte echoes the command id.
 */
package dev.artur.thermo.atc

import android.annotation.SuppressLint
import android.bluetooth.BluetoothAdapter
import android.bluetooth.BluetoothDevice
import android.bluetooth.BluetoothGatt
import android.bluetooth.BluetoothGattCallback
import android.bluetooth.BluetoothGattCharacteristic
import android.bluetooth.BluetoothGattDescriptor
import android.bluetooth.BluetoothProfile
import android.content.Context
import android.os.Build
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.withTimeoutOrNull
import java.util.UUID

/** The firmware's command/notify characteristic `0x1F1F`. */
private val COMMAND_UUID: UUID = UUID.fromString("00001f1f-0000-1000-8000-00805f9b34fb")
/** Client characteristic configuration descriptor, to switch notifications on. */
private val CCCD_UUID: UUID = UUID.fromString("00002902-0000-1000-8000-00805f9b34fb")

private const val CMD_DEV_ID = 0x00
private const val CMD_NAME = 0x01
private const val CMD_SCREEN = 0x22
private const val CMD_CLOCK = 0x23
private const val CMD_SECOND_TICKS = 0x24
private const val CMD_COEFFICIENTS = 0x25
private const val CMD_COEFFICIENTS_DEFAULT = 0x26
private const val CMD_LOG = 0x35
private const val CMD_CONFIG = 0x55

/** Oldest firmware whose config layout this client knows (BCD, v4.7). */
private const val MIN_VERSION = 0x47
/** Longest name the firmware stores. */
const val MAX_NAME_LEN = 20

private const val CONNECT_ATTEMPTS = 4
private const val CONNECT_TIMEOUT_MS = 25_000L
private const val CONNECT_RETRY_PAUSE_MS = 1_000L
/** Each GATT operation (discovery, descriptor write, ...) gets this long. */
private const val GATT_OP_TIMEOUT_MS = 10_000L
/** A name reply is 21 bytes, more than the default 20-byte payload. */
private const val WANTED_MTU = 64
/** Writes can get lost, so a command is resent if no answer arrives in time. */
private const val COMMAND_ATTEMPTS = 5
private const val REPLY_TIMEOUT_MS = 1_500L
/** Longest pause between two log records before the download counts as stalled. */
private const val LOG_RECORD_TIMEOUT_MS = 5_000L

/** Why talking to a thermometer failed. */
open class AtcException(message: String) : Exception(message)
class CommandTimeout(val command: Int) : AtcException("no answer to command 0x%02x".format(command))
class Disconnected : AtcException("device disconnected")

/** Identity reported by the firmware (`CMD_ID_DEV_ID`). */
data class DeviceInfo(val hardware: Int, val firmware: Int) {
    /** Firmware version as text, e.g. `"5.9"`. */
    val firmwareVersion: String get() = "${firmware shr 4}.${firmware and 0xf}"
}

/** An open connection to a thermometer running pvvx firmware. One command at a time. */
@SuppressLint("MissingPermission")
class Thermometer private constructor(
    private val gatt: BluetoothGatt,
    private val callback: Callback,
    private val command: BluetoothGattCharacteristic,
) {
    var info = DeviceInfo(0, 0)
        private set

    val address: String get() = gatt.device.address

    private val mutex = Mutex()

    suspend fun name(): String = decodeName(command(byteArrayOf(CMD_NAME.toByte()), 1), 1)

    /**
     * Stores a new name; the device uses it once disconnected. An empty name restores
     * the default `ATC_xxxxxx`. Returns the name as stored.
     */
    suspend fun setName(name: String): String {
        val bytes = name.toByteArray(Charsets.UTF_8)
        if (bytes.size > MAX_NAME_LEN) throw InvalidValue("name is ${bytes.size} bytes, max $MAX_NAME_LEN")
        val payload = byteArrayOf(CMD_NAME.toByte()) + if (bytes.isEmpty()) byteArrayOf(0) else bytes
        return decodeName(command(payload, 1), 1)
    }

    /** Config replies are `[0x55, firmware version, cfg_t...]`. */
    suspend fun config(): Config =
        Config(command(byteArrayOf(CMD_CONFIG.toByte()), 2 + CONFIG_LEN).copyOfRange(2, 2 + CONFIG_LEN))

    /** Writes the config and returns it as the device accepted it. */
    suspend fun setConfig(config: Config): Config {
        val reply = command(byteArrayOf(CMD_CONFIG.toByte()) + config.bytes(), 2 + CONFIG_LEN)
        return Config(reply.copyOfRange(2, 2 + CONFIG_LEN))
    }

    suspend fun coefficients(): Coefficients =
        Coefficients.fromBytes(command(byteArrayOf(CMD_COEFFICIENTS.toByte()), 1 + COEFFICIENTS_LEN), 1)

    /** Writes new sensor coefficients (persisted in flash) and returns them as read back. */
    suspend fun setCoefficients(coefficients: Coefficients): Coefficients {
        val reply = command(byteArrayOf(CMD_COEFFICIENTS.toByte()) + coefficients.toBytes(), 1 + COEFFICIENTS_LEN)
        return Coefficients.fromBytes(reply, 1)
    }

    /** Restores the factory coefficients for the sensor type. */
    suspend fun resetCoefficients(): Coefficients =
        Coefficients.fromBytes(command(byteArrayOf(CMD_COEFFICIENTS_DEFAULT.toByte()), 1 + COEFFICIENTS_LEN), 1)

    suspend fun clock(): ClockReading = clockFromReply(command(byteArrayOf(CMD_CLOCK.toByte()), 5), System.currentTimeMillis())

    /**
     * Sets the device clock to this phone's time and returns a reading taken right after.
     * The time is read again for every attempt, so a resent command doesn't set an old time.
     */
    suspend fun setClockToNow(): ClockReading {
        val reply = commandWith(CMD_CLOCK, 5) { le32(System.currentTimeMillis() / 1000) }
        return clockFromReply(reply, System.currentTimeMillis())
    }

    /** Shows [message] on the LCD, alternating with the readings. */
    suspend fun show(message: ScreenMessage) {
        val bytes = message.toBytes()
        val reply = command(byteArrayOf(CMD_SCREEN.toByte()) + bytes, 1 + SCREEN_MESSAGE_LEN)
        val stored = reply.copyOfRange(1, 1 + SCREEN_MESSAGE_LEN)
        if (!stored.contentEquals(bytes)) throw InvalidValue("device stored other screen data")
    }

    /** System timer ticks the device counts as one second, nominally [NOMINAL_SECOND_TICKS]. */
    suspend fun secondTicks(): Long = command(byteArrayOf(CMD_SECOND_TICKS.toByte()), 5).u32(1)

    /** Sets the length of a clock second; the device keeps it in flash. */
    suspend fun setSecondTicks(ticks: Long): Long {
        val delta = ticksDelta(ticks) ?: throw InvalidValue("$ticks ticks per second is out of range")
        val stored = command(byteArrayOf(CMD_SECOND_TICKS.toByte()) + le16(delta), 5).u32(1)
        if (stored != ticks) throw InvalidValue("device stored $stored ticks per second instead of $ticks")
        return stored
    }

    /**
     * Downloads log records, newest first, while [wanted] accepts them; at most
     * [maxRecords]. The first record [wanted] rejects is not included and ends the download.
     */
    suspend fun readLog(maxRecords: Int, wanted: (LogRecord) -> Boolean): List<LogRecord> = mutex.withLock {
        val request = byteArrayOf(CMD_LOG.toByte()) + le16(maxRecords.coerceIn(0, 0xffff))
        val records = mutableListOf<LogRecord>()
        var attempts = 0
        discardPending()
        while (true) {
            attempts++
            write(request)
            while (true) {
                val reply = nextReply(CMD_LOG, LOG_RECORD_TIMEOUT_MS)
                if (reply == null) {
                    // The start request can get lost like any write; ask again.
                    if (records.isEmpty() && attempts < COMMAND_ATTEMPTS) break
                    throw CommandTimeout(CMD_LOG)
                }
                // `[0x35, 0, 0]` marks the end of the log.
                if (reply.size < 3 + LOG_RECORD_LEN) return records
                val record = LogRecord.fromBytes(reply, 3)
                if (!wanted(record)) {
                    write(byteArrayOf(CMD_LOG.toByte(), 0, 0))
                    // Swallow the end marker so it doesn't confuse the next command.
                    while (true) {
                        val rest = nextReply(CMD_LOG, REPLY_TIMEOUT_MS) ?: break
                        if (rest.size < 3 + LOG_RECORD_LEN) break
                    }
                    return records
                }
                records += record
            }
        }
        @Suppress("UNREACHABLE_CODE")
        records
    }

    /** Disconnects and releases the connection; safe to call more than once. */
    suspend fun disconnect() {
        runCatching {
            gatt.disconnect()
            withTimeoutOrNull(2_000) { callback.disconnected.await() }
        }
        gatt.close()
    }

    private suspend fun command(payload: ByteArray, minLen: Int): ByteArray {
        val arguments = payload.copyOfRange(1, payload.size)
        return commandWith(payload[0].toInt() and 0xff, minLen) { arguments }
    }

    /** Sends a command, its arguments built anew for every attempt, and returns its reply. */
    private suspend fun commandWith(command: Int, minLen: Int, arguments: () -> ByteArray): ByteArray =
        mutex.withLock {
            discardPending()
            for (attempt in 1..COMMAND_ATTEMPTS) {
                write(byteArrayOf(command.toByte()) + arguments())
                val reply = nextReply(command, REPLY_TIMEOUT_MS) ?: continue
                if (attempt > 1) {
                    // The earlier writes may be answered too, and such an answer would pass
                    // for the reply to the next command with this id.
                    while (nextReply(command, REPLY_TIMEOUT_MS) != null) Unit
                }
                if (reply.size < minLen) {
                    throw AtcException("reply to command 0x%02x too short (%d bytes)".format(command, reply.size))
                }
                return@withLock reply
            }
            throw CommandTimeout(command)
        }

    private suspend fun write(payload: ByteArray) {
        if (callback.disconnected.isCompleted) throw Disconnected()
        val done = CompletableDeferred<Int>()
        callback.writeDone = done
        val writeType = if (command.properties and BluetoothGattCharacteristic.PROPERTY_WRITE != 0) {
            BluetoothGattCharacteristic.WRITE_TYPE_DEFAULT
        } else {
            BluetoothGattCharacteristic.WRITE_TYPE_NO_RESPONSE
        }
        val started = if (Build.VERSION.SDK_INT >= 33) {
            gatt.writeCharacteristic(command, payload, writeType) == BluetoothGatt.GATT_SUCCESS
        } else {
            @Suppress("DEPRECATION")
            command.writeType = writeType
            @Suppress("DEPRECATION")
            command.value = payload
            @Suppress("DEPRECATION")
            gatt.writeCharacteristic(command)
        }
        if (!started) {
            // The stack is busy with an earlier operation; the retry loop sends it again.
            delay(100)
            return
        }
        withTimeoutOrNull(GATT_OP_TIMEOUT_MS) { done.await() }
    }

    /** Drops notifications that arrived after their command had given up waiting. */
    private fun discardPending() {
        while (callback.notifications.tryReceive().isSuccess) Unit
    }

    /** Waits for the next notification answering [command], skipping unrelated ones. */
    private suspend fun nextReply(command: Int, waitMs: Long): ByteArray? {
        val deadline = System.currentTimeMillis() + waitMs
        while (true) {
            val left = deadline - System.currentTimeMillis()
            if (left <= 0) return null
            val data = withTimeoutOrNull(left) { callback.notifications.receive() } ?: run {
                if (callback.disconnected.isCompleted) throw Disconnected()
                return null
            }
            if (data.isNotEmpty() && (data[0].toInt() and 0xff) == command) return data
        }
    }

    /** Turns GATT callbacks into deferreds and a channel of notifications. */
    private class Callback : BluetoothGattCallback() {
        val connected = CompletableDeferred<Unit>()
        val disconnected = CompletableDeferred<Int>()
        val servicesDiscovered = CompletableDeferred<Int>()
        val mtuChanged = CompletableDeferred<Int>()
        @Volatile var descriptorWritten = CompletableDeferred<Int>()
        @Volatile var writeDone = CompletableDeferred<Int>()
        val notifications = Channel<ByteArray>(Channel.UNLIMITED)

        override fun onConnectionStateChange(gatt: BluetoothGatt, status: Int, newState: Int) {
            when (newState) {
                BluetoothProfile.STATE_CONNECTED -> connected.complete(Unit)
                BluetoothProfile.STATE_DISCONNECTED -> {
                    disconnected.complete(status)
                    connected.completeExceptionally(AtcException("connection failed (status $status)"))
                }
            }
        }

        override fun onServicesDiscovered(gatt: BluetoothGatt, status: Int) {
            servicesDiscovered.complete(status)
        }

        override fun onMtuChanged(gatt: BluetoothGatt, mtu: Int, status: Int) {
            mtuChanged.complete(mtu)
        }

        override fun onDescriptorWrite(gatt: BluetoothGatt, descriptor: BluetoothGattDescriptor, status: Int) {
            descriptorWritten.complete(status)
        }

        override fun onCharacteristicWrite(
            gatt: BluetoothGatt,
            characteristic: BluetoothGattCharacteristic,
            status: Int,
        ) {
            writeDone.complete(status)
        }

        override fun onCharacteristicChanged(
            gatt: BluetoothGatt,
            characteristic: BluetoothGattCharacteristic,
            value: ByteArray,
        ) {
            notifications.trySend(value.copyOf())
        }

        @Deprecated("Called instead of the variant with the value before Android 13")
        override fun onCharacteristicChanged(gatt: BluetoothGatt, characteristic: BluetoothGattCharacteristic) {
            if (Build.VERSION.SDK_INT < 33) {
                @Suppress("DEPRECATION")
                notifications.trySend(characteristic.value.copyOf())
            }
        }
    }

    companion object {
        /**
         * Connects to [address], switches on notifications and checks the firmware.
         *
         * @throws AtcException if the connection cannot be made, the device is not running
         * pvvx firmware, or the firmware is older than v4.7.
         */
        suspend fun connect(context: Context, adapter: BluetoothAdapter, address: String): Thermometer {
            val device = adapter.getRemoteDevice(address)
            var attempt = 0
            while (true) {
                attempt++
                val callback = Callback()
                val gatt = device.connectGatt(context, false, callback, BluetoothDevice.TRANSPORT_LE)
                    ?: throw AtcException("Bluetooth is unavailable")
                val failure = try {
                    return setUp(gatt, callback)
                } catch (e: Exception) {
                    if (e is kotlinx.coroutines.CancellationException && e !is kotlinx.coroutines.TimeoutCancellationException) {
                        gatt.close()
                        throw e
                    }
                    e
                }
                // A failed or timed-out attempt must be cleared, or the next one fails too
                // (status 133 on many phones).
                gatt.disconnect()
                gatt.close()
                if (attempt >= CONNECT_ATTEMPTS || failure is UnsupportedFirmware || failure is NotPvvx) {
                    throw failure as? AtcException ?: AtcException(failure.message ?: failure.toString())
                }
                delay(CONNECT_RETRY_PAUSE_MS)
            }
        }

        private suspend fun setUp(gatt: BluetoothGatt, callback: Callback): Thermometer {
            withTimeout(CONNECT_TIMEOUT_MS) { callback.connected.await() }
            gatt.requestConnectionPriority(BluetoothGatt.CONNECTION_PRIORITY_HIGH)
            if (gatt.requestMtu(WANTED_MTU)) {
                withTimeoutOrNull(GATT_OP_TIMEOUT_MS) { callback.mtuChanged.await() }
            }
            if (!gatt.discoverServices()) throw AtcException("service discovery did not start")
            withTimeout(GATT_OP_TIMEOUT_MS) { callback.servicesDiscovered.await() }
            val characteristic = gatt.services.firstNotNullOfOrNull { it.getCharacteristic(COMMAND_UUID) }
                ?: throw NotPvvx()
            gatt.setCharacteristicNotification(characteristic, true)
            val cccd = characteristic.getDescriptor(CCCD_UUID)
            if (cccd != null) {
                callback.descriptorWritten = CompletableDeferred()
                val value = BluetoothGattDescriptor.ENABLE_NOTIFICATION_VALUE
                if (Build.VERSION.SDK_INT >= 33) {
                    gatt.writeDescriptor(cccd, value)
                } else {
                    @Suppress("DEPRECATION")
                    cccd.value = value
                    @Suppress("DEPRECATION")
                    gatt.writeDescriptor(cccd)
                }
                withTimeout(GATT_OP_TIMEOUT_MS) { callback.descriptorWritten.await() }
            }
            val thermometer = Thermometer(gatt, callback, characteristic)
            val reply = thermometer.command(byteArrayOf(CMD_DEV_ID.toByte()), 6)
            thermometer.info = DeviceInfo(hardware = reply.u16(2), firmware = reply.u16(4))
            if (thermometer.info.firmware < MIN_VERSION) throw UnsupportedFirmware(thermometer.info.firmware)
            return thermometer
        }
    }
}

class NotPvvx : AtcException("not a pvvx ATC firmware device")
class UnsupportedFirmware(version: Int) :
    AtcException("firmware v${version shr 4}.${version and 0xf} is too old (need v4.7+)")

internal fun decodeName(reply: ByteArray, from: Int): String {
    val end = (from until reply.size).firstOrNull { reply[it].toInt() == 0 } ?: reply.size
    return String(reply, from, end - from, Charsets.UTF_8)
}

/** Clock replies are `[0x23, now u32, (last set u32)]`. */
internal fun clockFromReply(reply: ByteArray, hostNowMs: Long) = ClockReading(
    deviceNow = reply.u32(1),
    lastSet = if (reply.size >= 9) reply.u32(5) else null,
    hostNowMs = hostNowMs,
)
