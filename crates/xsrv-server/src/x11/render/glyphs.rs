// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! Glyph sets and CompositeGlyphs8/16/32. A glyph keeps its image as surface pixels (one u32 a
//! pixel) in the format of its set.
//!
//! Glyph memory is counted per client (the one that created the set) and for the server. Each
//! glyph costs its pixels plus `GLYPH_OVERHEAD`, so that empty glyphs are not free.
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use wire::BYTES_PER_PIXEL;

use super::composite::{
    add_row, empty_mask_keeps_target, masked_solid_row, Layer, Mask, Repeat, Source,
};
use super::draw::{check_operator, prepare_clip, read_clip, Budget, Operands};
use super::format::{Argb, Format};
use super::Picture;
use super::RenderError;
use crate::x11::codec::{padding, Reader};
use crate::x11::dispatch::Request;
use crate::x11::drawing_requests::bitmap_pixels;
use crate::x11::error::XError;
use crate::x11::resources::{IRect, PIXEL_MASK};
use crate::x11::state::{ClientId, State};
use crate::x11::zpixmap;

/// Glyph bytes of all clients, and of one client.
const MAX_GLYPH_BYTES: usize = 128 << 20;
const MAX_CLIENT_GLYPH_BYTES: usize = 32 << 20;
/// What a glyph costs besides its pixels: its entry and its metrics.
const GLYPH_OVERHEAD: usize = 64;
/// The most glyphs that one CompositeGlyphs request draws.
const MAX_GLYPHS_PER_REQUEST: usize = 1 << 16;
/// The length of an element that switches the glyph set instead of drawing.
const SWITCH_GLYPH_SET: u8 = 255;
const GLYPH_INFO_LEN: usize = 12;

pub struct Glyph {
    width: u16,
    height: u16,
    /// The pen position inside the image.
    x: i16,
    y: i16,
    /// How far the pen moves after the glyph.
    advance: (i16, i16),
    format: Format,
    pixels: Vec<u8>,
    /// For each row, the columns from its first to after its last pixel with ink.
    ink: Vec<(u16, u16)>,
}

/// The ink columns of each row of a glyph image. An rgb24 mask covers every pixel.
fn ink_spans(format: Format, width: u16, pixels: &[u8]) -> Vec<(u16, u16)> {
    let has_ink = |pixel: &[u8; BYTES_PER_PIXEL]| format == Format::Rgb24 || *pixel != [0; 4];
    pixels
        .chunks_exact((usize::from(width) * BYTES_PER_PIXEL).max(1))
        .map(|row| {
            let row = row.as_chunks().0;
            match (row.iter().position(has_ink), row.iter().rposition(has_ink)) {
                (Some(first), Some(last)) => (first as u16, last as u16 + 1),
                _ => (0, 0),
            }
        })
        .collect()
}

impl Glyph {
    fn cost(&self) -> usize {
        self.pixels.len() + GLYPH_OVERHEAD
    }
}

pub struct GlyphSet {
    format: Format,
    glyphs: HashMap<u32, Rc<Glyph>>,
    /// The client that created the set pays for its glyphs.
    owner: ClientId,
    bytes: usize,
}

/// A glyph at its place in the destination: `origin` is the top-left corner of its image, and
/// `rect` the part of the image that the destination can show.
struct Placed {
    origin: (i32, i32),
    rect: IRect,
    glyph: Rc<Glyph>,
}

impl State {
    pub(super) fn create_glyph_set(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let id = r.u32()?;
        let format_id = r.u32()?;
        r.end()?;
        if !self.owns_id(req.client, id) || self.id_in_use(id) {
            return Err(XError::IdChoice(id));
        }
        let format = Format::from_id(format_id).ok_or(RenderError::PictFormat.with(format_id))?;
        self.check_resource_quota(req.client)?;
        let set = GlyphSet {
            format,
            glyphs: HashMap::new(),
            owner: req.client,
            bytes: 0,
        };
        self.render
            .glyph_sets
            .insert(id, Rc::new(RefCell::new(set)));
        self.owners.insert(id, req.client);
        Ok(())
    }

