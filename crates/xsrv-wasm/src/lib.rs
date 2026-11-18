// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

use wasm_bindgen::prelude::*;
use wasm_bindgen::Clamped;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement, ImageData};
use wire::{DecodeError, Input, Message};

#[wasm_bindgen]
pub struct Display {
    canvas: HtmlCanvasElement,
    context: CanvasRenderingContext2d,
    tiles: TileDecoder,
}

#[wasm_bindgen]
impl Display {
    #[wasm_bindgen(constructor)]
    pub fn new(canvas: HtmlCanvasElement) -> Result<Display, JsValue> {
        let context = canvas
            .get_context("2d")?
            .ok_or_else(|| JsValue::from_str("2d context not available"))?
            .dyn_into::<CanvasRenderingContext2d>()?;
        Ok(Display {
            canvas,
            context,
            tiles: TileDecoder::default(),
        })
    }

    /// Draws RGBA pixels. The canvas reads them where they are, without a copy.
    fn draw(
        context: &CanvasRenderingContext2d,
        rect: wire::Rect,
        rgba: &[u8],
    ) -> Result<(), JsValue> {
        let image = ImageData::new_with_u8_clamped_array_and_sh(
            Clamped(rgba),
            rect.w.into(),
            rect.h.into(),
        )?;
        context.put_image_data(&image, rect.x.into(), rect.y.into())
    }

    /// Moves canvas pixels. The canvas handles a source and a target that overlap.
    fn copy(&self, from: wire::Rect, to: (u16, u16)) -> Result<(), JsValue> {
        let (w, h) = (f64::from(from.w), f64::from(from.h));
        self.context
            .draw_image_with_html_canvas_element_and_sw_and_sh_and_dx_and_dy_and_dw_and_dh(
                &self.canvas,
                from.x.into(),
                from.y.into(),
                w,
                h,
                to.0.into(),
                to.1.into(),
                w,
                h,
            )
    }

    /// Draws a message. Returns the text when the message is the X clipboard.
    pub fn apply(&mut self, message: &[u8]) -> Result<Option<String>, JsValue> {
        match wire::decode(message).map_err(describe)? {
            Message::Screen { width, height } => {
                self.canvas.set_width(width.into());
                self.canvas.set_height(height.into());
            }
            Message::Cursor(shape) => {
                self.canvas.style().set_property("cursor", shape.css())?;
            }
            Message::Clipboard(text) => return Ok(Some(text.to_owned())),
            // The page code decodes video with WebCodecs before the message gets here.
            Message::Video(_) => {}
            // The page code reads the layout message before it gets here (settings.js).
            Message::Layout(_) => {}
            // The page code reads clipboard sets (text, HTML, an image) before it gets here.
            Message::ClipboardSet { .. } => {}
            Message::Tile { rect, pixels } => {
                self.tiles.copy(pixels);
                Self::draw(&self.context, rect, self.tiles.rgba(pixels.len()))?;
            }
            Message::Copy { from, to } => self.copy(from, to)?,
            Message::Fill { rect, pixel } => {
                // An opaque colour with the default compositing: the same pixels as a tile, without
                // a buffer of the size of the rect.
                let [blue, green, red, _] = pixel;
                self.context
                    .set_fill_style_str(&format!("rgb({red},{green},{blue})"));
                self.context
                    .fill_rect(rect.x.into(), rect.y.into(), rect.w.into(), rect.h.into());
            }
            Message::TileZstd { rect, frame } => {
                let expected = rect
                    .pixel_bytes()
                    .filter(|bytes| *bytes <= MAX_TILE_BYTES)
                    .ok_or_else(|| JsValue::from_str("tile too large"))?;
                self.tiles
                    .decompress(frame, expected)
                    .map_err(|e| JsValue::from_str(&e))?;
                Self::draw(&self.context, rect, self.tiles.rgba(expected))?;
            }
            Message::TileIndexed {
                rect,
                palette,
                packing,
                indices,
            } => {
                let expected = rect
                    .pixel_bytes()
                    .filter(|bytes| *bytes <= MAX_TILE_BYTES)
                    .ok_or_else(|| JsValue::from_str("tile too large"))?;
                let rgba = self
                    .tiles
                    .indexed(palette, packing, indices, expected / 4)
                    .map_err(|e| JsValue::from_str(&e))?;
                Self::draw(&self.context, rect, rgba)?;
            }
            Message::TileLz4 { rect, block } => {
                let expected = rect
                    .pixel_bytes()
                    .filter(|bytes| *bytes <= MAX_TILE_BYTES)
                    .ok_or_else(|| JsValue::from_str("tile too large"))?;
                self.tiles
                    .decompress_lz4(block, expected)
                    .map_err(|e| JsValue::from_str(&e))?;
                Self::draw(&self.context, rect, self.tiles.rgba(expected))?;
            }
        }
        self.tiles.release();
        Ok(None)
    }
}

