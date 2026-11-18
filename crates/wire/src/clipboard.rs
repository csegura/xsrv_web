// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! The clipboard messages that carry several formats at once (text, HTML, a PNG image).
//!
//! X to page: `[KIND_CLIPBOARD_SET][source][count]` and the parts.
//! Page to X:  `[KIND_CLIPBOARD_SET_IN][count]` and the parts. A count of 0 says that the browser
//! clipboard has nothing that can be pasted.
//! A part is `[format][length: u32, little endian][bytes]`. A format appears once.
use crate::{DecodeError, Message};

pub const KIND_CLIPBOARD_SET: u8 = 12;
pub const KIND_CLIPBOARD_SET_IN: u8 = 25;
/// The most text, or HTML, in one part.
pub const MAX_CLIPBOARD_TEXT: usize = 4 * 1024 * 1024;
/// The most bytes of a PNG image in one part.
pub const MAX_CLIPBOARD_PNG: usize = 16 * 1024 * 1024;
/// The most bytes of one whole message of this kind, parts and headers.
pub const MAX_CLIPBOARD_SET: usize = 2 * MAX_CLIPBOARD_TEXT + MAX_CLIPBOARD_PNG + 64;
pub const PART_HEADER_LEN: usize = 5;
const MAX_PARTS: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ClipboardFormat {
    /// UTF-8 text.
    Text = 1,
    /// HTML as UTF-8 text.
    Html = 2,
    Png = 3,
}

impl ClipboardFormat {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Text),
            2 => Some(Self::Html),
            3 => Some(Self::Png),
            _ => None,
        }
    }

    /// The most bytes of a part of this format.
    pub fn limit(self) -> usize {
        match self {
            Self::Text | Self::Html => MAX_CLIPBOARD_TEXT,
            Self::Png => MAX_CLIPBOARD_PNG,
        }
    }

    fn is_text(self) -> bool {
        matches!(self, Self::Text | Self::Html)
    }
}

/// Where the X data came from: the clipboard (a copy command) or the selection by mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ClipboardSource {
    Clipboard = 0,
    Primary = 1,
}

/// The parts of a message that `decode_set` or `decode_set_in` checked. Iterating gives each
/// format with its bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipboardParts<'a> {
    rest: &'a [u8],
    left: u8,
}

impl<'a> ClipboardParts<'a> {
    pub fn len(&self) -> usize {
        usize::from(self.left)
    }

    pub fn is_empty(&self) -> bool {
        self.left == 0
    }
}

impl<'a> Iterator for ClipboardParts<'a> {
    type Item = (ClipboardFormat, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        if self.left == 0 {
            return None;
        }
        // `validate` checked every part, so these reads cannot go out of range.
        let format = ClipboardFormat::from_u8(self.rest[0])?;
        let length = u32::from_le_bytes([self.rest[1], self.rest[2], self.rest[3], self.rest[4]]);
        let end = PART_HEADER_LEN + length as usize;
        let data = &self.rest[PART_HEADER_LEN..end];
        self.rest = &self.rest[end..];
        self.left -= 1;
        Some((format, data))
    }
}

fn validate(count: u8, parts: &[u8]) -> Result<ClipboardParts<'_>, DecodeError> {
    if count > MAX_PARTS {
        return Err(DecodeError::BadValue);
    }
    let mut rest = parts;
    let mut seen = 0u8;
    for _ in 0..count {
        let header = rest.get(..PART_HEADER_LEN).ok_or(DecodeError::Truncated)?;
        let format = ClipboardFormat::from_u8(header[0]).ok_or(DecodeError::BadValue)?;
        let bit = 1u8 << (header[0] - 1);
        if seen & bit != 0 {
            return Err(DecodeError::BadValue);
        }
        seen |= bit;
        let length = u32::from_le_bytes([header[1], header[2], header[3], header[4]]) as usize;
        if length > format.limit() {
            return Err(DecodeError::TooLarge);
        }
        let data = rest
            .get(PART_HEADER_LEN..PART_HEADER_LEN + length)
            .ok_or(DecodeError::Truncated)?;
        if format.is_text() && core::str::from_utf8(data).is_err() {
            return Err(DecodeError::BadValue);
        }
        rest = &rest[PART_HEADER_LEN + length..];
    }
    if !rest.is_empty() {
        return Err(DecodeError::BadValue);
    }
    Ok(ClipboardParts {
        rest: parts,
        left: count,
    })
}

/// A set that X sends to the page: `body` is what follows the kind byte.
pub(crate) fn decode_set(body: &[u8]) -> Result<Message<'_>, DecodeError> {
    let [source, count, parts @ ..] = body else {
        return Err(DecodeError::Truncated);
    };
    let source = match source {
        0 => ClipboardSource::Clipboard,
        1 => ClipboardSource::Primary,
        _ => return Err(DecodeError::BadValue),
    };
    Ok(Message::ClipboardSet {
        source,
        parts: validate(*count, parts)?,
    })
}

/// A set that a page sends to X, whole message with its kind byte.
pub fn decode_clipboard_set_in(buf: &[u8]) -> Result<ClipboardParts<'_>, DecodeError> {
    let (&kind, body) = buf.split_first().ok_or(DecodeError::Empty)?;
    if kind != KIND_CLIPBOARD_SET_IN {
        return Err(DecodeError::UnknownKind(kind));
    }
    if buf.len() > MAX_CLIPBOARD_SET {
        return Err(DecodeError::TooLarge);
    }
    let [count, parts @ ..] = body else {
        return Err(DecodeError::Truncated);
    };
    validate(*count, parts)
}