    pub(super) fn reference_glyph_set(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let id = r.u32()?;
        let existing = r.u32()?;
        r.end()?;
        if !self.owns_id(req.client, id) || self.id_in_use(id) {
            return Err(XError::IdChoice(id));
        }
        let set = Rc::clone(self.glyph_set(existing)?);
        self.check_resource_quota(req.client)?;
        self.render.glyph_sets.insert(id, set);
        self.owners.insert(id, req.client);
        Ok(())
    }

    pub(super) fn free_glyph_set(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let id = r.u32()?;
        r.end()?;
        self.glyph_set(id)?;
        self.drop_glyph_set(id);
        Ok(())
    }

    /// Removes one id of a glyph set. The glyphs go when the last id goes.
    pub(super) fn drop_glyph_set(&mut self, id: u32) {
        let Some(set) = self.render.glyph_sets.remove(&id) else {
            return;
        };
        self.owners.remove(&id);
        if let Ok(set) = Rc::try_unwrap(set) {
            let set = set.into_inner();
            self.charge_glyphs(set.owner, set.bytes, 0);
        }
    }

    pub(super) fn add_glyphs(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let id = r.u32()?;
        let count = r.u32()? as usize;
        let set = Rc::clone(self.glyph_set(id)?);
        let (format, owner) = {
            let set = set.borrow();
            (set.format, set.owner)
        };
        if count
            .checked_mul(4 + GLYPH_INFO_LEN)
            .is_none_or(|len| len > r.remaining())
        {
            return Err(XError::Length);
        }
        let ids: Vec<u32> = (0..count).map(|_| r.u32()).collect::<Result<_, _>>()?;
        let infos: Vec<[i16; 6]> = (0..count)
            .map(|_| -> Result<[i16; 6], XError> {
                Ok([r.i16()?, r.i16()?, r.i16()?, r.i16()?, r.i16()?, r.i16()?])
            })
            .collect::<Result<_, _>>()?;
        // The cost is known from the sizes: check it before the images are decoded. The same id
        // twice in one request: the last image counts, as in Xorg.
        let sizes: HashMap<u32, usize> = ids
            .iter()
            .zip(&infos)
            .map(|(id, info)| {
                let pixels = usize::from(info[0] as u16) * usize::from(info[1] as u16);
                (*id, pixels * BYTES_PER_PIXEL + GLYPH_OVERHEAD)
            })
            .collect();
        let replaced: usize = {
            let set = set.borrow();
            sizes
                .keys()
                .filter_map(|id| set.glyphs.get(id))
                .map(|glyph| glyph.cost())
                .sum()
        };
        let added: usize = sizes.values().sum();
        self.check_glyph_budget(owner, replaced, added)?;
        let mut glyphs = HashMap::with_capacity(sizes.len());
        for (id, [width, height, x, y, advance_x, advance_y]) in ids.into_iter().zip(infos) {
            let (width, height) = (width as u16, height as u16);
            let pixels = read_glyph_image(&mut r, format, width, height)?;
            let glyph = Glyph {
                width,
                height,
                x,
                y,
                advance: (advance_x, advance_y),
                format,
                ink: ink_spans(format, width, &pixels),
                pixels,
            };
            glyphs.insert(id, Rc::new(glyph));
        }
        r.end()?;
        self.charge_glyphs(owner, replaced, added);
        let mut set = set.borrow_mut();
        set.bytes = set.bytes - replaced + added;
        set.glyphs.extend(glyphs);
        Ok(())
    }

    /// Frees the glyphs that are in the set, and then reports the first one that is not, as Xorg
    /// does.
    pub(super) fn free_glyphs(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let id = r.u32()?;
        if !r.remaining().is_multiple_of(4) {
            return Err(XError::Length);
        }
        let set = Rc::clone(self.glyph_set(id)?);
        let mut missing = None;
        let mut freed = 0;
        {
            let mut set = set.borrow_mut();
            while r.remaining() > 0 {
                let glyph_id = r.u32()?;
                match set.glyphs.remove(&glyph_id) {
                    Some(glyph) => freed += glyph.cost(),
                    None => {
                        missing.get_or_insert(glyph_id);
                    }
                }
            }
            set.bytes -= freed;
        }
        let owner = set.borrow().owner;
        self.charge_glyphs(owner, freed, 0);
        match missing {
            Some(glyph_id) => Err(RenderError::Glyph.with(glyph_id)),
            None => Ok(()),
        }
    }

