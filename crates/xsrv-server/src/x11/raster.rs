// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

use wire::BYTES_PER_PIXEL;

use super::graphics_requests::FILL_STIPPLED;
use super::graphics_requests::FILL_TILED;
use super::resources::{pixel_bytes, Gc, IRect, Surface};

type Point = (i32, i32);

fn brush_size(gc: &Gc) -> i32 {
    i32::from(gc.line_width).max(1)
}

fn brush_area(point: Point, size: i32) -> IRect {
    IRect::new(point.0 - size / 2, point.1 - size / 2, size, size)
}

fn stamp(surface: &mut Surface, area: IRect, gc: &Gc) {
    surface.fill_rect(area, gc.foreground, gc.function, gc.plane_mask);
}

pub fn plot(surface: &mut Surface, point: Point, gc: &Gc) -> IRect {
    let area = brush_area(point, brush_size(gc));
    stamp(surface, area, gc);
    area
}

/// The first and last step of a line whose major coordinate is inside `[low, high)`.
/// `start` is the major coordinate at step 0 and `direction` is 1 or -1.
fn visible_steps(
    start: i64,
    direction: i64,
    low: i64,
    high: i64,
    steps: i64,
) -> Option<(i64, i64)> {
    let (first, last) = if direction > 0 {
        (low - start, high - 1 - start)
    } else {
        (start - (high - 1), start - low)
    };
    let (first, last) = (first.max(0), last.min(steps));
    (first <= last).then_some((first, last))
}

/// Draws a line from `from` to `to`, both ends included.
pub fn line(surface: &mut Surface, from: Point, to: Point, gc: &Gc) -> IRect {
    segment(surface, from, to, gc, true, true)
}

/// Draws connected lines. Each joint pixel is drawn once, so XOR does not cancel it.
/// A closed path also joins the last point to the first one.
pub fn polyline(surface: &mut Surface, points: &[Point], gc: &Gc, closed: bool) -> Option<IRect> {
    let count = points.len();
    if count < 2 {
        return None;
    }
    let segments = if closed { count } else { count - 1 };
    (0..segments)
        .map(|index| {
            let from = points[index];
            let to = points[(index + 1) % count];
            let last = index + 1 == segments;
            segment(surface, from, to, gc, index == 0, !(closed && last))
        })
        .reduce(IRect::union)
}

/// A line width above 1 sweeps a square brush. Only the steps that can reach the surface
/// are computed, so a long line outside the surface costs almost nothing.
/// Returns the box that the line can touch. With a brush wider than 1, the joints can still overlap.
fn segment(
    surface: &mut Surface,
    from: Point,
    to: Point,
    gc: &Gc,
    include_start: bool,
    include_end: bool,
) -> IRect {
    let size = brush_size(gc);
    let touched = IRect {
        x0: from.0.min(to.0),
        y0: from.1.min(to.1),
        x1: from.0.max(to.0) + 1,
        y1: from.1.max(to.1) + 1,
    }
    .grow(size);

    let delta = (
        i64::from(to.0) - i64::from(from.0),
        i64::from(to.1) - i64::from(from.1),
    );
    let (length_x, length_y) = (delta.0.abs(), delta.1.abs());
    let steps = length_x.max(length_y);
    if steps == 0 {
        if include_start || include_end {
            plot(surface, from, gc);
        }
        return touched;
    }
    let x_is_major = length_x >= length_y;
    let (major_start, minor_start, major_dir, minor_dir, minor_length) = if x_is_major {
        (from.0, from.1, delta.0.signum(), delta.1.signum(), length_y)
    } else {
        (from.1, from.0, delta.1.signum(), delta.0.signum(), length_x)
    };
    let reach = surface.bounds().grow(size);
    let (low, high) = if x_is_major {
        (reach.x0, reach.x1)
    } else {
        (reach.y0, reach.y1)
    };
    let Some((first, last)) = visible_steps(
        major_start.into(),
        major_dir,
        low.into(),
        high.into(),
        steps,
    ) else {
        return touched;
    };
    let first = if include_start { first } else { first.max(1) };
    let last = if include_end {
        last
    } else {
        last.min(steps - 1)
    };
    if first > last {
        return touched;
    }

    let point_at = |step: i64| {
        let major = i64::from(major_start) + major_dir * step;
        let offset = (2 * minor_length * step + steps) / (2 * steps);
        let minor = i64::from(minor_start) + minor_dir * offset;
        let (x, y) = if x_is_major {
            (major, minor)
        } else {
            (minor, major)
        };
        (x as i32, y as i32)
    };
    let mut previous = point_at(first);
    plot(surface, previous, gc);
    for step in first + 1..=last {
        let point = point_at(step);
        if size == 1 {
            plot(surface, point, gc);
        } else {
            stamp_leading_edges(surface, previous, point, size, gc);
        }
        previous = point;
    }
    touched
}

