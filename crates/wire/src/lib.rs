#![no_std]
// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

mod clipboard;
mod cursor;
mod input;

pub use clipboard::{
    decode_clipboard_set_in, encode_part_header, encode_set_header, encode_set_in_header,
    ClipboardFormat, ClipboardParts, ClipboardSource, KIND_CLIPBOARD_SET, KIND_CLIPBOARD_SET_IN,
    MAX_CLIPBOARD_PNG, MAX_CLIPBOARD_SET, MAX_CLIPBOARD_TEXT, PART_HEADER_LEN,
};
pub use cursor::CursorShape;
pub use input::{decode_input, encode_input, Input, MAX_BUTTON, MAX_INPUT_LEN, MIN_KEYCODE};

const KIND_SCREEN: u8 = 0;
const KIND_TILE: u8 = 1;
const KIND_CURSOR: u8 = 2;
const KIND_CLIPBOARD: u8 = 3;
const KIND_TILE_ZSTD: u8 = 4;
const KIND_VIDEO: u8 = 5;
const KIND_COPY: u8 = 6;
/// The first byte of a message that tells the page the keyboard layout of its session.
pub const LAYOUT_PREFIX: u8 = 7;
/// The keyboard layouts, by the index that the layout messages carry.
pub const LAYOUTS: [&str; 4] = ["us", "es", "gb", "de"];
const KIND_TILE_LZ4: u8 = 8;
const KIND_TILE_INDEXED_LZ4: u8 = 9;
const KIND_TILE_INDEXED_ZSTD: u8 = 10;
const KIND_FILL: u8 = 11;
/// The most colours of an indexed tile: an index is one byte.
pub const MAX_PALETTE_COLOURS: usize = 256;
/// The first byte of a message that carries a piece of the H.264 video stream.
pub const VIDEO_PREFIX: u8 = KIND_VIDEO;
/// A page sends its clipboard text with this first byte (the input kinds are 16 and up).
pub const KIND_CLIPBOARD_IN: u8 = 23;
/// The most text that a text-only clipboard message carries, in bytes.
pub const MAX_CLIPBOARD_BYTES: usize = MAX_CLIPBOARD_TEXT;
pub const BYTES_PER_PIXEL: usize = 4;

