// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

use std::sync::atomic::{AtomicBool, Ordering};

use super::codec::{Endian, Writer};
use super::screen::ROOT_WINDOW;

pub const EXPOSURE: u32 = 0x8000;
pub const BUTTON_PRESS: u32 = 0x4;
pub const RESIZE_REDIRECT: u32 = 0x4_0000;
pub const SUBSTRUCTURE_REDIRECT: u32 = 0x10_0000;
pub const KEY_PRESS: u32 = 0x1;
pub const KEY_RELEASE: u32 = 0x2;
pub const BUTTON_RELEASE: u32 = 0x8;
pub const ENTER_WINDOW: u32 = 0x10;
pub const LEAVE_WINDOW: u32 = 0x20;
pub const POINTER_MOTION: u32 = 0x40;
pub const BUTTON_MOTION: u32 = 0x2000;
pub const BUTTON1_MOTION: u32 = 0x100;
pub const FOCUS_CHANGE: u32 = 0x20_0000;
pub const STRUCTURE_NOTIFY: u32 = 0x2_0000;
pub const SUBSTRUCTURE_NOTIFY: u32 = 0x8_0000;
pub const PROPERTY_CHANGE: u32 = 0x40_0000;

const EVENT_LEN: usize = 32;

/// The fields that key, button, motion and crossing events share.
#[derive(Debug, Clone, Copy)]
pub struct PointerEvent {
    pub detail: u8,
    pub time: u32,
    pub root: u32,
    pub event: u32,
    pub child: u32,
    pub root_x: i16,
    pub root_y: i16,
    pub event_x: i16,
    pub event_y: i16,
    pub state: u16,
}

pub enum Event {
    /// An event that a client sent with SendEvent, or that the server built in little-endian order.
    Raw {
        bytes: [u8; 32],
        from: Endian,
    },
    Key {
        press: bool,
        fields: PointerEvent,
    },
    Button {
        press: bool,
        fields: PointerEvent,
    },
    Motion {
        fields: PointerEvent,
    },
    /// `detail` of `fields` is the notify detail (Ancestor, Virtual, Inferior, Nonlinear ...).
    Crossing {
        enter: bool,
        fields: PointerEvent,
        mode: u8,
        /// The window is the focus window or inside it.
        focus: bool,
    },
    ShmCompletion {
        code: u8,
        drawable: u32,
        minor: u16,
        major: u8,
        seg: u32,
        offset: u32,
    },
    XfixesSelectionNotify {
        code: u8,
        subtype: u8,
        window: u32,
        owner: u32,
        selection: u32,
        time: u32,
        selection_time: u32,
    },
    RandrScreenChange {
        code: u8,
        time: u32,
        config_time: u32,
        width: u16,
        height: u16,
        mm_width: u16,
        mm_height: u16,
    },
    RandrCrtcChange {
        code: u8,
        sub_code: u8,
        time: u32,
        crtc: u32,
        mode: u32,
        width: u16,
        height: u16,
    },
    ShapeNotify {
        code: u8,
        kind: u8,
        window: u32,
        x: i16,
        y: i16,
        width: u16,
        height: u16,
        time: u32,
        shaped: bool,
    },
    MapRequest {
        parent: u32,
        window: u32,
    },
    ConfigureRequest {
        stack_mode: u8,
        parent: u32,
        window: u32,
        sibling: u32,
        x: i16,
        y: i16,
        width: u16,
        height: u16,
        border: u16,
        value_mask: u16,
    },
    ReparentNotify {
        event: u32,
        window: u32,
        parent: u32,
        x: i16,
        y: i16,
        override_redirect: bool,
    },
    Focus {
        focus_in: bool,
        event: u32,
        detail: u8,
        mode: u8,
    },
    CreateNotify {
        parent: u32,
        window: u32,
        x: i16,
        y: i16,
        width: u16,
        height: u16,
        border: u16,
        override_redirect: bool,
    },
    DestroyNotify {
        event: u32,
        window: u32,
    },
    UnmapNotify {
        event: u32,
        window: u32,
    },
    MapNotify {
        event: u32,
        window: u32,
        override_redirect: bool,
    },
    ConfigureNotify {
        event: u32,
        window: u32,
        above_sibling: u32,
        x: i16,
        y: i16,
        width: u16,
        height: u16,
        border: u16,
        override_redirect: bool,
    },
    Expose {
        window: u32,
        x: u16,
        y: u16,
        width: u16,
        height: u16,
    },
    PropertyNotify {
        window: u32,
        atom: u32,
        time: u32,
        deleted: bool,
    },
    GraphicsExpose {
        drawable: u32,
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        count: u16,
        major_opcode: u8,
    },
    NoExpose {
        drawable: u32,
        major_opcode: u8,
    },
    /// The keyboard or modifier mapping changed. Every client gets it, without a mask.
    MappingNotify {
        request: MappingRequest,
        first_keycode: u8,
        count: u8,
    },
}

