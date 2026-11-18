// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

use super::codec::Truncated;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XError {
    Request,
    Value(u32),
    Window(u32),
    Drawable(u32),
    Atom(u32),
    Match,
    IdChoice(u32),
    Alloc,
    Length,
    Access,
    /// An error that an extension defines: its code and the bad value.
    Extension(u8, u32),
    Name,
    Implementation,
    Font(u32),
    GContext(u32),
    Pixmap(u32),
    Colormap(u32),
    Cursor(u32),
}

impl XError {
    pub fn code(self) -> u8 {
        match self {
            Self::Extension(code, _) => code,
            Self::Request => 1,
            Self::Value(_) => 2,
            Self::Window(_) => 3,
            Self::Atom(_) => 5,
            Self::Match => 8,
            Self::Drawable(_) => 9,
            Self::Pixmap(_) => 4,
            Self::Cursor(_) => 6,
            Self::Font(_) => 7,
            Self::Name => 15,
            Self::GContext(_) => 13,
            Self::Implementation => 17,
            Self::Access => 10,
            Self::Colormap(_) => 12,
            Self::Alloc => 11,
            Self::IdChoice(_) => 14,
            Self::Length => 16,
        }
    }

    pub fn bad_value(self) -> u32 {
        match self {
            Self::Value(v)
            | Self::Window(v)
            | Self::Drawable(v)
            | Self::Atom(v)
            | Self::Pixmap(v)
            | Self::Colormap(v)
            | Self::Cursor(v)
            | Self::Font(v)
            | Self::GContext(v)
            | Self::Extension(_, v)
            | Self::IdChoice(v) => v,
            _ => 0,
        }
    }
}

impl From<Truncated> for XError {
    fn from(_: Truncated) -> Self {
        Self::Length
    }
}
