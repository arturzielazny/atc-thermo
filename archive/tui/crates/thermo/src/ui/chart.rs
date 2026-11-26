//! History chart: a pixel image when the terminal has a graphics protocol, braille otherwise.

use std::time::{Duration, Instant, SystemTime};

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Block, Chart, Dataset, GraphType, Paragraph};
use ratatui_image::picker::Picker;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use super::{device_color, kitty, plot, ratatui_color};
use crate::app::{App, ChartStyle, Metric};
use crate::devices::{Sample, Source};
use crate::localtime;

/// Kitty image id for the chart; reusing it makes the terminal replace the old frame.
/// Below 2²⁴ so it fits in a placeholder's foreground color.
const CHART_IMAGE_ID: u32 = 0x3a_51c4;
/// How often the image is redrawn for new data and the scrolling time axis.
const REFRESH: Duration = Duration::from_secs(5);
/// Columns reserved left of the plot for y-axis labels.
const Y_LABEL_WIDTH: u16 = 7;
/// Values between horizontal gridlines, finest first: hundredths for sensors that
/// agree closely, tenths, halves and whole units, then coarser ones for wide ranges
/// such as humidity outdoors.
const Y_STEPS: [f64; 10] = [0.02, 0.05, 0.1, 0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 50.0];
/// Fewest rows from one horizontal gridline to the next, so their labels don't touch.
const MIN_GRID_ROWS: f64 = 2.0;
/// Times between vertical gridlines in minutes, finest first; they fall on round
/// local times such as 14:30.
const X_STEPS_MINUTES: [u32; 11] = [1, 5, 10, 15, 30, 60, 120, 180, 360, 720, 1440];
/// Fewest columns from one vertical gridline to the next, room for `14:30` and a gap.
const MIN_GRID_COLS: f64 = 14.0;

/// Live samples further apart than this are not joined by a line.
const LIVE_GAP: Duration = Duration::from_secs(5 * 60);
/// The same for stretches filled from device logs, which hold one averaged record per
/// log period; the longest period the settings offer is an hour.
const LOG_GAP: Duration = Duration::from_secs(65 * 60);

/// Chart spans offered while showing a calibration window: the shortest that
/// covers it with a little room before its start is used.
const CUT_SPANS: [Duration; 11] = [
    Duration::from_secs(3600),
    Duration::from_secs(2 * 3600),
    Duration::from_secs(3 * 3600),
    Duration::from_secs(4 * 3600),
    Duration::from_secs(6 * 3600),
    Duration::from_secs(8 * 3600),
    Duration::from_secs(12 * 3600),
    Duration::from_secs(18 * 3600),
    Duration::from_secs(24 * 3600),
    Duration::from_secs(36 * 3600),
    Duration::from_secs(48 * 3600),
];

/// The data a calibration uses, drawn over the chart; `end: None` is now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cut {
    pub start: SystemTime,
    pub end: Option<SystemTime>,
}

impl Cut {
    /// The chart span that shows the whole cut with some time before it.
    fn span(self, now: SystemTime) -> Duration {
        let needed = now
            .duration_since(self.start)
            .unwrap_or_default()
            .mul_f64(1.1);
        CUT_SPANS
            .into_iter()
            .find(|&span| span >= needed)
            .unwrap_or(CUT_SPANS[CUT_SPANS.len() - 1])
    }

    /// The kept range in chart x values (seconds relative to `now`).
    fn x_range(self, now: SystemTime) -> (f64, f64) {
        let ago = |time: SystemTime| -now.duration_since(time).unwrap_or_default().as_secs_f64();
        (ago(self.start), self.end.map_or(0.0, ago))
    }
}

/// One device's line on the chart.
#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    pub color: (u8, u8, u8),
    /// Oldest first.
    pub points: Vec<Point>,
    pub emphasis: Emphasis,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    /// Seconds relative to now, ≤ 0.
    pub x: f64,
    pub y: f64,
    /// Whether a line connects this point to the previous one.
    pub joined: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Emphasis {
    Dimmed,
    Normal,
    Selected,
}