/// A square brush that moves by one pixel only covers a new column, a new row, or both.
/// When both are new, the row leaves out the corner that the column already covers.
fn stamp_leading_edges(surface: &mut Surface, from: Point, to: Point, size: i32, gc: &Gc) {
    let square = brush_area(to, size);
    if to.0 != from.0 {
        let x = if to.0 > from.0 {
            square.x1 - 1
        } else {
            square.x0
        };
        stamp(surface, IRect::new(x, square.y0, 1, size), gc);
    }
    if to.1 != from.1 {
        let y = if to.1 > from.1 {
            square.y1 - 1
        } else {
            square.y0
        };
        let mut row = IRect::new(square.x0, y, size, 1);
        if to.0 > from.0 {
            row.x1 -= 1;
        } else if to.0 < from.0 {
            row.x0 += 1;
        }
        stamp(surface, row, gc);
    }
}

/// Fills `area` with the tile or the stipple of the GC. The pattern origin is `gc.tile_origin`.
/// Tiled and opaque stippled fills write every pixel. A stippled fill writes only the set bits.
pub fn fill_pattern(surface: &mut Surface, area: IRect, pattern: &Surface, gc: &Gc) {
    let Some(area) = area.intersect(surface.bounds()) else {
        return;
    };
    let size = pattern.bounds();
    if size.width() == 0 || size.height() == 0 {
        return;
    }
    let (origin_x, origin_y) = gc.tile_origin;
    // The bits of the pattern's own depth: a depth-32 tile keeps its alpha.
    let mask = pattern.pixel_mask();
    let pixel_at = |x: i32, y: i32| {
        let column = (x - origin_x).rem_euclid(size.width());
        let row = (y - origin_y).rem_euclid(size.height());
        let bytes = pattern.row(row, column, column + 1);
        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) & mask
    };
    for y in area.y0..area.y1 {
        match gc.fill_style {
            FILL_STIPPLED => {
                let mut x = area.x0;
                while x < area.x1 {
                    if pixel_at(x, y) == 0 {
                        x += 1;
                        continue;
                    }
                    let start = x;
                    while x < area.x1 && pixel_at(x, y) != 0 {
                        x += 1;
                    }
                    let run = IRect::new(start, y, x - start, 1);
                    surface.fill_rect(run, gc.foreground, gc.function, gc.plane_mask);
                }
            }
            _ => {
                let mut row = Vec::with_capacity(area.width() as usize * BYTES_PER_PIXEL);
                for x in area.x0..area.x1 {
                    let value = pixel_at(x, y);
                    let pixel = match gc.fill_style {
                        FILL_TILED => value,
                        _ if value != 0 => gc.foreground,
                        _ => gc.background,
                    };
                    row.extend_from_slice(&pixel_bytes(pixel));
                }
                let size = (area.width(), 1);
                surface.write_rect((area.x0, y), size, &row, gc.function, gc.plane_mask);
            }
        }
    }
}

