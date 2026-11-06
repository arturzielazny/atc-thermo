//! The two pvvx firmware families this crate talks to, and where their layouts differ.
//!
//! - [`Firmware::Atc`]: <https://github.com/pvvx/ATC_MiThermometer> v4.7+ (Telink `TLSR825x`,
//!   e.g. Xiaomi LYWSD03MMC). Command characteristic `0x1F1F`.
//! - [`Firmware::Thb`]: <https://github.com/pvvx/THB2> v1.4+ (PHY62x2, e.g. Tuya THB1,
//!   THB2, BTH01, TH05). Command characteristic `0xFFF4` in service `0xFCD2`.
//!
//! Both answer commands as notifications whose first byte echoes the command id, and
//! most commands share ids and layouts. The differences are kept here so the rest of
//! the crate can stay I/O-free and testable. A backend tells them apart by which
//! command characteristic the device has.

use std::fmt;

use uuid::Uuid;

/// ATC command/notify characteristic, `src/ble.c` of `ATC_MiThermometer`.
pub const ATC_COMMAND_UUID: Uuid = Uuid::from_u128(0x0000_1f1f_0000_1000_8000_0080_5f9b_34fb);
/// THB command/notify characteristic, `CHARACTERISTIC_CMD_UUID16` in THB2 `sbp_profile.c`.
pub const THB_COMMAND_UUID: Uuid = Uuid::from_u128(0x0000_fff4_0000_1000_8000_0080_5f9b_34fb);

/// `dev_id_t.services` bits, THB2 `cmd_parser.h`. ATC v4.7+ replies are shorter, so
/// these are only known for THB.
pub(crate) const SERVICE_SCREEN: u32 = 0x0000_0020;
pub(crate) const SERVICE_HISTORY: u32 = 0x0000_0010;
/// Clock rate adjustment (`SERVICE_TIME_ADJUST`), commented out in every THB2 build.
pub(crate) const SERVICE_TIME_ADJUST: u32 = 0x0000_1000;

/// Which firmware a connected device runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Firmware {
    /// pvvx `ATC_MiThermometer`.
    Atc,
    /// pvvx `THB2`.
    Thb,
}

impl Firmware {
    /// The family whose command characteristic has this UUID.
    #[must_use]
    pub fn from_command_uuid(uuid: Uuid) -> Option<Self> {
        match uuid {
            ATC_COMMAND_UUID => Some(Self::Atc),
            THB_COMMAND_UUID => Some(Self::Thb),
            _ => None,
        }
    }

    /// Oldest version (BCD) whose layouts this crate knows.
    ///
    /// ATC: v4.7 introduced the current `cfg_t`. THB: v1.4 (Feb 2024) is the oldest
    /// release with today's 12-byte `cfg_t` and 19-byte coefficient reply.
    #[must_use]
    pub fn min_version(self) -> u16 {
        match self {
            Self::Atc => 0x47,
            Self::Thb => 0x14,
        }
    }

    /// Default name prefix, restored by an empty name (`ATC_xxxxxx`, `THB1-xxxxxx`, ...).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Atc => "ATC",
            Self::Thb => "THB",
        }
    }
}