/// Value ranges mapped onto a grid of text cells.
///
/// The first and last row/column *centers* sit at the range ends, so gridlines drawn
/// at cell centers line up exactly with labels printed in those cells.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Axes {
    pub x: (f64, f64),
    pub y: (f64, f64),
    pub cols: u16,
    pub rows: u16,
    /// Value between horizontal gridlines; `None` spreads them evenly instead.
    pub y_step: Option<f64>,
    /// Local clock time at x = 0 (now), in seconds as from
    /// [`localtime::local_seconds`]; x values are seconds relative to it.
    pub x_origin: i64,
    /// Seconds between vertical gridlines; `None` spreads them evenly instead.
    pub x_step: Option<i64>,
}

impl Axes {
    /// Widens `y` so horizontal gridlines sit on rows at multiples of a round step,
    /// the finest from [`Y_STEPS`] (not below `min_step`) the rows have room for.
    pub fn fit(
        x: (f64, f64),
        y: (f64, f64),
        (cols, rows): (u16, u16),
        min_step: f64,
        x_origin: i64,
    ) -> Self {
        let intervals = f64::from(rows.saturating_sub(1));
        let fitted = [MIN_GRID_ROWS, 1.0].into_iter().find_map(|min_rows| {
            Y_STEPS
                .into_iter()
                .filter(|&s| s >= min_step)
                .find_map(|step| {
                    let lo = (y.0 / step).floor();
                    let steps = ((y.1 / step).ceil() - lo).max(1.0);
                    let rows_per_step = (intervals / steps).floor();
                    (rows_per_step >= min_rows).then(|| {
                        // Center the data using whole steps, so gridlines stay on rows.
                        let spare = (intervals / rows_per_step).floor() - steps;
                        let lo = (lo - (spare / 2.0).floor()) * step;
                        ((lo, lo + intervals / rows_per_step * step), step)
                    })
                })
        });
        let (y, y_step) = fitted.map_or((y, None), |(y, step)| (y, Some(step)));
        let cols_per_second = f64::from(cols.saturating_sub(1)) / (x.1 - x.0);
        let x_step = X_STEPS_MINUTES
            .into_iter()
            .map(|minutes| i64::from(minutes) * 60)
            .find(|&step| seconds(step) * cols_per_second >= MIN_GRID_COLS);
        Self {
            x,
            y,
            cols,
            rows,
            y_step,
            x_origin,
            x_step,
        }
    }

    /// Columns that get a gridline, with the local time each stands for.
    pub fn grid_times(&self) -> Vec<(u16, i64)> {
        let Some(step) = self.x_step else {
            let last = self.cols.saturating_sub(1);
            return evenly_spaced(last, if self.cols >= 40 { 4 } else { 1 })
                .into_iter()
                .map(|col| (col, self.x_origin + whole_seconds(self.value_at_col(col))))
                .collect();
        };
        let (lo, hi) = self.x;
        let (start, end) = (
            self.x_origin + whole_seconds(lo.ceil()),
            self.x_origin + whole_seconds(hi.floor()),
        );
        let last = f64::from(self.cols.saturating_sub(1));
        let first = start + (-start).rem_euclid(step);
        (first..=end)
            .step_by(usize::try_from(step).expect("steps are positive"))
            .filter_map(|time| {
                let x = seconds(time - self.x_origin);
                let col = ((x - lo) / (hi - lo) * last).round();
                Some((whole_column(col)?, time))
            })
            .collect()
    }

    pub fn value_at_row(&self, row: u16) -> f64 {
        let (lo, hi) = self.y;
        hi - (hi - lo) * f64::from(row) / f64::from(self.rows.saturating_sub(1).max(1))
    }

    pub fn value_at_col(&self, col: u16) -> f64 {
        let (lo, hi) = self.x;
        lo + (hi - lo) * f64::from(col) / f64::from(self.cols.saturating_sub(1).max(1))
    }

