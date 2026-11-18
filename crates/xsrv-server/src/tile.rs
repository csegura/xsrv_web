// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

use wire::{Packing, Rect, BYTES_PER_PIXEL, MAX_PALETTE_COLOURS};
use zstd::bulk::Compressor;
use zstd::zstd_safe::compress_bound;

/// Tiles with fewer pixel bytes than this are sent as they are.
const MIN_COMPRESS_BYTES: usize = 1024;
const ZSTD_LEVEL: i32 = 1;
/// The work buffers go back to this size after a larger tile (a 1920x1080 screen).
const KEPT_BUFFER_BYTES: usize = 1920 * 1080 * BYTES_PER_PIXEL;

/// How tiles are compressed. The page decodes both, so the choice is only about speed and size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Compression {
    /// Fast to compress and, above all, fast to decode in the page. For a local or fast network.
    #[default]
    Lz4,
    /// Fewer bytes, but the page needs longer to decode them. For a slow network.
    Zstd,
}

impl Compression {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "lz4" => Some(Self::Lz4),
            "zstd" => Some(Self::Zstd),
            _ => None,
        }
    }

    fn packing(self) -> Packing {
        match self {
            Self::Lz4 => Packing::Lz4,
            Self::Zstd => Packing::Zstd,
        }
    }
}

/// Builds tile messages. It keeps one zstd context and its work buffers between tiles: a new
/// context for each tile cost more than the compression of a small tile.
pub struct TileEncoder {
    compression: Compression,
    /// `None` when zstd could not make a context. Tiles then go raw.
    zstd: Option<Compressor<'static>>,
    rows: Vec<u8>,
    packed: Vec<u8>,
    palette: Palette,
}

impl Default for TileEncoder {
    fn default() -> Self {
        Self::new(Compression::default())
    }
}

impl TileEncoder {
    pub fn new(compression: Compression) -> Self {
        Self {
            compression,
            zstd: Compressor::new(ZSTD_LEVEL).ok(),
            rows: Vec::new(),
            packed: Vec::new(),
            palette: Palette::default(),
        }
    }

    /// Frees the work buffers; the next tile allocates them again.
    pub fn release(&mut self) {
        self.rows = Vec::new();
        self.packed = Vec::new();
        self.palette.indices = Vec::new();
    }

    pub fn set_compression(&mut self, compression: Compression) {
        self.compression = compression;
    }

    /// The message for `band`, a part of `rect` whose BGRX pixels are `pixels`.
    pub fn band(&mut self, rect: Rect, pixels: &[u8], band: Rect) -> Vec<u8> {
        let stride = rect.w as usize * BYTES_PER_PIXEL;
        let top = (band.y - rect.y) as usize * stride;
        let rows = &pixels[top..top + band.h as usize * stride];
        if band.w == rect.w {
            return self.tile(band, rows);
        }
        let row_bytes = band.w as usize * BYTES_PER_PIXEL;
        let left = (band.x - rect.x) as usize * BYTES_PER_PIXEL;
        let mut gathered = std::mem::take(&mut self.rows);
        gathered.clear();
        for row in rows.chunks_exact(stride) {
            gathered.extend_from_slice(&row[left..left + row_bytes]);
        }
        let message = self.tile(band, &gathered);
        gathered.shrink_to(KEPT_BUFFER_BYTES);
        self.rows = gathered;
        message
    }

    /// The message for `rect` with its BGRX `pixels`, in the smallest of the forms that the
    /// compression allows. It is built once for all pages.
    pub fn tile(&mut self, rect: Rect, pixels: &[u8]) -> Vec<u8> {
        if let Some(pixel) = single_colour(pixels) {
            return wire::encode_fill(rect, pixel).to_vec();
        }
        let message = self
            .indexed_tile(rect, pixels)
            .or_else(|| self.packed_tile(rect, pixels))
            .unwrap_or_else(|| join(&wire::encode_tile_header(rect), &[], pixels));
        self.packed.truncate(KEPT_BUFFER_BYTES);
        self.packed.shrink_to(KEPT_BUFFER_BYTES);
        message
    }

