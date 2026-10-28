//! The calibration panel: proposed corrections for co-located devices.

use std::time::SystemTime;

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, Wrap};

use crate::app::{App, CalibrationScreen, Confirm, WindowEdge};
use crate::{calibrate, localtime};

/// Cells on each side of a correction bar's zero line.
const BAR_HALF_WIDTH: usize = 6;

/// Both sides and the zero line.
#[expect(clippy::cast_possible_truncation, reason = "a dozen cells")]
const BAR_WIDTH: u16 = (2 * BAR_HALF_WIDTH + 1) as u16;

/// How a correction bar is scaled and colored, in the quantity's unit.
struct BarScale {
    /// Correction that fills one side.
    full: f64,
    /// Up to this it is green, then yellow up to `large`, red beyond: about half the
    /// sensor's rated accuracy and the accuracy itself (`SHT4x`: ±0.2 °C, ±1.8 %RH).
    small: f64,
    large: f64,
}

const TEMPERATURE_BAR: BarScale = BarScale {
    full: 0.3,
    small: 0.1,
    large: 0.2,
};
const HUMIDITY_BAR: BarScale = BarScale {
    full: 3.0,
    small: 1.0,
    large: 2.0,
};

/// Explanation shown while there is no result.
const NO_RESULT_HINT: [&str; 4] = [
    "Put the thermometers side by side, a few cm apart, away from heaters, sun and people.",
    "Offsets need a few undisturbed hours; gains also need a second, different condition",
    "(e.g. a few hours outside) to cover at least 5 °C and 15 %RH.",
    "Live data is used only after the last calibration change of these devices.",
];

/// Rows the panel needs: borders, window line and a gap, then the result.
pub fn height(screen: &CalibrationScreen) -> u16 {
    let body = match &screen.result {
        None => 1,
        // The error, a gap and the hint, plus a row in case a line wraps.
        Some(Err(_)) => 2 + NO_RESULT_HINT.len() + 1,
        // Summary, table header and rows, footer.
        Some(Ok(run)) => 3 + 1 + run.devices.len() + 1,
    };
    u16::try_from(4 + body).unwrap_or(u16::MAX)
}

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
            let mut text = vec![
                Line::from(format!("No result: {err}")).yellow(),
                Line::from(""),
            ];
            text.extend(NO_RESULT_HINT.into_iter().map(Line::from));
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
        "Device", "T offset", "", "T gain", "T rms", "Lag", "H offset", "", "H gain", "H rms",
        "Applies",
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
            let (t_written, h_written) = calibrate::corrections(sensor);
            let mid = |range: (f64, f64)| f64::midpoint(range.0, range.1);
            let t_bar = bar(
                net_correction(&t, mid(run.report.temperature.range)),
                &TEMPERATURE_BAR,
                t_written.is_some(),
            );
            let h_bar = bar(
                net_correction(&h, mid(run.report.humidity.range)),
                &HUMIDITY_BAR,
                h_written.is_some(),
            );
            let applies = match (t_written, h_written) {
                (Some(_), Some(_)) => Cell::from("✓").green(),
                (None, None) => Cell::from(calibrate::notes(&t, &h)).yellow(),
                _ => Cell::from(format!("partly: {}", calibrate::notes(&t, &h))).yellow(),
            };
            Row::new([
                Cell::from(name.clone()),
                Cell::from(format!("{:+.3} °C", t.offset)),
                Cell::from(t_bar),
                Cell::from(gain(&t)),
                Cell::from(format!("{:.3}", t.residual_rms)),
                Cell::from(calibrate::format_lag(t.lag)),
                Cell::from(format!("{:+.2} %", h.offset)),
                Cell::from(h_bar),
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
            Constraint::Length(BAR_WIDTH),
            Constraint::Length(8),
            Constraint::Length(7),
            Constraint::Length(7),
            Constraint::Length(10),
            Constraint::Length(BAR_WIDTH),
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

/// What a fit adds at `at`, typically the middle of the covered range: with a gain
/// the offset alone can mislead (gain 1.01 and offset -0.22 nearly cancel at 22 °C).
fn net_correction(fit: &calibration::Fit, at: f64) -> f64 {
    fit.apply(at) - at
}

/// `value` as a bar left (negative) or right of a zero line, in half cells, with an
/// arrow when it does not fit. Grey if the correction will not be written.
fn bar(value: f64, scale: &BarScale, written: bool) -> Line<'static> {
    let half_cells = 2 * BAR_HALF_WIDTH;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to a dozen half cells"
    )]
    let steps = ((value.abs() / scale.full) * half_cells as f64)
        .round()
        .min(half_cells as f64) as usize;
    let clipped = value.abs() > scale.full;
    let (full, half) = (steps / 2, steps % 2 == 1);
    let pad = " ".repeat(BAR_HALF_WIDTH - full - usize::from(half));
    let blocks = "█".repeat(full);
    let (outer_arrow, outer_half) = if value < 0.0 {
        ('◀', '▐')
    } else {
        ('▶', '▌')
    };
    let mut body = String::new();
    if value < 0.0 {
        body.push_str(&pad);
        if half {
            body.push(outer_half);
        }
        body.push_str(&blocks);
    } else {
        body.push_str(&blocks);
        if half {
            body.push(outer_half);
        }
        body.push_str(&pad);
    }
    if clipped {
        // The outermost cell becomes an arrow pointing past the scale.
        let mut cells: Vec<char> = body.chars().collect();
        let outer = if value < 0.0 { 0 } else { cells.len() - 1 };
        cells[outer] = outer_arrow;
        body = cells.into_iter().collect();
    }
    let style = if !written {
        Style::new().dark_gray()
    } else if value.abs() <= scale.small {
        Style::new().green()
    } else if value.abs() <= scale.large {
        Style::new().yellow()
    } else {
        Style::new().red()
    };
    let empty = " ".repeat(BAR_HALF_WIDTH);
    let zero = Span::from("│").dark_gray();
    if value < 0.0 {
        Line::from(vec![Span::styled(body, style), zero, Span::from(empty)])
    } else {
        Line::from(vec![Span::from(empty), zero, Span::styled(body, style)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn bars_grow_from_zero_in_half_cells() {
        // 0.075 °C of 0.3 is 3 half cells: one full block and a half.
        assert_eq!(text(&bar(0.075, &TEMPERATURE_BAR, true)), "      │█▌    ");
        assert_eq!(text(&bar(-0.075, &TEMPERATURE_BAR, true)), "    ▐█│      ");
        assert_eq!(text(&bar(0.0, &TEMPERATURE_BAR, true)), "      │      ");
        // Past full scale the outermost cell is an arrow.
        assert_eq!(text(&bar(6.55, &HUMIDITY_BAR, true)), "      │█████▶");
        assert_eq!(text(&bar(-0.6, &TEMPERATURE_BAR, true)), "◀█████│      ");
        for value in [0.01, -0.29, 0.31, -2.0] {
            let width = text(&bar(value, &TEMPERATURE_BAR, true)).chars().count();
            assert_eq!(width, usize::from(BAR_WIDTH), "{value}");
        }
    }
}
