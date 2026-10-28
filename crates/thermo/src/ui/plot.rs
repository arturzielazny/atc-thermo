//! Rasterizes chart series into an RGBA image sized to whole text cells.

use tiny_skia::{
    Color, FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform,
};

use super::chart::{Axes, Emphasis, Point, Series};
use super::kitty::Rgba;
use crate::app::ChartStyle;

const GRID_RGBA: (u8, u8, u8, u8) = (128, 128, 128, 60);
const DIMMED_ALPHA: u8 = 80;

/// Draws gridlines and series; `None` if the image would be empty or too large.
///
/// The background stays transparent so the terminal's own background shows through.
pub fn rasterize(
    axes: &Axes,
    (cell_width, cell_height): (u32, u32),
    style: ChartStyle,
    series: &[Series],
) -> Option<Rgba> {
    let mut pixmap = Pixmap::new(
        u32::from(axes.cols) * cell_width,
        u32::from(axes.rows) * cell_height,
    )?;
    let mapping = Mapping {
        axes: *axes,
        cell_width: cell_width as f32,
        cell_height: cell_height as f32,
    };
    draw_grid(&mut pixmap, &mapping);

    let line_width = (cell_height as f32 / 9.0).max(1.5);
    let mut ordered: Vec<&Series> = series.iter().collect();
    // Draw dimmed lines first so the selected one ends up on top.
    ordered.sort_by_key(|s| s.emphasis);
    for s in ordered {
        draw_series(&mut pixmap, &mapping, s, line_width, style);
    }

    // Most pixels are transparent and stay zero; only the rest need un-premultiplying.
    let mut data = vec![0; pixmap.data().len()];
    for (out, p) in data.chunks_exact_mut(4).zip(pixmap.pixels()) {
        match p.alpha() {
            0 => {}
            u8::MAX => out.copy_from_slice(&[p.red(), p.green(), p.blue(), u8::MAX]),
            _ => {
                let c = p.demultiply();
                out.copy_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
            }
        }
    }
    Some(Rgba {
        width: pixmap.width(),
        height: pixmap.height(),
        data,
    })
}

struct Mapping {
    axes: Axes,
    cell_width: f32,
    cell_height: f32,
}

impl Mapping {
    /// Pixel x of the center of a cell column.
    fn col_center(&self, col: u16) -> f32 {
        (f32::from(col) + 0.5) * self.cell_width
    }

    fn row_center(&self, row: u16) -> f32 {
        (f32::from(row) + 0.5) * self.cell_height
    }

    fn x(&self, value: f64) -> f32 {
        let (lo, hi) = self.axes.x;
        let span = f64::from(self.axes.cols.saturating_sub(1)) * f64::from(self.cell_width);
        pixel(f64::from(self.cell_width) / 2.0 + (value - lo) / (hi - lo) * span)
    }

    fn y(&self, value: f64) -> f32 {
        let (lo, hi) = self.axes.y;
        let span = f64::from(self.axes.rows.saturating_sub(1)) * f64::from(self.cell_height);
        pixel(f64::from(self.cell_height) / 2.0 + (hi - value) / (hi - lo) * span)
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "pixel coordinates are far below f32's exact-integer range"
)]
fn pixel(value: f64) -> f32 {
    value as f32
}

fn draw_grid(pixmap: &mut Pixmap, mapping: &Mapping) {
    let (r, g, b, a) = GRID_RGBA;
    let mut paint = Paint::default();
    paint.set_color_rgba8(r, g, b, a);
    let (width, height) = (pixmap.width() as f32, pixmap.height() as f32);
    let lines =
        mapping
            .axes
            .grid_rows()
            .into_iter()
            .filter_map(|row| Rect::from_xywh(0.0, mapping.row_center(row).floor(), width, 1.0))
            .chain(mapping.axes.grid_cols().into_iter().filter_map(|col| {
                Rect::from_xywh(mapping.col_center(col).floor(), 0.0, 1.0, height)
            }));
    for rect in lines {
        pixmap.fill_rect(rect, &paint, Transform::identity(), None);
    }
}

fn draw_series(
    pixmap: &mut Pixmap,
    mapping: &Mapping,
    series: &Series,
    line_width: f32,
    style: ChartStyle,
) {
    let Some(&Point {
        x: last_x,
        y: last_y,
        ..
    }) = series.points.last()
    else {
        return;
    };
    let (r, g, b) = series.color;
    let (alpha, width) = match series.emphasis {
        Emphasis::Dimmed => (DIMMED_ALPHA, line_width),
        Emphasis::Normal => (u8::MAX, line_width),
        Emphasis::Selected => (u8::MAX, line_width * 1.7),
    };
    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba8(r, g, b, alpha));

    match style {
        ChartStyle::Lines => draw_lines(pixmap, mapping, series, &paint, width),
        ChartStyle::Points => draw_points(pixmap, mapping, series, &paint, width),
    }
    if let Some(dot) = PathBuilder::from_circle(mapping.x(last_x), mapping.y(last_y), width * 1.4) {
        pixmap.fill_path(&dot, &paint, FillRule::Winding, Transform::identity(), None);
    }
}

