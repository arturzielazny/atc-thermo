//! [`DeviceId`]: how a thermometer is named across BLE stacks.
//!
//! BlueZ (Linux) and WinRT expose the device's MAC address. CoreBluetooth (macOS)
//! hides it and hands out a UUID that is stable per host but differs between Macs,
//! so ids from one Mac never match a MAC or another Mac's id.

use std::fmt;
use std::str::FromStr;

use uuid::Uuid;

use crate::Error;

/// A thermometer's identity as the local BLE stack reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DeviceId {
    /// A MAC address, most significant byte first (`A4:C1:38:...`).
    Mac([u8; 6]),
    /// A CoreBluetooth peripheral identifier.
    Uuid(Uuid),
}

impl DeviceId {
    /// Text that fits a 17-column table cell: the MAC, or a UUID's first 16 characters
    /// followed by `…`.
    #[must_use]
    pub fn short(&self) -> String {
        match self {
            Self::Mac(_) => self.to_string(),
            Self::Uuid(_) => {
                let mut s: String = self.to_string().chars().take(16).collect();
                s.push('…');
                s
            }
        }
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mac(b) => write!(
                f,
                "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
                b[0], b[1], b[2], b[3], b[4], b[5]
            ),
            Self::Uuid(u) => write!(
                f,
                "{}",
                u.hyphenated().encode_upper(&mut Uuid::encode_buffer())
            ),
        }
    }
}

impl FromStr for DeviceId {
    type Err = Error;

    /// Accepts `AA:BB:CC:DD:EE:FF` (any case, `:` or `-`) or a UUID.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let parts: Vec<&str> = s.split([':', '-']).collect();
        if parts.len() == 6 && parts.iter().all(|p| p.len() == 2) {
            let mut bytes = [0; 6];
            for (byte, part) in bytes.iter_mut().zip(parts) {
                *byte = u8::from_str_radix(part, 16)
                    .map_err(|_| Error::InvalidValue(format!("bad device id {s:?}")))?;
            }
            return Ok(Self::Mac(bytes));
        }
        Uuid::parse_str(s)
            .map(Self::Uuid)
            .map_err(|_| Error::InvalidValue(format!("bad device id {s:?}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_round_trips() {
        let id: DeviceId = "a4:c1:38:27:c6:22".parse().expect("mac");
        assert_eq!(id, DeviceId::Mac([0xa4, 0xc1, 0x38, 0x27, 0xc6, 0x22]));
        assert_eq!(id.to_string(), "A4:C1:38:27:C6:22");
        assert_eq!(id.short(), "A4:C1:38:27:C6:22");
    }

    #[test]
    fn uuid_round_trips() {
        let text = "5E2A3C1B-9F4D-4A6E-8B7C-0D1E2F3A4B5C";
        let id: DeviceId = text.to_lowercase().parse().expect("uuid");
        assert!(matches!(id, DeviceId::Uuid(_)));
        assert_eq!(id.to_string(), text);
        assert_eq!(id.short(), "5E2A3C1B-9F4D-4A…");
        assert_eq!(id.short().chars().count(), 17);
    }

    #[test]
    fn rejects_garbage() {
        assert!("".parse::<DeviceId>().is_err());
        assert!("zz:c1:38:27:c6:22".parse::<DeviceId>().is_err());
        assert!("A4:C1:38:27:C6".parse::<DeviceId>().is_err());
    }
}