/// The outline of a rectangle. It covers `width + 1` by `height + 1` pixels, as X does.
pub fn rectangle_outline(surface: &mut Surface, rect: IRect, gc: &Gc) -> IRect {
    let corners = [
        (rect.x0, rect.y0),
        (rect.x1, rect.y0),
        (rect.x1, rect.y1),
        (rect.x0, rect.y1),
    ];
    polyline(surface, &corners, gc, true).unwrap_or(rect)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn gc(width: u16) -> Gc {
        Gc {
            foreground: 0xFFFFFF,
            line_width: width,
            ..Gc::default()
        }
    }

    fn lit(surface: &Surface, x: i32, y: i32) -> bool {
        surface.row(y, x, x + 1)[0] != 0
    }

    fn lit_count(surface: &Surface) -> usize {
        let bounds = surface.bounds();
        (bounds.y0..bounds.y1)
            .map(|y| {
                (bounds.x0..bounds.x1)
                    .filter(|&x| lit(surface, x, y))
                    .count()
            })
            .sum()
    }

    #[test]
    fn horizontal_line_includes_both_ends() {
        let mut surface = Surface::new(8, 3);
        line(&mut surface, (1, 1), (5, 1), &gc(0));
        assert!((1..=5).all(|x| lit(&surface, x, 1)));
        assert!(!lit(&surface, 0, 1) && !lit(&surface, 6, 1) && !lit(&surface, 3, 0));
    }

    #[test]
    fn diagonal_line_hits_the_corners_in_any_direction() {
        let mut surface = Surface::new(5, 5);
        line(&mut surface, (4, 4), (0, 0), &gc(1));
        assert!((0..5).all(|i| lit(&surface, i, i)));
        assert!(!lit(&surface, 1, 0));
    }

    #[test]
    fn steep_line_has_one_pixel_per_row() {
        let mut surface = Surface::new(6, 10);
        line(&mut surface, (1, 0), (4, 9), &gc(0));
        assert_eq!(lit_count(&surface), 10);
        assert!(lit(&surface, 1, 0) && lit(&surface, 4, 9));
    }

    #[test]
    fn line_far_outside_the_surface_is_safe_and_still_crosses_it() {
        let mut surface = Surface::new(4, 4);
        let touched = line(&mut surface, (-30000, -30000), (30000, 30000), &gc(0));
        assert!(lit(&surface, 2, 2));
        assert!(touched.x0 <= -30000 && touched.x1 > 30000);
    }

    #[test]
    fn many_long_off_screen_segments_are_cheap() {
        let mut surface = Surface::new(640, 480);
        let started = Instant::now();
        for i in 0..8000 {
            line(&mut surface, (-30000, 1000 + i), (30000, 1100 + i), &gc(0));
            line(
                &mut surface,
                (700 + i % 50, -30000),
                (700 + i % 50, 30000),
                &gc(1),
            );
        }
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(lit_count(&surface), 0);
    }

    #[test]
    fn very_wide_line_stays_cheap() {
        let mut surface = Surface::new(64, 64);
        let started = Instant::now();
        line(&mut surface, (0, 32), (60000, 32), &gc(2000));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(lit_count(&surface), 64 * 64);
    }

    #[test]
    fn wide_line_uses_a_square_brush() {
        let mut surface = Surface::new(9, 9);
        line(&mut surface, (4, 4), (4, 4), &gc(3));
        assert!(lit(&surface, 3, 3) && lit(&surface, 5, 5));
        assert!(!lit(&surface, 2, 4) && !lit(&surface, 6, 4));
    }

    #[test]
    fn wide_diagonal_line_has_no_gaps() {
        let mut surface = Surface::new(20, 20);
        line(&mut surface, (3, 3), (16, 12), &gc(3));
        for step in 0..=9 {
            let x = 3 + step * 13 / 9;
            let y = 3 + step;
            assert!(lit(&surface, x, y), "gap at {x},{y}");
        }
    }

    #[test]
    fn rectangle_outline_leaves_the_inside_empty() {
        let mut surface = Surface::new(8, 8);
        rectangle_outline(&mut surface, IRect::new(1, 1, 4, 4), &gc(0));
        assert!(lit(&surface, 1, 1) && lit(&surface, 5, 5) && lit(&surface, 5, 1));
        assert!(!lit(&surface, 3, 3));
        assert!(!lit(&surface, 6, 6));
    }

    fn xor_gc() -> Gc {
        Gc {
            function: 6,
            ..gc(0)
        }
    }

    #[test]
    fn xor_polyline_draws_each_joint_once() {
        let mut surface = Surface::new(12, 12);
        polyline(&mut surface, &[(1, 1), (6, 1), (6, 6)], &xor_gc(), false);
        assert!(lit(&surface, 6, 1), "the joint is drawn once");
        assert!(lit(&surface, 1, 1) && lit(&surface, 6, 6));
        assert_eq!(lit_count(&surface), 6 + 5);
    }

    #[test]
    fn xor_rectangle_outline_keeps_its_corners() {
        let mut surface = Surface::new(12, 12);
        rectangle_outline(&mut surface, IRect::new(2, 2, 6, 4), &xor_gc());
        for corner in [(2, 2), (8, 2), (8, 6), (2, 6)] {
            assert!(lit(&surface, corner.0, corner.1), "corner {corner:?}");
        }
        assert_eq!(lit_count(&surface), 2 * 7 + 2 * 3);
    }

    #[test]
    fn xor_wide_diagonal_has_no_holes() {
        let mut surface = Surface::new(20, 20);
        let gc = Gc {
            line_width: 3,
            ..xor_gc()
        };
        line(&mut surface, (3, 3), (12, 12), &gc);
        for i in 3..=12 {
            assert!(lit(&surface, i, i), "hole at {i},{i}");
        }
    }
}

