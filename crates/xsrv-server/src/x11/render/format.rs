// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! The picture formats: a1, a8, x8r8g8b8 and a8r8g8b8, one for each pixmap depth. A pixel in a
//! surface is a little-endian u32 (see `Surface`); compositing works on premultiplied ARGB.
use crate::x11::resources::PIXEL_MASK;

/// A premultiplied pixel, in the order of the bytes in a surface: blue, green, red, alpha.
pub type Argb = [u8; 4];
pub const ALPHA: usize = 3;
pub const TRANSPARENT: Argb = [0; 4];

/// Ids of the formats. They are server ids, outside the id range of every client.
const FIRST_FORMAT_ID: u32 = 0x30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    A1,
    A8,
    Rgb24,
    Argb32,
}

pub const FORMATS: [Format; 4] = [Format::A1, Format::A8, Format::Rgb24, Format::Argb32];

impl Format {
    pub fn id(self) -> u32 {
        FIRST_FORMAT_ID + self as u32
    }

    pub fn from_id(id: u32) -> Option<Self> {
        FORMATS.into_iter().find(|format| format.id() == id)
    }

    pub fn depth(self) -> u8 {
        match self {
            Self::A1 => 1,
            Self::A8 => 8,
            Self::Rgb24 => 24,
            Self::Argb32 => 32,
        }
    }

    /// True when the format has colour channels. A mask of such a format has component alpha.
    pub fn has_colour(self) -> bool {
        matches!(self, Self::Rgb24 | Self::Argb32)
    }

    /// Shift and mask of red, green, blue and alpha, as QueryPictFormats reports them.
    pub fn channels(self) -> [(u16, u16); 4] {
        match self {
            Self::A1 => [(0, 0), (0, 0), (0, 0), (0, 1)],
            Self::A8 => [(0, 0), (0, 0), (0, 0), (0, 0xFF)],
            Self::Rgb24 => [(16, 0xFF), (8, 0xFF), (0, 0xFF), (0, 0)],
            Self::Argb32 => [(16, 0xFF), (8, 0xFF), (0, 0xFF), (24, 0xFF)],
        }
    }

    /// The pixel that a stored value of this format means.
    pub fn fetch(self, value: u32) -> Argb {
        match self {
            Self::A1 => [0, 0, 0, if value != 0 { 0xFF } else { 0 }],
            Self::A8 => [0, 0, 0, value as u8],
            Self::Rgb24 => {
                let [b, g, r, _] = value.to_le_bytes();
                [b, g, r, 0xFF]
            }
            Self::Argb32 => value.to_le_bytes(),
        }
    }

    /// The stored value of a pixel. Formats without a channel drop it; a1 keeps alpha of at least
    /// one half, as a depth-1 pixel (0 or all ones).
    pub fn store(self, pixel: Argb) -> u32 {
        match self {
            Self::A1 if pixel[ALPHA] >= 0x80 => PIXEL_MASK,
            Self::A1 => 0,
            Self::A8 => u32::from(pixel[ALPHA]),
            Self::Rgb24 => u32::from_le_bytes([pixel[0], pixel[1], pixel[2], 0]),
            Self::Argb32 => u32::from_le_bytes(pixel),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_format_reads_back_what_it_stores() {
        let pixel = [0x10, 0x20, 0x30, 0x80];
        assert_eq!(Format::Argb32.fetch(Format::Argb32.store(pixel)), pixel);
        assert_eq!(
            Format::Rgb24.fetch(Format::Rgb24.store(pixel)),
            [0x10, 0x20, 0x30, 0xFF]
        );
        assert_eq!(Format::A8.fetch(Format::A8.store(pixel)), [0, 0, 0, 0x80]);
        assert_eq!(Format::A1.fetch(Format::A1.store(pixel)), [0, 0, 0, 0xFF]);
        assert_eq!(Format::A1.store([0, 0, 0, 0x7F]), 0);
    }

    #[test]
    fn ids_map_back_to_formats() {
        for format in FORMATS {
            assert_eq!(Format::from_id(format.id()), Some(format));
        }
        assert_eq!(Format::from_id(0), None);
    }
}
