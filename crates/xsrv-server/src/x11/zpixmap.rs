// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! ZPixmap images of depth 8, 24 and 32, and the 4-byte pixels that surfaces keep: the pixel value
//! as a little-endian u32 (see `Surface`). Rows are padded to 32 bits, as the setup announces.
use wire::BYTES_PER_PIXEL;

use super::resources::pixel_mask_for_depth;

/// Bytes of one pixel in a ZPixmap image of this depth. `None` for depths without ZPixmap bytes.
pub fn bytes_per_pixel(depth: u8) -> Option<usize> {
    match depth {
        8 => Some(1),
        24 | 32 => Some(4),
        _ => None,
    }
}

/// Bytes of one image row, padded to 32 bits.
pub fn stride(depth: u8, width: usize) -> Option<usize> {
    let bytes = width.checked_mul(bytes_per_pixel(depth)?)?;
    Some(bytes.div_ceil(4) * 4)
}

/// One image row of `width` pixels as surface pixels.
pub fn unpack_row(depth: u8, row: &[u8], width: usize) -> Vec<u8> {
    let mask = pixel_mask_for_depth(depth);
    match bytes_per_pixel(depth) {
        Some(1) => row[..width]
            .iter()
            .flat_map(|&value| u32::from(value).to_le_bytes())
            .collect(),
        _ => row[..width * BYTES_PER_PIXEL]
            .as_chunks::<BYTES_PER_PIXEL>()
            .0
            .iter()
            .flat_map(|pixel| (u32::from_le_bytes(*pixel) & mask).to_le_bytes())
            .collect(),
    }
}

/// An image of rows of `width` pixels as surface pixels, without the padding of the rows.
pub fn unpack(depth: u8, image: &[u8], width: usize) -> Vec<u8> {
    let Some(row_len) = stride(depth, width).filter(|len| *len > 0) else {
        return Vec::new();
    };
    let mut pixels = Vec::with_capacity(image.len() / row_len * width * BYTES_PER_PIXEL);
    for row in image.chunks_exact(row_len) {
        pixels.extend_from_slice(&unpack_row(depth, row, width));
    }
    pixels
}

/// Surface pixels as image rows of this depth, each padded to 32 bits.
pub fn pack(depth: u8, pixels: &[u8], width: usize, plane_mask: u32) -> Vec<u8> {
    let mask = pixel_mask_for_depth(depth) & plane_mask;
    let Some(row_len) = stride(depth, width) else {
        return Vec::new();
    };
    let row_pixels = width * BYTES_PER_PIXEL;
    let mut out = Vec::with_capacity(row_len * pixels.len() / row_pixels.max(1));
    for row in pixels.chunks(row_pixels.max(1)) {
        let start = out.len();
        for pixel in row.as_chunks::<BYTES_PER_PIXEL>().0 {
            let value = u32::from_le_bytes(*pixel) & mask;
            if depth == 8 {
                out.push(value as u8);
            } else {
                out.extend_from_slice(&value.to_le_bytes());
            }
        }
        out.resize(start + row_len, 0);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_8_rows_are_one_byte_a_pixel_padded_to_32_bits() {
        assert_eq!(stride(8, 5), Some(8));
        assert_eq!(stride(32, 5), Some(20));
        assert_eq!(stride(1, 5), None);
        let row = [7, 200, 0, 0];
        assert_eq!(unpack_row(8, &row, 2), [7, 0, 0, 0, 200, 0, 0, 0]);
        assert_eq!(pack(8, &[7, 0, 0, 0, 200, 0, 0, 0], 2, u32::MAX), row);
    }

    #[test]
    fn depth_32_keeps_alpha_and_depth_24_drops_the_fourth_byte() {
        let pixel = [1, 2, 3, 0x80];
        assert_eq!(unpack_row(32, &pixel, 1), pixel);
        assert_eq!(unpack_row(24, &pixel, 1), [1, 2, 3, 0]);
        assert_eq!(pack(32, &pixel, 1, 0xFF00_00FF), [1, 0, 0, 0x80]);
    }
}
