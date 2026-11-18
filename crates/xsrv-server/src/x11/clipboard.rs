// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! The clipboard between X clients and the browser.
//!
//! The server owns a hidden window. When the page sends text, that window becomes the owner of
//! CLIPBOARD and PRIMARY, and the server answers the requests for it. When an X client becomes the
//! owner of CLIPBOARD, the server asks for UTF8_STRING and gives the text to the page.
use super::codec::Endian;
use super::ext_shape::Shapes;
use super::ext_xfixes as xfixes;
use super::message_requests::{
    selection_event, SELECTION_CLEAR, SELECTION_NOTIFY, SELECTION_REQUEST,
};
use super::resources::{Background, Surface};
use super::screen::ROOT_WINDOW;
use super::state::{ClientId, Selection, State, Window, CLASS_INPUT_ONLY};
use crate::clipboard_set::ClipboardData;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};
use wire::{ClipboardSource, MAX_CLIPBOARD_PNG, MAX_CLIPBOARD_TEXT};

/// The id of the hidden window. It is below the ids of every client.
pub const CLIPBOARD_WINDOW: u32 = 0x30;
/// The owner of the hidden window. Real clients start at 1.
pub const SERVER_CLIENT: ClientId = 0;
const STRING_ATOM: u32 = 31;
const ATOM_ATOM: u32 = 4;
const NONE_ATOM: u32 = 0;
const PRIMARY_ATOM: u32 = 1;
/// How long an X owner may take to answer before the next transfer goes ahead of it.
const PULL_TIMEOUT: Duration = Duration::from_secs(5);
const PNG_SIGNATURE: [u8; 4] = [0x89, b'P', b'N', b'G'];

/// What the server asks an X owner for, one request at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wanted {
    Targets,
    Utf8,
    Latin1,
    Html,
    Png,
}

/// The transfer of the content of one selection, from its X owner to the pages.
pub(super) struct Pull {
    selection: u32,
    owner: Selection,
    started: Instant,
    wanted: VecDeque<Wanted>,
    /// The request that waits for an answer.
    current: Option<Wanted>,
    got: ClipboardData,
    /// The chunks of an INCR transfer that is coming in.
    incr: Option<Vec<u8>>,
}

impl State {
    fn clipboard_atoms(&mut self) -> Option<ClipboardAtoms> {
        Some(ClipboardAtoms {
            clipboard: self.atoms.intern(b"CLIPBOARD")?,
            utf8: self.atoms.intern(b"UTF8_STRING")?,
            targets: self.atoms.intern(b"TARGETS")?,
            transfer: self.atoms.intern(b"XSRV_CLIPBOARD")?,
            html: self.atoms.intern(b"text/html")?,
            png: self.atoms.intern(b"image/png")?,
            incr: self.atoms.intern(b"INCR")?,
        })
    }

    fn ensure_clipboard_window(&mut self) {
        if self.windows.contains_key(&CLIPBOARD_WINDOW) {
            return;
        }
        let window = Window {
            parent: Some(ROOT_WINDOW),
            children: Vec::new(),
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            border: 0,
            class: CLASS_INPUT_ONLY,
            depth: 0,
            mapped: false,
            override_redirect: true,
            bit_gravity: 0,
            win_gravity: 1,
            do_not_propagate: 0,
            background: Background::None,
            border_pixel: 0,
            cursor: 0,
            shapes: Shapes::default(),
            surface: Surface::empty(),
            listeners: HashMap::new(),
            properties: HashMap::new(),
        };
        self.insert_window(CLIPBOARD_WINDOW, SERVER_CLIENT, window);
    }

    /// The page has new clipboard content: the hidden window owns CLIPBOARD and PRIMARY. An empty
    /// set (the browser clipboard has nothing to paste) is served as empty text, so that X pastes
    /// nothing and not the old text.
    pub fn page_clipboard(&mut self, data: ClipboardData) {
        let Some(atoms) = self.clipboard_atoms() else {
            return;
        };
        // The same content again (a repeated Ctrl+V) must not take the selections from an X client.
        let owned = [atoms.clipboard, PRIMARY_ATOM].iter().all(|s| {
            self.selections
                .get(s)
                .is_some_and(|o| o.client == SERVER_CLIENT)
        });
        if owned && data == self.clipboard {
            return;
        }
        self.ensure_clipboard_window();
        self.clipboard = data;
        let time = self.timestamp();
        for selection in [atoms.clipboard, PRIMARY_ATOM] {
            let previous = self.selections.insert(
                selection,
                Selection {
                    window: CLIPBOARD_WINDOW,
                    client: SERVER_CLIENT,
                    time,
                },
            );
            self.notify_selection_change(selection, CLIPBOARD_WINDOW, xfixes::SET_OWNER, time);
            if let Some(old) = previous.filter(|old| old.client != SERVER_CLIENT) {
                let event = selection_event(SELECTION_CLEAR, &[time, old.window, selection]);
                self.send_event_to(old.client, &event);
            }
        }
    }

