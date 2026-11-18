// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

use std::borrow::Cow;

use super::codec::Reader;
use super::codec::Writer;
use super::dispatch::Reply;
use super::dispatch::Request;
use super::error::XError;
use super::event::{Event, EXPOSURE};
use super::raster;
use super::resources::{
    pixel_bytes, pixel_mask_for_depth, Gc, IRect, Surface, CAP_BUTT, PIXEL_MASK,
};
use super::screen::{MAX_PIXELS_PER_REQUEST, ROOT_DEPTH, ROOT_VISUAL};
use super::state::{State, CLASS_INPUT_ONLY};
use super::zpixmap;
use wire::BYTES_PER_PIXEL;

const BITMAP_DEPTH: u8 = 1;
const COORD_ORIGIN: u8 = 0;
const COORD_PREVIOUS: u8 = 1;
const FORMAT_BITMAP: u8 = 0;
const FORMAT_XY_PIXMAP: u8 = 1;
const FORMAT_Z_PIXMAP: u8 = 2;
const BITMAP_SCANLINE_PAD: u8 = 32;
const COPY_AREA: u8 = 62;
const COPY_PLANE: u8 = 63;
const MAX_FORMAT: u8 = 2;

type Point = (i32, i32);

/// Coordinates are 16-bit on the wire. Relative sums wrap around, as Xorg does.
/// A clip with more rectangles than this marks the whole bounding box of the request.
const MAX_CLIP_AREAS: usize = 16;

fn wrap16(value: i32) -> i32 {
    i32::from(value as i16)
}

fn read_points(r: &mut Reader, mode: u8) -> Result<Vec<Point>, XError> {
    if mode != COORD_ORIGIN && mode != COORD_PREVIOUS {
        return Err(XError::Value(mode.into()));
    }
    let mut points: Vec<Point> = Vec::new();
    while r.remaining() > 0 {
        let x = i32::from(r.i16()?);
        let y = i32::from(r.i16()?);
        let point = match points.last() {
            Some(&(px, py)) if mode == COORD_PREVIOUS => (wrap16(px + x), wrap16(py + y)),
            _ => (x, y),
        };
        points.push(point);
    }
    Ok(points)
}

fn read_rects(r: &mut Reader) -> Result<Vec<IRect>, XError> {
    let mut rects = Vec::new();
    while r.remaining() > 0 {
        let x = r.i16()?;
        let y = r.i16()?;
        let width = r.u16()?;
        let height = r.u16()?;
        rects.push(IRect::new(x.into(), y.into(), width.into(), height.into()));
    }
    Ok(rects)
}

/// The parts of `wanted` that `visible` does not cover, as up to four rects that do not overlap.
fn missing_parts(wanted: IRect, visible: Option<IRect>) -> Vec<IRect> {
    let Some(visible) = visible else {
        return if wanted.width() > 0 && wanted.height() > 0 {
            vec![wanted]
        } else {
            Vec::new()
        };
    };
    let strips = [
        IRect {
            y1: visible.y0,
            ..wanted
        },
        IRect {
            y0: visible.y1,
            ..wanted
        },
        IRect {
            x1: visible.x0,
            y0: visible.y0,
            y1: visible.y1,
            ..wanted
        },
        IRect {
            x0: visible.x1,
            y0: visible.y0,
            y1: visible.y1,
            ..wanted
        },
    ];
    strips
        .into_iter()
        .filter(|strip| strip.width() > 0 && strip.height() > 0)
        .collect()
}

/// Turns rows of bits (the lowest bit is the leftmost pixel) into BGRX pixels.
pub(super) fn bitmap_pixels(
    bits: &[u8],
    (width, height): (u16, u16),
    (left_pad, stride): (u8, usize),
    pixel_of: impl Fn(bool) -> u32,
) -> Vec<u8> {
    let mut pixels = Vec::with_capacity(usize::from(width) * usize::from(height) * BYTES_PER_PIXEL);
    for row in 0..usize::from(height) {
        for column in 0..usize::from(width) {
            let index = usize::from(left_pad) + column;
            let set = bits[row * stride + index / 8] >> (index % 8) & 1 == 1;
            pixels.extend_from_slice(&pixel_bytes(pixel_of(set)));
        }
    }
    pixels
}

