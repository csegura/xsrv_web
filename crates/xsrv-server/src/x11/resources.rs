// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

use std::rc::Rc;
use wire::BYTES_PER_PIXEL;

use super::screen::ROOT_DEPTH;

pub const PIXEL_MASK: u32 = 0x00FF_FFFF;
/// The depths that a pixmap may have.
pub const PIXMAP_DEPTHS: [u8; 4] = [1, 8, ROOT_DEPTH, 32];

/// The bits of a pixel value that a surface of this depth keeps. A depth-1 pixel is 0 or
/// `PIXEL_MASK` (see `Gc::on_bitmap`).
pub fn pixel_mask_for_depth(depth: u8) -> u32 {
    match depth {
        32 => u32::MAX,
        8 => 0xFF,
        _ => PIXEL_MASK,
    }
}
pub const FUNCTION_COPY: u8 = 3;
pub const CAP_BUTT: u8 = 1;
/// The pixels of one cell of `Surface::draw_cells`: the bits of one byte.
pub const CELL_WIDTH: usize = 8;
const HALF_CELL: usize = CELL_WIDTH / 2;
type Pixel = [u8; BYTES_PER_PIXEL];

/// The pixels of a row are compared in blocks of this many, so that the test has no branch for
/// each pixel.
const COMPARE_BLOCK: usize = 16;

/// The columns of `old` from the first to the last block where `new`, masked, has other pixels.
fn changed_span(old: &[Pixel], new: &[Pixel], mask: u32) -> Option<(usize, usize)> {
    let differs = |(old, new): (&[Pixel], &[Pixel])| {
        let pairs = old.iter().zip(new);
        pairs.fold(0, |all, (old, new)| {
            all | (u32::from_le_bytes(*old) ^ (u32::from_le_bytes(*new) & mask))
        }) != 0
    };
    let blocks = || old.chunks(COMPARE_BLOCK).zip(new.chunks(COMPARE_BLOCK));
    let first = blocks().position(differs)?;
    let last = blocks().rposition(differs)?;
    Some((
        first * COMPARE_BLOCK,
        ((last + 1) * COMPARE_BLOCK).min(old.len()),
    ))
}

/// Makes the span `columns` hold `start..end` too.
fn widen(columns: &mut Option<(usize, usize)>, start: usize, end: usize) {
    *columns = Some(columns.map_or((start, end), |(left, right)| {
        (left.min(start), right.max(end))
    }));
}

