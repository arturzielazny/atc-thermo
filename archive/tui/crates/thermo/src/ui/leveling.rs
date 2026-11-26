//! The signal levelling panel: each device's median signal, settings and TX power change.

use atc::{Firmware, TxPower};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Row, Table, TableState};

use super::{device_color, ratatui_color};
use crate::app::{App, Confirm};
use crate::devices::RSSI_FINE;
use crate::level::{self, LevelingScreen};

/// A planned TX power this far from the wanted one means the radio's range ends there.
const CLAMPED_DB: f32 = 1.0;

/// Rows the panel needs: borders, target line, a gap, table header and rows, footer.
pub fn height(app: &App) -> u16 {
    u16::try_from(2 + 2 + 1 + app.devices.len() + 1).unwrap_or(u16::MAX)
}

pub fn render(frame: &mut Frame, area: Rect, app: &App, screen: &LevelingScreen) {
    let leveled = app.leveled_devices();
    let title = if leveled.len() < app.devices.len() {
        format!(" Signal levelling · {} marked devices ", leveled.len())
    } else {
        format!(" Signal levelling · all {} devices ", leveled.len())
    };
    let block = Block::bordered().title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [target_area, _, table_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    frame.render_widget(Paragraph::new(target_line(screen)), target_area);

    let all: Vec<_> = app.devices.iter().map(|d| d.address).collect();
    let plan = screen.plan(&all);
    let header = Row::new([
        "", "Device", "Adverts", "Median", "TX power", "Adv", "Change",
    ])
    .style(Style::new().bold());
    let rows: Vec<Row> = plan
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let name = app
                .devices
                .get(row.address)
                .map_or_else(|| row.address.to_string(), |d| d.name.clone());
            let mark = if app.is_marked(row.address) {
                "✓"
            } else {
                " "
            };
            let state = app.state(row.address);
            let config = state.and_then(|s| s.config);
            let median = row
                .median
                .map_or_else(|| "–".to_owned(), |m| format!("{m} dBm"));
            let power = config.map_or_else(|| "?".to_owned(), |c| c.tx_power().to_string());
            let adv = config.map_or_else(
                || "?".to_owned(),
                |c| format!("{} s", c.advertising_interval().as_secs_f64()),
            );
            let change = match (state.and_then(|s| s.busy), row.median, row.change) {
                _ if !leveled.contains(&row.address) => Span::from("not marked").dark_gray(),
                (Some(label), _, _) => Span::from(label).yellow(),
                (None, None, _) => Span::from("listening…").dark_gray(),
                (None, Some(_), None) => Span::from("ok").green(),
                (None, Some(_), Some(change)) => {
                    let planned = config
                        .map(|c| planned_power(c.tx_power(), change))
                        .unwrap_or_default();
                    Span::from(format!("{change:+} dB{planned}")).bold()
                }
            };
            Row::new(vec![
                Line::from(vec![
                    Span::from(mark).bold(),
                    Span::from("●").fg(ratatui_color(device_color(i))),
                ]),
                name.into(),
                row.adverts.to_string().into(),
                median.into(),
                power.into(),
                adv.into(),
                change.into(),
            ])
        })
        .collect();
    let name_width = app
        .devices
        .iter()
        .map(|d| d.name.chars().count())
        .chain(["Device".len()])
        .max()
        .unwrap_or(0);
    let widths = [
        Constraint::Length(2),
        Constraint::Length(u16::try_from(name_width).unwrap_or(u16::MAX)),
        Constraint::Length(7),
        Constraint::Length(8),
        Constraint::Length(10),
        Constraint::Length(6),
        Constraint::Min(15),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut state = TableState::default().with_selected(app.selected_index());
    frame.render_stateful_widget(table, table_area, &mut state);

    frame.render_widget(footer(app, screen), footer_area);
}

