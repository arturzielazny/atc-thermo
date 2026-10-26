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
use crate::jobs::{Action, Job, JobEvent, Outcome};
use crate::scanner::ScanEvent;
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

/// Calibration windows, in hours, that `←`/`→` choose from.
pub const CALIBRATION_HOURS: [u32; 7] = [1, 2, 3, 6, 12, 24, 48];
const DEFAULT_CALIBRATION_HOURS: usize = 3;
/// How often the calibration screen refits as new data arrives.
const CALIBRATION_REFRESH: Duration = Duration::from_secs(60);

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

/// What device jobs are doing and have done, per device.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeviceState {
    pub queued: usize,
    pub busy: Option<&'static str>,
    /// Message of the last finished job.
    pub last: Option<Result<String, String>>,
    /// Device minus host clock, in seconds, when last measured.
    pub clock_offset: Option<f64>,
}

/// What the keyboard currently controls.
#[derive(Debug)]
pub enum Mode {
    Normal,
    Rename { address: Address, input: String },
    Settings(SettingsForm),
    Calibration(CalibrationScreen),
}

/// A destructive calibration step waiting for `y`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirm {
    Apply,
    Reset,
}

#[derive(Debug)]
pub struct CalibrationScreen {
    pub hours_index: usize,
    pub result: Option<Result<calibrate::Run, String>>,
    pub confirm: Option<Confirm>,
    computed_at: Option<Instant>,
}

impl CalibrationScreen {
    pub fn hours(&self) -> u32 {
        CALIBRATION_HOURS[self.hours_index]
    }
}

