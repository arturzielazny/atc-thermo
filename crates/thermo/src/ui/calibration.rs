//! The calibration panel: proposed corrections for co-located devices.

use std::time::SystemTime;

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, Wrap};

use crate::app::{App, CalibrationScreen, Confirm};
use crate::calibrate;

pub fn render(frame: &mut Frame, area: Rect, app: &App, screen: &CalibrationScreen) {
    let devices = app.calibration_devices();
    let title = format!(
        " Calibration · last {} h · {} devices{} ",
        screen.hours(),
        devices.len(),
        if devices.len() < app.devices.len() {
            " (marked)"
        } else {
            ""
        }
    );
    let block = Block::bordered().title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let run = match &screen.result {
        None => {
            frame.render_widget(Paragraph::new("computing…").dark_gray(), inner);
            return;
        }
        Some(Err(err)) => {
            let text = vec![
                Line::from(format!("No result: {err}")).yellow(),
                Line::from(""),
                Line::from(
                    "Put the thermometers side by side, a few cm apart, away from heaters, sun and people.",
                ),
                Line::from(
                    "Offsets need a few undisturbed hours; gains also need a second, different condition",
                ),
                Line::from("(e.g. a few hours outside) to cover at least 5 °C and 15 %RH."),
                Line::from(
                    "Live data is used only after the last calibration change of these devices.",
                ),
            ];
            frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: true }), inner);
            return;
        }
        Some(Ok(run)) => run,
    };

    let [summary_area, table_area, footer_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(inner);
    frame.render_widget(Paragraph::new(summary(run)), summary_area);
    frame.render_widget(table(run), table_area);
    frame.render_widget(footer(run, screen), footer_area);
}

fn summary(run: &calibrate::Run) -> Vec<Line<'static>> {
    let r = &run.report;
    let since = SystemTime::now()
        .duration_since(run.since)
        .unwrap_or_default()
        .as_secs_f64()
        / 3600.0;
    vec![
        Line::from(format!(
            "Data from the last {since:.1} h · steady: {} min temperature, {} min humidity · {} phase(s)",
            r.temperature.steady.as_secs() / 60,
            r.humidity.steady.as_secs() / 60,
            r.phases.len()
        )),
        Line::from(format!(
            "Range covered: {:.1}–{:.1} °C, {:.0}–{:.0} %RH (gains need ≥ 5 °C / 15 %RH)",
            r.temperature.range.0, r.temperature.range.1, r.humidity.range.0, r.humidity.range.1
        )),
        Line::from(vec![
            Span::from("Disagreement between devices: "),
            Span::from(format!(
                "{:.3} → {:.3} °C",
                r.temperature.spread.before, r.temperature.spread.after
            ))
            .bold(),
            Span::from(", "),
            Span::from(format!(
                "{:.2} → {:.2} %RH",
                r.humidity.spread.before, r.humidity.spread.after
            ))
            .bold(),
        ]),
    ]
}

fn table(run: &calibrate::Run) -> Table<'static> {
    let header = Row::new([
        "Device", "T offset", "T gain", "T rms", "H offset", "H gain", "H rms", "Applies",
    ])
    .style(Style::new().bold());
    let rows: Vec<Row> = run
        .devices
        .iter()
        .zip(&run.report.sensors)
        .map(|((_, name), sensor)| {
            let (t, h) = (sensor.temperature, sensor.humidity);
            let gain = |fit: &calibration::Fit| {
                if fit.gain_fitted {
                    format!("{:.4}", fit.gain)
                } else {
                    "–".to_owned()
                }
            };
            let applies = match (calibrate::correction(&t), calibrate::correction(&h)) {
                (Some(_), Some(_)) => Cell::from("✓").green(),
                (None, None) => Cell::from(calibrate::notes(&t, &h)).yellow(),
                _ => Cell::from(format!("partly: {}", calibrate::notes(&t, &h))).yellow(),
            };
            Row::new([
                Cell::from(name.clone()),
                Cell::from(format!("{:+.3} °C", t.offset)),
                Cell::from(gain(&t)),
                Cell::from(format!("{:.3}", t.residual_rms)),
                Cell::from(format!("{:+.2} %", h.offset)),
                Cell::from(gain(&h)),
                Cell::from(format!("{:.2}", h.residual_rms)),
                applies,
            ])
        })
        .collect();
    Table::new(
        rows,
        [
            Constraint::Length(16),
            Constraint::Length(10),
            Constraint::Length(8),
            Constraint::Length(7),
            Constraint::Length(10),
            Constraint::Length(8),
            Constraint::Length(7),
            Constraint::Min(8),
        ],
    )
    .header(header)
    .flex(Flex::Start)
}

fn footer(run: &calibrate::Run, screen: &CalibrationScreen) -> Line<'static> {
    let applicable = run
        .report
        .sensors
        .iter()
        .filter(|s| {
            calibrate::correction(&s.temperature).is_some()
                || calibrate::correction(&s.humidity).is_some()
        })
        .count();
    match screen.confirm {
        Some(Confirm::Apply) => Line::from(format!(
            "Write corrected coefficients to {applicable} device(s)? y = yes, any other key = no"
        ))
        .red()
        .bold(),
        Some(Confirm::Reset) => Line::from(format!(
            "Restore factory calibration on {} device(s)? y = yes, any other key = no",
            run.devices.len()
        ))
        .red()
        .bold(),
        None => Line::from(format!(
            "Enter writes the corrections to {applicable} device(s); offsets add to what they report now."
        ))
        .dark_gray(),
    }
}