    /// An X client asked for a selection that the page owns. Answers it at once.
    pub(super) fn serve_page_clipboard(
        &mut self,
        client: ClientId,
        requestor: u32,
        selection: u32,
        target: u32,
        property: u32,
        time: u32,
    ) {
        let property = if property == NONE_ATOM {
            target
        } else {
            property
        };
        let stored = self.clipboard_atoms().is_some_and(|atoms| {
            let text = self.clipboard.text.clone().unwrap_or_default();
            if target == atoms.targets {
                let mut offered = vec![atoms.targets, atoms.utf8, STRING_ATOM];
                if self.clipboard.html.is_some() {
                    offered.push(atoms.html);
                }
                if self.clipboard.png.is_some() {
                    offered.push(atoms.png);
                }
                let data = offered.iter().flat_map(|atom| atom.to_le_bytes()).collect();
                self.store_property(requestor, property, ATOM_ATOM, 32, data)
            } else if target == atoms.utf8 {
                self.store_property(requestor, property, atoms.utf8, 8, text.into_bytes())
            } else if target == STRING_ATOM {
                let data = text.chars().map(latin1).collect();
                self.store_property(requestor, property, STRING_ATOM, 8, data)
            } else if target == atoms.html && self.clipboard.html.is_some() {
                let data = self.clipboard.html.clone().unwrap_or_default().into_bytes();
                self.store_property(requestor, property, atoms.html, 8, data)
            } else if target == atoms.png && self.clipboard.png.is_some() {
                let data = self.clipboard.png.clone().unwrap_or_default();
                self.store_property(requestor, property, atoms.png, 8, data)
            } else {
                false
            }
        });
        let answer = if stored { property } else { NONE_ATOM };
        let event = selection_event(
            SELECTION_NOTIFY,
            &[time, requestor, selection, target, answer],
        );
        self.send_event_to(client, &event);
    }

    /// Writes a property of the server. Returns false when the window is gone or the data is too big.
    pub(super) fn store_property(
        &mut self,
        window: u32,
        property: u32,
        type_: u32,
        format: u8,
        data: Vec<u8>,
    ) -> bool {
        if data.len() > self.property_quota_left() {
            return false;
        }
        let Some(target) = self.windows.get_mut(&window) else {
            return false;
        };
        target.properties.insert(
            property,
            super::state::Property {
                type_,
                format,
                data,
            },
        );
        self.notify_property(window, property, false);
        true
    }

    /// An X client became the owner of CLIPBOARD or PRIMARY: the server asks it for the content,
    /// and gives it to the pages. CLIPBOARD (a copy command) and PRIMARY (text selected with the
    /// mouse) are different sources, and the page decides what to do with each.
    pub(super) fn pull_clipboard(&mut self, selection: u32, owner: Selection) {
        let Some(atoms) = self.clipboard_atoms() else {
            return;
        };
        if (selection != atoms.clipboard && selection != PRIMARY_ATOM)
            || owner.client == SERVER_CLIENT
        {
            return;
        }
        if selection == atoms.clipboard {
            // What the page gave is not what the X client holds now.
            self.clipboard = ClipboardData::default();
        }
        self.ensure_clipboard_window();
        // A new owner replaces the transfer of the same selection, and an owner that never
        // answered does not block the others for ever.
        self.clipboard_pulls
            .retain(|pull| pull.selection != selection);
        if self
            .clipboard_pulls
            .front()
            .is_some_and(|pull| pull.started.elapsed() > PULL_TIMEOUT)
        {
            self.clipboard_pulls.pop_front();
        }
        let first = if selection == atoms.clipboard {
            Wanted::Targets
        } else {
            Wanted::Utf8
        };
        self.clipboard_pulls.push_back(Pull {
            selection,
            owner,
            started: Instant::now(),
            wanted: VecDeque::from([first]),
            current: None,
            got: ClipboardData::default(),
            incr: None,
        });
        self.advance_pulls();
    }

