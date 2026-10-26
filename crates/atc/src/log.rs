/// One record of the on-device measurement log (`memo_blk_t`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LogRecord {
    /// Device clock (Unix seconds) at the end of the averaging period.
    pub device_time: u32,
    /// Average temperature in °C.
    pub temperature: f32,
    /// Average relative humidity in %.
    pub humidity: f32,
    /// Average battery voltage in V.
    pub voltage: f32,
}

pub(crate) const LOG_RECORD_LEN: usize = 10;

impl LogRecord {
    pub(crate) fn from_bytes(b: &[u8; LOG_RECORD_LEN]) -> Self {
        Self {
            device_time: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            temperature: f32::from(i16::from_le_bytes([b[4], b[5]])) / 100.0,
            humidity: f32::from(u16::from_le_bytes([b[6], b[7]])) / 100.0,
            voltage: f32::from(u16::from_le_bytes([b[8], b[9]])) / 1000.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_record() {
        let bytes = [0x70, 0x2e, 0xdf, 0x6a, 0xc4, 0x08, 0x3e, 0x16, 0x91, 0x0b];
        assert_eq!(
            LogRecord::from_bytes(&bytes),
            LogRecord {
                device_time: 0x6adf_2e70,
                temperature: 22.44,
                humidity: 56.94,
                voltage: 2.961,
            }
        );
    }
}
