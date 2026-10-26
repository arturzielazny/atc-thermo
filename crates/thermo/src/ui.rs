//! Screen layout: chart on top, device table below, status bar at the bottom.

mod chart;
mod plot;
mod table;

use std::path::{Path, PathBuf};

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui_image::picker::Picker;

use crate::app::{App, ScanStatus};

pub use chart::ChartView;

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
    pub fn new(picker: Option<Picker>) -> Self {
        Self {
            chart: ChartView::new(picker),
        }
    }
}

pub fn draw(frame: &mut Frame, app: &App, ui: &mut Ui) {
    let table_height = u16::try_from(app.devices.len().max(1))
        .unwrap_or(u16::MAX)
        .saturating_add(3);
    let [chart_area, table_area, status_area] = Layout::vertical([
        Constraint::Min(8),
        Constraint::Length(table_height),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    ui.chart.render(frame, chart_area, app);
    table::render(frame, table_area, app);
    frame.render_widget(status_line(app), status_area);
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
        spans.push(Span::from(format!("   history: {}", home_relative(dir))));
    }
    if let Some(warning) = &app.warning {
        spans.push(Span::from(format!("   ⚠ {warning}")).yellow());
    }
    spans.push(Span::from(
        "   t/h/Tab metric · w window · ↑↓ select · Esc deselect · q quit",
    ));
    Line::from(spans).style(Style::new().dark_gray())
}
