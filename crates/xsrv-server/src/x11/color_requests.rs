// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

use super::codec::{padding, Writer};
use super::colors::{color_of, parse_color, pixel_for, Rgb16};
use super::dispatch::{Reply, Request};
use super::error::XError;
use super::resources::PIXEL_MASK;
use super::screen::DEFAULT_COLORMAP;
use super::state::State;

fn require_default_colormap(id: u32) -> Result<(), XError> {
    if id == DEFAULT_COLORMAP {
        Ok(())
    } else {
        Err(XError::Colormap(id))
    }
}

fn write_color(w: &mut Writer, (red, green, blue): Rgb16) {
    w.u16(red);
    w.u16(green);
    w.u16(blue);
}

impl State {
    pub fn alloc_color(&self, req: &Request) -> Result<Reply, XError> {
        let mut r = req.reader();
        require_default_colormap(r.u32()?)?;
        let color = (r.u16()?, r.u16()?, r.u16()?);
        r.skip(2)?;
        r.end()?;
        let pixel = pixel_for(color);
        let mut w = Writer::new(req.endian);
        write_color(&mut w, color_of(pixel));
        w.zeros(2);
        w.u32(pixel);
        Ok(Reply::new(0, w))
    }

    pub fn alloc_named_color(&self, req: &Request) -> Result<Reply, XError> {
        let (exact, visual) = self.lookup_named(req)?;
        let mut w = Writer::new(req.endian);
        w.u32(pixel_for(exact));
        write_color(&mut w, exact);
        write_color(&mut w, visual);
        Ok(Reply::new(0, w))
    }

    pub fn lookup_color(&self, req: &Request) -> Result<Reply, XError> {
        let (exact, visual) = self.lookup_named(req)?;
        let mut w = Writer::new(req.endian);
        write_color(&mut w, exact);
        write_color(&mut w, visual);
        Ok(Reply::new(0, w))
    }

    /// Returns the exact color and the color the screen can show.
    fn lookup_named(&self, req: &Request) -> Result<(Rgb16, Rgb16), XError> {
        let mut r = req.reader();
        require_default_colormap(r.u32()?)?;
        let len = r.u16()? as usize;
        r.skip(2)?;
        let name = r.bytes(len)?;
        r.skip(padding(len))?;
        r.end()?;
        let exact = parse_color(name).ok_or(XError::Name)?;
        Ok((exact, color_of(pixel_for(exact))))
    }

    pub fn query_colors(&self, req: &Request) -> Result<Reply, XError> {
        let mut r = req.reader();
        require_default_colormap(r.u32()?)?;
        let mut pixels = Vec::new();
        while r.remaining() > 0 {
            let pixel = r.u32()?;
            if pixel > PIXEL_MASK {
                return Err(XError::Value(pixel));
            }
            pixels.push(pixel);
        }
        let mut w = Writer::new(req.endian);
        w.u16(u16::try_from(pixels.len()).map_err(|_| XError::Length)?);
        w.zeros(22);
        for pixel in pixels {
            write_color(&mut w, color_of(pixel));
            w.zeros(2);
        }
        Ok(Reply::new(0, w))
    }

    /// TrueColor pixels are not allocated, so there is nothing to free.
    pub fn free_colors(&self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        require_default_colormap(r.u32()?)?;
        r.skip(4)?;
        while r.remaining() > 0 {
            r.u32()?;
        }
        Ok(())
    }

    /// Any size is fine for a cursor, a tile or a stipple, so the answer is the size asked for.
    pub fn query_best_size(&self, req: &Request) -> Result<Reply, XError> {
        if req.detail > 2 {
            return Err(XError::Value(req.detail.into()));
        }
        let mut r = req.reader();
        let drawable = r.u32()?;
        let width = r.u16()?;
        let height = r.u16()?;
        r.end()?;
        if !self.drawable_exists(drawable) {
            return Err(XError::Drawable(drawable));
        }
        let mut w = Writer::new(req.endian);
        w.u16(width);
        w.u16(height);
        Ok(Reply::new(0, w))
    }
}
