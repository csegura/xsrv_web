// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! The XFIXES extension, version 5.0, in the parts that clients use without regions:
//! the version, the save set, selection owner events, cursor names and cursor visibility.
//! A client that selects selection events gets one when the owner of a selection changes.
use super::codec::{padding, Writer};
use super::dispatch::{Reply, Request};
use super::error::XError;
use super::event::Event;
use super::extensions::{event_code, XFIXES};
use super::state::{ClientId, State};

pub const SELECTION_OWNER_NOTIFY_MASK: u32 = 1;
pub const SELECTION_WINDOW_DESTROY_MASK: u32 = 2;
pub const SELECTION_CLIENT_CLOSE_MASK: u32 = 4;
const ALL_SELECTION_MASKS: u32 = 7;
const SET_CURSOR_NAME: u8 = 23;
const HIDE_CURSOR: u8 = 29;
const SHOW_CURSOR: u8 = 30;
const SELECTION_NOTIFY_EVENT: u8 = 0;
const SUBTYPE_SET_OWNER: u8 = 0;
const SUBTYPE_WINDOW_DESTROY: u8 = 1;
const SUBTYPE_CLIENT_CLOSE: u8 = 2;

impl State {
    pub(super) fn xfixes_request(&mut self, req: &Request) -> Result<Option<Reply>, XError> {
        match req.detail {
            0 => self.xfixes_query_version(req).map(Some),
            1 => self.xfixes_change_save_set(req).map(|()| None),
            2 => self.xfixes_select_selection_input(req).map(|()| None),
            3 => self.xfixes_select_cursor_input(req).map(|()| None),
            SET_CURSOR_NAME => self.xfixes_set_cursor_name(req).map(|()| None),
            HIDE_CURSOR | SHOW_CURSOR => self.xfixes_cursor_visibility(req).map(|()| None),
            5..=22 | 28 => self.xfixes_region_request(req),
            _ => Err(XError::Request),
        }
    }

    fn xfixes_set_cursor_name(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let cursor = r.u32()?;
        let len = usize::from(r.u16()?);
        r.skip(2)?;
        let name = r.bytes(len)?.to_vec();
        r.skip(padding(len))?;
        r.end()?;
        self.set_cursor_name(cursor, &name)
    }

    fn xfixes_query_version(&self, req: &Request) -> Result<Reply, XError> {
        let mut r = req.reader();
        let (major, minor) = (r.u32()?, r.u32()?);
        r.end()?;
        // A client may ask for less. HideCursor needs 4.0, so clients must see at least that.
        let (major, minor) = (major, minor).min((5, 0));
        let mut w = Writer::new(req.endian);
        w.u32(major);
        w.u32(minor);
        Ok(Reply::new(0, w))
    }

    /// The target is ignored: a saved window always goes back to the root.
    fn xfixes_change_save_set(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let mode = r.u8()?;
        let _target = r.u8()?;
        let map = r.u8()? == 0;
        r.skip(1)?;
        let window = r.u32()?;
        r.end()?;
        self.save_set_change(req.client, window, mode, map)
    }

    fn xfixes_select_selection_input(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let window = r.u32()?;
        let selection = r.u32()?;
        let mask = r.u32()?;
        r.end()?;
        self.window(window)?;
        self.require_atom(selection)?;
        if mask & !ALL_SELECTION_MASKS != 0 {
            return Err(XError::Value(mask));
        }
        let key = (req.client, selection, window);
        if mask == 0 {
            self.selection_watchers.remove(&key);
        } else {
            self.selection_watchers.insert(key, mask);
        }
        Ok(())
    }

    /// Cursor change events are not sent, because the cursors are not images (see `CursorShape`).
    fn xfixes_select_cursor_input(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        self.window(r.u32()?)?;
        r.skip(4)?;
        r.end()?;
        self.log_once("xfixes cursor input".into(), || {
            "xsrv-web: XFIXES CursorNotify events are not sent".into()
        });
        Ok(())
    }

    /// HideCursor hides the pointer of the page until the same client shows it or leaves.
    fn xfixes_cursor_visibility(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        self.window(r.u32()?)?;
        r.end()?;
        if req.detail == HIDE_CURSOR {
            self.hidden_cursors.insert(req.client);
        } else {
            self.hidden_cursors.remove(&req.client);
        }
        Ok(())
    }

    /// Tells the watchers of a selection that its owner changed.
    pub(super) fn notify_selection_change(
        &self,
        selection: u32,
        owner: u32,
        subtype: u8,
        selection_time: u32,
    ) {
        let mask_for = |subtype: u8| match subtype {
            SUBTYPE_WINDOW_DESTROY => SELECTION_WINDOW_DESTROY_MASK,
            SUBTYPE_CLIENT_CLOSE => SELECTION_CLIENT_CLOSE_MASK,
            _ => SELECTION_OWNER_NOTIFY_MASK,
        };
        let time = self.timestamp();
        for (&(client, watched, window), &mask) in &self.selection_watchers {
            if watched != selection || mask & mask_for(subtype) == 0 {
                continue;
            }
            let event = Event::XfixesSelectionNotify {
                code: event_code(XFIXES, SELECTION_NOTIFY_EVENT).unwrap_or(0),
                subtype,
                window,
                owner,
                selection,
                time,
                selection_time,
            };
            self.send_event_to(client, &event);
        }
    }

    pub(super) fn forget_selection_watchers(&mut self, client: ClientId) {
        self.selection_watchers
            .retain(|(watcher, _, _), _| *watcher != client);
    }
}

pub const SET_OWNER: u8 = SUBTYPE_SET_OWNER;
pub const WINDOW_DESTROY: u8 = SUBTYPE_WINDOW_DESTROY;
pub const CLIENT_CLOSE: u8 = SUBTYPE_CLIENT_CLOSE;
