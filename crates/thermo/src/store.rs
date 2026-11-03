//! Measurement history on disk: append-only CSV, one file per UTC day.
//!
//! Files live in `$XDG_DATA_HOME/thermo` (usually `~/.local/share/thermo`) and are
//! named `YYYY-MM-DD.csv`. Old files are never deleted, so they double as a long-term log.

use std::collections::HashMap;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use atc::DeviceId;
use atc::{Coefficients, Correction};

use crate::devices::{Measurement, Sample, Source};

const HEADER: &str =
    "time_ms,address,name,temperature_c,humidity_pct,battery_pct,voltage_v,source,rssi_dbm";
const CALIBRATION_FILE: &str = "calibrations.csv";
const DOWNLOAD_FILE: &str = "downloads.csv";
const DOWNLOAD_HEADER: &str = "time_ms,address";
const CLOCK_FILE: &str = "clocks.csv";
const CLOCK_HEADER: &str = "time_ms,address,offset_s";
const CALIBRATION_HEADER: &str = "time_ms,address,name,action,temperature_gain,temperature_offset,humidity_gain,humidity_offset,coefficients_before,coefficients_after";
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
                // Files written before the `source` column have a shorter header.
                if line.is_empty() || line.starts_with("time_ms,") {
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
            let file = open_for_append(&self.dir.join(file_name(day)), HEADER)?;
            self.current = Some((day, file));
        }
        let (_, file) = self.current.as_mut().expect("day file was opened above");
        // One write per line, so a crash can at worst leave one partial line.
        file.write_all(format!("{}\n", format_line(measurement)).as_bytes())
            .context("writing measurement")
    }

    /// Appends a coefficient change to `calibrations.csv`.
    ///
    /// # Errors
    ///
    /// Fails if the file cannot be opened or written.
    pub fn record_calibration(&self, record: &CalibrationRecord) -> Result<()> {
        let path = self.dir.join(CALIBRATION_FILE);
        let mut file = open_for_append(&path, CALIBRATION_HEADER)?;
        let ms = millis(record.at);
        let (action, corrections) = match record.corrections {
            Some((t, h)) => (
                "apply",
                format!(
                    "{:.6},{:.4},{:.6},{:.4}",
                    t.gain, t.offset, h.gain, h.offset
                ),
            ),
            None => ("reset", ",,,".to_owned()),
        };
        let line = format!(
            "{ms},{},{},{action},{corrections},{},{}\n",
            record.address,
            sanitize(&record.name),
            format_coefficients(&record.before),
            format_coefficients(&record.after),
        );
        file.write_all(line.as_bytes())
            .with_context(|| format!("writing {}", path.display()))
    }

    /// Time of the most recent coefficient change per device.
    ///
    /// # Errors
    ///
    /// Fails if `calibrations.csv` exists but cannot be read.
    pub fn last_calibrations(&self) -> Result<HashMap<DeviceId, SystemTime>> {
        latest_per_address(&self.dir.join(CALIBRATION_FILE))
    }

    /// Notes that a device's log was downloaded up to `at`.
    ///
    /// # Errors
    ///
    /// Fails if `downloads.csv` cannot be opened or written.
    pub fn record_download(&self, address: DeviceId, at: SystemTime) -> Result<()> {
        let path = self.dir.join(DOWNLOAD_FILE);
        let mut file = open_for_append(&path, DOWNLOAD_HEADER)?;
        file.write_all(format!("{},{address}\n", millis(at)).as_bytes())
            .with_context(|| format!("writing {}", path.display()))
    }

    /// Time of the most recent log download per device.
    ///
    /// # Errors
    ///
    /// Fails if `downloads.csv` exists but cannot be read.
    pub fn last_downloads(&self) -> Result<HashMap<DeviceId, SystemTime>> {
        latest_per_address(&self.dir.join(DOWNLOAD_FILE))
    }

    /// Notes a device's clock offset (device minus host, seconds) measured at `at`.
    ///
    /// # Errors
    ///
    /// Fails if `clocks.csv` cannot be opened or written.
    pub fn record_clock_offset(
        &self,
        address: DeviceId,
        at: SystemTime,
        offset: f64,
    ) -> Result<()> {
        let path = self.dir.join(CLOCK_FILE);
        let mut file = open_for_append(&path, CLOCK_HEADER)?;
        file.write_all(format!("{},{address},{offset:.1}\n", millis(at)).as_bytes())
            .with_context(|| format!("writing {}", path.display()))
    }

    /// The most recently measured clock offset per device.
    ///
    /// # Errors
    ///
    /// Fails if `clocks.csv` exists but cannot be read.
    pub fn last_clock_offsets(&self) -> Result<HashMap<DeviceId, f64>> {
        Ok(latest_rows(&self.dir.join(CLOCK_FILE))?
            .into_iter()
            .filter_map(|(address, (_, rest))| Some((address, rest.parse().ok()?)))
            .collect())
    }
}