/// `Listening for 3:12 · target -70 dBm`, explaining the default target.
fn target_line(screen: &LevelingScreen) -> Line<'static> {
    let elapsed = screen.started.elapsed().as_secs();
    Line::from(vec![
        Span::from(format!(
            "Listening for {}:{:02} · target ",
            elapsed / 60,
            elapsed % 60
        )),
        Span::from(format!(" {} dBm ", screen.target)).reversed(),
        Span::from(if screen.target == level::DEFAULT_TARGET {
            format!(
                " median, {} dB above where the signal stops counting as reliable",
                level::DEFAULT_TARGET - RSSI_FINE
            )
        } else {
            String::new()
        })
        .dark_gray(),
    ])
}

/// ` → -9.9 dBm`, with `(limit)` if the radio cannot go as far as `change` asks;
/// empty if the current power is not a documented level.
fn planned_power(power: TxPower, change: i16) -> String {
    let Some(dbm) = power.dbm() else {
        return String::new();
    };
    let wanted = dbm + f32::from(change);
    let planned = TxPower::nearest(power.firmware(), wanted);
    let limit = planned
        .dbm()
        .is_some_and(|p| (p - wanted).abs() > CLAMPED_DB);
    format!(" → {planned}{}", if limit { " (limit)" } else { "" })
}

/// The pending confirmation, or what the numbers mean.
fn footer(app: &App, screen: &LevelingScreen) -> Line<'static> {
    let ask = |what: String| {
        Line::from(format!(
            "{what}? Each is a flash write. y to confirm, any other key cancels"
        ))
        .yellow()
        .bold()
    };
    match screen.confirm {
        Some(Confirm::Apply) => ask(format!(
            "Change the TX power of {} devices",
            app.leveling_changes(screen).len()
        )),
        Some(Confirm::Reset) => {
            let resets = app.factory_resets();
            let families = resets.iter().map(|&address| {
                app.state(address)
                    .and_then(|s| s.config)
                    .map(|c| c.firmware())
            });
            ask(format!(
                "Restore the factory TX power, {}, on {} devices",
                factory_power(families),
                resets.len()
            ))
        }
        None => Line::from(format!(
            "A median needs {} adverts; changes under {} dB are skipped. TX power goes from −25 to +10 dBm (THB2: −20 to +5 dBm).",
            level::MIN_ADVERTS,
            level::TOLERANCE_DB
        ))
        .dark_gray(),
    }
}

/// Each family's factory TX power, `+0.0 dBm` or `ATC +0.0 dBm, THB +0.0 dBm`, for
/// devices of the given firmware (`None` where it isn't known yet, so it could be
/// either).
fn factory_power(families: impl IntoIterator<Item = Option<Firmware>>) -> String {
    let mut known = Vec::new();
    let mut unknown = false;
    for family in families {
        match family {
            Some(family) if !known.contains(&family) => known.push(family),
            Some(_) => {}
            None => unknown = true,
        }
    }
    if unknown {
        known = vec![Firmware::Atc, Firmware::Thb];
    }
    known.sort();
    match known.as_slice() {
        [family] => TxPower::factory(*family).to_string(),
        families => families
            .iter()
            .map(|&family| format!("{family} {}", TxPower::factory(family)))
            .collect::<Vec<_>>()
            .join(", "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_factory_power_is_named_per_family() {
        let atc = TxPower::factory(Firmware::Atc).to_string();
        let thb = TxPower::factory(Firmware::Thb).to_string();
        let both = format!("ATC {atc}, THB {thb}");
        assert_eq!(factory_power([Some(Firmware::Atc)]), atc);
        assert_eq!(
            factory_power([Some(Firmware::Thb), Some(Firmware::Thb)]),
            thb
        );
        assert_eq!(
            factory_power([Some(Firmware::Thb), Some(Firmware::Atc)]),
            both
        );
        // A device whose settings weren't read yet could be either.
        assert_eq!(factory_power([Some(Firmware::Thb), None]), both);
        assert_eq!(factory_power([None]), both);
    }
}
