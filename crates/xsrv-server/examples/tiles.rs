// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! Writes the tile messages that the server sends for typical screen contents, with each kind
//! of compression, and prints their size and the time to build them. `make bench-web` feeds
//! the files to the page module, so the page benchmark decodes what the server really sends.
//!
//! Usage: `tiles <output directory>`.
use std::path::Path;
use std::time::{Duration, Instant};

use wire::Rect;
use xsrv_server::display::{Compression, Display};

const ROUNDS: usize = 20;
const CELL: (usize, usize) = (8, 16);

type Failure = Box<dyn std::error::Error>;
/// The BGRX pixels of a picture of a size. `seed` gives another picture of the same kind.
type Content = fn(usize, usize, u32) -> Vec<u8>;

fn main() -> Result<(), Failure> {
    let dir = std::env::args().nth(1).ok_or("usage: tiles <directory>")?;
    let dir = Path::new(&dir);
    std::fs::create_dir_all(dir)?;
    let contents: [(&str, Content); 3] = [("text", text), ("window", window), ("photo", noise)];
    let sizes: [(u16, u16); 4] = [(8, 16), (1280, 16), (800, 600), (1920, 1080)];
    let codecs = [("lz4", Compression::Lz4), ("zstd", Compression::Zstd)];
    println!(
        "{:<24} {:<5} {:>9} {:>7} {:>9}",
        "tile", "codec", "bytes", "ratio", "mean ms"
    );
    for (content_name, content) in contents {
        for (width, height) in sizes {
            for (codec_name, codec) in codecs {
                let name = format!("{content_name}-{width}x{height}");
                let (message, time) = build(content, (width, height), codec)?;
                let raw = usize::from(width) * usize::from(height) * 4;
                println!(
                    "{name:<24} {codec_name:<5} {:>9} {:>7.1} {:>9.3}",
                    message.len(),
                    raw as f64 / message.len() as f64,
                    time.as_secs_f64() * 1000.0
                );
                std::fs::write(dir.join(format!("{name}.{codec_name}.tile")), &message)?;
            }
        }
    }
    Ok(())
}

/// The message of a full change of the screen, and the mean time of such an update (compare,
/// write and compress).
fn build(
    content: Content,
    (width, height): (u16, u16),
    codec: Compression,
) -> Result<(Vec<u8>, Duration), Failure> {
    let display = Display::new(width, height);
    display.set_compression(codec);
    let mut page = display.subscribe();
    let rect = Rect {
        x: 0,
        y: 0,
        w: width,
        h: height,
    };
    let pictures = [
        content(width.into(), height.into(), 1),
        content(width.into(), height.into(), 2),
    ];
    let mut total = Duration::ZERO;
    let mut message = Vec::new();
    for round in 0..ROUNDS {
        let start = Instant::now();
        display
            .update(rect, &pictures[round % 2])
            .map_err(|error| format!("update failed: {error:?}"))?;
        total += start.elapsed();
        // The first update changes the whole screen, so its tile is the largest.
        while let Ok(tile) = page.try_recv() {
            if tile.len() > message.len() {
                message = tile.to_vec();
            }
        }
    }
    Ok((message, total / ROUNDS as u32))
}

fn random(state: &mut u32) -> u32 {
    *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    *state >> 16
}

/// Dark text on a light background: 8x16 cells, each with a shape that depends on its
/// character, in lines of different lengths, as in a terminal.
fn text(width: usize, height: usize, seed: u32) -> Vec<u8> {
    let mut pixels = [0xF4, 0xF4, 0xF4, 0].repeat(width * height);
    let mut state = seed;
    for line in 0..height.div_ceil(CELL.1) {
        let length = 20 + random(&mut state) as usize % 140;
        for column in 0..length.min(width / CELL.0) {
            let character = random(&mut state) % 64;
            if character < 10 {
                continue;
            }
            let mut shape = character.wrapping_mul(0x9E37_79B9);
            for row in 2..14 {
                if row % 2 == 0 {
                    shape = shape.rotate_left(7) ^ character;
                }
                for bit in 1..7 {
                    let (x, y) = (column * CELL.0 + bit, line * CELL.1 + row);
                    if shape >> bit & 1 == 1 && x < width && y < height {
                        let at = (y * width + x) * 4;
                        pixels[at..at + 3].copy_from_slice(&[0x20, 0x20, 0x20]);
                    }
                }
            }
        }
    }
    pixels
}

/// A toolkit window: a header, flat areas, rows of short marks and a few coloured blocks.
fn window(width: usize, height: usize, seed: u32) -> Vec<u8> {
    let mut pixels = vec![0u8; width * height * 4];
    let block = (
        20 + seed as usize * 37 % width.max(1),
        30 + seed as usize * 23 % height.max(1),
    );
    for (index, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (x, y) = (index % width, index / width);
        let in_block =
            (block.0..block.0 + 200).contains(&x) && (block.1..block.1 + 150).contains(&y);
        let mark = y % 20 > 12 && (x * 7 + y / 20 * 13 + seed as usize) % 11 < 4;
        *pixel = if in_block {
            [0xA4, 0x65, 0x34, 0]
        } else if y < 40 {
            [0xD0, 0xD0, 0xD0, 0]
        } else if mark {
            [0x30, 0x30, 0x30, 0]
        } else {
            [0xF6, 0xF5, 0xF4, 0]
        };
    }
    pixels
}

/// A photo or a video frame: pixels that no compression makes smaller.
fn noise(width: usize, height: usize, seed: u32) -> Vec<u8> {
    let mut state = 0x9E37_79B9 ^ seed;
    let mut pixels = vec![0u8; width * height * 4];
    for pixel in pixels.as_chunks_mut::<4>().0 {
        let value = random(&mut state);
        *pixel = [value as u8, (value >> 4) as u8, (value >> 8) as u8, 0];
    }
    pixels
}