fn encoded(input: Input) -> Vec<u8> {
    let (bytes, len) = wire::encode_input(input);
    bytes[..len].to_vec()
}

#[wasm_bindgen]
pub fn encode_pointer(x: u16, y: u16) -> Vec<u8> {
    encoded(Input::Pointer { x, y })
}

#[wasm_bindgen]
pub fn encode_button(button: u8, pressed: bool) -> Vec<u8> {
    encoded(Input::Button { button, pressed })
}

#[wasm_bindgen]
pub fn encode_key(keycode: u8, pressed: bool) -> Vec<u8> {
    encoded(Input::Key { keycode, pressed })
}

#[wasm_bindgen]
pub fn encode_pointer_left() -> Vec<u8> {
    encoded(Input::PointerLeft)
}

#[wasm_bindgen]
pub fn encode_release_all() -> Vec<u8> {
    encoded(Input::ReleaseAll)
}

#[wasm_bindgen]
pub fn encode_resize(width: u16, height: u16) -> Vec<u8> {
    encoded(Input::Resize { width, height })
}

#[wasm_bindgen]
pub fn encode_locks(caps: bool, num: bool) -> Vec<u8> {
    encoded(Input::Locks { caps, num })
}

#[wasm_bindgen]
pub fn encode_layout(layout: u8) -> Vec<u8> {
    encoded(Input::Layout { layout })
}

/// The name of the keyboard layout with this index (`us`, `es` ...), for the page's list.
#[wasm_bindgen]
pub fn layout_name(index: u8) -> Option<String> {
    wire::LAYOUTS
        .get(usize::from(index))
        .map(|name| (*name).to_owned())
}

fn describe(error: DecodeError) -> JsValue {
    JsValue::from_str(&format!("bad message: {error:?}"))
}

/// The largest screen is 4096 x 4096 pixels. A bigger tile is a bad message.
const MAX_TILE_BYTES: usize = 4096 * 4096 * 4;

/// The pixel buffer goes back to this size after a larger tile (a 1920x1080 screen).
const KEPT_PIXEL_BYTES: usize = 1920 * 1080 * 4;

/// Holds the pixels of the tile that is drawn now. The buffer and the zstd decoder are kept
/// between tiles: a new buffer for each tile, and a second one for RGBA, took as long as the
/// decoding.
#[derive(Default)]
struct TileDecoder {
    decoder: ruzstd::decoding::FrameDecoder,
    pixels: Vec<u8>,
    indices: Vec<u8>,
}

impl TileDecoder {
    /// Decodes a tile of few colours: `count` indices, compressed, and the BGRX pixel of each
    /// colour. Returns the RGBA pixels. An index without a colour is an error.
    fn indexed(
        &mut self,
        palette: &[u8],
        packing: wire::Packing,
        packed: &[u8],
        count: usize,
    ) -> Result<&[u8], String> {
        let mut colours = [[0, 0, 0, 255]; wire::MAX_PALETTE_COLOURS];
        for (colour, bgrx) in colours.iter_mut().zip(palette.as_chunks::<4>().0) {
            *colour = [bgrx[2], bgrx[1], bgrx[0], 255];
        }
        if self.indices.len() < count {
            self.indices.resize(count, 0);
        }
        let indices = &mut self.indices[..count];
        let written = match packing {
            wire::Packing::Lz4 => lz4_flex::block::decompress_into(packed, indices)
                .map_err(|error| format!("bad lz4 data: {error}"))?,
            wire::Packing::Zstd => self
                .decoder
                .decode_all(packed, indices)
                .map_err(|error| format!("bad zstd data: {error}"))?,
        };
        if written != count {
            return Err(format!(
                "indexed tile has {written} pixels, expected {count}"
            ));
        }
        // The server never sends an index without a colour. One would show a fault of the
        // encoder, so it is an error here, as it is for the tile decoder of the server.
        let colour_count = palette.len() / 4;
        if indices
            .iter()
            .any(|index| usize::from(*index) >= colour_count)
        {
            return Err("indexed tile has an index without a colour".into());
        }
        if self.pixels.len() < count * 4 {
            self.pixels.resize(count * 4, 0);
        }
        let pixels = &mut self.pixels[..count * 4];
        for (pixel, index) in pixels.as_chunks_mut::<4>().0.iter_mut().zip(&*indices) {
            *pixel = colours[usize::from(*index)];
        }
        Ok(pixels)
    }

