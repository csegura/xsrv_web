// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! RENDER compositing on premultiplied pixels: the operators Clear to Saturate (0 to 13), masks
//! with and without component alpha, and the repeat modes. Pure functions, no X state.
use std::borrow::Cow;

use wire::BYTES_PER_PIXEL;

use super::format::{Argb, Format, ALPHA, TRANSPARENT};
use crate::x11::resources::{IRect, PIXEL_MASK};

/// The highest operator of RENDER 0.1 (Saturate).
pub const MAX_OPERATOR: u8 = 13;
const OP_CLEAR: u8 = 0;
const OP_SRC: u8 = 1;
const OP_OVER: u8 = 3;
const OP_ADD: u8 = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Repeat {
    None,
    Normal,
    Pad,
    Reflect,
}

impl Repeat {
    pub fn from_value(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::None),
            1 => Some(Self::Normal),
            2 => Some(Self::Pad),
            3 => Some(Self::Reflect),
            _ => None,
        }
    }

    /// The coordinate inside `0..size` that `at` reads, or `None` for a transparent pixel.
    fn wrap(self, at: i32, size: i32) -> Option<i32> {
        if size <= 0 {
            return None;
        }
        if (0..size).contains(&at) {
            return Some(at);
        }
        match self {
            Self::None => None,
            Self::Normal => Some(at.rem_euclid(size)),
            Self::Pad => Some(at.clamp(0, size - 1)),
            Self::Reflect => {
                let period = at.rem_euclid(2 * size);
                Some(if period < size {
                    period
                } else {
                    2 * size - 1 - period
                })
            }
        }
    }
}

/// Pixels of a picture that a composite reads. `part` holds the area `part_rect` of a drawable of
/// size `size`; a coordinate in the destination plus `offset` is a coordinate in the drawable.
pub struct Layer<'a> {
    pub part: Cow<'a, [u8]>,
    pub part_rect: IRect,
    pub size: (i32, i32),
    pub format: Format,
    pub repeat: Repeat,
    pub offset: (i32, i32),
}

impl Layer<'_> {
    fn fetch(&self, x: i32, y: i32) -> Argb {
        let wrapped = self
            .repeat
            .wrap(x + self.offset.0, self.size.0)
            .zip(self.repeat.wrap(y + self.offset.1, self.size.1));
        let Some((x, y)) = wrapped.filter(|(x, y)| self.part_rect.contains(*x, *y)) else {
            return TRANSPARENT;
        };
        let index = ((y - self.part_rect.y0) as usize * self.part_rect.width() as usize
            + (x - self.part_rect.x0) as usize)
            * BYTES_PER_PIXEL;
        let bytes = &self.part[index..index + BYTES_PER_PIXEL];
        self.format
            .fetch(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }
}

impl Layer<'_> {
    /// What the layer gives for `width` pixels of the destination row `y` that start at `x`.
    fn span(&self, x: i32, y: i32, width: usize) -> Span<'_> {
        // cairo gives a plain colour as a 1x1 picture that repeats: every pixel reads the same.
        if self.size == (1, 1) && self.repeat != Repeat::None {
            return Span::Solid(self.fetch(x, y));
        }
        let (left, top) = (x + self.offset.0, y + self.offset.1);
        let right = left + width as i32;
        let part = self.part_rect;
        let inside = (0..self.size.1).contains(&top)
            && (part.y0..part.y1).contains(&top)
            && left >= part.x0.max(0)
            && right <= part.x1.min(self.size.0);
        if !inside {
            return Span::Slow;
        }
        let start = ((top - part.y0) as usize * part.width() as usize + (left - part.x0) as usize)
            * BYTES_PER_PIXEL;
        let bytes = &self.part[start..start + width * BYTES_PER_PIXEL];
        Span::Pixels(bytes.as_chunks().0, self.format)
    }
}

pub enum Source<'a> {
    Solid(Argb),
    Layer(Layer<'a>),
}

impl Source<'_> {
    fn fetch(&self, x: i32, y: i32) -> Argb {
        match self {
            Self::Solid(pixel) => *pixel,
            Self::Layer(layer) => layer.fetch(x, y),
        }
    }

    fn span(&self, x: i32, y: i32, width: usize) -> Span<'_> {
        match self {
            Self::Solid(pixel) => Span::Solid(*pixel),
            Self::Layer(layer) => layer.span(x, y, width),
        }
    }
}

