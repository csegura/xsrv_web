// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! The RENDER extension, version 0.1: pictures, Composite, FillRectangles and glyphs.
//!
//! cairo-xcb (i3bar, libi3) takes its RENDER paths at 0.1. Without RENDER it draws in a local
//! copy of each pixmap and can lose drawing (see `docs/external-wm.md`). Transforms, filters,
//! trapezoids and gradients (0.4 and later) are not offered; cairo then draws those on the client
//! and uploads the result.
mod composite;
mod draw;
mod format;
mod glyphs;
mod picture;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use self::format::{Format, FORMATS};
use self::glyphs::GlyphSet;
use self::picture::{Orphan, Picture, Target};
use super::codec::Writer;
use super::dispatch::{Reply, Request};
use super::error::XError;
use super::extensions::{first_error, RENDER};
use super::screen::{ROOT_DEPTH, ROOT_VISUAL};
use super::state::{ClientId, State};

const MAJOR_VERSION: u32 = 0;
const MINOR_VERSION: u32 = 1;

const QUERY_VERSION: u8 = 0;
const QUERY_PICT_FORMATS: u8 = 1;
const QUERY_PICT_INDEX_VALUES: u8 = 2;
const CREATE_PICTURE: u8 = 4;
const CHANGE_PICTURE: u8 = 5;
const SET_PICTURE_CLIP_RECTANGLES: u8 = 6;
const FREE_PICTURE: u8 = 7;
const COMPOSITE: u8 = 8;
const CREATE_GLYPH_SET: u8 = 17;
const REFERENCE_GLYPH_SET: u8 = 18;
const FREE_GLYPH_SET: u8 = 19;
const ADD_GLYPHS: u8 = 20;
const FREE_GLYPHS: u8 = 22;
const COMPOSITE_GLYPHS_8: u8 = 23;
const COMPOSITE_GLYPHS_16: u8 = 24;
const COMPOSITE_GLYPHS_32: u8 = 25;
const FILL_RECTANGLES: u8 = 26;

const PICT_TYPE_DIRECT: u8 = 1;

/// The errors of RENDER, with their offset from the first error code. Code 2 (PictOp) is never
/// sent: a bad operator gives BadValue, as in Xorg.
#[derive(Clone, Copy)]
pub(super) enum RenderError {
    PictFormat = 0,
    Picture = 1,
    GlyphSet = 3,
    Glyph = 4,
}

impl RenderError {
    pub(super) fn with(self, value: u32) -> XError {
        XError::Extension(first_error(RENDER) + self as u8, value)
    }
}

#[derive(Default)]
pub struct RenderState {
    pictures: HashMap<u32, Picture>,
    /// A glyph set can have several ids (ReferenceGlyphSet).
    glyph_sets: HashMap<u32, Rc<RefCell<GlyphSet>>>,
    /// The pictures of each drawable.
    by_target: HashMap<Target, Vec<u32>>,
    orphans: HashMap<u64, Orphan>,
    next_orphan: u64,
    /// The clip rectangles of all pictures.
    clip_rects: usize,
    /// The cost of all glyphs, and of the glyphs of each client (see `glyphs.rs`).
    glyph_bytes: usize,
    client_glyph_bytes: HashMap<ClientId, usize>,
}

impl RenderState {
    pub fn uses_id(&self, id: u32) -> bool {
        self.pictures.contains_key(&id) || self.glyph_sets.contains_key(&id)
    }
}

impl State {
    pub(super) fn render_request(&mut self, req: &Request) -> Result<Option<Reply>, XError> {
        match req.detail {
            QUERY_VERSION => query_version(req).map(Some),
            QUERY_PICT_FORMATS => query_pict_formats(req).map(Some),
            // There are no indexed formats.
            QUERY_PICT_INDEX_VALUES => Err(XError::Match),
            CREATE_PICTURE => self.create_picture(req).map(|()| None),
            CHANGE_PICTURE => self.change_picture(req).map(|()| None),
            SET_PICTURE_CLIP_RECTANGLES => self.set_picture_clip_rectangles(req).map(|()| None),
            FREE_PICTURE => self.free_picture(req).map(|()| None),
            COMPOSITE => self.composite(req).map(|()| None),
            FILL_RECTANGLES => self.fill_rectangles(req).map(|()| None),
            CREATE_GLYPH_SET => self.create_glyph_set(req).map(|()| None),
            REFERENCE_GLYPH_SET => self.reference_glyph_set(req).map(|()| None),
            FREE_GLYPH_SET => self.free_glyph_set(req).map(|()| None),
            ADD_GLYPHS => self.add_glyphs(req).map(|()| None),
            FREE_GLYPHS => self.free_glyphs(req).map(|()| None),
            COMPOSITE_GLYPHS_8 => self.composite_glyphs(req, 1).map(|()| None),
            COMPOSITE_GLYPHS_16 => self.composite_glyphs(req, 2).map(|()| None),
            COMPOSITE_GLYPHS_32 => self.composite_glyphs(req, 4).map(|()| None),
            _ => Err(XError::Request),
        }
    }
}

/// The server version, but not above the version that the client asks for.
fn query_version(req: &Request) -> Result<Reply, XError> {
    let mut r = req.reader();
    let major = r.u32()?;
    let minor = r.u32()?;
    r.end()?;
    let (major, minor) = if major > MAJOR_VERSION {
        (MAJOR_VERSION, MINOR_VERSION)
    } else {
        (major, minor.min(MINOR_VERSION))
    };
    let mut w = Writer::new(req.endian);
    w.u32(major);
    w.u32(minor);
    w.zeros(16);
    Ok(Reply::new(0, w))
}

/// The four formats, and one screen whose root visual has the x8r8g8b8 format.
fn query_pict_formats(req: &Request) -> Result<Reply, XError> {
    req.reader().end()?;
    let mut w = Writer::new(req.endian);
    w.u32(FORMATS.len() as u32);
    w.u32(1);
    w.u32(FORMATS.len() as u32);
    w.u32(1);
    // numSubpixel: read only from 0.6 on.
    w.u32(0);
    w.zeros(4);
    for format in FORMATS {
        w.u32(format.id());
        w.u8(PICT_TYPE_DIRECT);
        w.u8(format.depth());
        w.zeros(2);
        for (shift, mask) in format.channels() {
            w.u16(shift);
            w.u16(mask);
        }
        w.u32(0);
    }
    w.u32(FORMATS.len() as u32);
    w.u32(Format::Rgb24.id());
    for format in FORMATS {
        let root = format.depth() == ROOT_DEPTH;
        w.u8(format.depth());
        w.u8(0);
        w.u16(u16::from(root));
        w.zeros(4);
        if root {
            w.u32(ROOT_VISUAL);
            w.u32(format.id());
        }
    }
    Ok(Reply::new(0, w))
}
