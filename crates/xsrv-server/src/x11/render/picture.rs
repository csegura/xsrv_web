// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! Pictures: a drawable with a format, a repeat mode, component alpha and a clip.
//!
//! A pixmap can be freed while pictures still use it (cairo does that with every temporary
//! source). Its surface then moves to `RenderState::orphans` until the last picture is freed. A
//! picture on a window dies with the window. `RenderState::by_target` lists the pictures of each
//! drawable, so that none of this scans all pictures.
use super::composite::Repeat;
use super::format::Format;
use super::RenderError;
use crate::x11::codec::Reader;
use crate::x11::dispatch::Request;
use crate::x11::error::XError;
use crate::x11::resources::{IRect, Surface, PIXEL_MASK};
use crate::x11::state::{ClientId, State};
use wire::BYTES_PER_PIXEL;

/// The most rectangles of one picture clip.
const MAX_CLIP_RECTS: usize = 1 << 16;
/// The most clip rectangles of all pictures together (16 bytes each).
const MAX_TOTAL_CLIP_RECTS: usize = 1 << 20;

const CP_REPEAT: u32 = 1 << 0;
const CP_ALPHA_MAP: u32 = 1 << 1;
const CP_CLIP_X_ORIGIN: u32 = 1 << 4;
const CP_CLIP_Y_ORIGIN: u32 = 1 << 5;
const CP_CLIP_MASK: u32 = 1 << 6;
const CP_GRAPHICS_EXPOSURE: u32 = 1 << 7;
const CP_SUBWINDOW_MODE: u32 = 1 << 8;
const CP_POLY_EDGE: u32 = 1 << 9;
const CP_POLY_MODE: u32 = 1 << 10;
const CP_COMPONENT_ALPHA: u32 = 1 << 12;
const CP_ALL: u32 = (1 << 13) - 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    Window(u32),
    Pixmap(u32),
    Orphan(u64),
}

#[derive(Clone, Debug)]
pub struct Picture {
    pub target: Target,
    pub format: Format,
    pub repeat: Repeat,
    pub component_alpha: bool,
    /// A window picture that includes inferiors is read from the screen, with its children.
    pub include_inferiors: bool,
    /// Rectangles in picture coordinates before `clip_origin`. `None`: no clip.
    pub clip: Option<Vec<IRect>>,
    pub clip_origin: (i32, i32),
}

impl Picture {
    /// The clip in drawable coordinates.
    pub fn clip_rects(&self) -> Option<Vec<IRect>> {
        let (x, y) = self.clip_origin;
        self.clip
            .as_ref()
            .map(|rects| rects.iter().map(|rect| rect.translate(x, y)).collect())
    }

    fn clip_len(&self) -> usize {
        self.clip.as_ref().map_or(0, Vec::len)
    }
}

pub struct Orphan {
    pub surface: Surface,
    owner: Option<ClientId>,
}

impl State {
    pub(super) fn create_picture(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let id = r.u32()?;
        let drawable = r.u32()?;
        let format_id = r.u32()?;
        let mask = r.u32()?;
        if !self.owns_id(req.client, id) || self.id_in_use(id) {
            return Err(XError::IdChoice(id));
        }
        let format = Format::from_id(format_id).ok_or(RenderError::PictFormat.with(format_id))?;
        if !self.drawable_exists(drawable) {
            return Err(XError::Drawable(drawable));
        }
        if self.surface(drawable)?.depth() != format.depth() {
            return Err(XError::Match);
        }
        let target = if self.window_exists(drawable) {
            Target::Window(drawable)
        } else {
            Target::Pixmap(drawable)
        };
        let mut picture = Picture {
            target,
            format,
            repeat: Repeat::None,
            component_alpha: false,
            include_inferiors: false,
            clip: None,
            clip_origin: (0, 0),
        };
        self.apply_picture_values(&mut picture, mask, &mut r)?;
        r.end()?;
        self.check_resource_quota(req.client)?;
        self.store_picture(id, picture)?;
        self.owners.insert(id, req.client);
        Ok(())
    }

    pub(super) fn change_picture(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let id = r.u32()?;
        let mask = r.u32()?;
        let mut picture = self.picture(id)?.clone();
        self.apply_picture_values(&mut picture, mask, &mut r)?;
        r.end()?;
        self.store_picture(id, picture)
    }