    /// Rows that get a gridline and a label.
    pub fn grid_rows(&self) -> Vec<u16> {
        let Some(step) = self.y_step else {
            let last = self.rows.saturating_sub(1);
            return evenly_spaced(last, (last / 3).clamp(1, 8));
        };
        (0..self.rows)
            .filter(|&row| {
                let steps = self.value_at_row(row) / step;
                (steps - steps.round()).abs() < 1e-6
            })
            .collect()
    }

    /// Decimals that y labels need to tell gridlines apart.
    pub fn y_decimals(&self) -> usize {
        self.y_step.map_or(1, step_decimals)
    }

    /// Columns that get a gridline and a time label.
    pub fn grid_cols(&self) -> Vec<u16> {
        self.grid_times().into_iter().map(|(col, _)| col).collect()
    }
}

#[expect(
    clippy::cast_precision_loss,
    reason = "times in seconds stay far below 2^53"
)]
fn seconds(value: i64) -> f64 {
    value as f64
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "callers pass whole numbers of seconds from a chart window"
)]
fn whole_seconds(value: f64) -> i64 {
    value.round() as i64
}

fn whole_column(value: f64) -> Option<u16> {
    (0.0..=f64::from(u16::MAX))
        .contains(&value)
        .then(|| u16::try_from(whole_seconds(value)).expect("checked range"))
}

/// Decimals that labels `step` apart need.
fn step_decimals(step: f64) -> usize {
    if step >= 1.0 {
        0
    } else if step < 0.1 {
        2
    } else {
        1
    }
}

/// Y bounds and labels for the braille chart, which ratatui spreads evenly over the
/// bounds: `y` widened to whole multiples of the finest step from [`Y_STEPS`] (not
/// below `min_step`) that leaves [`MIN_GRID_ROWS`] of the `rows` per label, with a
/// label at every step. Without such a step, `y` unchanged with its ends and middle.
fn braille_y_axis(y: (f64, f64), rows: u16, min_step: f64) -> ((f64, f64), Vec<String>) {
    let intervals = f64::from(rows.saturating_sub(1));
    let fitted = Y_STEPS
        .into_iter()
        .filter(|&s| s >= min_step)
        .find_map(|step| {
            let lo = (y.0 / step).floor();
            let steps = ((y.1 / step).ceil() - lo).max(1.0);
            (intervals / steps >= MIN_GRID_ROWS).then_some((step, lo, steps))
        });
    let Some((step, lo, steps)) = fitted else {
        let labels = [y.0, f64::midpoint(y.0, y.1), y.1]
            .iter()
            .map(|v| format!("{v:.1}"))
            .collect();
        return (y, labels);
    };
    let decimals = step_decimals(step);
    // Each label has at least `MIN_GRID_ROWS` rows, so there are fewer than `rows`.
    let labels = (0..rows)
        .map(f64::from)
        .take_while(|&i| i <= steps)
        .map(|i| format!("{:.decimals$}", (lo + i) * step))
        .collect();
    ((lo * step, (lo + steps) * step), labels)
}

fn evenly_spaced(last: u16, intervals: u16) -> Vec<u16> {
    let (last, n) = (u32::from(last), u32::from(intervals));
    (0..=n)
        .map(|i| u16::try_from((i * last + n / 2) / n).expect("at most `last`, a u16"))
        .collect()
}

pub struct ChartView {
    graphics: Option<Graphics>,
}

impl ChartView {
    /// Images go to the terminal through shared memory or a temporary file when
    /// `picker` says that works; otherwise the chart is drawn with braille characters.
    /// Finished images arrive on the returned receiver and must be passed to
    /// [`ChartView::accept`].
    pub fn new(picker: Option<Picker>) -> (Self, UnboundedReceiver<ChartUpdate>) {
        let (updates, receiver) = mpsc::unbounded_channel();
        let graphics = picker.filter(kitty::usable).map(|picker| {
            let font = picker.font_size();
            Graphics {
                requests: spawn_renderer((font.width.into(), font.height.into()), updates.clone()),
                in_flight: false,
                shown: None,
                failure: None,
            }
        });
        (Self { graphics }, receiver)
    }