/// Reads a CSV whose first two columns are `time_ms,address`; returns the newest time per address.
fn latest_per_address(path: &Path) -> Result<HashMap<DeviceId, SystemTime>> {
    Ok(latest_rows(path)?
        .into_iter()
        .map(|(address, (at, _))| (address, at))
        .collect())
}

/// Reads a CSV whose first two columns are `time_ms,address`; returns the newest line
/// per address as its time and the columns after the address.
fn latest_rows(path: &Path) -> Result<HashMap<DeviceId, (SystemTime, String)>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(err) => return Err(err).with_context(|| format!("opening {}", path.display())),
    };
    let mut latest: HashMap<DeviceId, (SystemTime, String)> = HashMap::new();
    for line in BufReader::new(file).lines() {
        let line = line.with_context(|| format!("reading {}", path.display()))?;
        let mut fields = line.splitn(3, ',');
        let (Some(Ok(ms)), Some(Ok(address))) = (
            fields.next().map(str::parse::<u64>),
            fields.next().map(str::parse::<DeviceId>),
        ) else {
            continue;
        };
        let at = UNIX_EPOCH + Duration::from_millis(ms);
        let rest = fields.next().unwrap_or_default();
        if latest.get(&address).is_none_or(|(newest, _)| at >= *newest) {
            latest.insert(address, (at, rest.to_owned()));
        }
    }
    Ok(latest)
}

/// One change of a device's sensor coefficients.
#[derive(Debug, Clone, PartialEq)]
pub struct CalibrationRecord {
    pub at: SystemTime,
    pub address: DeviceId,
    pub name: String,
    /// The applied `(temperature, humidity)` corrections; `None` for a factory reset.
    pub corrections: Option<(Correction, Correction)>,
    pub before: Coefficients,
    pub after: Coefficients,
}

fn format_coefficients(c: &Coefficients) -> String {
    format!(
        "{};{};{};{}",
        c.temperature_gain, c.temperature_offset, c.humidity_gain, c.humidity_offset
    )
}

/// Opens a CSV file for appending, writing `header` if it is new.
fn open_for_append(path: &Path, header: &str) -> Result<File> {
    upgrade_header(path, header).with_context(|| format!("updating {}", path.display()))?;
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    match file.metadata()?.len() {
        0 => writeln!(file, "{header}")?,
        // Terminate a line torn by a crash so the next record starts cleanly.
        len if last_byte(&mut file, len)? != b'\n' => writeln!(file)?,
        _ => {}
    }
    Ok(file)
}

/// Replaces a file's header with `header` when that only adds columns at the end, so
/// lines written with the new columns still match the header.
fn upgrade_header(path: &Path, header: &str) -> io::Result<()> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    let (first, rest) = contents.split_once('\n').unwrap_or((&contents, ""));
    let outdated = first != header
        && header
            .strip_prefix(first)
            .is_some_and(|added| added.starts_with(','));
    if !outdated {
        return Ok(());
    }
    // Write beside it and rename, so a crash leaves either the old or the new file.
    let temporary = path.with_extension("csv.tmp");
    fs::write(&temporary, format!("{header}\n{rest}"))?;
    fs::rename(&temporary, path)
}

fn millis(time: SystemTime) -> u128 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c == ',' || c.is_control() { ' ' } else { c })
        .collect()
}

fn last_byte(file: &mut File, len: u64) -> io::Result<u8> {
    let mut byte = [0];
    file.seek(SeekFrom::Start(len - 1))?;
    file.read_exact(&mut byte)?;
    Ok(byte[0])
}

fn format_line(m: &Measurement) -> String {
    let ms = millis(m.sample.at);
    let name = sanitize(&m.name);
    let battery = m.battery.map(|b| b.to_string()).unwrap_or_default();
    let voltage = m.voltage.map(|v| format!("{v:.3}")).unwrap_or_default();
    let source = match m.sample.source {
        Source::Live => "",
        Source::Log => "log",
    };
    let rssi = m.sample.rssi.map(|r| r.to_string()).unwrap_or_default();
    format!(
        "{ms},{},{name},{:.2},{:.2},{battery},{voltage},{source},{rssi}",
        m.address, m.sample.temperature, m.sample.humidity
    )
}

