// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

/// A color with 16 bits per channel, as the X protocol sends it.
pub type Rgb16 = (u16, u16, u16);

/// A subset of the X11 `rgb.txt` names, with 8-bit channels.
const NAMES: &[(&str, [u8; 3])] = &[
    ("aquamarine", [127, 255, 212]),
    ("beige", [245, 245, 220]),
    ("black", [0, 0, 0]),
    ("blue", [0, 0, 255]),
    ("brown", [165, 42, 42]),
    ("chocolate", [210, 105, 30]),
    ("coral", [255, 127, 80]),
    ("cornflowerblue", [100, 149, 237]),
    ("cyan", [0, 255, 255]),
    ("darkblue", [0, 0, 139]),
    ("darkgray", [169, 169, 169]),
    ("darkgreen", [0, 100, 0]),
    ("darkgrey", [169, 169, 169]),
    ("darkorange", [255, 140, 0]),
    ("darkred", [139, 0, 0]),
    ("firebrick", [178, 34, 34]),
    ("forestgreen", [34, 139, 34]),
    ("gainsboro", [220, 220, 220]),
    ("gold", [255, 215, 0]),
    ("gray", [190, 190, 190]),
    ("green", [0, 255, 0]),
    ("grey", [190, 190, 190]),
    ("ivory", [255, 255, 240]),
    ("khaki", [240, 230, 140]),
    ("lavender", [230, 230, 250]),
    ("lightblue", [173, 216, 230]),
    ("lightgray", [211, 211, 211]),
    ("lightgreen", [144, 238, 144]),
    ("lightgrey", [211, 211, 211]),
    ("lightpink", [255, 182, 193]),
    ("lightsteelblue", [176, 196, 222]),
    ("lightyellow", [255, 255, 224]),
    ("linen", [250, 240, 230]),
    ("magenta", [255, 0, 255]),
    ("maroon", [176, 48, 96]),
    ("navy", [0, 0, 128]),
    ("navyblue", [0, 0, 128]),
    ("orange", [255, 165, 0]),
    ("orchid", [218, 112, 214]),
    ("pink", [255, 192, 203]),
    ("plum", [221, 160, 221]),
    ("purple", [160, 32, 240]),
    ("red", [255, 0, 0]),
    ("royalblue", [65, 105, 225]),
    ("salmon", [250, 128, 114]),
    ("seagreen", [46, 139, 87]),
    ("skyblue", [135, 206, 235]),
    ("slateblue", [106, 90, 205]),
    ("slategray", [112, 128, 144]),
    ("slategrey", [112, 128, 144]),
    ("snow", [255, 250, 250]),
    ("steelblue", [70, 130, 180]),
    ("tan", [210, 180, 140]),
    ("turquoise", [64, 224, 208]),
    ("violet", [238, 130, 238]),
    ("wheat", [245, 222, 179]),
    ("white", [255, 255, 255]),
    ("whitesmoke", [245, 245, 245]),
    ("yellow", [255, 255, 0]),
];

fn widen(channel: u8) -> u16 {
    u16::from(channel) * 257
}

/// Scales a hex number with `digits` digits to the 16-bit range.
fn scale_hex(text: &str) -> Option<u16> {
    if text.is_empty() || text.len() > 4 {
        return None;
    }
    let value = u32::from_str_radix(text, 16).ok()?;
    let max = (1u32 << (4 * text.len())) - 1;
    Some((value * 0xFFFF / max) as u16)
}

fn parse_rgb_prefix(spec: &str) -> Option<Rgb16> {
    let mut parts = spec.split('/');
    let color = (
        scale_hex(parts.next()?)?,
        scale_hex(parts.next()?)?,
        scale_hex(parts.next()?)?,
    );
    parts.next().is_none().then_some(color)
}

fn parse_gray_level(name: &str) -> Option<Rgb16> {
    let level: u32 = name
        .strip_prefix("gray")
        .or_else(|| name.strip_prefix("grey"))?
        .parse()
        .ok()
        .filter(|level| *level <= 100)?;
    let value = widen(((level * 255 + 50) / 100) as u8);
    Some((value, value, value))
}

/// Understands `rgb:r/g/b` and the names above. Spaces and case do not matter.
/// The `#rgb` forms are parsed by Xlib in the client. Xorg answers BadName for them, and so do we.
pub fn parse_color(name: &[u8]) -> Option<Rgb16> {
    let text = std::str::from_utf8(name).ok()?.trim();
    let lower: String = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    if let Some(spec) = lower.strip_prefix("rgb:") {
        return parse_rgb_prefix(spec);
    }
    if let Some(color) = parse_gray_level(&lower) {
        return Some(color);
    }
    let (_, [r, g, b]) = NAMES.iter().find(|(known, _)| *known == lower)?;
    Some((widen(*r), widen(*g), widen(*b)))
}

/// The TrueColor pixel for a color, keeping the top 8 bits of each channel.
pub fn pixel_for((red, green, blue): Rgb16) -> u32 {
    u32::from(red >> 8) << 16 | u32::from(green >> 8) << 8 | u32::from(blue >> 8)
}

/// The color that a TrueColor pixel really shows.
pub fn color_of(pixel: u32) -> Rgb16 {
    let channel = |shift: u32| widen((pixel >> shift) as u8);
    (channel(16), channel(8), channel(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_colors_ignore_case_and_spaces() {
        assert_eq!(parse_color(b"Red"), Some((0xFFFF, 0, 0)));
        assert_eq!(parse_color(b"light gray"), parse_color(b"LightGrey"));
        assert_eq!(parse_color(b"nosuchcolor"), None);
    }

    #[test]
    fn hash_forms_are_left_to_the_client() {
        assert_eq!(parse_color(b"#fff"), None);
        assert_eq!(parse_color(b"#ff0000"), None);
    }

    #[test]
    fn rgb_prefix_form() {
        assert_eq!(parse_color(b"rgb:ff/00/ff"), Some((0xFFFF, 0, 0xFFFF)));
        assert_eq!(parse_color(b"rgb:f/0/f"), Some((0xFFFF, 0, 0xFFFF)));
        assert_eq!(parse_color(b"rgb:ff/00"), None);
        assert_eq!(parse_color(b"rgb:ff/00/ff/00"), None);
    }

    #[test]
    fn gray_levels() {
        assert_eq!(parse_color(b"gray0"), Some((0, 0, 0)));
        assert_eq!(parse_color(b"grey100"), Some((0xFFFF, 0xFFFF, 0xFFFF)));
        assert_eq!(parse_color(b"gray101"), None);
    }

    #[test]
    fn pixel_round_trip_keeps_8_bits() {
        let pixel = pixel_for((0x1234, 0x5678, 0x9ABC));
        assert_eq!(pixel, 0x12_56_9A);
        assert_eq!(color_of(pixel), (0x1212, 0x5656, 0x9A9A));
    }

    #[test]
    fn names_are_sorted_and_unique() {
        assert!(NAMES.windows(2).all(|pair| pair[0].0 < pair[1].0));
    }
}