    /// Takes a finished image; returns the escape sequence to write to the terminal.
    pub fn accept(&mut self, update: ChartUpdate) -> Option<String> {
        let graphics = self.graphics.as_mut()?;
        graphics.in_flight = false;
        match update.image {
            Ok(image) => {
                graphics.failure = None;
                let seq = image.transmit(CHART_IMAGE_ID, update.axes.cols, update.axes.rows);
                graphics.shown = Some(Shown {
                    view: update.view,
                    axes: update.axes,
                    requested_at: update.requested_at,
                });
                Some(seq)
            }
            Err(err) => {
                graphics.failure = Some((err, Instant::now()));
                None
            }
        }
    }

    /// Escape sequence that frees the chart image in the terminal, for shutdown.
    pub fn farewell(&self) -> Option<String> {
        self.graphics
            .as_ref()
            .map(|_| kitty::delete(CHART_IMAGE_ID))
    }

    /// `cut` marks a calibration window on the chart and stretches its span to fit.
    pub fn render(&mut self, frame: &mut Frame, area: Rect, app: &App, cut: Option<Cut>) {
        let now = SystemTime::now();
        let window = cut.map_or_else(|| app.window(), |cut| cut.span(now));
        let title = format!(
            " {} ({}) · last {}{} ",
            app.metric.label(),
            app.metric.unit(),
            format_span(window.as_secs_f64()),
            if cut.is_some() {
                " · shaded: left out of the calibration"
            } else {
                ""
            }
        );
        let block = Block::bordered().title(title);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let series = build_series(app, now, window);
        let Some(y) = y_bounds(&series, app.metric) else {
            let waiting = Paragraph::new("\n  Waiting for measurements…").dark_gray();
            frame.render_widget(waiting, inner);
            return;
        };
        let x = (-window.as_secs_f64(), 0.0);
        let origin = localtime::local_seconds(now);
        match &mut self.graphics {
            Some(graphics) => {
                let shape = Shape {
                    ranges: (x, y),
                    x_origin: origin,
                    window,
                    cut,
                    cut_x: cut.map(|cut| cut.x_range(now)),
                };
                graphics.render(frame, inner, app, series, shape);
            }
            None => render_braille(
                frame,
                inner,
                &series,
                (x, y),
                origin,
                app.metric.min_grid_step(),
            ),
        }
    }
}

/// A finished chart image from the renderer thread.
pub struct ChartUpdate {
    view: View,
    axes: Axes,
    requested_at: Instant,
    image: Result<kitty::Handoff, String>,
}

struct Request {
    view: View,
    axes: Axes,
    requested_at: Instant,
    series: Vec<Series>,
    /// Kept x range of [`View::cut`] at the time of the request.
    cut_x: Option<(f64, f64)>,
}

/// Value ranges, time span and calibration overlay of one chart image.
#[derive(Debug, Clone, Copy)]
struct Shape {
    ranges: Ranges,
    /// Local clock time at x = 0, see [`Axes::x_origin`].
    x_origin: i64,
    window: Duration,
    cut: Option<Cut>,
    cut_x: Option<(f64, f64)>,
}

/// Rasterizes on its own thread so a slow frame never delays a keypress.
fn spawn_renderer(
    (cell_width, cell_height): (u32, u32),
    updates: UnboundedSender<ChartUpdate>,
) -> std::sync::mpsc::Sender<Request> {
    let (requests, received) = std::sync::mpsc::channel::<Request>();
    std::thread::spawn(move || {
        let mut files = kitty::ImageFiles::new();
        for request in received {
            let image = plot::rasterize(
                &request.axes,
                (cell_width, cell_height),
                request.view.style,
                &request.series,
                request.cut_x,
            )
            .ok_or_else(|| "chart area too large".to_owned())
            .and_then(|pixels| {
                files
                    .write(&pixels)
                    .map_err(|e| format!("writing the chart image: {e}"))
            });
            let update = ChartUpdate {
                view: request.view,
                axes: request.axes,
                requested_at: request.requested_at,
                image,
            };
            if updates.send(update).is_err() {
                break;
            }
        }
    });
    requests
}