/// What a source gives for one row of the destination.
enum Span<'a> {
    Solid(Argb),
    /// One stored pixel for each pixel of the row.
    Pixels(&'a [Stored], Format),
    /// The row leaves the source or the source repeats inside it: each pixel is looked up.
    Slow,
}

/// A pixel as a surface stores it.
pub type Stored = [u8; BYTES_PER_PIXEL];

/// A mask and whether each colour channel masks its own channel (component alpha).
pub struct Mask<'a> {
    pub source: Source<'a>,
    pub component_alpha: bool,
}

/// Composites `area` (destination coordinates) into `pixels`, which hold exactly that area of a
/// drawable of format `format`.
pub fn composite_area(
    op: u8,
    source: &Source,
    mask: Option<&Mask>,
    (pixels, format): (&mut [u8], Format),
    area: IRect,
) {
    let row_len = area.width() as usize * BYTES_PER_PIXEL;
    if row_len == 0 {
        return;
    }
    for (row, y) in pixels.chunks_exact_mut(row_len).zip(area.y0..) {
        composite_row(op, source, mask, (row, format), (area.x0, y));
    }
}

/// Composites one row of the destination: `row` holds the pixels from (`x`, `y`) to the right.
/// The usual cases (a plain colour, a glyph as the mask, a picture without a mask) have their
/// own loops. They give the same pixels as `blend`, which the other cases use.
pub fn composite_row(
    op: u8,
    source: &Source,
    mask: Option<&Mask>,
    (row, format): (&mut [u8], Format),
    (x, y): (i32, i32),
) {
    let pixels = row.as_chunks_mut::<BYTES_PER_PIXEL>().0;
    let width = pixels.len();
    let source_span = source.span(x, y, width);
    let mask_span = mask.map(|mask| (mask.source.span(x, y, width), mask.component_alpha));
    match (source_span, mask_span) {
        (Span::Solid(colour), None) => solid_row(op, colour, pixels, format),
        (Span::Solid(colour), Some((Span::Pixels(mask, mask_format), false))) => {
            masked_solid_row(op, colour, (mask, mask_format), pixels, format);
        }
        (Span::Pixels(source, source_format), None) => {
            picture_row(op, (source, source_format), pixels, format);
        }
        _ => {
            for (bytes, x) in pixels.iter_mut().zip(x..) {
                let mask = mask.map(|mask| (mask.source.fetch(x, y), mask.component_alpha));
                put(op, source.fetch(x, y), mask, bytes, format);
            }
        }
    }
}

/// Adds a source into one row of a mask with the Add operator, as glyphs are gathered.
pub fn add_row(source: &Source, row: (&mut [u8], Format), at: (i32, i32)) {
    composite_row(OP_ADD, source, None, row, at);
}

#[inline]
fn put(op: u8, source: Argb, mask: Option<(Argb, bool)>, bytes: &mut Stored, format: Format) {
    let target = format.fetch(u32::from_le_bytes(*bytes));
    *bytes = format.store(blend(op, source, mask, target)).to_le_bytes();
}

/// Over with a source that is already masked: the source, plus what its alpha lets through.
#[inline(always)]
fn put_over(source: Argb, bytes: &mut Stored, format: Format) {
    let keep = 255 - source[ALPHA];
    if keep == 0 {
        *bytes = format.store(source).to_le_bytes();
        return;
    }
    let Some(kept) = kept_channels(format) else {
        let target = format.fetch(u32::from_le_bytes(*bytes));
        let result = std::array::from_fn(|i| source[i].saturating_add(mul(target[i], keep)));
        *bytes = format.store(result).to_le_bytes();
        return;
    };
    let sum = over_value(u32::from_le_bytes(source), u32::from_le_bytes(*bytes));
    *bytes = (sum & kept).to_le_bytes();
}

/// The formats with colour keep their channels where a pixel has them, so the four channels
/// can be blended in one value. This is the part of that value that the format stores: rgb24
/// drops the fourth channel, as `store` does. `None` for the formats with alpha only.
fn kept_channels(format: Format) -> Option<u32> {
    match format {
        Format::Argb32 => Some(u32::MAX),
        Format::Rgb24 => Some(PIXEL_MASK),
        Format::A8 | Format::A1 => None,
    }
}

