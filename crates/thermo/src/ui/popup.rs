//! Dialogs drawn over the main screen.

use atc::DeviceId;
use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::app::App;
use crate::forms::{Field, SettingsForm};

pub fn rename(frame: &mut Frame, app: &App, address: DeviceId, input: &str) {
    let current = app
        .devices
        .get(address)
        .map_or_else(|| address.to_string(), |d| d.name.clone());
    let area = centered(frame.area(), 50, 6);
    let lines = vec![
        Line::from(format!("Rename {current} ({address})")),
        Line::from(""),
        Line::from(vec![
            Span::from("> "),
            Span::from(input.to_owned()).bold(),
            Span::from("▏").slow_blink(),
        ]),
        Line::from(format!(
            "{}/{} bytes · the device restarts to apply it",
            input.len(),
            atc::MAX_NAME_LEN
        ))
        .dark_gray(),
    ];
    draw_dialog(frame, area, " Rename ", lines);
}

pub fn settings(frame: &mut Frame, app: &App, form: &SettingsForm) {
    let names: Vec<String> = form
        .targets
        .iter()
        .map(|&a| {
            app.devices
                .get(a)
                .map_or_else(|| a.to_string(), |d| d.name.clone())
        })
        .collect();
    let mut lines = vec![
        Line::from(format!("Apply to: {}", names.join(", "))),
        Line::from(""),
    ];
    for (i, field) in Field::ALL.iter().enumerate() {
        let selected = i == form.field;
        let value = form.value_label(*field);
        let value = if selected {
            Span::from(format!("◀ {value} ▶")).bold().reversed()
        } else {
            Span::from(format!("  {value}  "))
        };
        lines.push(Line::from(vec![
            Span::from(format!("{:<22}", field.label())),
            value,
        ]));
    }
    lines.push(Line::from(""));
    let hint = match form.measurement_period() {
        Some(period) => format!("→ a measurement every {} s", period.as_secs_f64()),
        None => "\"keep\" leaves a device's current value unchanged".to_owned(),
    };
    lines.push(Line::from(hint).dark_gray());
    let height = u16::try_from(lines.len()).unwrap_or(u16::MAX) + 2;
    let area = centered(frame.area(), 64, height);
    draw_dialog(frame, area, " Device settings ", lines);
}

fn draw_dialog(frame: &mut Frame, area: Rect, title: &str, lines: Vec<Line<'_>>) {
    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .title(title)
        .border_style(Style::new().cyan());
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [row] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    let [cell] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(row);
    cell
}
