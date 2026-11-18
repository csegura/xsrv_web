// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

use crate::DecodeError;

const KIND_POINTER: u8 = 16;
const KIND_BUTTON: u8 = 17;
const KIND_KEY: u8 = 18;
const KIND_POINTER_LEFT: u8 = 19;
const KIND_RELEASE_ALL: u8 = 20;
const KIND_LOCKS: u8 = 21;
const KIND_RESIZE: u8 = 22;
// 23 is the clipboard text from the page (`KIND_CLIPBOARD_IN`).
const KIND_LAYOUT: u8 = 24;
const LOCK_CAPS: u8 = 1;
const LOCK_NUM: u8 = 2;

pub const MAX_INPUT_LEN: usize = 5;
pub const MIN_KEYCODE: u8 = 8;
pub const MAX_BUTTON: u8 = 7;

/// What the page tells the server. The server turns it into X11 events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Pointer {
        x: u16,
        y: u16,
    },
    Button {
        button: u8,
        pressed: bool,
    },
    Key {
        keycode: u8,
        pressed: bool,
    },
    PointerLeft,
    /// The page lost the focus or closed. Every key and button that is down goes up.
    ReleaseAll,
    /// The lock keys of the user's keyboard. The page knows them; the server cannot guess.
    Locks {
        caps: bool,
        num: bool,
    },
    /// The page asks for a screen of this size. The server checks the size.
    Resize {
        width: u16,
        height: u16,
    },
    /// The page chooses the keyboard layout of the session: an index into `LAYOUTS`.
    Layout {
        layout: u8,
    },
}

/// Returns the bytes and how many of them are used.
pub fn encode_input(input: Input) -> ([u8; MAX_INPUT_LEN], usize) {
    let mut out = [0; MAX_INPUT_LEN];
    match input {
        Input::Pointer { x, y } => {
            out[0] = KIND_POINTER;
            out[1..3].copy_from_slice(&x.to_le_bytes());
            out[3..5].copy_from_slice(&y.to_le_bytes());
            (out, 5)
        }
        Input::Button { button, pressed } => {
            out[..3].copy_from_slice(&[KIND_BUTTON, button, pressed.into()]);
            (out, 3)
        }
        Input::Key { keycode, pressed } => {
            out[..3].copy_from_slice(&[KIND_KEY, keycode, pressed.into()]);
            (out, 3)
        }
        Input::PointerLeft => {
            out[0] = KIND_POINTER_LEFT;
            (out, 1)
        }
        Input::ReleaseAll => {
            out[0] = KIND_RELEASE_ALL;
            (out, 1)
        }
        Input::Resize { width, height } => {
            out[0] = KIND_RESIZE;
            out[1..3].copy_from_slice(&width.to_le_bytes());
            out[3..5].copy_from_slice(&height.to_le_bytes());
            (out, 5)
        }
        Input::Locks { caps, num } => {
            let flags = (u8::from(caps) * LOCK_CAPS) | (u8::from(num) * LOCK_NUM);
            out[..2].copy_from_slice(&[KIND_LOCKS, flags]);
            (out, 2)
        }
        Input::Layout { layout } => {
            out[..2].copy_from_slice(&[KIND_LAYOUT, layout]);
            (out, 2)
        }
    }
}