#[derive(Debug, Clone, Copy)]
pub enum MappingRequest {
    Modifier = 0,
    Keyboard = 1,
}

impl Event {
    pub fn encode(&self, endian: Endian, seq: u16) -> Vec<u8> {
        let mut w = Writer::new(endian);
        match *self {
            Self::Raw { bytes, from } => return forward_event(bytes, from, endian, seq),
            Self::CreateNotify {
                parent,
                window,
                x,
                y,
                width,
                height,
                border,
                override_redirect,
            } => {
                head(&mut w, 16, seq);
                w.u32(parent);
                w.u32(window);
                w.i16(x);
                w.i16(y);
                w.u16(width);
                w.u16(height);
                w.u16(border);
                w.u8(override_redirect.into());
            }
            Self::DestroyNotify { event, window } => {
                head(&mut w, 17, seq);
                w.u32(event);
                w.u32(window);
            }
            Self::UnmapNotify { event, window } => {
                head(&mut w, 18, seq);
                w.u32(event);
                w.u32(window);
                w.u8(0);
            }
            Self::MapNotify {
                event,
                window,
                override_redirect,
            } => {
                head(&mut w, 19, seq);
                w.u32(event);
                w.u32(window);
                w.u8(override_redirect.into());
            }
            Self::ConfigureNotify {
                event,
                window,
                above_sibling,
                x,
                y,
                width,
                height,
                border,
                override_redirect,
            } => {
                head(&mut w, 22, seq);
                w.u32(event);
                w.u32(window);
                w.u32(above_sibling);
                w.i16(x);
                w.i16(y);
                w.u16(width);
                w.u16(height);
                w.u16(border);
                w.u8(override_redirect.into());
            }
            Self::Expose {
                window,
                x,
                y,
                width,
                height,
            } => {
                head(&mut w, 12, seq);
                w.u32(window);
                w.u16(x);
                w.u16(y);
                w.u16(width);
                w.u16(height);
                w.u16(0);
            }
            Self::Key { press, fields } => {
                pointer_event(&mut w, if press { 2 } else { 3 }, seq, &fields);
            }
            Self::Button { press, fields } => {
                pointer_event(&mut w, if press { 4 } else { 5 }, seq, &fields);
            }
            Self::Motion { fields } => pointer_event(&mut w, 6, seq, &fields),
            Self::Crossing {
                enter,
                fields,
                mode,
                focus,
            } => {
                pointer_event(&mut w, if enter { 7 } else { 8 }, seq, &fields);
                // Replace the same-screen byte and the padding byte by `mode` and the flags.
                let mut bytes = w.finish();
                bytes.truncate(EVENT_LEN - 2);
                bytes.push(mode);
                bytes.push(SAME_SCREEN | u8::from(focus));
                return bytes;
            }
            Self::ShmCompletion {
                code,
                drawable,
                minor,
                major,
                seg,
                offset,
            } => {
                w.u8(code);
                w.u8(0);
                w.u16(seq);
                w.u32(drawable);
                w.u16(minor);
                w.u8(major);
                w.u8(0);
                w.u32(seg);
                w.u32(offset);
            }
            Self::XfixesSelectionNotify {
                code,
                subtype,
                window,
                owner,
                selection,
                time,
                selection_time,
            } => {
                w.u8(code);
                w.u8(subtype);
                w.u16(seq);
                w.u32(window);
                w.u32(owner);
                w.u32(selection);
                w.u32(time);
                w.u32(selection_time);
            }
            Self::RandrScreenChange {
                code,
                time,
                config_time,
                width,
                height,
                mm_width,
                mm_height,
            } => {
                w.u8(code);
                w.u8(1);
                w.u16(seq);
                w.u32(time);
                w.u32(config_time);
                w.u32(ROOT_WINDOW);
                w.u32(ROOT_WINDOW);
                w.u16(0);
                w.u16(0);
                w.u16(width);
                w.u16(height);
                w.u16(mm_width);
                w.u16(mm_height);
            }
            Self::RandrCrtcChange {
                code,
                sub_code,
                time,
                crtc,
                mode,
                width,
                height,
            } => {
                w.u8(code);
                w.u8(sub_code);
                w.u16(seq);
                w.u32(time);
                w.u32(ROOT_WINDOW);
                w.u32(crtc);
                w.u32(mode);
                w.u16(1);
                w.u16(0);
                w.i16(0);
                w.i16(0);
                w.u16(width);
                w.u16(height);
            }
            Self::ShapeNotify {
                code,
                kind,
                window,
                x,
                y,
                width,
                height,
                time,
                shaped,
            } => {
                w.u8(code);
                w.u8(kind);
                w.u16(seq);
                w.u32(window);
                w.i16(x);
                w.i16(y);
                w.u16(width);
                w.u16(height);
                w.u32(time);
                w.u8(shaped.into());
            }
            Self::MapRequest { parent, window } => {
                head(&mut w, 20, seq);
                w.u32(parent);
                w.u32(window);
            }
            Self::ConfigureRequest {
                stack_mode,
                parent,
                window,
                sibling,
                x,
                y,
                width,
                height,
                border,
                value_mask,
            } => {
                w.u8(23);
                w.u8(stack_mode);
                w.u16(seq);
                w.u32(parent);
                w.u32(window);
                w.u32(sibling);
                w.i16(x);
                w.i16(y);
                w.u16(width);
                w.u16(height);
                w.u16(border);
                w.u16(value_mask);
            }
            Self::ReparentNotify {
                event,
                window,
                parent,
                x,
                y,
                override_redirect,
            } => {
                head(&mut w, 21, seq);
                w.u32(event);
                w.u32(window);
                w.u32(parent);
                w.i16(x);
                w.i16(y);
                w.u8(override_redirect.into());
            }
            Self::Focus {
                focus_in,
                event,
                detail,
                mode,
            } => {
                w.u8(if focus_in { 9 } else { 10 });
                w.u8(detail);
                w.u16(seq);
                w.u32(event);
                w.u8(mode);
            }
            Self::GraphicsExpose {
                drawable,
                x,
                y,
                width,
                height,
                count,
                major_opcode,
            } => {
                head(&mut w, 13, seq);
                w.u32(drawable);
                w.u16(x);
                w.u16(y);
                w.u16(width);
                w.u16(height);
                w.u16(0);
                w.u16(count);
                w.u8(major_opcode);
            }
            Self::NoExpose {
                drawable,
                major_opcode,
            } => {
                head(&mut w, 14, seq);
                w.u32(drawable);
                w.u16(0);
                w.u8(major_opcode);
            }
            Self::MappingNotify {
                request,
                first_keycode,
                count,
            } => {
                head(&mut w, 34, seq);
                w.u8(request as u8);
                w.u8(first_keycode);
                w.u8(count);
            }
            Self::PropertyNotify {
                window,
                atom,
                time,
                deleted,
            } => {
                head(&mut w, 28, seq);
                w.u32(window);
                w.u32(atom);
                w.u32(time);
                w.u8(deleted.into());
            }
        }
        let missing = EVENT_LEN - w.len();
        w.zeros(missing);
        w.finish()
    }
}