fn parse_line(line: &str) -> Option<Measurement> {
    let mut fields = line.split(',');
    let mut next = || fields.next();
    let ms: u64 = next()?.parse().ok()?;
    let address: DeviceId = next()?.parse().ok()?;
    let name = next()?.to_owned();
    let temperature: f32 = next()?.parse().ok()?;
    let humidity: f32 = next()?.parse().ok()?;
    let battery = optional(next()?).ok()?;
    let voltage = optional(next()?).ok()?;
    // Columns added later are missing from older lines.
    let source = match next() {
        None | Some("") => Source::Live,
        Some("log") => Source::Log,
        Some(_) => return None,
    };
    let rssi = optional(next().unwrap_or_default()).ok()?;
    Some(Measurement {
        address,
        name,
        sample: Sample {
            at: UNIX_EPOCH + Duration::from_millis(ms),
            temperature,
            humidity,
            rssi,
            source,
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
            address: DeviceId::Mac([0xa4, 0xc1, 0x38, 0x84, 0x7e, 0xfc]),
            name: "Living, room".to_owned(),
            sample: Sample {
                at,
                temperature,
                humidity: 56.94,
                rssi: Some(-61),
                source: Source::Live,
            },
            battery: Some(87),
            voltage: None,
        }
    }

    #[test]
    fn old_header_gains_new_columns() {
        let dir = std::env::temp_dir().join(format!("thermo-header-test-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("day.csv");
        fs::write(&path, "a,b\n1,2\n").expect("write");
        upgrade_header(&path, "a,b,c").expect("upgrade");
        assert_eq!(fs::read_to_string(&path).expect("read"), "a,b,c\n1,2\n");
        // Anything but appended columns is left alone.
        fs::write(&path, "x,y\n1,2\n").expect("write");
        upgrade_header(&path, "a,b,c").expect("upgrade");
        assert_eq!(fs::read_to_string(&path).expect("read"), "x,y\n1,2\n");
        fs::write(&path, "a,bc\n").expect("write");
        upgrade_header(&path, "a,b").expect("upgrade");
        assert_eq!(fs::read_to_string(&path).expect("read"), "a,bc\n");
        fs::remove_dir_all(&dir).expect("cleanup");
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
            "1793046191123,A4:C1:38:84:7E:FC,Living  room,-3.50,56.94,87,,,-61"
        );
        let parsed = parse_line(&line).expect("round trip");
        assert_eq!(
            parsed.sample,
            Sample {
                at,
                temperature: -3.5,
                humidity: 56.94,
                rssi: Some(-61),
                source: Source::Live,
            }
        );
        assert_eq!(parsed.battery, Some(87));
        assert_eq!(parsed.voltage, None);
        assert!(parse_line("1793046191123,A4:C1:38:84:7E:FC,x,21.").is_none());
        // Lines written before the source column existed.
        let old =
            parse_line("1793046191123,A4:C1:38:84:7E:FC,x,21.00,50.00,,").expect("old format");
        assert_eq!(old.sample.source, Source::Live);
        let logged = parse_line("1793046191123,A4:C1:38:84:7E:FC,x,21.00,50.00,,2.900,log")
            .expect("log line");
        assert_eq!(logged.sample.source, Source::Log);
        assert_eq!(logged.sample.rssi, None);
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

    #[test]
    fn calibration_log() {
        let dir = env::temp_dir().join(format!("thermo-calibration-test-{}", std::process::id()));
        let store = Store::open(dir.clone()).expect("temp dir");
        let address = DeviceId::Mac([0xa4, 0xc1, 0x38, 0x84, 0x7e, 0xfc]);
        let coefficients = Coefficients {
            temperature_gain: 17_500,
            humidity_gain: 12_500,
            temperature_offset: -4_500,
            humidity_offset: -600,
        };
        let early = UNIX_EPOCH + Duration::from_secs(1_793_000_000);
        let late = early + Duration::from_secs(3600);
        for at in [late, early] {
            store
                .record_calibration(&CalibrationRecord {
                    at,
                    address,
                    name: "ATC_847EFC".to_owned(),
                    corrections: Some((Correction::IDENTITY, Correction::IDENTITY)),
                    before: coefficients,
                    after: coefficients,
                })
                .expect("write");
        }
        let latest = store.last_calibrations().expect("read");
        fs::remove_dir_all(&dir).expect("cleanup");
        assert_eq!(latest.get(&address), Some(&late));
    }

    #[test]
    fn clock_offsets_keep_the_newest() {
        let dir = env::temp_dir().join(format!("thermo-clock-test-{}", std::process::id()));
        let store = Store::open(dir.clone()).expect("temp dir");
        let address = DeviceId::Mac([0xa4, 0xc1, 0x38, 0x84, 0x7e, 0xfc]);
        let early = UNIX_EPOCH + Duration::from_secs(1_793_000_000);
        let late = early + Duration::from_secs(3600);
        store
            .record_clock_offset(address, late, 31.0)
            .expect("write");
        store
            .record_clock_offset(address, early, -2.5)
            .expect("write");
        let offsets = store.last_clock_offsets().expect("read");
        fs::remove_dir_all(&dir).expect("cleanup");
        assert_eq!(offsets.get(&address), Some(&31.0));
    }
}