    /// A tile of at most 256 colours (text, most of a toolkit window): one byte for each pixel,
    /// compressed. A quarter of the bytes go through the compressor and through the decoder of
    /// the page, and the result is smaller than the compressed pixels.
    fn indexed_tile(&mut self, rect: Rect, pixels: &[u8]) -> Option<Vec<u8>> {
        if pixels.len() < MIN_COMPRESS_BYTES {
            return None;
        }
        let mut palette = std::mem::take(&mut self.palette);
        let message = palette.index(pixels).and_then(|()| {
            let len = self.pack(&palette.indices)?;
            let mut head =
                wire::encode_indexed_tile_header(rect, self.compression.packing()).to_vec();
            head.push((palette.colours.len() - 1) as u8);
            Some(join(
                &head,
                palette.colours.as_flattened(),
                &self.packed[..len],
            ))
        });
        palette
            .indices
            .shrink_to(KEPT_BUFFER_BYTES / BYTES_PER_PIXEL);
        self.palette = palette;
        message
    }

    /// The pixels as they are, compressed.
    fn packed_tile(&mut self, rect: Rect, pixels: &[u8]) -> Option<Vec<u8>> {
        if pixels.len() < MIN_COMPRESS_BYTES {
            return None;
        }
        let len = self.pack(pixels)?;
        let header = match self.compression {
            Compression::Lz4 => wire::encode_lz4_tile_header(rect),
            Compression::Zstd => wire::encode_zstd_tile_header(rect),
        };
        Some(join(&header, &[], &self.packed[..len]))
    }

    /// Compresses `bytes` into `self.packed`. Returns the length of the result when it is
    /// smaller than `bytes`.
    fn pack(&mut self, bytes: &[u8]) -> Option<usize> {
        let len = match self.compression {
            Compression::Lz4 => {
                let room = lz4_flex::block::get_maximum_output_size(bytes.len());
                lz4_flex::block::compress_into(bytes, self.room(room)).ok()?
            }
            Compression::Zstd => {
                // With room for the worst case, zstd writes straight into the buffer.
                let room = compress_bound(bytes.len());
                self.room(room);
                let zstd = self.zstd.as_mut()?;
                zstd.compress_to_buffer(bytes, &mut self.packed[..room])
                    .ok()?
            }
        };
        (len < bytes.len()).then_some(len)
    }

    fn room(&mut self, len: usize) -> &mut [u8] {
        if self.packed.len() < len {
            self.packed.resize(len, 0);
        }
        &mut self.packed[..len]
    }
}

/// The colour of `pixels` when they all have the same one. A tile with two colours usually
/// differs within its first row, so the test is short where it fails.
fn single_colour(pixels: &[u8]) -> Option<Pixel> {
    let (pixels, _) = pixels.as_chunks::<BYTES_PER_PIXEL>();
    let first = *pixels.first()?;
    pixels.iter().all(|pixel| *pixel == first).then_some(first)
}

/// What a page makes of a tile message of any kind: the rect and its BGRX pixels. `None` when
/// the message is not a tile or its data is bad. Tests and tools read tiles with it.
pub fn decode_tile(message: &[u8]) -> Option<(Rect, Vec<u8>)> {
    match wire::decode(message).ok()? {
        wire::Message::Tile { rect, pixels } => Some((rect, pixels.to_vec())),
        wire::Message::TileZstd { rect, frame } => {
            let pixels = zstd::bulk::decompress(frame, rect.pixel_bytes()?).ok()?;
            Some((rect, pixels))
        }
        wire::Message::TileLz4 { rect, block } => {
            let pixels = lz4_flex::block::decompress(block, rect.pixel_bytes()?).ok()?;
            Some((rect, pixels))
        }
        wire::Message::Fill { rect, pixel } => {
            Some((rect, pixel.repeat(rect.pixel_bytes()? / BYTES_PER_PIXEL)))
        }
        wire::Message::TileIndexed {
            rect,
            palette,
            packing,
            indices,
        } => {
            let count = rect.pixel_bytes()? / BYTES_PER_PIXEL;
            let indices = match packing {
                Packing::Lz4 => lz4_flex::block::decompress(indices, count).ok()?,
                Packing::Zstd => zstd::bulk::decompress(indices, count).ok()?,
            };
            let colours = palette.as_chunks::<BYTES_PER_PIXEL>().0;
            let mut pixels = Vec::with_capacity(count * BYTES_PER_PIXEL);
            for index in indices {
                pixels.extend_from_slice(colours.get(usize::from(index))?);
            }
            (pixels.len() == count * BYTES_PER_PIXEL).then_some((rect, pixels))
        }
        _ => None,
    }
}