fn union_all(areas: impl Iterator<Item = IRect>) -> Option<IRect> {
    areas.reduce(IRect::union)
}

impl State {
    /// Runs `paint` on the pixels of a drawable and marks the changed part for the screen.
    /// `paint` returns the area that it may have changed, in drawable coordinates.
    pub(super) fn draw(
        &mut self,
        drawable: u32,
        gc_id: u32,
        paint: impl FnOnce(&mut Surface, &Gc) -> Option<IRect>,
    ) -> Result<(), XError> {
        let gc = self.gc(gc_id)?;
        if gc.subwindow_mode != 0 {
            self.log_once("include inferiors".into(), || {
                "xsrv-web: subwindow mode IncludeInferiors is ignored; drawing stays inside the window"
                    .into()
            });
        }
        let clip = self.clips.get(&gc_id).cloned();
        let surface = self.surface_mut(drawable)?;
        if surface.depth() != gc.depth {
            return Err(XError::Match);
        }
        let gc = if surface.depth() == BITMAP_DEPTH {
            gc.on_bitmap()
        } else {
            gc
        };
        surface.set_clip(clip);
        surface.start_work(MAX_PIXELS_PER_REQUEST);
        let changed = paint(surface, &gc);
        let too_much = surface.finish_work();
        let clip_areas = surface.take_clip();
        let changed = changed.and_then(|area| area.intersect(surface.bounds()));
        if let Some(area) = changed {
            match clip_areas.as_deref() {
                // Only the parts inside the clip rectangles changed. Sending each part alone
                // keeps a clip with rectangles far apart from costing one tile over the gap.
                Some(rects) if rects.len() <= MAX_CLIP_AREAS => {
                    for part in rects.iter().filter_map(|rect| rect.intersect(area)) {
                        self.mark_area_dirty(drawable, part);
                    }
                }
                _ => self.mark_area_dirty(drawable, area),
            }
        }
        if too_much {
            return Err(XError::Alloc);
        }
        Ok(())
    }

    fn warn_about_line_style(&mut self, gc_id: u32) -> Result<(), XError> {
        let gc = self.gc(gc_id)?;
        if gc.line_width > 1 {
            self.log_once("wide lines".into(), || {
                "xsrv-web: lines wider than 1 pixel are drawn with a square brush".into()
            });
        }
        if gc.fill_style != 0 {
            self.log_once("fill style lines".into(), || {
                "xsrv-web: lines and text ignore the fill style; only PolyFillRectangle honours it"
                    .into()
            });
        }
        if gc.line_style != 0 || gc.cap_style != CAP_BUTT || gc.join_style != 0 {
            self.log_once("line style".into(), || {
                "xsrv-web: dashes, caps and joins are ignored; lines are solid with butt caps"
                    .into()
            });
        }
        Ok(())
    }

    /// PolyArc and PolyFillArc: a list of arcs of 12 bytes each.
    fn read_arcs(r: &mut Reader) -> Result<Vec<raster::Arc>, XError> {
        let mut arcs = Vec::new();
        while r.remaining() > 0 {
            let (x, y) = (i32::from(r.i16()?), i32::from(r.i16()?));
            let (width, height) = (i32::from(r.u16()?), i32::from(r.u16()?));
            let (start, sweep) = (i32::from(r.i16()?), i32::from(r.i16()?));
            arcs.push(raster::Arc {
                area: IRect::new(x, y, width, height),
                start,
                sweep,
            });
        }
        Ok(arcs)
    }

    pub fn poly_arc(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let drawable = r.u32()?;
        let gc_id = r.u32()?;
        let arcs = Self::read_arcs(&mut r)?;
        self.warn_about_line_style(gc_id)?;
        self.draw(drawable, gc_id, |surface, gc| {
            union_all(
                arcs.iter()
                    .filter_map(|&arc| raster::arc_outline(surface, arc, gc)),
            )
        })
    }

    pub fn poly_fill_arc(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let drawable = r.u32()?;
        let gc_id = r.u32()?;
        let arcs = Self::read_arcs(&mut r)?;
        self.draw(drawable, gc_id, |surface, gc| {
            let pie = gc.arc_mode == 1;
            union_all(
                arcs.iter()
                    .filter_map(|&arc| raster::fill_arc(surface, arc, pie, gc)),
            )
        })
    }