const SAME_SCREEN: u8 = 2;

/// Copies a sent event for another client. The client has its own byte order and sequence number.
/// An event type that cannot be swapped is dropped when the byte orders differ (an empty result).
fn forward_event(mut bytes: [u8; 32], from: Endian, to: Endian, seq: u16) -> Vec<u8> {
    if from != to && !swap_fields(&mut bytes) {
        static WARNED: AtomicBool = AtomicBool::new(false);
        if !WARNED.swap(true, Ordering::Relaxed) {
            eprintln!(
                "xsrv-web: a sent event of type {} was dropped, because the clients have another byte order",
                bytes[0] & 0x7F
            );
        }
        return Vec::new();
    }
    let seq = match to {
        Endian::Little => seq.to_le_bytes(),
        Endian::Big => seq.to_be_bytes(),
    };
    bytes[2..4].copy_from_slice(&seq);
    bytes.to_vec()
}

fn swap_u32s(bytes: &mut [u8], start: usize, count: usize) {
    for field in bytes[start..start + count * 4]
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
    {
        field.reverse();
    }
}

fn swap_u16s(bytes: &mut [u8], start: usize, count: usize) {
    for field in bytes[start..start + count * 2]
        .as_chunks_mut::<2>()
        .0
        .iter_mut()
    {
        field.reverse();
    }
}

