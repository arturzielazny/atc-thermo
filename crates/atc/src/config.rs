use std::fmt;
use std::time::Duration;

use crate::Error;

/// Size of `cfg_t` for firmware v4.7+.
pub const CONFIG_LEN: usize = 11;

const ADV_INTERVAL_STEP: Duration = Duration::from_micros(62_500);
const ADV_INTERVAL_MAX: u8 = 160;
const MEASURE_INTERVAL_MIN: u8 = 2;
const MEASURE_INTERVAL_MAX: u8 = 25;

// Byte offsets in `cfg_t`.
const ADV_INTERVAL: usize = 4;
const MEASURE_INTERVAL: usize = 5;
const TX_POWER: usize = 6;
const AVERAGING: usize = 10;

/// Device configuration (`cfg_t`).
///
/// Keeps all bytes so that writing back a modified config leaves fields this
/// type does not interpret unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config([u8; CONFIG_LEN]);

impl Config {
    /// The config as the firmware stores it, e.g. for tests.
    #[must_use]
    pub fn from_bytes(bytes: [u8; CONFIG_LEN]) -> Self {
        Self(bytes)
    }

    pub(crate) fn as_bytes(&self) -> &[u8; CONFIG_LEN] {
        &self.0
    }

    /// Time between two advertisements.
    #[must_use]
    pub fn advertising_interval(&self) -> Duration {
        ADV_INTERVAL_STEP * u32::from(self.0[ADV_INTERVAL])
    }

    /// Sets the advertising interval, rounded to the firmware's 62.5 ms steps.
    ///
    /// # Errors
    ///
    /// Fails outside 62.5 ms ..= 10 s.
    pub fn set_advertising_interval(&mut self, interval: Duration) -> Result<(), Error> {
        let steps = (interval.as_secs_f64() / ADV_INTERVAL_STEP.as_secs_f64()).round();
        if !(1.0..=f64::from(ADV_INTERVAL_MAX)).contains(&steps) {
            return Err(Error::InvalidValue(format!(
                "advertising interval {interval:?} outside 62.5 ms..10 s"
            )));
        }
        self.0[ADV_INTERVAL] = steps_to_u8(steps);
        Ok(())
    }

    /// A measurement is taken every this many advertisements.
    #[must_use]
    pub fn measure_interval(&self) -> u8 {
        self.0[MEASURE_INTERVAL]
    }

    /// # Errors
    ///
    /// Fails outside 2 ..= 25.
    pub fn set_measure_interval(&mut self, adverts: u8) -> Result<(), Error> {
        if !(MEASURE_INTERVAL_MIN..=MEASURE_INTERVAL_MAX).contains(&adverts) {
            return Err(Error::InvalidValue(format!(
                "measure interval {adverts} outside {MEASURE_INTERVAL_MIN}..={MEASURE_INTERVAL_MAX}"
            )));
        }
        self.0[MEASURE_INTERVAL] = adverts;
        Ok(())
    }

    /// Time between two sensor measurements.
    #[must_use]
    pub fn measurement_period(&self) -> Duration {
        self.advertising_interval() * u32::from(self.measure_interval())
    }

    #[must_use]
    pub fn tx_power(&self) -> TxPower {
        TxPower(self.0[TX_POWER])
    }

    pub fn set_tx_power(&mut self, power: TxPower) {
        self.0[TX_POWER] = power.0;
    }

    /// Measurements averaged into one on-device log record; 0 disables the log.
    #[must_use]
    pub fn log_averaging(&self) -> u8 {
        self.0[AVERAGING]
    }

    pub fn set_log_averaging(&mut self, measurements: u8) {
        self.0[AVERAGING] = measurements;
    }

    /// Time covered by one log record, or `None` if logging is off.
    #[must_use]
    pub fn log_period(&self) -> Option<Duration> {
        match self.log_averaging() {
            0 => None,
            n => Some(self.measurement_period() * u32::from(n)),
        }
    }
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "callers clamp to 1..=160 first"
)]
fn steps_to_u8(steps: f64) -> u8 {
    steps as u8
}

/// Radio transmit power, as the firmware's register code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TxPower(pub u8);

impl TxPower {
    /// What the firmware starts with (`RF_POWER_P0p04dBm` in pvvx `def_cfg`), +0.0 dBm.
    pub const FACTORY: TxPower = TxPower(169);

    /// A sensible subset of [`TX_POWER_TABLE`] for menus, weakest first.
    pub const PRESETS: [TxPower; 8] = [
        TxPower(140),
        TxPower(150),
        TxPower(160),
        TxPower(169),
        TxPower(182),
        TxPower(191),
        TxPower(33),
        TxPower(63),
    ];

    /// Output power in dBm, if the code is one the firmware documents.
    #[must_use]
    pub fn dbm(self) -> Option<f32> {
        TX_POWER_TABLE
            .iter()
            .find(|&&(code, _)| code == self.0)
            .map(|&(_, centi_dbm)| f32::from(centi_dbm) / 100.0)
    }

