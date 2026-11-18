// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

use wire::{Rect, BYTES_PER_PIXEL};

#[derive(Debug, PartialEq, Eq)]
pub enum RegionError {
    OutOfBounds,
    Length,
}

pub struct Framebuffer {
    width: u16,
    height: u16,
    data: Vec<u8>,
    /// True when the pages do not show these pixels yet: after a resize, a page keeps its old
    /// picture. Until the whole screen is written once, a diff against these pixels is not valid.
    unsent: bool,
}

impl Framebuffer {
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            data: vec![0; width as usize * height as usize * BYTES_PER_PIXEL],
            unsent: false,
        }
    }

    /// A black screen that the pages do not show yet.
    pub fn new_unsent(width: u16, height: u16) -> Self {
        Self {
            unsent: true,
            ..Self::new(width, height)
        }
    }

    pub fn is_unsent(&self) -> bool {
        self.unsent
    }

    pub fn size(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    /// The pixels of a rectangle, BGRX. Pixels outside the screen are black.
    pub fn read_region(&self, x: i32, y: i32, width: i32, height: i32) -> Vec<u8> {
        let (screen_width, screen_height) = (i32::from(self.width), i32::from(self.height));
        let mut out = vec![0u8; (width.max(0) * height.max(0)) as usize * BYTES_PER_PIXEL];
        for row in 0..height.max(0) {
            let source_y = y + row;
            if !(0..screen_height).contains(&source_y) {
                continue;
            }
            let from = x.clamp(0, screen_width);
            let to = (x + width).clamp(0, screen_width);
            if from >= to {
                continue;
            }
            let source =
                (source_y as usize * usize::from(self.width) + from as usize) * BYTES_PER_PIXEL;
            let target = (row * width + (from - x)) as usize * BYTES_PER_PIXEL;
            let len = (to - from) as usize * BYTES_PER_PIXEL;
            out[target..target + len].copy_from_slice(&self.data[source..source + len]);
        }
        out
    }

    /// All pixels, BGRX, row after row.
    pub fn pixels(&self) -> &[u8] {
        &self.data
    }

    pub fn full_rect(&self) -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: self.width,
            h: self.height,
        }
    }

    /// Copies BGRX pixels into `rect`. The rect must be inside the screen.
    pub fn write_region(&mut self, rect: Rect, pixels: &[u8]) -> Result<(), RegionError> {
        self.check_inside(rect)?;
        if rect.pixel_bytes() != Some(pixels.len()) {
            return Err(RegionError::Length);
        }
        self.write_part(rect, pixels, rect);
        Ok(())
    }

    /// Copies `part` of `rect`, whose BGRX pixels are `pixels`. The caller has checked `rect`
    /// and `pixels` (with `changed_bands`), and `part` is inside `rect`.
    pub(crate) fn write_part(&mut self, rect: Rect, pixels: &[u8], part: Rect) {
        let stride = rect.w as usize * BYTES_PER_PIXEL;
        let row_bytes = part.w as usize * BYTES_PER_PIXEL;
        let left = (part.x - rect.x) as usize * BYTES_PER_PIXEL;
        let rows = pixels
            .chunks_exact(stride)
            .skip((part.y - rect.y) as usize)
            .take(part.h as usize);
        for (row, source) in rows.enumerate() {
            let start = self.offset(part.x, part.y + row as u16);
            self.data[start..start + row_bytes].copy_from_slice(&source[left..left + row_bytes]);
        }
        if part == self.full_rect() {
            self.unsent = false;
        }
    }

    /// The parts of `rect` where `pixels` differ from the screen, as bands that do not overlap.
    /// A band holds changed rows that are at most `BAND_GAP` rows apart and whose changed
    /// columns are about the same (`COLUMN_GAP`), cut to those columns. So a change in the shape
    /// of an L is two bands, and the unchanged pixels in its corner are not sent. A large band
    /// around an unchanged hole (a new root background around a window) is then cut around the
    /// hole (`split_around_holes`).
    pub fn changed_bands(&self, rect: Rect, pixels: &[u8]) -> Result<Vec<Rect>, RegionError> {
        self.check_inside(rect)?;
        if rect.pixel_bytes() != Some(pixels.len()) {
            return Err(RegionError::Length);
        }
        if self.unsent {
            return Ok(vec![rect]);
        }
        let mut bands = self.group_rows(rect, pixels, rect, false);
        if bands.len() > MAX_BANDS {
            bands = join_close_rows(bands);
        }
        Ok(bands
            .into_iter()
            .flat_map(|band| self.split_around_holes(rect, pixels, band))
            .collect())
    }

    /// The bands of the changed rows of `area`, a part of `rect`. With `split`, a row gives one
    /// run of changed columns for each part between long unchanged spans (`RUN_GAP_PIXELS`), and
    /// rows with alike runs make one band for each run; without it, one run from the first to
    /// the last changed column.
    fn group_rows(&self, rect: Rect, pixels: &[u8], area: Rect, split: bool) -> Vec<Rect> {
        let stride = rect.w as usize * BYTES_PER_PIXEL;
        let left = (area.x - rect.x) as usize * BYTES_PER_PIXEL;
        let row_bytes = area.w as usize * BYTES_PER_PIXEL;
        let mut bands: Vec<Rect> = Vec::new();
        // The bands that the rows above are still growing, one for each run, from the left.
        let mut open: Vec<Rect> = Vec::new();
        let mut runs: Vec<(u16, u16)> = Vec::new();
        let rows = pixels
            .chunks_exact(stride)
            .skip((area.y - rect.y) as usize)
            .take(area.h as usize);
        for (row, new) in rows.enumerate() {
            let y = area.y + row as u16;
            let start = self.offset(area.x, y);
            let (old, new) = (
                &self.data[start..start + row_bytes],
                &new[left..left + row_bytes],
            );
            if split {
                changed_runs(old, new, &mut runs);
            } else {
                runs.clear();
                runs.extend(changed_columns(old, new));
            }
            if runs.is_empty() {
                continue;
            }
            let changed = runs.iter().map(|&(first, last)| Rect {
                x: area.x + first,
                y,
                w: last + 1 - first,
                h: 1,
            });
            let alike = open.len() == runs.len()
                && open
                    .iter()
                    .zip(changed.clone())
                    .all(|(band, row)| rows_are_close(band, row) && columns_are_alike(band, row));
            if alike {
                for (band, row) in open.iter_mut().zip(changed) {
                    *band = union(*band, row);
                }
            } else {
                bands.append(&mut open);
                open.extend(changed);
            }
        }
        bands.append(&mut open);
        bands
    }

    /// `band` cut around the unchanged holes in it, when that gives a few tall pieces; else
    /// `band` itself. Text gives many thin pieces (the rows of a glyph line differ), and those
    /// cost more in headers than the unchanged pixels between them.
    fn split_around_holes(&self, rect: Rect, pixels: &[u8], band: Rect) -> Vec<Rect> {
        if band.h < MIN_SPLIT_ROWS
            || usize::from(band.w) < 2 * RUN_GAP_PIXELS
            || !self.sampled_rows_have_holes(rect, pixels, band)
        {
            return vec![band];
        }
        let pieces = self.group_rows(rect, pixels, band, true);
        let tall = pieces.iter().all(|piece| piece.h >= MIN_PIECE_ROWS);
        if pieces.len() > 1 && pieces.len() <= MAX_PIECES && tall {
            pieces
        } else {
            vec![band]
        }
    }

    /// True when one of `HOLE_SAMPLES` rows spread over `band` has an unchanged span between its
    /// changes. A changed photo has none; testing a few rows first spares it a second full pass.
    fn sampled_rows_have_holes(&self, rect: Rect, pixels: &[u8], band: Rect) -> bool {
        let stride = rect.w as usize * BYTES_PER_PIXEL;
        let left = (band.x - rect.x) as usize * BYTES_PER_PIXEL;
        let row_bytes = band.w as usize * BYTES_PER_PIXEL;
        let step = band.h / HOLE_SAMPLES;
        let mut runs = Vec::new();
        (0..HOLE_SAMPLES).any(|index| {
            let y = band.y + step / 2 + index * step;
            let new = &pixels[(y - rect.y) as usize * stride + left..][..row_bytes];
            let start = self.offset(band.x, y);
            changed_runs(&self.data[start..start + row_bytes], new, &mut runs);
            runs.len() > 1
        })
    }

    /// Moves the pixels of `from` so that its top-left corner is at `to`. The areas may overlap.
    pub fn copy_rect(&mut self, from: Rect, to: (u16, u16)) -> Result<(), RegionError> {
        self.check_inside(from)?;
        self.check_inside(Rect {
            x: to.0,
            y: to.1,
            ..from
        })?;
        let row_bytes = from.w as usize * BYTES_PER_PIXEL;
        let mut copy_row = |row: u16| {
            let source = self.offset(from.x, from.y + row);
            let target = self.offset(to.0, to.1 + row);
            self.data.copy_within(source..source + row_bytes, target);
        };
        // Rows that move down are copied from the bottom, so a row is read before it is written.
        if to.1 > from.y {
            (0..from.h).rev().for_each(&mut copy_row);
        } else {
            (0..from.h).for_each(&mut copy_row);
        }
        Ok(())
    }

    fn check_inside(&self, rect: Rect) -> Result<(), RegionError> {
        let right = u32::from(rect.x) + u32::from(rect.w);
        let bottom = u32::from(rect.y) + u32::from(rect.h);
        if rect.w == 0 || rect.h == 0 || right > self.width.into() || bottom > self.height.into() {
            return Err(RegionError::OutOfBounds);
        }
        Ok(())
    }

    fn offset(&self, x: u16, y: u16) -> usize {
        (y as usize * self.width as usize + x as usize) * BYTES_PER_PIXEL
    }
}

