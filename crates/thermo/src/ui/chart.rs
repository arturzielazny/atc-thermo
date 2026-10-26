//! History chart: a pixel image when the terminal has a graphics protocol, braille otherwise.

use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use image::DynamicImage;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect, Size};
use ratatui::style::{Style, Stylize};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Block, Chart, Dataset, GraphType, Paragraph};
use ratatui_image::Image;
use ratatui_image::Resize;
use ratatui_image::picker::{Capability, Picker, ProtocolType};
use ratatui_image::protocol::Protocol;
use ratatui_image::protocol::kitty::Kitty;

use super::{device_color, plot, ratatui_color};
use crate::app::{App, Metric};

/// Kitty image id for the chart; reusing it makes the terminal replace the old frame.
const CHART_IMAGE_ID: u32 = 0x7e3a_51c4;
/// How often the image is redrawn for new data and the scrolling time axis.
const REFRESH: Duration = Duration::from_secs(5);
/// Columns reserved left of the plot for y-axis labels.
const Y_LABEL_WIDTH: u16 = 7;

/// One device's line on the chart.
#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    pub color: (u8, u8, u8),
    /// `(seconds relative to now, value)`, oldest first; x is ≤ 0.
    pub points: Vec<(f64, f64)>,
    pub emphasis: Emphasis,
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
    pub fn new(picker: Option<Picker>) -> Self {
        let graphics = picker
            .filter(|p| p.protocol_type() != ProtocolType::Halfblocks)
            .map(|picker| Graphics {
                picker,
                cache: None,
            });
        Self { graphics }
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
            Some(graphics) => graphics.render(frame, inner, app, &series, (x, y)),
            None => render_braille(frame, inner, &series, (x, y)),
        }
    }
}

struct Graphics {
    picker: Picker,
    cache: Option<Cached>,
}

struct Cached {
    view: View,
    axes: Axes,
    rendered_at: Instant,
    protocol: Protocol,
}

/// Everything besides new data that changes the picture; a change redraws immediately.
#[derive(Debug, Clone, PartialEq)]
struct View {
    area: Rect,
    metric: Metric,
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
        series: &[Series],
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

        let view = View {
            area: image_area,
            metric: app.metric,
            window: app.window(),
            selected: app.selected_index(),
            devices: app.devices.len(),
        };
        let stale = self
            .cache
            .as_ref()
            .is_none_or(|c| c.view != view || c.rendered_at.elapsed() >= REFRESH);
        if stale {
            let (x, y) = ranges;
            let axes = Axes {
                x,
                y,
                cols: image_area.width,
                rows: image_area.height,
            };
            match self.encode(&axes, series) {
                Ok(protocol) => {
                    self.cache = Some(Cached {
                        view,
                        axes,
                        rendered_at: Instant::now(),
                        protocol,
                    });
                }
                Err(err) => {
                    let msg = Paragraph::new(format!("chart image failed: {err:#}")).red();
                    frame.render_widget(msg, area);
                    return;
                }
            }
        }
        let cached = self.cache.as_ref().expect("cache was filled above");
        frame.render_widget(Image::new(&cached.protocol), image_area);
        // Labels come from the cached axes so they always match the image on screen.
        render_y_labels(frame, y_labels, &cached.axes);
        render_x_labels(frame, x_labels, &cached.axes);
    }

    fn encode(&self, axes: &Axes, series: &[Series]) -> Result<Protocol> {
        let font = self.picker.font_size();
        let image = plot::rasterize(axes, font.width.into(), font.height.into(), series)
            .context("chart area too large")?;
        let image = DynamicImage::ImageRgba8(image);
        let size = Size::new(axes.cols, axes.rows);
        let protocol = if self.picker.protocol_type() == ProtocolType::Kitty {
            let compress = self
                .picker
                .capabilities()
                .contains(&Capability::KittyCompression);
            let kitty = Kitty::new(
                image,
                size,
                CHART_IMAGE_ID,
                self.picker.tmux_detected(),
                compress,
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?;
            Protocol::Kitty(kitty)
        } else {
            self.picker
                .new_protocol(image, size, Resize::Fit(None))
                .map_err(|e| anyhow::anyhow!("{e}"))?
        };
        Ok(protocol)
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
    let datasets: Vec<Dataset> = series
        .iter()
        .map(|s| {
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
                .data(&s.points)
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
            let mut points: Vec<(f64, f64)> = device
                .history()
                .iter()
                .rev()
                .map_while(|sample| {
                    let ago = now.duration_since(sample.at).unwrap_or_default();
                    (ago <= window).then(|| (-ago.as_secs_f64(), f64::from(app.metric.of(sample))))
                })
                .collect();
            points.reverse();
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

/// Y range covering all points with some headroom, or `None` without data.
fn y_bounds(series: &[Series], metric: Metric) -> Option<(f64, f64)> {
    let (lo, hi) =
        series
            .iter()
            .flat_map(|s| &s.points)
            .fold(None, |acc: Option<(f64, f64)>, &(_, v)| {
                Some(acc.map_or((v, v), |(lo, hi)| (lo.min(v), hi.max(v))))
            })?;
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
            points: vec![(-10.0, 21.0), (0.0, 21.2)],
            emphasis: Emphasis::Normal,
        }];
        let (lo, hi) = y_bounds(&series, Metric::Temperature).expect("has points");
        assert!(lo < 21.0 && hi > 21.2);
        assert!(hi - lo >= Metric::Temperature.min_span());
        assert_eq!(y_bounds(&[], Metric::Temperature), None);
    }
}