/// The pixels of half a cell for each value of its four bits: `set` for a 1, `clear` for a 0.
/// A cell row is then two copies, where a test of each bit took most of the time of a redraw.
fn half_cells(set: Pixel, clear: Pixel) -> [[Pixel; HALF_CELL]; 1 << HALF_CELL] {
    std::array::from_fn(|bits| {
        std::array::from_fn(|place| if bits >> place & 1 == 1 { set } else { clear })
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IRect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl IRect {
    pub fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x0: x,
            y0: y,
            x1: x + width,
            y1: y + height,
        }
    }

    pub fn width(self) -> i32 {
        self.x1 - self.x0
    }

    pub fn height(self) -> i32 {
        self.y1 - self.y0
    }

    pub fn contains(self, x: i32, y: i32) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }

    pub fn grow(self, by: i32) -> Self {
        Self {
            x0: self.x0 - by,
            y0: self.y0 - by,
            x1: self.x1 + by,
            y1: self.y1 + by,
        }
    }

    pub fn translate(self, dx: i32, dy: i32) -> Self {
        Self {
            x0: self.x0 + dx,
            y0: self.y0 + dy,
            x1: self.x1 + dx,
            y1: self.y1 + dy,
        }
    }

    /// `None` when the rects share no pixel.
    pub fn intersect(self, other: Self) -> Option<Self> {
        let rect = Self {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        };
        (rect.x0 < rect.x1 && rect.y0 < rect.y1).then_some(rect)
    }

    pub fn union(self, other: Self) -> Self {
        Self {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }
}

/// A raster of pixel values, each a little-endian u32 (4 bytes). The depth says which bits count:
/// 24 is BGRX with the fourth byte zero, 32 is BGRA, 8 keeps the value in the first byte, and a
/// depth-1 pixel is 0 or all ones.
#[derive(Clone)]
pub struct Surface {
    width: u16,
    height: u16,
    pixels: Vec<u8>,
    depth: u8,
    /// While it is set, drawing only reaches these rects. A drawing request sets it for its GC.
    clip: Option<Vec<IRect>>,
    /// The pixels that drawing may still write. A request sets it, and drawing spends it.
    work_left: u64,
    work_exhausted: bool,
}

impl Surface {
    pub fn empty() -> Self {
        Self {
            width: 0,
            height: 0,
            pixels: Vec::new(),
            depth: 0,
            clip: None,
            work_left: u64::MAX,
            work_exhausted: false,
        }
    }

    pub fn byte_len(width: u16, height: u16) -> Option<usize> {
        usize::from(width)
            .checked_mul(usize::from(height))?
            .checked_mul(BYTES_PER_PIXEL)
    }

    /// The caller checks the memory budget with `byte_len` first.
    pub fn new(width: u16, height: u16) -> Self {
        let len = Self::byte_len(width, height).unwrap_or(0);
        Self {
            width,
            height,
            pixels: vec![0; len],
            depth: ROOT_DEPTH,
            clip: None,
            work_left: u64::MAX,
            work_exhausted: false,
        }
    }

    pub fn same_pixels(&self, other: &Surface) -> bool {
        self.depth == other.depth && self.width == other.width && self.pixels == other.pixels
    }

    pub fn depth(&self) -> u8 {
        self.depth
    }

    pub fn set_depth(&mut self, depth: u8) {
        self.depth = depth;
    }

    pub fn pixel_mask(&self) -> u32 {
        pixel_mask_for_depth(self.depth)
    }

    pub fn set_clip(&mut self, clip: Option<Vec<IRect>>) {
        self.clip = clip;
    }

    /// Ends the clip and gives its rectangles back.
    pub fn take_clip(&mut self) -> Option<Vec<IRect>> {
        self.clip.take()
    }

    /// Limits what the next drawing may write. `finish_work` ends the limit.
    pub fn start_work(&mut self, pixels: u64) {
        self.work_left = pixels;
        self.work_exhausted = false;
    }

    /// Returns true when the drawing wanted more than its limit. The excess was not drawn.
    pub fn finish_work(&mut self) -> bool {
        self.work_left = u64::MAX;
        std::mem::take(&mut self.work_exhausted)
    }

    fn spend(&mut self, area: IRect) -> bool {
        let cost = area.width() as u64 * area.height() as u64;
        if cost > self.work_left {
            self.work_left = 0;
            self.work_exhausted = true;
            return false;
        }
        self.work_left -= cost;
        true
    }

    pub fn len(&self) -> usize {
        self.pixels.len()
    }

    pub fn bounds(&self) -> IRect {
        IRect::new(0, 0, self.width.into(), self.height.into())
    }

    #[cfg(test)]
    pub fn fill_all(&mut self, pixel: u32) {
        let bytes = pixel_bytes(pixel & self.pixel_mask());
        for chunk in self.pixels.as_chunks_mut::<BYTES_PER_PIXEL>().0 {
            *chunk = bytes;
        }
    }

    pub fn fill_rect(&mut self, area: IRect, pixel: u32, function: u8, plane_mask: u32) {
        let Some(clip) = self.clip.take() else {
            return self.fill_part(area, pixel, function, plane_mask);
        };
        for limit in &clip {
            if let Some(part) = area.intersect(*limit) {
                self.fill_part(part, pixel, function, plane_mask);
            }
        }
        self.clip = Some(clip);
    }

    /// True when a write with this function and plane mask stores the new pixel as it is.
    pub fn writes_plainly(&self, function: u8, plane_mask: u32) -> bool {
        let mask = self.pixel_mask();
        function == FUNCTION_COPY && plane_mask & mask == mask
    }

    /// Draws cells of `CELL_WIDTH` pixels side by side over `area`, as a plain copy.
    /// `cell_bits(cell, row)` gives the pixels of one cell in one row, both counted from the
    /// corner of `area`: bit 0 is the leftmost pixel. A set bit gives `set`. A clear bit gives
    /// `clear`, or leaves the pixel when there is no `clear`. Text uses it: one pass over the
    /// rows of the surface, where a fill for each run of glyph pixels took most of the time of
    /// a redraw.
    ///
    /// Returns a rect around the cells that got other pixels, or `None` when the surface already
    /// had all of them: text that is drawn again as it was needs no work for the screen.
    pub fn draw_cells(
        &mut self,
        area: IRect,
        set: u32,
        clear: Option<u32>,
        cell_bits: impl Fn(usize, usize) -> u8,
    ) -> Option<IRect> {
        let Some(clip) = self.clip.take() else {
            return self.draw_cells_part(area, area, (set, clear), &cell_bits);
        };
        let mut changed: Option<IRect> = None;
        for limit in &clip {
            let drawn = area
                .intersect(*limit)
                .and_then(|part| self.draw_cells_part(area, part, (set, clear), &cell_bits));
            changed = match (changed, drawn) {
                (Some(a), Some(b)) => Some(a.union(b)),
                (a, b) => a.or(b),
            };
        }
        self.clip = Some(clip);
        changed
    }

    fn draw_cells_part(
        &mut self,
        area: IRect,
        part: IRect,
        (set, clear): (u32, Option<u32>),
        cell_bits: &impl Fn(usize, usize) -> u8,
    ) -> Option<IRect> {
        let part = part.intersect(self.bounds())?;
        if !self.spend(part) {
            return None;
        }
        let mask = self.pixel_mask();
        let set = pixel_bytes(set & mask);
        let halves = clear.map(|clear| half_cells(set, pixel_bytes(clear & mask)));
        // Returns true when the cell may have other pixels than before.
        let put = |pixels: &mut [Pixel], bits: u8| match &halves {
            Some(halves) => {
                let new_low = &halves[usize::from(bits & 0x0F)];
                let new_high = &halves[usize::from(bits >> HALF_CELL)];
                // A whole cell is two compares of a fixed size; a cut cell goes by its length.
                if let ([low, high], []) = pixels.as_chunks_mut::<HALF_CELL>() {
                    let changed = low != new_low || high != new_high;
                    if changed {
                        (*low, *high) = (*new_low, *new_high);
                    }
                    return changed;
                }
                let (low, high) = pixels.split_at_mut(pixels.len().min(HALF_CELL));
                let (new_low, new_high) = (&new_low[..low.len()], &new_high[..high.len()]);
                let changed = low != new_low || high != new_high;
                if changed {
                    low.copy_from_slice(new_low);
                    high.copy_from_slice(new_high);
                }
                changed
            }
            None => {
                for (place, pixel) in pixels.iter_mut().enumerate() {
                    if bits >> place & 1 == 1 {
                        *pixel = set;
                    }
                }
                bits != 0
            }
        };
        // The part can start in the middle of a cell. After that, each cell is whole, but the
        // last one may be cut.
        let first = (part.x0 - area.x0) as usize;
        let (first_cell, skipped) = (first / CELL_WIDTH, first % CELL_WIDTH);
        // The changed pixels: columns counted from the left of the part, and rows.
        let mut columns: Option<(usize, usize)> = None;
        let mut rows: Option<(i32, i32)> = None;
        for y in part.y0..part.y1 {
            let row = (y - area.y0) as usize;
            let pixels = self
                .row_mut(y, part.x0, part.x1)
                .as_chunks_mut::<BYTES_PER_PIXEL>()
                .0;
            let lead = (CELL_WIDTH - skipped).min(pixels.len());
            let (head, rest) = pixels.split_at_mut(lead);
            let mut row_changed = false;
            if put(head, cell_bits(first_cell, row) >> skipped) {
                widen(&mut columns, 0, lead);
                row_changed = true;
            }
            for (cell, pixels) in rest.chunks_mut(CELL_WIDTH).enumerate() {
                let start = lead + cell * CELL_WIDTH;
                let end = start + pixels.len();
                if put(pixels, cell_bits(first_cell + 1 + cell, row)) {
                    widen(&mut columns, start, end);
                    row_changed = true;
                }
            }
            if row_changed {
                rows = Some((rows.map_or(y, |(top, _)| top), y + 1));
            }
        }
        let ((left, right), (top, bottom)) = (columns?, rows?);
        Some(IRect {
            x0: part.x0 + left as i32,
            y0: top,
            x1: part.x0 + right as i32,
            y1: bottom,
        })
    }

    fn fill_part(&mut self, area: IRect, pixel: u32, function: u8, plane_mask: u32) {
        let Some(area) = area.intersect(self.bounds()) else {
            return;
        };
        if !self.spend(area) {
            return;
        }
        let mask = self.pixel_mask();
        let plain = self.writes_plainly(function, plane_mask);
        for y in area.y0..area.y1 {
            let row = self.row_mut(y, area.x0, area.x1);
            if plain {
                for chunk in row.as_chunks_mut::<BYTES_PER_PIXEL>().0 {
                    *chunk = pixel_bytes(pixel & mask);
                }
            } else {
                for chunk in row.as_chunks_mut::<BYTES_PER_PIXEL>().0 {
                    let old = u32::from_le_bytes(*chunk) & mask;
                    let new = combine(function, pixel & mask, old);
                    *chunk = pixel_bytes(((old & !plane_mask) | (new & plane_mask)) & mask);
                }
            }
        }
    }

    /// Copies the pixels of `rect` out. The rect must be inside the surface.
    pub fn read_rect(&self, rect: IRect) -> Vec<u8> {
        let mut data =
            Vec::with_capacity(rect.width() as usize * rect.height() as usize * BYTES_PER_PIXEL);
        for y in rect.y0..rect.y1 {
            data.extend_from_slice(self.row(y, rect.x0, rect.x1));
        }
        data
    }

    /// Draws a `width` x `height` block of BGRX `data` at (`x`, `y`) and clips it to the surface.
    /// Returns the part that changed. `data` must hold exactly `width * height` pixels.
    pub fn write_rect(
        &mut self,
        origin: (i32, i32),
        size: (i32, i32),
        data: &[u8],
        function: u8,
        plane_mask: u32,
    ) -> Option<IRect> {
        let block = Block::Data {
            data,
            stride: size.0 as usize * BYTES_PER_PIXEL,
        };
        self.write_block(origin, size, &block, function, plane_mask)
    }

    /// Draws the area `from` of another surface of the same depth at `origin`, as `write_rect`
    /// does with a block of pixels. `from` must be inside `source`.
    pub fn copy_from(
        &mut self,
        source: &Surface,
        from: IRect,
        origin: (i32, i32),
        function: u8,
        plane_mask: u32,
    ) -> Option<IRect> {
        let size = (from.width(), from.height());
        let block = Block::Area { source, from };
        self.write_block(origin, size, &block, function, plane_mask)
    }

    /// Moves the pixels of `from`, which is inside the surface, by `shift`: a plain copy that
    /// ignores the clip. The two areas may overlap. Returns the part that changed.
    pub fn copy_inside(&mut self, from: IRect, shift: (i32, i32)) -> Option<IRect> {
        let target = from.translate(shift.0, shift.1).intersect(self.bounds())?;
        if !self.spend(target) {
            return None;
        }
        let len = target.width() as usize * BYTES_PER_PIXEL;
        let mut copy_row = |y: i32| {
            let source = self.offset(y - shift.1, target.x0 - shift.0);
            let destination = self.offset(y, target.x0);
            self.pixels.copy_within(source..source + len, destination);
        };
        // Rows that move down are copied from the bottom, so a row is read before it is written.
        if shift.1 > 0 {
            (target.y0..target.y1).rev().for_each(&mut copy_row);
        } else {
            (target.y0..target.y1).for_each(&mut copy_row);
        }
        Some(target)
    }

    fn write_block(
        &mut self,
        origin: (i32, i32),
        size: (i32, i32),
        block: &Block,
        function: u8,
        plane_mask: u32,
    ) -> Option<IRect> {
        let Some(clip) = self.clip.take() else {
            return self.write_part(origin, size, block, function, plane_mask, None);
        };
        let changed = clip
            .iter()
            .filter_map(|limit| {
                self.write_part(origin, size, block, function, plane_mask, Some(*limit))
            })
            .reduce(IRect::union);
        self.clip = Some(clip);
        changed
    }

    /// `limit` keeps the write inside one clip rect.
    fn write_part(
        &mut self,
        (x, y): (i32, i32),
        (width, height): (i32, i32),
        block: &Block,
        function: u8,
        plane_mask: u32,
        limit: Option<IRect>,
    ) -> Option<IRect> {
        let mut area = IRect::new(x, y, width, height).intersect(self.bounds())?;
        if let Some(limit) = limit {
            area = area.intersect(limit)?;
        }
        if !self.spend(area) {
            return None;
        }
        let mask = self.pixel_mask();
        let (left, columns) = ((area.x0 - x) as usize, area.width() as usize);
        if self.writes_plainly(function, plane_mask) {
            return self.write_changed(area, mask, |row| {
                block.row((row - y) as usize, left, columns)
            });
        }
        for row_y in area.y0..area.y1 {
            let source = block.row((row_y - y) as usize, left, columns);
            let target = self.row_mut(row_y, area.x0, area.x1);
            for (out, src) in target
                .as_chunks_mut::<BYTES_PER_PIXEL>()
                .0
                .iter_mut()
                .zip(source.as_chunks::<BYTES_PER_PIXEL>().0)
            {
                let old = u32::from_le_bytes(*out) & mask;
                let new = combine(function, u32::from_le_bytes(*src) & mask, old);
                *out = pixel_bytes(((old & !plane_mask) | (new & plane_mask)) & mask);
            }
        }
        Some(area)
    }

    /// A plain copy of rows into `area`, which is inside the surface. Only the part of a row
    /// from its first to its last changed block is written. Returns a rect around the written
    /// parts, or `None` when the surface already had every pixel: a toolkit that sends its whole
    /// window again for a small change gives the screen only that change to compose.
    fn write_changed<'a>(
        &mut self,
        area: IRect,
        mask: u32,
        source_row: impl Fn(i32) -> &'a [u8],
    ) -> Option<IRect> {
        let mut columns: Option<(usize, usize)> = None;
        let mut rows: Option<(i32, i32)> = None;
        for y in area.y0..area.y1 {
            let source = source_row(y).as_chunks::<BYTES_PER_PIXEL>().0;
            let target = self
                .row_mut(y, area.x0, area.x1)
                .as_chunks_mut::<BYTES_PER_PIXEL>()
                .0;
            let Some((start, end)) = changed_span(target, source, mask) else {
                continue;
            };
            for (out, new) in target[start..end].iter_mut().zip(&source[start..end]) {
                *out = pixel_bytes(u32::from_le_bytes(*new) & mask);
            }
            widen(&mut columns, start, end);
            rows = Some((rows.map_or(y, |(top, _)| top), y + 1));
        }
        let ((left, right), (top, bottom)) = (columns?, rows?);
        Some(IRect {
            x0: area.x0 + left as i32,
            y0: top,
            x1: area.x0 + right as i32,
            y1: bottom,
        })
    }

    /// `y`, `x0` and `x1` must be inside the surface.
    pub fn row(&self, y: i32, x0: i32, x1: i32) -> &[u8] {
        let start = self.offset(y, x0);
        &self.pixels[start..start + (x1 - x0) as usize * BYTES_PER_PIXEL]
    }

    pub(super) fn row_mut(&mut self, y: i32, x0: i32, x1: i32) -> &mut [u8] {
        let start = self.offset(y, x0);
        &mut self.pixels[start..start + (x1 - x0) as usize * BYTES_PER_PIXEL]
    }

    fn offset(&self, y: i32, x: i32) -> usize {
        (y as usize * self.width as usize + x as usize) * BYTES_PER_PIXEL
    }
}