    /// Takes the BGRX pixels of a raw tile.
    fn copy(&mut self, pixels: &[u8]) {
        self.room(pixels.len()).copy_from_slice(pixels);
    }

    /// Decodes one zstd frame that must hold exactly `expected` bytes of BGRX pixels. The frame
    /// is decoded into a buffer of that size, so a bad frame cannot fill the memory of the page.
    fn decompress(&mut self, frame: &[u8], expected: usize) -> Result<(), String> {
        if self.pixels.len() < expected {
            self.pixels.resize(expected, 0);
        }
        let written = self
            .decoder
            .decode_all(frame, &mut self.pixels[..expected])
            .map_err(|error| format!("bad zstd data: {error}"))?;
        if written != expected {
            return Err(format!(
                "zstd tile has {written} bytes, expected {expected}"
            ));
        }
        Ok(())
    }

    /// Decodes one LZ4 block that must hold exactly `expected` bytes of BGRX pixels. The
    /// decoder checks every read and write, and cannot write past the buffer.
    fn decompress_lz4(&mut self, block: &[u8], expected: usize) -> Result<(), String> {
        let written = lz4_flex::block::decompress_into(block, self.room(expected))
            .map_err(|error| format!("bad lz4 data: {error}"))?;
        if written != expected {
            return Err(format!("lz4 tile has {written} bytes, expected {expected}"));
        }
        Ok(())
    }

    /// The first `len` bytes of the buffer, turned from BGRX into RGBA where they are.
    fn rgba(&mut self, len: usize) -> &[u8] {
        let pixels = &mut self.pixels[..len];
        bgrx_to_rgba(pixels);
        pixels
    }

    fn room(&mut self, len: usize) -> &mut [u8] {
        if self.pixels.len() < len {
            self.pixels.resize(len, 0);
        }
        &mut self.pixels[..len]
    }

    /// Gives back the memory of a tile that was larger than a usual screen.
    fn release(&mut self) {
        if self.pixels.len() > KEPT_PIXEL_BYTES {
            self.pixels.truncate(KEPT_PIXEL_BYTES);
            self.pixels.shrink_to(KEPT_PIXEL_BYTES);
        }
        if self.indices.len() > KEPT_PIXEL_BYTES / 4 {
            self.indices.truncate(KEPT_PIXEL_BYTES / 4);
            self.indices.shrink_to(KEPT_PIXEL_BYTES / 4);
        }
    }
}

/// Swaps blue and red and makes each pixel opaque: the canvas wants RGBA.
fn bgrx_to_rgba(pixels: &mut [u8]) {
    for pixel in pixels.as_chunks_mut::<4>().0 {
        let [blue, green, red, _] = *pixel;
        *pixel = [red, green, blue, 255];
    }
}

#[cfg(test)]
mod tests {
    use super::{bgrx_to_rgba, TileDecoder};

