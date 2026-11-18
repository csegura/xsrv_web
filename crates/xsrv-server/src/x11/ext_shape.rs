// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! The SHAPE extension: windows that are not rectangles. A shape is a region in window
//! coordinates (relative to the inner top-left corner). The bounding shape decides what is drawn,
//! and the input shape decides what the pointer can hit. The clip shape is stored and reported.
use std::collections::HashSet;

use super::codec::Writer;
use super::dispatch::{Reply, Request};
use super::error::XError;
use super::event::Event;
use super::extensions::{event_code, SHAPE};
use super::region;
use super::resources::IRect;
use super::state::{ClientId, State};

const KIND_BOUNDING: u8 = 0;
const KIND_CLIP: u8 = 1;
const KIND_INPUT: u8 = 2;
const OP_SET: u8 = 0;
const OP_UNION: u8 = 1;
const OP_INTERSECT: u8 = 2;
const OP_SUBTRACT: u8 = 3;
const OP_INVERT: u8 = 4;
const MAX_RECTANGLES: usize = 2048;
pub const SHAPE_EVENT: u8 = 64;

/// The shapes of a window. `None` means the window is a rectangle.
#[derive(Default, Clone)]
pub struct Shapes {
    pub bounding: Option<Vec<IRect>>,
    pub clip: Option<Vec<IRect>>,
    pub input: Option<Vec<IRect>>,
    pub listeners: HashSet<ClientId>,
}

impl Shapes {
    fn slot(&mut self, kind: u8) -> Result<&mut Option<Vec<IRect>>, XError> {
        match kind {
            KIND_BOUNDING => Ok(&mut self.bounding),
            KIND_CLIP => Ok(&mut self.clip),
            KIND_INPUT => Ok(&mut self.input),
            other => Err(XError::Value(other.into())),
        }
    }

    fn get(&self, kind: u8) -> Result<&Option<Vec<IRect>>, XError> {
        match kind {
            KIND_BOUNDING => Ok(&self.bounding),
            KIND_CLIP => Ok(&self.clip),
            KIND_INPUT => Ok(&self.input),
            other => Err(XError::Value(other.into())),
        }
    }
}

fn rect_of_size(width: u16, height: u16) -> IRect {
    IRect::new(0, 0, width.into(), height.into())
}

impl State {
    /// The region that a kind of shape has now, with the default for a window without a shape.
    pub(super) fn shape_region(&self, window: u32, kind: u8) -> Result<Vec<IRect>, XError> {
        let w = self.window(window)?;
        if let Some(shape) = w.shapes.get(kind)? {
            return Ok(shape.clone());
        }
        let border = i32::from(w.border);
        let inner = rect_of_size(w.width, w.height);
        Ok(vec![if kind == KIND_CLIP {
            inner
        } else {
            inner.grow(border)
        }])
    }

    /// The bounding shape of a window in absolute coordinates. `None` for a rectangle.
    pub(super) fn bounding_shape_at(&self, id: u32, inner: (i32, i32)) -> Option<Vec<IRect>> {
        let shape = self.windows.get(&id)?.shapes.bounding.as_ref()?;
        Some(region::translate(shape, inner.0, inner.1))
    }

    /// True when the point, in window coordinates, can hit the window.
    pub(super) fn shape_hit(&self, id: u32, x: i32, y: i32) -> bool {
        let Some(window) = self.windows.get(&id) else {
            return false;
        };
        // SHAPE 1.1: the input region is the input shape inside the bounding shape.
        let inside = |shape: &Option<Vec<IRect>>| {
            shape
                .as_ref()
                .is_none_or(|rects| region::contains(rects, x, y))
        };
        inside(&window.shapes.bounding) && inside(&window.shapes.input)
    }

