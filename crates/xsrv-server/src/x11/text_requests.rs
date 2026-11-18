// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

use super::codec::{padding, Reader, Writer};
use super::dispatch::{Reply, Request};
use super::error::XError;
use super::font::{self, ASCENT, DEFAULT_CHAR, DESCENT, GLYPH_HEIGHT, GLYPH_WIDTH, LAST_CHAR};
use super::resources::{Gc, IRect, Surface, CELL_WIDTH};
use super::state::State;

const FONT_SHIFT: u8 = 255;
const FUNCTION_COPY: u8 = 3;
const CHARINFO_LEN: u32 = 12;
const FONT_INFO_LEN: usize = 52;

struct TextItem {
    delta: i32,
    codes: Vec<u16>,
}

fn codes8(bytes: &[u8]) -> Vec<u16> {
    bytes.iter().map(|&b| u16::from(b)).collect()
}

fn codes16(bytes: &[u8]) -> Vec<u16> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_be_bytes(*pair))
        .collect()
}

fn text_box(x: i32, baseline: i32, glyphs: usize) -> IRect {
    IRect::new(
        x,
        baseline - i32::from(ASCENT),
        glyphs as i32 * GLYPH_WIDTH,
        GLYPH_HEIGHT as i32,
    )
}

/// Draws the set pixels of a glyph as horizontal runs.
fn draw_glyph(surface: &mut Surface, x: i32, baseline: i32, code: u16, gc: &Gc, function: u8) {
    let top = baseline - i32::from(ASCENT);
    for (row, bits) in font::glyph(code).into_iter().enumerate() {
        let mut column = 0;
        while column < GLYPH_WIDTH {
            if bits >> column & 1 == 0 {
                column += 1;
                continue;
            }
            let start = column;
            while column < GLYPH_WIDTH && bits >> column & 1 == 1 {
                column += 1;
            }
            let run = IRect::new(x + start, top + row as i32, column - start, 1);
            surface.fill_rect(run, gc.foreground, function, gc.plane_mask);
        }
    }
}

// A glyph is one cell of `Surface::draw_cells`.
const _: () = assert!(GLYPH_WIDTH as usize == CELL_WIDTH);

/// Draws glyphs side by side, the first at `x`. `background` fills the rest of the text box;
/// without it only the glyph pixels are drawn. Returns the area that may have other pixels now.
fn draw_text(
    surface: &mut Surface,
    (x, baseline): (i32, i32),
    codes: &[u16],
    gc: &Gc,
    background: Option<u32>,
    function: u8,
) -> Option<IRect> {
    if codes.is_empty() {
        return None;
    }
    let area = text_box(x, baseline, codes.len());
    if surface.writes_plainly(function, gc.plane_mask) {
        let glyphs: Vec<[u8; GLYPH_HEIGHT]> = codes.iter().map(|&code| font::glyph(code)).collect();
        return surface.draw_cells(area, gc.foreground, background, |cell, row| {
            glyphs[cell][row]
        });
    }
    if let Some(background) = background {
        surface.fill_rect(area, background, FUNCTION_COPY, gc.plane_mask);
    }
    for (index, &code) in codes.iter().enumerate() {
        let left = x + index as i32 * GLYPH_WIDTH;
        draw_glyph(surface, left, baseline, code, gc, function);
    }
    Some(area)
}

impl State {
    pub fn open_font(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let id = r.u32()?;
        let len = r.u16()? as usize;
        r.skip(2)?;
        let name = r.bytes(len)?;
        r.skip(padding(len))?;
        r.end()?;
        if !self.owns_id(req.client, id) || self.id_in_use(id) {
            return Err(XError::IdChoice(id));
        }
        self.check_resource_quota(req.client)?;
        if !font::font_exists(name) {
            let name = String::from_utf8_lossy(name).into_owned();
            self.log_once(format!("font {name}"), || {
                format!("xsrv-web: font not found: {name:?}")
            });
            return Err(XError::Name);
        }
        self.fonts.insert(id);
        self.owners.insert(id, req.client);
        Ok(())
    }

    pub fn close_font(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let id = r.u32()?;
        r.end()?;
        if !self.fonts.remove(&id) {
            return Err(XError::Font(id));
        }
        self.owners.remove(&id);
        Ok(())
    }