    /// The documented code whose output is closest to `dbm`, clamped to the range the
    /// radio covers; the weaker one on a tie.
    #[must_use]
    pub fn nearest(dbm: f32) -> Self {
        let distance = |centi_dbm: i16| (f32::from(centi_dbm) / 100.0 - dbm).abs();
        let mut best = TX_POWER_TABLE[0];
        for entry in TX_POWER_TABLE {
            let closer = distance(entry.1)
                .total_cmp(&distance(best.1))
                .then(entry.1.cmp(&best.1));
            if closer.is_lt() {
                best = entry;
            }
        }
        Self(best.0)
    }
}

impl fmt::Display for TxPower {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.dbm() {
            Some(dbm) => write!(f, "{dbm:+.1} dBm"),
            None => write!(f, "code {}", self.0),
        }
    }
}

/// `(code, dBm × 100)` from the pvvx flasher page. Codes with bit 7 set use the
/// antenna supply, the others the battery supply (stronger, more current).
const TX_POWER_TABLE: [(u8, i16); 50] = [
    (63, 1046),
    (61, 1029),
    (58, 1001),
    (56, 981),
    (53, 948),
    (51, 924),
    (49, 897),
    (47, 873),
    (45, 844),
    (43, 813),
    (41, 779),
    (39, 741),
    (37, 702),
    (35, 660),
    (33, 614),
    (31, 565),
    (29, 513),
    (27, 457),
    (25, 394),
    (23, 323),
    (191, 301),
    (189, 281),
    (187, 261),
    (185, 239),
    (182, 199),
    (180, 173),
    (178, 145),
    (176, 117),
    (174, 90),
    (172, 58),
    (169, 4),
    (168, -14),
    (164, -97),
    (162, -142),
    (160, -189),
    (158, -248),
    (156, -303),
    (154, -361),
    (152, -426),
    (150, -503),
    (148, -581),
    (146, -667),
    (144, -765),
    (142, -865),
    (140, -989),
    (138, -1140),
    (136, -1329),
    (134, -1588),
    (132, -1927),
    (130, -2518),
];

#[cfg(test)]
mod tests {
    use super::*;

    // Factory config read from a LYWSD03MMC B1.6 on firmware v5.9.
    const FACTORY: [u8; CONFIG_LEN] = [
        0x87, 0x10, 0x0a, 0x06, 0x28, 0x04, 0xa9, 0x31, 0x31, 0x04, 0xb4,
    ];

    #[test]
    fn decodes_factory_config() {
        let config = Config::from_bytes(FACTORY);
        assert_eq!(config.advertising_interval(), Duration::from_millis(2500));
        assert_eq!(config.measure_interval(), 4);
        assert_eq!(config.measurement_period(), Duration::from_secs(10));
        assert_eq!(config.tx_power().to_string(), "+0.0 dBm");
        assert_eq!(config.tx_power(), TxPower::FACTORY);
        assert_eq!(config.log_period(), Some(Duration::from_secs(1800)));
    }

    #[test]
    fn setters_touch_only_their_byte() {
        let mut config = Config::from_bytes(FACTORY);
        config
            .set_advertising_interval(Duration::from_secs(5))
            .expect("valid");
        config.set_measure_interval(6).expect("valid");
        config.set_tx_power(TxPower(191));
        let mut expected = FACTORY;
        expected[ADV_INTERVAL] = 80;
        expected[MEASURE_INTERVAL] = 6;
        expected[TX_POWER] = 191;
        assert_eq!(config.as_bytes(), &expected);
    }

    #[test]
    fn rejects_out_of_range() {
        let mut config = Config::from_bytes(FACTORY);
        assert!(
            config
                .set_advertising_interval(Duration::from_secs(11))
                .is_err()
        );
        assert!(config.set_advertising_interval(Duration::ZERO).is_err());
        assert!(config.set_measure_interval(1).is_err());
        assert_eq!(config.as_bytes(), &FACTORY);
    }

    #[test]
    fn presets_are_documented_codes() {
        assert!(TxPower::PRESETS.iter().all(|p| p.dbm().is_some()));
        assert_eq!(TxPower(7).to_string(), "code 7");
    }

    #[test]
    fn nearest_power_is_clamped_to_the_table() {
        assert_eq!(TxPower::nearest(0.0), TxPower(169));
        assert_eq!(TxPower::nearest(-5.2), TxPower(150));
        assert_eq!(TxPower::nearest(40.0), TxPower(63));
        assert_eq!(TxPower::nearest(-40.0), TxPower(130));
        for &(code, _) in &TX_POWER_TABLE {
            let power = TxPower(code);
            assert_eq!(TxPower::nearest(power.dbm().expect("documented")), power);
        }
    }
}
