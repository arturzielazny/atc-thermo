//! Application state and input handling, independent of how it is drawn.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use atc::{Correction, MAX_NAME_LEN};
use bluer::Address;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::sync::mpsc::UnboundedSender;

use crate::calibrate;
use crate::devices::{Devices, HISTORY_SPAN, LoggedSample, Sample};
use crate::forms::SettingsForm;
use crate::jobs::{Action, IDENTIFY_DURATION, Job, JobEvent, Outcome, RateTrim};
use crate::level::LevelingScreen;
use crate::scanner::{ScanControl, ScanEvent};
use crate::store::{CalibrationRecord, Store};

/// Chart time spans the `w` key cycles through.
const WINDOWS: [Duration; 4] = [
    Duration::from_secs(10 * 60),
    Duration::from_secs(60 * 60),
    Duration::from_secs(6 * 60 * 60),
    Duration::from_secs(24 * 60 * 60),
];
const DEFAULT_WINDOW: usize = 1;

/// How far back missing history is fetched from device logs.
const DOWNLOAD_SPAN: Duration = Duration::from_secs(24 * 60 * 60);
/// A hole in history longer than this triggers a log download.
const MAX_HISTORY_GAP: Duration = Duration::from_secs(15 * 60);

/// Discovery restarts this long after the last device job, unless another one starts.
const SCAN_RESTART_DELAY: Duration = Duration::from_secs(5);

/// Devices heard within this span get their settings read when the levelling screen
/// opens; others are probably out of range, and a job would wait for them in vain.
const RECENTLY_HEARD: Duration = Duration::from_secs(2 * 60);

/// Calibration window when the screen opens: the last 6 h.
const DEFAULT_CALIBRATION_SPAN: Duration = Duration::from_secs(6 * 3600);
/// How far `←`/`→` move a calibration window edge, and with Shift.
const WINDOW_NUDGE: Duration = Duration::from_secs(15 * 60);
const WINDOW_JUMP: Duration = Duration::from_secs(3600);
/// How often the calibration screen refits as new data arrives.
const CALIBRATION_REFRESH: Duration = Duration::from_secs(60);

/// How the chart draws samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChartStyle {
    /// Consecutive samples joined by lines.
    Lines,
    /// Every sample on its own.
    Points,
}

/// Which reading the chart shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Metric {
    Temperature,
    Humidity,
    /// Signal strength of received adverts.
    Rssi,
}

impl Metric {
    pub fn label(self) -> &'static str {
        match self {
            Self::Temperature => "Temperature",
            Self::Humidity => "Humidity",
            Self::Rssi => "Signal",
        }
    }

    pub fn unit(self) -> &'static str {
        match self {
            Self::Temperature => "°C",
            Self::Humidity => "%",
            Self::Rssi => "dBm",
        }
    }

    /// The sample's value; `None` for signal strength of log records.
    pub fn of(self, sample: &Sample) -> Option<f32> {
        match self {
            Self::Temperature => Some(sample.temperature),
            Self::Humidity => Some(sample.humidity),
            Self::Rssi => sample.rssi.map(f32::from),
        }
    }

    /// Smallest y-axis span, so sensor noise doesn't fill the whole chart.
    pub fn min_span(self) -> f64 {
        match self {
            Self::Temperature => 0.1,
            Self::Humidity => 1.0,
            Self::Rssi => 10.0,
        }
    }

    /// Finest spacing of horizontal gridlines, near each sensor's noise: about 0.01 °C
    /// for temperature, 0.1 % for humidity; signal strength comes in whole dBm.
    pub fn min_grid_step(self) -> f64 {
        match self {
            Self::Temperature => 0.02,
            Self::Humidity => 0.1,
            Self::Rssi => 1.0,
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

/// What device jobs are doing and have done, per device.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeviceState {
    pub queued: usize,
    pub busy: Option<&'static str>,
    /// Message of the last finished job.
    pub last: Option<Result<String, String>>,
    /// Device minus host clock, in seconds, when last measured.
    pub clock_offset: Option<f64>,
    /// Settings last read from or written to the device.
    pub config: Option<atc::Config>,
}

/// What the keyboard currently controls.
#[derive(Debug)]
pub enum Mode {
    Normal,
    Rename { address: Address, input: String },
    Settings(SettingsForm),
    Calibration(CalibrationScreen),
    Leveling(LevelingScreen),
}

/// A calibration or TX power step waiting for `y`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirm {
    Apply,
    Reset,
}