/// The pixels that a surface draws from: rows of BGRX bytes, or an area of another surface.
enum Block<'a> {
    Data { data: &'a [u8], stride: usize },
    Area { source: &'a Surface, from: IRect },
}

impl Block<'_> {
    /// `columns` pixels of the row `row`, from the column `left` on.
    fn row(&self, row: usize, left: usize, columns: usize) -> &[u8] {
        match self {
            Self::Data { data, stride } => {
                let start = row * stride + left * BYTES_PER_PIXEL;
                &data[start..start + columns * BYTES_PER_PIXEL]
            }
            Self::Area { source, from } => {
                let x0 = from.x0 + left as i32;
                source.row(from.y0 + row as i32, x0, x0 + columns as i32)
            }
        }
    }
}

/// The stored bytes of a pixel value. A surface masks the value by its depth when it writes.
pub fn pixel_bytes(pixel: u32) -> [u8; BYTES_PER_PIXEL] {
    pixel.to_le_bytes()
}

/// The 16 X11 raster operations (GXclear to GXset).
fn combine(function: u8, src: u32, dst: u32) -> u32 {
    match function {
        0 => 0,
        1 => src & dst,
        2 => src & !dst,
        3 => src,
        4 => !src & dst,
        5 => dst,
        6 => src ^ dst,
        7 => src | dst,
        8 => !(src | dst),
        9 => !(src ^ dst),
        10 => !dst,
        11 => src | !dst,
        12 => !src,
        13 => !src | dst,
        14 => !(src & dst),
        _ => u32::MAX,
    }
}