#[derive(Debug)]
pub struct App {
    pub devices: Devices,
    pub metric: Metric,
    pub scan_status: ScanStatus,
    /// Most recent problem worth showing in the status bar.
    pub warning: Option<String>,
    pub mode: Mode,
    store: Option<Store>,
    jobs: Option<UnboundedSender<Job>>,
    /// Asks the scanner to restart discovery.
    scan_restarts: Option<UnboundedSender<()>>,
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
    /// `scan_restarts` is told when device connections are over, to restart discovery.
    pub fn new(
        jobs: Option<UnboundedSender<Job>>,
        scan_restarts: Option<UnboundedSender<()>>,
    ) -> Self {
        Self {
            devices: Devices::default(),
            metric: Metric::Temperature,
            scan_status: ScanStatus::Starting,
            warning: None,
            mode: Mode::Normal,
            store: None,
            jobs,
            scan_restarts,
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

    /// Devices included in calibration: the marked ones if at least two, else all.
    pub fn calibration_devices(&self) -> Vec<Address> {
        if self.marked.len() >= 2 {
            self.marked.iter().copied().collect()
        } else {
            self.devices.iter().map(|d| d.address).collect()
        }
    }

    pub fn on_scan_event(&mut self, event: ScanEvent) {
        match event {
            ScanEvent::Started { adapter } => self.scan_status = ScanStatus::Scanning { adapter },
            ScanEvent::Advert(advert) => {
                let (address, name) = (advert.address, advert.name.clone());
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
        let state = self.states.entry(address).or_default();
        match outcome {
            Outcome::Downloaded {
                samples,
                clock_offset,
            } => {
                state.clock_offset = Some(clock_offset);
                let added = self.merge_downloaded(address, &samples);
                format!("{added} of {} log records added", samples.len())
            }
            Outcome::ClockSynced {
                offset_before,
                samples,
            } => {
                state.clock_offset = Some(0.0);
                let added = self.merge_downloaded(address, &samples);
                format!(
                    "clock synced, was {}; {added} log records added",
                    format_offset(offset_before)
                )
            }
            Outcome::Renamed(name) => {
                self.devices.rename(address, name.clone());
                format!("renamed to {name}")
            }
            Outcome::Configured(config) => format!(
                "adv {} s, measure {} s, {}",
                config.advertising_interval().as_secs_f64(),
                config.measurement_period().as_secs_f64(),
                config.tx_power()
            ),
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
        if jobs.send(Job { address, action }).is_ok() {
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
            if idle && let Some(restarts) = &self.scan_restarts {
                let _ = restarts.send(());
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
        let devices = self.calibration_devices();
        let Mode::Calibration(screen) = &mut self.mode else {
            return;
        };
        let now = SystemTime::now();
        let since = calibrate::window_start(screen.hours(), now, &devices, &self.last_calibrations);
        screen.result =
            Some(calibrate::run(&self.devices, &devices, since, now).map_err(|e| e.to_string()));
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
            Mode::Calibration(screen) => self.on_calibration_key(key.code, screen),
        }
    }

    fn on_normal_key(&mut self, code: KeyCode) {
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
            KeyCode::Tab => {
                self.metric = match self.metric {
                    Metric::Temperature => Metric::Humidity,
                    Metric::Humidity => Metric::Temperature,
                };
            }
            KeyCode::Char('w') => self.window_index = (self.window_index + 1) % WINDOWS.len(),
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
            KeyCode::Char('e') => {
                let targets = self.targets();
                if targets.is_empty() {
                    self.warn_no_targets();
                } else {
                    self.mode = Mode::Settings(SettingsForm::new(targets));
                }
            }
            KeyCode::Char('c') => {
                self.mode = Mode::Calibration(CalibrationScreen {
                    hours_index: DEFAULT_CALIBRATION_HOURS,
                    result: None,
                    confirm: None,
                    computed_at: None,
                });
                self.refit();
            }
            _ => {}
        }
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

    fn on_calibration_key(&mut self, code: KeyCode, mut screen: CalibrationScreen) {
        if let Some(confirm) = screen.confirm.take() {
            if code == KeyCode::Char('y') {
                match confirm {
                    Confirm::Apply => self.apply_calibration(screen.result.as_ref()),
                    Confirm::Reset => {
                        for address in self.calibration_devices() {
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
            KeyCode::Left | KeyCode::Char('h') => {
                screen.hours_index = screen.hours_index.saturating_sub(1);
                screen.computed_at = None;
            }
            KeyCode::Right | KeyCode::Char('l') => {
                screen.hours_index = (screen.hours_index + 1).min(CALIBRATION_HOURS.len() - 1);
                screen.computed_at = None;
            }
            KeyCode::Enter => {
                if matches!(screen.result, Some(Ok(_))) {
                    screen.confirm = Some(Confirm::Apply);
                }
            }
            KeyCode::Char('x') => screen.confirm = Some(Confirm::Reset),
            KeyCode::Char('s') => {
                let devices = self.calibration_devices();
                self.sync_clocks(&devices);
            }
            _ => {}
        }
        self.mode = Mode::Calibration(screen);
        self.on_tick();
    }

    fn apply_calibration(&mut self, result: Option<&Result<calibrate::Run, String>>) {
        let Some(Ok(run)) = result else { return };
        let jobs: Vec<(Address, Correction, Correction)> = run
            .devices
            .iter()
            .zip(&run.report.sensors)
            .filter_map(|(&(address, _), sensor)| {
                let t = calibrate::correction(&sensor.temperature);
                let h = calibrate::correction(&sensor.humidity);
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
    fn offsets() {
        assert_eq!(format_offset(0.4), "ok");
        assert_eq!(format_offset(-37.0), "-37s");
        assert_eq!(format_offset(600.0), "+10m");
        assert_eq!(format_offset(-9000.0), "-2.5h");
        assert_eq!(format_offset(-1_793_040_361.0), "-20753d");
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

        // Esc clears the marks first, then the selection.
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.targets(), [A]);
        press(&mut app, KeyCode::Esc);
        assert!(app.targets().is_empty());
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
        assert!(drain(&mut rx).is_empty());
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