/// Over on all four channels: the source, plus what its alpha lets through of the target. It
/// has no branch, and it is right for every alpha: a transparent source gives the target, an
/// opaque one the source.
#[inline(always)]
fn over_value(source: u32, target: u32) -> u32 {
    let keep = 255 - (source >> 24) as u8;
    add_channels(source, scale_channels(target, keep))
}

const LOW_CHANNELS: u32 = 0x00FF_00FF;

/// `mul` on the four channels of a pixel at once: two channels in each half, 16 bits apart.
#[inline(always)]
fn scale_channels(pixel: u32, by: u8) -> u32 {
    const HALF: u32 = 0x0080_0080;
    let by = u32::from(by);
    let low = (pixel & LOW_CHANNELS) * by + HALF;
    let low = ((low + ((low >> 8) & LOW_CHANNELS)) >> 8) & LOW_CHANNELS;
    let high = ((pixel >> 8) & LOW_CHANNELS) * by + HALF;
    let high = (high + ((high >> 8) & LOW_CHANNELS)) & !LOW_CHANNELS;
    low | high
}

/// A saturating add on the four channels of two pixels at once.
#[inline(always)]
fn add_channels(a: u32, b: u32) -> u32 {
    // A channel that passed 255 has its ninth bit set: then all eight bits below it are set.
    let saturate = |sum: u32| (sum | (0x0100_0100 - ((sum >> 8) & LOW_CHANNELS))) & LOW_CHANNELS;
    let low = saturate((a & LOW_CHANNELS) + (b & LOW_CHANNELS));
    let high = saturate(((a >> 8) & LOW_CHANNELS) + ((b >> 8) & LOW_CHANNELS));
    low | (high << 8)
}

/// The alpha of a stored mask pixel: `format.fetch(pixel)[ALPHA]` without the other channels.
#[inline(always)]
fn stored_alpha(format: Format, stored: &Stored) -> u8 {
    match format {
        Format::A8 => stored[0],
        Format::A1 if *stored == [0; BYTES_PER_PIXEL] => 0,
        Format::A1 | Format::Rgb24 => 255,
        Format::Argb32 => stored[ALPHA],
    }
}

/// A row of one colour without a mask.
fn solid_row(op: u8, colour: Argb, pixels: &mut [Stored], format: Format) {
    match op {
        OP_CLEAR => pixels.fill(format.store(TRANSPARENT).to_le_bytes()),
        OP_SRC => pixels.fill(format.store(colour).to_le_bytes()),
        OP_OVER if colour[ALPHA] == 255 => pixels.fill(format.store(colour).to_le_bytes()),
        OP_OVER => pixels
            .iter_mut()
            .for_each(|bytes| put_over(colour, bytes, format)),
        _ => pixels
            .iter_mut()
            .for_each(|bytes| put(op, colour, None, bytes, format)),
    }
}

/// True when `masked_solid_row` leaves a pixel as it is where the mask is all zero.
pub fn empty_mask_keeps_target(op: u8) -> bool {
    op == OP_OVER
}

/// A row of one colour through a mask without component alpha: a line of glyphs.
pub fn masked_solid_row(
    op: u8,
    colour: Argb,
    (mask, mask_format): (&[Stored], Format),
    pixels: &mut [Stored],
    format: Format,
) {
    if op != OP_OVER {
        let masks = mask
            .iter()
            .map(|stored| mask_format.fetch(u32::from_le_bytes(*stored)));
        for (bytes, mask) in pixels.iter_mut().zip(masks) {
            put(op, colour, Some((mask, false)), bytes, format);
        }
        return;
    }
    let colour_value = u32::from_le_bytes(colour);
    // Most pixels of a glyph are empty or full, in runs, so a branch for each kind of coverage
    // is cheaper than the same blend for every pixel (measured: rows of glyphs are short).
    // Glyphs of 8-bit coverage on a window are the usual case: nothing depends on a format
    // inside the loop.
    if let (Format::A8, Some(kept)) = (mask_format, kept_channels(format)) {
        let opaque = colour[ALPHA] == 255;
        let full = (colour_value & kept).to_le_bytes();
        for (bytes, mask) in pixels.iter_mut().zip(mask) {
            match mask[0] {
                0 => {}
                255 if opaque => *bytes = full,
                alpha => {
                    let source = scale_channels(colour_value, alpha);
                    *bytes = (over_value(source, u32::from_le_bytes(*bytes)) & kept).to_le_bytes();
                }
            }
        }
        return;
    }
    for (bytes, mask) in pixels.iter_mut().zip(mask) {
        match stored_alpha(mask_format, mask) {
            // Nothing of the source: Over keeps the target.
            0 => {}
            255 => put_over(colour, bytes, format),
            alpha => put_over(
                scale_channels(colour_value, alpha).to_le_bytes(),
                bytes,
                format,
            ),
        }
    }
}

