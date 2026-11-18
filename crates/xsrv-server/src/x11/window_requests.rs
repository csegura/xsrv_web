// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

use std::collections::HashMap;

use super::codec::{Reader, Writer};
use super::dispatch::{Reply, Request};
use super::error::XError;
use super::event::{
    Event, BUTTON_PRESS, EXPOSURE, RESIZE_REDIRECT, STRUCTURE_NOTIFY, SUBSTRUCTURE_NOTIFY,
    SUBSTRUCTURE_REDIRECT,
};
use super::ext_shape::Shapes;
use super::resources::{Background, Surface, PIXEL_MASK};
use super::screen::{
    DEFAULT_COLORMAP, MAX_WINDOWS, MAX_WINDOW_DEPTH, ROOT_DEPTH, ROOT_VISUAL, ROOT_WINDOW,
};
use super::state::{ClientId, State, Window, CLASS_INPUT_ONLY, CLASS_INPUT_OUTPUT};

const CW_BACKGROUND_PIXMAP: u32 = 0x1;
const CW_BACKGROUND_PIXEL: u32 = 0x2;
const CW_BORDER_PIXMAP: u32 = 0x4;
const CW_BORDER_PIXEL: u32 = 0x8;
const CW_BIT_GRAVITY: u32 = 0x10;
const CW_WIN_GRAVITY: u32 = 0x20;
const CW_BACKING_STORE: u32 = 0x40;
const CW_OVERRIDE_REDIRECT: u32 = 0x200;
const CW_SAVE_UNDER: u32 = 0x400;
const CW_EVENT_MASK: u32 = 0x800;
const CW_DO_NOT_PROPAGATE: u32 = 0x1000;
const CW_COLORMAP: u32 = 0x2000;
const CW_CURSOR: u32 = 0x4000;
const CW_ALL_BITS: u32 = 0x7FFF;
const CW_BIT_COUNT: u32 = 15;

const PARENT_RELATIVE: u32 = 1;
const MAX_GRAVITY: u32 = 10;
const MAX_BACKING_STORE: u32 = 2;
const ALL_EVENT_MASKS: u32 = 0x01FF_FFFF;
const DEVICE_EVENT_MASKS: u32 = 0x3F4F;
const EXCLUSIVE_EVENT_MASKS: u32 = SUBSTRUCTURE_REDIRECT | RESIZE_REDIRECT | BUTTON_PRESS;

const CONFIGURE_X: u16 = 0x01;
const CONFIGURE_Y: u16 = 0x02;
const CONFIGURE_WIDTH: u16 = 0x04;
const CONFIGURE_HEIGHT: u16 = 0x08;
const CONFIGURE_BORDER: u16 = 0x10;
const CONFIGURE_SIBLING: u16 = 0x20;
const CONFIGURE_STACK_MODE: u16 = 0x40;
const CONFIGURE_ALL_BITS: u16 = 0x7F;
const CONFIGURE_BIT_COUNT: u16 = 7;

const STACK_ABOVE: u8 = 0;
const STACK_BELOW: u8 = 1;
const STACK_TOP_IF: u8 = 2;
const STACK_BOTTOM_IF: u8 = 3;
const STACK_OPPOSITE: u8 = 4;

#[derive(Default)]
struct WindowValues {
    event_mask: Option<u32>,
    override_redirect: Option<bool>,
    bit_gravity: Option<u8>,
    win_gravity: Option<u8>,
    do_not_propagate: Option<u16>,
    background: Option<Background>,
    /// A pixmap that is the background, unless a background pixel came with it.
    background_pixmap: Option<u32>,
    border_pixmap: Option<u32>,
    border_pixel: Option<u32>,
    cursor: Option<u32>,
}

fn require_at_most(value: u32, max: u32) -> Result<(), XError> {
    if value <= max {
        Ok(())
    } else {
        Err(XError::Value(value))
    }
}

fn require_flag(value: u32) -> Result<bool, XError> {
    require_at_most(value, 1)?;
    Ok(value == 1)
}