pub struct Pixmap {
    pub surface: Surface,
}

/// `Tile` holds a copy of the pixmap as it was when the background was set, shared by the windows
/// that use it.
#[derive(Clone)]
pub enum Background {
    None,
    Pixel(u32),
    ParentRelative,
    Tile(Rc<Surface>),
}

#[derive(Clone, Copy)]
pub struct Gc {
    pub function: u8,
    pub plane_mask: u32,
    pub foreground: u32,
    pub background: u32,
    pub line_width: u16,
    pub line_style: u8,
    pub cap_style: u8,
    pub join_style: u8,
    pub fill_style: u8,
    pub subwindow_mode: u8,
    pub graphics_exposures: bool,
    /// The depth of the drawable that the GC was made for. It must match when drawing.
    pub depth: u8,
    /// Pixmap ids of the tile and the stipple. 0 means none: a solid foreground tile.
    pub tile: u32,
    pub stipple: u32,
    pub tile_origin: (i32, i32),
    /// 0 EvenOdd, 1 Winding: which parts of a polygon are inside.
    pub fill_rule: u8,
    /// 0 Chord, 1 PieSlice: how a filled arc is closed.
    pub arc_mode: u8,
}

impl Default for Gc {
    fn default() -> Self {
        Self {
            function: FUNCTION_COPY,
            plane_mask: u32::MAX,
            foreground: 0,
            background: 1,
            line_width: 0,
            line_style: 0,
            cap_style: CAP_BUTT,
            join_style: 0,
            fill_style: 0,
            subwindow_mode: 0,
            graphics_exposures: true,
            depth: ROOT_DEPTH,
            tile: 0,
            stipple: 0,
            tile_origin: (0, 0),
            fill_rule: 0,
            arc_mode: 1,
        }
    }
}