/// Unchanged rows between two changed rows that still go in one tile. A tile header and a
/// compressed frame cost more than a few rows that did not change.
const BAND_GAP: u16 = 4;
/// How far the first or the last changed column of a row may be from those of its band.
const COLUMN_GAP: u16 = 64;
/// More bands than this for one update are joined by their rows only: many small tiles cost
/// more than one tile with pixels that did not change.
const MAX_BANDS: usize = 16;
/// A band is cut around an unchanged hole only when it has this many rows, the cut gives at
/// most `MAX_PIECES` pieces, and each piece has at least `MIN_PIECE_ROWS` rows.
const MIN_SPLIT_ROWS: u16 = 16;
/// Rows of a band that are tested for a hole before the band is cut around holes.
const HOLE_SAMPLES: u16 = 8;
const MAX_PIECES: usize = 8;
const MIN_PIECE_ROWS: u16 = 8;

fn rows_are_close(band: &Rect, below: Rect) -> bool {
    below.y - (band.y + band.h) <= BAND_GAP
}

fn columns_are_alike(band: &Rect, row: Rect) -> bool {
    band.x.abs_diff(row.x) <= COLUMN_GAP && (band.x + band.w).abs_diff(row.x + row.w) <= COLUMN_GAP
}

/// The smallest rect that holds `band` and `below`, which starts under the top of `band`.
fn union(band: Rect, below: Rect) -> Rect {
    let left = band.x.min(below.x);
    let right = (band.x + band.w).max(below.x + below.w);
    Rect {
        x: left,
        y: band.y,
        w: right - left,
        h: below.y + below.h - band.y,
    }
}