/// An arc of an ellipse that fits in the rectangle `area`. The angles are in 1/64 degree: `start`
/// counts from three o'clock, counter-clockwise, and `sweep` is how far the arc goes (it may be
/// negative, and 360 degrees or more is the whole ellipse).
#[derive(Clone, Copy)]
pub struct Arc {
    pub area: IRect,
    pub start: i32,
    pub sweep: i32,
}

const FULL_TURN: i32 = 360 * 64;

impl Arc {
    fn centre_and_radii(&self) -> (f64, f64, f64, f64) {
        let (w, h) = (f64::from(self.area.width()), f64::from(self.area.height()));
        (
            f64::from(self.area.x0) + w / 2.0,
            f64::from(self.area.y0) + h / 2.0,
            w / 2.0,
            h / 2.0,
        )
    }

    /// The angles of the two ends, as a start in `[0, FULL_TURN)` and a sweep that is not negative.
    fn range(&self) -> (i32, i32) {
        let (start, sweep) = if self.sweep < 0 {
            (self.start + self.sweep, -self.sweep)
        } else {
            (self.start, self.sweep)
        };
        (start.rem_euclid(FULL_TURN), sweep.min(FULL_TURN))
    }

    fn point_at(&self, angle: f64) -> (f64, f64) {
        let (cx, cy, a, b) = self.centre_and_radii();
        (cx + a * angle.cos(), cy - b * angle.sin())
    }
}

fn units_to_radians(units: i32) -> f64 {
    f64::from(units) * std::f64::consts::PI / f64::from(FULL_TURN / 2)
}

/// Fills an arc. `pie` joins the ends to the centre, otherwise they are joined to each other.
pub fn fill_arc(surface: &mut Surface, arc: Arc, pie: bool, gc: &Gc) -> Option<IRect> {
    let area = arc.area;
    if area.width() <= 0 || area.height() <= 0 {
        return None;
    }
    let (cx, cy, a, b) = arc.centre_and_radii();
    let (start, sweep) = arc.range();
    let whole = sweep >= FULL_TURN;
    let first = arc.point_at(units_to_radians(start));
    let last = arc.point_at(units_to_radians(start + sweep));
    let middle = arc.point_at(units_to_radians(start + sweep / 2));
    let side =
        |p: (f64, f64)| (last.0 - first.0) * (p.1 - first.1) - (last.1 - first.1) * (p.0 - first.0);
    let middle_side = side(middle);
    for y in area.y0..area.y1 {
        let dy = (f64::from(y) + 0.5 - cy) / b;
        if dy.abs() > 1.0 {
            continue;
        }
        let mut run_start: Option<i32> = None;
        for x in area.x0..=area.x1 {
            let inside = x < area.x1 && {
                let (px, py) = (f64::from(x) + 0.5, f64::from(y) + 0.5);
                let dx = (px - cx) / a;
                let in_ellipse = dx * dx + dy * dy <= 1.0;
                in_ellipse
                    && (whole || {
                        let angle = (cy - py) / b;
                        let turn =
                            angle.atan2(dx) * f64::from(FULL_TURN / 2) / std::f64::consts::PI;
                        let from_start = (turn.round() as i32 - start).rem_euclid(FULL_TURN);
                        if pie {
                            from_start <= sweep
                        } else {
                            // A chord: the side of the chord line where the arc is.
                            side((px, py)) * middle_side >= 0.0
                        }
                    })
            };
            match (inside, run_start) {
                (true, None) => run_start = Some(x),
                (false, Some(begin)) => {
                    surface.fill_rect(
                        IRect::new(begin, y, x - begin, 1),
                        gc.foreground,
                        gc.function,
                        gc.plane_mask,
                    );
                    run_start = None;
                }
                _ => {}
            }
        }
    }
    Some(area)
}