    pub(super) fn shape_request(&mut self, req: &Request) -> Result<Option<Reply>, XError> {
        match req.detail {
            0 => self.shape_query_version(req).map(Some),
            1 => self.shape_rectangles(req).map(|()| None),
            2 => self.shape_mask(req).map(|()| None),
            3 => self.shape_combine(req).map(|()| None),
            4 => self.shape_offset(req).map(|()| None),
            5 => self.shape_query_extents(req).map(Some),
            6 => self.shape_select_input(req).map(|()| None),
            7 => self.shape_input_selected(req).map(Some),
            8 => self.shape_get_rectangles(req).map(Some),
            _ => Err(XError::Request),
        }
    }

    fn shape_query_version(&self, req: &Request) -> Result<Reply, XError> {
        req.reader().end()?;
        let mut w = Writer::new(req.endian);
        w.u16(1);
        w.u16(1);
        Ok(Reply::new(0, w))
    }

    /// Combines `source` into one kind of shape of a window and tells the listeners.
    pub(super) fn apply_shape(
        &mut self,
        window: u32,
        kind: u8,
        operation: u8,
        source: Vec<IRect>,
    ) -> Result<(), XError> {
        if source.len() > MAX_RECTANGLES {
            return Err(XError::Alloc);
        }
        let current = self.shape_region(window, kind)?;
        // A shape is not cut to the window: the window may grow later. Painting clips it.
        let combined = match operation {
            OP_SET => Some(source),
            OP_UNION => Some(region::union(&current, &source)),
            OP_INTERSECT => region::intersect(&current, &source, MAX_RECTANGLES),
            OP_SUBTRACT => region::subtract(&current, &source, MAX_RECTANGLES),
            // The protocol defines Invert as `source - shape`.
            OP_INVERT => region::subtract(&source, &current, MAX_RECTANGLES),
            other => return Err(XError::Value(other.into())),
        };
        let combined = region::normalize(&combined.ok_or(XError::Alloc)?);
        if combined.len() > MAX_RECTANGLES {
            return Err(XError::Alloc);
        }
        self.mark_window_dirty(window);
        *self.window_mut(window)?.shapes.slot(kind)? = Some(combined);
        self.mark_window_dirty(window);
        self.notify_shape(window, kind);
        Ok(())
    }

    fn shape_rectangles(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let operation = r.u8()?;
        let kind = r.u8()?;
        r.skip(2)?;
        let window = r.u32()?;
        let x = i32::from(r.i16()?);
        let y = i32::from(r.i16()?);
        let mut rects = Vec::new();
        while r.remaining() > 0 {
            let rx = i32::from(r.i16()?);
            let ry = i32::from(r.i16()?);
            let (width, height) = (i32::from(r.u16()?), i32::from(r.u16()?));
            rects.push(IRect::new(x + rx, y + ry, width, height));
        }
        self.window(window)?;
        self.apply_shape(window, kind, operation, rects)
    }

    pub(super) fn clear_shape(&mut self, window: u32, kind: u8) -> Result<(), XError> {
        self.mark_window_dirty(window);
        *self.window_mut(window)?.shapes.slot(kind)? = None;
        self.mark_window_dirty(window);
        self.notify_shape(window, kind);
        Ok(())
    }

    fn shape_mask(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let operation = r.u8()?;
        let kind = r.u8()?;
        r.skip(2)?;
        let window = r.u32()?;
        let x = i32::from(r.i16()?);
        let y = i32::from(r.i16()?);
        let source = r.u32()?;
        r.end()?;
        self.window(window)?;
        if source == 0 {
            // A source of None removes the shape.
            return self.clear_shape(window, kind);
        }
        let bitmap = &self
            .pixmaps
            .get(&source)
            .ok_or(XError::Pixmap(source))?
            .surface;
        if bitmap.depth() != 1 {
            return Err(XError::Match);
        }
        let rects = region::translate(&region::from_bitmap(bitmap), x, y);
        self.apply_shape(window, kind, operation, rects)
    }

