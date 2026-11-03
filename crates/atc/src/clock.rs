use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Device clock compared with the host clock at one moment.
///
/// Devices count Unix seconds from whenever they were last set (or from boot if
/// never), and drift by up to minutes per day. Their log records carry device
/// time; [`ClockReading::to_host`] maps it back to host time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockReading {
    /// Device time when it was read.
    pub device_now: u32,
    /// Device time at which the clock was last set, if the firmware tracks it.
    pub last_set: Option<u32>,
    /// Host time when it was read.
    pub host_now: SystemTime,
}

/// Below this the device clock was never set to real time (it counts from boot).
const PLAUSIBLE_SET_TIME: u32 = 1_600_000_000; // 2020-09-13
/// Shorter spans since the last set are too short to estimate drift from.
const MIN_DRIFT_SPAN: u32 = 3600;
/// A log record can be newer than the clock reading only by the time a download takes
/// after it; one further ahead was stamped before the clock was set back.
const MAX_RECORD_AHEAD: u32 = 300;
/// Shorter spans are too short to measure the clock *rate* from: offsets have a
/// resolution of about a second, which over 12 h is 3 s/day.
const MIN_RATE_SPAN: f64 = 12.0 * 3600.0;

/// System timer ticks (16 MHz) the firmware counts as one second by default.
pub const NOMINAL_SECOND_TICKS: u32 = 16_000_000;
/// The firmware takes the second length as a signed 16-bit difference from nominal,
/// about ±2000 ppm.
const MAX_TICKS_DELTA: i64 = i16::MAX as i64;

impl ClockReading {
    /// Device minus host time in seconds; positive means the device is ahead.
    #[must_use]
    pub fn offset_secs(&self) -> f64 {
        f64::from(self.device_now) - unix_secs(self.host_now)
    }

    /// Converts a device timestamp to host time.
    ///
    /// If the clock was set to real time at `last_set`, the offset is assumed to have
    /// grown linearly from zero then to [`offset_secs`](Self::offset_secs) now (constant
    /// drift). Otherwise the current offset is applied unchanged. Returns `None` for
    /// timestamps from before the last set, which belong to an older, unrelated time base,
    /// and for those well ahead of the reading: stamped by a clock that was later set
    /// back, which THB2 (no `last_set`) leaves no other trace of.
    #[must_use]
    pub fn to_host(&self, device_time: u32) -> Option<SystemTime> {
        if device_time > self.device_now.saturating_add(MAX_RECORD_AHEAD) {
            return None;
        }
        let offset = match self.last_set {
            Some(set) if set >= PLAUSIBLE_SET_TIME && set <= self.device_now => {
                if device_time < set {
                    return None;
                }
                let span = self.device_now - set;
                if span < MIN_DRIFT_SPAN {
                    self.offset_secs()
                } else {
                    self.offset_secs() * f64::from(device_time - set) / f64::from(span)
                }
            }
            _ => self.offset_secs(),
        };
        let host = f64::from(device_time) - offset;
        (host >= 0.0).then(|| UNIX_EPOCH + Duration::from_secs_f64(host))
    }

    /// Fraction by which the device clock has run fast (positive) or slow since it was
    /// last set, assuming it was set to host time; `None` if it was never set to real
    /// time or too recently to tell.
    #[must_use]
    pub fn drift(&self) -> Option<f64> {
        let set = self
            .last_set
            .filter(|&set| set >= PLAUSIBLE_SET_TIME && set <= self.device_now)?;
        let host_span = f64::from(self.device_now - set) - self.offset_secs();
        (host_span >= MIN_RATE_SPAN).then(|| self.offset_secs() / host_span)
    }
}

/// The second length (see [`NOMINAL_SECOND_TICKS`]) that cancels `drift` measured while
/// the device counted `current` ticks per second; `None` beyond what the firmware can
/// set.
#[must_use]
pub fn trimmed_second_ticks(current: u32, drift: f64) -> Option<u32> {
    // A clock that gains `drift` per second needs each second to last that much longer.
    let ticks = (f64::from(current) * (1.0 + drift)).round();
    let nominal = f64::from(NOMINAL_SECOND_TICKS);
    #[expect(clippy::cast_precision_loss, reason = "i16::MAX is exact in f64")]
    let max_delta = MAX_TICKS_DELTA as f64;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "checked to lie within ±i16::MAX of 16 million"
    )]
    ((ticks - nominal).abs() <= max_delta).then_some(ticks as u32)
}

