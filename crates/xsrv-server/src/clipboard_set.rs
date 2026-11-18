// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! What the clipboard holds: text, HTML and a PNG image. The page and the X clients each give
//! a set, and the server passes it on whole.
use wire::{
    encode_part_header, encode_set_header, ClipboardFormat, ClipboardParts, ClipboardSource,
    MAX_CLIPBOARD_SET,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClipboardData {
    pub text: Option<String>,
    pub html: Option<String>,
    pub png: Option<Vec<u8>>,
}

impl ClipboardData {
    pub fn text_only(text: String) -> Self {
        Self {
            text: Some(text),
            ..Self::default()
        }
    }

    /// The parts of a message that `wire` checked.
    pub fn from_parts(parts: ClipboardParts<'_>) -> Self {
        let mut data = Self::default();
        for (format, bytes) in parts {
            match format {
                ClipboardFormat::Text => data.text = String::from_utf8(bytes.to_vec()).ok(),
                ClipboardFormat::Html => data.html = String::from_utf8(bytes.to_vec()).ok(),
                ClipboardFormat::Png => data.png = Some(bytes.to_vec()),
            }
        }
        data
    }

    /// True when there is nothing to paste: no text (an empty text counts), no HTML, no image.
    pub fn is_empty(&self) -> bool {
        self.text.as_deref().is_none_or(str::is_empty)
            && self.html.as_deref().is_none_or(str::is_empty)
            && self.png.as_deref().is_none_or(<[u8]>::is_empty)
    }

    /// The message for the pages. `None` when it would be bigger than a message may be.
    pub fn encode_for_page(&self, source: ClipboardSource) -> Option<Vec<u8>> {
        let parts: Vec<(ClipboardFormat, &[u8])> = [
            (
                ClipboardFormat::Text,
                self.text.as_deref().map(str::as_bytes),
            ),
            (
                ClipboardFormat::Html,
                self.html.as_deref().map(str::as_bytes),
            ),
            (ClipboardFormat::Png, self.png.as_deref()),
        ]
        .into_iter()
        .filter_map(|(format, bytes)| bytes.filter(|b| !b.is_empty()).map(|b| (format, b)))
        .collect();
        let mut out = encode_set_header(source, parts.len() as u8).to_vec();
        for (format, bytes) in parts {
            out.extend_from_slice(&encode_part_header(format, bytes.len()));
            out.extend_from_slice(bytes);
        }
        (out.len() <= MAX_CLIPBOARD_SET).then_some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wire::{decode, Message};

    fn parts_of(message: &[u8]) -> (ClipboardSource, Vec<(ClipboardFormat, Vec<u8>)>) {
        let Ok(Message::ClipboardSet { source, parts }) = decode(message) else {
            panic!("not a clipboard set");
        };
        (source, parts.map(|(f, b)| (f, b.to_vec())).collect())
    }

    #[test]
    fn a_set_goes_to_the_page_with_only_the_formats_it_has() {
        let data = ClipboardData {
            text: Some("hola".into()),
            html: None,
            png: Some(vec![1, 2, 3]),
        };
        let message = data.encode_for_page(ClipboardSource::Clipboard).unwrap();
        assert_eq!(
            parts_of(&message),
            (
                ClipboardSource::Clipboard,
                vec![
                    (ClipboardFormat::Text, b"hola".to_vec()),
                    (ClipboardFormat::Png, vec![1, 2, 3])
                ]
            )
        );
    }

    #[test]
    fn empty_formats_are_left_out_and_an_empty_set_is_empty() {
        let data = ClipboardData {
            text: Some(String::new()),
            html: Some(String::new()),
            png: Some(Vec::new()),
        };
        assert!(data.is_empty());
        assert!(ClipboardData::default().is_empty());
        let message = data.encode_for_page(ClipboardSource::Primary).unwrap();
        assert_eq!(parts_of(&message), (ClipboardSource::Primary, vec![]));
        assert!(!ClipboardData::text_only("a".into()).is_empty());
    }
}