/// Joins bands, which are in order from the top, when their rows are close.
fn join_close_rows(bands: Vec<Rect>) -> Vec<Rect> {
    let mut joined: Vec<Rect> = Vec::new();
    for band in bands {
        match joined.last_mut() {
            Some(last) if rows_are_close(last, band) => *last = union(*last, band),
            _ => joined.push(band),
        }
    }
    joined
}

/// Unchanged pixels between two changes of a row that split it into two runs. Shorter gaps stay
/// in the run: a tile header costs more than a few unchanged pixels.
const RUN_GAP_PIXELS: usize = 64;
/// The rows are compared in blocks of this many pixels inside the changed columns.
const BLOCK_PIXELS: usize = 16;

/// The runs of changed columns of a row, (first, last), from the left, exact to the pixel. A run
/// ends where at least `RUN_GAP_PIXELS` unchanged pixels follow. The row is compared in blocks;
/// only the blocks at the ends of a run are looked at pixel by pixel.
fn changed_runs(old: &[u8], new: &[u8], runs: &mut Vec<(u16, u16)>) {
    runs.clear();
    let Some((first, last)) = changed_columns(old, new) else {
        return;
    };
    let (first, last) = (usize::from(first), usize::from(last));
    let span = |from: usize, to: usize| from * BYTES_PER_PIXEL..to * BYTES_PER_PIXEL;
    let mut run_start = first;
    // The last block with a change, and the unchanged pixels after it.
    let mut changed_block = (first, first);
    let mut unchanged = 0;
    let mut x = first;
    while x <= last {
        let end = (x + BLOCK_PIXELS).min(last + 1);
        if old[span(x, end)] == new[span(x, end)] {
            unchanged += end - x;
        } else {
            if unchanged >= RUN_GAP_PIXELS {
                let (from, to) = changed_block;
                let (_, run_end) = changed_columns(&old[span(from, to)], &new[span(from, to)])
                    .expect("the block has a change");
                runs.push((run_start as u16, (from + usize::from(run_end)) as u16));
                let (start, _) = changed_columns(&old[span(x, end)], &new[span(x, end)])
                    .expect("the block has a change");
                run_start = x + usize::from(start);
            }
            unchanged = 0;
            changed_block = (x, end);
        }
        x = end;
    }
    runs.push((run_start as u16, last as u16));
}