fn read_window_values(r: &mut Reader, mask: u32) -> Result<WindowValues, XError> {
    if mask & !CW_ALL_BITS != 0 {
        return Err(XError::Value(mask));
    }
    let mut values = WindowValues::default();
    for bit in 0..CW_BIT_COUNT {
        let flag = 1 << bit;
        if mask & flag == 0 {
            continue;
        }
        let value = r.u32()?;
        match flag {
            CW_BACKGROUND_PIXMAP if value > PARENT_RELATIVE => {
                values.background_pixmap = Some(value);
            }
            CW_BORDER_PIXMAP => values.border_pixmap = Some(value).filter(|id| *id != 0),
            CW_BACKGROUND_PIXMAP => {
                values.background = Some(if value == PARENT_RELATIVE {
                    Background::ParentRelative
                } else {
                    Background::None
                });
            }
            CW_BACKGROUND_PIXEL => values.background = Some(Background::Pixel(value & PIXEL_MASK)),
            CW_BORDER_PIXEL => values.border_pixel = Some(value & PIXEL_MASK),
            CW_BIT_GRAVITY => {
                require_at_most(value, MAX_GRAVITY)?;
                values.bit_gravity = Some(value as u8);
            }
            CW_WIN_GRAVITY => {
                require_at_most(value, MAX_GRAVITY)?;
                values.win_gravity = Some(value as u8);
            }
            CW_BACKING_STORE => require_at_most(value, MAX_BACKING_STORE)?,
            CW_OVERRIDE_REDIRECT => values.override_redirect = Some(require_flag(value)?),
            CW_SAVE_UNDER => {
                require_flag(value)?;
            }
            CW_EVENT_MASK => {
                if value & !ALL_EVENT_MASKS != 0 {
                    return Err(XError::Value(value));
                }
                values.event_mask = Some(value);
            }
            CW_DO_NOT_PROPAGATE => {
                if value & !DEVICE_EVENT_MASKS != 0 {
                    return Err(XError::Value(value));
                }
                values.do_not_propagate = Some(value as u16);
            }
            CW_COLORMAP if value != 0 && value != DEFAULT_COLORMAP => {
                return Err(XError::Colormap(value))
            }
            CW_CURSOR => values.cursor = Some(value),
            _ => {}
        }
    }
    r.end()?;
    Ok(values)
}

#[derive(Default)]
struct ConfigureValues {
    x: Option<i16>,
    y: Option<i16>,
    width: Option<u16>,
    height: Option<u16>,
    border: Option<u16>,
    sibling: Option<u32>,
    stack_mode: Option<u8>,
}

fn read_configure_values(r: &mut Reader, mask: u16) -> Result<ConfigureValues, XError> {
    if mask & !CONFIGURE_ALL_BITS != 0 {
        return Err(XError::Value(mask.into()));
    }
    let mut values = ConfigureValues::default();
    for bit in 0..CONFIGURE_BIT_COUNT {
        let flag = 1 << bit;
        if mask & flag == 0 {
            continue;
        }
        let value = r.u32()?;
        match flag {
            CONFIGURE_X => values.x = Some(value as u16 as i16),
            CONFIGURE_Y => values.y = Some(value as u16 as i16),
            CONFIGURE_WIDTH => values.width = Some(value as u16),
            CONFIGURE_HEIGHT => values.height = Some(value as u16),
            CONFIGURE_BORDER => values.border = Some(value as u16),
            CONFIGURE_SIBLING => values.sibling = Some(value),
            CONFIGURE_STACK_MODE => {
                require_at_most(value, STACK_OPPOSITE.into())?;
                values.stack_mode = Some(value as u8);
            }
            _ => {}
        }
    }
    r.end()?;
    Ok(values)
}

#[derive(Clone, Copy)]
enum Placement {
    Top,
    Bottom,
    AboveSibling(u32),
    BelowSibling(u32),
}

fn overlaps(a: &Window, b: &Window) -> bool {
    let extent = |w: &Window| {
        let border = i32::from(w.border) * 2;
        let left = i32::from(w.x);
        let top = i32::from(w.y);
        (
            left,
            top,
            left + i32::from(w.width) + border,
            top + i32::from(w.height) + border,
        )
    };
    let (al, at, ar, ab) = extent(a);
    let (bl, bt, br, bb) = extent(b);
    al < br && bl < ar && at < bb && bt < ab
}