pub fn decode_input(buf: &[u8]) -> Result<Input, DecodeError> {
    let (&kind, body) = buf.split_first().ok_or(DecodeError::Empty)?;
    match (kind, body) {
        (KIND_POINTER, [x0, x1, y0, y1]) => Ok(Input::Pointer {
            x: u16::from_le_bytes([*x0, *x1]),
            y: u16::from_le_bytes([*y0, *y1]),
        }),
        (KIND_BUTTON, [button, pressed]) => {
            if !(1..=MAX_BUTTON).contains(button) {
                return Err(DecodeError::BadValue);
            }
            Ok(Input::Button {
                button: *button,
                pressed: decode_flag(*pressed)?,
            })
        }
        (KIND_KEY, [keycode, pressed]) => {
            if *keycode < MIN_KEYCODE {
                return Err(DecodeError::BadValue);
            }
            Ok(Input::Key {
                keycode: *keycode,
                pressed: decode_flag(*pressed)?,
            })
        }
        (KIND_POINTER_LEFT, []) => Ok(Input::PointerLeft),
        (KIND_RELEASE_ALL, []) => Ok(Input::ReleaseAll),
        (KIND_RESIZE, [w0, w1, h0, h1]) => Ok(Input::Resize {
            width: u16::from_le_bytes([*w0, *w1]),
            height: u16::from_le_bytes([*h0, *h1]),
        }),
        (KIND_LOCKS, [flags]) => {
            if flags & !(LOCK_CAPS | LOCK_NUM) != 0 {
                return Err(DecodeError::BadValue);
            }
            Ok(Input::Locks {
                caps: flags & LOCK_CAPS != 0,
                num: flags & LOCK_NUM != 0,
            })
        }
        (KIND_LAYOUT, [layout]) => match usize::from(*layout) < crate::LAYOUTS.len() {
            true => Ok(Input::Layout { layout: *layout }),
            false => Err(DecodeError::BadValue),
        },
        (
            KIND_POINTER | KIND_BUTTON | KIND_KEY | KIND_POINTER_LEFT | KIND_RELEASE_ALL
            | KIND_LOCKS | KIND_RESIZE | KIND_LAYOUT,
            _,
        ) => Err(DecodeError::Truncated),
        (other, _) => Err(DecodeError::UnknownKind(other)),
    }
}

fn decode_flag(byte: u8) -> Result<bool, DecodeError> {
    match byte {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(DecodeError::BadValue),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(input: Input) {
        let (bytes, len) = encode_input(input);
        assert_eq!(decode_input(&bytes[..len]), Ok(input));
    }

    #[test]
    fn every_message_round_trips() {
        round_trip(Input::Pointer { x: 639, y: 479 });
        round_trip(Input::Button {
            button: 5,
            pressed: true,
        });
        round_trip(Input::Key {
            keycode: 255,
            pressed: false,
        });
        round_trip(Input::PointerLeft);
        round_trip(Input::ReleaseAll);
        round_trip(Input::Resize {
            width: 1920,
            height: 1080,
        });
        round_trip(Input::Locks {
            caps: true,
            num: false,
        });
        round_trip(Input::Locks {
            caps: false,
            num: true,
        });
        round_trip(Input::Layout { layout: 3 });
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(decode_input(&[]), Err(DecodeError::Empty));
        assert_eq!(decode_input(&[99]), Err(DecodeError::UnknownKind(99)));
        assert_eq!(
            decode_input(&[KIND_POINTER, 1, 2]),
            Err(DecodeError::Truncated)
        );
        assert_eq!(
            decode_input(&[KIND_POINTER_LEFT, 0]),
            Err(DecodeError::Truncated)
        );
        assert_eq!(
            decode_input(&[KIND_BUTTON, 0, 1]),
            Err(DecodeError::BadValue)
        );
        assert_eq!(
            decode_input(&[KIND_BUTTON, 8, 1]),
            Err(DecodeError::BadValue)
        );
        assert_eq!(
            decode_input(&[KIND_BUTTON, 1, 2]),
            Err(DecodeError::BadValue)
        );
        assert_eq!(decode_input(&[KIND_KEY, 7, 1]), Err(DecodeError::BadValue));
        assert_eq!(decode_input(&[KIND_LOCKS, 4]), Err(DecodeError::BadValue));
        assert_eq!(decode_input(&[KIND_LOCKS]), Err(DecodeError::Truncated));
        assert_eq!(decode_input(&[KIND_LAYOUT, 4]), Err(DecodeError::BadValue));
        assert_eq!(decode_input(&[KIND_LAYOUT]), Err(DecodeError::Truncated));
        assert_eq!(
            decode_input(&[KIND_RELEASE_ALL, 1]),
            Err(DecodeError::Truncated)
        );
    }
}