    /// Sends the next request of the first transfer, or finishes it and goes on with the next one.
    fn advance_pulls(&mut self) {
        let Some(atoms) = self.clipboard_atoms() else {
            self.clipboard_pulls.clear();
            return;
        };
        loop {
            let Some(front) = self.clipboard_pulls.front_mut() else {
                return;
            };
            if front.current.is_some() {
                return;
            }
            let Some(next) = front.wanted.pop_front() else {
                if let Some(done) = self.clipboard_pulls.pop_front() {
                    self.finish_pull(done);
                }
                continue;
            };
            front.current = Some(next);
            let (owner, selection) = (front.owner, front.selection);
            let target = match next {
                Wanted::Targets => atoms.targets,
                Wanted::Utf8 => atoms.utf8,
                Wanted::Latin1 => STRING_ATOM,
                Wanted::Html => atoms.html,
                Wanted::Png => atoms.png,
            };
            let event = selection_event(
                SELECTION_REQUEST,
                &[
                    owner.time,
                    owner.window,
                    CLIPBOARD_WINDOW,
                    selection,
                    target,
                    atoms.transfer,
                ],
            );
            self.send_event_to(owner.client, &event);
            return;
        }
    }

    /// The owner answered everything: gives the content to the pages. Nothing goes out when the
    /// owner had nothing to copy (an empty answer keeps the browser clipboard as it is).
    fn finish_pull(&mut self, pull: Pull) {
        if pull.got.is_empty() {
            return;
        }
        let source = if pull.selection == PRIMARY_ATOM {
            ClipboardSource::Primary
        } else {
            self.clipboard = pull.got.clone();
            ClipboardSource::Clipboard
        };
        self.display.publish_clipboard(source, &pull.got);
    }

    /// An owner answered a request of `advance_pulls`: `selection`, `target` and `property` are the
    /// fields of its SelectionNotify event.
    pub(super) fn clipboard_answer(
        &mut self,
        sender: ClientId,
        selection: u32,
        target: u32,
        property: u32,
    ) {
        let Some(atoms) = self.clipboard_atoms() else {
            return;
        };
        let Some(front) = self.clipboard_pulls.front() else {
            return;
        };
        let Some(current) = front.current else {
            return;
        };
        // Only the owner of the selection may answer. An old owner cannot replace the content.
        let is_owner = self
            .selections
            .get(&selection)
            .is_some_and(|owner| owner.client == sender);
        let wanted_target = self.wanted_atom(&atoms, current);
        if front.owner.client != sender
            || front.selection != selection
            || wanted_target != target
            || !is_owner
        {
            return;
        }
        if property == NONE_ATOM {
            self.refused(current);
            return;
        }
        if property != atoms.transfer {
            return;
        }
        let Some(found) = self
            .windows
            .get_mut(&CLIPBOARD_WINDOW)
            .and_then(|window| window.properties.remove(&property))
        else {
            return;
        };
        if found.type_ == atoms.incr {
            // The owner sends the content in chunks, each one when the last was deleted.
            if let Some(front) = self.clipboard_pulls.front_mut() {
                front.incr = Some(Vec::new());
            }
            self.notify_property(CLIPBOARD_WINDOW, property, true);
            return;
        }
        self.take_value(current, found.format, found.data);
    }

    fn wanted_atom(&self, atoms: &ClipboardAtoms, wanted: Wanted) -> u32 {
        match wanted {
            Wanted::Targets => atoms.targets,
            Wanted::Utf8 => atoms.utf8,
            Wanted::Latin1 => STRING_ATOM,
            Wanted::Html => atoms.html,
            Wanted::Png => atoms.png,
        }
    }

    /// The owner refused a target. A refused TARGETS means an old client: ask for text only. A
    /// refused UTF8_STRING is tried again as STRING.
    fn refused(&mut self, current: Wanted) {
        if let Some(front) = self.clipboard_pulls.front_mut() {
            front.current = None;
            match current {
                Wanted::Targets => front.wanted = VecDeque::from([Wanted::Utf8]),
                Wanted::Utf8 => front.wanted.push_front(Wanted::Latin1),
                Wanted::Latin1 | Wanted::Html | Wanted::Png => {}
            }
        }
        self.advance_pulls();
    }