impl State {
    pub fn create_window(&mut self, req: &Request) -> Result<(), XError> {
        let depth = req.detail;
        let mut r = req.reader();
        let id = r.u32()?;
        let parent_id = r.u32()?;
        let x = r.i16()?;
        let y = r.i16()?;
        let width = r.u16()?;
        let height = r.u16()?;
        let border = r.u16()?;
        let class = r.u16()?;
        let visual = r.u32()?;
        let mask = r.u32()?;
        let values = read_window_values(&mut r, mask)?;
        self.check_cursor(values.cursor)?;

        if !self.owns_id(req.client, id) || self.id_in_use(id) {
            return Err(XError::IdChoice(id));
        }
        let pixmap_depth = match depth {
            0 => self.window(parent_id)?.depth,
            other => other,
        };
        let background = self.window_background(&values, pixmap_depth)?;
        let parent_class = self.window(parent_id)?.class;
        if self.window_depth(parent_id) >= MAX_WINDOW_DEPTH {
            return Err(XError::Alloc);
        }
        if width == 0 || height == 0 {
            return Err(XError::Value(0));
        }
        let class = match class {
            0 if parent_class == CLASS_INPUT_ONLY => CLASS_INPUT_ONLY,
            CLASS_INPUT_OUTPUT if parent_class == CLASS_INPUT_ONLY => return Err(XError::Match),
            0 | CLASS_INPUT_OUTPUT => CLASS_INPUT_OUTPUT,
            CLASS_INPUT_ONLY => CLASS_INPUT_ONLY,
            other => return Err(XError::Value(other.into())),
        };
        let depth = match (class, depth) {
            (CLASS_INPUT_OUTPUT, 0 | ROOT_DEPTH) => ROOT_DEPTH,
            (CLASS_INPUT_ONLY, 0) if border == 0 => 0,
            _ => return Err(XError::Match),
        };
        if visual != 0 && visual != ROOT_VISUAL {
            return Err(XError::Match);
        }
        if self.window_count() >= MAX_WINDOWS {
            return Err(XError::Alloc);
        }

        self.check_resource_quota(req.client)?;
        let surface = if class == CLASS_INPUT_OUTPUT {
            self.alloc_surface_for(req.client, width, height)?
        } else {
            Surface::empty()
        };
        let override_redirect = values.override_redirect.unwrap_or(false);
        let listeners = match values.event_mask {
            Some(mask) if mask != 0 => HashMap::from([(req.client, mask)]),
            _ => HashMap::new(),
        };
        let window = Window {
            parent: Some(parent_id),
            children: Vec::new(),
            x,
            y,
            width,
            height,
            border,
            class,
            depth,
            mapped: false,
            override_redirect,
            bit_gravity: values.bit_gravity.unwrap_or(0),
            win_gravity: values.win_gravity.unwrap_or(1),
            do_not_propagate: values.do_not_propagate.unwrap_or(0),
            background: background.unwrap_or(Background::None),
            border_pixel: values.border_pixel.unwrap_or(0),
            cursor: values.cursor.unwrap_or(0),
            shapes: Shapes::default(),
            surface,
            listeners,
            properties: HashMap::new(),
        };
        self.insert_window(id, req.client, window);
        self.notify_listeners(
            parent_id,
            SUBSTRUCTURE_NOTIFY,
            &Event::CreateNotify {
                parent: parent_id,
                window: id,
                x,
                y,
                width,
                height,
                border,
                override_redirect,
            },
        );
        Ok(())
    }

    /// The background that the values ask for. A pixmap is copied, so the client may free it. A
    /// window that has depth 0 (CopyFromParent) takes any pixmap that the parent could.
    fn window_background(
        &mut self,
        values: &WindowValues,
        depth: u8,
    ) -> Result<Option<Background>, XError> {
        if let Some(id) = values.border_pixmap {
            // The frame of a window is drawn with the border pixel, so a border pixmap is checked
            // and not shown.
            self.check_attribute_pixmap(id, depth)?;
        }
        match (&values.background, values.background_pixmap) {
            (Some(Background::Pixel(_)), _) | (_, None) => Ok(values.background.clone()),
            (_, Some(id)) => {
                self.check_attribute_pixmap(id, depth)?;
                Ok(Some(Background::Tile(self.tile_of(id)?)))
            }
        }
    }

    fn check_attribute_pixmap(&self, id: u32, depth: u8) -> Result<(), XError> {
        let pixmap = self.pixmaps.get(&id).ok_or(XError::Pixmap(id))?;
        if depth != 0 && pixmap.surface.depth() != depth {
            return Err(XError::Match);
        }
        Ok(())
    }

    pub fn change_window_attributes(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let id = r.u32()?;
        let mask = r.u32()?;
        let values = read_window_values(&mut r, mask)?;
        self.check_cursor(values.cursor)?;
        let depth = self.window(id)?.depth;
        let background = self.window_background(&values, depth)?;
        let window = self.window_mut(id)?;
        if let Some(new_mask) = values.event_mask {
            claim_event_mask(window, req.client, new_mask)?;
        }
        // Only the border colour shows at once (the background shows where the window is cleared
        // later), and override-redirect can change the frame of the built-in manager. xterm
        // changes its cursor with every line of output; drawing the whole window again for that
        // made scrolling cost twice the CPU.
        let looks_different = values
            .border_pixel
            .is_some_and(|pixel| pixel != window.border_pixel)
            || values
                .override_redirect
                .is_some_and(|value| value != window.override_redirect);
        // The frame of the built-in manager goes or comes with override-redirect: the area that
        // it covered is drawn again too.
        let old_outer = if looks_different {
            self.outer_rect(id)
        } else {
            None
        };
        let window = self.window_mut(id)?;
        if let Some(value) = values.override_redirect {
            window.override_redirect = value;
        }
        window.bit_gravity = values.bit_gravity.unwrap_or(window.bit_gravity);
        window.win_gravity = values.win_gravity.unwrap_or(window.win_gravity);
        window.do_not_propagate = values.do_not_propagate.unwrap_or(window.do_not_propagate);
        if let Some(background) = background {
            window.background = background;
        }
        window.border_pixel = values.border_pixel.unwrap_or(window.border_pixel);
        window.cursor = values.cursor.unwrap_or(window.cursor);
        if looks_different {
            if let Some(old) = old_outer.filter(|_| self.is_viewable(id)) {
                self.mark_dirty(old);
            }
            self.mark_window_dirty(id);
        }
        Ok(())
    }

