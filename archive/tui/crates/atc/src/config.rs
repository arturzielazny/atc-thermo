use std::fmt;
use std::time::Duration;

use crate::{Error, Firmware};

/// Size of ATC `cfg_t` for firmware v4.7+.
pub const CONFIG_LEN: usize = 11;
/// Size of THB2 `cfg_t` (`config.h`).
pub const THB_CONFIG_LEN: usize = 12;
const MAX_LEN: usize = THB_CONFIG_LEN;

const ADV_INTERVAL_STEP: Duration = Duration::from_micros(62_500);
const ADV_INTERVAL_MAX: u8 = 160;
const MEASURE_INTERVAL_MIN: u8 = 2;

/// Where one family keeps the fields this type interprets.
struct Layout {
    len: usize,
    adv_interval: usize,
    measure_interval: usize,
    tx_power: usize,
    averaging: usize,
    /// ATC caps it at 25 (`cmd_parser.c`); THB only enforces the minimum.
    measure_interval_max: u8,
}

/// ATC `cfg_t`: `flg, flg2, temp_offset, humi_offset, advertising_interval,
/// measure_interval, rf_tx_power, connect_latency, min_step_time_update_lcd,
/// hw_cfg, averaging_measurements`.
const ATC_LAYOUT: Layout = Layout {
    len: CONFIG_LEN,
    adv_interval: 4,
    measure_interval: 5,
    tx_power: 6,
    averaging: 10,
    measure_interval_max: 25,
};

/// THB2 `cfg_t`: `flg u32, rf_tx_power, advertising_interval, connect_latency,
/// adv_event_cnt, measure_interval, batt_interval, averaging_measurements, reserved`.
const THB_LAYOUT: Layout = Layout {
    len: THB_CONFIG_LEN,
    adv_interval: 5,
    measure_interval: 8,
    tx_power: 4,
    averaging: 10,
    measure_interval_max: u8::MAX,
};

/// Device configuration (`cfg_t`).
///
/// Keeps all bytes so that writing back a modified config leaves fields this
/// type does not interpret unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    firmware: Firmware,
    bytes: [u8; MAX_LEN],
}

impl Config {
    /// An ATC config as the firmware stores it, e.g. for tests.
    #[must_use]
    pub fn from_bytes(bytes: [u8; CONFIG_LEN]) -> Self {
        Self::from_slice(Firmware::Atc, &bytes)
    }

    /// A THB2 config as the firmware stores it.
    #[must_use]
    pub fn from_thb_bytes(bytes: [u8; THB_CONFIG_LEN]) -> Self {
        Self::from_slice(Firmware::Thb, &bytes)
    }

    /// `bytes` must be at least [`Self::len_for`] long.
    pub(crate) fn from_slice(firmware: Firmware, bytes: &[u8]) -> Self {
        let len = Self::len_for(firmware);
        let mut all = [0; MAX_LEN];
        all[..len].copy_from_slice(&bytes[..len]);
        Self {
            firmware,
            bytes: all,
        }
    }

