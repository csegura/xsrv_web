// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

use super::codec::Writer;
use super::dispatch::{Reply, Request};
use super::error::XError;
use super::input::{Focus, KeyboardGrab, PassiveButton, PassiveKey, PointerGrab, ANY_MODIFIER};
use super::keymap::{self, KEYCODES_PER_MODIFIER, MAX_KEYCODE, MIN_KEYCODE};
use super::screen::ROOT_WINDOW;
use super::state::State;

const GRAB_SUCCESS: u8 = 0;
const GRAB_ALREADY_GRABBED: u8 = 1;
const GRAB_NOT_VIEWABLE: u8 = 3;
const GRAB_MODE_SYNC: u8 = 0;
const GRAB_MODE_ASYNC: u8 = 1;
const FOCUS_NONE: u32 = 0;
const FOCUS_POINTER_ROOT: u32 = 1;
const MAX_REVERT_TO: u8 = 2;
const POINTER_BUTTONS: u8 = 7;
const MODIFIER_BITS: u16 = 0x00FF;
const MAX_PASSIVE_GRABS: usize = 8192;
/// One client may not use all the passive grabs of the server. A window manager grabs each key
/// binding about four times (with and without NumLock and CapsLock): i3 needs some hundreds.
const MAX_PASSIVE_GRABS_PER_CLIENT: usize = 2048;
const REPLAY_POINTER: u8 = 2;
const REPLAY_KEYBOARD: u8 = 5;

/// A modifier mask holds the eight modifier bits, or AnyModifier. Nothing else.
fn check_modifiers(modifiers: u16) -> Result<(), XError> {
    if modifiers == ANY_MODIFIER || modifiers & !MODIFIER_BITS == 0 {
        Ok(())
    } else {
        Err(XError::Value(modifiers.into()))
    }
}

impl State {
    pub fn get_input_focus(&self, req: &Request) -> Result<Reply, XError> {
        req.reader().end()?;
        let focus = match self.input.focus {
            Focus::None => FOCUS_NONE,
            Focus::PointerRoot => FOCUS_POINTER_ROOT,
            Focus::Window(window) => window,
        };
        let mut w = Writer::new(req.endian);
        w.u32(focus);
        Ok(Reply::new(self.input.revert_to, w))
    }

    pub fn set_input_focus(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let focus = r.u32()?;
        r.skip(4)?;
        r.end()?;
        if req.detail > MAX_REVERT_TO {
            return Err(XError::Value(req.detail.into()));
        }
        let focus = match focus {
            FOCUS_NONE => Focus::None,
            FOCUS_POINTER_ROOT => Focus::PointerRoot,
            window => {
                self.window(window)?;
                if !self.is_viewable(window) {
                    return Err(XError::Match);
                }
                Focus::Window(window)
            }
        };
        self.set_focus(focus, req.detail);
        Ok(())
    }

    pub fn query_pointer(&self, req: &Request) -> Result<Reply, XError> {
        let mut r = req.reader();
        let id = r.u32()?;
        r.end()?;
        self.window(id)?;
        let (x, y) = self.input.pointer;
        let (origin_x, origin_y) = self.window_origin(id).unwrap_or((0, 0));
        let chain = self.chain(self.window_at(x, y));
        let child = match chain.iter().position(|w| *w == id) {
            Some(index) if index > 0 => chain[index - 1],
            _ => 0,
        };
        let mut w = Writer::new(req.endian);
        w.u32(ROOT_WINDOW);
        w.u32(child);
        w.i16(x as i16);
        w.i16(y as i16);
        w.i16((x - origin_x) as i16);
        w.i16((y - origin_y) as i16);
        w.u16(self.input.state());
        Ok(Reply::new(1, w))
    }

    pub fn translate_coordinates(&self, req: &Request) -> Result<Reply, XError> {
        let mut r = req.reader();
        let source = r.u32()?;
        let target = r.u32()?;
        let x = i32::from(r.i16()?);
        let y = i32::from(r.i16()?);
        r.end()?;
        let (source_x, source_y) = self
            .window(source)
            .map(|_| self.window_origin(source))?
            .unwrap_or((0, 0));
        let (target_x, target_y) = self
            .window(target)
            .map(|_| self.window_origin(target))?
            .unwrap_or((0, 0));
        let (root_x, root_y) = (source_x + x, source_y + y);
        let chain = self.chain(self.window_at(root_x, root_y));
        let child = match chain.iter().position(|w| *w == target) {
            Some(index) if index > 0 => chain[index - 1],
            _ => 0,
        };
        let mut w = Writer::new(req.endian);
        w.u32(child);
        w.i16((root_x - target_x) as i16);
        w.i16((root_y - target_y) as i16);
        Ok(Reply::new(1, w))
    }