    pub fn fill_poly(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let drawable = r.u32()?;
        let gc_id = r.u32()?;
        let shape = r.u8()?;
        let mode = r.u8()?;
        r.skip(2)?;
        if shape > 2 {
            return Err(XError::Value(shape.into()));
        }
        let points = read_points(&mut r, mode)?;
        self.draw(drawable, gc_id, |surface, gc| {
            raster::fill_polygon(surface, &points, gc.fill_rule == 1, gc)
        })
    }

    pub fn poly_fill_rectangle(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let drawable = r.u32()?;
        let gc_id = r.u32()?;
        let rects = read_rects(&mut r)?;
        let gc = self.gc(gc_id)?;
        let pattern = self.fill_pattern(&gc);
        self.draw(drawable, gc_id, |surface, gc| {
            for &rect in &rects {
                match &pattern {
                    Some(pattern) => raster::fill_pattern(surface, rect, pattern, gc),
                    None => surface.fill_rect(rect, gc.foreground, gc.function, gc.plane_mask),
                }
            }
            union_all(rects.iter().copied())
        })
    }

    pub fn poly_rectangle(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let drawable = r.u32()?;
        let gc_id = r.u32()?;
        let rects = read_rects(&mut r)?;
        self.warn_about_line_style(gc_id)?;
        self.draw(drawable, gc_id, |surface, gc| {
            union_all(
                rects
                    .iter()
                    .map(|&rect| raster::rectangle_outline(surface, rect, gc)),
            )
        })
    }

    pub fn poly_point(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let drawable = r.u32()?;
        let gc_id = r.u32()?;
        let points = read_points(&mut r, req.detail)?;
        self.draw(drawable, gc_id, |surface, gc| {
            union_all(points.iter().map(|&point| raster::plot(surface, point, gc)))
        })
    }

    pub fn poly_line(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let drawable = r.u32()?;
        let gc_id = r.u32()?;
        let points = read_points(&mut r, req.detail)?;
        self.warn_about_line_style(gc_id)?;
        self.draw(drawable, gc_id, |surface, gc| {
            raster::polyline(surface, &points, gc, false)
        })
    }

    pub fn poly_segment(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let drawable = r.u32()?;
        let gc_id = r.u32()?;
        let mut segments = Vec::new();
        while r.remaining() > 0 {
            let from = (i32::from(r.i16()?), i32::from(r.i16()?));
            let to = (i32::from(r.i16()?), i32::from(r.i16()?));
            segments.push((from, to));
        }
        self.warn_about_line_style(gc_id)?;
        self.draw(drawable, gc_id, |surface, gc| {
            union_all(
                segments
                    .iter()
                    .map(|&(from, to)| raster::line(surface, from, to, gc)),
            )
        })
    }

    /// Sends `NoExpose`, or `GraphicsExpose` for the parts of the source that lie outside it.
    /// Windows keep their own pixels, so no part of a source window is ever hidden.
    pub fn copy_area(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let source = r.u32()?;
        let target = r.u32()?;
        let gc_id = r.u32()?;
        let source_at = (i32::from(r.i16()?), i32::from(r.i16()?));
        let target_at = (i32::from(r.i16()?), i32::from(r.i16()?));
        let (width, height) = (i32::from(r.u16()?), i32::from(r.u16()?));
        r.end()?;
        let gc = self.gc(gc_id)?;
        let (source_bounds, source_depth) = {
            let surface = self.surface(source)?;
            (surface.bounds(), surface.depth())
        };
        let (target_bounds, target_depth) = {
            let surface = self.surface(target)?;
            (surface.bounds(), surface.depth())
        };
        if source_depth != target_depth || gc.depth != target_depth {
            return Err(XError::Match);
        }
        let gc = if target_depth == BITMAP_DEPTH {
            gc.on_bitmap()
        } else {
            gc
        };

        let wanted = IRect::new(source_at.0, source_at.1, width, height);
        let visible = wanted.intersect(source_bounds);
        let shift = (target_at.0 - source_at.0, target_at.1 - source_at.1);
        if let Some(visible) = visible {
            let moved = visible.translate(shift.0, shift.1);
            let plain = gc.is_plain_copy() && !self.clips.contains_key(&gc_id);
            // A scroll inside a window: the page moves its own pixels, before the window changes.
            let on_screen = source == target
                && plain
                && moved.intersect(target_bounds) == Some(moved)
                && self.copy_on_screen(target, visible, shift);
            self.copy_pixels((source, visible), (target, shift), (&gc, gc_id), !on_screen)?;
        }
        self.report_copy(
            req,
            &gc,
            target,
            (wanted, visible),
            shift,
            target_bounds,
            COPY_AREA,
        );
        Ok(())
    }

