// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

use font8x8::legacy::BASIC_LEGACY;

pub const GLYPH_WIDTH: i32 = 8;
pub const GLYPH_HEIGHT: usize = 16;
pub const ASCENT: i16 = 13;
pub const DESCENT: i16 = 3;
pub const LAST_CHAR: u16 = 255;
pub const DEFAULT_CHAR: u16 = b' ' as u16;

const FONT_NAME: &str = "-xsrv-fixed-medium-r-normal--16-160-75-75-c-80-iso8859-1";
const ALIASES: [&str; 3] = ["fixed", "8x16", "cursor"];

/// One row per byte. The bit for the leftmost pixel is bit 0.
/// The 8x8 source glyphs are stretched to 16 rows. Codes above 127 are blank.
pub fn glyph(code: u16) -> [u8; GLYPH_HEIGHT] {
    let rows = BASIC_LEGACY
        .get(usize::from(code))
        .copied()
        .unwrap_or([0; 8]);
    std::array::from_fn(|row| rows[row / 2])
}

/// Only the ASCII range has glyphs. The font claims iso8859-1 so that clients accept it.
pub fn has_glyph(code: u16) -> bool {
    usize::from(code) < BASIC_LEGACY.len()
}

/// The value of a font property: the name of an atom, or a number.
#[derive(Debug, PartialEq)]
pub enum PropertyValue {
    Atom(&'static str),
    Card(u32),
}

/// The XLFD properties of the font, taken from its name, as the X font server gives them.
/// Clients read them: urxvt takes the charset from `FONT` and stops without it.
pub fn properties() -> Vec<(&'static str, PropertyValue)> {
    const FIELDS: [(&str, bool); 14] = [
        ("FOUNDRY", false),
        ("FAMILY_NAME", false),
        ("WEIGHT_NAME", false),
        ("SLANT", false),
        ("SETWIDTH_NAME", false),
        ("ADD_STYLE_NAME", false),
        ("PIXEL_SIZE", true),
        ("POINT_SIZE", true),
        ("RESOLUTION_X", true),
        ("RESOLUTION_Y", true),
        ("SPACING", false),
        ("AVERAGE_WIDTH", true),
        ("CHARSET_REGISTRY", false),
        ("CHARSET_ENCODING", false),
    ];
    let values = FONT_NAME.split('-').skip(1);
    let mut list: Vec<_> = FIELDS
        .into_iter()
        .zip(values)
        .filter(|(_, value)| !value.is_empty())
        .map(|((name, number), value)| match value.parse() {
            Ok(number_value) if number => (name, PropertyValue::Card(number_value)),
            _ => (name, PropertyValue::Atom(value)),
        })
        .collect();
    list.push(("FONT", PropertyValue::Atom(FONT_NAME)));
    list.push(("FONT_ASCENT", PropertyValue::Card(ASCENT as u32)));
    list.push(("FONT_DESCENT", PropertyValue::Card(DESCENT as u32)));
    list.push(("DEFAULT_CHAR", PropertyValue::Card(u32::from(DEFAULT_CHAR))));
    list
}

fn names() -> impl Iterator<Item = &'static str> {
    ALIASES.into_iter().chain([FONT_NAME])
}

/// The built-in font answers to its own names, and it stands in for the `misc-fixed` family
/// that terminals ask for by default. Other fonts do not exist.
pub fn font_exists(pattern: &[u8]) -> bool {
    let pattern = ignore_sizes(pattern);
    names().any(|name| glob_match(&pattern, name.as_bytes()))
        || glob_match(b"-misc-fixed-*", &pattern.to_ascii_lowercase())
}

/// There is one size, so the size fields of an XLFD pattern (pixels, points, the two resolutions
/// and the average width) match anything. A pattern that is not an XLFD stays as it is.
fn ignore_sizes(pattern: &[u8]) -> Vec<u8> {
    const SIZE_FIELDS: [usize; 5] = [7, 8, 9, 10, 12];
    const XLFD_FIELDS: usize = 15;
    let fields: Vec<&[u8]> = pattern.split(|&byte| byte == b'-').collect();
    if fields.len() != XLFD_FIELDS || !pattern.starts_with(b"-") {
        return pattern.to_vec();
    }
    let relaxed: Vec<&[u8]> = fields
        .iter()
        .enumerate()
        .map(|(index, field)| {
            if SIZE_FIELDS.contains(&index) {
                &b"*"[..]
            } else {
                field
            }
        })
        .collect();
    relaxed.join(&b'-')
}

pub fn matching_names(pattern: &[u8], max: usize) -> Vec<&'static str> {
    names()
        .filter(|name| glob_match(pattern, name.as_bytes()))
        .take(max)
        .collect()
}

/// Case-insensitive match with `*` and `?`, as font name patterns use.
fn glob_match(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0, 0);
    let mut restart: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some(b'*') => {
                restart = Some((p, t));
                p += 1;
            }
            Some(&c) if c == b'?' || c.eq_ignore_ascii_case(&text[t]) => {
                p += 1;
                t += 1;
            }
            _ => match restart {
                Some((star, matched)) => {
                    restart = Some((star, matched + 1));
                    p = star + 1;
                    t = matched + 1;
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&c| c == b'*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letter_a_has_its_apex_in_columns_2_and_3() {
        let rows = glyph(u16::from(b'A'));
        assert_eq!(rows[0], 0b0000_1100);
        assert_eq!(rows[0], rows[1], "rows are doubled");
        assert_eq!(rows[6], 0b0011_0011);
    }

    #[test]
    fn codes_outside_the_table_are_blank() {
        assert_eq!(glyph(200), [0; GLYPH_HEIGHT]);
        assert!(has_glyph(127) && !has_glyph(128));
        assert_eq!(glyph(0x4142), [0; GLYPH_HEIGHT]);
    }

    #[test]
    fn patterns_match_names_without_regard_to_case() {
        assert!(font_exists(b"fixed"));
        assert!(font_exists(b"FIXED"));
        assert!(font_exists(b"-*-fixed-*"));
        assert!(font_exists(
            b"-xsrv-fixed-medium-r-normal--16-*-*-*-c-80-iso8859-1"
        ));
        assert!(font_exists(
            b"-misc-fixed-medium-r-semicondensed--13-120-75-75-c-60-iso10646-1"
        ));
        assert!(!font_exists(b"-adobe-helvetica-medium-r-normal--12-*"));
        assert!(
            font_exists(b"-*-*-*-R-*-*-*-120-*-*-*-*-ISO8859-*"),
            "any point size"
        );
        assert!(!font_exists(b"-*-*-*-R-*-*-*-120-*-*-*-*-KOI8-*"));
        assert!(!font_exists(b"nosuchfont"));
        assert!(!font_exists(b"fixe"));
        assert_eq!(matching_names(b"fix*", 10), ["fixed"]);
        assert!(font_exists(b"cursor"));
        assert_eq!(matching_names(b"*", 2).len(), 2);
    }

    #[test]
    fn long_patterns_do_not_blow_up() {
        let pattern = b"*a".repeat(2000);
        assert!(!font_exists(&pattern));
    }
}
