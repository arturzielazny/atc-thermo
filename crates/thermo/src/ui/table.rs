//! One row per device with its latest readings.

use std::time::{Duration, SystemTime};

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::Text;
use ratatui::widgets::{Block, Cell, Row, Table, TableState};

use super::{device_color, ratatui_color};
use crate::app::App;
use crate::devices::Device;

/// Rows of devices not heard from for this long are dimmed.
const STALE_AFTER: Duration = Duration::from_secs(120);
const MIN_NAME_WIDTH: usize = "Device".len();

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let now = SystemTime::now();
    let rows: Vec<Row> = app
        .devices
        .iter()
        .enumerate()
        .map(|(i, device)| row(device, device_color(i), now))
        .collect();
    let header = Row::new([
        "", "Device", "Address", "Temp", "Humidity", "Battery", "Voltage", "RSSI", "Seen",
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
        Constraint::Length(1),
        Constraint::Length(u16::try_from(name_width).unwrap_or(u16::MAX)),
        Constraint::Length(17),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(7),
        Constraint::Length(7),
        Constraint::Length(8),
        Constraint::Length(5),
    ];
    let title = format!(" Devices ({}) ", app.devices.len());
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

fn row(device: &Device, color: (u8, u8, u8), now: SystemTime) -> Row<'static> {
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
    let row = Row::new([
        Cell::from("●").style(Style::new().fg(ratatui_color(color))),
        Cell::from(device.name.clone()),
        Cell::from(device.address.to_string()),
        Cell::from(temperature),
        Cell::from(humidity),
        Cell::from(
            device
                .battery
                .map_or_else(|| "–".to_owned(), |b| format!("{b} %")),
        ),
        Cell::from(fmt(device.voltage, " V", 2)),
        Cell::from(
            device
                .rssi
                .map_or_else(|| "–".to_owned(), |r| format!("{r} dBm")),
        ),
        Cell::from(format_age(age)),
    ]);
    if age > STALE_AFTER {
        row.dark_gray()
    } else {
        row
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
}