    pub fn get_window_attributes(&self, req: &Request) -> Result<Reply, XError> {
        let mut r = req.reader();
        let id = r.u32()?;
        r.end()?;
        let window = self.window(id)?;
        let map_state = match (window.mapped, self.is_viewable(id)) {
            (false, _) => 0,
            (true, false) => 1,
            (true, true) => 2,
        };
        let all_masks = window.listeners.values().fold(0, |all, mask| all | mask);
        let your_mask = window.listeners.get(&req.client).copied().unwrap_or(0);
        let mut w = Writer::new(req.endian);
        w.u32(ROOT_VISUAL);
        w.u16(window.class);
        w.u8(window.bit_gravity);
        w.u8(window.win_gravity);
        w.u32(u32::MAX);
        w.u32(0);
        w.u8(0);
        w.u8(1);
        w.u8(map_state);
        w.u8(window.override_redirect.into());
        w.u32(DEFAULT_COLORMAP);
        w.u32(all_masks);
        w.u32(your_mask);
        w.u16(window.do_not_propagate);
        w.u16(0);
        Ok(Reply::new(0, w))
    }

    pub fn destroy_window_request(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let id = r.u32()?;
        r.end()?;
        self.window(id)?;
        if id != ROOT_WINDOW {
            self.destroy_window(id);
        }
        Ok(())
    }

    /// Destroys the children of a window, the highest first.
    pub fn destroy_subwindows(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let id = r.u32()?;
        r.end()?;
        for child in self.window(id)?.children.clone().into_iter().rev() {
            self.destroy_window(child);
        }
        Ok(())
    }

    pub fn map_window(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let id = r.u32()?;
        r.end()?;
        self.map_or_request(id, req.client)
    }

    /// Maps the children of a window, the lowest first.
    pub fn map_subwindows(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let id = r.u32()?;
        r.end()?;
        for child in self.window(id)?.children.clone() {
            self.map_or_request(child, req.client)?;
        }
        Ok(())
    }

    /// The client that has selected SubstructureRedirect on `parent`, when it is not `requester`.
    fn redirect_holder(&self, parent: u32, requester: ClientId) -> Option<ClientId> {
        let parent = self.windows.get(&parent)?;
        parent
            .listeners
            .iter()
            .find(|(client, mask)| **mask & SUBSTRUCTURE_REDIRECT != 0 && **client != requester)
            .map(|(client, _)| *client)
    }

    /// The window that a map request is about, and the window manager that must decide on it.
    fn redirected_to(&self, id: u32, requester: ClientId) -> Option<(u32, ClientId)> {
        let window = self.windows.get(&id).filter(|w| !w.override_redirect)?;
        let parent = window.parent?;
        Some((parent, self.redirect_holder(parent, requester)?))
    }

    /// Maps the window, or asks the window manager to map it.
    fn map_or_request(&mut self, id: u32, requester: ClientId) -> Result<(), XError> {
        if let Some((parent, holder)) = self.redirected_to(id, requester) {
            if !self.window(id)?.mapped {
                let event = Event::MapRequest { parent, window: id };
                self.send_event_to(holder, &event);
            }
            return Ok(());
        }
        self.map_one(id)
    }

    fn fill_whole_background(&mut self, id: u32) -> Result<(), XError> {
        let bounds = self.window(id)?.surface.bounds();
        self.fill_background(id, bounds)
    }

    fn map_one(&mut self, id: u32) -> Result<(), XError> {
        let window = self.window_mut(id)?;
        if window.mapped {
            return Ok(());
        }
        window.mapped = true;
        let override_redirect = window.override_redirect;
        self.fill_whole_background(id)?;
        self.mark_window_dirty(id);
        self.notify_structure(id, |event| Event::MapNotify {
            event,
            window: id,
            override_redirect,
        });
        if self.is_viewable(id) {
            self.expose_tree(id);
        }
        self.place_new_window(id);
        Ok(())
    }

