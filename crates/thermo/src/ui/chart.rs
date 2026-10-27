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

/// Kitty image id for the chart; reusing it makes the terminal replace the old frame.
/// Below 2²⁴ so it fits in a placeholder's foreground color.
const CHART_IMAGE_ID: u32 = 0x3a_51c4;
/// How often the image is redrawn for new data and the scrolling time axis.
const REFRESH: Duration = Duration::from_secs(5);
/// Columns reserved left of the plot for y-axis labels.
const Y_LABEL_WIDTH: u16 = 7;

/// Live samples further apart than this are not joined by a line.
const LIVE_GAP: Duration = Duration::from_secs(5 * 60);
/// The same for stretches filled from device logs, which hold one averaged record per
/// log period; the longest period the settings offer is an hour.
const LOG_GAP: Duration = Duration::from_secs(65 * 60);

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
}

impl Axes {
    pub fn value_at_row(&self, row: u16) -> f64 {
        let (lo, hi) = self.y;
        hi - (hi - lo) * f64::from(row) / f64::from(self.rows.saturating_sub(1).max(1))
    }

    pub fn value_at_col(&self, col: u16) -> f64 {
        let (lo, hi) = self.x;
        lo + (hi - lo) * f64::from(col) / f64::from(self.cols.saturating_sub(1).max(1))
    }

    /// Rows that get a gridline and a label, roughly every third row.
    pub fn grid_rows(&self) -> Vec<u16> {
        let last = self.rows.saturating_sub(1);
        evenly_spaced(last, (last / 3).clamp(1, 8))
    }

    /// Columns that get a gridline and a time label.
    pub fn grid_cols(&self) -> Vec<u16> {
        let last = self.cols.saturating_sub(1);
        evenly_spaced(last, if self.cols >= 40 { 4 } else { 1 })
    }
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
    /// Images go to the terminal through shared memory when `picker` says that works;
    /// otherwise the chart is drawn with braille characters. Finished images arrive on
    /// the returned receiver and must be passed to [`ChartView::accept`].
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

    pub fn render(&mut self, frame: &mut Frame, area: Rect, app: &App) {
        let window = app.window();
        let title = format!(
            " {} ({}) · last {} ",
            app.metric.label(),
            app.metric.unit(),
            format_span(window.as_secs_f64())
        );
        let block = Block::bordered().title(title);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let series = build_series(app, SystemTime::now());
        let Some(y) = y_bounds(&series, app.metric) else {
            let waiting = Paragraph::new("\n  Waiting for measurements…").dark_gray();
            frame.render_widget(waiting, inner);
            return;
        };
        let x = (-window.as_secs_f64(), 0.0);
        match &mut self.graphics {
            Some(graphics) => graphics.render(frame, inner, app, series, (x, y)),
            None => render_braille(frame, inner, &series, (x, y)),
        }
    }
}

/// A finished chart image from the renderer thread.
pub struct ChartUpdate {
    view: View,
    axes: Axes,
    requested_at: Instant,
    image: Result<kitty::Shared, String>,
}

struct Request {
    view: View,
    axes: Axes,
    requested_at: Instant,
    series: Vec<Series>,
}

/// Rasterizes on its own thread so a slow frame never delays a keypress.
fn spawn_renderer(
    (cell_width, cell_height): (u32, u32),
    updates: UnboundedSender<ChartUpdate>,
) -> std::sync::mpsc::Sender<Request> {
    let (requests, received) = std::sync::mpsc::channel::<Request>();
    std::thread::spawn(move || {
        let mut memory = kitty::SharedMemory::new();
        for request in received {
            let image = plot::rasterize(
                &request.axes,
                (cell_width, cell_height),
                request.view.style,
                &request.series,
            )
            .ok_or_else(|| "chart area too large".to_owned())
            .and_then(|pixels| {
                memory
                    .write(&pixels)
                    .map_err(|e| format!("writing to shared memory: {e}"))
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
}

type Ranges = ((f64, f64), (f64, f64));

impl Graphics {
    fn render(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        app: &App,
        series: Vec<Series>,
        ranges: Ranges,
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
            window: app.window(),
            selected: app.selected_index(),
            devices: app.devices.len(),
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
            let (x, y) = ranges;
            let request = Request {
                view,
                axes: Axes {
                    x,
                    y,
                    cols: image_area.width,
                    rows: image_area.height,
                },
                requested_at: Instant::now(),
                series,
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
    for row in axes.grid_rows() {
        if row >= area.height {
            continue;
        }
        let label = format!("{:>width$.1}", axes.value_at_row(row));
        let cell = Rect {
            y: area.y + row,
            height: 1,
            ..area
        };
        frame.render_widget(Span::from(label).dark_gray(), cell);
    }
}

fn render_x_labels(frame: &mut Frame, area: Rect, axes: &Axes) {
    for col in axes.grid_cols() {
        let label = format_ago(-axes.value_at_col(col));
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

fn render_braille(frame: &mut Frame, area: Rect, series: &[Series], (x, y): Ranges) {
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
        .map(|v| Line::from(format_ago(-v)))
        .collect();
    let y_labels: Vec<Line> = [y.0, f64::midpoint(y.0, y.1), y.1]
        .into_iter()
        .map(|v| Line::from(format!("{v:.1}")))
        .collect();
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

fn build_series(app: &App, now: SystemTime) -> Vec<Series> {
    let window = app.window();
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
                .map(|sample| {
                    let joined = previous.is_some_and(|prev| joins(prev, sample));
                    previous = Some(sample);
                    Point {
                        x: -now
                            .duration_since(sample.at)
                            .unwrap_or_default()
                            .as_secs_f64(),
                        y: f64::from(app.metric.of(sample)),
                        joined,
                    }
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

/// `"now"` for zero, otherwise e.g. `"-15m"` or `"-1.5h"`.
fn format_ago(secs: f64) -> String {
    if secs.abs() < 0.5 {
        "now".to_owned()
    } else {
        format!("-{}", format_span(secs))
    }
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
        };
        let rows = axes.grid_rows();
        assert_eq!(rows.first(), Some(&0));
        assert_eq!(rows.last(), Some(&12));
        assert!((axes.value_at_row(0) - 22.0).abs() < 1e-9);
        assert!((axes.value_at_row(12) - 20.0).abs() < 1e-9);
        assert_eq!(axes.grid_cols(), [0, 20, 40, 60, 80]);
        assert!((axes.value_at_col(40) + 1800.0).abs() < 1e-9);
    }

    #[test]
    fn span_labels() {
        assert_eq!(format_ago(0.0), "now");
        assert_eq!(format_ago(900.0), "-15m");
        assert_eq!(format_ago(450.0), "-7.5m");
        assert_eq!(format_ago(5.0 * 3600.0), "-5h");
        assert_eq!(format_ago(4.5 * 3600.0), "-4.5h");
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