impl Gc {
    /// True when drawing with the GC writes the source pixels as they are, on all planes.
    pub fn is_plain_copy(&self) -> bool {
        self.function == FUNCTION_COPY && self.plane_mask & PIXEL_MASK == PIXEL_MASK
    }

    /// The GC as a bitmap sees it: only the lowest bit of each pixel value counts.
    pub fn on_bitmap(mut self) -> Self {
        let expand = |value: u32| if value & 1 == 1 { PIXEL_MASK } else { 0 };
        self.foreground = expand(self.foreground);
        self.background = expand(self.background);
        self.plane_mask = expand(self.plane_mask);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel_at(surface: &Surface, x: i32, y: i32) -> u32 {
        let row = surface.row(y, x, x + 1);
        u32::from_le_bytes([row[0], row[1], row[2], row[3]])
    }

    #[test]
    fn fill_rect_clips_to_the_surface() {
        let mut surface = Surface::new(4, 4);
        surface.fill_rect(IRect::new(-2, 2, 100, 100), 0xAABBCC, 3, u32::MAX);
        assert_eq!(pixel_at(&surface, 0, 2), 0xAABBCC);
        assert_eq!(pixel_at(&surface, 3, 3), 0xAABBCC);
        assert_eq!(pixel_at(&surface, 0, 1), 0);
    }

    #[test]
    fn fill_rect_outside_does_nothing() {
        let mut surface = Surface::new(4, 4);
        surface.fill_rect(IRect::new(4, 0, 2, 2), 0xFFFFFF, 3, u32::MAX);
        surface.fill_rect(IRect::new(0, -5, 2, 5), 0xFFFFFF, 3, u32::MAX);
        assert_eq!(pixel_at(&surface, 3, 0), 0);
        assert_eq!(pixel_at(&surface, 0, 0), 0);
    }

    #[test]
    fn xor_and_plane_mask_are_applied() {
        let mut surface = Surface::new(2, 1);
        surface.fill_all(0x00FF00);
        surface.fill_rect(IRect::new(0, 0, 1, 1), 0x0000FF, 6, u32::MAX);
        assert_eq!(pixel_at(&surface, 0, 0), 0x00FFFF);
        surface.fill_rect(IRect::new(1, 0, 1, 1), 0xFFFFFF, 3, 0x0000FF);
        assert_eq!(pixel_at(&surface, 1, 0), 0x00FFFF);
    }

    #[test]
    fn fourth_byte_stays_zero() {
        let mut surface = Surface::new(1, 1);
        surface.fill_rect(IRect::new(0, 0, 1, 1), 0xFFFF_FFFF, 3, u32::MAX);
        assert_eq!(surface.row(0, 0, 1), [0xFF, 0xFF, 0xFF, 0]);
    }

    #[test]
    fn each_depth_keeps_its_own_bits() {
        let mut deep = Surface::new(1, 1);
        deep.set_depth(32);
        deep.fill_rect(IRect::new(0, 0, 1, 1), 0x80FF_0000, 3, u32::MAX);
        assert_eq!(pixel_at(&deep, 0, 0), 0x80FF_0000);
        let mut alpha = Surface::new(1, 1);
        alpha.set_depth(8);
        alpha.write_rect((0, 0), (1, 1), &[0x40, 9, 9, 9], 3, u32::MAX);
        assert_eq!(pixel_at(&alpha, 0, 0), 0x40);
        alpha.fill_rect(IRect::new(0, 0, 1, 1), 0x0F, 6, u32::MAX);
        assert_eq!(pixel_at(&alpha, 0, 0), 0x4F, "xor stays inside the 8 bits");
    }

    #[test]
    fn write_rect_clips_and_keeps_row_alignment() {
        let mut surface = Surface::new(3, 2);
        let data = [1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0];
        let changed = surface.write_rect((2, 1), (2, 2), &data, 3, u32::MAX);
        assert_eq!(changed, Some(IRect::new(2, 1, 1, 1)));
        assert_eq!(pixel_at(&surface, 2, 1), 1);
        assert_eq!(pixel_at(&surface, 1, 1), 0);
        let changed = surface.write_rect((-1, 0), (2, 2), &data, 3, u32::MAX);
        assert_eq!(changed, Some(IRect::new(0, 0, 1, 2)));
        assert_eq!((pixel_at(&surface, 0, 0), pixel_at(&surface, 0, 1)), (2, 4));
    }

    #[test]
    fn write_rect_reports_only_what_changed() {
        let mut surface = Surface::new(100, 6);
        let mut image = [7, 8, 9, 0xEE].repeat(100 * 6);
        let all = IRect::new(0, 0, 100, 6);
        assert_eq!(
            surface.write_rect((0, 0), (100, 6), &image, 3, u32::MAX),
            Some(all)
        );
        assert_eq!(
            pixel_at(&surface, 99, 5),
            0x09_0807,
            "the pad byte is masked"
        );
        assert_eq!(
            surface.write_rect((0, 0), (100, 6), &image, 3, u32::MAX),
            None,
            "the same image again changes nothing"
        );
        // One other pixel in row 2 and one in row 4.
        image[(2 * 100 + 40) * 4] = 1;
        image[(4 * 100 + 70) * 4] = 1;
        let changed = surface
            .write_rect((0, 0), (100, 6), &image, 3, u32::MAX)
            .expect("two pixels changed");
        assert!(
            changed.contains(40, 2) && changed.contains(70, 4),
            "{changed:?}"
        );
        assert_eq!((changed.y0, changed.y1), (2, 5));
        assert!(changed.x0 > 16 && changed.x1 < 100 - 16, "{changed:?}");
        assert_eq!(pixel_at(&surface, 40, 2), 0x09_0801);
        assert_eq!(pixel_at(&surface, 70, 4), 0x09_0801);
        // Another function than a copy still reports its whole area.
        assert_eq!(
            surface.write_rect((0, 0), (100, 6), &image, 6, u32::MAX),
            Some(all)
        );
    }

    #[test]
    fn read_rect_returns_what_was_written() {
        let mut surface = Surface::new(4, 4);
        surface.fill_rect(IRect::new(1, 1, 2, 2), 0x0A0B0C, 3, u32::MAX);
        let data = surface.read_rect(IRect::new(1, 1, 2, 2));
        assert_eq!(data, [0x0C, 0x0B, 0x0A, 0].repeat(4));
    }

    #[test]
    fn drawing_stays_inside_the_clip_rects() {
        let mut surface = Surface::new(10, 4);
        surface.set_clip(Some(vec![IRect::new(1, 1, 2, 2), IRect::new(6, 0, 2, 1)]));
        surface.fill_rect(IRect::new(0, 0, 10, 4), 0xFFFFFF, 3, u32::MAX);
        let block = [9, 9, 9, 0].repeat(40);
        surface.write_rect((0, 0), (10, 4), &block, 3, u32::MAX);
        surface.set_clip(None);
        let lit = |x, y| pixel_at(&surface, x, y) != 0;
        assert!(lit(1, 1) && lit(2, 2) && lit(6, 0) && lit(7, 0));
        assert!(!lit(0, 0) && !lit(3, 3) && !lit(5, 0) && !lit(8, 0) && !lit(1, 3));
    }

    /// Cells whose bits depend on the cell and the row, so that a pixel in the wrong place shows.
    fn cell_bits(cell: usize, row: usize) -> u8 {
        (cell as u8).wrapping_mul(0x5B) ^ (row as u8).wrapping_mul(0x37) ^ 0xA5
    }

    /// Checks `draw_cells` over `area` against the bit of each pixel, inside `limit`.
    fn check_cells(area: IRect, clip: Option<Vec<IRect>>, clear: Option<u32>) {
        const OLD: u32 = 0x11_2233;
        let mut surface = Surface::new(40, 6);
        surface.fill_all(OLD);
        surface.set_clip(clip.clone());
        let changed = surface.draw_cells(area, 0xFF_0000, clear, cell_bits);
        if clear.is_some() {
            assert_eq!(
                surface.draw_cells(area, 0xFF_0000, clear, cell_bits),
                None,
                "the same cells again change nothing"
            );
        }
        surface.set_clip(None);
        for y in 0..6 {
            for x in 0..40 {
                if pixel_at(&surface, x, y) != OLD {
                    assert!(
                        changed.is_some_and(|rect| rect.contains(x, y)),
                        "the changed pixel ({x}, {y}) is outside {changed:?}"
                    );
                }
                let inside = area.contains(x, y)
                    && clip
                        .as_ref()
                        .is_none_or(|rects| rects.iter().any(|rect| rect.contains(x, y)));
                let (column, row) = ((x - area.x0) as usize, (y - area.y0) as usize);
                let expected = match inside {
                    false => OLD,
                    true if cell_bits(column / CELL_WIDTH, row) >> (column % CELL_WIDTH) & 1
                        == 1 =>
                    {
                        0xFF_0000
                    }
                    true => clear.unwrap_or(OLD),
                };
                assert_eq!(pixel_at(&surface, x, y), expected, "pixel ({x}, {y})");
            }
        }
    }

    #[test]
    fn cells_put_each_bit_at_its_pixel() {
        let background = Some(0x00_00FF);
        check_cells(IRect::new(4, 1, 24, 4), None, background);
        check_cells(IRect::new(4, 1, 24, 4), None, None);
        // The area starts left of the surface and ends right of it.
        check_cells(IRect::new(-13, -2, 64, 5), None, background);
        // Clip rectangles that start and end in the middle of a cell, and one of a single pixel.
        let clip = vec![
            IRect::new(7, 0, 6, 3),
            IRect::new(19, 2, 9, 4),
            IRect::new(33, 1, 1, 1),
        ];
        check_cells(IRect::new(2, 0, 32, 6), Some(clip.clone()), background);
        check_cells(IRect::new(-3, 1, 40, 4), Some(clip), None);
    }

    #[test]
    fn cells_report_only_the_cells_that_changed() {
        let letters = |cell: usize, _row: usize| if cell == 2 { 0x0F } else { 0xF0 };
        let area = IRect::new(3, 1, 32, 4);
        let mut surface = Surface::new(40, 6);
        assert_eq!(
            surface.draw_cells(area, 0xFF_0000, Some(0x00_00FF), |_, _| 0xF0),
            Some(area)
        );
        assert_eq!(
            surface.draw_cells(area, 0xFF_0000, Some(0x00_00FF), letters),
            Some(IRect::new(3 + 2 * 8, 1, 8, 4)),
            "one other cell: its columns and all its rows"
        );
        let last_row = |cell: usize, row: usize| if row == 3 { 0x55 } else { letters(cell, row) };
        assert_eq!(
            surface.draw_cells(area, 0xFF_0000, Some(0x00_00FF), last_row),
            Some(IRect::new(3, 4, 32, 1)),
            "one other row"
        );
        assert_eq!(
            surface.draw_cells(area, 0x00_FF00, Some(0x00_00FF), last_row),
            Some(area),
            "another colour changes every cell"
        );
    }

    #[test]
    fn drawing_stops_at_its_work_limit() {
        let mut surface = Surface::new(100, 100);
        surface.start_work(250);
        surface.fill_rect(IRect::new(0, 0, 10, 10), 0xFFFFFF, 3, u32::MAX);
        surface.fill_rect(IRect::new(0, 10, 10, 10), 0xFFFFFF, 3, u32::MAX);
        assert!(!surface.finish_work(), "200 pixels fit in 250");
        let mut surface = Surface::new(100, 100);
        surface.start_work(150);
        surface.fill_rect(IRect::new(0, 0, 10, 10), 0xFFFFFF, 3, u32::MAX);
        surface.fill_rect(IRect::new(0, 10, 10, 10), 0xFFFFFF, 3, u32::MAX);
        surface.fill_rect(IRect::new(0, 20, 10, 10), 0xFFFFFF, 3, u32::MAX);
        assert!(surface.finish_work());
        assert_eq!(pixel_at(&surface, 0, 0), 0xFFFFFF);
        assert_eq!(
            pixel_at(&surface, 0, 20),
            0,
            "the part over the limit was not drawn"
        );
        surface.fill_rect(IRect::new(0, 50, 100, 50), 0xFFFFFF, 3, u32::MAX);
        assert_eq!(
            pixel_at(&surface, 50, 60),
            0xFFFFFF,
            "there is no limit outside a request"
        );
    }

    #[test]
    fn an_empty_clip_draws_nothing() {
        let mut surface = Surface::new(4, 4);
        surface.set_clip(Some(Vec::new()));
        surface.fill_rect(IRect::new(0, 0, 4, 4), 0xFFFFFF, 3, u32::MAX);
        assert_eq!(
            surface.write_rect((0, 0), (1, 1), &[1, 1, 1, 0], 3, u32::MAX),
            None
        );
        surface.set_clip(None);
        assert_eq!(pixel_at(&surface, 1, 1), 0);
    }

    #[test]
    fn rect_math() {
        let a = IRect::new(0, 0, 10, 10);
        assert_eq!(
            a.intersect(IRect::new(5, 5, 10, 10)),
            Some(IRect::new(5, 5, 5, 5))
        );
        assert_eq!(a.intersect(IRect::new(10, 0, 5, 5)), None);
        assert_eq!(a.union(IRect::new(20, 20, 1, 1)).x1, 21);
    }
}