fn join(head: &[u8], middle: &[u8], tail: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(head.len() + middle.len() + tail.len());
    message.extend_from_slice(head);
    message.extend_from_slice(middle);
    message.extend_from_slice(tail);
    message
}

const SLOTS: usize = 1024;
type Pixel = [u8; BYTES_PER_PIXEL];

/// The colours of a tile and, for each pixel, the number of its colour.
struct Palette {
    colours: Vec<Pixel>,
    indices: Vec<u8>,
    table: ColourTable,
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            colours: Vec::new(),
            indices: Vec::new(),
            table: ColourTable {
                pixels: [0; SLOTS],
                numbers: [0; SLOTS],
            },
        }
    }
}

impl Palette {
    /// Fills `colours` and `indices` from BGRX pixels. `None` when the pixels have more than
    /// 256 colours; a photo has that many in its first rows, so the test ends early.
    fn index(&mut self, pixels: &[u8]) -> Option<()> {
        let Self {
            colours,
            indices,
            table,
        } = self;
        colours.clear();
        table.numbers.fill(0);
        let pixels = pixels.as_chunks::<BYTES_PER_PIXEL>().0;
        indices.clear();
        indices.resize(pixels.len(), 0);
        let first = u32::from_le_bytes(*pixels.first()?);
        let mut last = (first, table.place(colours, first)?);
        for (index, pixel) in indices.iter_mut().zip(pixels) {
            let pixel = u32::from_le_bytes(*pixel);
            // Most pixels have the colour of the pixel before them.
            if pixel != last.0 {
                last = (pixel, table.place(colours, pixel)?);
            }
            *index = last.1;
        }
        Some(())
    }
}

/// A hash table from a pixel value to the number of its colour. Text changes colour at every
/// second pixel, so a lookup reads the pixel and its number from the table and nothing else.
struct ColourTable {
    pixels: [u32; SLOTS],
    /// The number of the colour plus one. 0: the slot is free.
    numbers: [u16; SLOTS],
}