/// Draws the outline of an arc with the line width of the GC.
pub fn arc_outline(surface: &mut Surface, arc: Arc, gc: &Gc) -> Option<IRect> {
    let area = arc.area;
    if area.width() <= 0 || area.height() <= 0 {
        return None;
    }
    let (cx, cy, a, b) = arc.centre_and_radii();
    let (start, sweep) = arc.range();
    // Pixels at the edge of the rectangle, so the radii are half a pixel smaller.
    let (ra, rb) = ((a - 0.5).max(0.0), (b - 0.5).max(0.0));
    let steps = ((units_to_radians(sweep) * ra.max(rb) * 2.0).ceil() as i32).max(8);
    let mut points: Vec<Point> = Vec::new();
    for step in 0..=steps {
        let angle =
            units_to_radians(start) + units_to_radians(sweep) * f64::from(step) / f64::from(steps);
        let point = (
            (cx - 0.5 + ra * angle.cos()).round() as i32,
            (cy - 0.5 - rb * angle.sin()).round() as i32,
        );
        if points.last() != Some(&point) {
            points.push(point);
        }
    }
    if points.len() == 1 {
        return Some(plot(surface, points[0], gc));
    }
    polyline(surface, &points, gc, false).or(Some(area))
}

/// Fills a polygon. `winding` is the non-zero winding rule, otherwise even-odd.
pub fn fill_polygon(
    surface: &mut Surface,
    points: &[Point],
    winding: bool,
    gc: &Gc,
) -> Option<IRect> {
    if points.len() < 3 {
        return None;
    }
    let top = points.iter().map(|p| p.1).min()?;
    let bottom = points.iter().map(|p| p.1).max()?;
    let left = points.iter().map(|p| p.0).min()?;
    let right = points.iter().map(|p| p.0).max()?;
    for y in top..bottom {
        let centre = f64::from(y) + 0.5;
        let mut crossings: Vec<(f64, i32)> = Vec::new();
        for index in 0..points.len() {
            let (from, to) = (points[index], points[(index + 1) % points.len()]);
            if from.1 == to.1 {
                continue;
            }
            let (low, high, direction) = if from.1 < to.1 {
                (from, to, 1)
            } else {
                (to, from, -1)
            };
            if centre < f64::from(low.1) || centre >= f64::from(high.1) {
                continue;
            }
            let t = (centre - f64::from(low.1)) / f64::from(high.1 - low.1);
            crossings.push((f64::from(low.0) + t * f64::from(high.0 - low.0), direction));
        }
        crossings.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut winding_number = 0;
        for (index, pair) in crossings.windows(2).enumerate() {
            winding_number += pair[0].1;
            let inside = if winding {
                winding_number != 0
            } else {
                index % 2 == 0
            };
            if inside {
                let begin = (pair[0].0 - 0.5).ceil() as i32;
                let end = (pair[1].0 - 0.5).ceil() as i32;
                if end > begin {
                    surface.fill_rect(
                        IRect::new(begin, y, end - begin, 1),
                        gc.foreground,
                        gc.function,
                        gc.plane_mask,
                    );
                }
            }
        }
    }
    Some(IRect::new(left, top, right - left, bottom - top))
}