    /// Copies one bit plane of the source. Set bits become the foreground, clear bits the background.
    pub fn copy_plane(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let source = r.u32()?;
        let target = r.u32()?;
        let gc_id = r.u32()?;
        let source_at = (i32::from(r.i16()?), i32::from(r.i16()?));
        let target_at = (i32::from(r.i16()?), i32::from(r.i16()?));
        let (width, height) = (i32::from(r.u16()?), i32::from(r.u16()?));
        let plane = r.u32()?;
        r.end()?;
        let gc = self.gc(gc_id)?;
        let (source_bounds, source_depth) = {
            let surface = self.surface(source)?;
            (surface.bounds(), surface.depth())
        };
        let (target_bounds, target_depth) = {
            let surface = self.surface(target)?;
            (surface.bounds(), surface.depth())
        };
        if gc.depth != target_depth {
            return Err(XError::Match);
        }
        if plane.count_ones() != 1 || u64::from(plane) >= 1 << source_depth {
            return Err(XError::Value(plane));
        }
        let gc = if target_depth == BITMAP_DEPTH {
            gc.on_bitmap()
        } else {
            gc
        };

        let wanted = IRect::new(source_at.0, source_at.1, width, height);
        let visible = wanted.intersect(source_bounds);
        if let Some(visible) = visible {
            let source_pixels = self.surface(source)?.read_rect(visible);
            let (on, off) = (pixel_bytes(gc.foreground), pixel_bytes(gc.background));
            let mut pixels = Vec::with_capacity(source_pixels.len());
            let source_mask = pixel_mask_for_depth(source_depth);
            for pixel in source_pixels.as_chunks::<BYTES_PER_PIXEL>().0 {
                let value = u32::from_le_bytes(*pixel) & source_mask;
                let set = if source_depth == BITMAP_DEPTH {
                    value != 0
                } else {
                    value & plane != 0
                };
                pixels.extend_from_slice(if set { &on } else { &off });
            }
            let origin = (
                target_at.0 + visible.x0 - source_at.0,
                target_at.1 + visible.y0 - source_at.1,
            );
            let size = (visible.width(), visible.height());
            self.write_copy(target, origin, size, &pixels, (&gc, gc_id), true)?;
        }
        let shift = (target_at.0 - source_at.0, target_at.1 - source_at.1);
        self.report_copy(
            req,
            &gc,
            target,
            (wanted, visible),
            shift,
            target_bounds,
            COPY_PLANE,
        );
        Ok(())
    }

    /// Copies the area `from` of `source`, moved by `shift`, into `target`, without a buffer
    /// between the two: a scroll or a redraw from a pixmap copies each pixel once.
    fn copy_pixels(
        &mut self,
        (source, from): (u32, IRect),
        (target, shift): (u32, (i32, i32)),
        (gc, gc_id): (&Gc, u32),
        mark_dirty: bool,
    ) -> Result<(), XError> {
        let clip = self.clips.get(&gc_id).cloned();
        let origin = (from.x0 + shift.0, from.y0 + shift.1);
        let (changed, too_much) = if source != target {
            let (source, surface) = self.surface_pair(source, target)?;
            surface.set_clip(clip);
            surface.start_work(MAX_PIXELS_PER_REQUEST);
            let changed = surface.copy_from(source, from, origin, gc.function, gc.plane_mask);
            surface.set_clip(None);
            (changed, surface.finish_work())
        } else {
            let surface = self.surface_mut(target)?;
            surface.start_work(MAX_PIXELS_PER_REQUEST);
            let changed = if gc.is_plain_copy() && clip.is_none() {
                surface.copy_inside(from, shift)
            } else {
                let pixels = surface.read_rect(from);
                let size = (from.width(), from.height());
                surface.set_clip(clip);
                let changed = surface.write_rect(origin, size, &pixels, gc.function, gc.plane_mask);
                surface.set_clip(None);
                changed
            };
            (changed, surface.finish_work())
        };
        if let Some(area) = changed.filter(|_| mark_dirty) {
            self.mark_area_dirty(target, area);
        }
        if too_much {
            return Err(XError::Alloc);
        }
        Ok(())
    }

