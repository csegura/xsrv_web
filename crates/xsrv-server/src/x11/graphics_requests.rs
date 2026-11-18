// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

use std::rc::{Rc, Weak};

use super::codec::Reader;
use super::dispatch::Request;
use super::error::XError;
use super::raster;
use super::resources::{Background, Gc, IRect, Pixmap, Surface, FUNCTION_COPY, PIXMAP_DEPTHS};
use super::screen::{MAX_CLIENT_PIXEL_BYTES, MAX_PIXEL_BYTES, MAX_RESOURCES_PER_CLIENT};
use super::state::{State, CLASS_INPUT_ONLY};

const BITMAP_DEPTH: u8 = 1;
pub const FILL_TILED: u8 = 1;
pub const FILL_STIPPLED: u8 = 2;
pub const FILL_OPAQUE_STIPPLED: u8 = 3;
const GC_FUNCTION: u32 = 0x1;
const GC_PLANE_MASK: u32 = 0x2;
const GC_FOREGROUND: u32 = 0x4;
const GC_BACKGROUND: u32 = 0x8;
const GC_LINE_WIDTH: u32 = 0x10;
const GC_LINE_STYLE: u32 = 0x20;
const GC_CAP_STYLE: u32 = 0x40;
const GC_JOIN_STYLE: u32 = 0x80;
const GC_FILL_STYLE: u32 = 0x100;
const GC_FILL_RULE: u32 = 0x200;
const GC_TILE: u32 = 0x400;
const GC_STIPPLE: u32 = 0x800;
const GC_TILE_X_ORIGIN: u32 = 0x1000;
const GC_TILE_Y_ORIGIN: u32 = 0x2000;
const GC_FONT: u32 = 0x4000;
const GC_SUBWINDOW_MODE: u32 = 0x8000;
const GC_GRAPHICS_EXPOSURES: u32 = 0x1_0000;
const GC_CLIP_MASK: u32 = 0x8_0000;
const GC_DASH_LIST: u32 = 0x20_0000;
const GC_ARC_MODE: u32 = 0x40_0000;
const GC_ALL_BITS: u32 = 0x7F_FFFF;
const GC_BIT_COUNT: u32 = 23;

