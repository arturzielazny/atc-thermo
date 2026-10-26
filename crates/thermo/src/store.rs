//! Measurement history on disk: append-only CSV, one file per UTC day.
//!
//! Files live in `$XDG_DATA_HOME/thermo` (usually `~/.local/share/thermo`) and are
//! named `YYYY-MM-DD.csv`. Old files are never deleted, so they double as a long-term log.

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use bluer::Address;

use crate::devices::{Measurement, Sample};

const HEADER: &str = "time_ms,address,name,temperature_c,humidity_pct,battery_pct,voltage_v";
const SECS_PER_DAY: u64 = 24 * 60 * 60;

/// Measurements read back from disk.
#[derive(Debug, Default)]
pub struct Loaded {
    /// Oldest first.
    pub measurements: Vec<Measurement>,
    /// Lines that could not be parsed, e.g. cut off by a crash mid-write.
    pub skipped_lines: usize,
}

#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
    /// The day file currently open for appending.
    current: Option<(u64, File)>,
}

impl Store {
    /// `$XDG_DATA_HOME/thermo`, falling back to `~/.local/share/thermo`.
    pub fn default_dir() -> Option<PathBuf> {
        let data_home = env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))?;
        Some(data_home.join("thermo"))
    }

    /// Uses `dir` for history files, creating it if needed.
    ///
    /// # Errors
    ///
    /// Fails if the directory cannot be created.
    pub fn open(dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(Self { dir, current: None })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Reads every stored measurement taken at or after `since`.
    ///
    /// # Errors
    ///
    /// Fails if a day file exists but cannot be read.
    pub fn load(&self, since: SystemTime) -> Result<Loaded> {
        let mut loaded = Loaded::default();
        for day in day_of(since)..=day_of(SystemTime::now()) {
            let path = self.dir.join(file_name(day));
            let file = match File::open(&path) {
                Ok(file) => file,
                Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
                Err(err) => return Err(err).with_context(|| format!("opening {}", path.display())),
            };
            for line in BufReader::new(file).lines() {
                let line = line.with_context(|| format!("reading {}", path.display()))?;
                if line.is_empty() || line == HEADER {
                    continue;
                }
                match parse_line(&line) {
                    Some(m) if m.sample.at >= since => loaded.measurements.push(m),
                    Some(_) => {}
                    None => loaded.skipped_lines += 1,
                }
            }
        }
        loaded.measurements.sort_by_key(|m| m.sample.at);
        Ok(loaded)
    }

    /// Appends one measurement to the file of the day it was taken.
    ///
    /// # Errors
    ///
    /// Fails if the day file cannot be opened or written.
    pub fn append(&mut self, measurement: &Measurement) -> Result<()> {
        let day = day_of(measurement.sample.at);
        if self
            .current
            .as_ref()
            .is_none_or(|(open_day, _)| *open_day != day)
        {
            let path = self.dir.join(file_name(day));
            let mut file = OpenOptions::new()
                .create(true)
                .read(true)
                .append(true)
                .open(&path)
                .with_context(|| format!("opening {}", path.display()))?;
            match file.metadata()?.len() {
                0 => writeln!(file, "{HEADER}")?,
                // Terminate a line torn by a crash so the next record starts cleanly.
                len if last_byte(&mut file, len)? != b'\n' => writeln!(file)?,
                _ => {}
            }
            self.current = Some((day, file));
        }
        let (_, file) = self.current.as_mut().expect("day file was opened above");
        // One write per line, so a crash can at worst leave one partial line.
        file.write_all(format!("{}\n", format_line(measurement)).as_bytes())
            .context("writing measurement")
    }
}

fn last_byte(file: &mut File, len: u64) -> io::Result<u8> {
    let mut byte = [0];
    file.seek(SeekFrom::Start(len - 1))?;
    file.read_exact(&mut byte)?;
    Ok(byte[0])
}

fn format_line(m: &Measurement) -> String {
    let ms = m
        .sample
        .at
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let name: String = m
        .name
        .chars()
        .map(|c| if c == ',' || c.is_control() { ' ' } else { c })
        .collect();
    let battery = m.battery.map(|b| b.to_string()).unwrap_or_default();
    let voltage = m.voltage.map(|v| format!("{v:.3}")).unwrap_or_default();
    format!(
        "{ms},{},{name},{:.2},{:.2},{battery},{voltage}",
        m.address, m.sample.temperature, m.sample.humidity
    )
}

