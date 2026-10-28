//! The calibration panel: proposed corrections for co-located devices.

use std::time::SystemTime;

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, Wrap};

use crate::app::{App, CalibrationScreen, Confirm, WindowEdge};
use crate::{calibrate, localtime};

pub fn render(frame: &mut Frame, area: Rect, app: &App, screen: &CalibrationScreen) {
    let devices = app.calibration_devices();
    let title = format!(
        " Calibration · {} devices{} ",
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
    let [window_area, _, inner] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(inner);
    frame.render_widget(window_line(app, screen), window_area);

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

/// `Window  [13:15] → [now]  5.8 h`, with the edge that `←`/`→` move highlighted.
fn window_line(app: &App, screen: &CalibrationScreen) -> Line<'static> {
    let now = SystemTime::now();
    let edge = |text: String, edge: WindowEdge| {
        let span = Span::from(format!(" {text} "));
        if screen.editing == edge {
            span.reversed()
        } else {
            span.bold()
        }
    };
    let end = screen.end.unwrap_or(now);
    let hours = end
        .duration_since(screen.start)
        .unwrap_or_default()
        .as_secs_f64()
        / 3600.0;
    let mut spans = vec![
        Span::from("Window "),
        edge(
            localtime::format_moment(screen.start, now),
            WindowEdge::Start,
        ),
        Span::from(" → "),
        edge(
            screen.end.map_or_else(
                || "now".to_owned(),
                |end| localtime::format_moment(end, now),
            ),
            WindowEdge::End,
        ),
        Span::from(format!("  {hours:.1} h")),
    ];
    let earliest = app.earliest_calibration_start();
    if earliest > SystemTime::UNIX_EPOCH && screen.start <= earliest {
        spans.push(
            Span::from("  · starts at the last calibration, the earliest usable data").dark_gray(),
        );
    }
    Line::from(spans)
}

fn summary(run: &calibrate::Run) -> Vec<Line<'static>> {
    let r = &run.report;
    vec![
        Line::from(format!(
            "Steady data: {} min temperature, {} min humidity · {} phase(s)",
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
        "Device", "T offset", "T gain", "T rms", "Lag", "H offset", "H gain", "H rms", "Applies",
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
            let applies = match calibrate::corrections(sensor) {
                (Some(_), Some(_)) => Cell::from("✓").green(),
                (None, None) => Cell::from(calibrate::notes(&t, &h)).yellow(),
                _ => Cell::from(format!("partly: {}", calibrate::notes(&t, &h))).yellow(),
            };
            Row::new([
                Cell::from(name.clone()),
                Cell::from(format!("{:+.3} °C", t.offset)),
                Cell::from(gain(&t)),
                Cell::from(format!("{:.3}", t.residual_rms)),
                Cell::from(calibrate::format_lag(t.lag)),
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
            let (t, h) = calibrate::corrections(s);
            t.is_some() || h.is_some()
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
