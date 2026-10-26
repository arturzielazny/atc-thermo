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
    /// timestamps from before the last set, which belong to an older, unrelated time base.
    #[must_use]
    pub fn to_host(&self, device_time: u32) -> Option<SystemTime> {
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
