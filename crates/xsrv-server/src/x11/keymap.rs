// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

/// X11 keycodes are Linux evdev codes plus this offset. The page sends X11 keycodes.
pub const KEYCODE_OFFSET: u8 = 8;
pub const MIN_KEYCODE: u8 = 8;
pub const MAX_KEYCODE: u8 = 255;
pub const SHIFT_MASK: u16 = 0x01;
pub const LOCK_MASK: u16 = 0x02;
pub const CONTROL_MASK: u16 = 0x04;
pub const MOD1_MASK: u16 = 0x08;
pub const MOD2_MASK: u16 = 0x10;
pub const MOD4_MASK: u16 = 0x40;
pub const MOD5_MASK: u16 = 0x80;

const CAPS_LOCK: u8 = 58 + KEYCODE_OFFSET;
const NUM_LOCK: u8 = 69 + KEYCODE_OFFSET;

/// The keyboard layout. The page sends the physical key, so the server decides what it types.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Layout {
    #[default]
    Us,
    /// Spanish (ISO). The right Alt key is AltGr, which gives the third and fourth symbol.
    Es,
    /// British (ISO). AltGr is `Mode_switch`, as in the Spanish layout.
    Gb,
    /// German (ISO). Same AltGr.
    De,
}

impl Layout {
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "us" => Some(Self::Us),
            "es" => Some(Self::Es),
            "gb" | "uk" => Some(Self::Gb),
            "de" => Some(Self::De),
            _ => None,
        }
    }

    /// The layout at `index` of `wire::LAYOUTS`, the numbering that the page uses.
    pub fn from_index(index: u8) -> Option<Self> {
        wire::LAYOUTS
            .get(usize::from(index))
            .and_then(|name| Self::from_name(name))
    }

    pub fn index(self) -> u8 {
        let name = match self {
            Self::Us => "us",
            Self::Es => "es",
            Self::Gb => "gb",
            Self::De => "de",
        };
        wire::LAYOUTS
            .iter()
            .position(|known| *known == name)
            .expect("every layout is in wire::LAYOUTS") as u8
    }

    pub fn keysyms_per_keycode(self) -> u8 {
        match self {
            Self::Us => 2,
            Self::Es | Self::Gb | Self::De => 4,
        }
    }
}

/// Xlib without XKB finds the second group of a key only through this keysym on a modifier.
const MODE_SWITCH: u32 = 0xFF7E;

/// Keycodes of the modifiers, in the order Shift, Lock, Control, Mod1 to Mod5.
/// Two slots for each modifier. 0 means no key.
fn modifier_keys(layout: Layout) -> [[u8; 2]; 8] {
    let alt_right = match layout {
        Layout::Us => 100 + KEYCODE_OFFSET,
        Layout::Es | Layout::Gb | Layout::De => 0,
    };
    let altgr = match layout {
        Layout::Us => 0,
        Layout::Es | Layout::Gb | Layout::De => 100 + KEYCODE_OFFSET,
    };
    [
        [42 + KEYCODE_OFFSET, 54 + KEYCODE_OFFSET],
        [CAPS_LOCK, 0],
        [29 + KEYCODE_OFFSET, 97 + KEYCODE_OFFSET],
        [56 + KEYCODE_OFFSET, alt_right],
        [NUM_LOCK, 0],
        [0, 0],
        [125 + KEYCODE_OFFSET, 126 + KEYCODE_OFFSET],
        [altgr, 0],
    ]
}

pub const KEYCODES_PER_MODIFIER: u8 = 2;

pub fn modifier_mapping(layout: Layout) -> [u8; 16] {
    let mut out = [0; 16];
    for (slot, key) in modifier_keys(layout).iter().flatten().enumerate() {
        out[slot] = *key;
    }
    out
}

/// The state bit that a held key sets. Caps Lock and Num Lock are locks and are handled apart.
pub fn held_modifier(layout: Layout, keycode: u8) -> Option<u16> {
    let keys = modifier_keys(layout);
    [
        (0, SHIFT_MASK),
        (2, CONTROL_MASK),
        (3, MOD1_MASK),
        (6, MOD4_MASK),
        (7, MOD5_MASK),
    ]
    .into_iter()
    .find(|(row, _)| keys[*row].contains(&keycode))
    .map(|(_, mask)| mask)
}

pub fn modifier_keycodes(layout: Layout) -> impl Iterator<Item = u8> {
    modifier_keys(layout)
        .into_iter()
        .flatten()
        .filter(|key| *key != 0)
}

const fn ascii(unshifted: char, shifted: char) -> (u32, u32) {
    (unshifted as u32, shifted as u32)
}

const fn same(keysym: u32) -> (u32, u32) {
    (keysym, keysym)
}

