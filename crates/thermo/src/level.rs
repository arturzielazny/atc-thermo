//! Signal levelling: TX power changes that bring every device to about the same
//! signal strength at this computer, by default the weakest that is still reliable,
//! so no device spends more battery on its radio than it needs.

use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::time::Instant;

use atc::DeviceId;

use crate::app::Confirm;
use crate::devices::{RSSI_FINE, median_rssi};

/// Fewer adverts than this give too rough a median: single adverts scatter by several
/// dB from channel to channel and fade to fade.
pub const MIN_ADVERTS: usize = 20;
/// Smaller changes are skipped: medians of separate observations differ by about
/// this much, and every change is a flash write.
pub const TOLERANCE_DB: i16 = 2;
/// Median to aim for by default: a margin above where reception stops counting as
/// reliable, so the scatter between adverts rarely drops below it.
pub const DEFAULT_TARGET: i16 = RSSI_FINE + 5;
/// Targets beyond these are not useful: stronger than any device a room away
/// reaches, or weaker than BLE receivers hear reliably.
const TARGET_RANGE: RangeInclusive<i16> = -95..=-30;

/// Signal strengths heard while the levelling screen is open, and the level to aim for.
#[derive(Debug)]
pub struct LevelingScreen {
    pub started: Instant,
    /// Every advert's signal strength since `started` or the device's last TX power change.
    samples: HashMap<DeviceId, Vec<i16>>,
    /// Median signal strength to aim for, in dBm.
    pub target: i16,
    /// A TX power change waiting for `y`.
    pub confirm: Option<Confirm>,
}

/// What levelling would do to the devices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub rows: Vec<Row>,
}

impl Plan {
    /// Whether some device has been heard often enough to have a median.
    pub fn has_medians(&self) -> bool {
        self.rows.iter().any(|row| row.median.is_some())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub address: DeviceId,
    pub adverts: usize,
    pub median: Option<i16>,
    /// TX power change in dB; `None` while the device is heard too rarely or is
    /// already within [`TOLERANCE_DB`] of the target.
    pub change: Option<i16>,
}

impl LevelingScreen {
    pub fn new(now: Instant) -> Self {
        Self {
            started: now,
            samples: HashMap::new(),
            target: DEFAULT_TARGET,
            confirm: None,
        }
    }

    pub fn record(&mut self, address: DeviceId, rssi: i16) {
        self.samples.entry(address).or_default().push(rssi);
    }

    /// Drops a device's samples after its TX power changed; they no longer apply.
    pub fn forget(&mut self, address: DeviceId) {
        self.samples.remove(&address);
    }

    /// Starts observing anew, keeping the target.
    pub fn restart(&mut self, now: Instant) {
        self.started = now;
        self.samples.clear();
    }

    /// Moves the target by `step` dB.
    pub fn nudge_target(&mut self, step: i16) {
        self.target = (self.target + step).clamp(*TARGET_RANGE.start(), *TARGET_RANGE.end());
    }

    /// Proposed TX power changes for `devices`, in that order.
    pub fn plan(&self, devices: &[DeviceId]) -> Plan {
        let rows = devices
            .iter()
            .map(|&address| {
                let samples = self.samples.get(&address).map_or(&[][..], Vec::as_slice);
                let median = (samples.len() >= MIN_ADVERTS)
                    .then(|| median_rssi(&mut samples.to_vec()))
                    .flatten();
                Row {
                    address,
                    adverts: samples.len(),
                    median,
                    change: median
                        .map(|median| self.target - median)
                        .filter(|change| change.abs() >= TOLERANCE_DB),
                }
            })
            .collect();
        Plan { rows }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: DeviceId = DeviceId::Mac([0xa4, 0xc1, 0x38, 0, 0, 1]);
    const B: DeviceId = DeviceId::Mac([0xa4, 0xc1, 0x38, 0, 0, 2]);
    const C: DeviceId = DeviceId::Mac([0xa4, 0xc1, 0x38, 0, 0, 3]);

    fn screen_with(levels: &[(DeviceId, i16)]) -> LevelingScreen {
        let mut screen = LevelingScreen::new(Instant::now());
        for &(address, rssi) in levels {
            for i in 0..MIN_ADVERTS {
                // Scatter of ±3 dB around the level must not move the median.
                let scatter = [-3, 0, 3][i % 3];
                screen.record(address, rssi + scatter);
            }
        }
        screen
    }

    #[test]
    fn aims_just_above_the_marginal_signal() {
        assert_eq!(DEFAULT_TARGET, -70);
        let screen = screen_with(&[(A, -50), (B, -81), (C, -71)]);
        let changes: Vec<_> = screen
            .plan(&[A, B, C])
            .rows
            .iter()
            .map(|r| r.change)
            .collect();
        // C is within the tolerance and stays as it is.
        assert_eq!(changes, [Some(-20), Some(11), None]);
    }

    #[test]
    fn devices_heard_too_rarely_are_left_alone() {
        let mut screen = LevelingScreen::new(Instant::now());
        screen.record(C, -90);
        let plan = screen.plan(&[C]);
        assert_eq!(plan.rows[0].adverts, 1);
        assert_eq!(plan.rows[0].median, None);
        assert_eq!(plan.rows[0].change, None);
        assert!(!plan.has_medians());

        let mut screen = screen_with(&[(A, -50)]);
        assert!(screen.plan(&[A]).has_medians());
        screen.forget(A);
        assert!(!screen.plan(&[A]).has_medians());
    }

    #[test]
    fn target_moves_within_range_and_survives_a_restart() {
        let mut screen = screen_with(&[(A, -50)]);
        screen.nudge_target(-1);
        assert_eq!(screen.plan(&[A]).rows[0].change, Some(-21));
        for _ in 0..100 {
            screen.nudge_target(1);
        }
        assert_eq!(screen.target, *TARGET_RANGE.end());
        screen.restart(Instant::now());
        assert_eq!(screen.target, *TARGET_RANGE.end());
        assert!(!screen.plan(&[A]).has_medians());
    }
}