    pub fn get_keyboard_mapping(&self, req: &Request) -> Result<Reply, XError> {
        let mut r = req.reader();
        let first = r.u8()?;
        let count = r.u8()?;
        r.skip(2)?;
        r.end()?;
        if first < MIN_KEYCODE {
            return Err(XError::Value(first.into()));
        }
        if usize::from(first) + usize::from(count) > usize::from(MAX_KEYCODE) + 1 {
            return Err(XError::Value(count.into()));
        }
        let mut w = Writer::new(req.endian);
        w.zeros(24);
        // Counted in `usize`: the last keycode, 255, plus a count of 1 does not fit in a `u8`.
        let range = usize::from(first)..usize::from(first) + usize::from(count);
        for keycode in range {
            let layout = self.input.layout;
            for keysym in
                &keymap::keysyms(layout, keycode as u8)[..layout.keysyms_per_keycode().into()]
            {
                w.u32(*keysym);
            }
        }
        Ok(Reply::new(self.input.layout.keysyms_per_keycode(), w))
    }

    pub fn get_modifier_mapping(&self, req: &Request) -> Result<Reply, XError> {
        req.reader().end()?;
        let mut w = Writer::new(req.endian);
        w.zeros(24);
        w.bytes(&keymap::modifier_mapping(self.input.layout));
        Ok(Reply::new(KEYCODES_PER_MODIFIER, w))
    }

    pub fn get_pointer_mapping(&self, req: &Request) -> Result<Reply, XError> {
        req.reader().end()?;
        let mut w = Writer::new(req.endian);
        w.zeros(24);
        for button in 1..=POINTER_BUTTONS {
            w.u8(button);
        }
        Ok(Reply::new(POINTER_BUTTONS, w))
    }

    pub fn query_keymap(&self, req: &Request) -> Result<Reply, XError> {
        req.reader().end()?;
        let mut w = Writer::new(req.endian);
        w.bytes(&self.input.pressed_keys());
        Ok(Reply::new(0, w))
    }

    fn check_grab_modes(&mut self, pointer_mode: u8, keyboard_mode: u8) -> Result<(), XError> {
        for mode in [pointer_mode, keyboard_mode] {
            if mode > GRAB_MODE_ASYNC {
                return Err(XError::Value(mode.into()));
            }
        }
        if pointer_mode != GRAB_MODE_ASYNC || keyboard_mode != GRAB_MODE_ASYNC {
            self.log_once("sync grab".into(), || {
                "xsrv-web: synchronous grab modes do not freeze events here; only AllowEvents ReplayPointer after a passive button grab is supported".into()
            });
        }
        Ok(())
    }

    fn grab_status(status: u8, endian: super::codec::Endian) -> Reply {
        Reply::new(status, Writer::new(endian))
    }

    pub fn grab_pointer(&mut self, req: &Request) -> Result<Reply, XError> {
        let owner_events = req.detail != 0;
        let mut r = req.reader();
        let window = r.u32()?;
        let event_mask = r.u16()?;
        let (pointer_mode, keyboard_mode) = (r.u8()?, r.u8()?);
        let confine_to = r.u32()?;
        let cursor = r.u32()?;
        r.skip(4)?;
        r.end()?;
        self.window(window)?;
        if confine_to != 0 {
            self.window(confine_to)?;
            self.log_once("confine to".into(), || {
                "xsrv-web: the confine-to window of a grab is ignored".into()
            });
        }
        self.check_cursor(Some(cursor))?;
        self.check_grab_modes(pointer_mode, keyboard_mode)?;
        if !self.is_viewable(window) {
            return Ok(Self::grab_status(GRAB_NOT_VIEWABLE, req.endian));
        }
        if self
            .input
            .pointer_grab
            .is_some_and(|g| g.client != req.client)
        {
            return Ok(Self::grab_status(GRAB_ALREADY_GRABBED, req.endian));
        }
        self.input.pointer_grab = Some(PointerGrab {
            client: req.client,
            window,
            event_mask: Some(u32::from(event_mask)),
            owner_events,
            ends_with_buttons: false,
        });
        self.grab_crossing(window);
        Ok(Self::grab_status(GRAB_SUCCESS, req.endian))
    }