pub const SCREEN_LEN: usize = 5;
pub const TILE_HEADER_LEN: usize = 9;
pub const CURSOR_LEN: usize = 2;
pub const COPY_LEN: usize = 13;
pub const FILL_LEN: usize = 13;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl Rect {
    /// Computed in `u32` so the result is the same on 32-bit and 64-bit targets.
    pub fn pixel_bytes(&self) -> Option<usize> {
        let pixels = u32::from(self.w) * u32::from(self.h);
        let bytes = pixels.checked_mul(BYTES_PER_PIXEL as u32)?;
        usize::try_from(bytes).ok()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Message<'a> {
    Screen {
        width: u16,
        height: u16,
    },
    Tile {
        rect: Rect,
        pixels: &'a [u8],
    },
    /// The same as `Tile`, but the pixels are one zstd frame. The page decodes it.
    TileZstd {
        rect: Rect,
        frame: &'a [u8],
    },
    /// The same as `Tile`, but the pixels are one LZ4 block. The page decodes it.
    TileLz4 {
        rect: Rect,
        block: &'a [u8],
    },
    /// A tile of few colours: `palette` holds the BGRX pixel of each colour, and `indices` is
    /// one byte for each pixel of the tile, compressed with `packing`.
    TileIndexed {
        rect: Rect,
        palette: &'a [u8],
        packing: Packing,
        indices: &'a [u8],
    },
    Cursor(CursorShape),
    /// The keyboard layout of the session: an index into `LAYOUTS`.
    Layout(u8),
    /// The text of the X clipboard, for the page to put in the browser clipboard.
    Clipboard(&'a str),
    /// Text, HTML and an image from X, for the page to put in the browser clipboard.
    ClipboardSet {
        source: ClipboardSource,
        parts: ClipboardParts<'a>,
    },
    /// A piece of an H.264 stream in Annex B format. The page decodes it with WebCodecs.
    Video(&'a [u8]),
    /// Moves the pixels of `from` on the screen so that its top-left corner is at `to`.
    Copy {
        from: Rect,
        to: (u16, u16),
    },
    /// Every pixel of `rect` has the colour `pixel` (BGRX): a solid background costs 13 bytes
    /// instead of a tile.
    Fill {
        rect: Rect,
        pixel: [u8; BYTES_PER_PIXEL],
    },
}

/// How the indices of an indexed tile are compressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packing {
    Lz4,
    Zstd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    Empty,
    UnknownKind(u8),
    Truncated,
    EmptyRect,
    TooLarge,
    PixelLength,
    BadValue,
}

pub fn encode_cursor(shape: CursorShape) -> [u8; CURSOR_LEN] {
    [KIND_CURSOR, shape as u8]
}

pub fn encode_layout(layout: u8) -> [u8; 2] {
    [LAYOUT_PREFIX, layout]
}

pub fn encode_screen(width: u16, height: u16) -> [u8; SCREEN_LEN] {
    let mut out = [0; SCREEN_LEN];
    out[0] = KIND_SCREEN;
    out[1..3].copy_from_slice(&width.to_le_bytes());
    out[3..5].copy_from_slice(&height.to_le_bytes());
    out
}

pub fn encode_tile_header(rect: Rect) -> [u8; TILE_HEADER_LEN] {
    tile_header(KIND_TILE, rect)
}

/// The header of a tile whose pixels are compressed. The frame follows it.
pub fn encode_zstd_tile_header(rect: Rect) -> [u8; TILE_HEADER_LEN] {
    tile_header(KIND_TILE_ZSTD, rect)
}

/// The header of a tile whose pixels are one LZ4 block. The block follows it.
pub fn encode_lz4_tile_header(rect: Rect) -> [u8; TILE_HEADER_LEN] {
    tile_header(KIND_TILE_LZ4, rect)
}

/// The header of an indexed tile. After it come one byte with the number of colours less one,
/// the BGRX pixel of each colour, and the compressed indices.
pub fn encode_indexed_tile_header(rect: Rect, packing: Packing) -> [u8; TILE_HEADER_LEN] {
    let kind = match packing {
        Packing::Lz4 => KIND_TILE_INDEXED_LZ4,
        Packing::Zstd => KIND_TILE_INDEXED_ZSTD,
    };
    tile_header(kind, rect)
}

pub fn encode_copy(from: Rect, to: (u16, u16)) -> [u8; COPY_LEN] {
    let mut out = [0; COPY_LEN];
    out[..TILE_HEADER_LEN].copy_from_slice(&tile_header(KIND_COPY, from));
    out[9..11].copy_from_slice(&to.0.to_le_bytes());
    out[11..13].copy_from_slice(&to.1.to_le_bytes());
    out
}

pub fn encode_fill(rect: Rect, pixel: [u8; BYTES_PER_PIXEL]) -> [u8; FILL_LEN] {
    let mut out = [0; FILL_LEN];
    out[..TILE_HEADER_LEN].copy_from_slice(&tile_header(KIND_FILL, rect));
    out[TILE_HEADER_LEN..].copy_from_slice(&pixel);
    out
}

fn tile_header(kind: u8, rect: Rect) -> [u8; TILE_HEADER_LEN] {
    let mut out = [0; TILE_HEADER_LEN];
    out[0] = kind;
    out[1..3].copy_from_slice(&rect.x.to_le_bytes());
    out[3..5].copy_from_slice(&rect.y.to_le_bytes());
    out[5..7].copy_from_slice(&rect.w.to_le_bytes());
    out[7..9].copy_from_slice(&rect.h.to_le_bytes());
    out
}

pub fn decode(buf: &[u8]) -> Result<Message<'_>, DecodeError> {
    let (&kind, body) = buf.split_first().ok_or(DecodeError::Empty)?;
    match kind {
        KIND_SCREEN => decode_screen(body),
        KIND_TILE => decode_tile(body),
        KIND_TILE_ZSTD => {
            let (rect, frame) = decode_packed_tile(body)?;
            Ok(Message::TileZstd { rect, frame })
        }
        KIND_TILE_LZ4 => {
            let (rect, block) = decode_packed_tile(body)?;
            Ok(Message::TileLz4 { rect, block })
        }
        KIND_TILE_INDEXED_LZ4 => decode_indexed_tile(body, Packing::Lz4),
        KIND_TILE_INDEXED_ZSTD => decode_indexed_tile(body, Packing::Zstd),
        KIND_COPY => decode_copy(body),
        KIND_FILL => decode_fill(body),
        KIND_VIDEO if !body.is_empty() => Ok(Message::Video(body)),
        KIND_VIDEO => Err(DecodeError::Truncated),
        KIND_CURSOR => match body {
            [shape] => CursorShape::from_u8(*shape)
                .map(Message::Cursor)
                .ok_or(DecodeError::BadValue),
            _ => Err(DecodeError::Truncated),
        },
        LAYOUT_PREFIX => match body {
            [layout] if usize::from(*layout) < LAYOUTS.len() => Ok(Message::Layout(*layout)),
            [_] => Err(DecodeError::BadValue),
            _ => Err(DecodeError::Truncated),
        },
        clipboard::KIND_CLIPBOARD_SET => clipboard::decode_set(body),
        KIND_CLIPBOARD => core::str::from_utf8(body)
            .map(Message::Clipboard)
            .map_err(|_| DecodeError::BadValue),
        other => Err(DecodeError::UnknownKind(other)),
    }
}

/// The first byte of a message that carries clipboard text to the page.
pub const CLIPBOARD_PREFIX: u8 = KIND_CLIPBOARD;

/// The text in a clipboard message from a page.
pub fn decode_clipboard_in(buf: &[u8]) -> Result<&str, DecodeError> {
    let (&kind, body) = buf.split_first().ok_or(DecodeError::Empty)?;
    if kind != KIND_CLIPBOARD_IN {
        return Err(DecodeError::UnknownKind(kind));
    }
    if body.len() > MAX_CLIPBOARD_BYTES {
        return Err(DecodeError::TooLarge);
    }
    core::str::from_utf8(body).map_err(|_| DecodeError::BadValue)
}

fn read_u16(body: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([body[offset], body[offset + 1]])
}

fn decode_screen(body: &[u8]) -> Result<Message<'_>, DecodeError> {
    if body.len() != SCREEN_LEN - 1 {
        return Err(DecodeError::Truncated);
    }
    Ok(Message::Screen {
        width: read_u16(body, 0),
        height: read_u16(body, 2),
    })
}

