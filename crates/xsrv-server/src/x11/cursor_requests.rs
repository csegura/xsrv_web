// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

use wire::CursorShape;

use super::dispatch::Request;
use super::error::XError;
use super::state::State;

impl State {
    /// A cursor id of 0 means "no cursor" and is always valid.
    pub(super) fn check_cursor(&self, cursor: Option<u32>) -> Result<(), XError> {
        match cursor {
            Some(id) if id != 0 && !self.cursors.contains_key(&id) => Err(XError::Cursor(id)),
            _ => Ok(()),
        }
    }

    /// The glyph of the standard cursor font decides which pointer the page shows.
    pub fn create_glyph_cursor(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let id = r.u32()?;
        let source_font = r.u32()?;
        let mask_font = r.u32()?;
        let source_char = r.u16()?;
        r.skip(2)?;
        r.skip(12)?;
        r.end()?;
        if !self.owns_id(req.client, id) || self.id_in_use(id) {
            return Err(XError::IdChoice(id));
        }
        self.check_resource_quota(req.client)?;
        for font in [source_font, mask_font] {
            if font != 0 && !self.fonts.contains(&font) {
                return Err(XError::Font(font));
            }
        }
        if source_font == 0 {
            return Err(XError::Font(0));
        }
        self.cursors.insert(id, shape_for_glyph(source_char));
        self.owners.insert(id, req.client);
        Ok(())
    }

    /// CreateCursor: a cursor from two bitmaps. The page cannot show their picture, so the cursor
    /// is the default pointer, or no pointer when the mask hides every pixel (programs hide the
    /// pointer that way). libXcursor names a theme cursor right after it makes it (XFIXES
    /// SetCursorName), and the name then chooses the shape.
    pub fn create_cursor(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let id = r.u32()?;
        let source = r.u32()?;
        let mask = r.u32()?;
        r.skip(12)?;
        let (x, y) = (i32::from(r.u16()?), i32::from(r.u16()?));
        r.end()?;
        if !self.owns_id(req.client, id) || self.id_in_use(id) {
            return Err(XError::IdChoice(id));
        }
        self.check_resource_quota(req.client)?;
        let bitmap = |id: u32| {
            let pixmap = self.pixmaps.get(&id).ok_or(XError::Pixmap(id))?;
            match pixmap.surface.depth() {
                1 => Ok(&pixmap.surface),
                _ => Err(XError::Match),
            }
        };
        let bounds = bitmap(source)?.bounds();
        // As in the X.Org server, the hot spot may be on the right or bottom edge: GTK hides the
        // pointer while you type with a 1x1 cursor whose hot spot is (1, 1).
        if x > bounds.width() || y > bounds.height() {
            return Err(XError::Match);
        }
        let shape = match mask {
            0 => CursorShape::Default,
            mask => {
                let mask = bitmap(mask)?;
                if mask.bounds() != bounds {
                    return Err(XError::Match);
                }
                if mask.read_rect(bounds).iter().all(|&byte| byte == 0) {
                    CursorShape::Hidden
                } else {
                    CursorShape::Default
                }
            }
        };
        self.cursors.insert(id, shape);
        self.owners.insert(id, req.client);
        Ok(())
    }

    /// XFIXES SetCursorName: a name of the cursor theme chooses the pointer of the page. An unknown
    /// name, and a hidden cursor, keep their shape.
    pub(super) fn set_cursor_name(&mut self, cursor: u32, name: &[u8]) -> Result<(), XError> {
        let shape = self
            .cursors
            .get_mut(&cursor)
            .ok_or(XError::Cursor(cursor))?;
        if let (false, Some(named)) = (*shape == CursorShape::Hidden, shape_for_name(name)) {
            *shape = named;
        }
        Ok(())
    }

    /// The page shows its own pointer, so the colours of a cursor have no effect.
    pub fn recolor_cursor(&self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let id = r.u32()?;
        r.skip(12)?;
        r.end()?;
        if self.cursors.contains_key(&id) {
            Ok(())
        } else {
            Err(XError::Cursor(id))
        }
    }

