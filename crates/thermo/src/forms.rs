//! The device settings form: each field is "keep" or one of a few presets.

use std::time::Duration;

use atc::DeviceId;
use atc::TX_POWER_PRESETS_DBM;

use crate::jobs::Settings;

const ADVERTISING: [Duration; 4] = [
    Duration::from_secs(1),
    Duration::from_millis(2500),
    Duration::from_secs(5),
    Duration::from_secs(10),
];
const MEASURE_EVERY: [u8; 6] = [2, 3, 4, 6, 8, 12];
const LOG_EVERY: [Duration; 5] = [
    Duration::from_secs(60),
    Duration::from_secs(5 * 60),
    Duration::from_secs(10 * 60),
    Duration::from_secs(30 * 60),
    Duration::from_secs(60 * 60),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Advertising,
    MeasureEvery,
    TxPower,
    LogEvery,
}

impl Field {
    pub const ALL: [Field; 4] = [
        Field::Advertising,
        Field::MeasureEvery,
        Field::TxPower,
        Field::LogEvery,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Advertising => "Advertising interval",
            Self::MeasureEvery => "Measure every",
            Self::TxPower => "TX power",
            Self::LogEvery => "Log record every",
        }
    }

    fn presets(self) -> usize {
        match self {
            Self::Advertising => ADVERTISING.len(),
            Self::MeasureEvery => MEASURE_EVERY.len(),
            Self::TxPower => TX_POWER_PRESETS_DBM.len(),
            Self::LogEvery => LOG_EVERY.len(),
        }
    }
}

/// Settings to apply to `targets`; choice 0 of every field means "keep".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsForm {
    pub targets: Vec<DeviceId>,
    pub field: usize,
    choices: [usize; Field::ALL.len()],
}

impl SettingsForm {
    pub fn new(targets: Vec<DeviceId>) -> Self {
        Self {
            targets,
            field: 0,
            choices: [0; Field::ALL.len()],
        }
    }

    pub fn move_field(&mut self, step: isize) {
        let last = Field::ALL.len() - 1;
        self.field = self.field.saturating_add_signed(step).min(last);
    }

    /// Cycles the current field through "keep" and its presets.
    pub fn change(&mut self, step: isize) {
        let options = Field::ALL[self.field].presets() + 1;
        let current = self.choices[self.field];
        self.choices[self.field] = (current + options).saturating_add_signed(step) % options;
    }

    pub fn value_label(&self, field: Field) -> String {
        let index = self.choices[Self::index(field)];
        if index == 0 {
            return "keep".to_owned();
        }
        let i = index - 1;
        match field {
            Field::Advertising => format!("{} s", ADVERTISING[i].as_secs_f64()),
            Field::MeasureEvery => format!("{} adverts", MEASURE_EVERY[i]),
            Field::TxPower => format!("{:+.1} dBm", TX_POWER_PRESETS_DBM[i]),
            Field::LogEvery => format!("{} min", LOG_EVERY[i].as_secs() / 60),
        }
    }

    pub fn settings(&self) -> Settings {
        let pick = |field: Field| self.choices[Self::index(field)].checked_sub(1);
        Settings {
            advertising_interval: pick(Field::Advertising).map(|i| ADVERTISING[i]),
            measure_interval: pick(Field::MeasureEvery).map(|i| MEASURE_EVERY[i]),
            tx_power: pick(Field::TxPower).map(|i| TX_POWER_PRESETS_DBM[i]),
            log_period: pick(Field::LogEvery).map(|i| LOG_EVERY[i]),
        }
    }

    /// Resulting measurement period, when both of its inputs are being set.
    pub fn measurement_period(&self) -> Option<Duration> {
        let s = self.settings();
        Some(s.advertising_interval? * u32::from(s.measure_interval?))
    }

    fn index(field: Field) -> usize {
        Field::ALL
            .iter()
            .position(|&f| f == field)
            .expect("every field is in ALL")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_everything_by_default() {
        let form = SettingsForm::new(vec![]);
        assert!(form.settings().is_empty());
        assert_eq!(form.value_label(Field::TxPower), "keep");
    }

    #[test]
    fn cycles_through_presets_and_back_to_keep() {
        let mut form = SettingsForm::new(vec![]);
        form.change(1);
        form.change(1);
        assert_eq!(
            form.settings().advertising_interval,
            Some(Duration::from_millis(2500))
        );
        form.change(-1);
        form.change(-1);
        assert!(form.settings().is_empty());
        form.change(-1);
        assert_eq!(
            form.settings().advertising_interval,
            Some(Duration::from_secs(10))
        );

        form.move_field(1);
        form.change(1);
        assert_eq!(form.measurement_period(), Some(Duration::from_secs(20)));
    }
}
