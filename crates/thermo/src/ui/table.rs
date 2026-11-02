//! One row per device with its latest readings.

use std::time::{Duration, SystemTime};

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Cell, Row, Table, TableState};

use super::{device_color, ratatui_color};
use crate::app::{App, format_offset};
use crate::devices::{Device, RSSI_FINE, RSSI_MARGINAL};

/// Rows of devices not heard from for this long are dimmed.
const STALE_AFTER: Duration = Duration::from_secs(120);
/// Battery percentages from which a level counts as fine, then as getting low.
const BATTERY_FINE: u8 = 50;
const BATTERY_LOW: u8 = 20;
const MIN_NAME_WIDTH: usize = "Device".len();

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let now = SystemTime::now();
    let rows: Vec<Row> = app
        .devices
        .iter()
        .enumerate()
        .map(|(i, device)| row(app, i + 1, device, device_color(i), now))
        .collect();
    let header = Row::new([
        "", "#", "Device", "Address", "Temp", "Humidity", "Battery", "Voltage", "RSSI", "RSSI 5m",
        "Seen", "Clock", "TX power", "Adv", "Measure", "Log", "Status",
    ])
    .style(Style::new().bold());
    let name_width = app
        .devices
        .iter()
        .map(|d| d.name.chars().count())
        .max()
        .unwrap_or(0)
        .max(MIN_NAME_WIDTH);
    let widths = [
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(u16::try_from(name_width).unwrap_or(u16::MAX)),
        Constraint::Length(17),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(7),
        Constraint::Length(7),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(5),
        Constraint::Length(7),
        Constraint::Length(9),
        Constraint::Length(6),
        Constraint::Length(7),
        Constraint::Length(6),
        Constraint::Min(10),
    ];
    let marked = app
        .devices
        .iter()
        .filter(|d| app.is_marked(d.address))
        .count();
    let title = if marked > 0 {
        format!(" Devices ({}, {marked} marked) ", app.devices.len())
    } else {
        format!(" Devices ({}) ", app.devices.len())
    };
    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::bordered().title(title))
        .flex(Flex::Start)
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED));

    if app.devices.is_empty() {
        frame.render_widget(table, area);
        let hint = Text::from("  Listening for BTHome adverts…").dark_gray();
        let inner = Block::bordered().inner(area);
        frame.render_widget(
            hint,
            Rect {
                y: inner.y + 1,
                height: 1,
                ..inner
            },
        );
        return;
    }
    let mut state = TableState::default().with_selected(app.selected_index());
    frame.render_stateful_widget(table, area, &mut state);
}

/// `number` is what `i` shows on the device's screen.
fn row(
    app: &App,
    number: usize,
    device: &Device,
    color: (u8, u8, u8),
    now: SystemTime,
) -> Row<'static> {
    let fmt = |value: Option<f32>, unit: &str, decimals: usize| {
        value.map_or_else(|| "–".to_owned(), |v| format!("{v:.decimals$}{unit}"))
    };
    let (temperature, humidity) = if device.encrypted {
        ("locked".to_owned(), "locked".to_owned())
    } else {
        (
            fmt(device.temperature, " °C", 2),
            fmt(device.humidity, " %", 1),
        )
    };
    let age = now.duration_since(device.last_seen).unwrap_or_default();
    let mark = if app.is_marked(device.address) {
        "✓"
    } else {
        " "
    };
    let is_stale = age > STALE_AFTER;
    // A stale row is dimmed as a whole; colors would suggest current values.
    let level_cell = |text: String, level: Option<Level>| match level {
        Some(level) if !is_stale => Cell::from(text).style(level.style()),
        _ => Cell::from(text),
    };
    let rssi_cell = |rssi: Option<i16>| {
        level_cell(
            rssi.map_or_else(|| "–".to_owned(), |r| format!("{r} dBm")),
            rssi.map(rssi_level),
        )
    };
    let state = app.state(device.address);
    let clock = state
        .and_then(|s| s.clock_offset)
        .map_or_else(|| "?".to_owned(), format_offset);
    // Settings are known once read or written this session.
    let setting = |show: fn(&atc::Config) -> String| {
        state
            .and_then(|s| s.config)
            .map_or_else(|| "–".to_owned(), |c| show(&c))
    };
    let row = Row::new([
        Cell::from(Line::from(vec![
            Span::from(mark).bold(),
            Span::from("●").fg(ratatui_color(color)),
        ])),
        Cell::from(format!("{number:>2}")),
        Cell::from(device.name.clone()),
        Cell::from(device.address.to_string()),
        Cell::from(temperature),
        Cell::from(humidity),
        level_cell(
            device
                .battery
                .map_or_else(|| "–".to_owned(), |b| format!("{b} %")),
            device.battery.map(battery_level),
        ),
        Cell::from(fmt(device.voltage, " V", 2)),
        rssi_cell(device.rssi),
        rssi_cell(device.typical_rssi()),
        Cell::from(format_age(age)),
        Cell::from(clock),
        Cell::from(setting(|c| c.tx_power().to_string())),
        Cell::from(setting(|c| format_interval(c.advertising_interval()))),
        Cell::from(setting(|c| format_interval(c.measurement_period()))),
        Cell::from(setting(|c| {
            c.log_period()
                .map_or_else(|| "off".to_owned(), format_interval)
        })),
        status_cell(state),
    ]);
    if is_stale { row.dark_gray() } else { row }
}