    fn shape_combine(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let operation = r.u8()?;
        let kind = r.u8()?;
        let source_kind = r.u8()?;
        r.skip(1)?;
        let window = r.u32()?;
        let x = i32::from(r.i16()?);
        let y = i32::from(r.i16()?);
        let source = r.u32()?;
        r.end()?;
        self.window(window)?;
        let rects = region::translate(&self.shape_region(source, source_kind)?, x, y);
        self.apply_shape(window, kind, operation, rects)
    }

    fn shape_offset(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let kind = r.u8()?;
        r.skip(3)?;
        let window = r.u32()?;
        let x = i32::from(r.i16()?);
        let y = i32::from(r.i16()?);
        r.end()?;
        self.window(window)?;
        let Some(shape) = self.window(window)?.shapes.get(kind)?.clone() else {
            return Ok(());
        };
        self.mark_window_dirty(window);
        *self.window_mut(window)?.shapes.slot(kind)? = Some(region::translate(&shape, x, y));
        self.mark_window_dirty(window);
        self.notify_shape(window, kind);
        Ok(())
    }

    fn shape_query_extents(&self, req: &Request) -> Result<Reply, XError> {
        let mut r = req.reader();
        let window = r.u32()?;
        r.end()?;
        let shapes = &self.window(window)?.shapes;
        let mut w = Writer::new(req.endian);
        w.u8(shapes.bounding.is_some().into());
        w.u8(shapes.clip.is_some().into());
        w.zeros(2);
        for kind in [KIND_BOUNDING, KIND_CLIP] {
            let area = region::extents(&self.shape_region(window, kind)?)
                .unwrap_or(IRect::new(0, 0, 0, 0));
            w.i16(area.x0 as i16);
            w.i16(area.y0 as i16);
            w.u16(area.width() as u16);
            w.u16(area.height() as u16);
        }
        Ok(Reply::new(0, w))
    }

    fn shape_select_input(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let window = r.u32()?;
        let enable = r.u8()? != 0;
        r.skip(3)?;
        r.end()?;
        let listeners = &mut self.window_mut(window)?.shapes.listeners;
        if enable {
            listeners.insert(req.client);
        } else {
            listeners.remove(&req.client);
        }
        Ok(())
    }

    fn shape_input_selected(&self, req: &Request) -> Result<Reply, XError> {
        let mut r = req.reader();
        let window = r.u32()?;
        r.end()?;
        let enabled = self.window(window)?.shapes.listeners.contains(&req.client);
        Ok(Reply::new(enabled.into(), Writer::new(req.endian)))
    }

    fn shape_get_rectangles(&self, req: &Request) -> Result<Reply, XError> {
        let mut r = req.reader();
        let window = r.u32()?;
        let kind = r.u8()?;
        r.skip(3)?;
        r.end()?;
        let rects = self.shape_region(window, kind)?;
        let mut w = Writer::new(req.endian);
        w.u32(rects.len() as u32);
        w.zeros(20);
        for rect in rects {
            w.i16(rect.x0 as i16);
            w.i16(rect.y0 as i16);
            w.u16(rect.width() as u16);
            w.u16(rect.height() as u16);
        }
        const UNSORTED: u8 = 0;
        Ok(Reply::new(UNSORTED, w))
    }

    /// Sends ShapeNotify to the clients that asked for it.
    fn notify_shape(&self, window: u32, kind: u8) {
        let Ok(w) = self.window(window) else {
            return;
        };
        let shaped = w.shapes.get(kind).ok().is_some_and(Option::is_some);
        let area = self
            .shape_region(window, kind)
            .ok()
            .and_then(|rects| region::extents(&rects))
            .unwrap_or(IRect::new(0, 0, 0, 0));
        let time = self.timestamp();
        for &client in &w.shapes.listeners {
            let event = Event::ShapeNotify {
                kind,
                window,
                x: area.x0 as i16,
                y: area.y0 as i16,
                width: area.width() as u16,
                height: area.height() as u16,
                time,
                shaped,
                code: event_code(SHAPE, 0).unwrap_or(SHAPE_EVENT),
            };
            self.send_event_to(client, &event);
        }
    }
}