    /// Keeps what the owner gave for the request that was waiting, and asks for the next one.
    fn take_value(&mut self, current: Wanted, format: u8, data: Vec<u8>) {
        let Some(atoms) = self.clipboard_atoms() else {
            return;
        };
        let Some(front) = self.clipboard_pulls.front() else {
            return;
        };
        let endian = self
            .client_endian(front.owner.client)
            .unwrap_or(Endian::Little);
        let Some(front) = self.clipboard_pulls.front_mut() else {
            return;
        };
        front.current = None;
        front.incr = None;
        match current {
            Wanted::Targets if format == 32 => {
                let offered: Vec<u32> = data
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|bytes| match endian {
                        Endian::Little => u32::from_le_bytes(*bytes),
                        Endian::Big => u32::from_be_bytes(*bytes),
                    })
                    .collect();
                front.wanted = Self::wanted_from_targets(&offered, &atoms, front.selection);
            }
            Wanted::Targets => front.wanted = VecDeque::from([Wanted::Utf8]),
            Wanted::Utf8 if data.len() <= MAX_CLIPBOARD_TEXT => {
                front.got.text = String::from_utf8(data).ok();
            }
            Wanted::Latin1 if data.len() <= MAX_CLIPBOARD_TEXT && front.got.text.is_none() => {
                front.got.text = Some(data.iter().map(|&b| char::from(b)).collect());
            }
            Wanted::Html if data.len() <= MAX_CLIPBOARD_TEXT => {
                front.got.html = String::from_utf8(data).ok();
            }
            Wanted::Png if data.len() <= MAX_CLIPBOARD_PNG && data.starts_with(&PNG_SIGNATURE) => {
                front.got.png = Some(data);
            }
            // Too big, or not what it says: left out.
            _ => {}
        }
        self.advance_pulls();
    }

    /// What to ask after TARGETS: the text format the owner has (UTF8_STRING first), and for
    /// CLIPBOARD also HTML and a PNG image. PRIMARY is text only.
    fn wanted_from_targets(
        offered: &[u32],
        atoms: &ClipboardAtoms,
        selection: u32,
    ) -> VecDeque<Wanted> {
        let has = |atom: u32| offered.contains(&atom);
        let mut wanted = VecDeque::new();
        if has(atoms.utf8) || offered.is_empty() {
            wanted.push_back(Wanted::Utf8);
        } else if has(STRING_ATOM) {
            wanted.push_back(Wanted::Latin1);
        }
        if selection != PRIMARY_ATOM {
            if has(atoms.html) {
                wanted.push_back(Wanted::Html);
            }
            if has(atoms.png) {
                wanted.push_back(Wanted::Png);
            }
        }
        wanted
    }

    /// An X client wrote a property of the hidden window. During an INCR transfer from that
    /// client it is the next chunk: the server takes it, deletes the property (which tells the
    /// owner to send the next one), and an empty chunk ends the transfer.
    pub(super) fn clipboard_property_written(&mut self, client: ClientId, property: u32) {
        let Some(atoms) = self.clipboard_atoms() else {
            return;
        };
        let Some(front) = self.clipboard_pulls.front() else {
            return;
        };
        let (Some(current), true) = (front.current, front.incr.is_some()) else {
            return;
        };
        if property != atoms.transfer || front.owner.client != client {
            return;
        }
        let Some(chunk) = self
            .windows
            .get_mut(&CLIPBOARD_WINDOW)
            .and_then(|window| window.properties.remove(&property))
        else {
            return;
        };
        self.notify_property(CLIPBOARD_WINDOW, property, true);
        let limit = if current == Wanted::Png {
            MAX_CLIPBOARD_PNG
        } else {
            MAX_CLIPBOARD_TEXT
        };
        let Some(front) = self.clipboard_pulls.front_mut() else {
            return;
        };
        let received = front.incr.get_or_insert_with(Vec::new);
        if chunk.data.is_empty() {
            let whole = std::mem::take(received);
            self.take_value(current, 8, whole);
        } else if received.len() + chunk.data.len() > limit {
            // Too big: the rest of the chunks is thrown away, and this target is left out.
            front.incr = None;
            front.current = None;
            self.advance_pulls();
        } else {
            received.extend_from_slice(&chunk.data);
        }
    }

    /// The owner of a transfer went away.
    pub(super) fn forget_clipboard_pulls(&mut self, client: ClientId) {
        self.clipboard_pulls
            .retain(|pull| pull.owner.client != client);
        self.advance_pulls();
    }

    pub(super) fn is_clipboard_answer(event: &[u8; 32], destination: u32) -> bool {
        destination == CLIPBOARD_WINDOW && event[0] & 0x7F == SELECTION_NOTIFY
    }
}

fn latin1(c: char) -> u8 {
    u8::try_from(u32::from(c)).unwrap_or(b'?')
}

struct ClipboardAtoms {
    clipboard: u32,
    utf8: u32,
    targets: u32,
    transfer: u32,
    html: u32,
    png: u32,
    incr: u32,
}