struct Graphics {
    requests: std::sync::mpsc::Sender<Request>,
    /// A request is with the renderer; wait for it before asking again.
    in_flight: bool,
    /// What the terminal currently holds as [`CHART_IMAGE_ID`].
    shown: Option<Shown>,
    /// The last attempt's error; retried after [`REFRESH`].
    failure: Option<(String, Instant)>,
}

struct Shown {
    view: View,
    axes: Axes,
    requested_at: Instant,
}

/// Everything besides new data that changes the picture; a change redraws immediately.
#[derive(Debug, Clone, PartialEq)]
struct View {
    area: Rect,
    metric: Metric,
    style: ChartStyle,
    window: Duration,
    selected: Option<usize>,
    devices: usize,
    cut: Option<Cut>,
}

type Ranges = ((f64, f64), (f64, f64));

impl Graphics {
    fn render(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        app: &App,
        series: Vec<Series>,
        shape: Shape,
    ) {
        let [top, bottom] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);
        let [y_labels, image_area] =
            Layout::horizontal([Constraint::Length(Y_LABEL_WIDTH), Constraint::Min(1)]).areas(top);
        let [_, x_labels] =
            Layout::horizontal([Constraint::Length(Y_LABEL_WIDTH), Constraint::Min(1)])
                .areas(bottom);
        if image_area.width < 2 || image_area.height < 2 {
            return;
        }
        if let Some((err, _)) = &self.failure {
            let msg = Paragraph::new(format!("chart image failed: {err}")).red();
            frame.render_widget(msg, area);
        }

        let view = View {
            area: image_area,
            metric: app.metric,
            style: app.style,
            window: shape.window,
            selected: app.selected_index(),
            devices: app.devices.len(),
            cut: shape.cut,
        };
        let stale = self
            .shown
            .as_ref()
            .is_none_or(|s| s.view != view || s.requested_at.elapsed() >= REFRESH);
        let retry = self
            .failure
            .as_ref()
            .is_none_or(|(_, at)| at.elapsed() >= REFRESH);
        if stale && retry && !self.in_flight {
            let (x, y) = shape.ranges;
            let request = Request {
                view,
                axes: Axes::fit(
                    x,
                    y,
                    (image_area.width, image_area.height),
                    app.metric.min_grid_step(),
                    shape.x_origin,
                ),
                requested_at: Instant::now(),
                series,
                cut_x: shape.cut_x,
            };
            match self.requests.send(request) {
                Ok(()) => self.in_flight = true,
                Err(_) => {
                    self.failure = Some(("chart renderer stopped".to_owned(), Instant::now()));
                }
            }
        }
        // Until a new image arrives the previous one stays up; after a resize it
        // would be the wrong size, so the area is left blank instead.
        if let Some(shown) = self.shown.as_ref().filter(|s| s.view.area == image_area) {
            kitty::place(frame.buffer_mut(), image_area, CHART_IMAGE_ID);
            // Labels come from the shown image's axes so they always match it.
            render_y_labels(frame, y_labels, &shown.axes);
            render_x_labels(frame, x_labels, &shown.axes);
        }
    }
}

fn render_y_labels(frame: &mut Frame, area: Rect, axes: &Axes) {
    let width = usize::from(Y_LABEL_WIDTH - 1);
    let decimals = axes.y_decimals();
    for row in axes.grid_rows() {
        if row >= area.height {
            continue;
        }
        let label = format!("{:>width$.decimals$}", axes.value_at_row(row));
        let cell = Rect {
            y: area.y + row,
            height: 1,
            ..area
        };
        frame.render_widget(Span::from(label).dark_gray(), cell);
    }
}

fn render_x_labels(frame: &mut Frame, area: Rect, axes: &Axes) {
    for (col, time) in axes.grid_times() {
        let label = localtime::label(time);
        let len = u16::try_from(label.chars().count()).unwrap_or(u16::MAX);
        let Some(max_x) = area.width.checked_sub(len) else {
            continue;
        };
        let x = col.saturating_sub(len / 2).min(max_x);
        let cell = Rect {
            x: area.x + x,
            width: len,
            ..area
        };
        frame.render_widget(Span::from(label).dark_gray(), cell);
    }
}