/// The first and the last pixel that differ between two rows of the same length.
fn changed_columns(old: &[u8], new: &[u8]) -> Option<(u16, u16)> {
    if old == new {
        return None;
    }
    let differs = |(a, b): (&[u8; BYTES_PER_PIXEL], &[u8; BYTES_PER_PIXEL])| a != b;
    let pairs = || old.as_chunks().0.iter().zip(new.as_chunks().0);
    let first = pairs().position(differs)?;
    let last = pairs().rposition(differs)?;
    Some((first as u16, last as u16))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn the_same_pixels_have_no_changed_band() {
        let mut fb = Framebuffer::new(8, 8);
        let pixels = [3, 4, 5, 0].repeat(16);
        fb.write_region(rect(2, 2, 4, 4), &pixels).unwrap();
        assert_eq!(fb.changed_bands(rect(2, 2, 4, 4), &pixels), Ok(vec![]));
    }

    #[test]
    fn a_change_gives_a_band_cut_to_the_changed_pixels() {
        let fb = Framebuffer::new(16, 16);
        let mut pixels = vec![0; 16 * 16 * 4];
        let mut set = |x: usize, y: usize| pixels[(y * 16 + x) * 4] = 9;
        set(5, 3);
        set(9, 5);
        set(2, 14);
        assert_eq!(
            fb.changed_bands(fb.full_rect(), &pixels),
            Ok(vec![rect(5, 3, 5, 3), rect(2, 14, 1, 1)]),
            "rows 3 and 5 are close, row 14 is far"
        );
    }

    #[test]
    fn a_change_in_the_shape_of_an_l_gives_two_bands() {
        let fb = Framebuffer::new(400, 40);
        let mut pixels = vec![0; 400 * 40 * 4];
        // A strip at the right of rows 0 to 29, then rows 30 to 39 in full.
        for y in 0..40 {
            let columns = if y < 30 { 300..400 } else { 0..400 };
            for x in columns {
                pixels[(y * 400 + x) * 4] = 9;
            }
        }
        assert_eq!(
            fb.changed_bands(fb.full_rect(), &pixels),
            Ok(vec![rect(300, 0, 100, 30), rect(0, 30, 400, 10)]),
            "the unchanged corner is in no band"
        );
    }

    #[test]
    fn a_change_around_an_unchanged_window_leaves_the_window_out() {
        // A new background on a 400x60 screen with an unchanged 200x20 window at (100, 20).
        let fb = Framebuffer::new(400, 60);
        let mut pixels = vec![9; 400 * 60 * 4];
        for y in 20..40 {
            for x in 100..300 {
                pixels[(y * 400 + x) * 4..][..4].fill(0);
            }
        }
        assert_eq!(
            fb.changed_bands(fb.full_rect(), &pixels),
            Ok(vec![
                rect(0, 0, 400, 20),
                rect(0, 20, 100, 20),
                rect(300, 20, 100, 20),
                rect(0, 40, 400, 20),
            ])
        );
    }

    #[test]
    fn a_short_unchanged_gap_stays_in_its_run() {
        let mut runs = Vec::new();
        let old = vec![0; 300 * 4];
        let mut new = old.clone();
        for x in [10, 50, 200] {
            new[x * 4] = 1;
        }
        changed_runs(&old, &new, &mut runs);
        // 10 and 50 are 40 pixels apart: one run. 200 is far: a second run.
        assert_eq!(runs, [(10, 50), (200, 200)]);
        changed_runs(&old, &old, &mut runs);
        assert!(runs.is_empty());
    }

    #[test]
    fn too_many_bands_are_joined_by_their_rows() {
        let fb = Framebuffer::new(400, 80);
        let mut pixels = vec![0; 400 * 80 * 4];
        // One pixel in each row, at the left and at the right in turn.
        for y in 0..80 {
            let x = if y % 2 == 0 { 0 } else { 399 };
            pixels[(y * 400 + x) * 4] = 9;
        }
        assert_eq!(
            fb.changed_bands(fb.full_rect(), &pixels),
            Ok(vec![rect(0, 0, 400, 80)])
        );
    }

    #[test]
    fn copy_rect_moves_overlapping_rows_up_and_down() {
        let mut fb = Framebuffer::new(2, 4);
        let rows: Vec<u8> = (1..=4)
            .flat_map(|row| [row, 0, 0, 0, row, 0, 0, 0])
            .collect();
        fb.write_region(fb.full_rect(), &rows).unwrap();
        let first_column =
            |fb: &Framebuffer| -> Vec<u8> { fb.data.iter().step_by(8).copied().collect() };
        fb.copy_rect(rect(0, 1, 2, 3), (0, 0)).unwrap();
        assert_eq!(first_column(&fb), [2, 3, 4, 4]);
        fb.copy_rect(rect(0, 0, 2, 3), (0, 1)).unwrap();
        assert_eq!(first_column(&fb), [2, 2, 3, 4]);
        assert_eq!(
            fb.copy_rect(rect(0, 1, 2, 3), (0, 2)),
            Err(RegionError::OutOfBounds)
        );
    }

    #[test]
    fn an_unsent_screen_sends_all_until_the_whole_screen_is_written() {
        let mut fb = Framebuffer::new_unsent(4, 4);
        let black = [0; 2 * 2 * 4];
        assert_eq!(
            fb.changed_bands(rect(1, 1, 2, 2), &black),
            Ok(vec![rect(1, 1, 2, 2)])
        );
        fb.write_region(rect(1, 1, 2, 2), &black).unwrap();
        assert!(fb.is_unsent(), "a part of the screen is not enough");
        fb.write_region(fb.full_rect(), &[0; 4 * 4 * 4]).unwrap();
        assert!(!fb.is_unsent());
        assert_eq!(fb.changed_bands(rect(1, 1, 2, 2), &black), Ok(vec![]));
    }

    #[test]
    fn changed_bands_checks_the_rect() {
        let fb = Framebuffer::new(8, 8);
        assert_eq!(
            fb.changed_bands(rect(7, 0, 2, 1), &[0; 8]),
            Err(RegionError::OutOfBounds)
        );
        assert_eq!(
            fb.changed_bands(rect(0, 0, 2, 1), &[0; 4]),
            Err(RegionError::Length)
        );
    }

    #[test]
    fn written_region_comes_back_when_it_is_read() {
        let mut fb = Framebuffer::new(8, 8);
        let area = rect(2, 3, 2, 2);
        fb.write_region(area, &[9, 8, 7, 0].repeat(4)).unwrap();
        assert_eq!(fb.read_region(2, 3, 2, 2), [9, 8, 7, 0].repeat(4));
    }

    #[test]
    fn rows_land_in_the_right_place() {
        let mut fb = Framebuffer::new(4, 2);
        fb.write_region(rect(1, 1, 2, 1), &[1, 1, 1, 0, 2, 2, 2, 0])
            .unwrap();
        let pixels = fb.pixels();
        assert_eq!(&pixels[(4 + 1) * 4..(4 + 3) * 4], [1, 1, 1, 0, 2, 2, 2, 0]);
        assert!(pixels[..4 * 4].iter().all(|&b| b == 0));
    }

    #[test]
    fn rejects_regions_outside_or_with_wrong_length() {
        let mut fb = Framebuffer::new(8, 8);
        assert_eq!(
            fb.write_region(rect(7, 0, 2, 1), &[0; 8]),
            Err(RegionError::OutOfBounds)
        );
        assert_eq!(
            fb.write_region(rect(0, 0, 2, 1), &[0; 4]),
            Err(RegionError::Length)
        );
    }
}