    /// Reads the values of `mask`, in bit order. Graphics exposures, poly edge and poly mode are
    /// checked and kept nowhere: RENDER 0.1 has no request that uses them here. Dither is an atom
    /// that the protocol does not check.
    fn apply_picture_values(
        &self,
        picture: &mut Picture,
        mask: u32,
        r: &mut Reader,
    ) -> Result<(), XError> {
        if mask & !CP_ALL != 0 {
            return Err(XError::Value(mask));
        }
        for bit in 0..13 {
            let flag = 1 << bit;
            if mask & flag == 0 {
                continue;
            }
            let value = r.u32()?;
            match flag {
                CP_REPEAT => {
                    picture.repeat = Repeat::from_value(value).ok_or(XError::Value(value))?
                }
                CP_ALPHA_MAP if value != 0 => return Err(XError::Implementation),
                CP_CLIP_X_ORIGIN => picture.clip_origin.0 = i32::from(value as i16),
                CP_CLIP_Y_ORIGIN => picture.clip_origin.1 = i32::from(value as i16),
                CP_CLIP_MASK => picture.clip = self.clip_from_pixmap(value)?,
                CP_COMPONENT_ALPHA => picture.component_alpha = boolean(value)?,
                // 1 is IncludeInferiors.
                CP_SUBWINDOW_MODE => picture.include_inferiors = boolean(value)?,
                CP_GRAPHICS_EXPOSURE | CP_POLY_EDGE | CP_POLY_MODE => {
                    boolean(value)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// The set pixels of a depth-1 pixmap as rows of rectangles. 0 means no clip.
    fn clip_from_pixmap(&self, pixmap: u32) -> Result<Option<Vec<IRect>>, XError> {
        if pixmap == 0 {
            return Ok(None);
        }
        let surface = &self
            .pixmaps
            .get(&pixmap)
            .ok_or(XError::Pixmap(pixmap))?
            .surface;
        if surface.depth() != 1 {
            return Err(XError::Match);
        }
        let bounds = surface.bounds();
        let mut rects = Vec::new();
        for y in bounds.y0..bounds.y1 {
            let row = surface.row(y, bounds.x0, bounds.x1);
            let set = |x: i32| {
                let at = x as usize * BYTES_PER_PIXEL;
                u32::from_le_bytes([row[at], row[at + 1], row[at + 2], row[at + 3]]) & PIXEL_MASK
                    != 0
            };
            let mut x = bounds.x0;
            while x < bounds.x1 {
                if !set(x) {
                    x += 1;
                    continue;
                }
                let start = x;
                while x < bounds.x1 && set(x) {
                    x += 1;
                }
                rects.push(IRect::new(start, y, x - start, 1));
                if rects.len() > MAX_CLIP_RECTS {
                    return Err(XError::Alloc);
                }
            }
        }
        Ok(Some(rects))
    }

    pub(super) fn set_picture_clip_rectangles(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let id = r.u32()?;
        let origin = (i32::from(r.i16()?), i32::from(r.i16()?));
        if !r.remaining().is_multiple_of(8) {
            return Err(XError::Length);
        }
        if r.remaining() / 8 > MAX_CLIP_RECTS {
            return Err(XError::Alloc);
        }
        let mut picture = self.picture(id)?.clone();
        let mut rects = Vec::with_capacity(r.remaining() / 8);
        while r.remaining() > 0 {
            let (x, y) = (i32::from(r.i16()?), i32::from(r.i16()?));
            let (width, height) = (i32::from(r.u16()?), i32::from(r.u16()?));
            if width > 0 && height > 0 {
                rects.push(IRect::new(x, y, width, height));
            }
        }
        picture.clip = Some(rects);
        picture.clip_origin = origin;
        self.store_picture(id, picture)
    }

    /// The clip of a picture, for XFIXES. `None` when the picture has no clip.
    pub(in crate::x11) fn picture_clip(&self, id: u32) -> Result<Option<Vec<IRect>>, XError> {
        Ok(self.picture(id)?.clip.clone())
    }

    /// Sets or removes the clip of a picture, for XFIXES.
    pub(in crate::x11) fn set_picture_clip(
        &mut self,
        id: u32,
        clip: Option<Vec<IRect>>,
        origin: (i32, i32),
    ) -> Result<(), XError> {
        let mut picture = self.picture(id)?.clone();
        picture.clip = clip;
        picture.clip_origin = origin;
        self.store_picture(id, picture)
    }

    pub(super) fn free_picture(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let id = r.u32()?;
        r.end()?;
        self.picture(id)?;
        self.drop_picture(id);
        Ok(())
    }

    pub(super) fn picture(&self, id: u32) -> Result<&Picture, XError> {
        self.render
            .pictures
            .get(&id)
            .ok_or(RenderError::Picture.with(id))
    }

    pub(super) fn picture_surface(&self, target: Target) -> Option<&Surface> {
        match target {
            Target::Window(id) | Target::Pixmap(id) => self.surface(id).ok(),
            Target::Orphan(key) => self.render.orphans.get(&key).map(|orphan| &orphan.surface),
        }
    }

    pub(super) fn picture_surface_mut(&mut self, target: Target) -> Option<&mut Surface> {
        match target {
            Target::Window(id) | Target::Pixmap(id) => self.surface_mut(id).ok(),
            Target::Orphan(key) => self
                .render
                .orphans
                .get_mut(&key)
                .map(|orphan| &mut orphan.surface),
        }
    }

    /// Adds or replaces a picture, within the limit of all clip rectangles.
    fn store_picture(&mut self, id: u32, picture: Picture) -> Result<(), XError> {
        let render = &mut self.render;
        let old = render.pictures.get(&id).map_or(0, Picture::clip_len);
        let total = render.clip_rects - old + picture.clip_len();
        if total > MAX_TOTAL_CLIP_RECTS {
            return Err(XError::Alloc);
        }
        render.clip_rects = total;
        if !render.pictures.contains_key(&id) {
            render.by_target.entry(picture.target).or_default().push(id);
        }
        render.pictures.insert(id, picture);
        Ok(())
    }

    /// Removes a picture, and the orphan surface that only it used.
    fn drop_picture(&mut self, id: u32) {
        let Some(picture) = self.render.pictures.remove(&id) else {
            return;
        };
        self.owners.remove(&id);
        self.render.clip_rects -= picture.clip_len();
        let ids = self.render.by_target.entry(picture.target).or_default();
        ids.retain(|other| *other != id);
        if !ids.is_empty() {
            return;
        }
        self.render.by_target.remove(&picture.target);
        if let Target::Orphan(key) = picture.target {
            if let Some(orphan) = self.render.orphans.remove(&key) {
                self.release_owned_surface(orphan.owner, &orphan.surface);
            }
        }
    }

    /// Frees a pixmap. When pictures still use it, its surface moves to the orphans.
    pub(in crate::x11) fn release_pixmap(&mut self, id: u32, owner: Option<ClientId>) {
        let Some(pixmap) = self.pixmaps.remove(&id) else {
            return;
        };
        self.tile_cache.remove(&id);
        let Some(ids) = self.render.by_target.remove(&Target::Pixmap(id)) else {
            self.release_owned_surface(owner, &pixmap.surface);
            return;
        };
        let key = self.render.next_orphan;
        self.render.next_orphan += 1;
        for picture_id in &ids {
            if let Some(picture) = self.render.pictures.get_mut(picture_id) {
                picture.target = Target::Orphan(key);
            }
        }
        self.render.by_target.insert(Target::Orphan(key), ids);
        let orphan = Orphan {
            surface: pixmap.surface,
            owner,
        };
        self.render.orphans.insert(key, orphan);
    }

    /// Pictures on a window that is destroyed are freed.
    pub(in crate::x11) fn forget_window_pictures(&mut self, window: u32) {
        let ids = self
            .render
            .by_target
            .get(&Target::Window(window))
            .cloned()
            .unwrap_or_default();
        for id in ids {
            self.drop_picture(id);
        }
    }

    /// Frees the picture or glyph set `id`, when it is one. For a client that goes away.
    pub(in crate::x11) fn forget_render_resource(&mut self, id: u32) {
        self.drop_picture(id);
        self.drop_glyph_set(id);
    }

    /// The orphans of a client that goes away belong to nobody: a new client can get its number.
    pub(in crate::x11) fn forget_orphan_owner(&mut self, client: ClientId) {
        for orphan in self.render.orphans.values_mut() {
            if orphan.owner == Some(client) {
                orphan.owner = None;
            }
        }
    }
}

fn boolean(value: u32) -> Result<bool, XError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(XError::Value(value)),
    }
}