impl fmt::Display for Firmware {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Identity reported by the firmware (`CMD_ID_DEV_ID`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceInfo {
    pub family: Firmware,
    /// Hardware code (LYWSD03MMC: 4 = B1.6, ...; THB2 `DEVICE_*`: 23 = THB1, ...).
    pub hardware: u16,
    /// Firmware version, BCD: `0x59` is v5.9.
    pub firmware: u16,
    /// `dev_id_t.services`, when the reply carries it (THB).
    pub services: Option<u32>,
}

/// Bytes a dev-id reply needs: `[0x00, revision, hw u16, sw u16]`.
pub(crate) const DEV_ID_MIN_LEN: usize = 6;

impl DeviceInfo {
    /// Parses `[0x00, pid/revision, hw u16, sw u16, dev_spec u16, services u32]`; ATC
    /// replies stop after the version.
    pub(crate) fn from_reply(family: Firmware, reply: &[u8]) -> Self {
        let word = |at: usize| u16::from_le_bytes([reply[at], reply[at + 1]]);
        Self {
            family,
            hardware: word(2),
            firmware: word(4),
            services: match family {
                Firmware::Atc => None,
                Firmware::Thb => reply
                    .get(8..12)
                    .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            },
        }
    }

    /// Firmware version as text, e.g. `"5.9"`.
    #[must_use]
    pub fn firmware_version(&self) -> String {
        format!("{}.{}", self.firmware >> 4, self.firmware & 0xf)
    }

    /// Model name for THB2 hardware codes (`DEVICE_*` in `config.h`).
    #[must_use]
    pub fn model(&self) -> Option<&'static str> {
        if self.family != Firmware::Thb {
            return None;
        }
        Some(match self.hardware {
            19 => "THB2",
            20 => "BTH01",
            21 => "TH05",
            23 => "THB1",
            24 => "TH05D",
            25 => "TH05F",
            26 => "THB3",
            32 => "KEY2",
            34 => "TH04",
            42 => "HDP16",
            43 => "TN6ATAG3",
            _ => return None,
        })
    }

    /// Whether the device has an LCD that takes [`ScreenMessage`](crate::ScreenMessage)s.
    /// ATC devices all have one; THB says so in its service bits (THB1 has, THB2 not).
    #[must_use]
    pub fn has_screen(&self) -> bool {
        self.services.is_none_or(|s| s & SERVICE_SCREEN != 0)
    }

    /// Whether the device keeps a measurement log.
    #[must_use]
    pub fn has_log(&self) -> bool {
        self.services.is_none_or(|s| s & SERVICE_HISTORY != 0)
    }

    /// Whether the length of a clock second can be trimmed (`0x24`). THB2 answers
    /// `[0x24, 0xff]` (unknown command) unless built with `SERVICE_TIME_ADJUST`.
    #[must_use]
    pub fn has_clock_rate(&self) -> bool {
        match self.family {
            Firmware::Atc => true,
            Firmware::Thb => self.services.is_some_and(|s| s & SERVICE_TIME_ADJUST != 0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_from_characteristic() {
        assert_eq!(
            Firmware::from_command_uuid(ATC_COMMAND_UUID),
            Some(Firmware::Atc)
        );
        assert_eq!(
            Firmware::from_command_uuid(THB_COMMAND_UUID),
            Some(Firmware::Thb)
        );
        // 0xFFF3 is the THB2 OTA characteristic, never the command one.
        let ota = Uuid::from_u128(0x0000_fff3_0000_1000_8000_0080_5f9b_34fb);
        assert_eq!(Firmware::from_command_uuid(ota), None);
    }

    #[test]
    fn parses_thb1_dev_id() {
        // THB2 `cmd_parser.c`: pid 0x00, revision 0x00, hw DEVICE_THB1 = 23, sw
        // APP_VERSION 0x21, dev_spec_data 0 (always, see the `&obuf` bug), services of
        // the THB1 APP build: OTA|SCREEN|THS|KEY|HISTORY|TH_TRG|RDS|BINDKEY.
        let services: u32 = 0x01 | 0x20 | 0x80 | 0x200 | 0x10 | 0x4000 | 0x100 | 0x08;
        let mut reply = vec![0x00, 0x00, 23, 0, 0x21, 0x00, 0, 0];
        reply.extend_from_slice(&services.to_le_bytes());
        let info = DeviceInfo::from_reply(Firmware::Thb, &reply);
        assert_eq!(info.hardware, 23);
        assert_eq!(info.model(), Some("THB1"));
        assert_eq!(info.firmware_version(), "2.1");
        assert!(info.firmware >= Firmware::Thb.min_version());
        assert!(info.firmware < Firmware::Atc.min_version());
        assert!(info.has_screen());
        assert!(info.has_log());
        assert!(!info.has_clock_rate());
    }

    #[test]
    fn thb_without_screen_bit_has_no_screen() {
        // BTH01: no LCD, so SCREEN (0x20) is clear.
        let reply = [0x00, 0x00, 20, 0, 0x21, 0x00, 0, 0, 0x99, 0, 0, 0];
        let info = DeviceInfo::from_reply(Firmware::Thb, &reply);
        assert_eq!(info.model(), Some("BTH01"));
        assert!(!info.has_screen());
    }

    #[test]
    fn atc_dev_id_keeps_old_meaning() {
        let info = DeviceInfo::from_reply(Firmware::Atc, &[0x00, 0x00, 4, 0, 0x59, 0]);
        assert_eq!(info.hardware, 4);
        assert_eq!(info.firmware_version(), "5.9");
        assert_eq!(info.services, None);
        assert_eq!(info.model(), None);
        assert!(info.has_screen() && info.has_log() && info.has_clock_rate());
    }
}