    /// Every open font is the same built-in font, so the answer does not depend on the id.
    /// Characters without a glyph get an all-zero entry and `all_chars_exist` is false, as X does.
    pub fn query_font(&mut self, req: &Request) -> Result<Reply, XError> {
        let mut r = req.reader();
        let id = r.u32()?;
        r.end()?;
        if !self.fonts.contains(&id) && !self.gcs.contains_key(&id) {
            return Err(XError::Font(id));
        }
        let chars = u32::from(LAST_CHAR) + 1;
        let properties = self.font_properties();
        let mut w = Writer::new(req.endian);
        write_font_info(&mut w, &properties, chars);
        for code in 0..chars {
            if font::has_glyph(code as u16) {
                write_charinfo(&mut w);
            } else {
                w.zeros(CHARINFO_LEN as usize);
            }
        }
        debug_assert_eq!(
            w.len(),
            FONT_INFO_LEN + 8 * properties.len() + (chars * CHARINFO_LEN) as usize
        );
        Ok(Reply::new(0, w))
    }

    /// One reply for each font, then an empty one. Every reply carries the metrics of the font.
    pub fn list_fonts_with_info(&mut self, req: &Request) -> Result<Option<Reply>, XError> {
        let mut r = req.reader();
        let max = usize::from(r.u16()?);
        let len = r.u16()? as usize;
        let pattern = r.bytes(len)?;
        r.skip(padding(len))?;
        r.end()?;
        let names = font::matching_names(pattern, max);
        let seq = self.current_sequence(req.client).unwrap_or(0);
        let total = names.len();
        let properties = self.font_properties();
        for (index, name) in names.into_iter().enumerate() {
            let mut w = Writer::new(req.endian);
            write_font_info(&mut w, &properties, (total - index - 1) as u32);
            w.bytes(name.as_bytes());
            let reply = Reply::new(name.len() as u8, w);
            self.send_raw(req.client, reply.encode(req.endian, seq));
        }
        // The last reply has a name of length 0. It keeps the length of the others.
        let mut empty = Writer::new(req.endian);
        empty.zeros(FONT_INFO_LEN);
        let end = Reply::new(0, empty);
        self.send_raw(req.client, end.encode(req.endian, seq));
        Ok(None)
    }

    pub fn list_fonts(&self, req: &Request) -> Result<Reply, XError> {
        let mut r = req.reader();
        let max = usize::from(r.u16()?);
        let len = r.u16()? as usize;
        let pattern = r.bytes(len)?;
        r.skip(padding(len))?;
        r.end()?;
        let names = font::matching_names(pattern, max);
        let mut w = Writer::new(req.endian);
        w.u16(names.len() as u16);
        w.zeros(22);
        for name in names {
            w.u8(name.len() as u8);
            w.bytes(name.as_bytes());
        }
        Ok(Reply::new(0, w))
    }

    pub fn image_text8(&mut self, req: &Request) -> Result<(), XError> {
        self.image_text(req, 1, codes8)
    }

    pub fn image_text16(&mut self, req: &Request) -> Result<(), XError> {
        self.image_text(req, 2, codes16)
    }

    /// Fills the text box with the background pixel, then draws the glyphs. The GC function is ignored.
    fn image_text(
        &mut self,
        req: &Request,
        char_size: usize,
        decode: fn(&[u8]) -> Vec<u16>,
    ) -> Result<(), XError> {
        let count = usize::from(req.detail);
        let mut r = req.reader();
        let drawable = r.u32()?;
        let gc_id = r.u32()?;
        let x = i32::from(r.i16()?);
        let y = i32::from(r.i16()?);
        let bytes = r.bytes(count * char_size)?;
        r.skip(padding(count * char_size))?;
        r.end()?;
        let codes = decode(bytes);
        self.draw(drawable, gc_id, |surface, gc| {
            draw_text(
                surface,
                (x, y),
                &codes,
                gc,
                Some(gc.background),
                FUNCTION_COPY,
            )
        })
    }

    pub fn poly_text8(&mut self, req: &Request) -> Result<(), XError> {
        self.poly_text(req, 1, codes8)
    }

