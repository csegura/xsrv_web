// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

use super::codec::{Endian, Writer};
use super::dispatch::{Reply, Request};
use super::error::XError;
use super::event::Event;
use super::ext_xfixes as xfixes;
use super::input::Focus;
use super::state::{ClientId, Selection, State};

const MIN_SENT_EVENT: u8 = 2;
const CLIENT_MESSAGE: u8 = 33;
pub(super) const SELECTION_CLEAR: u8 = 29;
pub(super) const SELECTION_REQUEST: u8 = 30;
pub(super) const SELECTION_NOTIFY: u8 = 31;
const DESTINATION_POINTER_WINDOW: u32 = 0;
const DESTINATION_INPUT_FOCUS: u32 = 1;
const NONE_ATOM: u32 = 0;

/// Builds a 32-byte event whose fields are 32-bit numbers after the header.
pub(super) fn selection_event(code: u8, fields: &[u32]) -> Event {
    let mut bytes = [0u8; 32];
    bytes[0] = code;
    for (index, field) in fields.iter().enumerate() {
        bytes[4 + index * 4..8 + index * 4].copy_from_slice(&field.to_le_bytes());
    }
    Event::Raw {
        bytes,
        from: Endian::Little,
    }
}

impl State {
    /// Sends a 32-byte event that a client made. The high bit of the event code marks it as sent.
    pub fn send_event(&mut self, req: &Request) -> Result<(), XError> {
        let propagate = req.detail != 0;
        let mut r = req.reader();
        let destination = r.u32()?;
        let mask = r.u32()?;
        let bytes: [u8; 32] = r.bytes(32)?.try_into().map_err(|_| XError::Length)?;
        r.end()?;
        let code = bytes[0] & 0x7F;
        if code < MIN_SENT_EVENT {
            return Err(XError::Value(code.into()));
        }
        if code == CLIENT_MESSAGE && !matches!(bytes[1], 8 | 16 | 32) {
            return Err(XError::Value(bytes[1].into()));
        }
        if Self::is_clipboard_answer(&bytes, destination) {
            let field = |at: usize| {
                let word = [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]];
                match req.endian {
                    Endian::Little => u32::from_le_bytes(word),
                    Endian::Big => u32::from_be_bytes(word),
                }
            };
            self.clipboard_answer(req.client, field(12), field(16), field(20));
            return Ok(());
        }
        let window = match destination {
            DESTINATION_POINTER_WINDOW => self.input.pointer_window,
            DESTINATION_INPUT_FOCUS => match self.input.focus {
                Focus::Window(window) => window,
                Focus::PointerRoot => self.input.pointer_window,
                Focus::None => return Ok(()),
            },
            id => {
                self.window(id)?;
                id
            }
        };
        let mut sent = bytes;
        sent[0] |= 0x80;
        let event = Event::Raw {
            bytes: sent,
            from: req.endian,
        };
        if mask == 0 {
            if let Some(&owner) = self.owners.get(&window) {
                self.send_event_to(owner, &event);
            }
            return Ok(());
        }
        let mut current = Some(window);
        while let Some(candidate) = current {
            let clients = self.listeners_with(candidate, mask, None);
            if !clients.is_empty() {
                for client in clients {
                    self.send_event_to(client, &event);
                }
                return Ok(());
            }
            if !propagate {
                break;
            }
            current = self.windows.get(&candidate).and_then(|w| w.parent);
        }
        Ok(())
    }

    pub fn set_selection_owner(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let owner = r.u32()?;
        let selection = r.u32()?;
        let time = r.u32()?;
        r.end()?;
        self.require_atom(selection)?;
        if owner != 0 {
            self.window(owner)?;
        }
        let now = self.timestamp();
        if (time.wrapping_sub(now) as i32) > 0 {
            return Ok(());
        }
        let time = if time == 0 { now } else { time };
        let previous = self.selections.get(&selection).copied();
        if previous.is_some_and(|p| (time.wrapping_sub(p.time) as i32) < 0) {
            return Ok(());
        }
        if owner == 0 {
            self.selections.remove(&selection);
        } else {
            let new = Selection {
                window: owner,
                client: req.client,
                time,
            };
            self.selections.insert(selection, new);
        }
        self.notify_selection_change(selection, owner, xfixes::SET_OWNER, time);
        if let Some(new) = self.selections.get(&selection).copied() {
            self.pull_clipboard(selection, new);
        }
        if let Some(old) = previous.filter(|p| p.client != req.client || owner == 0) {
            let event = selection_event(SELECTION_CLEAR, &[time, old.window, selection]);
            self.send_event_to(old.client, &event);
        }
        Ok(())
    }

    pub fn get_selection_owner(&self, req: &Request) -> Result<Reply, XError> {
        let mut r = req.reader();
        let selection = r.u32()?;
        r.end()?;
        self.require_atom(selection)?;
        let owner = self.selections.get(&selection).map_or(0, |s| s.window);
        let mut w = Writer::new(req.endian);
        w.u32(owner);
        Ok(Reply::new(0, w))
    }

    /// Asks the owner of the selection to convert it. With no owner, the requestor is told at once.
    pub fn convert_selection(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let requestor = r.u32()?;
        let selection = r.u32()?;
        let target = r.u32()?;
        let property = r.u32()?;
        let time = r.u32()?;
        r.end()?;
        self.window(requestor)?;
        self.require_atom(selection)?;
        self.require_atom(target)?;
        if property != NONE_ATOM {
            self.require_atom(property)?;
        }
        let time = if time == 0 { self.timestamp() } else { time };
        match self.selections.get(&selection).copied() {
            Some(owner) if owner.client == super::clipboard::SERVER_CLIENT => {
                self.serve_page_clipboard(req.client, requestor, selection, target, property, time);
            }
            Some(owner) => {
                let event = selection_event(
                    SELECTION_REQUEST,
                    &[time, owner.window, requestor, selection, target, property],
                );
                self.send_event_to(owner.client, &event);
            }
            None => {
                let event = selection_event(
                    SELECTION_NOTIFY,
                    &[time, requestor, selection, target, NONE_ATOM],
                );
                self.send_event_to(req.client, &event);
            }
        }
        Ok(())
    }

    /// Drops the selections that a window or a client owned. The watchers are told.
    pub(super) fn forget_selections(&mut self, window: Option<u32>, client: Option<ClientId>) {
        let gone: Vec<(u32, Selection)> = self
            .selections
            .iter()
            .filter(|(_, s)| {
                window.is_some_and(|w| s.window == w) || client.is_some_and(|c| s.client == c)
            })
            .map(|(atom, s)| (*atom, *s))
            .collect();
        for (atom, selection) in gone {
            let subtype = if window.is_some() {
                xfixes::WINDOW_DESTROY
            } else {
                xfixes::CLIENT_CLOSE
            };
            self.notify_selection_change(atom, 0, subtype, selection.time);
        }
        self.selections.retain(|_, selection| {
            window.is_none_or(|w| selection.window != w)
                && client.is_none_or(|c| selection.client != c)
        });
    }
}
