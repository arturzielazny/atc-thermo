use crate::Error;

pub(crate) const COEFFICIENTS_LEN: usize = 12;

/// Largest gain the ATC firmware can apply without overflowing `raw * k` in an i32.
pub const ATC_MAX_GAIN: u32 = 32_767;
/// THB2 computes `raw * k` in a u32 (`sensors.c`): a 16-bit raw value fits any k up
/// to 65535 (signed CHT8215 temperatures: 32768 · 65535 < 2³¹).
pub const THB_MAX_GAIN: u32 = 65_535;
/// AHT2x raw values have 20 bits, so `raw * k` stays in a u32 only for k < 2¹².
pub const THB_AHT_MAX_GAIN: u32 = 4_095;

/// `sensor_type` codes in the THB2 coefficient reply (`sensors.h`).
const THB_SENSOR_AHT2X: u8 = 5;
/// AHT2x report this vendor id (`sensors.c`).
const THB_AHT_VID: u16 = 0xAAAA;
/// THB2 replies to `0x25`/`0x26` with `thsensor_cfg_t` up to `sensor_type`
/// (`thsensor_cfg_send_size` = 19): `[cmd, coef 12, mid u16, vid u16, i2c_addr,
/// sensor_type]`, little endian like every field.
pub(crate) const THB_COEFFICIENTS_REPLY_LEN: usize = 1 + COEFFICIENTS_LEN + 6;

/// Which sensor a THB2 device reports with its coefficients.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SensorInfo {
    pub manufacturer_id: u16,
    pub vendor_id: u16,
    pub i2c_address: u8,
    /// `TH_SENSOR_*`: 4 CHT8305, 5 AHT2x, 6 CHT8215, 7 CHT832x, ...
    pub sensor_type: u8,
}

impl SensorInfo {
    /// The tail of a THB2 coefficient reply, if present.
    pub(crate) fn from_thb_reply(reply: &[u8]) -> Option<Self> {
        let tail = reply.get(1 + COEFFICIENTS_LEN..THB_COEFFICIENTS_REPLY_LEN)?;
        Some(Self {
            manufacturer_id: u16::from_le_bytes([tail[0], tail[1]]),
            vendor_id: u16::from_le_bytes([tail[2], tail[3]]),
            i2c_address: tail[4],
            sensor_type: tail[5],
        })
    }