fn render_braille(
    frame: &mut Frame,
    area: Rect,
    series: &[Series],
    (x, y): Ranges,
    x_origin: i64,
    min_step: f64,
) {
    let coordinates: Vec<Vec<(f64, f64)>> = series
        .iter()
        .map(|s| s.points.iter().map(|p| (p.x, p.y)).collect())
        .collect();
    let datasets: Vec<Dataset> = series
        .iter()
        .zip(&coordinates)
        .map(|(s, coordinates)| {
            let style = Style::new().fg(ratatui_color(s.color));
            let style = if s.emphasis == Emphasis::Dimmed {
                style.dim()
            } else {
                style
            };
            Dataset::default()
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(style)
                .data(coordinates)
        })
        .collect();
    let x_labels: Vec<Line> = [x.0, x.0 / 2.0, 0.0]
        .into_iter()
        .map(|v| Line::from(localtime::label(x_origin + whole_seconds(v))))
        .collect();
    // The x axis line and its labels take the bottom two rows.
    let (y, y_labels) = braille_y_axis(y, area.height.saturating_sub(2), min_step);
    let y_labels: Vec<Line> = y_labels.into_iter().map(Line::from).collect();
    let chart = Chart::new(datasets)
        .x_axis(
            Axis::default()
                .bounds([x.0, x.1])
                .labels(x_labels)
                .dark_gray(),
        )
        .y_axis(
            Axis::default()
                .bounds([y.0, y.1])
                .labels(y_labels)
                .dark_gray(),
        );
    frame.render_widget(chart, area);
}

fn build_series(app: &App, now: SystemTime, window: Duration) -> Vec<Series> {
    let selected = app.selected_index();
    app.devices
        .iter()
        .enumerate()
        .map(|(i, device)| {
            let mut samples: Vec<&Sample> = device
                .history()
                .iter()
                .rev()
                .take_while(|sample| now.duration_since(sample.at).unwrap_or_default() <= window)
                .collect();
            samples.reverse();
            let mut previous: Option<&Sample> = None;
            let points = samples
                .into_iter()
                .filter_map(|sample| {
                    let y = app.metric.of(sample)?;
                    let joined = previous.is_some_and(|prev| joins(prev, sample));
                    previous = Some(sample);
                    Some(Point {
                        x: -now
                            .duration_since(sample.at)
                            .unwrap_or_default()
                            .as_secs_f64(),
                        y: f64::from(y),
                        joined,
                    })
                })
                .collect();
            let emphasis = match selected {
                None => Emphasis::Normal,
                Some(s) if s == i => Emphasis::Selected,
                Some(_) => Emphasis::Dimmed,
            };
            Series {
                color: device_color(i),
                points,
                emphasis,
            }
        })
        .collect()
}

/// Whether a line belongs between two consecutive samples.
fn joins(earlier: &Sample, later: &Sample) -> bool {
    let limit = if earlier.source == Source::Log || later.source == Source::Log {
        LOG_GAP
    } else {
        LIVE_GAP
    };
    later.at.duration_since(earlier.at).unwrap_or_default() <= limit
}

/// Y range covering all points with some headroom, or `None` without data.
fn y_bounds(series: &[Series], metric: Metric) -> Option<(f64, f64)> {
    let (lo, hi) = series.iter().flat_map(|s| &s.points).fold(
        None,
        |acc: Option<(f64, f64)>, &Point { y: v, .. }| {
            Some(acc.map_or((v, v), |(lo, hi)| (lo.min(v), hi.max(v))))
        },
    )?;
    let span = (hi - lo).max(metric.min_span());
    let center = f64::midpoint(lo, hi);
    Some((center - span * 0.55, center + span * 0.55))
}