/// One dot per sample, skipping samples that land on the previous one's pixel.
fn draw_points(pixmap: &mut Pixmap, mapping: &Mapping, series: &Series, paint: &Paint, width: f32) {
    let mut dots = PathBuilder::new();
    let mut previous: Option<(f32, f32)> = None;
    for point in &series.points {
        let (px, py) = (mapping.x(point.x).round(), mapping.y(point.y).round());
        if previous.is_some_and(|(x, y)| same_pixel(x, px) && same_pixel(y, py)) {
            continue;
        }
        previous = Some((px, py));
        dots.push_circle(px, py, width * 0.6);
    }
    if let Some(dots) = dots.finish() {
        pixmap.fill_path(&dots, paint, FillRule::Winding, Transform::identity(), None);
    }
}

fn draw_lines(pixmap: &mut Pixmap, mapping: &Mapping, series: &Series, paint: &Paint, width: f32) {
    let mut path = PathBuilder::new();
    let mut column: Option<Column> = None;
    let mut dots = PathBuilder::new();
    for (i, point) in series.points.iter().enumerate() {
        let (px, py) = (mapping.x(point.x), mapping.y(point.y));
        let joined = point.joined;
        // A point joined to neither neighbor would leave no trace as a line.
        if !joined && series.points.get(i + 1).is_none_or(|next| !next.joined) {
            dots.push_circle(px, py, width / 2.0);
        }
        match &mut column {
            Some(c) if joined && same_pixel(c.pixel, px.floor()) => c.add(px, py),
            _ => {
                if let Some(c) = column.take() {
                    c.extend(&mut path);
                }
                column = Some(Column::new(px, py, joined));
            }
        }
    }
    if let Some(c) = column {
        c.extend(&mut path);
    }
    if let Some(path) = path.finish() {
        let stroke = Stroke {
            width,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Stroke::default()
        };
        pixmap.stroke_path(&path, paint, &stroke, Transform::identity(), None);
    }
    if let Some(dots) = dots.finish() {
        pixmap.fill_path(&dots, paint, FillRule::Winding, Transform::identity(), None);
    }
}

#[expect(clippy::float_cmp, reason = "both are floored, so exact whole numbers")]
fn same_pixel(a: f32, b: f32) -> bool {
    a == b
}

/// The points of a series that fall into one pixel column.
///
/// A 48-hour window has about nine samples per pixel column. Stroking each of them
/// with round joins is slow and invisible, so a column becomes its average point, or
/// its highest and lowest point when they are more than a pixel apart.
struct Column {
    pixel: f32,
    /// Whether the line continues from the previous column.
    joined: bool,
    count: f32,
    sum: (f32, f32),
    /// `(order, x, y)`; y grows downwards, so `top` has the smallest y.
    top: (u32, f32, f32),
    bottom: (u32, f32, f32),
}

impl Column {
    fn new(x: f32, y: f32, joined: bool) -> Self {
        Self {
            pixel: x.floor(),
            joined,
            count: 1.0,
            sum: (x, y),
            top: (0, x, y),
            bottom: (0, x, y),
        }
    }

