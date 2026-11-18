// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! Composite and FillRectangles, and the step that every RENDER drawing shares: read the
//! destination area, blend, and write it back through the clip.
//!
//! Each request spends one `Budget`: the pixels it blends, the source pixels it copies, and the
//! clip rectangles it walks. A request over the budget fails with BadAlloc.
use std::borrow::Cow;

use super::composite::{composite_area, composite_row, Layer, Mask, Repeat, Source, MAX_OPERATOR};
use super::format::{Argb, Format};
use super::picture::{Picture, Target};
use super::RenderError;
use crate::x11::dispatch::Request;
use crate::x11::error::XError;
use crate::x11::resources::{IRect, Surface};
use crate::x11::screen::MAX_PIXELS_PER_REQUEST;
use crate::x11::state::State;

const FUNCTION_COPY: u8 = 3;
/// The most rectangles that the intersection of several clips may have.
const MAX_CLIP_RECTS: usize = 1 << 16;
/// The most rectangle pairs that an intersection of two clips may test.
const MAX_CLIP_PAIRS: usize = 1 << 22;

/// The work that one request may still do, counted in pixels.
pub(super) struct Budget(u64);

impl Budget {
    pub(super) fn new() -> Self {
        Self(MAX_PIXELS_PER_REQUEST)
    }

    pub(super) fn spend(&mut self, area: IRect) -> Result<(), XError> {
        self.spend_units(area.width() as u64 * area.height() as u64)
    }

    pub(super) fn spend_units(&mut self, units: u64) -> Result<(), XError> {
        self.0 = self.0.checked_sub(units).ok_or(XError::Alloc)?;
        Ok(())
    }
}

/// The clip of one request in destination coordinates: the clip of the destination picture,
/// cut by the clips of the source and the mask (as in Xorg). `extent` is `None` when it is empty.
pub(super) struct Clip {
    rects: Vec<IRect>,
    extent: Option<IRect>,
    /// True when no two rectangles overlap, so that drawing each one draws each pixel once.
    disjoint: bool,
}

/// What a composite reads, and where it may write.
pub(super) struct Operands<'a> {
    pub source: Source<'a>,
    pub mask: Option<Mask<'a>>,
    pub clip: Option<Clip>,
}

pub(super) fn check_operator(op: u8) -> Result<(), XError> {
    if op > MAX_OPERATOR {
        return Err(XError::Value(op.into()));
    }
    Ok(())
}

impl State {
    pub(super) fn composite(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let op = r.u8()?;
        r.skip(3)?;
        let (source, mask, target) = (r.u32()?, r.u32()?, r.u32()?);
        let source_at = (i32::from(r.i16()?), i32::from(r.i16()?));
        let mask_at = (i32::from(r.i16()?), i32::from(r.i16()?));
        let (x, y) = (i32::from(r.i16()?), i32::from(r.i16()?));
        let (width, height) = (i32::from(r.u16()?), i32::from(r.u16()?));
        r.end()?;
        check_operator(op)?;
        let target = self.picture(target)?.clone();
        let source = self.picture(source)?.clone();
        let mask = (mask != 0)
            .then(|| self.picture(mask).cloned())
            .transpose()?;
        let source_offset = (source_at.0 - x, source_at.1 - y);
        let mask_offset = (mask_at.0 - x, mask_at.1 - y);
        let reads = [
            read_clip(&source, source_offset),
            mask.as_ref().and_then(|mask| read_clip(mask, mask_offset)),
        ];
        let clip = prepare_clip(&target, reads)?;
        let Some(area) =
            self.destination_area(&target, clip.as_ref(), IRect::new(x, y, width, height))
        else {
            return Ok(());
        };
        self.flush_for_reading([Some(&source), mask.as_ref()]);
        let mut budget = Budget::new();
        let mask = mask
            .map(|picture| -> Result<Mask, XError> {
                Ok(Mask {
                    source: self.picture_source(&picture, mask_offset, area, &mut budget)?,
                    component_alpha: picture.component_alpha,
                })
            })
            .transpose()?;
        let operands = Operands {
            source: self.picture_source(&source, source_offset, area, &mut budget)?,
            mask,
            clip,
        };
        self.composite_into(&target, op, area, &operands, &mut budget)
    }