    fn check_glyph_budget(
        &self,
        owner: ClientId,
        freed: usize,
        added: usize,
    ) -> Result<(), XError> {
        let client = self
            .render
            .client_glyph_bytes
            .get(&owner)
            .copied()
            .unwrap_or(0);
        if self.render.glyph_bytes - freed + added > MAX_GLYPH_BYTES
            || client - freed.min(client) + added > MAX_CLIENT_GLYPH_BYTES
        {
            return Err(XError::Alloc);
        }
        Ok(())
    }

    fn charge_glyphs(&mut self, owner: ClientId, freed: usize, added: usize) {
        let render = &mut self.render;
        render.glyph_bytes = render.glyph_bytes - freed + added;
        let client = render.client_glyph_bytes.entry(owner).or_insert(0);
        *client = (*client).saturating_sub(freed) + added;
        if *client == 0 {
            render.client_glyph_bytes.remove(&owner);
        }
    }

    /// CompositeGlyphs8, 16 or 32: `id_len` is the size of a glyph id in bytes.
    pub(super) fn composite_glyphs(&mut self, req: &Request, id_len: usize) -> Result<(), XError> {
        let mut r = req.reader();
        let op = r.u8()?;
        r.skip(3)?;
        let (source, target) = (r.u32()?, r.u32()?);
        let mask_format = r.u32()?;
        let set = r.u32()?;
        let source_at = (r.i16()?, r.i16()?);
        check_operator(op)?;
        let source = self.picture(source)?.clone();
        let target = self.picture(target)?.clone();
        let mask_format = match mask_format {
            0 => None,
            id => Some(Format::from_id(id).ok_or(RenderError::PictFormat.with(id))?),
        };
        let set = Rc::clone(self.glyph_set(set)?);
        let (placed, origin) = self.place_glyphs(&mut r, id_len, set)?;
        // The source position is relative to the first element. As in Xorg, it wraps in 16 bits.
        let offset = (
            i32::from(source_at.0.wrapping_sub(origin.0)),
            i32::from(source_at.1.wrapping_sub(origin.1)),
        );
        let clip = prepare_clip(&target, [read_clip(&source, offset)])?;
        // Only what the destination can show is drawn, and the mask covers only that.
        let placed: Vec<Placed> = placed
            .into_iter()
            .filter_map(|mut placed| {
                placed.rect = self.destination_area(&target, clip.as_ref(), placed.rect)?;
                Some(placed)
            })
            .collect();
        let Some(extent) = placed.iter().map(|placed| placed.rect).reduce(IRect::union) else {
            return Ok(());
        };
        self.flush_for_reading([Some(&source)]);
        let mut budget = Budget::new();
        let source = self.picture_source(&source, offset, extent, &mut budget)?;
        match mask_format {
            Some(format) => {
                let mask = gather_mask(&placed, extent, format, &mut budget)?;
                let operands = Operands {
                    source,
                    mask: Some(mask),
                    clip,
                };
                self.composite_into(&target, op, extent, &operands, &mut budget)
            }
            None => {
                let plain_glyphs = placed.iter().all(|p| !p.glyph.format.has_colour());
                if let (Source::Solid(colour), None, true) = (&source, &clip, plain_glyphs) {
                    if target.format != Format::A1 {
                        let (changed, outcome) =
                            self.solid_glyph_rows(&target, op, *colour, &placed, &mut budget);
                        self.mark_picture_dirty(&target, changed);
                        return outcome;
                    }
                }
                let mut operands = Operands {
                    source,
                    mask: None,
                    clip,
                };
                // A line of glyphs is one changed area for the page, marked once.
                let mut changed = None;
                let mut outcome = Ok(());
                for placed in &placed {
                    operands.mask = Some(glyph_mask(placed));
                    match self.blend_into(&target, op, placed.rect, &operands, &mut budget) {
                        Ok(part) => changed = union_of(changed, part),
                        Err(error) => {
                            outcome = Err(error);
                            break;
                        }
                    }
                }
                self.mark_picture_dirty(&target, changed);
                outcome
            }
        }
    }