/// Swaps the fields of the event types that clients send to each other: ClientMessage, the
/// selection events, Expose and VisibilityNotify. Returns false for any other type.
fn swap_fields(bytes: &mut [u8; 32]) -> bool {
    match bytes[0] & 0x7F {
        29 => swap_u32s(bytes, 4, 3),
        30 => swap_u32s(bytes, 4, 6),
        31 => swap_u32s(bytes, 4, 5),
        // Expose: window, then x, y, width, height and count as 16-bit fields.
        12 => {
            swap_u32s(bytes, 4, 1);
            swap_u16s(bytes, 8, 4);
            swap_u16s(bytes, 16, 1);
        }
        // VisibilityNotify: only the window is wider than a byte.
        15 => swap_u32s(bytes, 4, 1),
        33 => {
            swap_u32s(bytes, 4, 2);
            match bytes[1] {
                8 => {}
                16 => swap_u16s(bytes, 12, 10),
                32 => swap_u32s(bytes, 12, 5),
                _ => return false,
            }
        }
        _ => return false,
    }
    true
}

fn pointer_event(w: &mut Writer, code: u8, seq: u16, fields: &PointerEvent) {
    w.u8(code);
    w.u8(fields.detail);
    w.u16(seq);
    w.u32(fields.time);
    w.u32(fields.root);
    w.u32(fields.event);
    w.u32(fields.child);
    w.i16(fields.root_x);
    w.i16(fields.root_y);
    w.i16(fields.event_x);
    w.i16(fields.event_y);
    w.u16(fields.state);
    w.u8(1);
}