/// (evdev code, keysym without Shift, keysym with Shift) for a US keyboard.
const US_LAYOUT: &[(u8, (u32, u32))] = &[
    (1, same(0xFF1B)),
    (2, ascii('1', '!')),
    (3, ascii('2', '@')),
    (4, ascii('3', '#')),
    (5, ascii('4', '$')),
    (6, ascii('5', '%')),
    (7, ascii('6', '^')),
    (8, ascii('7', '&')),
    (9, ascii('8', '*')),
    (10, ascii('9', '(')),
    (11, ascii('0', ')')),
    (12, ascii('-', '_')),
    (13, ascii('=', '+')),
    (14, same(0xFF08)),
    (15, (0xFF09, 0xFE20)),
    (16, ascii('q', 'Q')),
    (17, ascii('w', 'W')),
    (18, ascii('e', 'E')),
    (19, ascii('r', 'R')),
    (20, ascii('t', 'T')),
    (21, ascii('y', 'Y')),
    (22, ascii('u', 'U')),
    (23, ascii('i', 'I')),
    (24, ascii('o', 'O')),
    (25, ascii('p', 'P')),
    (26, ascii('[', '{')),
    (27, ascii(']', '}')),
    (28, same(0xFF0D)),
    (29, same(0xFFE3)),
    (30, ascii('a', 'A')),
    (31, ascii('s', 'S')),
    (32, ascii('d', 'D')),
    (33, ascii('f', 'F')),
    (34, ascii('g', 'G')),
    (35, ascii('h', 'H')),
    (36, ascii('j', 'J')),
    (37, ascii('k', 'K')),
    (38, ascii('l', 'L')),
    (39, ascii(';', ':')),
    (40, ascii('\'', '"')),
    (41, ascii('`', '~')),
    (42, same(0xFFE1)),
    (43, ascii('\\', '|')),
    (44, ascii('z', 'Z')),
    (45, ascii('x', 'X')),
    (46, ascii('c', 'C')),
    (47, ascii('v', 'V')),
    (48, ascii('b', 'B')),
    (49, ascii('n', 'N')),
    (50, ascii('m', 'M')),
    (51, ascii(',', '<')),
    (52, ascii('.', '>')),
    (53, ascii('/', '?')),
    (54, same(0xFFE2)),
    (55, same(0xFFAA)),
    (56, same(0xFFE9)),
    (57, ascii(' ', ' ')),
    (58, same(0xFFE5)),
    (59, same(0xFFBE)),
    (60, same(0xFFBF)),
    (61, same(0xFFC0)),
    (62, same(0xFFC1)),
    (63, same(0xFFC2)),
    (64, same(0xFFC3)),
    (65, same(0xFFC4)),
    (66, same(0xFFC5)),
    (67, same(0xFFC6)),
    (68, same(0xFFC7)),
    (69, same(0xFF7F)),
    (70, same(0xFF14)),
    (71, (0xFF95, 0xFFB7)),
    (72, (0xFF97, 0xFFB8)),
    (73, (0xFF9A, 0xFFB9)),
    (74, same(0xFFAD)),
    (75, (0xFF96, 0xFFB4)),
    (76, (0xFF9D, 0xFFB5)),
    (77, (0xFF98, 0xFFB6)),
    (78, same(0xFFAB)),
    (79, (0xFF9C, 0xFFB1)),
    (80, (0xFF99, 0xFFB2)),
    (81, (0xFF9B, 0xFFB3)),
    (82, (0xFF9E, 0xFFB0)),
    (83, (0xFF9F, 0xFFAE)),
    (86, ascii('<', '>')),
    (87, same(0xFFC8)),
    (88, same(0xFFC9)),
    (96, same(0xFF8D)),
    (97, same(0xFFE4)),
    (98, same(0xFFAF)),
    (99, same(0xFF61)),
    (100, same(0xFFEA)),
    (102, same(0xFF50)),
    (103, same(0xFF52)),
    (104, same(0xFF55)),
    (105, same(0xFF51)),
    (106, same(0xFF53)),
    (107, same(0xFF57)),
    (108, same(0xFF54)),
    (109, same(0xFF56)),
    (110, same(0xFF63)),
    (111, same(0xFFFF)),
    (117, same(0xFFBD)),
    (119, same(0xFF13)),
    (125, same(0xFFEB)),
    (126, same(0xFFEC)),
    (127, same(0xFF67)),
    (183, same(0xFFCA)),
    (184, same(0xFFCB)),
    (185, same(0xFFCC)),
    (186, same(0xFFCD)),
    (187, same(0xFFCE)),
    (188, same(0xFFCF)),
    (189, same(0xFFD0)),
    (190, same(0xFFD1)),
    (191, same(0xFFD2)),
    (192, same(0xFFD3)),
    (193, same(0xFFD4)),
    (194, same(0xFFD5)),
];