    /// Text in one colour without a clip, the usual case: each glyph blends only its own rect,
    /// as one composite for each glyph would, but the surface and the colour are looked up once
    /// for the whole request. Returns the area that it wrote, also when the budget stops it.
    fn solid_glyph_rows(
        &mut self,
        target: &Picture,
        op: u8,
        colour: Argb,
        placed: &[Placed],
        budget: &mut Budget,
    ) -> (Option<IRect>, Result<(), XError>) {
        let format = target.format;
        let Some(surface) = self.picture_surface_mut(target.target) else {
            return (None, Err(RenderError::Picture.with(0)));
        };
        let mut changed = None;
        let skips_empty = empty_mask_keeps_target(op);
        for placed in placed {
            if let Err(error) = budget.spend(placed.rect) {
                return (changed, Err(error));
            }
            let (glyph, rect) = (&placed.glyph, placed.rect);
            let stride = usize::from(glyph.width) * BYTES_PER_PIXEL;
            let (left, top) = placed.origin;
            for y in rect.y0..rect.y1 {
                let glyph_row = (y - top) as usize;
                // A pixel without ink leaves the target as it is, so the row starts and ends
                // at its ink. Most rows of a glyph are shorter than its image, and some empty.
                let (x0, x1) = match (skips_empty, glyph.ink.get(glyph_row)) {
                    (true, Some(&(first, end))) => (
                        rect.x0.max(left + i32::from(first)),
                        rect.x1.min(left + i32::from(end)),
                    ),
                    _ => (rect.x0, rect.x1),
                };
                if x0 >= x1 {
                    continue;
                }
                let start = glyph_row * stride + (x0 - left) as usize * BYTES_PER_PIXEL;
                let row_bytes = (x1 - x0) as usize * BYTES_PER_PIXEL;
                let mask = glyph.pixels[start..start + row_bytes].as_chunks().0;
                let row = surface.row_mut(y, x0, x1).as_chunks_mut().0;
                masked_solid_row(op, colour, (mask, glyph.format), row, format);
            }
            changed = union_of(changed, Some(rect));
        }
        (changed, Ok(()))
    }

    /// Reads the glyph elements and puts each glyph at its place. Returns the glyphs and the
    /// position of the first element. The pen moves in 16 bits, as in Xorg.
    fn place_glyphs(
        &self,
        r: &mut Reader,
        id_len: usize,
        mut set: Rc<RefCell<GlyphSet>>,
    ) -> Result<(Vec<Placed>, (i16, i16)), XError> {
        let mut placed = Vec::new();
        let (mut x, mut y) = (0i16, 0i16);
        let mut origin = None;
        // As in Xorg, a tail shorter than an element header is ignored.
        while r.remaining() >= 8 {
            let len = r.u8()?;
            r.skip(3)?;
            let (dx, dy) = (r.i16()?, r.i16()?);
            if len == SWITCH_GLYPH_SET {
                set = Rc::clone(self.glyph_set(r.u32()?)?);
                continue;
            }
            x = x.wrapping_add(dx);
            y = y.wrapping_add(dy);
            origin.get_or_insert((x, y));
            let current = set.borrow();
            for _ in 0..len {
                let id = match id_len {
                    1 => u32::from(r.u8()?),
                    2 => u32::from(r.u16()?),
                    _ => r.u32()?,
                };
                // A glyph that is not in the set draws nothing, as in Xorg.
                let Some(glyph) = current.glyphs.get(&id) else {
                    continue;
                };
                let left = i32::from(x) - i32::from(glyph.x);
                let top = i32::from(y) - i32::from(glyph.y);
                let rect = IRect::new(left, top, glyph.width.into(), glyph.height.into());
                if rect.width() > 0 && rect.height() > 0 {
                    placed.push(Placed {
                        origin: (left, top),
                        rect,
                        glyph: Rc::clone(glyph),
                    });
                    if placed.len() > MAX_GLYPHS_PER_REQUEST {
                        return Err(XError::Alloc);
                    }
                }
                x = x.wrapping_add(glyph.advance.0);
                y = y.wrapping_add(glyph.advance.1);
            }
            drop(current);
            r.skip(padding(usize::from(len) * id_len))?;
        }
        Ok((placed, origin.unwrap_or((0, 0))))
    }