    pub(crate) fn len_for(firmware: Firmware) -> usize {
        layout(firmware).len
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.layout().len]
    }

    fn layout(&self) -> &'static Layout {
        layout(self.firmware)
    }

    #[must_use]
    pub fn firmware(&self) -> Firmware {
        self.firmware
    }

    /// Time between two advertisements.
    #[must_use]
    pub fn advertising_interval(&self) -> Duration {
        ADV_INTERVAL_STEP * u32::from(self.bytes[self.layout().adv_interval])
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
        self.bytes[self.layout().adv_interval] = steps_to_u8(steps);
        Ok(())
    }

    /// A measurement is taken every this many advertisements.
    #[must_use]
    pub fn measure_interval(&self) -> u8 {
        self.bytes[self.layout().measure_interval]
    }

    /// # Errors
    ///
    /// Fails outside 2 ..= 25 (ATC) or 2 ..= 255 (THB).
    pub fn set_measure_interval(&mut self, adverts: u8) -> Result<(), Error> {
        let max = self.layout().measure_interval_max;
        if !(MEASURE_INTERVAL_MIN..=max).contains(&adverts) {
            return Err(Error::InvalidValue(format!(
                "measure interval {adverts} outside {MEASURE_INTERVAL_MIN}..={max}"
            )));
        }
        self.bytes[self.layout().measure_interval] = adverts;
        Ok(())
    }

    /// Time between two sensor measurements.
    #[must_use]
    pub fn measurement_period(&self) -> Duration {
        self.advertising_interval() * u32::from(self.measure_interval())
    }

    #[must_use]
    pub fn tx_power(&self) -> TxPower {
        TxPower {
            firmware: self.firmware,
            code: self.bytes[self.layout().tx_power],
        }
    }

    /// Sets the TX power. A power meant for the other family is converted to this
    /// family's code nearest in dBm.
    pub fn set_tx_power(&mut self, power: TxPower) {
        let power = if power.firmware == self.firmware {
            power
        } else {
            power.dbm().map_or_else(
                || TxPower::factory(self.firmware),
                |dbm| TxPower::nearest(self.firmware, dbm),
            )
        };
        self.bytes[self.layout().tx_power] = power.code;
    }

    /// Measurements averaged into one on-device log record; 0 disables the log.
    #[must_use]
    pub fn log_averaging(&self) -> u8 {
        self.bytes[self.layout().averaging]
    }

    pub fn set_log_averaging(&mut self, measurements: u8) {
        self.bytes[self.layout().averaging] = measurements;
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

fn layout(firmware: Firmware) -> &'static Layout {
    match firmware {
        Firmware::Atc => &ATC_LAYOUT,
        Firmware::Thb => &THB_LAYOUT,
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

/// Menu choices in dBm, weakest first: the outputs of a sensible subset of the ATC
/// codes (140, 150, 160, 169, 182, 191, 33, 63). Each device takes its nearest code.
pub const TX_POWER_PRESETS_DBM: [f32; 8] = [-9.89, -5.03, -1.89, 0.04, 1.99, 3.01, 6.14, 10.46];

/// Radio transmit power, as one firmware family's register code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TxPower {
    firmware: Firmware,
    code: u8,
}

impl TxPower {
    #[must_use]
    pub fn new(firmware: Firmware, code: u8) -> Self {
        Self { firmware, code }
    }

    /// What the firmware starts with: ATC `RF_POWER_P0p04dBm` (169, +0.0 dBm) in pvvx
    /// `def_cfg`; THB `RF_PHY_TX_POWER_MAX` (0x1F, 0 dBm) in THB2 `def_cfg`.
    #[must_use]
    pub fn factory(firmware: Firmware) -> Self {
        let code = match firmware {
            Firmware::Atc => 169,
            Firmware::Thb => 0x1f,
        };
        Self { firmware, code }
    }

    #[must_use]
    pub fn is_factory(self) -> bool {
        self == Self::factory(self.firmware)
    }

    #[must_use]
    pub fn firmware(self) -> Firmware {
        self.firmware
    }

    #[must_use]
    pub fn code(self) -> u8 {
        self.code
    }

    /// Output power in dBm, if the code is one the firmware documents.
    #[must_use]
    pub fn dbm(self) -> Option<f32> {
        table(self.firmware)
            .iter()
            .find(|&&(code, _)| code == self.code)
            .map(|&(_, centi_dbm)| f32::from(centi_dbm) / 100.0)
    }

    /// The documented code whose output is closest to `dbm`, clamped to the range the
    /// radio covers; the weaker one on a tie.
    #[must_use]
    pub fn nearest(firmware: Firmware, dbm: f32) -> Self {
        let table = table(firmware);
        let distance = |centi_dbm: i16| (f32::from(centi_dbm) / 100.0 - dbm).abs();
        let mut best = table[0];
        for &entry in table {
            let closer = distance(entry.1)
                .total_cmp(&distance(best.1))
                .then(entry.1.cmp(&best.1));
            if closer.is_lt() {
                best = entry;
            }
        }
        Self {
            firmware,
            code: best.0,
        }
    }

    /// This family's code nearest to the output of `self` changed by `change_db`.
    #[must_use]
    pub fn changed_by(self, change_db: f32) -> Option<Self> {
        self.dbm()
            .map(|dbm| Self::nearest(self.firmware, dbm + change_db))
    }
}

impl fmt::Display for TxPower {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.dbm() {
            Some(dbm) => write!(f, "{dbm:+.1} dBm"),
            None => write!(f, "code {}", self.code),
        }
    }
}