/// `"10m"`, `"7.5m"`, `"6h"`, `"4.5h"`.
fn format_span(secs: f64) -> String {
    let (value, unit) = if secs < 2.0 * 3600.0 {
        (secs / 60.0, "m")
    } else {
        (secs / 3600.0, "h")
    };
    let value = (value * 10.0).round() / 10.0;
    if value.fract() == 0.0 {
        format!("{value:.0}{unit}")
    } else {
        format!("{value:.1}{unit}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_includes_both_ends() {
        let axes = Axes {
            x: (-3600.0, 0.0),
            y: (20.0, 22.0),
            cols: 81,
            rows: 13,
            y_step: None,
            x_origin: 0,
            x_step: None,
        };
        let rows = axes.grid_rows();
        assert_eq!(rows.first(), Some(&0));
        assert_eq!(rows.last(), Some(&12));
        assert!((axes.value_at_row(0) - 22.0).abs() < 1e-9);
        assert!((axes.value_at_row(12) - 20.0).abs() < 1e-9);
        assert_eq!(axes.grid_cols(), [0, 20, 40, 60, 80]);
        assert!((axes.value_at_col(40) + 1800.0).abs() < 1e-9);
    }

    /// Fits `y` into `rows` and returns the step and the gridline values, top first.
    fn gridlines(y: (f64, f64), rows: u16, min_step: f64) -> (Option<f64>, Vec<f64>) {
        let axes = Axes::fit((-3600.0, 0.0), y, (80, rows), min_step, 0);
        assert!(
            axes.y.0 <= y.0 && axes.y.1 >= y.1,
            "{:?} misses {y:?}",
            axes.y
        );
        let values = axes
            .grid_rows()
            .into_iter()
            .map(|row| (axes.value_at_row(row) * 100.0).round() / 100.0)
            .collect();
        (axes.y_step, values)
    }

    #[test]
    fn gridlines_get_finer_with_more_rows() {
        let y = (20.85, 21.95);
        let (step, values) = gridlines(y, 40, 0.1);
        assert_eq!(step, Some(0.1));
        assert_eq!(values.len(), 14, "{values:?}");
        assert!(
            values.contains(&21.0) && values.contains(&21.9),
            "{values:?}"
        );

        let (step, values) = gridlines(y, 12, 0.1);
        assert_eq!(step, Some(0.5));
        assert!(
            values.contains(&21.0) && values.contains(&21.5),
            "{values:?}"
        );

        let (step, values) = gridlines(y, 6, 0.1);
        assert_eq!(step, Some(1.0));
        assert_eq!(values, [22.0, 21.0, 20.0]);
    }

    #[test]
    fn close_readings_get_hundredths() {
        let (step, values) = gridlines((21.93, 22.05), 30, 0.02);
        assert_eq!(step, Some(0.02));
        assert!(
            values.contains(&21.94) && values.contains(&22.04),
            "{values:?}"
        );
        let axes = Axes::fit((-3600.0, 0.0), (21.93, 22.05), (80, 30), 0.02, 0);
        assert_eq!(axes.y_decimals(), 2);
        // Humidity stops at tenths.
        assert_eq!(gridlines((55.43, 55.55), 30, 0.1).0, Some(0.1));
    }

    #[test]
    fn braille_labels_sit_on_round_steps() {
        // Four sensors within 0.1 °C, as on a 46-line terminal: labels every 0.02 °C.
        let (bounds, labels) = braille_y_axis((18.695, 18.805), 30, 0.02);
        assert_eq!(
            labels,
            [
                "18.68", "18.70", "18.72", "18.74", "18.76", "18.78", "18.80", "18.82"
            ]
        );
        assert!((bounds.0 - 18.68).abs() < 1e-9 && (bounds.1 - 18.82).abs() < 1e-9);
        // Fewer rows take a coarser step; whole units drop the decimals.
        let (_, labels) = braille_y_axis((18.695, 18.805), 12, 0.02);
        assert_eq!(labels, ["18.65", "18.70", "18.75", "18.80", "18.85"]);
        let (_, labels) = braille_y_axis((19.6, 23.4), 12, 0.02);
        assert_eq!(labels, ["19", "20", "21", "22", "23", "24"]);
        // No room for any round step: the range ends and middle, as before.
        let (bounds, labels) = braille_y_axis((0.0, 200.0), 3, 0.1);
        assert_eq!(bounds, (0.0, 200.0));
        assert_eq!(labels, ["0.0", "100.0", "200.0"]);
    }

    #[test]
    fn gridlines_are_evenly_spaced_and_centered() {
        let axes = Axes::fit((-3600.0, 0.0), (20.85, 21.95), (80, 30), 0.1, 0);
        let rows = axes.grid_rows();
        let gaps: Vec<u16> = rows.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(gaps.iter().all(|&g| g == gaps[0] && g >= 2), "{rows:?}");
        let (above, below) = (rows[0], axes.rows - 1 - rows[rows.len() - 1]);
        assert!(above.abs_diff(below) <= gaps[0], "{rows:?}");
    }

    #[test]
    fn gridlines_respect_metric_step_and_wide_ranges() {
        let (step, _) = gridlines((-80.5, -69.5), 60, 1.0);
        assert_eq!(step, Some(1.0));
        let (step, values) = gridlines((38.0, 92.0), 20, 0.1);
        assert_eq!(step, Some(10.0));
        assert!(
            values.contains(&40.0) && values.contains(&90.0),
            "{values:?}"
        );
        let (step, _) = gridlines((0.0, 200.0), 3, 0.1);
        assert_eq!(step, None);
    }

    #[test]
    fn time_gridlines_fall_on_round_local_times() {
        // Now is 14:07:30 local; one hour across 151 columns.
        let origin = 20_754 * 86_400 + 14 * 3600 + 7 * 60 + 30;
        let axes = Axes::fit((-3600.0, 0.0), (20.0, 22.0), (151, 20), 0.1, origin);
        assert_eq!(axes.x_step, Some(600));
        let labels: Vec<String> = axes
            .grid_times()
            .into_iter()
            .map(|(_, time)| localtime::label(time))
            .collect();
        assert_eq!(
            labels,
            ["13:10", "13:20", "13:30", "13:40", "13:50", "14:00"]
        );
        // 13:10 is 57.5 minutes back: column 150 · 2.5 / 60 ≈ 6.
        assert_eq!(axes.grid_times()[0].0, 6);
        // A day across a narrow chart steps by hours.
        let day = Axes::fit((-86_400.0, 0.0), (20.0, 22.0), (80, 20), 0.1, origin);
        assert_eq!(day.x_step, Some(6 * 3600));
    }

    #[test]
    fn span_labels() {
        assert_eq!(format_span(900.0), "15m");
        assert_eq!(format_span(450.0), "7.5m");
        assert_eq!(format_span(5.0 * 3600.0), "5h");
        assert_eq!(format_span(4.5 * 3600.0), "4.5h");
    }

    #[test]
    fn y_bounds_have_minimum_span() {
        let series = [Series {
            color: (0, 0, 0),
            points: vec![
                Point {
                    x: -10.0,
                    y: 21.0,
                    joined: false,
                },
                Point {
                    x: 0.0,
                    y: 21.2,
                    joined: true,
                },
            ],
            emphasis: Emphasis::Normal,
        }];
        let (lo, hi) = y_bounds(&series, Metric::Temperature).expect("has points");
        assert!(lo < 21.0 && hi > 21.2);
        assert!(hi - lo >= Metric::Temperature.min_span());
        assert_eq!(y_bounds(&[], Metric::Temperature), None);
    }

    #[test]
    fn log_records_join_across_their_period() {
        let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1_793_000_000);
        let sample = |minutes: u64, source| Sample {
            at: start + Duration::from_secs(minutes * 60),
            temperature: 21.0,
            humidity: 50.0,
            rssi: None,
            source,
        };
        let live = |m| sample(m, Source::Live);
        let log = |m| sample(m, Source::Log);
        assert!(joins(&live(0), &live(5)));
        assert!(!joins(&live(0), &live(6)));
        assert!(joins(&log(0), &log(30)));
        assert!(joins(&log(0), &live(30)));
        assert!(joins(&log(0), &log(60)));
        assert!(!joins(&log(0), &log(70)));
    }
}