/// A row of a picture without a mask.
fn picture_row(
    op: u8,
    (source, source_format): (&[Stored], Format),
    pixels: &mut [Stored],
    format: Format,
) {
    // A stored pixel means the same in both, so Src is a copy. Not for a1: `store` gives a set
    // pixel its one stored form, and a copy would keep any other non-zero value of the source.
    if op == OP_SRC && source_format == format && format != Format::A1 {
        pixels.copy_from_slice(source);
        return;
    }
    let sources = source
        .iter()
        .map(|stored| source_format.fetch(u32::from_le_bytes(*stored)));
    match op {
        OP_SRC => {
            for (bytes, source) in pixels.iter_mut().zip(sources) {
                *bytes = format.store(source).to_le_bytes();
            }
        }
        OP_OVER if source_format == Format::Argb32 && kept_channels(format).is_some() => {
            let kept = kept_channels(format).unwrap_or(u32::MAX);
            for (bytes, source) in pixels.iter_mut().zip(source) {
                let sum = over_value(u32::from_le_bytes(*source), u32::from_le_bytes(*bytes));
                *bytes = (sum & kept).to_le_bytes();
            }
        }
        OP_OVER => {
            for (bytes, source) in pixels.iter_mut().zip(sources) {
                // A transparent pixel is all zero (premultiplied): Over keeps the target.
                if source != TRANSPARENT {
                    put_over(source, bytes, format);
                }
            }
        }
        _ => {
            for (bytes, source) in pixels.iter_mut().zip(sources) {
                put(op, source, None, bytes, format);
            }
        }
    }
}

/// One pixel: `source` IN `mask`, then `op` with `target`.
pub fn blend(op: u8, source: Argb, mask: Option<(Argb, bool)>, target: Argb) -> Argb {
    let alpha = source[ALPHA];
    let (colour, source_alpha): (Argb, Argb) = match mask {
        None => (source, [alpha; 4]),
        Some((mask, false)) => {
            let m = mask[ALPHA];
            (source.map(|c| mul(c, m)), [mul(alpha, m); 4])
        }
        Some((mask, true)) => (
            std::array::from_fn(|i| mul(source[i], mask[i])),
            std::array::from_fn(|i| mul(alpha, mask[i])),
        ),
    };
    let target_alpha = target[ALPHA];
    std::array::from_fn(|i| channel(op, colour[i], source_alpha[i], target[i], target_alpha))
}

/// The Porter-Duff equations on one channel: s, its source alpha, d, and the target alpha.
fn channel(op: u8, s: u8, sa: u8, d: u8, da: u8) -> u8 {
    let inv = |value: u8| 255 - value;
    match op {
        0 => 0,
        1 => s,
        2 => d,
        3 => s.saturating_add(mul(d, inv(sa))),
        4 => mul(s, inv(da)).saturating_add(d),
        5 => mul(s, da),
        6 => mul(d, sa),
        7 => mul(s, inv(da)),
        8 => mul(d, inv(sa)),
        9 => mul(s, da).saturating_add(mul(d, inv(sa))),
        10 => mul(s, inv(da)).saturating_add(mul(d, sa)),
        11 => mul(s, inv(da)).saturating_add(mul(d, inv(sa))),
        12 => s.saturating_add(d),
        13 if sa <= inv(da) => s.saturating_add(d),
        // Saturate: the source is scaled so that it just fills the room left in the target.
        13 => {
            let scaled = (u32::from(s) * u32::from(inv(da)) + u32::from(sa) / 2) / u32::from(sa);
            (scaled.min(255) as u8).saturating_add(d)
        }
        _ => d,
    }
}