    pub(super) fn fill_rectangles(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let op = r.u8()?;
        r.skip(3)?;
        let target = r.u32()?;
        let [red, green, blue, alpha] = [r.u16()?, r.u16()?, r.u16()?, r.u16()?];
        if !r.remaining().is_multiple_of(8) {
            return Err(XError::Length);
        }
        check_operator(op)?;
        let target = self.picture(target)?.clone();
        // The colour is premultiplied, 16 bits a channel.
        let colour: Argb = [blue, green, red, alpha].map(|channel| (channel >> 8) as u8);
        let mut budget = Budget::new();
        let operands = Operands {
            source: Source::Solid(colour),
            mask: None,
            clip: prepare_clip(&target, [])?,
        };
        while r.remaining() > 0 {
            let (x, y) = (i32::from(r.i16()?), i32::from(r.i16()?));
            let (width, height) = (i32::from(r.u16()?), i32::from(r.u16()?));
            let area = IRect::new(x, y, width, height);
            self.composite_into(&target, op, area, &operands, &mut budget)?;
        }
        Ok(())
    }

    /// A window picture that includes inferiors is read from the screen, which must be drawn.
    pub(super) fn flush_for_reading<const N: usize>(&mut self, pictures: [Option<&Picture>; N]) {
        let reads_screen = pictures.into_iter().flatten().any(|picture| {
            matches!(picture.target, Target::Window(_)) && picture.include_inferiors
        });
        if reads_screen {
            self.flush_display();
        }
    }

    /// The part of `area` that a drawing may change: inside the destination and its clip.
    pub(super) fn destination_area(
        &self,
        target: &Picture,
        clip: Option<&Clip>,
        area: IRect,
    ) -> Option<IRect> {
        let bounds = self.picture_surface(target.target)?.bounds();
        let area = area.intersect(bounds)?;
        match clip {
            None => Some(area),
            Some(clip) => area.intersect(clip.extent?),
        }
    }

    /// The pixels of `picture` that a composite of `area` reads. A destination coordinate plus
    /// `offset` is a coordinate in the picture. A repeating picture is copied whole.
    pub(super) fn picture_source(
        &self,
        picture: &Picture,
        offset: (i32, i32),
        area: IRect,
        budget: &mut Budget,
    ) -> Result<Source<'static>, XError> {
        let surface = self.picture_surface(picture.target).ok_or(XError::Match)?;
        let bounds = surface.bounds();
        // cairo gives a plain colour as a 1x1 picture that repeats. Every pixel reads the same
        // one, so it is looked up once for the request and not for each row.
        if picture.repeat != Repeat::None && (bounds.width(), bounds.height()) == (1, 1) {
            budget.spend(bounds)?;
            let pixel = self.read_picture(picture, bounds);
            let stored = pixel.first_chunk().copied().unwrap_or_default();
            return Ok(Source::Solid(
                picture.format.fetch(u32::from_le_bytes(stored)),
            ));
        }
        let needed = if picture.repeat == Repeat::None {
            area.translate(offset.0, offset.1).intersect(bounds)
        } else {
            Some(bounds)
        };
        let (part, part_rect) = match needed {
            Some(rect) => {
                budget.spend(rect)?;
                (self.read_picture(picture, rect), rect)
            }
            None => (Vec::new(), IRect::new(0, 0, 0, 0)),
        };
        Ok(Source::Layer(Layer {
            part: Cow::Owned(part),
            part_rect,
            size: (bounds.width(), bounds.height()),
            format: picture.format,
            repeat: picture.repeat,
            offset,
        }))
    }

    /// The pixels of `rect` of a picture. A viewable window that includes inferiors gives what the
    /// screen shows there, with its children, as GetImage does.
    fn read_picture(&self, picture: &Picture, rect: IRect) -> Vec<u8> {
        if let (Target::Window(window), true) = (picture.target, picture.include_inferiors) {
            if let Some((x, y)) = self
                .window_origin(window)
                .filter(|_| self.is_viewable(window))
            {
                return self.display.read_region(
                    x + rect.x0,
                    y + rect.y0,
                    rect.width(),
                    rect.height(),
                );
            }
        }
        self.picture_surface(picture.target)
            .map_or_else(Vec::new, |surface| surface.read_rect(rect))
    }

    /// Blends `area` of the destination picture with the operands, and writes the result through
    /// the clip. A window shows the change on the page.
    pub(super) fn composite_into(
        &mut self,
        target: &Picture,
        op: u8,
        area: IRect,
        operands: &Operands,
        budget: &mut Budget,
    ) -> Result<(), XError> {
        let changed = self.blend_into(target, op, area, operands, budget)?;
        self.mark_picture_dirty(target, changed);
        Ok(())
    }

    /// A window shows on the page what a drawing changed.
    pub(super) fn mark_picture_dirty(&mut self, target: &Picture, changed: Option<IRect>) {
        if let (Target::Window(window), Some(changed)) = (target.target, changed) {
            self.mark_area_dirty(window, changed);
        }
    }

    /// Blends `area` of the destination picture with the operands, through the clip. Returns the
    /// area that it wrote. The caller marks it for the page.
    pub(super) fn blend_into(
        &mut self,
        target: &Picture,
        op: u8,
        area: IRect,
        operands: &Operands,
        budget: &mut Budget,
    ) -> Result<Option<IRect>, XError> {
        let Some(area) = self.destination_area(target, operands.clip.as_ref(), area) else {
            return Ok(None);
        };
        budget.spend(area)?;
        if let Some(clip) = &operands.clip {
            budget.spend_units(clip.rects.len() as u64)?;
        }
        let surface = self
            .picture_surface_mut(target.target)
            .ok_or(RenderError::Picture.with(0))?;
        let in_place =
            target.format != Format::A1 && operands.clip.as_ref().is_none_or(|clip| clip.disjoint);
        if in_place {
            Ok(blend_in_place(surface, target.format, op, area, operands))
        } else {
            Ok(blend_through_copy(
                surface,
                target.format,
                op,
                area,
                operands,
            ))
        }
    }
}