/// How a battery or signal value rates, shown as green, yellow or red.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Fine,
    Marginal,
    Bad,
}

impl Level {
    fn style(self) -> Style {
        match self {
            Self::Fine => Style::new().green(),
            Self::Marginal => Style::new().yellow(),
            Self::Bad => Style::new().red(),
        }
    }
}

fn battery_level(percent: u8) -> Level {
    match percent {
        p if p >= BATTERY_FINE => Level::Fine,
        p if p >= BATTERY_LOW => Level::Marginal,
        _ => Level::Bad,
    }
}

/// Below about -85 dBm adverts get lost and connections become unreliable.
fn rssi_level(dbm: i16) -> Level {
    match dbm {
        r if r >= RSSI_FINE => Level::Fine,
        r if r >= RSSI_MARGINAL => Level::Marginal,
        _ => Level::Bad,
    }
}

fn status_cell(state: Option<&crate::app::DeviceState>) -> Cell<'static> {
    let Some(state) = state else {
        return Cell::from("");
    };
    let waiting = state
        .queued
        .saturating_sub(usize::from(state.busy.is_some()));
    match (&state.busy, &state.last) {
        (Some(label), _) if waiting > 0 => {
            Cell::from(format!("⟳ {label}… (+{waiting} queued)")).yellow()
        }
        (Some(label), _) => Cell::from(format!("⟳ {label}…")).yellow(),
        (None, _) if waiting > 0 => Cell::from(format!("… {waiting} queued")).yellow(),
        (None, Some(Ok(message))) => Cell::from(format!("✓ {message}")).green(),
        (None, Some(Err(message))) => Cell::from(format!("✗ {message}")).red(),
        (None, None) => Cell::from(""),
    }
}

/// `500 ms`, `2.5 s`, `10 s`, `30 min`.
fn format_interval(interval: Duration) -> String {
    let secs = interval.as_secs_f64();
    if secs < 1.0 {
        format!("{} ms", interval.as_millis())
    } else if secs < 60.0 {
        format!("{} s", (secs * 10.0).round() / 10.0)
    } else {
        format!("{} min", (secs / 6.0).round() / 10.0)
    }
}

fn format_age(age: Duration) -> String {
    let secs = age.as_secs();
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        _ => format!("{}h", secs / 3600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages() {
        assert_eq!(format_age(Duration::from_secs(5)), "5s");
        assert_eq!(format_age(Duration::from_secs(150)), "2m");
        assert_eq!(format_age(Duration::from_secs(7300)), "2h");
    }

    #[test]
    fn intervals() {
        assert_eq!(format_interval(Duration::from_micros(62_500)), "62 ms");
        assert_eq!(format_interval(Duration::from_millis(2500)), "2.5 s");
        assert_eq!(format_interval(Duration::from_secs(10)), "10 s");
        assert_eq!(format_interval(Duration::from_secs(1800)), "30 min");
        assert_eq!(format_interval(Duration::from_secs(150)), "2.5 min");
    }

    #[test]
    fn levels() {
        assert_eq!(battery_level(100), Level::Fine);
        assert_eq!(battery_level(50), Level::Fine);
        assert_eq!(battery_level(49), Level::Marginal);
        assert_eq!(battery_level(19), Level::Bad);
        assert_eq!(rssi_level(-45), Level::Fine);
        assert_eq!(rssi_level(-75), Level::Fine);
        assert_eq!(rssi_level(-80), Level::Marginal);
        assert_eq!(rssi_level(-86), Level::Bad);
    }
}