impl ColourTable {
    /// The number of a colour, which is added to `colours` when it is new. `None` when there
    /// is no room for one more.
    #[inline]
    fn place(&mut self, colours: &mut Vec<Pixel>, pixel: u32) -> Option<u8> {
        let mut slot = (pixel.wrapping_mul(0x9E37_79B1) >> 22) as usize;
        loop {
            match self.numbers[slot] {
                0 => {
                    if colours.len() == MAX_PALETTE_COLOURS {
                        return None;
                    }
                    colours.push(pixel.to_le_bytes());
                    self.pixels[slot] = pixel;
                    self.numbers[slot] = colours.len() as u16;
                    return Some((colours.len() - 1) as u8);
                }
                number if self.pixels[slot] == pixel => return Some((number - 1) as u8),
                _ => slot = (slot + 1) % SLOTS,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect { x, y, w, h }
    }

    /// Pixels with more colours than a palette holds, which still compress well.
    fn many_colours(count: usize) -> Vec<u8> {
        (0..count as u32)
            .flat_map(|index| ((index / 4) % 1000 * 3).to_le_bytes())
            .collect()
    }

    #[test]
    fn a_band_holds_only_its_own_columns_and_rows() {
        let area = rect(10, 20, 4, 3);
        let pixels: Vec<u8> = (0..4 * 3 * 4).map(|byte| byte as u8).collect();
        let band = rect(11, 21, 2, 2);
        let message = TileEncoder::default().band(area, &pixels, band);
        let Ok(wire::Message::Tile { rect: sent, pixels }) = wire::decode(&message) else {
            panic!("a small band is a raw tile");
        };
        assert_eq!(sent, band);
        assert_eq!(
            pixels,
            [20, 21, 22, 23, 24, 25, 26, 27, 36, 37, 38, 39, 40, 41, 42, 43]
        );
    }

    #[test]
    fn every_kind_of_tile_comes_back_the_same() {
        let area = rect(3, 4, 64, 32);
        let few_colours: Vec<u8> = (0..64 * 32_u32)
            .flat_map(|index| [(index / 7 % 5) as u8 * 40, 20, 30, 0])
            .collect();
        let many = many_colours(64 * 32);
        for compression in [Compression::Lz4, Compression::Zstd] {
            let mut encoder = TileEncoder::new(compression);
            for pixels in [&few_colours, &many, &few_colours] {
                let message = encoder.tile(area, pixels);
                assert!(message.len() < pixels.len(), "{compression:?}");
                assert_eq!(
                    decode_tile(&message),
                    Some((area, pixels.clone())),
                    "{compression:?}"
                );
            }
        }
    }

    #[test]
    fn few_colours_make_an_indexed_tile_and_many_colours_a_packed_one() {
        let area = rect(0, 0, 64, 32);
        let mut encoder = TileEncoder::new(Compression::Zstd);
        let two = [[10, 20, 30, 0], [40, 50, 60, 0]].concat().repeat(32 * 32);
        assert!(matches!(
            wire::decode(&encoder.tile(area, &two)),
            Ok(wire::Message::TileIndexed {
                palette: [10, 20, 30, 0, 40, 50, 60, 0],
                packing: Packing::Zstd,
                ..
            })
        ));
        assert!(matches!(
            wire::decode(&encoder.tile(area, &many_colours(64 * 32))),
            Ok(wire::Message::TileZstd { .. })
        ));
        encoder.set_compression(Compression::Lz4);
        assert!(matches!(
            wire::decode(&encoder.tile(area, &many_colours(64 * 32))),
            Ok(wire::Message::TileLz4 { .. })
        ));
    }

    #[test]
    fn released_buffers_are_made_again_for_the_next_tile() {
        let area = rect(0, 0, 64, 32);
        let two = [[10, 20, 30, 0], [40, 50, 60, 0]].concat().repeat(32 * 32);
        let mut encoder = TileEncoder::default();
        let before = encoder.tile(area, &two);
        encoder.release();
        assert_eq!(encoder.packed.capacity(), 0);
        assert_eq!(encoder.tile(area, &two), before);
    }

    #[test]
    fn one_colour_is_a_fill_of_13_bytes() {
        let area = rect(5, 6, 640, 400);
        let flat = [10, 20, 30, 0].repeat(640 * 400);
        let message = TileEncoder::default().tile(area, &flat);
        assert_eq!(message.len(), wire::FILL_LEN);
        assert_eq!(decode_tile(&message), Some((area, flat)));
        let mut almost = [10, 20, 30, 0].repeat(4 * 4);
        almost[60] = 11;
        assert!(!matches!(
            wire::decode(&TileEncoder::default().tile(rect(0, 0, 4, 4), &almost)),
            Ok(wire::Message::Fill { .. })
        ));
    }

    #[test]
    fn a_palette_holds_256_colours_and_not_one_more() {
        let colours = |count: u32| -> Vec<u8> {
            (0..count)
                .flat_map(|colour| (colour * 0x0001_0101).to_le_bytes())
                .collect()
        };
        let mut palette = Palette::default();
        assert_eq!(palette.index(&colours(256)), Some(()));
        assert_eq!(palette.colours.len(), 256);
        assert_eq!(palette.indices, (0..=255).collect::<Vec<u8>>());
        assert_eq!(palette.index(&colours(257)), None);
        assert_eq!(palette.index(&colours(2)), Some(()), "it works again");
        assert_eq!(palette.indices, [0, 1]);
    }
}