fn table(firmware: Firmware) -> &'static [(u8, i16)] {
    match firmware {
        Firmware::Atc => &TX_POWER_TABLE,
        Firmware::Thb => &THB_TX_POWER_TABLE,
    }
}

/// `(code, dBm × 100)` for the PHY62x2 radio of THB2 devices, from
/// `RF_PHY_TX_POWER_*` in the PHY62xx SDK `rf_phy_driver.h`. The firmware clamps
/// codes to 0x3F (`RF_PHY_TX_POWER_EXTRA_MAX`).
const THB_TX_POWER_TABLE: [(u8, i16); 7] = [
    (0x3f, 500),
    (0x1f, 0),
    (0x0f, -200),
    (0x0a, -500),
    (0x04, -1000),
    (0x02, -1500),
    (0x01, -2000),
];

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

    const ADV_INTERVAL: usize = ATC_LAYOUT.adv_interval;
    const MEASURE_INTERVAL: usize = ATC_LAYOUT.measure_interval;
    const TX_POWER: usize = ATC_LAYOUT.tx_power;

    // Factory config read from a LYWSD03MMC B1.6 on firmware v5.9.
    const FACTORY: [u8; CONFIG_LEN] = [
        0x87, 0x10, 0x0a, 0x06, 0x28, 0x04, 0xa9, 0x31, 0x31, 0x04, 0xb4,
    ];

    /// THB2 `def_cfg` (`config.c`): flg `FLG_MEAS_NOTIFY | FLG_SHOW_SMILEY` (u32 LE),
    /// `rf_tx_power` 0x1F, `advertising_interval` 80 (5 s), `connect_latency` 29,
    /// `adv_event_cnt` 16, `measure_interval` 2, `batt_interval` 60, averaging 180.
    const THB_FACTORY: [u8; THB_CONFIG_LEN] = [0x05, 0, 0, 0, 0x1f, 80, 29, 16, 2, 60, 180, 0];

    fn atc(code: u8) -> TxPower {
        TxPower::new(Firmware::Atc, code)
    }

    #[test]
    fn decodes_factory_config() {
        let config = Config::from_bytes(FACTORY);
        assert_eq!(config.firmware(), Firmware::Atc);
        assert_eq!(config.advertising_interval(), Duration::from_millis(2500));
        assert_eq!(config.measure_interval(), 4);
        assert_eq!(config.measurement_period(), Duration::from_secs(10));
        assert_eq!(config.tx_power().to_string(), "+0.0 dBm");
        assert!(config.tx_power().is_factory());
        assert_eq!(config.log_period(), Some(Duration::from_secs(1800)));
        assert_eq!(config.as_bytes(), &FACTORY);
    }

    #[test]
    fn decodes_thb_factory_config() {
        let config = Config::from_thb_bytes(THB_FACTORY);
        assert_eq!(config.firmware(), Firmware::Thb);
        assert_eq!(config.advertising_interval(), Duration::from_secs(5));
        assert_eq!(config.measure_interval(), 2);
        assert_eq!(config.measurement_period(), Duration::from_secs(10));
        assert_eq!(config.tx_power().code(), 0x1f);
        assert_eq!(config.tx_power().to_string(), "+0.0 dBm");
        assert!(config.tx_power().is_factory());
        assert_eq!(config.log_period(), Some(Duration::from_secs(1800)));
        assert_eq!(config.as_bytes(), &THB_FACTORY);
    }

    #[test]
    fn thb_setters_touch_only_their_byte() {
        let mut config = Config::from_thb_bytes(THB_FACTORY);
        config
            .set_advertising_interval(Duration::from_millis(2500))
            .expect("valid");
        config
            .set_measure_interval(40)
            .expect("THB allows more than 25");
        config.set_tx_power(TxPower::new(Firmware::Thb, 0x0a));
        config.set_log_averaging(0);
        let mut expected = THB_FACTORY;
        expected[5] = 40;
        expected[8] = 40;
        expected[4] = 0x0a;
        expected[10] = 0;
        assert_eq!(config.as_bytes(), &expected);
        assert_eq!(config.log_period(), None);
        assert!(config.set_measure_interval(1).is_err());
    }

    #[test]
    fn power_from_other_family_is_converted() {
        let mut config = Config::from_thb_bytes(THB_FACTORY);
        config.set_tx_power(atc(150)); // −5.03 dBm
        assert_eq!(config.tx_power(), TxPower::new(Firmware::Thb, 0x0a));
        let mut config = Config::from_bytes(FACTORY);
        config.set_tx_power(TxPower::new(Firmware::Thb, 0x3f)); // +5 dBm
        assert_eq!(config.tx_power(), atc(29)); // +5.13 dBm
    }

    #[test]
    fn setters_touch_only_their_byte() {
        let mut config = Config::from_bytes(FACTORY);
        config
            .set_advertising_interval(Duration::from_secs(5))
            .expect("valid");
        config.set_measure_interval(6).expect("valid");
        config.set_tx_power(atc(191));
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
        assert!(config.set_measure_interval(26).is_err());
        assert_eq!(config.as_bytes(), &FACTORY);
    }

    #[test]
    fn presets_map_to_documented_atc_codes() {
        let codes: Vec<u8> = TX_POWER_PRESETS_DBM
            .iter()
            .map(|&dbm| TxPower::nearest(Firmware::Atc, dbm).code())
            .collect();
        assert_eq!(codes, [140, 150, 160, 169, 182, 191, 33, 63]);
        assert_eq!(atc(7).to_string(), "code 7");
    }

    #[test]
    fn nearest_power_is_clamped_to_the_table() {
        assert_eq!(TxPower::nearest(Firmware::Atc, 0.0), atc(169));
        assert_eq!(TxPower::nearest(Firmware::Atc, -5.2), atc(150));
        assert_eq!(TxPower::nearest(Firmware::Atc, 40.0), atc(63));
        assert_eq!(TxPower::nearest(Firmware::Atc, -40.0), atc(130));
        for &(code, _) in &TX_POWER_TABLE {
            let power = atc(code);
            assert_eq!(
                TxPower::nearest(Firmware::Atc, power.dbm().expect("documented")),
                power
            );
        }
    }

    #[test]
    fn thb_power_table() {
        let thb = |code| TxPower::new(Firmware::Thb, code);
        assert_eq!(TxPower::nearest(Firmware::Thb, 10.0), thb(0x3f));
        assert_eq!(TxPower::nearest(Firmware::Thb, -1.0), thb(0x0f));
        assert_eq!(TxPower::nearest(Firmware::Thb, -40.0), thb(0x01));
        assert_eq!(thb(0x1f).changed_by(-5.0), Some(thb(0x0a)));
        assert_eq!(thb(0x1f).changed_by(3.0), Some(thb(0x3f)));
        assert_eq!(thb(0x30).to_string(), "code 48");
        assert_eq!(thb(0x30).changed_by(1.0), None);
    }
}