    pub fn poly_text16(&mut self, req: &Request) -> Result<(), XError> {
        self.poly_text(req, 2, codes16)
    }

    /// Every font is the same one, so a font-shift item is only checked and then skipped.
    /// Draws text items with the foreground pixel and leaves the background alone.
    fn poly_text(
        &mut self,
        req: &Request,
        char_size: usize,
        decode: fn(&[u8]) -> Vec<u16>,
    ) -> Result<(), XError> {
        let mut r = req.reader();
        let drawable = r.u32()?;
        let gc_id = r.u32()?;
        let x = i32::from(r.i16()?);
        let y = i32::from(r.i16()?);
        let items = self.read_text_items(&mut r, char_size, decode)?;
        self.draw(drawable, gc_id, |surface, gc| {
            let mut pen = x;
            let mut touched: Option<IRect> = None;
            for TextItem { delta, codes } in &items {
                pen += delta;
                let area = draw_text(surface, (pen, y), codes, gc, None, gc.function);
                pen += codes.len() as i32 * GLYPH_WIDTH;
                touched = match (touched, area) {
                    (Some(a), Some(b)) => Some(a.union(b)),
                    (a, b) => a.or(b),
                };
            }
            touched
        })
    }

    fn read_text_items(
        &self,
        r: &mut Reader,
        char_size: usize,
        decode: fn(&[u8]) -> Vec<u16>,
    ) -> Result<Vec<TextItem>, XError> {
        let mut items = Vec::new();
        while r.remaining() >= 2 {
            let first = r.u8()?;
            if first == FONT_SHIFT {
                let id = u32::from_be_bytes([r.u8()?, r.u8()?, r.u8()?, r.u8()?]);
                if !self.fonts.contains(&id) && !self.gcs.contains_key(&id) {
                    return Err(XError::Font(id));
                }
                continue;
            }
            let delta = i32::from(r.u8()? as i8);
            let bytes = r.bytes(usize::from(first) * char_size)?;
            items.push(TextItem {
                delta,
                codes: decode(bytes),
            });
        }
        Ok(items)
    }
}

/// The 52 bytes that QueryFont and ListFontsWithInfo start with. `tail` is the number of
/// character infos that follow (QueryFont) or the number of replies still to come.
impl State {
    /// The atom and value pairs of the font properties. A property whose atom cannot be made
    /// (the atom table is full) is left out.
    fn font_properties(&mut self) -> Vec<(u32, u32)> {
        font::properties()
            .into_iter()
            .filter_map(|(name, value)| {
                let name = self.atoms.intern(name.as_bytes())?;
                let value = match value {
                    font::PropertyValue::Atom(text) => self.atoms.intern(text.as_bytes())?,
                    font::PropertyValue::Card(number) => number,
                };
                Some((name, value))
            })
            .collect()
    }
}

fn write_font_info(w: &mut Writer, properties: &[(u32, u32)], tail: u32) {
    debug_assert_eq!(w.len(), 0);
    write_charinfo(w);
    w.zeros(4);
    write_charinfo(w);
    w.zeros(4);
    w.u16(0);
    w.u16(LAST_CHAR);
    w.u16(DEFAULT_CHAR);
    w.u16(properties.len() as u16);
    w.u8(0);
    w.u8(0);
    w.u8(0);
    w.u8(0);
    w.i16(ASCENT);
    w.i16(DESCENT);
    w.u32(tail);
    for &(name, value) in properties {
        w.u32(name);
        w.u32(value);
    }
}

fn write_charinfo(w: &mut Writer) {
    w.i16(0);
    w.i16(GLYPH_WIDTH as i16);
    w.i16(GLYPH_WIDTH as i16);
    w.i16(ASCENT);
    w.i16(DESCENT);
    w.u16(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_pairs_are_read_high_byte_first() {
        assert_eq!(codes16(&[0, 65, 1, 2]), [65, 0x0102]);
    }

    #[test]
    fn text_box_sits_on_the_baseline() {
        let area = text_box(10, 30, 3);
        assert_eq!(
            (area.x0, area.y0, area.width(), area.height()),
            (10, 17, 24, 16)
        );
    }
}