    /// Changes the event mask of the pointer grab that the client holds. The cursor is checked and
    /// not used, as for GrabPointer, and so is the time. Without such a grab nothing changes.
    pub fn change_active_pointer_grab(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let cursor = r.u32()?;
        r.skip(4)?;
        let event_mask = r.u16()?;
        r.skip(2)?;
        r.end()?;
        self.check_cursor(Some(cursor))?;
        if let Some(grab) = self
            .input
            .pointer_grab
            .as_mut()
            .filter(|grab| grab.client == req.client)
        {
            grab.event_mask = Some(u32::from(event_mask));
        }
        Ok(())
    }

    pub fn ungrab_pointer(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        r.skip(4)?;
        r.end()?;
        if self
            .input
            .pointer_grab
            .is_some_and(|g| g.client == req.client)
        {
            self.input.pointer_grab = None;
            self.refresh_pointer_window_with(super::input::CROSSING_UNGRAB);
        }
        Ok(())
    }

    pub fn grab_keyboard(&mut self, req: &Request) -> Result<Reply, XError> {
        let owner_events = req.detail != 0;
        let mut r = req.reader();
        let window = r.u32()?;
        r.skip(4)?;
        let (pointer_mode, keyboard_mode) = (r.u8()?, r.u8()?);
        r.skip(2)?;
        r.end()?;
        self.window(window)?;
        self.check_grab_modes(pointer_mode, keyboard_mode)?;
        if !self.is_viewable(window) {
            return Ok(Self::grab_status(GRAB_NOT_VIEWABLE, req.endian));
        }
        if self
            .input
            .keyboard_grab
            .is_some_and(|g| g.client != req.client)
        {
            return Ok(Self::grab_status(GRAB_ALREADY_GRABBED, req.endian));
        }
        self.input.keyboard_grab = Some(KeyboardGrab {
            client: req.client,
            window,
            owner_events,
            release_key: None,
        });
        // The keys go to the grab window now: FocusOut and FocusIn with mode Grab.
        let focus = self.input.focus;
        self.send_focus_events(focus, Focus::Window(window), super::input::CROSSING_GRAB);
        Ok(Self::grab_status(GRAB_SUCCESS, req.endian))
    }

    pub fn ungrab_keyboard(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        r.skip(4)?;
        r.end()?;
        if self
            .input
            .keyboard_grab
            .is_some_and(|g| g.client == req.client)
        {
            let grab_window = self.input.keyboard_grab.map(|g| g.window);
            self.input.keyboard_grab = None;
            if let Some(window) = grab_window {
                let focus = self.input.focus;
                self.send_focus_events(Focus::Window(window), focus, super::input::CROSSING_UNGRAB);
            }
        }
        Ok(())
    }

    pub fn grab_button(&mut self, req: &Request) -> Result<(), XError> {
        let owner_events = req.detail != 0;
        let mut r = req.reader();
        let window = r.u32()?;
        let event_mask = r.u16()?;
        let (pointer_mode, keyboard_mode) = (r.u8()?, r.u8()?);
        let confine_to = r.u32()?;
        let cursor = r.u32()?;
        let button = r.u8()?;
        r.skip(1)?;
        let modifiers = r.u16()?;
        r.end()?;
        check_modifiers(modifiers)?;
        self.window(window)?;
        if confine_to != 0 {
            self.window(confine_to)?;
            self.log_once("confine to".into(), || {
                "xsrv-web: the confine-to window of a grab is ignored".into()
            });
        }
        self.check_cursor(Some(cursor))?;
        self.check_grab_modes(pointer_mode, keyboard_mode)?;
        let overlaps = |other: &PassiveButton| {
            other.window == window
                && (other.button == button || other.button == 0 || button == 0)
                && (other.modifiers == modifiers
                    || other.modifiers == ANY_MODIFIER
                    || modifiers == ANY_MODIFIER)
        };
        if self
            .input
            .passive_buttons
            .iter()
            .any(|other| other.client != req.client && overlaps(other))
        {
            return Err(XError::Access);
        }
        self.input
            .passive_buttons
            .retain(|other| !(other.client == req.client && overlaps(other)));
        let held = self
            .input
            .passive_buttons
            .iter()
            .filter(|other| other.client == req.client)
            .count();
        if self.input.passive_buttons.len() >= MAX_PASSIVE_GRABS
            || held >= MAX_PASSIVE_GRABS_PER_CLIENT
        {
            return Err(XError::Alloc);
        }
        self.input.passive_buttons.push(PassiveButton {
            client: req.client,
            window,
            button,
            modifiers,
            event_mask: u32::from(event_mask),
            owner_events,
            sync: pointer_mode == GRAB_MODE_SYNC,
        });
        Ok(())
    }