/// Blends each row where it is. The operands hold their own pixels, so the destination can be
/// the drawable that a source came from.
fn blend_in_place(
    surface: &mut Surface,
    format: Format,
    op: u8,
    area: IRect,
    operands: &Operands,
) -> Option<IRect> {
    let (source, mask) = (&operands.source, operands.mask.as_ref());
    let mut blend_part = |part: IRect| {
        for y in part.y0..part.y1 {
            let row = surface.row_mut(y, part.x0, part.x1);
            composite_row(op, source, mask, (row, format), (part.x0, y));
        }
        part
    };
    match &operands.clip {
        None => Some(blend_part(area)),
        Some(clip) => clip
            .rects
            .iter()
            .filter_map(|rect| rect.intersect(area))
            .map(&mut blend_part)
            .reduce(IRect::union),
    }
}

/// Blends a copy of the area and writes it back through the clip. For clip rectangles that
/// overlap (a pixel must be blended once) and for a1, where the surface stores one bit.
fn blend_through_copy(
    surface: &mut Surface,
    format: Format,
    op: u8,
    area: IRect,
    operands: &Operands,
) -> Option<IRect> {
    let mut pixels = surface.read_rect(area);
    let (source, mask) = (&operands.source, operands.mask.as_ref());
    composite_area(op, source, mask, (&mut pixels, format), area);
    let clip = operands.clip.as_ref().map(|clip| {
        clip.rects
            .iter()
            .filter_map(|rect| rect.intersect(area))
            .collect()
    });
    surface.set_clip(clip);
    let changed = surface.write_rect(
        (area.x0, area.y0),
        (area.width(), area.height()),
        &pixels,
        FUNCTION_COPY,
        u32::MAX,
    );
    surface.set_clip(None);
    changed
}

/// The clip of a picture that is read, in destination coordinates. `None`: no clip.
pub(super) fn read_clip(picture: &Picture, offset: (i32, i32)) -> Option<Vec<IRect>> {
    picture.clip_rects().map(|rects| {
        rects
            .iter()
            .map(|rect| rect.translate(-offset.0, -offset.1))
            .collect()
    })
}

/// The clip of a request: the clip of the destination cut by the clips of what it reads.
pub(super) fn prepare_clip<const N: usize>(
    target: &Picture,
    reads: [Option<Vec<IRect>>; N],
) -> Result<Option<Clip>, XError> {
    let mut clip = target.clip_rects();
    for read in reads.into_iter().flatten() {
        clip = Some(match clip {
            None => read,
            Some(rects) => intersect_clips(&rects, &read)?,
        });
    }
    Ok(clip.map(|rects| Clip {
        extent: rects.iter().copied().reduce(IRect::union),
        disjoint: are_disjoint(&rects),
        rects,
    }))
}

/// A longer list is taken as overlapping: the test compares each pair.
const MAX_DISJOINT_TEST: usize = 64;

fn are_disjoint(rects: &[IRect]) -> bool {
    rects.len() <= MAX_DISJOINT_TEST
        && rects.iter().enumerate().all(|(index, first)| {
            rects[index + 1..]
                .iter()
                .all(|second| first.intersect(*second).is_none())
        })
}

/// The parts that two clips share.
fn intersect_clips(a: &[IRect], b: &[IRect]) -> Result<Vec<IRect>, XError> {
    if a.len().saturating_mul(b.len()) > MAX_CLIP_PAIRS {
        return Err(XError::Alloc);
    }
    let mut shared = Vec::new();
    for first in a {
        for second in b {
            if let Some(part) = first.intersect(*second) {
                shared.push(part);
                if shared.len() > MAX_CLIP_RECTS {
                    return Err(XError::Alloc);
                }
            }
        }
    }
    Ok(shared)
}