    fn write_copy(
        &mut self,
        target: u32,
        origin: (i32, i32),
        size: (i32, i32),
        pixels: &[u8],
        (gc, gc_id): (&Gc, u32),
        mark_dirty: bool,
    ) -> Result<(), XError> {
        let clip = self.clips.get(&gc_id).cloned();
        let mut too_much = false;
        let changed = self.surface_mut(target).ok().and_then(|surface| {
            surface.set_clip(clip);
            surface.start_work(MAX_PIXELS_PER_REQUEST);
            let changed = surface.write_rect(origin, size, pixels, gc.function, gc.plane_mask);
            too_much = surface.finish_work();
            surface.set_clip(None);
            changed
        });
        if let Some(area) = changed.filter(|_| mark_dirty) {
            self.mark_area_dirty(target, area);
        }
        if too_much {
            return Err(XError::Alloc);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn report_copy(
        &self,
        req: &Request,
        gc: &Gc,
        target: u32,
        (wanted, visible): (IRect, Option<IRect>),
        (shift_x, shift_y): (i32, i32),
        target_bounds: IRect,
        major_opcode: u8,
    ) {
        if !gc.graphics_exposures {
            return;
        }
        let missing = missing_parts(wanted, visible)
            .into_iter()
            .filter_map(|part| part.translate(shift_x, shift_y).intersect(target_bounds))
            .collect::<Vec<_>>();
        self.send_copy_exposures(req.client, target, &missing, major_opcode);
    }

    fn send_copy_exposures(&self, client: u32, drawable: u32, missing: &[IRect], major_opcode: u8) {
        if missing.is_empty() {
            let event = Event::NoExpose {
                drawable,
                major_opcode,
            };
            self.send_event_to(client, &event);
        }
        for (index, area) in missing.iter().enumerate() {
            let event = Event::GraphicsExpose {
                drawable,
                x: area.x0 as u16,
                y: area.y0 as u16,
                width: area.width() as u16,
                height: area.height() as u16,
                count: (missing.len() - 1 - index) as u16,
                major_opcode,
            };
            self.send_event_to(client, &event);
        }
    }

    /// Draws ZPixmap images of the depth of the drawable (1, 8, 24 or 32), Bitmap images, and
    /// XYPixmap images of depth 1 and 24. XYPixmap of depth 8 or 32 gets BadImplementation.
    pub fn put_image(&mut self, req: &Request) -> Result<(), XError> {
        let format = req.detail;
        let mut r = req.reader();
        let drawable = r.u32()?;
        let gc_id = r.u32()?;
        let width = r.u16()?;
        let height = r.u16()?;
        let x = i32::from(r.i16()?);
        let y = i32::from(r.i16()?);
        let left_pad = r.u8()?;
        let depth = r.u8()?;
        r.skip(2)?;
        if format > MAX_FORMAT {
            return Err(XError::Value(format.into()));
        }
        let size = (i32::from(width), i32::from(height));
        match (format, depth) {
            (FORMAT_Z_PIXMAP, 8 | ROOT_DEPTH | 32) if left_pad == 0 => {
                if self.surface(drawable)?.depth() != depth {
                    return Err(XError::Match);
                }
                let columns = usize::from(width);
                let stride = zpixmap::stride(depth, columns).ok_or(XError::Length)?;
                let len = stride
                    .checked_mul(usize::from(height))
                    .ok_or(XError::Length)?;
                let image = r.bytes(len)?;
                r.end()?;
                // A row of 4-byte pixels has no padding, so the image is laid out as the surface
                // wants it. The surface masks each pixel to its depth when it writes.
                let pixels = if zpixmap::bytes_per_pixel(depth) == Some(BYTES_PER_PIXEL) {
                    Cow::Borrowed(image)
                } else {
                    Cow::Owned(zpixmap::unpack(depth, image, columns))
                };
                self.draw(drawable, gc_id, |surface, gc| {
                    surface.write_rect((x, y), size, &pixels, gc.function, gc.plane_mask)
                })
            }
            // With one plane, XYPixmap is the same as ZPixmap: the bits are the pixel values.
            (FORMAT_BITMAP | FORMAT_Z_PIXMAP | FORMAT_XY_PIXMAP, BITMAP_DEPTH)
                if left_pad < BITMAP_SCANLINE_PAD =>
            {
                if format != FORMAT_BITMAP && self.surface(drawable)?.depth() != depth {
                    return Err(XError::Match);
                }
                let columns = usize::from(left_pad) + usize::from(width);
                let stride = columns.div_ceil(BITMAP_SCANLINE_PAD as usize) * 4;
                let len = stride
                    .checked_mul(usize::from(height))
                    .ok_or(XError::Length)?;
                let bits = r.bytes(len)?;
                r.end()?;
                let direct = format != FORMAT_BITMAP;
                self.draw(drawable, gc_id, |surface, gc| {
                    let pixels = bitmap_pixels(bits, (width, height), (left_pad, stride), |bit| {
                        match (bit, direct) {
                            (true, true) => PIXEL_MASK,
                            (false, true) => 0,
                            (true, false) => gc.foreground,
                            (false, false) => gc.background,
                        }
                    });
                    surface.write_rect((x, y), size, &pixels, gc.function, gc.plane_mask)
                })
            }
            (FORMAT_XY_PIXMAP, ROOT_DEPTH) if left_pad < BITMAP_SCANLINE_PAD => {
                if self.surface(drawable)?.depth() != depth {
                    return Err(XError::Match);
                }
                // One bitmap for each of the 24 planes, the most significant plane first.
                let columns = usize::from(left_pad) + usize::from(width);
                let stride = columns.div_ceil(BITMAP_SCANLINE_PAD as usize) * 4;
                let plane_len = stride
                    .checked_mul(usize::from(height))
                    .ok_or(XError::Length)?;
                let len = plane_len
                    .checked_mul(usize::from(ROOT_DEPTH))
                    .ok_or(XError::Length)?;
                let bits = r.bytes(len)?;
                r.end()?;
                self.draw(drawable, gc_id, |surface, gc| {
                    let mut values = vec![0u32; usize::from(width) * usize::from(height)];
                    for (plane, bytes) in bits.chunks(plane_len).enumerate() {
                        let position = u32::from(ROOT_DEPTH) - 1 - plane as u32;
                        let pixels =
                            bitmap_pixels(bytes, (width, height), (left_pad, stride), |bit| {
                                u32::from(bit)
                            });
                        for (value, pixel) in values
                            .iter_mut()
                            .zip(pixels.as_chunks::<BYTES_PER_PIXEL>().0)
                        {
                            *value |= (u32::from_le_bytes(*pixel) & 1) << position;
                        }
                    }
                    let pixels: Vec<u8> = values
                        .iter()
                        .flat_map(|value| pixel_bytes(*value))
                        .collect();
                    surface.write_rect((x, y), size, &pixels, gc.function, gc.plane_mask)
                })
            }
            (FORMAT_XY_PIXMAP, 8 | 32) => Err(XError::Implementation),
            _ => Err(XError::Match),
        }
    }

    /// Reads pixels back. A window gives its own pixels, without its children.
    /// Only the ZPixmap format works: depth 24 as BGRX, depth 1 as packed bits.
    pub fn get_image(&self, req: &Request) -> Result<Reply, XError> {
        let format = req.detail;
        let mut r = req.reader();
        let drawable = r.u32()?;
        let x = i32::from(r.i16()?);
        let y = i32::from(r.i16()?);
        let (width, height) = (i32::from(r.u16()?), i32::from(r.u16()?));
        let plane_mask = r.u32()?;
        r.end()?;
        if format != FORMAT_Z_PIXMAP {
            return Err(if format > MAX_FORMAT {
                XError::Value(format.into())
            } else {
                XError::Implementation
            });
        }
        let surface = self.surface(drawable)?;
        let area = IRect::new(x, y, width, height);
        if area.x0 < 0
            || area.y0 < 0
            || area.x1 > surface.bounds().x1
            || area.y1 > surface.bounds().y1
        {
            return Err(XError::Match);
        }
        let mut w = Writer::new(req.endian);
        w.u32(if self.window_exists(drawable) {
            ROOT_VISUAL
        } else {
            0
        });
        w.zeros(20);
        if width > 0 && height > 0 {
            // A window shows what is on the screen, with its children and their frames, as a
            // screenshot tool expects. A pixmap has only its own pixels.
            let pixels = if self.window_exists(drawable) && self.is_viewable(drawable) {
                let (origin_x, origin_y) = self.window_origin(drawable).unwrap_or((0, 0));
                self.display
                    .read_region(origin_x + x, origin_y + y, width, height)
            } else {
                surface.read_rect(area)
            };
            if surface.depth() == BITMAP_DEPTH {
                let columns = usize::try_from(width).unwrap_or(0);
                let stride = columns.div_ceil(BITMAP_SCANLINE_PAD as usize) * 4;
                for row in pixels.chunks(columns * BYTES_PER_PIXEL) {
                    let mut packed = vec![0u8; stride];
                    for (column, pixel) in row.as_chunks::<BYTES_PER_PIXEL>().0.iter().enumerate() {
                        let set =
                            u32::from_le_bytes(*pixel) & PIXEL_MASK != 0 && plane_mask & 1 == 1;
                        packed[column / 8] |= u8::from(set) << (column % 8);
                    }
                    w.bytes(&packed);
                }
            } else {
                let columns = usize::try_from(width).unwrap_or(0);
                w.bytes(&zpixmap::pack(
                    surface.depth(),
                    &pixels,
                    columns,
                    plane_mask,
                ));
            }
        }
        Ok(Reply::new(surface.depth(), w))
    }

    pub fn clear_area(&mut self, req: &Request) -> Result<(), XError> {
        let exposures = req.detail != 0;
        let mut r = req.reader();
        let id = r.u32()?;
        let x = i32::from(r.i16()?);
        let y = i32::from(r.i16()?);
        let width = r.u16()?;
        let height = r.u16()?;
        r.end()?;
        let window = self.window(id)?;
        if window.class == CLASS_INPUT_ONLY {
            return Err(XError::Match);
        }
        let bounds = window.surface.bounds();
        let width = if width == 0 {
            bounds.width() - x
        } else {
            width.into()
        };
        let height = if height == 0 {
            bounds.height() - y
        } else {
            height.into()
        };
        let area = IRect::new(x, y, width, height);
        self.fill_background(id, area)?;
        if let Some(visible) = area.intersect(bounds) {
            self.mark_area_dirty(id, visible);
            if exposures && self.is_viewable(id) {
                self.notify_listeners(
                    id,
                    EXPOSURE,
                    &Event::Expose {
                        window: id,
                        x: visible.x0 as u16,
                        y: visible.y0 as u16,
                        width: visible.width() as u16,
                        height: visible.height() as u16,
                    },
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_missing_when_the_source_is_inside() {
        let wanted = IRect::new(2, 2, 4, 4);
        assert!(missing_parts(wanted, Some(wanted)).is_empty());
    }

    #[test]
    fn missing_parts_do_not_overlap_and_cover_the_rest() {
        let wanted = IRect::new(-2, -3, 10, 10);
        let visible = wanted.intersect(IRect::new(0, 0, 5, 5)).unwrap();
        let parts = missing_parts(wanted, Some(visible));
        let area: i32 = parts.iter().map(|p| p.width() * p.height()).sum();
        assert_eq!(area, 10 * 10 - visible.width() * visible.height());
        assert_eq!(parts.len(), 4);
    }

    #[test]
    fn a_source_fully_outside_is_one_missing_part() {
        let wanted = IRect::new(100, 100, 3, 3);
        assert_eq!(missing_parts(wanted, None), vec![wanted]);
        assert!(missing_parts(IRect::new(0, 0, 0, 5), None).is_empty());
    }
}