/// Where the Spanish layout differs from the US one: (evdev code, [plain, Shift, AltGr, AltGr+Shift]).
const ES_LAYOUT: &[(u8, [u32; 4])] = &[
    (2, [0x31, 0x21, 0x7C, 0]),
    (3, [0x32, 0x22, 0x40, 0]),
    (4, [0x33, 0xB7, 0x23, 0]),
    (5, [0x34, 0x24, 0x7E, 0]),
    (6, [0x35, 0x25, 0, 0]),
    (7, [0x36, 0x26, 0xAC, 0]),
    (8, [0x37, 0x2F, 0, 0]),
    (9, [0x38, 0x28, 0, 0]),
    (10, [0x39, 0x29, 0, 0]),
    (11, [0x30, 0x3D, 0, 0]),
    (12, [0x27, 0x3F, 0, 0]),
    (13, [0xA1, 0xBF, 0, 0]),
    (18, [0x65, 0x45, 0x20AC, 0]),
    (26, [0xFE50, 0xFE52, 0x5B, 0]),
    (27, [0x2B, 0x2A, 0x5D, 0]),
    (39, [0xF1, 0xD1, 0, 0]),
    (40, [0xFE51, 0xFE57, 0x7B, 0]),
    (41, [0xBA, 0xAA, 0x5C, 0]),
    (43, [0xE7, 0xC7, 0x7D, 0]),
    (51, [0x2C, 0x3B, 0, 0]),
    (52, [0x2E, 0x3A, 0, 0]),
    (53, [0x2D, 0x5F, 0, 0]),
    (100, [MODE_SWITCH, 0, 0, 0]),
];

/// British: (evdev code, [plain, Shift, AltGr, AltGr+Shift]).
const GB_LAYOUT: &[(u8, [u32; 4])] = &[
    (3, [0x32, 0x22, 0, 0]),
    (4, [0x33, 0xA3, 0, 0]),
    (5, [0x34, 0x24, 0x20AC, 0]),
    (18, [0x65, 0x45, 0x20AC, 0]),
    (40, [0x27, 0x40, 0, 0]),
    (41, [0x60, 0xAC, 0x7C, 0]),
    (43, [0x23, 0x7E, 0, 0]),
    (86, [0x5C, 0x7C, 0, 0]),
    (100, [MODE_SWITCH, 0, 0, 0]),
];

/// German: (evdev code, [plain, Shift, AltGr, AltGr+Shift]).
const DE_LAYOUT: &[(u8, [u32; 4])] = &[
    (2, [0x31, 0x21, 0, 0]),
    (3, [0x32, 0x22, 0xB2, 0]),
    (4, [0x33, 0xA7, 0xB3, 0]),
    (5, [0x34, 0x24, 0, 0]),
    (6, [0x35, 0x25, 0, 0]),
    (7, [0x36, 0x26, 0, 0]),
    (8, [0x37, 0x2F, 0x7B, 0]),
    (9, [0x38, 0x28, 0x5B, 0]),
    (10, [0x39, 0x29, 0x5D, 0]),
    (11, [0x30, 0x3D, 0x7D, 0]),
    (12, [0xDF, 0x3F, 0x5C, 0]),
    (13, [0xFE51, 0xFE50, 0, 0]),
    (16, [0x71, 0x51, 0x40, 0]),
    (18, [0x65, 0x45, 0x20AC, 0]),
    (21, [0x7A, 0x5A, 0, 0]),
    (26, [0xFC, 0xDC, 0, 0]),
    (27, [0x2B, 0x2A, 0x7E, 0]),
    (39, [0xF6, 0xD6, 0, 0]),
    (40, [0xE4, 0xC4, 0, 0]),
    (41, [0xFE52, 0xB0, 0, 0]),
    (43, [0x23, 0x27, 0, 0]),
    (44, [0x79, 0x59, 0, 0]),
    (50, [0x6D, 0x4D, 0xB5, 0]),
    (51, [0x2C, 0x3B, 0, 0]),
    (52, [0x2E, 0x3A, 0, 0]),
    (53, [0x2D, 0x5F, 0, 0]),
    (86, [0x3C, 0x3E, 0x7C, 0]),
    (100, [MODE_SWITCH, 0, 0, 0]),
];

/// The keys that differ from the US layout, or none for the US layout.
fn overrides(layout: Layout) -> &'static [(u8, [u32; 4])] {
    match layout {
        Layout::Us => &[],
        Layout::Es => ES_LAYOUT,
        Layout::Gb => GB_LAYOUT,
        Layout::De => DE_LAYOUT,
    }
}

