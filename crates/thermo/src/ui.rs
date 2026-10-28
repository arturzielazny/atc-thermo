//! Screen layout: chart (or calibration panel) on top, device table below, status at the bottom.

mod calibration;
mod chart;
mod kitty;
mod plot;
mod popup;
mod table;

use std::path::{Path, PathBuf};

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui_image::picker::Picker;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::app::{App, Mode, ScanStatus};

pub use chart::{ChartUpdate, ChartView};

/// Line colors, assigned to devices in table order.
const PALETTE: [(u8, u8, u8); 8] = [
    (0x4e, 0x9a, 0xe0),
    (0xf2, 0x8e, 0x2b),
    (0x59, 0xc1, 0x4f),
    (0xe1, 0x57, 0x59),
    (0xb0, 0x7a, 0xd1),
    (0xed, 0xc9, 0x48),
    (0x76, 0xd7, 0xd2),
    (0xff, 0x9d, 0xa7),
];

fn device_color(index: usize) -> (u8, u8, u8) {
    PALETTE[index % PALETTE.len()]
}

fn ratatui_color((r, g, b): (u8, u8, u8)) -> Color {
    Color::Rgb(r, g, b)
}

/// View state that outlives a single frame.
pub struct Ui {
    chart: ChartView,
}

impl Ui {
    /// `picker` is the terminal's graphics capability; `None` draws the chart with text.
    /// Chart images finish in the background and arrive on the returned receiver.
    pub fn new(picker: Option<Picker>) -> (Self, UnboundedReceiver<ChartUpdate>) {
        let (chart, updates) = ChartView::new(picker);
        (Self { chart }, updates)
    }

    /// Takes a finished chart image; returns bytes to write to the terminal.
    pub fn accept_chart(&mut self, update: ChartUpdate) -> Option<String> {
        self.chart.accept(update)
    }

    /// Bytes to write to the terminal before exiting.
    pub fn farewell(&self) -> Option<String> {
        self.chart.farewell()
    }
}

/// Rows the chart needs under the calibration panel before the device table may
/// take space from it: borders, time labels and a readable plot.
const MIN_CALIBRATION_CHART: u16 = 12;

pub fn draw(frame: &mut Frame, app: &App, ui: &mut Ui) {
    let table_height = u16::try_from(app.devices.len().max(1))
        .unwrap_or(u16::MAX)
        .saturating_add(3);
    let [main_area, status_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).areas(frame.area());

    if let Mode::Calibration(screen) = &app.mode {
        // Panel, then the chart with the window marked; the device table only if
        // the chart still gets enough rows.
        let panel_height = calibration::height(screen).min(main_area.height);
        let show_table = main_area.height >= panel_height + MIN_CALIBRATION_CHART + table_height;
        let [panel_area, chart_area, table_area] = Layout::vertical([
            Constraint::Length(panel_height),
            Constraint::Min(0),
            Constraint::Length(if show_table { table_height } else { 0 }),
        ])
        .areas(main_area);
        calibration::render(frame, panel_area, app, screen);
        if chart_area.height >= MIN_CALIBRATION_CHART / 2 {
            let cut = chart::Cut {
                start: screen.start,
                end: screen.end,
            };
            ui.chart.render(frame, chart_area, app, Some(cut));
        }
        if show_table {
            table::render(frame, table_area, app);
        }
    } else {
        let [top_area, table_area] =
            Layout::vertical([Constraint::Min(8), Constraint::Length(table_height)])
                .areas(main_area);
        ui.chart.render(frame, top_area, app, None);
        table::render(frame, table_area, app);
    }
    let [status, keys] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(status_area);
    frame.render_widget(status_line(app), status);
    frame.render_widget(key_help(app), keys);

    match &app.mode {
        Mode::Rename { address, input } => popup::rename(frame, app, *address, input),
        Mode::Settings(form) => popup::settings(frame, app, form),
        Mode::Normal | Mode::Calibration(_) => {}
    }
}

/// Shows paths under `$HOME` as `~/...`.
fn home_relative(path: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match home
        .as_deref()
        .and_then(|home| path.strip_prefix(home).ok())
    {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

fn status_line(app: &App) -> Line<'_> {
    let mut spans = match &app.scan_status {
        ScanStatus::Starting => vec![Span::from(" ◌ starting scan").yellow()],
        ScanStatus::Scanning { adapter } => {
            vec![Span::from(format!(" ● scanning on {adapter}")).green()]
        }
        ScanStatus::Failed(reason) => vec![Span::from(format!(" ✖ scan failed: {reason}")).red()],
    };
    if let Some(dir) = app.store_dir() {
        spans.push(Span::from(format!("   history: {}", home_relative(dir))).dark_gray());
    }
    if let Some(warning) = &app.warning {
        spans.push(Span::from(format!("   ⚠ {warning}")).yellow());
    }
    Line::from(spans)
}

fn key_help(app: &App) -> Line<'static> {
    let help = match &app.mode {
        Mode::Normal => {
            " ↑↓ select · space mark · a all · i identify · n rename · s sync clock · e settings · d download log · c calibrate · t/h/r metric · w window · p points · q quit"
        }
        Mode::Rename { .. } => " type a name · Enter save (empty = default) · Esc cancel",
        Mode::Settings(_) => " ↑↓ field · ←→ value · Enter apply · Esc cancel",
        Mode::Calibration(_) => {
            " Tab start/end · ←→ move 15 min (Shift: 1 h) · Enter apply · s sync clocks · x factory reset · mark devices (space/a) first to pick a subset · Esc back"
        }
    };
    Line::from(help).style(Style::new().dark_gray())
}