    pub(super) fn expose_tree(&self, id: u32) {
        let Ok(window) = self.window(id) else {
            return;
        };
        if window.class != CLASS_INPUT_OUTPUT {
            return;
        }
        self.notify_listeners(
            id,
            EXPOSURE,
            &Event::Expose {
                window: id,
                x: 0,
                y: 0,
                width: window.width,
                height: window.height,
            },
        );
        for &child in &window.children {
            if self.window(child).is_ok_and(|c| c.mapped) {
                self.expose_tree(child);
            }
        }
    }

    pub fn unmap_window(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let id = r.u32()?;
        r.end()?;
        self.unmap_one(id)
    }

    /// Unmaps the children of a window, the highest first.
    pub fn unmap_subwindows(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let id = r.u32()?;
        r.end()?;
        for child in self.window(id)?.children.clone().into_iter().rev() {
            self.unmap_one(child)?;
        }
        Ok(())
    }

    fn unmap_one(&mut self, id: u32) -> Result<(), XError> {
        let window = self.window_mut(id)?;
        if id == ROOT_WINDOW || !window.mapped {
            return Ok(());
        }
        self.mark_window_dirty(id);
        self.window_mut(id)?.mapped = false;
        self.notify_structure(id, |event| Event::UnmapNotify { event, window: id });
        Ok(())
    }

    pub fn configure_window(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let id = r.u32()?;
        let mask = r.u16()?;
        r.skip(2)?;
        let values = read_configure_values(&mut r, mask)?;
        self.window(id)?;
        if id == ROOT_WINDOW {
            return Err(XError::Match);
        }
        if values.width == Some(0) || values.height == Some(0) {
            return Err(XError::Value(0));
        }
        if self.window(id)?.class == CLASS_INPUT_ONLY && values.border.is_some_and(|b| b != 0) {
            return Err(XError::Match);
        }
        if values.stack_mode.is_none() && values.sibling.is_some() {
            return Err(XError::Match);
        }
        if let Some(sibling) = values.sibling {
            self.check_sibling(id, sibling)?;
        }
        if let Some((parent, holder)) = self.redirected_to(id, req.client) {
            self.send_configure_request(holder, parent, id, mask, &values);
            return Ok(());
        }
        // Everything that can fail runs before the first change, so an error leaves the window as it was.
        let resized = self.resize_surface(id, values.width, values.height)?;
        if let Some(mode) = values.stack_mode {
            self.restack(id, mode, values.sibling)?;
        }
        let window = self.window(id)?;
        let (border, at) = (window.border, (window.x, window.y));
        let pure_move = !resized
            && values.stack_mode.is_none()
            && values.border.is_none_or(|value| value == border);
        let (x, y) = (values.x, values.y);
        if pure_move && (x.unwrap_or(at.0), y.unwrap_or(at.1)) == at {
            // Nothing changes on the screen: no drawing work, and the burst is not cut.
            self.notify_configure(id);
            return Ok(());
        }
        if pure_move {
            self.move_with_screen_copy(id, |state| {
                if let Ok(window) = state.window_mut(id) {
                    window.x = x.unwrap_or(window.x);
                    window.y = y.unwrap_or(window.y);
                }
            });
            self.notify_configure(id);
            return Ok(());
        }

        self.mark_window_dirty(id);
        let window = self.window_mut(id)?;
        window.x = values.x.unwrap_or(window.x);
        window.y = values.y.unwrap_or(window.y);
        window.width = values.width.unwrap_or(window.width);
        window.height = values.height.unwrap_or(window.height);
        window.border = values.border.unwrap_or(window.border);
        self.mark_window_dirty(id);
        if resized && self.is_viewable(id) {
            self.expose_tree(id);
        }
        self.notify_configure(id);
        Ok(())
    }

    /// Tells the window and its parent about the current geometry of the window.
    pub(super) fn notify_configure(&self, id: u32) {
        let Ok(window) = self.window(id) else {
            return;
        };
        let (x, y, width, height, border) = (
            window.x,
            window.y,
            window.width,
            window.height,
            window.border,
        );
        let override_redirect = window.override_redirect;
        let above_sibling = self.sibling_below(id);
        self.notify_structure(id, |event| Event::ConfigureNotify {
            event,
            window: id,
            above_sibling,
            x,
            y,
            width,
            height,
            border,
            override_redirect,
        });
    }

    fn check_sibling(&self, id: u32, sibling: u32) -> Result<(), XError> {
        let parent = self.window(id)?.parent.ok_or(XError::Match)?;
        if sibling != id && self.window(parent)?.children.contains(&sibling) {
            Ok(())
        } else {
            Err(XError::Match)
        }
    }

