//! Application state and input handling, independent of how it is drawn.

use std::path::Path;
use std::time::{Duration, SystemTime};

use bluer::Address;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::devices::{Devices, HISTORY_SPAN, Sample};
use crate::scanner::ScanEvent;
use crate::store::Store;

/// Chart time spans the `w` key cycles through.
const WINDOWS: [Duration; 4] = [
    Duration::from_secs(10 * 60),
    Duration::from_secs(60 * 60),
    Duration::from_secs(6 * 60 * 60),
    Duration::from_secs(24 * 60 * 60),
];
const DEFAULT_WINDOW: usize = 1;

/// Which reading the chart shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Metric {
    Temperature,
    Humidity,
}

impl Metric {
    pub fn label(self) -> &'static str {
        match self {
            Self::Temperature => "Temperature",
            Self::Humidity => "Humidity",
        }
    }

    pub fn unit(self) -> &'static str {
        match self {
            Self::Temperature => "°C",
            Self::Humidity => "%",
        }
    }

    pub fn of(self, sample: &Sample) -> f32 {
        match self {
            Self::Temperature => sample.temperature,
            Self::Humidity => sample.humidity,
        }
    }

    /// Smallest y-axis span, so sensor noise doesn't fill the whole chart.
    pub fn min_span(self) -> f64 {
        match self {
            Self::Temperature => 1.0,
            Self::Humidity => 4.0,
        }
    }
}

/// State of the background scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanStatus {
    Starting,
    Scanning { adapter: String },
    Failed(String),
}

#[derive(Debug)]
pub struct App {
    pub devices: Devices,
    pub metric: Metric,
    pub scan_status: ScanStatus,
    /// Most recent problem worth showing in the status bar.
    pub warning: Option<String>,
    store: Option<Store>,
    selected: Option<Address>,
    window_index: usize,
    quit: bool,
}

impl App {
    pub fn new() -> Self {
        Self {
            devices: Devices::default(),
            metric: Metric::Temperature,
            scan_status: ScanStatus::Starting,
            warning: None,
            store: None,
            selected: None,
            window_index: DEFAULT_WINDOW,
            quit: false,
        }
    }

    /// Loads recent history from `store` and saves every new measurement to it.
    pub fn use_store(&mut self, store: Store) {
        let since = SystemTime::now() - HISTORY_SPAN;
        match store.load(since) {
            Ok(loaded) => {
                for measurement in loaded.measurements {
                    self.devices.restore(measurement);
                }
                if loaded.skipped_lines > 0 {
                    self.warning = Some(format!(
                        "skipped {} unreadable history lines",
                        loaded.skipped_lines
                    ));
                }
            }
            Err(err) => self.warning = Some(format!("loading history failed: {err:#}")),
        }
        self.store = Some(store);
    }

    pub fn store_dir(&self) -> Option<&Path> {
        self.store.as_ref().map(Store::dir)
    }

    pub fn window(&self) -> Duration {
        WINDOWS[self.window_index]
    }

    pub fn should_quit(&self) -> bool {
        self.quit
    }

    /// Position of the selected device in [`Devices::iter`] order.
    pub fn selected_index(&self) -> Option<usize> {
        let selected = self.selected?;
        self.devices.iter().position(|d| d.address == selected)
    }

    pub fn on_scan_event(&mut self, event: ScanEvent) {
        match event {
            ScanEvent::Started { adapter } => self.scan_status = ScanStatus::Scanning { adapter },
            ScanEvent::Advert(advert) => {
                let name = advert.name.clone();
                match self.devices.record(advert) {
                    Ok(Some(measurement)) => {
                        if let Some(store) = &mut self.store
                            && let Err(err) = store.append(&measurement)
                        {
                            self.warning = Some(format!("saving history failed: {err:#}"));
                        }
                    }
                    Ok(None) => {}
                    Err(err) => self.warning = Some(format!("{name}: {err}")),
                }
            }
            ScanEvent::Failed(reason) => self.scan_status = ScanStatus::Failed(reason),
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc => self.selected = None,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => self.quit = true,
            KeyCode::Char('t') => self.metric = Metric::Temperature,
            KeyCode::Char('h') => self.metric = Metric::Humidity,
            KeyCode::Tab => {
                self.metric = match self.metric {
                    Metric::Temperature => Metric::Humidity,
                    Metric::Humidity => Metric::Temperature,
                };
            }
            KeyCode::Char('w') => self.window_index = (self.window_index + 1) % WINDOWS.len(),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            _ => {}
        }
    }

    fn move_selection(&mut self, step: isize) {
        let count = self.devices.len();
        if count == 0 {
            return;
        }
        let next = match self.selected_index() {
            None if step > 0 => 0,
            None => count - 1,
            Some(i) => i.saturating_add_signed(step).min(count - 1),
        };
        self.selected = self.devices.iter().nth(next).map(|d| d.address);
    }
}