fn parse_line(line: &str) -> Option<Measurement> {
    let mut fields = line.split(',');
    let mut next = || fields.next();
    let ms: u64 = next()?.parse().ok()?;
    let address: Address = next()?.parse().ok()?;
    let name = next()?.to_owned();
    let temperature: f32 = next()?.parse().ok()?;
    let humidity: f32 = next()?.parse().ok()?;
    let battery = optional(next()?).ok()?;
    let voltage = optional(next()?).ok()?;
    Some(Measurement {
        address,
        name,
        sample: Sample {
            at: UNIX_EPOCH + Duration::from_millis(ms),
            temperature,
            humidity,
        },
        battery,
        voltage,
    })
}

/// Parses a field that may be left empty for "unknown".
fn optional<T: std::str::FromStr>(field: &str) -> Result<Option<T>, T::Err> {
    if field.is_empty() {
        Ok(None)
    } else {
        field.parse().map(Some)
    }
}

fn day_of(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / SECS_PER_DAY
}

fn file_name(day: u64) -> String {
    let (year, month, day) = civil_from_days(day);
    format!("{year:04}-{month:02}-{day:02}.csv")
}

/// Converts days since 1970-01-01 to a proleptic Gregorian `(year, month, day)`.
///
/// Howard Hinnant's algorithm: <https://howardhinnant.github.io/date_algorithms.html#civil_from_days>.
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468; // shift the epoch to 0000-03-01
    let era = z / 146_097;
    let day_of_era = z % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153; // March = 0
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measurement(at: SystemTime, temperature: f32) -> Measurement {
        Measurement {
            address: Address([0xa4, 0xc1, 0x38, 0x84, 0x7e, 0xfc]),
            name: "Living, room".to_owned(),
            sample: Sample {
                at,
                temperature,
                humidity: 56.94,
            },
            battery: Some(87),
            voltage: None,
        }
    }

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(11_017), (2000, 3, 1));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(file_name(20_752), "2026-10-26.csv");
    }

    #[test]
    fn line_round_trip() {
        let at = UNIX_EPOCH + Duration::from_millis(1_793_046_191_123);
        let line = format_line(&measurement(at, -3.5));
        assert_eq!(
            line,
            "1793046191123,A4:C1:38:84:7E:FC,Living  room,-3.50,56.94,87,"
        );
        let parsed = parse_line(&line).expect("round trip");
        assert_eq!(
            parsed.sample,
            Sample {
                at,
                temperature: -3.5,
                humidity: 56.94
            }
        );
        assert_eq!(parsed.battery, Some(87));
        assert_eq!(parsed.voltage, None);
        assert!(parse_line("1793046191123,A4:C1:38:84:7E:FC,x,21.").is_none());
    }

    #[test]
    fn append_then_load() {
        let dir = env::temp_dir().join(format!("thermo-store-test-{}", std::process::id()));
        let mut store = Store::open(dir.clone()).expect("temp dir");
        let now = SystemTime::now();
        let old = now - Duration::from_secs(2 * SECS_PER_DAY);
        store.append(&measurement(old, 20.0)).expect("append");
        store.append(&measurement(now, 21.0)).expect("append");
        fs::OpenOptions::new()
            .append(true)
            .open(dir.join(file_name(day_of(now))))
            .and_then(|mut f| f.write_all(b"17930461911"))
            .expect("simulate a torn line");

        // A fresh store (as after a restart) must not glue its first line onto the torn one.
        let mut store = Store::open(dir.clone()).expect("temp dir");
        store
            .append(&measurement(now, 22.0))
            .expect("append after torn line");

        let loaded = store.load(now - Duration::from_secs(60)).expect("load");
        fs::remove_dir_all(&dir).expect("cleanup");
        let temperatures: Vec<f32> = loaded
            .measurements
            .iter()
            .map(|m| m.sample.temperature)
            .collect();
        assert_eq!(temperatures, [21.0, 22.0]);
        assert_eq!(loaded.skipped_lines, 1);
    }
}
