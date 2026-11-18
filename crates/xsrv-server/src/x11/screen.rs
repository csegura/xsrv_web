// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

pub const ROOT_WINDOW: u32 = 0x20;
pub const DEFAULT_COLORMAP: u32 = 0x21;
pub const ROOT_VISUAL: u32 = 0x22;
pub const ROOT_DEPTH: u8 = 24;
/// The size of the screen at the start. RANDR and the page can change it.
pub const SCREEN_WIDTH: u16 = 640;
pub const SCREEN_HEIGHT: u16 = 480;
pub const RESOURCE_ID_MASK: u32 = 0x1F_FFFF;
pub const RESOURCE_ID_SHIFT: u32 = 21;
pub const MAX_CLIENTS: u32 = 64;
pub const MAX_WINDOWS: usize = 4096;
pub const MAX_WINDOW_DEPTH: usize = 256;
pub const MAX_PIXEL_BYTES: usize = 256 << 20;
/// What one client may hold of the pixel budget, so that one client cannot use it all.
pub const MAX_CLIENT_PIXEL_BYTES: usize = 128 << 20;
/// Windows, pixmaps, GCs, fonts, cursors and shared segments that one client may own.
pub const MAX_RESOURCES_PER_CLIENT: usize = 8192;
/// The pixels that one drawing request may write. Past it the request fails and stops.
pub const MAX_PIXELS_PER_REQUEST: u64 = 64 << 20;