    pub fn free_cursor(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let id = r.u32()?;
        r.end()?;
        if self.cursors.remove(&id).is_none() {
            return Err(XError::Cursor(id));
        }
        self.owners.remove(&id);
        Ok(())
    }
}

/// Maps a glyph of the standard X11 cursor font (`XC_*`, always even) to a pointer shape.
fn shape_for_glyph(glyph: u16) -> CursorShape {
    match glyph {
        152 => CursorShape::Text,
        150 => CursorShape::Wait,
        52 => CursorShape::Move,
        58 | 60 => CursorShape::Pointer,
        30 | 34 => CursorShape::Crosshair,
        70 | 96 | 108 => CursorShape::ResizeEw,
        16 | 116 | 138 => CursorShape::ResizeNs,
        12 | 136 => CursorShape::ResizeNesw,
        14 | 134 => CursorShape::ResizeNwse,
        88 => CursorShape::NotAllowed,
        92 => CursorShape::Help,
        _ => CursorShape::Default,
    }
}

/// The names of X cursor themes (and the CSS names that newer themes also use).
fn shape_for_name(name: &[u8]) -> Option<CursorShape> {
    Some(match name {
        b"left_ptr" | b"default" | b"arrow" | b"top_left_arrow" => CursorShape::Default,
        b"xterm" | b"text" | b"ibeam" | b"vertical-text" => CursorShape::Text,
        b"watch" | b"wait" | b"progress" | b"left_ptr_watch" => CursorShape::Wait,
        b"fleur" | b"move" | b"all-scroll" | b"grabbing" | b"size_all" => CursorShape::Move,
        b"hand1" | b"hand2" | b"pointer" | b"pointing_hand" => CursorShape::Pointer,
        b"crosshair" | b"cross" | b"tcross" => CursorShape::Crosshair,
        b"sb_h_double_arrow" | b"h_double_arrow" | b"ew-resize" | b"col-resize" | b"left_side"
        | b"right_side" | b"e-resize" | b"w-resize" | b"size_hor" => CursorShape::ResizeEw,
        b"sb_v_double_arrow" | b"v_double_arrow" | b"ns-resize" | b"row-resize" | b"top_side"
        | b"bottom_side" | b"n-resize" | b"s-resize" | b"size_ver" => CursorShape::ResizeNs,
        b"bottom_left_corner"
        | b"top_right_corner"
        | b"nesw-resize"
        | b"ne-resize"
        | b"sw-resize"
        | b"size_bdiag" => CursorShape::ResizeNesw,
        b"top_left_corner"
        | b"bottom_right_corner"
        | b"nwse-resize"
        | b"nw-resize"
        | b"se-resize"
        | b"size_fdiag" => CursorShape::ResizeNwse,
        b"not-allowed" | b"crossed_circle" | b"no-drop" | b"forbidden" => CursorShape::NotAllowed,
        b"help" | b"question_arrow" | b"whats_this" => CursorShape::Help,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_names_have_a_shape_and_unknown_names_none() {
        assert_eq!(shape_for_name(b"xterm"), Some(CursorShape::Text));
        assert_eq!(shape_for_name(b"hand2"), Some(CursorShape::Pointer));
        assert_eq!(
            shape_for_name(b"nwse-resize"),
            Some(CursorShape::ResizeNwse)
        );
        assert_eq!(shape_for_name(b"dnd-ask"), None);
    }

    #[test]
    fn common_glyphs_have_a_shape() {
        assert_eq!(shape_for_glyph(152), CursorShape::Text);
        assert_eq!(shape_for_glyph(68), CursorShape::Default);
        assert_eq!(shape_for_glyph(150), CursorShape::Wait);
        assert_eq!(shape_for_glyph(9999), CursorShape::Default);
    }
}