    fn add(&mut self, x: f32, y: f32) {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a pixel column holds far fewer than 2^24 points"
        )]
        let point = (self.count as u32, x, y);
        self.count += 1.0;
        self.sum = (self.sum.0 + x, self.sum.1 + y);
        if y < self.top.2 {
            self.top = point;
        }
        if y > self.bottom.2 {
            self.bottom = point;
        }
    }

    fn extend(&self, path: &mut PathBuilder) {
        let mut add = |first: bool, x: f32, y: f32| {
            if first && !self.joined {
                path.move_to(x, y);
            } else {
                path.line_to(x, y);
            }
        };
        if self.bottom.2 - self.top.2 <= 1.0 {
            add(true, self.sum.0 / self.count, self.sum.1 / self.count);
        } else {
            let (a, b) = if self.top.0 < self.bottom.0 {
                (self.top, self.bottom)
            } else {
                (self.bottom, self.top)
            };
            add(true, a.1, a.2);
            add(false, b.1, b.2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AXES: Axes = Axes {
        x: (-100.0, 0.0),
        y: (0.0, 10.0),
        cols: 11,
        rows: 6,
        y_step: None,
        x_origin: 0,
        x_step: None,
    };

    /// Points joined into one line.
    fn line(points: impl IntoIterator<Item = (f64, f64)>) -> Vec<Point> {
        points
            .into_iter()
            .enumerate()
            .map(|(i, (x, y))| Point {
                x,
                y,
                joined: i > 0,
            })
            .collect()
    }

    fn pixel(image: &Rgba, x: u32, y: u32) -> [u8; 4] {
        let i = 4 * (y * image.width + x) as usize;
        image.data[i..i + 4].try_into().expect("four bytes")
    }

    fn alpha_at(image: &Rgba, x: u32, y: u32) -> u8 {
        pixel(image, x, y)[3]
    }

    #[test]
    fn image_covers_whole_cells() {
        let image = rasterize(&AXES, (8, 16), ChartStyle::Lines, &[]).expect("small image");
        assert_eq!((image.width, image.height), (88, 96));
        assert_eq!(image.data.len(), 88 * 96 * 4);
    }

    #[test]
    fn range_ends_map_to_first_and_last_cell_centers() {
        let mapping = Mapping {
            axes: AXES,
            cell_width: 8.0,
            cell_height: 16.0,
        };
        assert!((mapping.x(-100.0) - 4.0).abs() < 1e-4);
        assert!((mapping.x(0.0) - 84.0).abs() < 1e-4);
        assert!((mapping.y(10.0) - 8.0).abs() < 1e-4);
        assert!((mapping.y(0.0) - 88.0).abs() < 1e-4);
    }

    #[test]
    fn draws_line_through_points() {
        let series = Series {
            color: (255, 0, 0),
            points: line([(-100.0, 5.0), (-50.0, 5.0), (0.0, 5.0)]),
            emphasis: Emphasis::Normal,
        };
        let image = rasterize(&AXES, (8, 16), ChartStyle::Lines, &[series]).expect("small image");
        // Value 5.0 sits halfway between the first and last row centers: y = 48.
        assert_eq!(alpha_at(&image, 30, 48), 255);
        assert_eq!(pixel(&image, 30, 48), [255, 0, 0, 255]);
        // Far from the line and off any gridline stays transparent.
        assert_eq!(alpha_at(&image, 30, 30), 0);
    }

    #[test]
    fn dense_points_keep_their_extremes() {
        // 400 samples alternating between 2 and 8 squeezed into a few pixel columns.
        let series = Series {
            color: (0, 0, 255),
            points: line((0..400).map(|i| {
                (
                    -1.0 + f64::from(i) / 400.0,
                    if i % 2 == 0 { 2.0 } else { 8.0 },
                )
            })),
            emphasis: Emphasis::Normal,
        };
        let image = rasterize(&AXES, (8, 16), ChartStyle::Lines, &[series]).expect("small image");
        // All samples land in pixel column 83 (x from 83.2 to 84), which must still
        // show the full 2..8 swing.
        // Values 7.5, 5 and 2.5 sit at y = 8 + (10 - value) · 8.
        for y in [28, 48, 68] {
            assert_eq!(alpha_at(&image, 83, y), 255, "y {y}");
        }
    }

    #[test]
    fn breaks_lines_and_dots_lone_points() {
        let point = |x, joined| Point { x, y: 5.0, joined };
        let series = Series {
            color: (255, 0, 0),
            // A lone point at -80, a line from -50 to -30, and a lone point at 0.
            points: vec![
                point(-80.0, false),
                point(-50.0, false),
                point(-30.0, true),
                point(0.0, false),
            ],
            emphasis: Emphasis::Normal,
        };
        let image = rasterize(&AXES, (8, 16), ChartStyle::Lines, &[series]).expect("small image");
        // x = 4 + (value + 100) · 0.8; y = 48 for 5.0.
        // The dot is as wide as the line, centered on a pixel corner here.
        assert!(alpha_at(&image, 20, 48) > 100, "lone point at -80");
        assert_eq!(alpha_at(&image, 36, 48), 0, "no line from -80 to -50");
        assert_eq!(alpha_at(&image, 52, 48), 255, "line from -50 to -30");
        assert_eq!(alpha_at(&image, 70, 48), 0, "no line from -30 to 0");
    }

    #[test]
    fn points_style_leaves_gaps_between_samples() {
        let series = Series {
            color: (255, 0, 0),
            points: line([(-80.0, 5.0), (-50.0, 5.0), (0.0, 5.0)]),
            emphasis: Emphasis::Normal,
        };
        let image = rasterize(&AXES, (8, 16), ChartStyle::Points, &[series]).expect("small image");
        // x = 4 + (value + 100) · 0.8: samples at x 20 and 44, nothing between.
        assert!(alpha_at(&image, 20, 48) > 100, "sample at -80");
        assert!(alpha_at(&image, 44, 48) > 100, "sample at -50");
        assert_eq!(alpha_at(&image, 32, 48), 0, "no line between");
    }
}