    /// Gives an input-output window new pixels when its size changes. Returns whether it changed.
    pub(super) fn resize_surface(
        &mut self,
        id: u32,
        width: Option<u16>,
        height: Option<u16>,
    ) -> Result<bool, XError> {
        let window = self.window(id)?;
        let new_width = width.unwrap_or(window.width);
        let new_height = height.unwrap_or(window.height);
        if (new_width, new_height) == (window.width, window.height) {
            return Ok(false);
        }
        if window.class == CLASS_INPUT_OUTPUT {
            let surface = match self.owners.get(&id).copied() {
                Some(owner) => self.alloc_surface_for(owner, new_width, new_height)?,
                None => self.alloc_surface(new_width, new_height)?,
            };
            let old = std::mem::replace(&mut self.window_mut(id)?.surface, surface);
            let owner = self.owners.get(&id).copied();
            self.release_owned_surface(owner, &old);
            self.fill_whole_background(id)?;
        }
        Ok(true)
    }

    /// Tells the window manager what a client wants. The window keeps its geometry for now.
    fn send_configure_request(
        &self,
        holder: ClientId,
        parent: u32,
        id: u32,
        mask: u16,
        values: &ConfigureValues,
    ) {
        let Ok(window) = self.window(id) else {
            return;
        };
        let event = Event::ConfigureRequest {
            stack_mode: values.stack_mode.unwrap_or(STACK_ABOVE),
            parent,
            window: id,
            sibling: values.sibling.unwrap_or(0),
            x: values.x.unwrap_or(window.x),
            y: values.y.unwrap_or(window.y),
            width: values.width.unwrap_or(window.width),
            height: values.height.unwrap_or(window.height),
            border: values.border.unwrap_or(window.border),
            value_mask: mask,
        };
        self.send_event_to(holder, &event);
    }

    /// ChangeSaveSet. A window in the save set of a client goes back to the root window when
    /// that client leaves, instead of being destroyed with the windows of the client.
    pub fn change_save_set(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let window = r.u32()?;
        r.end()?;
        self.save_set_change(req.client, window, req.detail, true)
    }

    /// `mode` is 0 to insert and 1 to delete. `map` says whether to map the window when it is saved.
    pub(super) fn save_set_change(
        &mut self,
        client: ClientId,
        window: u32,
        mode: u8,
        map: bool,
    ) -> Result<(), XError> {
        self.window(window)?;
        if self.owners.get(&window) == Some(&client) {
            return Err(XError::Match);
        }
        match mode {
            0 => {
                self.save_sets
                    .entry(client)
                    .or_default()
                    .insert(window, map);
            }
            1 => {
                if let Some(set) = self.save_sets.get_mut(&client) {
                    set.remove(&window);
                }
            }
            other => return Err(XError::Value(other.into())),
        }
        Ok(())
    }

    /// Puts the saved windows of a client back on the root, where they were on the screen.
    pub(super) fn rescue_saved_windows(&mut self, client: ClientId) {
        let Some(saved) = self.save_sets.remove(&client) else {
            return;
        };
        for (id, map) in saved {
            let Some(parent) = self.windows.get(&id).and_then(|w| w.parent) else {
                continue;
            };
            if self.owners.get(&id) == Some(&client) || parent == ROOT_WINDOW {
                continue;
            }
            let (x, y) = self.window_origin(id).unwrap_or((0, 0));
            let border = self.windows.get(&id).map_or(0, |w| i32::from(w.border));
            let at = ((x - border) as i16, (y - border) as i16);
            if self.reparent_to(id, parent, ROOT_WINDOW, at).is_ok() && map {
                let _ = self.map_one(id);
            }
        }
    }

    /// Moves a window to a new parent. The window is unmapped first and mapped again afterwards.
    pub fn reparent_window(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let id = r.u32()?;
        let new_parent = r.u32()?;
        let x = r.i16()?;
        let y = r.i16()?;
        r.end()?;
        let old_parent = self.window(id)?.parent.ok_or(XError::Match)?;
        self.window(new_parent)?;
        if self.chain(new_parent).contains(&id) {
            return Err(XError::Match);
        }
        if self.window(new_parent)?.class == CLASS_INPUT_ONLY
            && self.window(id)?.class == CLASS_INPUT_OUTPUT
        {
            return Err(XError::Match);
        }
        self.reparent_to(id, old_parent, new_parent, (x, y))
    }

    /// Unmaps the window, gives it a new parent and maps it again. The parent was checked.
    pub(super) fn reparent_to(
        &mut self,
        id: u32,
        old_parent: u32,
        new_parent: u32,
        (x, y): (i16, i16),
    ) -> Result<(), XError> {
        let was_mapped = self.window(id)?.mapped;
        if was_mapped {
            self.unmap_one(id)?;
        }
        self.mark_window_dirty(id);
        if let Some(parent) = self.windows.get_mut(&old_parent) {
            parent.children.retain(|child| *child != id);
        }
        if let Some(parent) = self.windows.get_mut(&new_parent) {
            parent.children.push(id);
        }
        let window = self.window_mut(id)?;
        window.parent = Some(new_parent);
        window.x = x;
        window.y = y;
        let override_redirect = window.override_redirect;
        for event_window in [id, old_parent, new_parent] {
            let mask = if event_window == id {
                STRUCTURE_NOTIFY
            } else {
                SUBSTRUCTURE_NOTIFY
            };
            let event = Event::ReparentNotify {
                event: event_window,
                window: id,
                parent: new_parent,
                x,
                y,
                override_redirect,
            };
            self.notify_listeners(event_window, mask, &event);
        }
        if was_mapped {
            self.map_one(id)?;
        }
        Ok(())
    }

