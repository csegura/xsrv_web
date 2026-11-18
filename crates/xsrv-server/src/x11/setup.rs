// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

use super::codec::{padded, Endian, Writer};
use super::screen::{DEFAULT_COLORMAP, RESOURCE_ID_MASK, ROOT_DEPTH, ROOT_VISUAL, ROOT_WINDOW};

pub const PROTOCOL_MAJOR: u16 = 11;
pub const PROTOCOL_MINOR: u16 = 0;
pub const SETUP_HEADER_LEN: usize = 12;

const MAX_AUTH_LEN: usize = 1024;
const RELEASE: u32 = 1;
const MOTION_BUFFER_SIZE: u32 = 256;
const MAX_REQUEST_WORDS: u16 = u16::MAX;
const MIN_KEYCODE: u8 = 8;
const MAX_KEYCODE: u8 = 255;
const VENDOR: &[u8] = b"xsrv-web";
const TRUE_COLOR: u8 = 4;
const PIXELS_PER_MM: f32 = 3.78;

#[derive(Debug, PartialEq, Eq)]
pub enum SetupError {
    ByteOrder(u8),
    AuthTooLong,
}

/// The only authorization protocol that the server knows.
const MAGIC_COOKIE: &[u8] = b"MIT-MAGIC-COOKIE-1";

pub struct SetupRequest {
    pub endian: Endian,
    pub major: u16,
    auth_name_len: usize,
    auth_data_len: usize,
}

impl SetupRequest {
    pub fn parse_header(header: &[u8; SETUP_HEADER_LEN]) -> Result<Self, SetupError> {
        let endian = Endian::from_byte(header[0]).ok_or(SetupError::ByteOrder(header[0]))?;
        let major = endian.read_u16([header[2], header[3]]);
        let auth_name_len = endian.read_u16([header[6], header[7]]) as usize;
        let auth_data_len = endian.read_u16([header[8], header[9]]) as usize;
        if auth_name_len > MAX_AUTH_LEN || auth_data_len > MAX_AUTH_LEN {
            return Err(SetupError::AuthTooLong);
        }
        Ok(Self {
            endian,
            major,
            auth_name_len,
            auth_data_len,
        })
    }

    pub fn trailer_len(&self) -> usize {
        padded(self.auth_name_len) + padded(self.auth_data_len)
    }

    /// True when the client showed the cookie `expected` with MIT-MAGIC-COOKIE-1. The compare takes
    /// the same time whatever the bytes are.
    pub fn shows_cookie(&self, trailer: &[u8], expected: &[u8]) -> bool {
        let name = trailer.get(..self.auth_name_len).unwrap_or(&[]);
        let data_start = padded(self.auth_name_len);
        let data = trailer
            .get(data_start..data_start + self.auth_data_len)
            .unwrap_or(&[]);
        let same_name = name == MAGIC_COOKIE;
        let mut difference = data.len() ^ expected.len();
        for (a, b) in data.iter().zip(expected) {
            difference |= usize::from(a ^ b);
        }
        same_name && difference == 0
    }
}

pub fn failure_reply(endian: Endian, reason: &str) -> Vec<u8> {
    let mut body = Writer::new(endian);
    body.bytes(reason.as_bytes());
    body.pad();
    let mut out = Writer::new(endian);
    out.u8(0);
    out.u8(reason.len() as u8);
    out.u16(PROTOCOL_MAJOR);
    out.u16(PROTOCOL_MINOR);
    out.u16((body.len() / 4) as u16);
    out.bytes(&body.finish());
    out.finish()
}

