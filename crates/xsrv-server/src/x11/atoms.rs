// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

use std::collections::HashMap;

pub const MAX_ATOMS: usize = 65536;
pub const MAX_ATOM_NAME: usize = 1024;

const PREDEFINED: [&str; 68] = [
    "PRIMARY",
    "SECONDARY",
    "ARC",
    "ATOM",
    "BITMAP",
    "CARDINAL",
    "COLORMAP",
    "CURSOR",
    "CUT_BUFFER0",
    "CUT_BUFFER1",
    "CUT_BUFFER2",
    "CUT_BUFFER3",
    "CUT_BUFFER4",
    "CUT_BUFFER5",
    "CUT_BUFFER6",
    "CUT_BUFFER7",
    "DRAWABLE",
    "FONT",
    "INTEGER",
    "PIXMAP",
    "POINT",
    "RECTANGLE",
    "RESOURCE_MANAGER",
    "RGB_COLOR_MAP",
    "RGB_BEST_MAP",
    "RGB_BLUE_MAP",
    "RGB_DEFAULT_MAP",
    "RGB_GRAY_MAP",
    "RGB_GREEN_MAP",
    "RGB_RED_MAP",
    "STRING",
    "VISUALID",
    "WINDOW",
    "WM_COMMAND",
    "WM_HINTS",
    "WM_CLIENT_MACHINE",
    "WM_ICON_NAME",
    "WM_ICON_SIZE",
    "WM_NAME",
    "WM_NORMAL_HINTS",
    "WM_SIZE_HINTS",
    "WM_ZOOM_HINTS",
    "MIN_SPACE",
    "NORM_SPACE",
    "MAX_SPACE",
    "END_SPACE",
    "SUPERSCRIPT_X",
    "SUPERSCRIPT_Y",
    "SUBSCRIPT_X",
    "SUBSCRIPT_Y",
    "UNDERLINE_POSITION",
    "UNDERLINE_THICKNESS",
    "STRIKEOUT_ASCENT",
    "STRIKEOUT_DESCENT",
    "ITALIC_ANGLE",
    "X_HEIGHT",
    "QUAD_WIDTH",
    "WEIGHT",
    "POINT_SIZE",
    "RESOLUTION",
    "COPYRIGHT",
    "NOTICE",
    "FONT_NAME",
    "FAMILY_NAME",
    "FULL_NAME",
    "CAP_HEIGHT",
    "WM_CLASS",
    "WM_TRANSIENT_FOR",
];

pub struct AtomTable {
    names: Vec<Vec<u8>>,
    ids: HashMap<Vec<u8>, u32>,
}

impl AtomTable {
    pub fn new() -> Self {
        let mut table = Self {
            names: Vec::new(),
            ids: HashMap::new(),
        };
        for name in PREDEFINED {
            table.intern(name.as_bytes());
        }
        table
    }

    pub fn lookup(&self, name: &[u8]) -> Option<u32> {
        self.ids.get(name).copied()
    }

    /// Returns `None` when the table is full.
    pub fn intern(&mut self, name: &[u8]) -> Option<u32> {
        if let Some(id) = self.lookup(name) {
            return Some(id);
        }
        if self.names.len() >= MAX_ATOMS {
            return None;
        }
        self.names.push(name.to_vec());
        let id = self.names.len() as u32;
        self.ids.insert(name.to_vec(), id);
        Some(id)
    }

    pub fn name(&self, id: u32) -> Option<&[u8]> {
        let index = usize::try_from(id).ok()?.checked_sub(1)?;
        self.names.get(index).map(Vec::as_slice)
    }

    pub fn contains(&self, id: u32) -> bool {
        self.name(id).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predefined_atoms_have_protocol_ids() {
        let table = AtomTable::new();
        assert_eq!(table.lookup(b"PRIMARY"), Some(1));
        assert_eq!(table.lookup(b"STRING"), Some(31));
        assert_eq!(table.lookup(b"WM_NAME"), Some(39));
        assert_eq!(table.lookup(b"WM_TRANSIENT_FOR"), Some(68));
    }

    #[test]
    fn new_atoms_follow_the_predefined_ones() {
        let mut table = AtomTable::new();
        assert_eq!(table.intern(b"MINE"), Some(69));
        assert_eq!(table.intern(b"MINE"), Some(69));
        assert_eq!(table.name(69), Some(&b"MINE"[..]));
        assert!(!table.contains(0));
        assert!(!table.contains(70));
    }

    #[test]
    fn table_has_a_limit() {
        let mut table = AtomTable::new();
        let mut n = 0u32;
        while table.intern(format!("A{n}").as_bytes()).is_some() {
            n += 1;
        }
        assert_eq!(table.names.len(), MAX_ATOMS);
    }
}