    pub(super) fn restack(
        &mut self,
        id: u32,
        mode: u8,
        sibling: Option<u32>,
    ) -> Result<(), XError> {
        let parent_id = self.window(id)?.parent.ok_or(XError::Match)?;
        let order = self.window(parent_id)?.children.clone();
        if sibling.is_some_and(|s| s == id || !order.contains(&s)) {
            return Err(XError::Match);
        }
        let Some(placement) = self.placement(id, mode, sibling, &order) else {
            return Ok(());
        };
        let list = &mut self.window_mut(parent_id)?.children;
        list.retain(|&c| c != id);
        let index = match placement {
            Placement::Top => list.len(),
            Placement::Bottom => 0,
            Placement::AboveSibling(s) => list
                .iter()
                .position(|&c| c == s)
                .map_or(list.len(), |p| p + 1),
            Placement::BelowSibling(s) => list.iter().position(|&c| c == s).unwrap_or(0),
        };
        list.insert(index, id);
        Ok(())
    }

    /// Decides where the window goes. `None` means the stacking order stays as it is.
    fn placement(
        &self,
        id: u32,
        mode: u8,
        sibling: Option<u32>,
        order: &[u32],
    ) -> Option<Placement> {
        let position = |w: u32| order.iter().position(|&c| c == w);
        let me = self.window(id).ok()?;
        let others: Vec<u32> = match sibling {
            Some(s) => vec![s],
            None => order.iter().copied().filter(|&c| c != id).collect(),
        };
        let covers = |upper: u32, lower: u32| {
            let (Ok(u), Ok(l)) = (self.window(upper), self.window(lower)) else {
                return false;
            };
            u.mapped && l.mapped && position(upper) > position(lower) && overlaps(u, l)
        };
        let covered = me.mapped && others.iter().any(|&o| covers(o, id));
        let covering = me.mapped && others.iter().any(|&o| covers(id, o));
        let anchored_above = || sibling.map_or(Placement::Top, Placement::AboveSibling);
        let anchored_below = || sibling.map_or(Placement::Bottom, Placement::BelowSibling);
        match mode {
            STACK_ABOVE => Some(anchored_above()),
            STACK_BELOW => Some(anchored_below()),
            STACK_TOP_IF => covered.then(anchored_above),
            STACK_BOTTOM_IF => covering.then(anchored_below),
            STACK_OPPOSITE if covered => Some(anchored_above()),
            STACK_OPPOSITE => covering.then(anchored_below),
            _ => None,
        }
    }

    fn sibling_below(&self, id: u32) -> u32 {
        let Some(parent) = self.window(id).ok().and_then(|w| w.parent) else {
            return 0;
        };
        let Ok(parent) = self.window(parent) else {
            return 0;
        };
        match parent.children.iter().position(|&c| c == id) {
            Some(index) if index > 0 => parent.children[index - 1],
            _ => 0,
        }
    }

    pub fn get_geometry(&self, req: &Request) -> Result<Reply, XError> {
        let mut r = req.reader();
        let id = r.u32()?;
        r.end()?;
        if let Some((width, height, depth)) = self.pixmap_info(id) {
            let mut w = Writer::new(req.endian);
            w.u32(ROOT_WINDOW);
            w.i16(0);
            w.i16(0);
            w.u16(width);
            w.u16(height);
            w.u16(0);
            return Ok(Reply::new(depth, w));
        }
        let window = self.window(id).map_err(|_| XError::Drawable(id))?;
        let mut w = Writer::new(req.endian);
        w.u32(ROOT_WINDOW);
        w.i16(window.x);
        w.i16(window.y);
        w.u16(window.width);
        w.u16(window.height);
        w.u16(window.border);
        Ok(Reply::new(window.depth, w))
    }

    pub fn query_tree(&self, req: &Request) -> Result<Reply, XError> {
        let mut r = req.reader();
        let id = r.u32()?;
        r.end()?;
        let window = self.window(id)?;
        let mut w = Writer::new(req.endian);
        w.u32(ROOT_WINDOW);
        w.u32(window.parent.unwrap_or(0));
        w.u16(window.children.len() as u16);
        w.zeros(14);
        for &child in &window.children {
            w.u32(child);
        }
        Ok(Reply::new(0, w))
    }
}