fn us_keysyms(evdev: u8) -> [u32; 2] {
    US_LAYOUT
        .iter()
        .find(|(code, _)| *code == evdev)
        .map_or([0, 0], |(_, (plain, shifted))| [*plain, *shifted])
}

/// The keysyms of a keycode: without Shift, with Shift, and for AltGr (only the Spanish layout
/// has them). 0 (NoSymbol) when the key does not exist. The reply uses the first
/// `layout.keysyms_per_keycode()` of them.
pub fn keysyms(layout: Layout, keycode: u8) -> [u32; 4] {
    let Some(evdev) = keycode.checked_sub(KEYCODE_OFFSET) else {
        return [0; 4];
    };
    if let Some((_, symbols)) = overrides(layout).iter().find(|(code, _)| *code == evdev) {
        return *symbols;
    }
    let [plain, shifted] = us_keysyms(evdev);
    [plain, shifted, 0, 0]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letter_keys_have_both_cases() {
        assert_eq!(keysyms(Layout::Us, 30 + KEYCODE_OFFSET), [0x61, 0x41, 0, 0]);
        assert_eq!(keysyms(Layout::Us, 2 + KEYCODE_OFFSET), [0x31, 0x21, 0, 0]);
    }

    #[test]
    fn unknown_keys_have_no_symbol() {
        assert_eq!(keysyms(Layout::Us, 8), [0; 4]);
        assert_eq!(keysyms(Layout::Us, 7), [0; 4]);
        assert_eq!(keysyms(Layout::Us, 250), [0; 4]);
    }

    #[test]
    fn layout_is_sorted_and_unique() {
        assert!(US_LAYOUT.windows(2).all(|pair| pair[0].0 < pair[1].0));
    }

    #[test]
    fn every_modifier_key_has_a_symbol() {
        for layout in [Layout::Us, Layout::Es, Layout::Gb, Layout::De] {
            for keycode in modifier_keycodes(layout) {
                assert_ne!(
                    keysyms(layout, keycode)[0],
                    0,
                    "{layout:?} keycode {keycode}"
                );
            }
        }
    }

    #[test]
    fn modifier_bits() {
        assert_eq!(held_modifier(Layout::Us, 50), Some(SHIFT_MASK));
        assert_eq!(held_modifier(Layout::Us, 105), Some(CONTROL_MASK));
        assert_eq!(held_modifier(Layout::Us, 66), None);
        assert_eq!(modifier_mapping(Layout::Us)[0], 50);
        assert_eq!(held_modifier(Layout::Us, 108), Some(MOD1_MASK));
        assert_eq!(held_modifier(Layout::Es, 108), Some(MOD5_MASK), "AltGr");
    }

    #[test]
    fn the_spanish_layout_types_n_tilde_and_altgr_symbols() {
        let at = |evdev: u8| keysyms(Layout::Es, evdev + KEYCODE_OFFSET);
        assert_eq!(at(39)[..2], [0xF1, 0xD1], "n with tilde");
        assert_eq!(at(3), [0x32, 0x22, 0x40, 0], "2, double quote, at sign");
        assert_eq!(at(30)[..2], [0x61, 0x41], "letters are as in the US layout");
        assert_eq!(Layout::Es.keysyms_per_keycode(), 4);
        assert_eq!(Layout::from_name("ES"), Some(Layout::Es));
        assert_eq!(Layout::from_name("fr"), None);
    }

    #[test]
    fn the_tables_are_sorted_and_unique() {
        for layout in [Layout::Es, Layout::Gb, Layout::De] {
            assert!(
                overrides(layout)
                    .windows(2)
                    .all(|pair| pair[0].0 < pair[1].0),
                "{layout:?}"
            );
        }
    }

    #[test]
    fn british_and_german_layouts_differ_where_they_should() {
        let at = |layout, evdev: u8| keysyms(layout, evdev + KEYCODE_OFFSET);
        assert_eq!(at(Layout::Gb, 4)[..2], [0x33, 0xA3], "3 and pound");
        assert_eq!(at(Layout::Gb, 40)[..2], [0x27, 0x40], "apostrophe and at");
        assert_eq!(
            at(Layout::De, 21)[..2],
            [0x7A, 0x5A],
            "z is where y is on a US keyboard"
        );
        assert_eq!(at(Layout::De, 44)[..2], [0x79, 0x59]);
        assert_eq!(at(Layout::De, 26)[..2], [0xFC, 0xDC], "u with diaeresis");
        assert_eq!(at(Layout::De, 16)[2], 0x40, "AltGr+Q is at");
        assert_eq!(Layout::from_name("uk"), Some(Layout::Gb));
    }
}