#[derive(Debug)]
pub struct CalibrationScreen {
    /// First moment of data to fit; never before the participating devices' last
    /// calibration, since older data was recorded with other coefficients.
    pub start: SystemTime,
    /// Last moment of data to fit; `None` follows the current time.
    pub end: Option<SystemTime>,
    /// Which edge `←`/`→` move.
    pub editing: WindowEdge,
    pub result: Option<Result<calibrate::Run, String>>,
    pub confirm: Option<Confirm>,
    computed_at: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowEdge {
    Start,
    End,
}

#[derive(Debug)]
pub struct App {
    pub devices: Devices,
    pub metric: Metric,
    pub style: ChartStyle,
    pub scan_status: ScanStatus,
    /// Most recent problem worth showing in the status bar.
    pub warning: Option<String>,
    pub mode: Mode,
    store: Option<Store>,
    jobs: Option<UnboundedSender<Job>>,
    /// Tells the scanner when device connections start and when they are over.
    scan_controls: Option<UnboundedSender<ScanControl>>,
    /// When to restart discovery, once device jobs have been idle for a while.
    restart_scan_at: Option<Instant>,
    states: HashMap<Address, DeviceState>,
    marked: BTreeSet<Address>,
    selected: Option<Address>,
    window_index: usize,
    /// Devices already checked for history gaps this session.
    gap_checked: HashSet<Address>,
    last_calibrations: HashMap<Address, SystemTime>,
    /// When each device's log was last downloaded; older history needs no new download.
    last_downloads: HashMap<Address, SystemTime>,
    quit: bool,
}

impl App {
    /// `jobs` runs device commands; `None` disables everything that needs a connection.
    /// `scan_controls` is told when device connections start and when they are over.
    pub fn new(
        jobs: Option<UnboundedSender<Job>>,
        scan_controls: Option<UnboundedSender<ScanControl>>,
    ) -> Self {
        Self {
            devices: Devices::default(),
            metric: Metric::Temperature,
            style: ChartStyle::Lines,
            scan_status: ScanStatus::Starting,
            warning: None,
            mode: Mode::Normal,
            store: None,
            jobs,
            scan_controls,
            restart_scan_at: None,
            states: HashMap::new(),
            marked: BTreeSet::new(),
            selected: None,
            window_index: DEFAULT_WINDOW,
            gap_checked: HashSet::new(),
            last_calibrations: HashMap::new(),
            last_downloads: HashMap::new(),
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
        match store.last_calibrations() {
            Ok(changes) => self.last_calibrations = changes,
            Err(err) => self.warning = Some(format!("reading calibration log failed: {err:#}")),
        }
        match store.last_downloads() {
            Ok(downloads) => self.last_downloads = downloads,
            Err(err) => self.warning = Some(format!("reading download log failed: {err:#}")),
        }
        match store.last_clock_offsets() {
            Ok(offsets) => {
                for (address, offset) in offsets {
                    self.states.entry(address).or_default().clock_offset = Some(offset);
                }
            }
            Err(err) => self.warning = Some(format!("reading clock log failed: {err:#}")),
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

    pub fn is_marked(&self, address: Address) -> bool {
        self.marked.contains(&address)
    }

    pub fn state(&self, address: Address) -> Option<&DeviceState> {
        self.states.get(&address)
    }

    /// Devices an action applies to: the marked ones, or else the selected one.
    pub fn targets(&self) -> Vec<Address> {
        if self.marked.is_empty() {
            self.selected.into_iter().collect()
        } else {
            self.marked.iter().copied().collect()
        }
    }

    /// Devices compared with each other by calibration: the marked ones if at least
    /// two, else all.
    pub fn compared_devices(&self) -> Vec<Address> {
        if self.marked.len() >= 2 {
            self.marked.iter().copied().collect()
        } else {
            self.devices.iter().map(|d| d.address).collect()
        }
    }

    /// Devices signal levelling applies to: the marked ones, or else all.
    pub fn leveled_devices(&self) -> Vec<Address> {
        if self.marked.is_empty() {
            self.devices.iter().map(|d| d.address).collect()
        } else {
            self.marked.iter().copied().collect()
        }
    }

    pub fn on_scan_event(&mut self, event: ScanEvent) {
        match event {
            ScanEvent::Started { adapter } => self.scan_status = ScanStatus::Scanning { adapter },
            ScanEvent::Advert(advert) => {
                let (address, name) = (advert.address, advert.name.clone());
                if let (Mode::Leveling(screen), Some(rssi)) = (&mut self.mode, advert.rssi) {
                    screen.record(address, rssi);
                }
                match self.devices.record(advert) {
                    Ok(Some(measurement)) => self.save(&[measurement]),
                    Ok(None) => {}
                    Err(err) => self.warning = Some(format!("{name}: {err}")),
                }
                if self.gap_checked.insert(address) {
                    self.download_if_gaps(address);
                }
            }
            ScanEvent::Failed(reason) => self.scan_status = ScanStatus::Failed(reason),
        }
    }

    /// The first time a device is heard, fetches its log if history since the last
    /// download (at most a day back) has holes.
    fn download_if_gaps(&mut self, address: Address) {
        let now = SystemTime::now();
        let since = now - DOWNLOAD_SPAN;
        let check_from = self.download_since(address);
        let has_gap = self
            .devices
            .get(address)
            .is_some_and(|d| d.has_gap(check_from, now, MAX_HISTORY_GAP));
        if has_gap {
            self.enqueue(address, Action::Download { since });
        }
    }

    pub fn on_job_event(&mut self, event: JobEvent) {
        match event {
            JobEvent::Started { address, label } => {
                self.states.entry(address).or_default().busy = Some(label);
            }
            JobEvent::Finished { address, outcome } => {
                let message = match outcome {
                    Ok(outcome) => Ok(self.apply_outcome(address, outcome)),
                    Err(err) => Err(err),
                };
                let state = self.states.entry(address).or_default();
                state.queued = state.queued.saturating_sub(1);
                state.busy = None;
                state.last = Some(message);
                if self.states.values().all(|s| s.queued == 0) {
                    self.restart_scan_at = Some(Instant::now() + SCAN_RESTART_DELAY);
                }
            }
        }
    }

    /// Updates the model with a job's result; returns a short message for the table.
    fn apply_outcome(&mut self, address: Address, outcome: Outcome) -> String {
        match outcome {
            Outcome::Downloaded {
                samples,
                clock_offset,
            } => {
                self.note_clock_offset(address, clock_offset);
                let added = self.merge_downloaded(address, &samples);
                format!("{added} of {} log records added", samples.len())
            }
            Outcome::ClockSynced {
                offset_before,
                rate,
                samples,
            } => {
                self.note_clock_offset(address, 0.0);
                let added = self.merge_downloaded(address, &samples);
                format!(
                    "clock synced, was {}{}; {added} log records added",
                    format_offset(offset_before),
                    describe_rate(rate)
                )
            }
            Outcome::Identified { number } => format!(
                "showing {number} for {} min",
                IDENTIFY_DURATION.as_secs() / 60
            ),
            Outcome::Renamed(name) => {
                self.devices.rename(address, name.clone());
                format!("renamed to {name}")
            }
            Outcome::ConfigRead(config) => {
                self.states.entry(address).or_default().config = Some(config);
                "settings read".to_owned()
            }
            Outcome::Configured(config) => {
                self.states.entry(address).or_default().config = Some(config);
                "settings saved".to_owned()
            }
            Outcome::TxPowerSet { before, config } => {
                self.states.entry(address).or_default().config = Some(config);
                let after = config.tx_power();
                if before == after {
                    return format!("TX power {after} unchanged");
                }
                // Signal strengths heard before the change no longer apply.
                if let Mode::Leveling(screen) = &mut self.mode {
                    screen.forget(address);
                }
                format!("TX power {before} → {after}")
            }
            Outcome::Calibrated {
                before,
                after,
                temperature,
                humidity,
            } => {
                self.record_calibration(address, Some((temperature, humidity)), before, after);
                format!(
                    "calibrated: T {:+.2} °C, H {:+.1} %",
                    temperature.offset, humidity.offset
                )
            }
            Outcome::CalibrationReset { before, after } => {
                self.record_calibration(address, None, before, after);
                "factory calibration restored".to_owned()
            }
        }
    }

    /// Shows a freshly measured clock offset and keeps it for the next start.
    fn note_clock_offset(&mut self, address: Address, offset: f64) {
        self.states.entry(address).or_default().clock_offset = Some(offset);
        if let Some(store) = &self.store
            && let Err(err) = store.record_clock_offset(address, SystemTime::now(), offset)
        {
            self.warning = Some(format!("saving clock log failed: {err:#}"));
        }
    }

    /// Adds downloaded log records to history and notes the download; returns how many were new.
    fn merge_downloaded(&mut self, address: Address, samples: &[LoggedSample]) -> usize {
        let added = self.devices.merge_log(address, samples);
        self.save(&added);
        let now = SystemTime::now();
        self.last_downloads.insert(address, now);
        if let Some(store) = &self.store
            && let Err(err) = store.record_download(address, now)
        {
            self.warning = Some(format!("saving download log failed: {err:#}"));
        }
        added.len()
    }

    /// Start of the history still worth downloading: since the last download, at most a day.
    fn download_since(&self, address: Address) -> SystemTime {
        let day_ago = SystemTime::now() - DOWNLOAD_SPAN;
        self.last_downloads
            .get(&address)
            .map_or(day_ago, |&last| last.max(day_ago))
    }

    fn record_calibration(
        &mut self,
        address: Address,
        corrections: Option<(Correction, Correction)>,
        before: atc::Coefficients,
        after: atc::Coefficients,
    ) {
        let at = SystemTime::now();
        self.last_calibrations.insert(address, at);
        let name = self
            .devices
            .get(address)
            .map(|d| d.name.clone())
            .unwrap_or_default();
        if let Some(store) = &self.store
            && let Err(err) = store.record_calibration(&CalibrationRecord {
                at,
                address,
                name,
                corrections,
                before,
                after,
            })
        {
            self.warning = Some(format!("saving calibration log failed: {err:#}"));
        }
        // Data before the change is now in other units; refit from scratch.
        if let Mode::Calibration(screen) = &mut self.mode {
            screen.computed_at = None;
        }
    }

    fn save(&mut self, measurements: &[crate::devices::Measurement]) {
        let Some(store) = &mut self.store else { return };
        for measurement in measurements {
            if let Err(err) = store.append(measurement) {
                self.warning = Some(format!("saving history failed: {err:#}"));
                return;
            }
        }
    }

    fn enqueue(&mut self, address: Address, action: Action) {
        let Some(jobs) = &self.jobs else {
            self.warning = Some("device commands are unavailable".to_owned());
            return;
        };
        let idle = self.states.values().all(|s| s.queued == 0);
        if jobs.send(Job { address, action }).is_ok() {
            if idle && let Some(controls) = &self.scan_controls {
                let _ = controls.send(ScanControl::Pause);
            }
            self.states.entry(address).or_default().queued += 1;
        } else {
            self.warning = Some("device command worker stopped".to_owned());
        }
    }

    /// Periodic housekeeping, called about once a second.
    pub fn on_tick(&mut self) {
        let idle = self.states.values().all(|s| s.queued == 0);
        if self.restart_scan_at.is_some_and(|at| Instant::now() >= at) {
            self.restart_scan_at = None;
            if idle && let Some(controls) = &self.scan_controls {
                let _ = controls.send(ScanControl::Resume);
            }
        }
        let stale = matches!(
            &self.mode,
            Mode::Calibration(screen)
                if screen.computed_at.is_none_or(|t| t.elapsed() >= CALIBRATION_REFRESH)
        );
        if stale {
            self.refit();
        }
    }

    fn refit(&mut self) {
        let devices = self.compared_devices();
        let Mode::Calibration(screen) = &mut self.mode else {
            return;
        };
        let now = SystemTime::now();
        // A calibration since the screen opened moves the earliest usable data.
        screen.start = calibrate::window_start(screen.start, &devices, &self.last_calibrations);
        let until = screen.end.unwrap_or(now);
        screen.result = Some(
            calibrate::run(&self.devices, &devices, screen.start, until).map_err(|e| e.to_string()),
        );
        screen.computed_at = Some(Instant::now());
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return;
        }
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::Normal => self.on_normal_key(key.code),
            Mode::Rename { address, input } => self.on_rename_key(key.code, address, input),
            Mode::Settings(form) => self.on_settings_key(key.code, form),
            Mode::Calibration(screen) => self.on_calibration_key(key, screen),
            Mode::Leveling(screen) => self.on_leveling_key(key.code, screen),
        }
    }

    /// Moves the selection and marks devices; returns whether `code` was such a key.
    fn on_selection_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Char(' ') => {
                if let Some(address) = self.selected {
                    if !self.marked.remove(&address) {
                        self.marked.insert(address);
                    }
                    self.move_selection(1);
                }
            }
            KeyCode::Char('a') => {
                if self.marked.len() == self.devices.len() {
                    self.marked.clear();
                } else {
                    self.marked = self.devices.iter().map(|d| d.address).collect();
                }
            }
            _ => return false,
        }
        true
    }