pub fn encode_set_header(source: ClipboardSource, count: u8) -> [u8; 3] {
    [KIND_CLIPBOARD_SET, source as u8, count]
}

pub fn encode_set_in_header(count: u8) -> [u8; 2] {
    [KIND_CLIPBOARD_SET_IN, count]
}

/// The header of a part of `length` bytes. A length above `u32::MAX` cannot be a part.
pub fn encode_part_header(format: ClipboardFormat, length: usize) -> [u8; PART_HEADER_LEN] {
    let length = u32::try_from(length).unwrap_or(u32::MAX).to_le_bytes();
    [format as u8, length[0], length[1], length[2], length[3]]
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    fn part(format: ClipboardFormat, data: &[u8]) -> Vec<u8> {
        let mut out = encode_part_header(format, data.len()).to_vec();
        out.extend_from_slice(data);
        out
    }

    fn set_in(parts: &[Vec<u8>]) -> Vec<u8> {
        let mut out = encode_set_in_header(parts.len() as u8).to_vec();
        for part in parts {
            out.extend_from_slice(part);
        }
        out
    }

    #[test]
    fn a_page_set_with_three_formats_decodes_in_order() {
        let png = [0x89, b'P', b'N', b'G'];
        let message = set_in(&[
            part(ClipboardFormat::Text, "hola ñ".as_bytes()),
            part(ClipboardFormat::Html, b"<b>hola</b>"),
            part(ClipboardFormat::Png, &png),
        ]);
        let parts = decode_clipboard_set_in(&message).unwrap();
        assert_eq!(parts.len(), 3);
        let found: Vec<_> = parts.collect();
        assert_eq!(
            found,
            [
                (ClipboardFormat::Text, "hola ñ".as_bytes()),
                (ClipboardFormat::Html, &b"<b>hola</b>"[..]),
                (ClipboardFormat::Png, &png[..]),
            ]
        );
    }

    #[test]
    fn an_empty_set_means_nothing_to_paste() {
        let message = encode_set_in_header(0);
        let parts = decode_clipboard_set_in(&message).unwrap();
        assert!(parts.is_empty());
    }

    #[test]
    fn a_set_for_the_page_carries_its_source() {
        let mut message = encode_set_header(ClipboardSource::Primary, 1).to_vec();
        message.extend_from_slice(&part(ClipboardFormat::Text, b"selected"));
        let Ok(Message::ClipboardSet { source, parts }) = crate::decode(&message) else {
            panic!("not decoded");
        };
        assert_eq!(source, ClipboardSource::Primary);
        assert_eq!(
            parts.collect::<Vec<_>>(),
            [(ClipboardFormat::Text, &b"selected"[..])]
        );
    }

    #[test]
    fn bad_sets_are_refused() {
        let text = part(ClipboardFormat::Text, b"abc");
        // The same format twice.
        assert_eq!(
            decode_clipboard_set_in(&set_in(&[text.clone(), text.clone()])),
            Err(DecodeError::BadValue)
        );
        // Text that is not UTF-8, and HTML that is not.
        for format in [ClipboardFormat::Text, ClipboardFormat::Html] {
            assert_eq!(
                decode_clipboard_set_in(&set_in(&[part(format, &[0xC0])])),
                Err(DecodeError::BadValue)
            );
        }
        // An unknown format, a cut part, bytes after the last part, a wrong kind.
        let mut unknown = text.clone();
        unknown[0] = 9;
        assert_eq!(
            decode_clipboard_set_in(&set_in(&[unknown])),
            Err(DecodeError::BadValue)
        );
        let cut = set_in(std::slice::from_ref(&text));
        assert_eq!(
            decode_clipboard_set_in(&cut[..cut.len() - 1]),
            Err(DecodeError::Truncated)
        );
        let mut extra = set_in(std::slice::from_ref(&text));
        extra.push(0);
        assert_eq!(decode_clipboard_set_in(&extra), Err(DecodeError::BadValue));
        assert_eq!(
            decode_clipboard_set_in(&[1, 0]),
            Err(DecodeError::UnknownKind(1))
        );
        // More parts than formats, and a source that does not exist.
        assert_eq!(
            decode_clipboard_set_in(&[KIND_CLIPBOARD_SET_IN, 4]),
            Err(DecodeError::BadValue)
        );
        assert_eq!(
            crate::decode(&[KIND_CLIPBOARD_SET, 2, 0]),
            Err(DecodeError::BadValue)
        );
    }

    #[test]
    fn a_part_over_its_limit_is_refused_before_its_bytes_are_read() {
        // Only the header: the length alone is enough to refuse it.
        let mut message = encode_set_in_header(1).to_vec();
        message.extend_from_slice(&encode_part_header(
            ClipboardFormat::Text,
            MAX_CLIPBOARD_TEXT + 1,
        ));
        assert_eq!(
            decode_clipboard_set_in(&message),
            Err(DecodeError::TooLarge)
        );
        let mut png = encode_set_in_header(1).to_vec();
        png.extend_from_slice(&encode_part_header(
            ClipboardFormat::Png,
            MAX_CLIPBOARD_PNG + 1,
        ));
        assert_eq!(decode_clipboard_set_in(&png), Err(DecodeError::TooLarge));
        // A PNG may be bigger than the text limit.
        let mut ok = encode_set_in_header(1).to_vec();
        ok.extend_from_slice(&part(
            ClipboardFormat::Png,
            &std::vec![0u8; MAX_CLIPBOARD_TEXT + 1],
        ));
        assert!(decode_clipboard_set_in(&ok).is_ok());
    }
}