#[derive(Default)]
struct GcValues {
    function: Option<u8>,
    plane_mask: Option<u32>,
    foreground: Option<u32>,
    background: Option<u32>,
    line_width: Option<u16>,
    line_style: Option<u8>,
    cap_style: Option<u8>,
    join_style: Option<u8>,
    fill_style: Option<u8>,
    subwindow_mode: Option<u8>,
    graphics_exposures: Option<bool>,
    clear_clip: bool,
    tile: Option<u32>,
    stipple: Option<u32>,
    tile_x: Option<i32>,
    tile_y: Option<i32>,
    pixmap_refs: Vec<(&'static str, u32)>,
    font: Option<u32>,
    fill_rule: Option<u8>,
    arc_mode: Option<u8>,
}

impl GcValues {
    fn apply(&self, gc: &mut Gc) {
        gc.function = self.function.unwrap_or(gc.function);
        gc.plane_mask = self.plane_mask.unwrap_or(gc.plane_mask);
        gc.foreground = self.foreground.unwrap_or(gc.foreground);
        gc.background = self.background.unwrap_or(gc.background);
        gc.line_width = self.line_width.unwrap_or(gc.line_width);
        gc.line_style = self.line_style.unwrap_or(gc.line_style);
        gc.cap_style = self.cap_style.unwrap_or(gc.cap_style);
        gc.join_style = self.join_style.unwrap_or(gc.join_style);
        gc.fill_style = self.fill_style.unwrap_or(gc.fill_style);
        gc.subwindow_mode = self.subwindow_mode.unwrap_or(gc.subwindow_mode);
        gc.graphics_exposures = self.graphics_exposures.unwrap_or(gc.graphics_exposures);
        gc.fill_rule = self.fill_rule.unwrap_or(gc.fill_rule);
        gc.arc_mode = self.arc_mode.unwrap_or(gc.arc_mode);
        gc.tile = self.tile.unwrap_or(gc.tile);
        gc.stipple = self.stipple.unwrap_or(gc.stipple);
        gc.tile_origin = (
            self.tile_x.unwrap_or(gc.tile_origin.0),
            self.tile_y.unwrap_or(gc.tile_origin.1),
        );
    }
}

fn at_most(value: u32, max: u32) -> Result<u8, XError> {
    if value <= max {
        Ok(value as u8)
    } else {
        Err(XError::Value(value))
    }
}

fn read_gc_values(r: &mut Reader, mask: u32) -> Result<GcValues, XError> {
    if mask & !GC_ALL_BITS != 0 {
        return Err(XError::Value(mask));
    }
    let mut values = GcValues::default();
    for bit in 0..GC_BIT_COUNT {
        let flag = 1 << bit;
        if mask & flag == 0 {
            continue;
        }
        let value = r.u32()?;
        match flag {
            GC_FUNCTION => values.function = Some(at_most(value, 15)?),
            GC_PLANE_MASK => values.plane_mask = Some(value),
            // The drawable masks the value by its depth: depth 32 keeps all 32 bits.
            GC_FOREGROUND => values.foreground = Some(value),
            GC_BACKGROUND => values.background = Some(value),
            GC_LINE_WIDTH => values.line_width = Some(value as u16),
            GC_LINE_STYLE => values.line_style = Some(at_most(value, 2)?),
            GC_CAP_STYLE => values.cap_style = Some(at_most(value, 3)?),
            GC_JOIN_STYLE => values.join_style = Some(at_most(value, 2)?),
            GC_FILL_STYLE => values.fill_style = Some(at_most(value, 3)?),
            GC_FILL_RULE => values.fill_rule = Some(at_most(value, 1)?),
            GC_ARC_MODE => values.arc_mode = Some(at_most(value, 1)?),
            GC_TILE => values.tile = Some(value),
            GC_STIPPLE => values.stipple = Some(value),
            GC_TILE_X_ORIGIN => values.tile_x = Some(i32::from(value as i16)),
            GC_TILE_Y_ORIGIN => values.tile_y = Some(i32::from(value as i16)),
            GC_CLIP_MASK if value == 0 => values.clear_clip = true,
            GC_CLIP_MASK => values.pixmap_refs.push(("clip mask", value)),
            GC_FONT => values.font = Some(value),
            GC_SUBWINDOW_MODE => values.subwindow_mode = Some(at_most(value, 1)?),
            GC_GRAPHICS_EXPOSURES => values.graphics_exposures = Some(at_most(value, 1)? == 1),
            GC_DASH_LIST if value == 0 => return Err(XError::Value(value)),
            _ => {}
        }
    }
    r.end()?;
    Ok(values)
}

impl State {
    pub(super) fn id_in_use(&self, id: u32) -> bool {
        self.window_exists(id)
            || self.pixmaps.contains_key(&id)
            || self.gcs.contains_key(&id)
            || self.fonts.contains(&id)
            || self.cursors.contains_key(&id)
            || self.regions.contains_key(&id)
            || self.shm_segments.contains_key(&id)
            || self.render.uses_id(id)
    }

    /// How many resources a client owns.
    fn resources_of(&self, client: u32) -> usize {
        self.owners.count(client)
    }

    /// The pixel bytes of the windows and pixmaps that a client owns.
    fn pixel_bytes_of(&self, client: u32) -> usize {
        self.client_pixels.get(&client).copied().unwrap_or(0)
    }

    /// A client may own only so many resources.
    pub(super) fn check_resource_quota(&self, client: u32) -> Result<(), XError> {
        if self.resources_of(client) >= MAX_RESOURCES_PER_CLIENT {
            return Err(XError::Alloc);
        }
        Ok(())
    }

    /// Takes memory from the pixel budget of the server and of one client.
    pub(super) fn alloc_surface_for(
        &mut self,
        client: u32,
        width: u16,
        height: u16,
    ) -> Result<Surface, XError> {
        let wanted = Surface::byte_len(width, height).ok_or(XError::Alloc)?;
        if self.pixel_bytes_of(client) + wanted > MAX_CLIENT_PIXEL_BYTES {
            return Err(XError::Alloc);
        }
        let surface = self.alloc_surface(width, height)?;
        *self.client_pixels.entry(client).or_default() += surface.len();
        Ok(surface)
    }

