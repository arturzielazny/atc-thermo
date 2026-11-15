/*
 * The two pvvx firmware families this app talks to, and where their layouts differ; a
 * port of `crates/atc/src/firmware.rs`.
 *
 * - [Firmware.Atc]: <https://github.com/pvvx/ATC_MiThermometer> v4.7+ (Telink TLSR825x,
 *   e.g. Xiaomi LYWSD03MMC). Command characteristic `0x1F1F`.
 * - [Firmware.Thb]: <https://github.com/pvvx/THB2> v1.4+ (PHY62x2, e.g. Tuya THB1, THB2,
 *   BTH01, TH05). Command characteristic `0xFFF4` in service `0xFCD2`.
 *
 * Both answer commands as notifications whose first byte echoes the command id, and most
 * commands share ids and layouts. A connection tells them apart by which command
 * characteristic the device has.
 */
package dev.artur.thermo.atc

import java.util.UUID

/** ATC command/notify characteristic, `src/ble.c` of ATC_MiThermometer. */
val ATC_COMMAND_UUID: UUID = UUID.fromString("00001f1f-0000-1000-8000-00805f9b34fb")
/** THB command/notify characteristic, `CHARACTERISTIC_CMD_UUID16` in THB2 `sbp_profile.c`. */
val THB_COMMAND_UUID: UUID = UUID.fromString("0000fff4-0000-1000-8000-00805f9b34fb")

/** `dev_id_t.services` bits, THB2 `cmd_parser.h`. ATC v4.7+ replies are shorter, so these are only known for THB. */
internal const val SERVICE_SCREEN = 0x0000_0020L
internal const val SERVICE_HISTORY = 0x0000_0010L
/** Clock rate adjustment (`SERVICE_TIME_ADJUST`), commented out in every THB2 build. */
internal const val SERVICE_TIME_ADJUST = 0x0000_1000L

/** Which firmware a connected device runs. */
enum class Firmware(
    /** Default name prefix, restored by an empty name (`ATC_xxxxxx`, `THB1-xxxxxx`, ...). */
    val label: String,
    /**
     * Oldest version (BCD) whose layouts this app knows. ATC: v4.7 introduced the
     * current `cfg_t`. THB: v1.4 (Feb 2024) is the oldest release with today's 12-byte
     * `cfg_t` and 19-byte coefficient reply.
     */
    val minVersion: Int,
) {
    Atc("ATC", 0x47),
    Thb("THB", 0x14);

    override fun toString() = label

    companion object {
        /** The family whose command characteristic has this UUID. */
        fun fromCommandUuid(uuid: UUID): Firmware? = when (uuid) {
            ATC_COMMAND_UUID -> Atc
            THB_COMMAND_UUID -> Thb
            else -> null
        }
    }
}

/** Bytes a dev-id reply needs: `[0x00, revision, hw u16, sw u16]`. */
internal const val DEV_ID_MIN_LEN = 6

/** Identity reported by the firmware (`CMD_ID_DEV_ID`). */
data class DeviceInfo(
    val family: Firmware,
    /** Hardware code (LYWSD03MMC: 4 = B1.6, ...; THB2 `DEVICE_*`: 23 = THB1, ...). */
    val hardware: Int,
    /** Firmware version, BCD: `0x59` is v5.9. */
    val firmware: Int,
    /** `dev_id_t.services`, when the reply carries it (THB). */
    val services: Long? = null,
) {
    /** Firmware version as text, e.g. `"5.9"`. */
    val firmwareVersion: String get() = "${firmware shr 4}.${firmware and 0xf}"

    /** Model name for THB2 hardware codes (`DEVICE_*` in `config.h`). */
    val model: String?
        get() = if (family != Firmware.Thb) null else when (hardware) {
            19 -> "THB2"
            20 -> "BTH01"
            21 -> "TH05"
            23 -> "THB1"
            24 -> "TH05D"
            25 -> "TH05F"
            26 -> "THB3"
            32 -> "KEY2"
            34 -> "TH04"
            42 -> "HDP16"
            43 -> "TN6ATAG3"
            else -> null
        }

    /** Whether the device has an LCD that takes [ScreenMessage]s. ATC devices all have one; THB says so in its service bits. */
    val hasScreen: Boolean get() = services?.let { it and SERVICE_SCREEN != 0L } ?: true

    /** Whether the device keeps a measurement log. */
    val hasLog: Boolean get() = services?.let { it and SERVICE_HISTORY != 0L } ?: true

    /** Whether the length of a clock second can be trimmed (`0x24`). THB2 answers `[0x24, 0xff]` (unknown command) unless built with `SERVICE_TIME_ADJUST`. */
    val hasClockRate: Boolean
        get() = when (family) {
            Firmware.Atc -> true
            Firmware.Thb -> services?.let { it and SERVICE_TIME_ADJUST != 0L } ?: false
        }

    companion object {
        /** Parses `[0x00, pid/revision, hw u16, sw u16, dev_spec u16, services u32]`; ATC replies stop after the version. */
        fun fromReply(family: Firmware, reply: ByteArray) = DeviceInfo(
            family = family,
            hardware = reply.u16(2),
            firmware = reply.u16(4),
            services = if (family == Firmware.Thb && reply.size >= 12) reply.u32(8) else null,
        )
    }
}