fn head(w: &mut Writer, code: u8, seq: u16) {
    w.u8(code);
    w.u8(0);
    w.u16(seq);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_event_is_32_bytes() {
        let events = [
            Event::CreateNotify {
                parent: 1,
                window: 2,
                x: -1,
                y: 2,
                width: 3,
                height: 4,
                border: 0,
                override_redirect: false,
            },
            Event::DestroyNotify {
                event: 1,
                window: 2,
            },
            Event::UnmapNotify {
                event: 1,
                window: 2,
            },
            Event::MapNotify {
                event: 1,
                window: 2,
                override_redirect: true,
            },
            Event::ConfigureNotify {
                event: 1,
                window: 2,
                above_sibling: 0,
                x: 1,
                y: 1,
                width: 1,
                height: 1,
                border: 0,
                override_redirect: false,
            },
            Event::Expose {
                window: 1,
                x: 0,
                y: 0,
                width: 2,
                height: 3,
            },
            Event::PropertyNotify {
                window: 1,
                atom: 2,
                time: 3,
                deleted: true,
            },
            Event::GraphicsExpose {
                drawable: 1,
                x: 2,
                y: 3,
                width: 4,
                height: 5,
                count: 0,
                major_opcode: 62,
            },
            Event::NoExpose {
                drawable: 1,
                major_opcode: 62,
            },
            Event::MappingNotify {
                request: MappingRequest::Keyboard,
                first_keycode: 8,
                count: 248,
            },
            Event::MapRequest {
                parent: 1,
                window: 2,
            },
            Event::ShmCompletion {
                code: 65,
                drawable: 1,
                minor: 3,
                major: 130,
                seg: 2,
                offset: 0,
            },
            Event::XfixesSelectionNotify {
                code: 66,
                subtype: 0,
                window: 1,
                owner: 2,
                selection: 3,
                time: 4,
                selection_time: 5,
            },
            Event::RandrScreenChange {
                code: 68,
                time: 1,
                config_time: 1,
                width: 5,
                height: 5,
                mm_width: 1,
                mm_height: 1,
            },
            Event::RandrCrtcChange {
                code: 69,
                sub_code: 0,
                time: 1,
                crtc: 2,
                mode: 3,
                width: 5,
                height: 5,
            },
            Event::ShapeNotify {
                code: 64,
                kind: 0,
                window: 1,
                x: 0,
                y: 0,
                width: 5,
                height: 5,
                time: 1,
                shaped: true,
            },
            Event::ConfigureRequest {
                stack_mode: 0,
                parent: 1,
                window: 2,
                sibling: 0,
                x: -1,
                y: 2,
                width: 3,
                height: 4,
                border: 0,
                value_mask: 0x0F,
            },
            Event::ReparentNotify {
                event: 1,
                window: 2,
                parent: 3,
                x: 4,
                y: 5,
                override_redirect: false,
            },
        ];
        for event in events {
            assert_eq!(event.encode(Endian::Little, 7).len(), 32);
            assert_eq!(event.encode(Endian::Big, 7).len(), 32);
        }
    }

    #[test]
    fn a_sent_event_is_swapped_for_another_byte_order() {
        let mut bytes = [0u8; 32];
        bytes[0] = 33 | 0x80;
        bytes[1] = 32;
        bytes[4..8].copy_from_slice(&0x0102_0304u32.to_le_bytes());
        bytes[12..16].copy_from_slice(&0x0A0B_0C0Du32.to_le_bytes());
        let event = Event::Raw {
            bytes,
            from: Endian::Little,
        };
        let same = event.encode(Endian::Little, 0x0102);
        assert_eq!(
            &same[2..4],
            [0x02, 0x01],
            "the sequence number is the receiver's"
        );
        assert_eq!(&same[4..8], 0x0102_0304u32.to_le_bytes());
        let swapped = event.encode(Endian::Big, 0x0102);
        assert_eq!(&swapped[2..4], [0x01, 0x02]);
        assert_eq!(&swapped[4..8], 0x0102_0304u32.to_be_bytes());
        assert_eq!(&swapped[12..16], 0x0A0B_0C0Du32.to_be_bytes());
    }

    #[test]
    fn an_event_that_cannot_be_swapped_is_dropped() {
        let mut bytes = [0u8; 32];
        bytes[0] = 4;
        let event = Event::Raw {
            bytes,
            from: Endian::Little,
        };
        assert_eq!(event.encode(Endian::Little, 1).len(), 32);
        assert!(event.encode(Endian::Big, 1).is_empty());
    }

    #[test]
    fn expose_carries_the_sequence_number() {
        let bytes = Event::Expose {
            window: 5,
            x: 3,
            y: 4,
            width: 10,
            height: 20,
        }
        .encode(Endian::Little, 0x0102);
        assert_eq!(&bytes[..4], [12, 0, 0x02, 0x01]);
        assert_eq!(&bytes[8..12], [3, 0, 4, 0]);
    }
}