/// The difference from nominal that the firmware's `0x24` command takes.
pub(crate) fn ticks_delta(ticks: u32) -> Option<i16> {
    i16::try_from(i64::from(ticks) - i64::from(NOMINAL_SECOND_TICKS)).ok()
}

fn unix_secs(time: SystemTime) -> f64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST_NOW: u32 = 1_793_046_000;

    fn at(secs: u32) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs.into())
    }

    fn secs(time: SystemTime) -> f64 {
        unix_secs(time)
    }

    #[test]
    fn constant_offset_when_never_set() {
        // Device booted 1000 s ago and counts from zero.
        let clock = ClockReading {
            device_now: 1000,
            last_set: Some(0),
            host_now: at(HOST_NOW),
        };
        let host = clock.to_host(400).expect("valid");
        assert!((secs(host) - f64::from(HOST_NOW - 600)).abs() < 1e-6);
    }

    #[test]
    fn drift_interpolates_from_last_set() {
        // Set correctly 24 h ago, now 120 s ahead.
        let set = HOST_NOW - 86_400;
        let clock = ClockReading {
            device_now: HOST_NOW + 120,
            last_set: Some(set),
            host_now: at(HOST_NOW),
        };
        assert!((clock.offset_secs() - 120.0).abs() < 1e-6);
        // Half way: device ahead by 60 s.
        let halfway_device = set + 43_200 + 60;
        let host = clock.to_host(halfway_device).expect("valid");
        assert!((secs(host) - f64::from(set + 43_200)).abs() < 0.01);
        // Records from before the clock was set are unusable.
        assert_eq!(clock.to_host(set - 10), None);
    }

    #[test]
    fn drift_needs_a_real_set_long_enough_ago() {
        // Set 2 days ago, now 40 s ahead: gains 20 s/day.
        let clock = ClockReading {
            device_now: HOST_NOW + 40,
            last_set: Some(HOST_NOW - 2 * 86_400),
            host_now: at(HOST_NOW),
        };
        let drift = clock.drift().expect("long enough");
        assert!((drift * 86_400.0 - 20.0).abs() < 1e-9, "{drift}");

        let recent = ClockReading {
            last_set: Some(HOST_NOW - 3600),
            ..clock
        };
        assert_eq!(recent.drift(), None);
        let never_set = ClockReading {
            device_now: 100_000,
            last_set: Some(0),
            ..clock
        };
        assert_eq!(never_set.drift(), None);
    }

    #[test]
    fn trimming_lengthens_seconds_of_fast_clocks() {
        // 20 s/day fast is 231.5 ppm, 3704 ticks of 16 MHz.
        let drift = 20.0 / 86_400.0;
        let ticks = trimmed_second_ticks(NOMINAL_SECOND_TICKS, drift).expect("in range");
        assert_eq!(ticks, NOMINAL_SECOND_TICKS + 3704);
        assert_eq!(ticks_delta(ticks), Some(3704));
        // A second trim builds on the first.
        let again = trimmed_second_ticks(ticks, -1.0 / 86_400.0).expect("in range");
        assert_eq!(again, ticks - 185);
        // Slow clocks get shorter seconds.
        let slow = trimmed_second_ticks(NOMINAL_SECOND_TICKS, -drift).expect("in range");
        assert_eq!(ticks_delta(slow), Some(-3704));
        // 0.3 % is more than the firmware can correct.
        assert_eq!(trimmed_second_ticks(NOMINAL_SECOND_TICKS, 0.003), None);
    }

    #[test]
    fn records_newer_than_the_reading_are_from_an_older_time_base() {
        // Set back by 3 h without saying so (THB2 has no `last_set`), the device still
        // holds records stamped with the old, ahead clock: they map into the future.
        let clock = ClockReading {
            device_now: HOST_NOW + 2,
            last_set: None,
            host_now: at(HOST_NOW),
        };
        assert_eq!(clock.to_host(HOST_NOW + 10_757), None);
        // A record logged while the download runs is a little newer than the reading.
        let host = clock.to_host(HOST_NOW + 62).expect("valid");
        assert!((secs(host) - f64::from(HOST_NOW + 60)).abs() < 1e-6);
    }

    #[test]
    fn recent_set_uses_constant_offset() {
        let clock = ClockReading {
            device_now: HOST_NOW + 5,
            last_set: Some(HOST_NOW - 100),
            host_now: at(HOST_NOW),
        };
        let host = clock.to_host(HOST_NOW - 50).expect("valid");
        assert!((secs(host) - f64::from(HOST_NOW - 55)).abs() < 1e-6);
    }
}
