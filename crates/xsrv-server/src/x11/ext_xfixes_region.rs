// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! The region requests of XFIXES: server-side regions that clients build and then use as clips
//! and shapes. A region is kept as a list of rectangles that do not overlap.
use super::codec::Writer;
use super::dispatch::{Reply, Request};
use super::error::XError;
use super::extensions::{first_error, XFIXES};
use super::region;
use super::resources::IRect;
use super::state::State;

/// The most rectangles that one region may have.
const MAX_REGION_RECTS: usize = 8192;

impl State {
    fn region(&self, id: u32) -> Result<&Vec<IRect>, XError> {
        self.regions
            .get(&id)
            .ok_or(XError::Extension(first_error(XFIXES), id))
    }

    fn region_checked(&self, id: u32) -> Result<(), XError> {
        self.region(id).map(|_| ())
    }

    fn new_region_id(&mut self, req: &Request, id: u32) -> Result<(), XError> {
        if !self.owns_id(req.client, id) || self.id_in_use(id) {
            return Err(XError::IdChoice(id));
        }
        self.check_resource_quota(req.client)
    }

    fn store_region(&mut self, id: u32, rects: Vec<IRect>) -> Result<(), XError> {
        if rects.len() > MAX_REGION_RECTS {
            return Err(XError::Alloc);
        }
        let rects = region::normalize(&rects);
        if rects.len() > MAX_REGION_RECTS {
            return Err(XError::Alloc);
        }
        self.regions.insert(id, rects);
        Ok(())
    }

    fn add_region(&mut self, req: &Request, id: u32, rects: Vec<IRect>) -> Result<(), XError> {
        self.new_region_id(req, id)?;
        self.store_region(id, rects)?;
        self.owners.insert(id, req.client);
        Ok(())
    }

    pub(super) fn xfixes_region_request(&mut self, req: &Request) -> Result<Option<Reply>, XError> {
        let mut r = req.reader();
        match req.detail {
            5 => {
                let id = r.u32()?;
                let rects = read_rects(&mut r, 0, 0)?;
                self.add_region(req, id, rects)?;
            }
            6 => {
                let (id, bitmap) = (r.u32()?, r.u32()?);
                r.end()?;
                let surface = &self
                    .pixmaps
                    .get(&bitmap)
                    .ok_or(XError::Pixmap(bitmap))?
                    .surface;
                if surface.depth() != 1 {
                    return Err(XError::Match);
                }
                let rects = region::from_bitmap(surface);
                self.add_region(req, id, rects)?;
            }
            7 => {
                let (id, window, kind) = (r.u32()?, r.u32()?, r.u8()?);
                r.skip(3)?;
                r.end()?;
                self.window(window)?;
                let rects = self.shape_region(window, kind)?;
                self.add_region(req, id, rects)?;
            }
            8 => {
                let (id, gc) = (r.u32()?, r.u32()?);
                r.end()?;
                self.gc(gc)?;
                let rects = self.clips.get(&gc).cloned().ok_or(XError::Match)?;
                self.add_region(req, id, rects)?;
            }
            9 => {
                let (id, picture) = (r.u32()?, r.u32()?);
                r.end()?;
                let rects = self.picture_clip(picture)?.ok_or(XError::Match)?;
                self.add_region(req, id, rects)?;
            }
            10 => {
                let id = r.u32()?;
                r.end()?;
                self.region_checked(id)?;
                self.regions.remove(&id);
                self.owners.remove(&id);
            }
            11 => {
                let id = r.u32()?;
                let rects = read_rects(&mut r, 0, 0)?;
                self.region_checked(id)?;
                self.store_region(id, rects)?;
            }
            12..=15 => self.combine_regions(req.detail, &mut r)?,
            16 => {
                let source = r.u32()?;
                let bounds = read_rect(&mut r, 0, 0)?;
                let destination = r.u32()?;
                r.end()?;
                let rects = region::subtract(&[bounds], self.region(source)?, MAX_REGION_RECTS)
                    .ok_or(XError::Alloc)?;
                self.region_checked(destination)?;
                self.store_region(destination, rects)?;
            }
            17 => {
                let id = r.u32()?;
                let (dx, dy) = (i32::from(r.i16()?), i32::from(r.i16()?));
                r.end()?;
                let moved = region::translate(self.region(id)?, dx, dy);
                self.regions.insert(id, moved);
            }
            18 => {
                let (source, destination) = (r.u32()?, r.u32()?);
                r.end()?;
                let rects: Vec<IRect> = region::extents(self.region(source)?).into_iter().collect();
                self.region_checked(destination)?;
                self.store_region(destination, rects)?;
            }
            19 => return self.fetch_region(req, &mut r).map(Some),
            20 => {
                let (gc, id) = (r.u32()?, r.u32()?);
                let (x, y) = (i32::from(r.i16()?), i32::from(r.i16()?));
                r.end()?;
                self.gc(gc)?;
                if id == 0 {
                    self.clips.remove(&gc);
                } else {
                    let rects = region::translate(self.region(id)?, x, y);
                    self.clips.insert(gc, rects);
                }
            }
            21 => {
                let (window, kind) = (r.u32()?, r.u8()?);
                r.skip(3)?;
                let (x, y) = (i32::from(r.i16()?), i32::from(r.i16()?));
                let id = r.u32()?;
                r.end()?;
                self.window(window)?;
                if id == 0 {
                    self.clear_shape(window, kind)?;
                } else {
                    let rects = region::translate(self.region(id)?, x, y);
                    self.apply_shape(window, kind, 0, rects)?;
                }
            }
            22 => {
                let (picture, id) = (r.u32()?, r.u32()?);
                let origin = (i32::from(r.i16()?), i32::from(r.i16()?));
                r.end()?;
                let rects = if id == 0 {
                    None
                } else {
                    Some(self.region(id)?.clone())
                };
                self.set_picture_clip(picture, rects, origin)?;
            }
            28 => {
                let (source, destination) = (r.u32()?, r.u32()?);
                let [left, right, top, bottom] =
                    [r.u16()?, r.u16()?, r.u16()?, r.u16()?].map(i32::from);
                r.end()?;
                let grown: Vec<IRect> = self
                    .region(source)?
                    .iter()
                    .map(|rect| {
                        IRect::new(
                            rect.x0 - left,
                            rect.y0 - top,
                            rect.width() + left + right,
                            rect.height() + top + bottom,
                        )
                    })
                    .collect();
                self.region_checked(destination)?;
                self.store_region(destination, grown)?;
            }
            _ => return Err(XError::Request),
        }
        Ok(None)
    }