/// Redirect and button-press selections belong to one client at a time.
fn claim_event_mask(window: &mut Window, client: ClientId, mask: u32) -> Result<(), XError> {
    let held_by_others = window
        .listeners
        .iter()
        .filter(|(other, _)| **other != client)
        .fold(0, |all, (_, held)| all | held);
    if mask & held_by_others & EXCLUSIVE_EVENT_MASKS != 0 {
        return Err(XError::Access);
    }
    if mask == 0 {
        window.listeners.remove(&client);
    } else {
        window.listeners.insert(client, mask);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use tokio::sync::mpsc::unbounded_channel;
    use tokio::sync::Notify;

    use super::*;
    use crate::display::Display;
    use crate::x11::codec::Endian;
    use crate::x11::resources::IRect;
    use crate::x11::screen::ROOT_WINDOW;

    const CREATE_WINDOW: u8 = 1;
    const CHANGE_WINDOW_ATTRIBUTES: u8 = 2;
    const MAP_WINDOW: u8 = 8;

    fn le(values: &[u32]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect()
    }

    /// A state with one client and one mapped 300x200 window with a border, and no dirty area.
    fn mapped_window() -> (State, ClientId, u32) {
        mapped_window_with(false)
    }

    fn mapped_window_with(window_manager: bool) -> (State, ClientId, u32) {
        let mut state = State::new(Display::new(640, 480));
        state.set_window_manager(window_manager);
        let (tx, _rx) = unbounded_channel();
        let (client, base) = state
            .add_client(
                Endian::Little,
                tx,
                Rc::new(Cell::new(0)),
                Rc::new(Notify::new()),
                Rc::new(Cell::new(false)),
            )
            .unwrap();
        let window = base | 1;
        // wid, parent, x and y, width and height, border and class, visual, mask, border pixel.
        let create = le(&[
            window,
            ROOT_WINDOW,
            10 | 10 << 16,
            300 | 200 << 16,
            2 | 1 << 16,
            0,
            CW_BORDER_PIXEL,
            0xFF,
        ]);
        state.process(client, CREATE_WINDOW, 24, &create);
        state.process(client, MAP_WINDOW, 0, &le(&[window]));
        state.flush_display();
        assert!(state.dirty.is_empty());
        (state, client, window)
    }

    #[test]
    fn a_new_cursor_or_the_same_border_draws_nothing_again() {
        let (mut state, client, window) = mapped_window();
        // xterm changes its cursor with every line of output.
        state.process(
            client,
            CHANGE_WINDOW_ATTRIBUTES,
            0,
            &le(&[window, CW_CURSOR, 0]),
        );
        assert!(state.dirty.is_empty(), "{:?}", state.dirty);
        state.process(
            client,
            CHANGE_WINDOW_ATTRIBUTES,
            0,
            &le(&[window, CW_BORDER_PIXEL, 0xFF]),
        );
        assert!(
            state.dirty.is_empty(),
            "the same border colour: {:?}",
            state.dirty
        );
    }

    #[test]
    fn a_window_that_becomes_override_redirect_loses_its_frame_on_the_screen() {
        let (mut state, client, window) = mapped_window_with(true);
        let frame = state
            .frame_of(window)
            .expect("the built-in manager frames it")
            .outer;
        state.process(
            client,
            CHANGE_WINDOW_ATTRIBUTES,
            0,
            &le(&[window, CW_OVERRIDE_REDIRECT, 1]),
        );
        assert!(state.frame_of(window).is_none());
        assert!(
            state
                .dirty
                .iter()
                .any(|rect| rect.intersect(frame) == Some(frame)),
            "the old frame area is drawn again: {:?} not in {:?}",
            frame,
            state.dirty
        );
    }

    #[test]
    fn a_change_of_the_root_leaves_the_windows_on_it_out() {
        let (mut state, _client, _window) = mapped_window();
        // The root just around the window, which covers most of it (as a terminal does).
        state.mark_area_dirty(ROOT_WINDOW, IRect::new(0, 0, 330, 230));
        assert!(!state.dirty.is_empty());
        let inside = IRect::new(100, 100, 1, 1);
        assert!(
            state
                .dirty
                .iter()
                .all(|rect| rect.intersect(inside).is_none()),
            "the window at (10, 10, 304x204) is not composed again: {:?}",
            state.dirty
        );
    }

    #[test]
    fn a_new_border_colour_is_drawn() {
        let (mut state, client, window) = mapped_window();
        state.process(
            client,
            CHANGE_WINDOW_ATTRIBUTES,
            0,
            &le(&[window, CW_BORDER_PIXEL, 0xFF00]),
        );
        assert!(!state.dirty.is_empty());
    }
}