    /// Largest gain coefficient the firmware's integer maths takes for this sensor.
    #[must_use]
    pub fn max_gain(&self) -> u32 {
        if self.sensor_type == THB_SENSOR_AHT2X || self.vendor_id == THB_AHT_VID {
            THB_AHT_MAX_GAIN
        } else {
            THB_MAX_GAIN
        }
    }
}

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
        self.corrected_within(temperature, humidity, ATC_MAX_GAIN)
    }

    /// Like [`Self::corrected`], with gain coefficients up to `max_gain` (see
    /// [`Thermometer::max_gain`](crate::Thermometer::max_gain)).
    ///
    /// # Errors
    ///
    /// Fails if a resulting coefficient does not fit.
    pub fn corrected_within(
        &self,
        temperature: Correction,
        humidity: Correction,
        max_gain: u32,
    ) -> Result<Self, Error> {
        let (temperature_gain, temperature_offset) = apply(
            self.temperature_gain,
            self.temperature_offset,
            temperature,
            max_gain,
        )?;
        let (humidity_gain, humidity_offset) =
            apply(self.humidity_gain, self.humidity_offset, humidity, max_gain)?;
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
fn apply(gain: u32, offset: i16, c: Correction, max_gain: u32) -> Result<(u32, i16), Error> {
    let new_gain = (c.gain * f64::from(gain)).round();
    let new_offset = (c.gain * f64::from(offset) + 100.0 * c.offset).round();
    if !(1.0..=f64::from(max_gain)).contains(&new_gain) {
        return Err(Error::InvalidValue(format!(
            "gain {:.4} makes coefficient {new_gain} (max {max_gain})",
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

    // THB2 `sensors.c` defaults for the THB1's CHT832x (type 7).
    const CHT832X: Coefficients = Coefficients {
        temperature_gain: 17_500,
        humidity_gain: 10_000,
        temperature_offset: -4_500,
        humidity_offset: 0,
    };

    /// THB2 `read_sensor_cht83xx`: `(raw * k) >> 16 + z` in a u32, then i16.
    #[expect(clippy::cast_possible_wrap, reason = "mirrors the C casts")]
    fn thb_firmware(gain: u32, offset: i16, raw: u16) -> f64 {
        let scaled = (u32::from(raw).wrapping_mul(gain) >> 16) as i32;
        f64::from(scaled + i32::from(offset)) / 100.0
    }

    #[test]
    fn thb_reply_layout() {
        // `[0x25, coef, mid, vid, i2c addr, type]` as a THB1 with a CHT832x sends it:
        // mid 0x5959, vid `CHT832x_VID` 0x2083, `TH_SENSOR_CHT832x` = 7.
        let mut reply = vec![0x25];
        reply.extend_from_slice(&CHT832X.to_bytes());
        reply.extend_from_slice(&[0x59, 0x59, 0x83, 0x20, 0x44, 7]);
        assert_eq!(reply.len(), THB_COEFFICIENTS_REPLY_LEN);
        let bytes: &[u8; COEFFICIENTS_LEN] = reply[1..=COEFFICIENTS_LEN].try_into().expect("len");
        assert_eq!(Coefficients::from_bytes(bytes), CHT832X);
        let sensor = SensorInfo::from_thb_reply(&reply).expect("full reply");
        assert_eq!(sensor.sensor_type, 7);
        assert_eq!(sensor.vendor_id, 0x2083);
        assert_eq!(sensor.max_gain(), THB_MAX_GAIN);
        assert_eq!(SensorInfo::from_thb_reply(&reply[..13]), None);
    }

    #[test]
    fn thb_correction_matches_firmware_maths() {
        let t = Correction {
            gain: 1.01,
            offset: -0.4,
        };
        let h = Correction {
            gain: 1.05,
            offset: -3.0,
        };
        let new = CHT832X.corrected_within(t, h, THB_MAX_GAIN).expect("fits");
        for raw in [20_000_u16, 26_000, 32_000, 40_000] {
            let before = thb_firmware(CHT832X.temperature_gain, CHT832X.temperature_offset, raw);
            let after = thb_firmware(new.temperature_gain, new.temperature_offset, raw);
            assert!((after - (t.gain * before + t.offset)).abs() < 0.02);
            let before = thb_firmware(CHT832X.humidity_gain, CHT832X.humidity_offset, raw);
            let after = thb_firmware(new.humidity_gain, new.humidity_offset, raw);
            assert!((after - (h.gain * before + h.offset)).abs() < 0.02);
        }
    }

    #[test]
    fn gain_limits_per_sensor() {
        // CHT8215 humidity k = 20000: doubling passes on THB, fails the ATC limit.
        let cht8215 = Coefficients {
            temperature_gain: 25_606,
            humidity_gain: 20_000,
            temperature_offset: 0,
            humidity_offset: 0,
        };
        let double = Correction {
            gain: 2.0,
            offset: 0.0,
        };
        assert!(cht8215.corrected(Correction::IDENTITY, double).is_err());
        assert!(
            cht8215
                .corrected_within(Correction::IDENTITY, double, THB_MAX_GAIN)
                .is_ok()
        );
        // AHT2x (k 1250 / 625, 20-bit raw): capped at 4095.
        let aht = Coefficients {
            temperature_gain: 1_250,
            humidity_gain: 625,
            temperature_offset: -5_000,
            humidity_offset: 0,
        };
        let sensor = SensorInfo {
            manufacturer_id: 0,
            vendor_id: THB_AHT_VID,
            i2c_address: 0x70,
            sensor_type: THB_SENSOR_AHT2X,
        };
        let triple = Correction {
            gain: 3.5,
            offset: 0.0,
        };
        assert!(
            aht.corrected_within(triple, Correction::IDENTITY, sensor.max_gain())
                .is_err()
        );
        let small = Correction {
            gain: 1.03,
            offset: 0.2,
        };
        assert!(
            aht.corrected_within(small, small, sensor.max_gain())
                .is_ok()
        );
    }
}