    fn on_normal_key(&mut self, code: KeyCode) {
        if self.on_selection_key(code) {
            return;
        }
        match code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc => {
                if self.marked.is_empty() {
                    self.selected = None;
                } else {
                    self.marked.clear();
                }
            }
            KeyCode::Char('t') => self.metric = Metric::Temperature,
            KeyCode::Char('h') => self.metric = Metric::Humidity,
            KeyCode::Char('r') => self.metric = Metric::Rssi,
            KeyCode::Tab => {
                self.metric = match self.metric {
                    Metric::Temperature => Metric::Humidity,
                    Metric::Humidity => Metric::Rssi,
                    Metric::Rssi => Metric::Temperature,
                };
            }
            KeyCode::Char('w') => self.window_index = (self.window_index + 1) % WINDOWS.len(),
            KeyCode::Char('p') => {
                self.style = match self.style {
                    ChartStyle::Lines => ChartStyle::Points,
                    ChartStyle::Points => ChartStyle::Lines,
                };
            }
            KeyCode::Char('n') => self.start_rename(),
            KeyCode::Char('s') => {
                let targets = self.targets();
                if targets.is_empty() {
                    self.warn_no_targets();
                }
                self.sync_clocks(&targets);
            }
            KeyCode::Char('d') => {
                let since = SystemTime::now() - DOWNLOAD_SPAN;
                self.for_targets(|_| Action::Download { since });
            }
            KeyCode::Char('i') => {
                let numbers: Vec<(Address, u8)> = self
                    .targets()
                    .into_iter()
                    .filter_map(|address| Some((address, self.device_number(address)?)))
                    .collect();
                if numbers.is_empty() {
                    self.warn_no_targets();
                }
                for (address, number) in numbers {
                    self.enqueue(address, Action::Identify { number });
                }
            }
            KeyCode::Char('e') => {
                let targets = self.targets();
                if targets.is_empty() {
                    self.warn_no_targets();
                } else {
                    self.mode = Mode::Settings(SettingsForm::new(targets));
                }
            }
            KeyCode::Char('c') => {
                let requested = SystemTime::now() - DEFAULT_CALIBRATION_SPAN;
                let devices = self.compared_devices();
                self.mode = Mode::Calibration(CalibrationScreen {
                    start: calibrate::window_start(requested, &devices, &self.last_calibrations),
                    end: None,
                    editing: WindowEdge::Start,
                    result: None,
                    confirm: None,
                    computed_at: None,
                });
                self.refit();
            }
            KeyCode::Char('l') => {
                self.metric = Metric::Rssi;
                self.mode = Mode::Leveling(LevelingScreen::new(Instant::now()));
                self.read_unknown_configs();
            }
            _ => {}
        }
    }

    /// The device's row number in the table, from 1; what `i` shows on its screen.
    pub fn device_number(&self, address: Address) -> Option<u8> {
        let index = self.devices.iter().position(|d| d.address == address)?;
        u8::try_from(index + 1).ok()
    }

    fn start_rename(&mut self) {
        match self.targets()[..] {
            [address] => {
                let input = self
                    .devices
                    .get(address)
                    .map(|d| d.name.clone())
                    .unwrap_or_default();
                self.mode = Mode::Rename { address, input };
            }
            [] => self.warn_no_targets(),
            _ => self.warning = Some("rename one device at a time".to_owned()),
        }
    }

    fn sync_clocks(&mut self, addresses: &[Address]) {
        for &address in addresses {
            let download_since = self.download_since(address);
            self.enqueue(address, Action::SyncClock { download_since });
        }
    }

    fn for_targets(&mut self, action: impl Fn(Address) -> Action) {
        let targets = self.targets();
        if targets.is_empty() {
            self.warn_no_targets();
        }
        for address in targets {
            self.enqueue(address, action(address));
        }
    }

    fn warn_no_targets(&mut self) {
        self.warning = Some("select a device (↑↓) or mark some (space, a) first".to_owned());
    }

    fn on_rename_key(&mut self, code: KeyCode, address: Address, mut input: String) {
        match code {
            KeyCode::Esc => {}
            KeyCode::Enter => {
                let unchanged = self.devices.get(address).is_some_and(|d| d.name == input);
                if !unchanged {
                    self.enqueue(address, Action::Rename(input));
                }
            }
            KeyCode::Backspace => {
                input.pop();
                self.mode = Mode::Rename { address, input };
            }
            KeyCode::Char(c) => {
                if input.len() + c.len_utf8() <= MAX_NAME_LEN && c != ',' {
                    input.push(c);
                }
                self.mode = Mode::Rename { address, input };
            }
            _ => self.mode = Mode::Rename { address, input },
        }
    }

    fn on_settings_key(&mut self, code: KeyCode, mut form: SettingsForm) {
        match code {
            KeyCode::Esc => return,
            KeyCode::Enter => {
                let settings = form.settings();
                if !settings.is_empty() {
                    for &address in &form.targets {
                        self.enqueue(address, Action::Configure(settings));
                    }
                }
                return;
            }
            KeyCode::Up | KeyCode::Char('k') => form.move_field(-1),
            KeyCode::Down | KeyCode::Char('j') => form.move_field(1),
            KeyCode::Left | KeyCode::Char('h') => form.change(-1),
            KeyCode::Right | KeyCode::Char('l') => form.change(1),
            _ => {}
        }
        self.mode = Mode::Settings(form);
    }

    fn on_calibration_key(&mut self, key: KeyEvent, mut screen: CalibrationScreen) {
        let code = key.code;
        if let Some(confirm) = screen.confirm.take() {
            if code == KeyCode::Char('y') {
                match confirm {
                    Confirm::Apply => self.apply_calibration(screen.result.as_ref()),
                    Confirm::Reset => {
                        for address in self.compared_devices() {
                            self.enqueue(address, Action::ResetCalibration);
                        }
                    }
                }
            }
            self.mode = Mode::Calibration(screen);
            return;
        }
        match code {
            KeyCode::Esc | KeyCode::Char('q') => return,
            KeyCode::Tab | KeyCode::BackTab => {
                screen.editing = match screen.editing {
                    WindowEdge::Start => WindowEdge::End,
                    WindowEdge::End => WindowEdge::Start,
                };
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char('h' | 'l') => {
                let step = if key.modifiers.contains(KeyModifiers::SHIFT) {
                    WINDOW_JUMP
                } else {
                    WINDOW_NUDGE
                };
                let later = matches!(code, KeyCode::Right | KeyCode::Char('l'));
                let earliest = self.earliest_calibration_start();
                move_window_edge(&mut screen, step, later, earliest, SystemTime::now());
                screen.computed_at = None;
            }
            KeyCode::Enter => {
                if matches!(screen.result, Some(Ok(_))) {
                    screen.confirm = Some(Confirm::Apply);
                }
            }
            KeyCode::Char('x') => screen.confirm = Some(Confirm::Reset),
            KeyCode::Char('s') => {
                let devices = self.compared_devices();
                self.sync_clocks(&devices);
            }
            _ => {}
        }
        self.mode = Mode::Calibration(screen);
        self.on_tick();
    }

    /// Reads the settings of recently heard devices whose settings are not known yet.
    fn read_unknown_configs(&mut self) {
        let now = SystemTime::now();
        let unknown: Vec<Address> = self
            .devices
            .iter()
            .filter(|d| now.duration_since(d.last_seen).unwrap_or_default() < RECENTLY_HEARD)
            .map(|d| d.address)
            .filter(|&a| {
                self.state(a)
                    .is_none_or(|s| s.config.is_none() && s.queued == 0)
            })
            .collect();
        for address in unknown {
            self.enqueue(address, Action::ReadConfig);
        }
    }

    fn on_leveling_key(&mut self, code: KeyCode, mut screen: LevelingScreen) {
        if let Some(confirm) = screen.confirm.take() {
            if code == KeyCode::Char('y') {
                match confirm {
                    Confirm::Apply => {
                        for (address, change) in self.leveling_changes(&screen) {
                            let change_db = f32::from(change);
                            self.enqueue(address, Action::AdjustTxPower { change_db });
                        }
                    }
                    Confirm::Reset => {
                        for address in self.factory_resets() {
                            self.enqueue(address, Action::ResetTxPower);
                        }
                    }
                }
            }
            self.mode = Mode::Leveling(screen);
            return;
        }
        if self.on_selection_key(code) {
            self.mode = Mode::Leveling(screen);
            return;
        }
        match code {
            KeyCode::Esc | KeyCode::Char('q') => return,
            KeyCode::Left | KeyCode::Char('h') => screen.nudge_target(-1),
            KeyCode::Right | KeyCode::Char('l') => screen.nudge_target(1),
            KeyCode::Char('r') => screen.restart(Instant::now()),
            KeyCode::Enter => {
                if !self.leveling_changes(&screen).is_empty() {
                    screen.confirm = Some(Confirm::Apply);
                } else if screen.plan(&self.leveled_devices()).has_medians() {
                    self.warning = Some("nothing to change".to_owned());
                } else {
                    self.warning = Some(format!(
                        "wait until devices are heard {} times",
                        crate::level::MIN_ADVERTS
                    ));
                }
            }
            KeyCode::Char('x') => {
                if self.factory_resets().is_empty() {
                    self.warning = Some("TX power is already the factory value".to_owned());
                } else {
                    screen.confirm = Some(Confirm::Reset);
                }
            }
            _ => {}
        }
        self.mode = Mode::Leveling(screen);
    }

    /// The TX power changes `y` queues after `Enter`, in dB. Devices with a job waiting
    /// are left out: a queued change was planned from the same samples.
    pub fn leveling_changes(&self, screen: &LevelingScreen) -> Vec<(Address, i16)> {
        screen
            .plan(&self.leveled_devices())
            .rows
            .iter()
            .filter_map(|row| Some((row.address, row.change?)))
            .filter(|(address, _)| self.state(*address).is_none_or(|s| s.queued == 0))
            .collect()
    }

    /// The devices `y` restores the factory TX power on after `x`: those not known
    /// to have it already.
    pub fn factory_resets(&self) -> Vec<Address> {
        self.leveled_devices()
            .into_iter()
            .filter(|&address| {
                self.state(address)
                    .and_then(|s| s.config)
                    .is_none_or(|c| c.tx_power() != atc::TxPower::FACTORY)
            })
            .collect()
    }

    /// Where the calibration window would start if moved as far back as allowed.
    pub fn earliest_calibration_start(&self) -> SystemTime {
        calibrate::window_start(
            SystemTime::UNIX_EPOCH,
            &self.compared_devices(),
            &self.last_calibrations,
        )
    }

    fn apply_calibration(&mut self, result: Option<&Result<calibrate::Run, String>>) {
        let Some(Ok(run)) = result else { return };
        let jobs: Vec<(Address, Correction, Correction)> = run
            .devices
            .iter()
            .zip(&run.report.sensors)
            .filter_map(|(&(address, _), sensor)| {
                let (t, h) = calibrate::corrections(sensor);
                (t.is_some() || h.is_some()).then(|| {
                    (
                        address,
                        t.unwrap_or(Correction::IDENTITY),
                        h.unwrap_or(Correction::IDENTITY),
                    )
                })
            })
            .collect();
        for (address, temperature, humidity) in jobs {
            self.enqueue(
                address,
                Action::Calibrate {
                    temperature,
                    humidity,
                },
            );
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

/// `"ok"`, `"+12s"`, `"-3m"`, `"+2.4h"`.
/// Status text after the clock offset, e.g. `", drift +19 s/day trimmed"`.
fn describe_rate(rate: RateTrim) -> String {
    let per_day = |drift: f64| format!("{:+.1} s/day", drift * 86_400.0);
    match rate {
        RateTrim::Unknown => String::new(),
        RateTrim::Trimmed { drift } => format!(", drift {} trimmed", per_day(drift)),
        RateTrim::TooLarge { drift } => format!(", drift {} too large to trim", per_day(drift)),
        RateTrim::Unsupported => ", rate not adjustable".to_owned(),
    }
}

/// Moves the edge being edited by `step`, snapped to round local times (14:15, 14:30,
/// ...), keeping the start at or after `earliest` and before the end. An end at or
/// past `now` follows the current time again.
fn move_window_edge(
    screen: &mut CalibrationScreen,
    step: Duration,
    later: bool,
    earliest: SystemTime,
    now: SystemTime,
) {
    let end = screen.end.unwrap_or(now);
    match screen.editing {
        WindowEdge::Start => {
            let moved = snap(screen.start, step, later).max(earliest);
            screen.start = moved.min(end - WINDOW_NUDGE).max(earliest);
        }
        WindowEdge::End => {
            let moved = snap(end, step, later).max(screen.start + WINDOW_NUDGE);
            screen.end = (moved < now).then_some(moved);
        }
    }
}

/// The next multiple of `step` on the local clock after `time`, or before it.
fn snap(time: SystemTime, step: Duration, later: bool) -> SystemTime {
    let step = i64::try_from(step.as_secs()).expect("window steps are small");
    let local = crate::localtime::local_seconds(time);
    let offset = local - unix_seconds(time);
    let snapped = if later {
        local.div_euclid(step) * step + step
    } else {
        (local - 1).div_euclid(step) * step
    };
    SystemTime::UNIX_EPOCH + Duration::from_secs(u64::try_from(snapped - offset).unwrap_or(0))
}

fn unix_seconds(time: SystemTime) -> i64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

pub fn format_offset(secs: f64) -> String {
    match secs.abs() {
        a if a < 1.5 => "ok".to_owned(),
        a if a < 90.0 => format!("{secs:+.0}s"),
        a if a < 90.0 * 60.0 => format!("{:+.0}m", secs / 60.0),
        a if a < 48.0 * 3600.0 => format!("{:+.1}h", secs / 3600.0),
        _ => format!("{:+.0}d", secs / 86_400.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calibration_window_edges_snap_and_stay_ordered() {
        let local = |time| crate::localtime::local_seconds(time);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_793_200_000);
        let earliest = now - Duration::from_secs(5 * 3600);
        let mut screen = CalibrationScreen {
            start: now - Duration::from_secs(3 * 3600),
            end: None,
            editing: WindowEdge::Start,
            result: None,
            confirm: None,
            computed_at: None,
        };
        // Earlier by 15 min lands on a quarter hour of the local clock.
        move_window_edge(&mut screen, WINDOW_NUDGE, false, earliest, now);
        assert_eq!(local(screen.start) % 900, 0);
        assert!(screen.start < now - Duration::from_secs(3 * 3600));
        // Never before the last calibration.
        for _ in 0..30 {
            move_window_edge(&mut screen, WINDOW_JUMP, false, earliest, now);
        }
        assert_eq!(screen.start, earliest);

        // The end leaves "now", snaps, and cannot pass the start.
        screen.editing = WindowEdge::End;
        move_window_edge(&mut screen, WINDOW_JUMP, false, earliest, now);
        let end = screen.end.expect("moved off now");
        assert_eq!(local(end) % 3600, 0);
        for _ in 0..30 {
            move_window_edge(&mut screen, WINDOW_JUMP, false, earliest, now);
        }
        assert_eq!(screen.end, Some(screen.start + WINDOW_NUDGE));
        // Moving it past the present follows the clock again.
        for _ in 0..30 {
            move_window_edge(&mut screen, WINDOW_JUMP, true, earliest, now);
        }
        assert_eq!(screen.end, None);
    }

    #[test]
    fn offsets() {
        assert_eq!(format_offset(0.4), "ok");
        assert_eq!(format_offset(-37.0), "-37s");
        assert_eq!(format_offset(600.0), "+10m");
        assert_eq!(format_offset(-9000.0), "-2.5h");
        assert_eq!(format_offset(-1_793_040_361.0), "-20753d");
        assert_eq!(
            describe_rate(RateTrim::Trimmed {
                drift: 19.0 / 86_400.0
            }),
            ", drift +19.0 s/day trimmed"
        );
        assert_eq!(describe_rate(RateTrim::Unknown), "");
    }

    use tokio::sync::mpsc::{self, UnboundedReceiver};

    use crate::jobs::Settings;
    use crate::scanner::Advert;

    const A: Address = Address([0xa4, 0xc1, 0x38, 0, 0, 1]);
    const B: Address = Address([0xa4, 0xc1, 0x38, 0, 0, 2]);
    // A pvvx measurement packet (temperature 22.44 °C, humidity 56.94 %).
    const MEASUREMENT: [u8; 11] = [
        0x40, 0x00, 0xa4, 0x01, 0x57, 0x02, 0xc4, 0x08, 0x03, 0x3e, 0x16,
    ];

    fn app_with_devices() -> (App, UnboundedReceiver<Job>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut app = App::new(Some(tx), None);
        for (address, name) in [(A, "ATC_000001"), (B, "ATC_000002")] {
            app.on_scan_event(ScanEvent::Advert(Advert {
                address,
                name: name.to_owned(),
                rssi: Some(-50),
                payload: MEASUREMENT.to_vec(),
                received: SystemTime::now(),
            }));
        }
        (app, rx)
    }

    fn press(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn drain(rx: &mut UnboundedReceiver<Job>) -> Vec<Job> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    #[test]
    fn new_devices_without_history_get_their_log_downloaded() {
        let (_app, mut rx) = app_with_devices();
        let jobs = drain(&mut rx);
        assert_eq!(jobs.len(), 2);
        assert!(
            jobs.iter()
                .all(|j| matches!(j.action, Action::Download { .. }))
        );
    }

    #[test]
    fn actions_apply_to_marked_devices_or_the_selected_one() {
        let (mut app, mut rx) = app_with_devices();
        drain(&mut rx);

        press(&mut app, KeyCode::Char('s'));
        assert!(drain(&mut rx).is_empty(), "nothing selected yet");
        assert!(app.warning.is_some());

        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('s'));
        let jobs = drain(&mut rx);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].address, A);
        assert!(matches!(jobs[0].action, Action::SyncClock { .. }));

        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('s'));
        let addresses: Vec<Address> = drain(&mut rx).iter().map(|j| j.address).collect();
        assert_eq!(addresses, [A, B]);

        // Each marked device shows its own row number.
        press(&mut app, KeyCode::Char('i'));
        let actions: Vec<Action> = drain(&mut rx).into_iter().map(|j| j.action).collect();
        assert_eq!(
            actions,
            [
                Action::Identify { number: 1 },
                Action::Identify { number: 2 }
            ]
        );

        // Esc clears the marks first, then the selection.
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.targets(), [A]);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.targets(), Vec::<Address>::new());
    }

    #[test]
    fn space_toggles_marks_and_advances() {
        let (mut app, _rx) = app_with_devices();
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char(' '));
        assert!(app.is_marked(A));
        assert_eq!(app.selected_index(), Some(1));
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Char(' '));
        assert!(!app.is_marked(A));
    }

    #[test]
    fn rename_edits_the_current_name() {
        let (mut app, mut rx) = app_with_devices();
        drain(&mut rx);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('n'));
        for _ in 0..6 {
            press(&mut app, KeyCode::Backspace);
        }
        for c in "Kitchen".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
        let jobs = drain(&mut rx);
        assert_eq!(
            jobs,
            [Job {
                address: A,
                action: Action::Rename("ATC_Kitchen".to_owned())
            }]
        );
        assert!(matches!(app.mode, Mode::Normal));

        // Renaming several devices at once is refused.
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('n'));
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(drain(&mut rx), Vec::<Job>::new());
    }

    #[test]
    fn scanning_pauses_for_device_jobs_and_resumes_after_them() {
        let (jobs, _rx) = mpsc::unbounded_channel();
        let (controls, mut controls_rx) = mpsc::unbounded_channel();
        let mut app = App::new(Some(jobs), Some(controls));
        // The first adverts queue two log downloads; one pause covers both.
        for address in [A, B] {
            app.on_scan_event(ScanEvent::Advert(Advert {
                address,
                name: "ATC_00000X".to_owned(),
                rssi: Some(-50),
                payload: MEASUREMENT.to_vec(),
                received: SystemTime::now(),
            }));
        }
        let mut sent = || std::iter::from_fn(|| controls_rx.try_recv().ok()).collect::<Vec<_>>();
        assert_eq!(sent(), [ScanControl::Pause]);

        for address in [A, B] {
            app.on_job_event(JobEvent::Finished {
                address,
                outcome: Err("no log".to_owned()),
            });
        }
        app.on_tick();
        assert_eq!(sent(), []);
        app.restart_scan_at = Some(Instant::now());
        app.on_tick();
        assert_eq!(sent(), [ScanControl::Resume]);
    }

    #[test]
    fn leveling_turns_loud_devices_down_to_the_target() {
        let (mut app, mut rx) = app_with_devices();
        for address in [A, B] {
            finish(&mut app, address, Err("no log".to_owned()));
        }
        drain(&mut rx);
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(app.metric, Metric::Rssi);
        // Opening the screen reads the settings it shows.
        let reads: Vec<_> = drain(&mut rx).into_iter().map(|j| j.action).collect();
        assert_eq!(reads, [Action::ReadConfig, Action::ReadConfig]);
        for address in [A, B] {
            finish(
                &mut app,
                address,
                Ok(Outcome::ConfigRead(config_with_power(169))),
            );
        }
        // Nothing is heard yet.
        press(&mut app, KeyCode::Enter);
        assert_eq!(drain(&mut rx), Vec::<Job>::new());

        for _ in 0..crate::level::MIN_ADVERTS {
            for (address, rssi) in [(A, -50), (B, -70)] {
                app.on_scan_event(ScanEvent::Advert(Advert {
                    address,
                    name: "ATC_00000X".to_owned(),
                    rssi: Some(rssi),
                    payload: MEASUREMENT.to_vec(),
                    received: SystemTime::now(),
                }));
            }
        }
        // Down to the default target, -70 dBm, where B already is; only after `y`.
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(drain(&mut rx), Vec::<Job>::new());
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('y'));
        let expected = [Job {
            address: A,
            action: Action::AdjustTxPower { change_db: -20.0 },
        }];
        assert_eq!(drain(&mut rx), expected);
        // Not again while the change is queued.
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('y'));
        assert_eq!(drain(&mut rx), Vec::<Job>::new());

        let before = atc::TxPower::FACTORY;
        let config = config_with_power(150);
        finish(&mut app, A, Ok(Outcome::TxPowerSet { before, config }));
        assert_eq!(
            app.state(A).and_then(|s| s.last.clone()),
            Some(Ok("TX power +0.0 dBm → -5.0 dBm".to_owned()))
        );
        assert_eq!(app.state(A).and_then(|s| s.config), Some(config));
        let Mode::Leveling(screen) = &app.mode else {
            panic!("still levelling");
        };
        // Signal heard at the old power is dropped.
        let plan = screen.plan(&app.leveled_devices());
        assert_eq!(plan.rows[0].adverts, 0);
        assert_eq!(plan.rows[1].median, Some(-70));

        // Factory TX power, only where it isn't already.
        press(&mut app, KeyCode::Char('x'));
        press(&mut app, KeyCode::Char('y'));
        let expected = [Job {
            address: A,
            action: Action::ResetTxPower,
        }];
        assert_eq!(drain(&mut rx), expected);
        press(&mut app, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn leveling_screen_selects_and_marks_devices() {
        let (mut app, mut rx) = app_with_devices();
        drain(&mut rx);
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(app.leveled_devices(), [A, B]);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(app.leveled_devices(), [A]);
        assert_eq!(app.selected_index(), Some(1));
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(app.leveled_devices(), [A, B]);
        assert!(app.is_marked(B));
        assert!(matches!(app.mode, Mode::Leveling(_)));
    }

    fn finish(app: &mut App, address: Address, outcome: Result<Outcome, String>) {
        app.on_job_event(JobEvent::Finished { address, outcome });
    }

    /// The factory config of a LYWSD03MMC on firmware v5.9 with another TX power code.
    fn config_with_power(code: u8) -> atc::Config {
        let mut bytes = [
            0x87, 0x10, 0x0a, 0x06, 0x28, 0x04, 0xa9, 0x31, 0x31, 0x04, 0xb4,
        ];
        bytes[6] = code;
        atc::Config::from_bytes(bytes)
    }

    #[test]
    fn settings_form_sends_only_changed_fields() {
        let (mut app, mut rx) = app_with_devices();
        drain(&mut rx);
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('e'));
        press(&mut app, KeyCode::Right); // advertising: keep → 1 s
        press(&mut app, KeyCode::Enter);
        let jobs = drain(&mut rx);
        assert_eq!(jobs.len(), 2);
        let expected = Settings {
            advertising_interval: Some(Duration::from_secs(1)),
            ..Settings::default()
        };
        assert!(jobs.iter().all(|j| j.action == Action::Configure(expected)));
    }

    #[test]
    fn downloaded_records_fill_history_and_are_remembered() {
        let (mut app, mut rx) = app_with_devices();
        drain(&mut rx);
        let at = SystemTime::now() - Duration::from_secs(3600);
        app.on_job_event(JobEvent::Started {
            address: A,
            label: "downloading log",
        });
        app.on_job_event(JobEvent::Finished {
            address: A,
            outcome: Ok(Outcome::Downloaded {
                samples: vec![LoggedSample {
                    at,
                    temperature: 20.0,
                    humidity: 50.0,
                    voltage: 2.9,
                    span: Duration::from_secs(1800),
                }],
                clock_offset: -5.0,
            }),
        });
        let device = app.devices.get(A).expect("device");
        assert_eq!(device.history().len(), 2);
        let state = app.state(A).expect("state");
        assert_eq!(state.clock_offset, Some(-5.0));
        assert_eq!(state.busy, None);
        assert_eq!(state.last, Some(Ok("1 of 1 log records added".to_owned())));
        // Later downloads start from now, not a day back.
        assert!(app.download_since(A) > at);
    }
}