    /// Gives back the memory of a surface that a client owned.
    pub(super) fn release_owned_surface(&mut self, owner: Option<u32>, surface: &Surface) {
        if let Some(count) = owner.and_then(|owner| self.client_pixels.get_mut(&owner)) {
            *count = count.saturating_sub(surface.len());
        }
        self.release_surface(surface);
    }

    /// Takes memory from the pixel budget.
    pub(super) fn alloc_surface(&mut self, width: u16, height: u16) -> Result<Surface, XError> {
        let bytes = Surface::byte_len(width, height).ok_or(XError::Alloc)?;
        if self.pixel_bytes + bytes > MAX_PIXEL_BYTES {
            return Err(XError::Alloc);
        }
        self.pixel_bytes += bytes;
        Ok(Surface::new(width, height))
    }

    pub(super) fn release_surface(&mut self, surface: &Surface) {
        self.pixel_bytes = self.pixel_bytes.saturating_sub(surface.len());
    }

    pub(super) fn drawable_exists(&self, id: u32) -> bool {
        self.window_exists(id) || self.pixmaps.contains_key(&id)
    }

    /// Width, height and depth of a pixmap.
    pub(super) fn pixmap_info(&self, id: u32) -> Option<(u16, u16, u8)> {
        let surface = &self.pixmaps.get(&id)?.surface;
        let bounds = surface.bounds();
        Some((
            bounds.width() as u16,
            bounds.height() as u16,
            surface.depth(),
        ))
    }

    /// The depth of a window or a pixmap. Input-only windows have none.
    fn drawable_depth(&self, id: u32) -> Option<u8> {
        match self.windows.get(&id) {
            Some(window) => Some(window.depth).filter(|depth| *depth != 0),
            None => self.pixmaps.get(&id).map(|pixmap| pixmap.surface.depth()),
        }
    }

    pub fn create_pixmap(&mut self, req: &Request) -> Result<(), XError> {
        let depth = req.detail;
        let mut r = req.reader();
        let id = r.u32()?;
        let drawable = r.u32()?;
        let width = r.u16()?;
        let height = r.u16()?;
        r.end()?;
        if !self.owns_id(req.client, id) || self.id_in_use(id) {
            return Err(XError::IdChoice(id));
        }
        if !self.drawable_exists(drawable) {
            return Err(XError::Drawable(drawable));
        }
        if !PIXMAP_DEPTHS.contains(&depth) {
            return Err(XError::Value(depth.into()));
        }
        if width == 0 || height == 0 {
            return Err(XError::Value(0));
        }
        self.check_resource_quota(req.client)?;
        let mut surface = self.alloc_surface_for(req.client, width, height)?;
        surface.set_depth(depth);
        self.pixmaps.insert(id, Pixmap { surface });
        self.owners.insert(id, req.client);
        Ok(())
    }

    pub fn free_pixmap(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let id = r.u32()?;
        r.end()?;
        if !self.pixmaps.contains_key(&id) {
            return Err(XError::Pixmap(id));
        }
        let owner = self.owners.remove(&id);
        self.release_pixmap(id, owner);
        Ok(())
    }

    pub fn create_gc(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let id = r.u32()?;
        let drawable = r.u32()?;
        let mask = r.u32()?;
        let values = read_gc_values(&mut r, mask)?;
        if !self.owns_id(req.client, id) || self.id_in_use(id) {
            return Err(XError::IdChoice(id));
        }
        self.check_resource_quota(req.client)?;
        if !self.drawable_exists(drawable) {
            return Err(XError::Drawable(drawable));
        }
        let depth = self.drawable_depth(drawable).ok_or(XError::Match)?;
        self.check_unsupported(&values, depth)?;
        let mut gc = Gc {
            depth,
            ..Gc::default()
        };
        values.apply(&mut gc);
        self.gcs.insert(id, gc);
        self.owners.insert(id, req.client);
        Ok(())
    }

    pub fn change_gc(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let id = r.u32()?;
        let mask = r.u32()?;
        let values = read_gc_values(&mut r, mask)?;
        let mut gc = self.gc(id)?;
        self.check_unsupported(&values, gc.depth)?;
        values.apply(&mut gc);
        if values.clear_clip {
            self.clips.remove(&id);
        }
        self.gcs.insert(id, gc);
        Ok(())
    }