    pub fn ungrab_button(&mut self, req: &Request) -> Result<(), XError> {
        let button = req.detail;
        let mut r = req.reader();
        let window = r.u32()?;
        let modifiers = r.u16()?;
        r.skip(2)?;
        r.end()?;
        self.window(window)?;
        self.input.passive_buttons.retain(|grab| {
            !(grab.client == req.client
                && grab.window == window
                && (button == 0 || grab.button == button)
                && (modifiers == ANY_MODIFIER || grab.modifiers == modifiers))
        });
        Ok(())
    }

    pub fn grab_key(&mut self, req: &Request) -> Result<(), XError> {
        let owner_events = req.detail != 0;
        let mut r = req.reader();
        let window = r.u32()?;
        let modifiers = r.u16()?;
        let key = r.u8()?;
        let (pointer_mode, keyboard_mode) = (r.u8()?, r.u8()?);
        r.skip(3)?;
        r.end()?;
        check_modifiers(modifiers)?;
        self.window(window)?;
        if key != 0 && key < MIN_KEYCODE {
            return Err(XError::Value(key.into()));
        }
        self.check_grab_modes(pointer_mode, keyboard_mode)?;
        let overlaps = |other: &PassiveKey| {
            other.window == window
                && (other.key == key || other.key == 0 || key == 0)
                && (other.modifiers == modifiers
                    || other.modifiers == ANY_MODIFIER
                    || modifiers == ANY_MODIFIER)
        };
        if self
            .input
            .passive_keys
            .iter()
            .any(|other| other.client != req.client && overlaps(other))
        {
            return Err(XError::Access);
        }
        self.input
            .passive_keys
            .retain(|other| !(other.client == req.client && overlaps(other)));
        let held = self
            .input
            .passive_keys
            .iter()
            .filter(|other| other.client == req.client)
            .count();
        if self.input.passive_keys.len() >= MAX_PASSIVE_GRABS
            || held >= MAX_PASSIVE_GRABS_PER_CLIENT
        {
            return Err(XError::Alloc);
        }
        self.input.passive_keys.push(PassiveKey {
            client: req.client,
            window,
            key,
            modifiers,
            owner_events,
        });
        Ok(())
    }

    pub fn ungrab_key(&mut self, req: &Request) -> Result<(), XError> {
        let key = req.detail;
        let mut r = req.reader();
        let window = r.u32()?;
        let modifiers = r.u16()?;
        r.skip(2)?;
        r.end()?;
        self.window(window)?;
        self.input.passive_keys.retain(|grab| {
            !(grab.client == req.client
                && grab.window == window
                && (key == 0 || grab.key == key)
                && (modifiers == ANY_MODIFIER || grab.modifiers == modifiers))
        });
        Ok(())
    }

    /// Grabs always run asynchronously here, so there is nothing to release.
    pub fn allow_events(&mut self, req: &Request) -> Result<(), XError> {
        const MAX_MODE: u8 = 7;
        let mut r = req.reader();
        r.skip(4)?;
        r.end()?;
        if req.detail > MAX_MODE {
            return Err(XError::Value(req.detail.into()));
        }
        if req.detail == REPLAY_POINTER {
            self.replay_button_press(req.client);
        } else if matches!(req.detail, 0 | 1 | 6 | 7) {
            // AsyncPointer, SyncPointer, AsyncBoth, SyncBoth: the grab keeps the press, and the
            // release that was held back goes to it.
            if self
                .input
                .replay
                .is_some_and(|(owner, _)| owner == req.client)
            {
                self.finish_held_release();
            }
        }
        if req.detail == REPLAY_KEYBOARD {
            self.log_once("replay".into(), || {
                "xsrv-web: AllowEvents cannot replay keyboard events; the replayed event is lost"
                    .into()
            });
        }
        Ok(())
    }

    /// GrabServer and UngrabServer. Every request runs to the end before the next one starts, so
    /// a client cannot see another client's request half done. There is nothing to hold back.
    pub fn grab_server(&self, req: &Request) -> Result<(), XError> {
        req.reader().end()?;
        Ok(())
    }

    pub fn bell(&self, req: &Request) -> Result<(), XError> {
        req.reader().end()?;
        Ok(())
    }
}