    #[test]
    fn zstd_tiles_decode_to_the_original_pixels() {
        let pixels: Vec<u8> = (0..4096u32).map(|i| (i / 7) as u8).collect();
        let frame = zstd::bulk::compress(&pixels, 1).unwrap();
        let mut tiles = TileDecoder::default();
        let mut decompress = |frame: &[u8], expected: usize| {
            tiles
                .decompress(frame, expected)
                .map(|()| tiles.pixels[..expected].to_vec())
        };
        assert_eq!(decompress(&frame, pixels.len()).unwrap(), pixels);
        assert!(
            decompress(&frame, pixels.len() - 1).is_err(),
            "too much data"
        );
        assert!(
            decompress(&frame, pixels.len() + 1).is_err(),
            "too little data"
        );
        assert!(
            decompress(&frame[..frame.len() / 2], pixels.len()).is_err(),
            "cut frame"
        );
        assert!(decompress(b"not zstd", 8).is_err());
        assert_eq!(
            decompress(&frame, pixels.len()).unwrap(),
            pixels,
            "the decoder works again after bad frames"
        );
    }

    #[test]
    fn lz4_tiles_decode_to_the_original_pixels() {
        let pixels: Vec<u8> = (0..4096u32).map(|i| (i / 7) as u8).collect();
        let block = lz4_flex::block::compress(&pixels);
        let mut tiles = TileDecoder::default();
        tiles.decompress_lz4(&block, pixels.len()).unwrap();
        assert_eq!(tiles.pixels[..pixels.len()], pixels);
        assert!(
            tiles.decompress_lz4(&block, pixels.len() - 1).is_err(),
            "too much data"
        );
        assert!(
            tiles.decompress_lz4(&block, pixels.len() + 1).is_err(),
            "too little data"
        );
        assert!(
            tiles
                .decompress_lz4(&block[..block.len() / 2], pixels.len())
                .is_err(),
            "cut block"
        );
    }

    #[test]
    fn indexed_tiles_give_the_colour_of_each_index_as_rgba() {
        let palette = [1, 2, 3, 0, 4, 5, 6, 0];
        let indices = [0u8, 1, 1, 0];
        let mut tiles = TileDecoder::default();
        let expected = [
            [3, 2, 1, 255],
            [6, 5, 4, 255],
            [6, 5, 4, 255],
            [3, 2, 1, 255],
        ]
        .concat();
        let lz4 = lz4_flex::block::compress(&indices);
        assert_eq!(
            tiles
                .indexed(&palette, wire::Packing::Lz4, &lz4, 4)
                .unwrap(),
            expected
        );
        let zstd = zstd::bulk::compress(&indices, 1).unwrap();
        assert_eq!(
            tiles
                .indexed(&palette, wire::Packing::Zstd, &zstd, 4)
                .unwrap(),
            expected
        );
        assert!(tiles
            .indexed(&palette, wire::Packing::Zstd, &zstd, 3)
            .is_err());
        assert!(tiles
            .indexed(&palette, wire::Packing::Lz4, &lz4, 5)
            .is_err());
        let bad = lz4_flex::block::compress(&[0u8, 1, 2, 0]);
        assert!(
            tiles
                .indexed(&palette, wire::Packing::Lz4, &bad, 4)
                .is_err(),
            "an index without a colour"
        );
    }

    #[test]
    fn a_smaller_tile_after_a_larger_one_gives_only_its_own_pixels() {
        let mut tiles = TileDecoder::default();
        tiles.copy(&[9; 16]);
        tiles.copy(&[1, 2, 3, 0]);
        assert_eq!(tiles.rgba(4), [3, 2, 1, 255]);
    }

    #[test]
    fn swaps_channels_and_sets_alpha() {
        let mut pixels = [1, 2, 3, 0, 4, 5, 6, 7];
        bgrx_to_rgba(&mut pixels);
        assert_eq!(pixels, [3, 2, 1, 255, 6, 5, 4, 255]);
    }

    #[test]
    fn page_messages_decode_on_the_server_side() {
        use super::{encode_button, encode_key, encode_pointer, encode_pointer_left};
        use wire::Input;
        assert_eq!(
            wire::decode_input(&encode_pointer(3, 4)),
            Ok(Input::Pointer { x: 3, y: 4 })
        );
        assert_eq!(
            wire::decode_input(&encode_button(3, true)),
            Ok(Input::Button {
                button: 3,
                pressed: true
            })
        );
        assert_eq!(
            wire::decode_input(&encode_key(38, false)),
            Ok(Input::Key {
                keycode: 38,
                pressed: false
            })
        );
        assert_eq!(
            wire::decode_input(&encode_pointer_left()),
            Ok(Input::PointerLeft)
        );
    }
}