/// The rect of a tile whose pixels are compressed, and the compressed bytes.
fn decode_packed_tile(body: &[u8]) -> Result<(Rect, &[u8]), DecodeError> {
    if body.len() < TILE_HEADER_LEN {
        return Err(DecodeError::Truncated);
    }
    let rect = read_rect(body)?;
    rect.pixel_bytes().ok_or(DecodeError::TooLarge)?;
    Ok((rect, &body[TILE_HEADER_LEN - 1..]))
}

fn decode_indexed_tile(body: &[u8], packing: Packing) -> Result<Message<'_>, DecodeError> {
    let (rect, rest) = decode_packed_tile(body)?;
    let (&last_colour, rest) = rest.split_first().ok_or(DecodeError::Truncated)?;
    let palette_len = (usize::from(last_colour) + 1) * BYTES_PER_PIXEL;
    let (palette, indices) = rest
        .split_at_checked(palette_len)
        .ok_or(DecodeError::Truncated)?;
    Ok(Message::TileIndexed {
        rect,
        palette,
        packing,
        indices,
    })
}

fn decode_copy(body: &[u8]) -> Result<Message<'_>, DecodeError> {
    if body.len() != COPY_LEN - 1 {
        return Err(DecodeError::Truncated);
    }
    Ok(Message::Copy {
        from: read_rect(body)?,
        to: (read_u16(body, 8), read_u16(body, 10)),
    })
}

fn decode_fill(body: &[u8]) -> Result<Message<'_>, DecodeError> {
    if body.len() != FILL_LEN - 1 {
        return Err(DecodeError::Truncated);
    }
    let rect = read_rect(body)?;
    rect.pixel_bytes().ok_or(DecodeError::TooLarge)?;
    let mut pixel = [0; BYTES_PER_PIXEL];
    pixel.copy_from_slice(&body[TILE_HEADER_LEN - 1..]);
    Ok(Message::Fill { rect, pixel })
}

