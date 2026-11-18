// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

/// The pointer shapes that the page can show. The server maps X11 cursors to these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum CursorShape {
    #[default]
    Default = 0,
    Text = 1,
    Wait = 2,
    Move = 3,
    Pointer = 4,
    Crosshair = 5,
    ResizeEw = 6,
    ResizeNs = 7,
    ResizeNesw = 8,
    ResizeNwse = 9,
    NotAllowed = 10,
    Help = 11,
    /// No pointer at all (XFixesHideCursor).
    Hidden = 12,
}

impl CursorShape {
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Default,
            1 => Self::Text,
            2 => Self::Wait,
            3 => Self::Move,
            4 => Self::Pointer,
            5 => Self::Crosshair,
            6 => Self::ResizeEw,
            7 => Self::ResizeNs,
            8 => Self::ResizeNesw,
            9 => Self::ResizeNwse,
            10 => Self::NotAllowed,
            11 => Self::Help,
            12 => Self::Hidden,
            _ => return None,
        })
    }

    /// The value of the CSS `cursor` property.
    pub fn css(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Text => "text",
            Self::Wait => "wait",
            Self::Move => "move",
            Self::Pointer => "pointer",
            Self::Crosshair => "crosshair",
            Self::ResizeEw => "ew-resize",
            Self::ResizeNs => "ns-resize",
            Self::ResizeNesw => "nesw-resize",
            Self::ResizeNwse => "nwse-resize",
            Self::NotAllowed => "not-allowed",
            Self::Help => "help",
            Self::Hidden => "none",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shape_round_trips_and_has_a_css_name() {
        for value in 0..=12 {
            let shape = CursorShape::from_u8(value).unwrap();
            assert_eq!(shape as u8, value);
            assert!(!shape.css().is_empty());
        }
        assert_eq!(CursorShape::from_u8(13), None);
    }
}