    pub fn free_gc(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let id = r.u32()?;
        r.end()?;
        self.gcs.remove(&id).ok_or(XError::GContext(id))?;
        self.clips.remove(&id);
        self.owners.remove(&id);
        Ok(())
    }

    /// Drawing with the GC reaches only the union of the rects. No rects means no drawing.
    pub fn set_clip_rectangles(&mut self, req: &Request) -> Result<(), XError> {
        const MAX_ORDERING: u8 = 3;
        let mut r = req.reader();
        let id = r.u32()?;
        let origin_x = i32::from(r.i16()?);
        let origin_y = i32::from(r.i16()?);
        let mut rects = Vec::new();
        while r.remaining() > 0 {
            let x = i32::from(r.i16()?);
            let y = i32::from(r.i16()?);
            let (width, height) = (i32::from(r.u16()?), i32::from(r.u16()?));
            rects.push(IRect::new(origin_x + x, origin_y + y, width, height));
        }
        self.gc(id)?;
        if req.detail > MAX_ORDERING {
            return Err(XError::Value(req.detail.into()));
        }
        self.clips.insert(id, rects);
        Ok(())
    }

    /// A tile has the depth of the GC and a stipple is a bitmap. Clip masks are not drawn yet,
    /// so they are refused instead of ignored. A font must be open.
    fn check_unsupported(&mut self, values: &GcValues, gc_depth: u8) -> Result<(), XError> {
        if let Some(tile) = values.tile {
            let (_, _, depth) = self.pixmap_info(tile).ok_or(XError::Pixmap(tile))?;
            if depth != gc_depth {
                return Err(XError::Match);
            }
        }
        if let Some(stipple) = values.stipple {
            let (_, _, depth) = self.pixmap_info(stipple).ok_or(XError::Pixmap(stipple))?;
            if depth != BITMAP_DEPTH {
                return Err(XError::Match);
            }
        }
        if let Some(&(what, pixmap)) = values.pixmap_refs.first() {
            return Err(if self.pixmaps.contains_key(&pixmap) {
                self.log_once(format!("gc {what}"), || {
                    format!("xsrv-web: a GC {what} is not supported yet")
                });
                XError::Implementation
            } else {
                XError::Pixmap(pixmap)
            });
        }
        match values.font {
            Some(font) if !self.fonts.contains(&font) => Err(XError::Font(font)),
            _ => Ok(()),
        }
    }

    /// The tile or the stipple that the fill style of the GC needs, copied out of its pixmap.
    /// `None` means a solid fill, also when the GC has no pattern for its fill style.
    pub(super) fn fill_pattern(&mut self, gc: &Gc) -> Option<Surface> {
        let id = match gc.fill_style {
            FILL_TILED => gc.tile,
            FILL_OPAQUE_STIPPLED | FILL_STIPPLED => gc.stipple,
            _ => return None,
        };
        let pattern = self.pixmaps.get(&id).map(|pixmap| pixmap.surface.clone());
        if pattern.is_none() {
            self.log_once("fill without pattern".into(), || {
                "xsrv-web: the fill style has no tile or stipple, so the fill is solid".into()
            });
        }
        pattern
    }

    pub(super) fn gc(&self, id: u32) -> Result<Gc, XError> {
        self.gcs.get(&id).copied().ok_or(XError::GContext(id))
    }

    pub(super) fn surface(&self, id: u32) -> Result<&Surface, XError> {
        if let Some(window) = self.windows.get(&id) {
            if window.class == CLASS_INPUT_ONLY {
                return Err(XError::Match);
            }
            return Ok(&window.surface);
        }
        match self.pixmaps.get(&id) {
            Some(pixmap) => Ok(&pixmap.surface),
            None => Err(XError::Drawable(id)),
        }
    }

    /// Finds the pixels of a window or a pixmap. Input-only windows have none.
    pub(super) fn surface_mut(&mut self, id: u32) -> Result<&mut Surface, XError> {
        if let Some(window) = self.windows.get_mut(&id) {
            if window.class == CLASS_INPUT_ONLY {
                return Err(XError::Match);
            }
            return Ok(&mut window.surface);
        }
        match self.pixmaps.get_mut(&id) {
            Some(pixmap) => Ok(&mut pixmap.surface),
            None => Err(XError::Drawable(id)),
        }
    }