    /// CopyRegion, UnionRegion, IntersectRegion and SubtractRegion.
    fn combine_regions(&mut self, opcode: u8, r: &mut super::codec::Reader) -> Result<(), XError> {
        let (first, second, destination) = if opcode == 12 {
            let (source, destination) = (r.u32()?, r.u32()?);
            (source, source, destination)
        } else {
            (r.u32()?, r.u32()?, r.u32()?)
        };
        r.end()?;
        let (a, b) = (self.region(first)?, self.region(second)?);
        let result = match opcode {
            12 => a.clone(),
            13 => region::union(a, b),
            14 => region::intersect(a, b, MAX_REGION_RECTS).ok_or(XError::Alloc)?,
            _ => region::subtract(a, b, MAX_REGION_RECTS).ok_or(XError::Alloc)?,
        };
        self.region_checked(destination)?;
        self.store_region(destination, result)
    }

    fn fetch_region(&self, req: &Request, r: &mut super::codec::Reader) -> Result<Reply, XError> {
        let id = r.u32()?;
        r.end()?;
        let rects = self.region(id)?;
        let extents = region::extents(rects).unwrap_or_else(|| IRect::new(0, 0, 0, 0));
        let mut w = Writer::new(req.endian);
        write_rect(&mut w, extents);
        w.zeros(16);
        for rect in rects {
            write_rect(&mut w, *rect);
        }
        Ok(Reply::new(0, w))
    }

    pub(super) fn forget_region(&mut self, id: u32) {
        self.regions.remove(&id);
    }
}

fn write_rect(w: &mut Writer, rect: IRect) {
    w.i16(rect.x0 as i16);
    w.i16(rect.y0 as i16);
    w.u16(rect.width() as u16);
    w.u16(rect.height() as u16);
}

fn read_rect(r: &mut super::codec::Reader, dx: i32, dy: i32) -> Result<IRect, XError> {
    let (x, y) = (i32::from(r.i16()?), i32::from(r.i16()?));
    let (width, height) = (i32::from(r.u16()?), i32::from(r.u16()?));
    Ok(IRect::new(x + dx, y + dy, width, height))
}

/// The rectangles that fill the rest of a request. A rectangle with no area is dropped.
fn read_rects(r: &mut super::codec::Reader, dx: i32, dy: i32) -> Result<Vec<IRect>, XError> {
    if !r.remaining().is_multiple_of(8) {
        return Err(XError::Length);
    }
    if r.remaining() / 8 > MAX_REGION_RECTS {
        return Err(XError::Alloc);
    }
    let mut rects = Vec::with_capacity(r.remaining() / 8);
    while r.remaining() > 0 {
        let rect = read_rect(r, dx, dy)?;
        if rect.width() > 0 && rect.height() > 0 {
            rects.push(rect);
        }
    }
    Ok(rects)
}