pub fn success_reply(endian: Endian, resource_id_base: u32, screen: (u16, u16)) -> Vec<u8> {
    let mut body = Writer::new(endian);
    body.u32(RELEASE);
    body.u32(resource_id_base);
    body.u32(RESOURCE_ID_MASK);
    body.u32(MOTION_BUFFER_SIZE);
    body.u16(VENDOR.len() as u16);
    body.u16(MAX_REQUEST_WORDS);
    body.u8(1);
    body.u8(PIXMAP_FORMATS.len() as u8);
    body.u8(0);
    body.u8(0);
    body.u8(32);
    body.u8(32);
    body.u8(MIN_KEYCODE);
    body.u8(MAX_KEYCODE);
    body.zeros(4);
    body.bytes(VENDOR);
    body.pad();
    for (depth, bits_per_pixel) in PIXMAP_FORMATS {
        write_format(&mut body, depth, bits_per_pixel);
    }
    write_screen(&mut body, screen);

    let mut out = Writer::new(endian);
    out.u8(1);
    out.u8(0);
    out.u16(PROTOCOL_MAJOR);
    out.u16(PROTOCOL_MINOR);
    out.u16((body.len() / 4) as u16);
    out.bytes(&body.finish());
    out.finish()
}

/// Depth and bits per pixel of each pixmap format. RENDER clients (cairo) need 8 and 32.
const PIXMAP_FORMATS: [(u8, u8); 4] = [(1, 1), (8, 8), (ROOT_DEPTH, 32), (32, 32)];

fn write_format(out: &mut Writer, depth: u8, bits_per_pixel: u8) {
    out.u8(depth);
    out.u8(bits_per_pixel);
    out.u8(32);
    out.zeros(5);
}

fn write_screen(out: &mut Writer, (width, height): (u16, u16)) {
    out.u32(ROOT_WINDOW);
    out.u32(DEFAULT_COLORMAP);
    out.u32(0x00FF_FFFF);
    out.u32(0);
    out.u32(0);
    out.u16(width);
    out.u16(height);
    out.u16((width as f32 / PIXELS_PER_MM) as u16);
    out.u16((height as f32 / PIXELS_PER_MM) as u16);
    out.u16(1);
    out.u16(1);
    out.u32(ROOT_VISUAL);
    out.u8(0);
    out.u8(0);
    out.u8(ROOT_DEPTH);
    out.u8(PIXMAP_FORMATS.len() as u8);

    out.u8(ROOT_DEPTH);
    out.u8(0);
    out.u16(1);
    out.zeros(4);
    out.u32(ROOT_VISUAL);
    out.u8(TRUE_COLOR);
    out.u8(8);
    out.u16(256);
    out.u32(0x00FF_0000);
    out.u32(0x0000_FF00);
    out.u32(0x0000_00FF);
    out.zeros(4);

    // The other depths have no visuals: they are for pixmaps.
    for (depth, _) in PIXMAP_FORMATS
        .iter()
        .filter(|(depth, _)| *depth != ROOT_DEPTH)
    {
        out.u8(*depth);
        out.u8(0);
        out.u16(0);
        out.zeros(4);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_byte_order() {
        let mut header = [0u8; SETUP_HEADER_LEN];
        header[0] = b'x';
        assert_eq!(
            SetupRequest::parse_header(&header).err(),
            Some(SetupError::ByteOrder(b'x'))
        );
    }

    #[test]
    fn rejects_long_auth_fields() {
        let mut header = [0u8; SETUP_HEADER_LEN];
        header[0] = b'l';
        header[6..8].copy_from_slice(&5000u16.to_le_bytes());
        assert_eq!(
            SetupRequest::parse_header(&header).err(),
            Some(SetupError::AuthTooLong)
        );
    }

    #[test]
    fn reply_length_field_matches_the_data() {
        for endian in [Endian::Little, Endian::Big] {
            let reply = success_reply(endian, 0x20_0000, (640, 480));
            let words = endian.read_u16([reply[6], reply[7]]) as usize;
            assert_eq!(reply.len(), 8 + words * 4);
        }
    }

    #[test]
    fn failure_reply_is_padded() {
        let reply = failure_reply(Endian::Little, "no");
        assert_eq!(reply[0], 0);
        assert_eq!(reply.len(), 8 + 4);
    }
}