    fn glyph_set(&self, id: u32) -> Result<&Rc<RefCell<GlyphSet>>, XError> {
        self.render
            .glyph_sets
            .get(&id)
            .ok_or(RenderError::GlyphSet.with(id))
    }
}

/// The image of one glyph from AddGlyphs, as surface pixels. Rows are padded to 32 bits.
fn read_glyph_image(
    r: &mut Reader,
    format: Format,
    width: u16,
    height: u16,
) -> Result<Vec<u8>, XError> {
    let columns = usize::from(width);
    let stride = match format {
        Format::A1 => columns.div_ceil(32) * 4,
        _ => zpixmap::stride(format.depth(), columns).ok_or(XError::Length)?,
    };
    let len = stride
        .checked_mul(usize::from(height))
        .ok_or(XError::Length)?;
    let data = r.bytes(len)?;
    if format == Format::A1 {
        return Ok(bitmap_pixels(data, (width, height), (0, stride), |set| {
            if set {
                PIXEL_MASK
            } else {
                0
            }
        }));
    }
    Ok(data
        .chunks(stride.max(1))
        .flat_map(|row| zpixmap::unpack_row(format.depth(), row, columns))
        .collect())
}

fn union_of(first: Option<IRect>, second: Option<IRect>) -> Option<IRect> {
    match (first, second) {
        (Some(first), Some(second)) => Some(first.union(second)),
        (first, second) => first.or(second),
    }
}

/// A glyph as a mask over its place. A glyph with colour channels has component alpha.
fn glyph_mask(placed: &Placed) -> Mask<'_> {
    Mask {
        source: glyph_layer(placed),
        component_alpha: placed.glyph.format.has_colour(),
    }
}

fn glyph_layer(placed: &Placed) -> Source<'_> {
    let glyph = &placed.glyph;
    let (width, height) = (i32::from(glyph.width), i32::from(glyph.height));
    Source::Layer(Layer {
        part: Cow::Borrowed(&glyph.pixels),
        part_rect: IRect::new(0, 0, width, height),
        size: (width, height),
        format: glyph.format,
        repeat: Repeat::None,
        offset: (-placed.origin.0, -placed.origin.1),
    })
}

/// All glyphs added into one mask of `format` that covers `extent`. Each glyph spends the budget.
fn gather_mask(
    placed: &[Placed],
    extent: IRect,
    format: Format,
    budget: &mut Budget,
) -> Result<Mask<'static>, XError> {
    budget.spend(extent)?;
    let (width, height) = (extent.width(), extent.height());
    let stride = width as usize * BYTES_PER_PIXEL;
    let mut pixels = vec![0; stride * height as usize];
    for placed in placed {
        let rect = placed.rect;
        budget.spend(rect)?;
        let row_len = rect.width() as usize * BYTES_PER_PIXEL;
        let left = (rect.x0 - extent.x0) as usize * BYTES_PER_PIXEL;
        let layer = glyph_layer(placed);
        for y in rect.y0..rect.y1 {
            let start = (y - extent.y0) as usize * stride + left;
            let row = &mut pixels[start..start + row_len];
            add_row(&layer, (row, format), (rect.x0, y));
        }
    }
    Ok(Mask {
        source: Source::Layer(Layer {
            part: Cow::Owned(pixels),
            part_rect: IRect::new(0, 0, width, height),
            size: (width, height),
            format,
            repeat: Repeat::None,
            offset: (-extent.x0, -extent.y0),
        }),
        component_alpha: format.has_colour(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ink_spans_hold_the_ink_of_each_row() {
        let a8 = |alphas: &[u8]| -> Vec<u8> { alphas.iter().flat_map(|&a| [a, 0, 0, 0]).collect() };
        let image = a8(&[0, 0, 0, 0, 0, 9, 0, 255, 7, 0, 0, 0]);
        assert_eq!(
            ink_spans(Format::A8, 4, &image),
            vec![(0, 0), (1, 4), (0, 1)],
            "an empty row, ink with a gap, ink at the left"
        );
        assert_eq!(
            ink_spans(Format::Rgb24, 4, &image),
            vec![(0, 4); 3],
            "an rgb24 mask covers every pixel"
        );
    }
}