/// a * b / 255, rounded.
#[inline]
fn mul(a: u8, b: u8) -> u8 {
    let t = u32::from(a) * u32::from(b) + 128;
    ((t + (t >> 8)) >> 8) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED_HALF: Argb = [0, 0, 0x80, 0x80];
    const BLUE: Argb = [0xFF, 0, 0, 0xFF];

    #[test]
    fn multiplication_rounds_like_the_x_server() {
        assert_eq!(mul(255, 255), 255);
        assert_eq!(mul(255, 0x80), 0x80);
        assert_eq!(mul(0x80, 0x80), 0x40);
        assert_eq!(mul(0, 200), 0);
    }

    #[test]
    fn porter_duff_operators_on_half_red_over_opaque_blue() {
        let check =
            |op, expected: Argb| assert_eq!(blend(op, RED_HALF, None, BLUE), expected, "op {op}");
        check(0, [0, 0, 0, 0]);
        check(1, RED_HALF);
        check(2, BLUE);
        check(3, [0x7F, 0, 0x80, 0xFF]); // Over: red + blue * (1 - 0.5)
        check(4, BLUE); // OverReverse: the target is opaque
        check(5, RED_HALF); // In: target alpha is 1
        check(6, [0x80, 0, 0, 0x80]); // InReverse: blue * 0.5
        check(7, [0, 0, 0, 0]); // Out: target alpha is 1
        check(8, [0x7F, 0, 0, 0x7F]);
        check(9, [0x7F, 0, 0x80, 0xFF]); // Atop
        check(10, [0x80, 0, 0, 0x80]); // AtopReverse
        check(11, [0x7F, 0, 0, 0x7F]); // Xor
        check(12, [0xFF, 0, 0x80, 0xFF]); // Add saturates
        check(13, BLUE); // Saturate: no room left in the target
    }

    #[test]
    fn saturate_fills_the_room_left_in_the_target() {
        let target = [0, 0, 0, 0xC0];
        let source = [0, 0, 0xFF, 0xFF];
        assert_eq!(blend(13, source, None, target), [0, 0, 0x3F, 0xFF]);
    }

    #[test]
    fn a_mask_scales_the_source_and_component_alpha_works_per_channel() {
        let white = [0xFF; 4];
        let grey = [0, 0, 0, 0x80];
        assert_eq!(blend(1, white, Some((grey, false)), BLUE), [0x80; 4]);
        let per_channel = [0xFF, 0x80, 0, 0xFF];
        assert_eq!(
            blend(3, white, Some((per_channel, true)), [0, 0, 0, 0xFF]),
            [0xFF, 0x80, 0, 0xFF],
            "Over with component alpha keeps the target where the mask channel is 0"
        );
    }

    #[test]
    fn repeat_modes_map_coordinates() {
        assert_eq!(Repeat::None.wrap(3, 3), None);
        assert_eq!(Repeat::Normal.wrap(-1, 3), Some(2));
        assert_eq!(Repeat::Pad.wrap(7, 3), Some(2));
        assert_eq!(Repeat::Reflect.wrap(3, 3), Some(2));
        assert_eq!(Repeat::Reflect.wrap(-1, 3), Some(0));
    }

    /// The definition of a composite: every pixel looked up and blended on its own.
    fn reference(
        op: u8,
        source: &Source,
        mask: Option<&Mask>,
        (pixels, format): (&mut [u8], Format),
        area: IRect,
    ) {
        let width = area.width() as usize;
        for (index, bytes) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let x = area.x0 + (index % width) as i32;
            let y = area.y0 + (index / width) as i32;
            let target = format.fetch(u32::from_le_bytes(*bytes));
            let mask = mask.map(|mask| (mask.source.fetch(x, y), mask.component_alpha));
            *bytes = format
                .store(blend(op, source.fetch(x, y), mask, target))
                .to_le_bytes();
        }
    }

    /// Stored pixels of `format` with all kinds of alpha: 0, 255 and values between.
    fn stored_pixels(format: Format, count: usize, seed: u32) -> Vec<u8> {
        let mut state = seed;
        let mut next = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 24) as u8
        };
        (0..count)
            .flat_map(|_| {
                let alpha = match next() % 4 {
                    0 => 0,
                    1 => 255,
                    _ => next(),
                };
                let premultiplied = |channel: u8| mul(channel, alpha);
                let pixel = [
                    premultiplied(next()),
                    premultiplied(next()),
                    premultiplied(next()),
                    alpha,
                ];
                format.store(pixel).to_le_bytes()
            })
            .collect()
    }

    fn layer(
        format: Format,
        size: (i32, i32),
        repeat: Repeat,
        offset: (i32, i32),
    ) -> Source<'static> {
        Source::Layer(Layer {
            part: Cow::Owned(stored_pixels(
                format,
                (size.0 * size.1) as usize,
                0xABCD ^ size.0 as u32,
            )),
            part_rect: IRect::new(0, 0, size.0, size.1),
            size,
            format,
            repeat,
            offset,
        })
    }

    #[test]
    fn the_row_loops_give_the_same_pixels_as_the_definition() {
        let area = IRect::new(5, 7, 9, 4);
        let inside = (-5, -7);
        let sources = || {
            vec![
                Source::Solid([0x10, 0x20, 0x30, 0xFF]),
                Source::Solid([0x08, 0x10, 0x18, 0x80]),
                Source::Solid(TRANSPARENT),
                layer(Format::Argb32, (1, 1), Repeat::Normal, (3, 3)),
                layer(Format::Argb32, (9, 4), Repeat::None, inside),
                layer(Format::Rgb24, (9, 4), Repeat::None, inside),
                layer(Format::A8, (9, 4), Repeat::None, inside),
                layer(Format::A1, (9, 4), Repeat::None, inside),
                // Smaller than the area, and shifted: some pixels are outside, some repeat.
                layer(Format::Argb32, (4, 3), Repeat::None, (-7, -8)),
                layer(Format::Argb32, (4, 3), Repeat::Normal, (-7, -8)),
                layer(Format::Rgb24, (4, 3), Repeat::Reflect, (2, 1)),
            ]
        };
        let masks = || -> Vec<Option<Mask<'static>>> {
            let mask = |source, component_alpha| {
                Some(Mask {
                    source,
                    component_alpha,
                })
            };
            vec![
                None,
                mask(layer(Format::A8, (9, 4), Repeat::None, inside), false),
                mask(layer(Format::A1, (9, 4), Repeat::None, inside), false),
                mask(layer(Format::Argb32, (9, 4), Repeat::None, inside), true),
                mask(layer(Format::A8, (4, 3), Repeat::None, (-6, -7)), false),
                mask(Source::Solid([0, 0, 0, 0x60]), false),
            ]
        };
        for format in super::super::format::FORMATS {
            for op in 0..=MAX_OPERATOR {
                for (source_index, source) in sources().iter().enumerate() {
                    for (mask_index, mask) in masks().iter().enumerate() {
                        let before = stored_pixels(format, 9 * 4, 0x1357);
                        let mut expected = before.clone();
                        reference(op, source, mask.as_ref(), (&mut expected, format), area);
                        let mut actual = before;
                        composite_area(op, source, mask.as_ref(), (&mut actual, format), area);
                        assert_eq!(
                            actual, expected,
                            "{format:?}, op {op}, source {source_index}, mask {mask_index}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn composite_area_reads_through_the_layer_offset() {
        let source = Source::Layer(Layer {
            part: Cow::Owned([1, 2, 3, 0xFF, 4, 5, 6, 0xFF].to_vec()),
            part_rect: IRect::new(0, 0, 2, 1),
            size: (2, 1),
            format: Format::Argb32,
            repeat: Repeat::None,
            offset: (-10, 0),
        });
        let mut pixels = vec![0; 3 * BYTES_PER_PIXEL];
        composite_area(
            1,
            &source,
            None,
            (&mut pixels, Format::Rgb24),
            IRect::new(10, 0, 3, 1),
        );
        assert_eq!(pixels, [1, 2, 3, 0, 4, 5, 6, 0, 0, 0, 0, 0]);
    }
}
