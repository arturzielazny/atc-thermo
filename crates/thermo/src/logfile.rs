//! Debug log of device jobs, connection steps and Bluetooth stack messages, written
//! to `thermo.log` next to the history so it can be sent along with a bug report.
//!
//! `THERMO_LOG` sets the level (`off`, `error`, `warn`, `info` (default), `debug`,
//! `trace`); `THERMO_LOG_FILE` the path. At `debug` and above, messages from
//! `btleplug` are included too. The previous run's log is kept as `thermo.log.1`.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use jiff::Zoned;
use log::{Level, LevelFilter, Log, Metadata, Record};

pub const FILE_NAME: &str = "thermo.log";

struct FileLog {
    file: Mutex<File>,
    level: LevelFilter,
}

impl FileLog {
    /// Our own crates log at the chosen level; the BLE stack only warnings and errors
    /// unless debugging.
    fn wanted(&self, target: &str, level: Level) -> bool {
        let own = ["thermo", "atc"]
            .iter()
            .any(|c| target == *c || target.starts_with(&format!("{c}::")));
        let limit = if own || self.level >= LevelFilter::Debug {
            self.level
        } else {
            self.level.min(LevelFilter::Warn)
        };
        level <= limit
    }
}

impl Log for FileLog {
    fn enabled(&self, metadata: &Metadata) -> bool {
        self.wanted(metadata.target(), metadata.level())
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format_line(
            &Zoned::now(),
            record.level(),
            record.target(),
            record.args(),
        );
        if let Ok(mut file) = self.file.lock() {
            let _ = file.write_all(line.as_bytes());
        }
    }

    fn flush(&self) {
        if let Ok(mut file) = self.file.lock() {
            let _ = file.flush();
        }
    }
}

fn format_line(at: &Zoned, level: Level, target: &str, message: &std::fmt::Arguments) -> String {
    format!(
        "{} {level:<5} {target}: {message}\n",
        at.strftime("%Y-%m-%d %H:%M:%S%.3f %:z")
    )
}

/// Parses `THERMO_LOG`; `None` means the default.
fn parse_level(value: &str) -> Option<LevelFilter> {
    value.trim().parse().ok()
}

/// Opens the log and installs it as the `log` backend. Returns the path written to,
/// or `None` if logging is off.
///
/// # Errors
///
/// Fails if the file cannot be created.
pub fn init(data_dir: Option<&Path>) -> io::Result<Option<PathBuf>> {
    let level = std::env::var("THERMO_LOG")
        .ok()
        .and_then(|v| parse_level(&v))
        .unwrap_or(LevelFilter::Info);
    if level == LevelFilter::Off {
        return Ok(None);
    }
    let path = match std::env::var_os("THERMO_LOG_FILE") {
        Some(path) => PathBuf::from(path),
        None => match data_dir {
            Some(dir) => dir.join(FILE_NAME),
            None => return Ok(None),
        },
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if path.exists() {
        let mut previous = path.clone().into_os_string();
        previous.push(".1");
        let _ = fs::rename(&path, previous);
    }
    let file = File::create(&path)?;
    let logger = FileLog {
        file: Mutex::new(file),
        level,
    };
    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(level);
    }
    log::info!(
        "thermo {} on {} {}, log level {level}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn logger(level: LevelFilter) -> FileLog {
        FileLog {
            file: Mutex::new(tempfile()),
            level,
        }
    }

    fn tempfile() -> File {
        let path = std::env::temp_dir().join(format!("thermo-log-test-{}", std::process::id()));
        File::create(path).expect("temp dir is writable")
    }

    #[test]
    fn stack_messages_only_when_debugging() {
        let info = logger(LevelFilter::Info);
        assert!(info.wanted("atc::thermometer", Level::Info));
        assert!(info.wanted("thermo::jobs", Level::Info));
        assert!(!info.wanted("atc::thermometer", Level::Debug));
        assert!(!info.wanted("btleplug::corebluetooth", Level::Info));
        assert!(info.wanted("btleplug::corebluetooth", Level::Warn));
        let debug = logger(LevelFilter::Debug);
        assert!(debug.wanted("btleplug::corebluetooth", Level::Debug));
        assert!(!debug.wanted("btleplug::corebluetooth", Level::Trace));
    }

    #[test]
    fn levels_parse() {
        assert_eq!(parse_level("debug"), Some(LevelFilter::Debug));
        assert_eq!(parse_level(" TRACE "), Some(LevelFilter::Trace));
        assert_eq!(parse_level("off"), Some(LevelFilter::Off));
        assert_eq!(parse_level("loud"), None);
    }

    #[test]
    fn line_has_time_level_and_target() {
        let at: Zoned = "2026-11-03T10:34:01.5+01:00[Europe/Warsaw]"
            .parse()
            .expect("valid");
        let line = format_line(&at, Level::Warn, "atc::thermometer", &format_args!("x"));
        assert_eq!(
            line,
            "2026-11-03 10:34:01.500 +01:00 WARN  atc::thermometer: x\n"
        );
    }
}