fn read_rect(body: &[u8]) -> Result<Rect, DecodeError> {
    let rect = Rect {
        x: read_u16(body, 0),
        y: read_u16(body, 2),
        w: read_u16(body, 4),
        h: read_u16(body, 6),
    };
    if rect.w == 0 || rect.h == 0 {
        return Err(DecodeError::EmptyRect);
    }
    Ok(rect)
}

fn decode_tile(body: &[u8]) -> Result<Message<'_>, DecodeError> {
    if body.len() < TILE_HEADER_LEN - 1 {
        return Err(DecodeError::Truncated);
    }
    let rect = read_rect(body)?;
    let expected = rect.pixel_bytes().ok_or(DecodeError::TooLarge)?;
    let pixels = &body[TILE_HEADER_LEN - 1..];
    if pixels.len() != expected {
        return Err(DecodeError::PixelLength);
    }
    Ok(Message::Tile { rect, pixels })
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    #[test]
    fn a_fill_carries_its_rect_and_colour() {
        let rect = Rect {
            x: 1,
            y: 2,
            w: 300,
            h: 40,
        };
        let message = encode_fill(rect, [1, 2, 3, 0]);
        assert_eq!(
            decode(&message),
            Ok(Message::Fill {
                rect,
                pixel: [1, 2, 3, 0]
            })
        );
        assert_eq!(
            decode(&message[..FILL_LEN - 1]),
            Err(DecodeError::Truncated)
        );
        let mut empty = message;
        empty[5] = 0;
        empty[6] = 0;
        assert_eq!(decode(&empty), Err(DecodeError::EmptyRect));
    }

    #[test]
    fn video_chunks_are_passed_as_they_are() {
        assert_eq!(
            decode(&[VIDEO_PREFIX, 0, 0, 0, 1, 0x65]),
            Ok(Message::Video(&[0, 0, 0, 1, 0x65]))
        );
        assert_eq!(decode(&[VIDEO_PREFIX]), Err(DecodeError::Truncated));
    }

    #[test]
    fn compressed_tiles_carry_the_rect_and_the_frame() {
        let rect = Rect {
            x: 1,
            y: 2,
            w: 3,
            h: 4,
        };
        let mut bytes = encode_zstd_tile_header(rect).to_vec();
        bytes.extend_from_slice(&[9, 9, 9]);
        assert_eq!(
            decode(&bytes),
            Ok(Message::TileZstd {
                rect,
                frame: &[9, 9, 9]
            })
        );
        assert_eq!(
            decode(&bytes[..TILE_HEADER_LEN - 1]),
            Err(DecodeError::Truncated)
        );
        let mut lz4 = encode_lz4_tile_header(rect).to_vec();
        lz4.extend_from_slice(&[7, 7]);
        assert_eq!(
            decode(&lz4),
            Ok(Message::TileLz4 {
                rect,
                block: &[7, 7]
            })
        );
        let mut indexed = encode_indexed_tile_header(rect, Packing::Zstd).to_vec();
        indexed.extend_from_slice(&[1, 10, 20, 30, 0, 40, 50, 60, 0, 5, 5]);
        assert_eq!(
            decode(&indexed),
            Ok(Message::TileIndexed {
                rect,
                palette: &[10, 20, 30, 0, 40, 50, 60, 0],
                packing: Packing::Zstd,
                indices: &[5, 5]
            })
        );
        assert_eq!(
            decode(&indexed[..TILE_HEADER_LEN + 6]),
            Err(DecodeError::Truncated),
            "a palette that is cut"
        );
        let empty = Rect { w: 0, ..rect };
        let mut bad = encode_zstd_tile_header(empty).to_vec();
        bad.push(1);
        assert_eq!(decode(&bad), Err(DecodeError::EmptyRect));
    }

    #[test]
    fn clipboard_text_goes_both_ways() {
        let mut out = [CLIPBOARD_PREFIX].to_vec();
        out.extend_from_slice("hola ñ".as_bytes());
        assert_eq!(decode(&out), Ok(Message::Clipboard("hola ñ")));
        assert_eq!(
            decode(&[CLIPBOARD_PREFIX, 0xFF]),
            Err(DecodeError::BadValue)
        );
        let mut incoming = [KIND_CLIPBOARD_IN].to_vec();
        incoming.extend_from_slice(b"abc");
        assert_eq!(decode_clipboard_in(&incoming), Ok("abc"));
        assert_eq!(
            decode_clipboard_in(&[KIND_CLIPBOARD_IN, 0xC0]),
            Err(DecodeError::BadValue)
        );
        assert_eq!(
            decode_clipboard_in(&[1, 2]),
            Err(DecodeError::UnknownKind(1))
        );
        let mut buf = std::vec![b'a'; MAX_CLIPBOARD_BYTES + 2];
        buf[0] = KIND_CLIPBOARD_IN;
        assert_eq!(decode_clipboard_in(&buf), Err(DecodeError::TooLarge));
    }

    #[test]
    fn copy_round_trip() {
        let from = Rect {
            x: 0,
            y: 16,
            w: 640,
            h: 464,
        };
        let bytes = encode_copy(from, (0, 0));
        assert_eq!(decode(&bytes), Ok(Message::Copy { from, to: (0, 0) }));
        assert_eq!(decode(&bytes[..COPY_LEN - 1]), Err(DecodeError::Truncated));
        let empty = encode_copy(Rect { h: 0, ..from }, (0, 0));
        assert_eq!(decode(&empty), Err(DecodeError::EmptyRect));
    }

    #[test]
    fn screen_round_trip() {
        let bytes = encode_screen(640, 480);
        assert_eq!(
            decode(&bytes),
            Ok(Message::Screen {
                width: 640,
                height: 480
            })
        );
    }

    #[test]
    fn tile_round_trip() {
        let rect = Rect {
            x: 1,
            y: 2,
            w: 2,
            h: 1,
        };
        let mut bytes = Vec::from(encode_tile_header(rect));
        bytes.extend_from_slice(&[1, 2, 3, 0, 4, 5, 6, 0]);
        assert_eq!(
            decode(&bytes),
            Ok(Message::Tile {
                rect,
                pixels: &[1, 2, 3, 0, 4, 5, 6, 0]
            })
        );
    }

    #[test]
    fn cursor_round_trip() {
        let bytes = encode_cursor(CursorShape::Text);
        assert_eq!(decode(&bytes), Ok(Message::Cursor(CursorShape::Text)));
        assert_eq!(decode(&[KIND_CURSOR, 99]), Err(DecodeError::BadValue));
        assert_eq!(decode(&[KIND_CURSOR]), Err(DecodeError::Truncated));
        assert_eq!(decode(&encode_layout(2)), Ok(Message::Layout(2)));
        assert_eq!(decode(&[LAYOUT_PREFIX, 4]), Err(DecodeError::BadValue));
        assert_eq!(decode(&[LAYOUT_PREFIX]), Err(DecodeError::Truncated));
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(decode(&[]), Err(DecodeError::Empty));
        assert_eq!(decode(&[200]), Err(DecodeError::UnknownKind(200)));
        assert_eq!(decode(&[KIND_SCREEN, 1]), Err(DecodeError::Truncated));
        assert_eq!(decode(&[KIND_TILE, 0, 0]), Err(DecodeError::Truncated));
        let rect = Rect {
            x: 0,
            y: 0,
            w: 2,
            h: 2,
        };
        let mut bytes = Vec::from(encode_tile_header(rect));
        bytes.extend_from_slice(&[0; 4]);
        assert_eq!(decode(&bytes), Err(DecodeError::PixelLength));
        bytes.extend_from_slice(&[0; 8]);
        assert_eq!(decode(&bytes), Err(DecodeError::PixelLength));
    }

    #[test]
    fn rejects_zero_size_tile() {
        let rect = Rect {
            x: 0,
            y: 0,
            w: 0,
            h: 5,
        };
        assert_eq!(
            decode(&encode_tile_header(rect)),
            Err(DecodeError::EmptyRect)
        );
    }

    #[test]
    fn rejects_size_that_overflows_32_bits() {
        let rect = Rect {
            x: 0,
            y: 0,
            w: 32768,
            h: 32768,
        };
        assert_eq!(rect.pixel_bytes(), None);
        assert_eq!(
            decode(&encode_tile_header(rect)),
            Err(DecodeError::TooLarge)
        );
    }
}
