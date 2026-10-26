use crate::Error;

pub(crate) const COEFFICIENTS_LEN: usize = 12;

/// Largest gain the firmware can apply without overflowing `raw * k` in an i32.
const MAX_GAIN_CODE: f64 = 32_767.0;

/// Sensor conversion coefficients (`sensor_coef_t`).
///
/// The firmware computes, in hundredths of °C and %RH:
///
/// ```text
/// temperature = (raw_t * temperature_gain) >> 16 + temperature_offset
/// humidity    = (raw_h * humidity_gain)    >> 16 + humidity_offset
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Coefficients {
    pub temperature_gain: u32,
    pub humidity_gain: u32,
    pub temperature_offset: i16,
    pub humidity_offset: i16,
}

/// A linear correction of reported values: `corrected = gain * reported + offset`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Correction {
    pub gain: f64,
    /// In °C or %RH.
    pub offset: f64,
}

impl Correction {
    pub const IDENTITY: Self = Self {
        gain: 1.0,
        offset: 0.0,
    };
}

impl Coefficients {
    pub(crate) fn from_bytes(b: &[u8; COEFFICIENTS_LEN]) -> Self {
        Self {
            temperature_gain: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            humidity_gain: u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
            temperature_offset: i16::from_le_bytes([b[8], b[9]]),
            humidity_offset: i16::from_le_bytes([b[10], b[11]]),
        }
    }

    pub(crate) fn to_bytes(self) -> [u8; COEFFICIENTS_LEN] {
        let mut b = [0; COEFFICIENTS_LEN];
        b[0..4].copy_from_slice(&self.temperature_gain.to_le_bytes());
        b[4..8].copy_from_slice(&self.humidity_gain.to_le_bytes());
        b[8..10].copy_from_slice(&self.temperature_offset.to_le_bytes());
        b[10..12].copy_from_slice(&self.humidity_offset.to_le_bytes());
        b
    }

    /// Coefficients that make the device report `correction` applied to what it reports now.
    ///
    /// With `t = (raw·k >> 16 + z) / 100`, wanting `a·t + b` gives `k' = a·k` and
    /// `z' = a·z + 100·b`.
    ///
    /// # Errors
    ///
    /// Fails if a resulting coefficient does not fit the firmware's integer types.
    pub fn corrected(&self, temperature: Correction, humidity: Correction) -> Result<Self, Error> {
        let (temperature_gain, temperature_offset) =
            apply(self.temperature_gain, self.temperature_offset, temperature)?;
        let (humidity_gain, humidity_offset) =
            apply(self.humidity_gain, self.humidity_offset, humidity)?;
        Ok(Self {
            temperature_gain,
            humidity_gain,
            temperature_offset,
            humidity_offset,
        })
    }
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "both values are range-checked before the cast"
)]
fn apply(gain: u32, offset: i16, c: Correction) -> Result<(u32, i16), Error> {
    let new_gain = (c.gain * f64::from(gain)).round();
    let new_offset = (c.gain * f64::from(offset) + 100.0 * c.offset).round();
    if !(1.0..=MAX_GAIN_CODE).contains(&new_gain) {
        return Err(Error::InvalidValue(format!(
            "gain {:.4} makes coefficient {new_gain} (max {MAX_GAIN_CODE})",
            c.gain
        )));
    }
    if !(f64::from(i16::MIN)..=f64::from(i16::MAX)).contains(&new_offset) {
        return Err(Error::InvalidValue(format!(
            "offset {:.2} makes coefficient {new_offset} (outside i16)",
            c.offset
        )));
    }
    Ok((new_gain as u32, new_offset as i16))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHT4X: Coefficients = Coefficients {
        temperature_gain: 17_500,
        humidity_gain: 12_500,
        temperature_offset: -4_500,
        humidity_offset: -600,
    };

    /// What the firmware would report for a raw reading, in °C / %RH.
    fn firmware(gain: u32, offset: i16, raw: u16) -> f64 {
        let scaled = (i64::from(raw) * i64::from(gain)) >> 16;
        (scaled + i64::from(offset)) as f64 / 100.0
    }

    #[test]
    fn byte_round_trip() {
        let bytes = SHT4X.to_bytes();
        assert_eq!(
            bytes,
            [0x5c, 0x44, 0, 0, 0xd4, 0x30, 0, 0, 0x6c, 0xee, 0xa8, 0xfd]
        );
        assert_eq!(Coefficients::from_bytes(&bytes), SHT4X);
    }

    #[test]
    fn identity_keeps_coefficients() {
        let same = SHT4X
            .corrected(Correction::IDENTITY, Correction::IDENTITY)
            .expect("fits");
        assert_eq!(same, SHT4X);
    }

    #[test]
    fn correction_changes_firmware_output_as_requested() {
        let t = Correction {
            gain: 1.02,
            offset: -0.35,
        };
        let h = Correction {
            gain: 0.97,
            offset: 2.1,
        };
        let new = SHT4X.corrected(t, h).expect("fits");
        for raw in [20_000, 26_000, 32_000, 40_000] {
            let before = firmware(SHT4X.temperature_gain, SHT4X.temperature_offset, raw);
            let after = firmware(new.temperature_gain, new.temperature_offset, raw);
            let want = t.gain * before + t.offset;
            assert!((after - want).abs() < 0.02, "raw {raw}: {after} vs {want}");

            let before = firmware(SHT4X.humidity_gain, SHT4X.humidity_offset, raw);
            let after = firmware(new.humidity_gain, new.humidity_offset, raw);
            let want = h.gain * before + h.offset;
            assert!((after - want).abs() < 0.02, "raw {raw}: {after} vs {want}");
        }
    }

    #[test]
    fn rejects_unrepresentable() {
        let huge = Correction {
            gain: 2.0,
            offset: 0.0,
        };
        assert!(SHT4X.corrected(huge, Correction::IDENTITY).is_err());
        let far = Correction {
            gain: 1.0,
            offset: 400.0,
        };
        assert!(SHT4X.corrected(far, Correction::IDENTITY).is_err());
    }
}
