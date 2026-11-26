//! Wall-clock times in the system time zone, for labels.

use std::time::SystemTime;

use jiff::Timestamp;
use jiff::tz::TimeZone;

const SECS_PER_DAY: i64 = 86_400;
const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];

/// Seconds since the epoch on the local clock: Unix time plus the UTC offset then.
pub fn local_seconds(time: SystemTime) -> i64 {
    let Ok(timestamp) = Timestamp::try_from(time) else {
        return 0;
    };
    let offset = TimeZone::system().to_offset(timestamp).seconds();
    timestamp.as_second() + i64::from(offset)
}

/// `"14:05"`, or the weekday (`"Wed"`) at midnight; `local` as from [`local_seconds`].
pub fn label(local: i64) -> String {
    let secs = local.rem_euclid(SECS_PER_DAY);
    if secs == 0 {
        let day = usize::try_from(local.div_euclid(SECS_PER_DAY).rem_euclid(7))
            .expect("rem_euclid(7) is below 7");
        WEEKDAYS[day].to_owned()
    } else {
        format!("{:02}:{:02}", secs / 3600, secs / 60 % 60)
    }
}

/// `"14:05"` on the day of `now`, otherwise `"Tue 14:05"`.
pub fn format_moment(time: SystemTime, now: SystemTime) -> String {
    let (local, today) = (local_seconds(time), local_seconds(now));
    let secs = local.rem_euclid(SECS_PER_DAY);
    let hm = format!("{:02}:{:02}", secs / 3600, secs / 60 % 60);
    if local.div_euclid(SECS_PER_DAY) == today.div_euclid(SECS_PER_DAY) {
        hm
    } else {
        let midnight = local - secs;
        format!("{} {hm}", label(midnight))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        // 2026-10-28 is a Wednesday.
        let midnight = 20_754 * SECS_PER_DAY;
        assert_eq!(label(midnight), "Wed");
        assert_eq!(label(midnight + 14 * 3600 + 5 * 60), "14:05");
        assert_eq!(label(midnight - 30 * 60), "23:30");
    }
}
