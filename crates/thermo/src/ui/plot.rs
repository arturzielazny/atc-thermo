//! Rasterizes chart series into an RGBA image sized to whole text cells.

use image::RgbaImage;
use tiny_skia::{
    Color, FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform,
};

use super::chart::{Axes, Emphasis, Series};

/// Consecutive samples further apart than this are not joined by a line.
const GAP_SECS: f64 = 300.0;
const GRID_RGBA: (u8, u8, u8, u8) = (128, 128, 128, 60);
const DIMMED_ALPHA: u8 = 80;

/// Draws gridlines and series; `None` if the image would be empty or too large.
///
/// The background stays transparent so the terminal's own background shows through.
pub fn rasterize(
    axes: &Axes,
    cell_width: u32,
    cell_height: u32,
    series: &[Series],
) -> Option<RgbaImage> {
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
        draw_series(&mut pixmap, &mapping, s, line_width);
    }

    let rgba: Vec<u8> = pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    RgbaImage::from_raw(pixmap.width(), pixmap.height(), rgba)
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

fn draw_series(pixmap: &mut Pixmap, mapping: &Mapping, series: &Series, line_width: f32) {
    let Some(&(last_x, last_y)) = series.points.last() else {
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

    let mut path = PathBuilder::new();
    let mut previous_x = None;
    for &(x, y) in &series.points {
        let (px, py) = (mapping.x(x), mapping.y(y));
        match previous_x {
            Some(prev) if x - prev <= GAP_SECS => path.line_to(px, py),
            _ => path.move_to(px, py),
        }
        previous_x = Some(x);
    }
    if let Some(path) = path.finish() {
        let stroke = Stroke {
            width,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Stroke::default()
        };
        pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }
    if let Some(dot) = PathBuilder::from_circle(mapping.x(last_x), mapping.y(last_y), width * 1.4) {
        pixmap.fill_path(&dot, &paint, FillRule::Winding, Transform::identity(), None);
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
    };

    fn alpha_at(image: &RgbaImage, x: u32, y: u32) -> u8 {
        image.get_pixel(x, y).0[3]
    }

    #[test]
    fn image_covers_whole_cells() {
        let image = rasterize(&AXES, 8, 16, &[]).expect("small image");
        assert_eq!(image.dimensions(), (88, 96));
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
    fn draws_line_through_points_and_skips_gaps() {
        let series = Series {
            color: (255, 0, 0),
            points: vec![(-100.0, 5.0), (-50.0, 5.0), (0.0, 5.0)],
            emphasis: Emphasis::Normal,
        };
        let image = rasterize(&AXES, 8, 16, &[series]).expect("small image");
        // Value 5.0 sits halfway between the first and last row centers: y = 48.
        assert_eq!(alpha_at(&image, 30, 48), 255);
        assert_eq!(image.get_pixel(30, 48).0[..3], [255, 0, 0]);
        // Far from the line and off any gridline stays transparent.
        assert_eq!(alpha_at(&image, 30, 30), 0);
    }
}