    /// The pixels of two different drawables: the first to read, the second to write.
    pub(super) fn surface_pair(
        &mut self,
        source: u32,
        target: u32,
    ) -> Result<(&Surface, &mut Surface), XError> {
        self.surface(source)?;
        self.surface(target)?;
        if source == target {
            return Err(XError::Match);
        }
        let missing = XError::Drawable(target);
        let window_source = self.windows.contains_key(&source);
        let window_target = self.windows.contains_key(&target);
        match (window_source, window_target) {
            (true, true) => match self.windows.get_disjoint_mut([&source, &target]) {
                [Some(source), Some(target)] => Ok((&source.surface, &mut target.surface)),
                _ => Err(missing),
            },
            (false, false) => match self.pixmaps.get_disjoint_mut([&source, &target]) {
                [Some(source), Some(target)] => Ok((&source.surface, &mut target.surface)),
                _ => Err(missing),
            },
            (true, false) => match (self.windows.get(&source), self.pixmaps.get_mut(&target)) {
                (Some(source), Some(target)) => Ok((&source.surface, &mut target.surface)),
                _ => Err(missing),
            },
            (false, true) => match (self.pixmaps.get(&source), self.windows.get_mut(&target)) {
                (Some(source), Some(target)) => Ok((&source.surface, &mut target.surface)),
                _ => Err(missing),
            },
        }
    }

    /// Clears `area` of the window with its background. Nothing happens when no ancestor has
    /// one. A tile starts at the origin of the window that owns the background.
    pub(super) fn fill_background(&mut self, id: u32, area: IRect) -> Result<(), XError> {
        let mut current = id;
        let (mut offset_x, mut offset_y) = (0, 0);
        let background = loop {
            let Some(window) = self.windows.get(&current) else {
                return Ok(());
            };
            match &window.background {
                Background::None => return Ok(()),
                Background::ParentRelative => {
                    offset_x += i32::from(window.x) + i32::from(window.border);
                    offset_y += i32::from(window.y) + i32::from(window.border);
                    let Some(parent) = window.parent else {
                        return Ok(());
                    };
                    current = parent;
                }
                background => break background.clone(),
            }
        };
        let surface = self.surface_mut(id)?;
        match background {
            Background::Pixel(pixel) => surface.fill_rect(area, pixel, FUNCTION_COPY, u32::MAX),
            Background::Tile(tile) => {
                let gc = Gc {
                    fill_style: FILL_TILED,
                    tile_origin: (-offset_x, -offset_y),
                    plane_mask: u32::MAX,
                    ..Gc::default()
                };
                raster::fill_pattern(surface, area, &tile, &gc);
            }
            Background::None | Background::ParentRelative => {}
        }
        Ok(())
    }

    /// The shared copy of a pixmap for backgrounds. Windows that use the same unchanged pixmap
    /// share one copy, which lives as long as one window uses it. The copies of all pixmaps
    /// together may take half of the pixel budget.
    pub(super) fn tile_of(&mut self, pixmap: u32) -> Result<Rc<Surface>, XError> {
        let surface = &self
            .pixmaps
            .get(&pixmap)
            .ok_or(XError::Pixmap(pixmap))?
            .surface;
        let cached = self.tile_cache.get(&pixmap).and_then(Weak::upgrade);
        if let Some(tile) = cached.filter(|tile| tile.same_pixels(surface)) {
            return Ok(tile);
        }
        self.live_tiles.retain(|tile| tile.strong_count() > 0);
        let used: usize = self
            .live_tiles
            .iter()
            .filter_map(Weak::upgrade)
            .map(|tile| tile.len())
            .sum();
        if used + surface.len() > MAX_PIXEL_BYTES / 2 {
            return Err(XError::Alloc);
        }
        let tile = Rc::new(surface.clone());
        self.tile_cache.insert(pixmap, Rc::downgrade(&tile));
        self.live_tiles.push(Rc::downgrade(&tile));
        Ok(tile)
    }
}
