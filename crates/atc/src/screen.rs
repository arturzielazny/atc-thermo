//! Numbers shown on the LCD in place of the readings (`0x22`).

use std::time::Duration;

use crate::Error;

/// Length of the firmware's `external_data_t` for LCD models like LYWSD03MMC.
pub(crate) const SCREEN_MESSAGE_LEN: usize = 7;
/// Validity time that the firmware takes as "until changed".
const FOREVER_SECS: u16 = u16::MAX;

/// Face drawn with the smiley segments.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Smiley {
    #[default]
    Off,
    Happy,
    Sad,
}

/// What to show instead of the readings. The device alternates it with its readings
/// every few seconds for [`ScreenMessage::duration`], then shows only readings again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenMessage {
    /// The big digits in tenths, −995..=19995: 225 shows `22.5`. From 1996 on the
    /// point is dropped and the value rounded to whole units, so 3330 shows `333`.
    pub big_tenths: i16,
    /// The small digits, −9..=99.
    pub small: i8,
    pub smiley: Smiley,
    /// At most 65534 s; zero ends an earlier message.
    pub duration: Duration,
}

impl ScreenMessage {
    pub(crate) fn to_bytes(self) -> Result<[u8; SCREEN_MESSAGE_LEN], Error> {
        if !(-995..=19_995).contains(&self.big_tenths) {
            return Err(Error::InvalidValue(format!(
                "big number {} is outside -995..=19995 tenths",
                self.big_tenths
            )));
        }
        if !(-9..=99).contains(&self.small) {
            return Err(Error::InvalidValue(format!(
                "small number {} is outside -9..=99",
                self.small
            )));
        }
        let secs = u16::try_from(self.duration.as_secs())
            .ok()
            .filter(|&s| s != FOREVER_SECS)
            .ok_or_else(|| {
                Error::InvalidValue(format!("{:?} is too long to show", self.duration))
            })?;
        // Flag bits: smiley 0-2, percent 3, battery 4, temperature symbol 5-7 (blank).
        let smiley: u8 = match self.smiley {
            Smiley::Off => 0,
            Smiley::Happy => 5,
            Smiley::Sad => 6,
        };
        let [b0, b1] = self.big_tenths.to_le_bytes();
        let [s0, s1] = i16::from(self.small).to_le_bytes();
        let [t0, t1] = secs.to_le_bytes();
        Ok([b0, b1, s0, s1, t0, t1, smiley])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_firmware_layout() {
        let message = ScreenMessage {
            big_tenths: 3330,
            small: 3,
            smiley: Smiley::Happy,
            duration: Duration::from_secs(600),
        };
        assert_eq!(
            message.to_bytes().expect("valid"),
            [0x02, 0x0d, 0x03, 0x00, 0x58, 0x02, 0x05]
        );
        let negative = ScreenMessage {
            big_tenths: -5,
            small: -1,
            smiley: Smiley::Off,
            duration: Duration::ZERO,
        };
        assert_eq!(
            negative.to_bytes().expect("valid"),
            [0xfb, 0xff, 0xff, 0xff, 0, 0, 0]
        );
    }

    #[test]
    fn rejects_what_the_display_cannot_show() {
        let ok = ScreenMessage {
            big_tenths: 0,
            small: 0,
            smiley: Smiley::Off,
            duration: Duration::from_secs(1),
        };
        for bad in [
            ScreenMessage {
                big_tenths: 20_000,
                ..ok
            },
            ScreenMessage { small: 100, ..ok },
            ScreenMessage {
                duration: Duration::from_secs(65_535),
                ..ok
            },
        ] {
            assert!(bad.to_bytes().is_err(), "{bad:?}");
        }
    }
}
