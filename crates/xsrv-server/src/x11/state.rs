// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

use std::cell::Cell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::os::fd::OwnedFd;
use std::rc::{Rc, Weak};
use std::time::Instant;

use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::Notify;

use wire::CursorShape;

use crate::clipboard_set::ClipboardData;
use crate::display::Display;

use super::atoms::AtomTable;
use super::codec::Endian;
use super::error::XError;
use super::event::{Event, STRUCTURE_NOTIFY, SUBSTRUCTURE_NOTIFY};
use super::ext_shape::Shapes;
use super::ext_shm::ShmSegment;
use super::input::InputState;
use super::resources::{Background, Gc, IRect, Pixmap, Surface};
use super::retained::CloseDown;
use super::screen::{MAX_CLIENTS, RESOURCE_ID_MASK, RESOURCE_ID_SHIFT, ROOT_DEPTH, ROOT_WINDOW};

pub type ClientId = u32;

pub const CLASS_INPUT_OUTPUT: u16 = 1;
pub const CLASS_INPUT_ONLY: u16 = 2;

const MAX_QUEUED_BYTES: usize = 8 << 20;
const MAX_LOGGED_KEYS: usize = 256;
const MAX_TOTAL_PROPERTY_BYTES: usize = 64 << 20;

/// Bytes queued for one client. The writer task lowers the count after each write.
pub type PendingBytes = Rc<Cell<usize>>;

struct ClientInfo {
    endian: Endian,
    seq: u16,
    tx: UnboundedSender<Vec<u8>>,
    pending: PendingBytes,
    overflowed: Cell<bool>,
    overflow_signal: Rc<Notify>,
    big_requests: Rc<Cell<bool>>,
    /// The user id of the process on the other end of the socket.
    uid: Option<u32>,
    pub(super) close_down: CloseDown,
    /// File descriptors that came with the client's requests (ShmAttachFd takes them).
    fds: VecDeque<OwnedFd>,
}

/// File descriptors that may wait for a request that takes them.
pub(super) const MAX_QUEUED_FDS: usize = 16;

impl ClientInfo {
    /// A client that does not read its socket is cut off once its queue is too large.
    fn send(&self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        if self.pending.get() + bytes.len() > MAX_QUEUED_BYTES {
            // The connection task wakes up at once and closes the connection.
            self.overflowed.set(true);
            self.overflow_signal.notify_one();
            return;
        }
        self.pending.set(self.pending.get() + bytes.len());
        let _ = self.tx.send(bytes);
    }
}

/// The owner of a selection such as PRIMARY or CLIPBOARD.
#[derive(Clone, Copy)]
pub struct Selection {
    pub window: u32,
    pub client: ClientId,
    pub time: u32,
}

/// Which client owns each resource id, and how many resources each client owns.
/// The count makes the quota check cheap, however many resources a client has.
#[derive(Default)]
pub(super) struct Owners {
    by_id: HashMap<u32, ClientId>,
    counts: HashMap<ClientId, usize>,
}

impl Owners {
    pub fn insert(&mut self, id: u32, client: ClientId) {
        if let Some(previous) = self.by_id.insert(id, client) {
            self.forget(previous);
        }
        *self.counts.entry(client).or_default() += 1;
    }

    pub fn remove(&mut self, id: &u32) -> Option<ClientId> {
        let client = self.by_id.remove(id)?;
        self.forget(client);
        Some(client)
    }

    fn forget(&mut self, client: ClientId) {
        if let Some(count) = self.counts.get_mut(&client) {
            *count -= 1;
            if *count == 0 {
                self.counts.remove(&client);
            }
        }
    }

    pub fn get(&self, id: &u32) -> Option<&ClientId> {
        self.by_id.get(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&u32, &ClientId)> {
        self.by_id.iter()
    }

    pub fn count(&self, client: ClientId) -> usize {
        self.counts.get(&client).copied().unwrap_or(0)
    }
}

pub struct Property {
    pub type_: u32,
    pub format: u8,
    pub data: Vec<u8>,
}

pub struct Window {
    pub parent: Option<u32>,
    pub children: Vec<u32>,
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
    pub border: u16,
    pub class: u16,
    pub depth: u8,
    pub mapped: bool,
    pub override_redirect: bool,
    pub bit_gravity: u8,
    pub win_gravity: u8,
    pub do_not_propagate: u16,
    pub background: Background,
    pub border_pixel: u32,
    /// The cursor id of the window. 0 means the cursor of the parent.
    pub cursor: u32,
    pub shapes: Shapes,
    pub surface: Surface,
    pub listeners: HashMap<ClientId, u32>,
    pub properties: HashMap<u32, Property>,
}

pub struct State {
    pub atoms: AtomTable,
    pub(super) windows: HashMap<u32, Window>,
    pub(super) pixmaps: HashMap<u32, Pixmap>,
    pub(super) gcs: HashMap<u32, Gc>,
    pub(super) clips: HashMap<u32, Vec<IRect>>,
    /// The XFIXES regions, as rectangles that do not overlap.
    pub(super) regions: HashMap<u32, Vec<IRect>>,
    pub(super) selections: HashMap<u32, Selection>,
    /// What the browser clipboard and the X clipboard share. Empty after an X client takes
    /// CLIPBOARD, until its answer comes.
    pub(super) clipboard: ClipboardData,
    /// The transfers from X owners to the pages. The first is the one that is waiting for an answer.
    pub(super) clipboard_pulls: std::collections::VecDeque<super::clipboard::Pull>,
    pub(super) randr_listeners: HashMap<ClientId, u16>,
    /// For each client, the windows that go back to the root when it leaves, and whether to map them.
    pub(super) save_sets: HashMap<ClientId, HashMap<u32, bool>>,
    pub(super) shm_segments: HashMap<u32, ShmSegment>,
    /// Pictures and glyph sets of the RENDER extension.
    pub(super) render: super::render::RenderState,
    /// (client, selection, window) to the mask of events that the client wants for the window.
    pub(super) selection_watchers: HashMap<(ClientId, u32, u32), u32>,
    pub(super) hidden_cursors: HashSet<ClientId>,
    /// XKEYBOARD event selections and per-client flags.
    pub(super) xkb: super::ext_xkb::XkbClients,
    pub(super) xkb_enabled: bool,
    pub(super) config_time: u32,
    pub(super) fonts: HashSet<u32>,
    pub(super) cursors: HashMap<u32, CursorShape>,
    pub(super) owners: Owners,
    /// The pixel bytes of the windows and pixmaps of each client.
    pub(super) client_pixels: HashMap<ClientId, usize>,
    /// Clients that left with SetCloseDownMode Retain*: their resources stay until KillClient.
    /// The copies of pixmaps that windows use as background, by pixmap id.
    pub(super) tile_cache: HashMap<u32, Weak<Surface>>,
    /// Every tile that a window still uses, to count their bytes.
    pub(super) live_tiles: Vec<Weak<Surface>>,
    /// The primary output that RANDR reports: 0 is none.
    pub(super) primary_output: u32,
    pub(super) retained: HashMap<ClientId, CloseDown>,
    clients: HashMap<ClientId, ClientInfo>,
    started: Instant,
    logged: HashSet<String>,
    pub(super) display: Display,
    /// The changed parts of the screen that the page has not got yet. They do not overlap.
    pub(super) dirty: Vec<IRect>,
    /// The pixels of the area that is composed now. Kept between areas: a new buffer for each
    /// one cost page faults on every update.
    pub(super) compose_buffer: Vec<u8>,
    /// When the work buffers were last used: they are given back after `BUFFERS_QUIET`.
    pub(super) buffers_used_at: Option<Instant>,
    /// When the first changed area of the current burst appeared, and when the last request ran.
    pub(super) dirty_since: Option<Instant>,
    /// Wakes the timer task when there is timed work (`needs_ticks`). Without work it sleeps, so
    /// an idle server does not wake up every few milliseconds.
    pub(super) tick_wake: Rc<Notify>,
    pub(super) last_request: Option<Instant>,
    /// After a change of the screen size the clients (a window manager) need a moment to lay their
    /// windows out again. The timer does not draw before this time, so that the page does not show
    /// the windows in their old places first.
    pub(super) hold_flush_until: Option<Instant>,
    pub(super) pixel_bytes: usize,
    pub(super) input: InputState,
    window_manager: bool,
    /// Top-level windows that fill the screen and follow its size (the built-in manager).
    maximize: bool,
    /// The cookie that X clients must show. `None`: any client may connect.
    cookie: Option<Vec<u8>>,
    /// How many virtual monitors RANDR reports: the screen cut into columns.
    pub(super) monitors: usize,
    pub(super) maximized: HashSet<u32>,
}

impl State {
    pub fn new(display: Display) -> Self {
        let (screen_width, screen_height) = display.size();
        let root_surface = Surface::new(screen_width, screen_height);
        let pixel_bytes = root_surface.len();
        let root = Window {
            parent: None,
            children: Vec::new(),
            x: 0,
            y: 0,
            width: screen_width,
            height: screen_height,
            border: 0,
            class: CLASS_INPUT_OUTPUT,
            depth: ROOT_DEPTH,
            mapped: true,
            override_redirect: false,
            bit_gravity: 0,
            win_gravity: 1,
            do_not_propagate: 0,
            background: Background::Pixel(0),
            border_pixel: 0,
            cursor: 0,
            shapes: Shapes::default(),
            surface: root_surface,
            listeners: HashMap::new(),
            properties: HashMap::new(),
        };
        Self {
            atoms: AtomTable::new(),
            windows: HashMap::from([(ROOT_WINDOW, root)]),
            pixmaps: HashMap::new(),
            gcs: HashMap::new(),
            clips: HashMap::new(),
            regions: HashMap::new(),
            selections: HashMap::new(),
            clipboard: ClipboardData::default(),
            clipboard_pulls: std::collections::VecDeque::new(),
            randr_listeners: HashMap::new(),
            save_sets: HashMap::new(),
            shm_segments: HashMap::new(),
            render: Default::default(),
            selection_watchers: HashMap::new(),
            hidden_cursors: HashSet::new(),
            xkb: Default::default(),
            xkb_enabled: false,
            config_time: 0,
            fonts: HashSet::new(),
            cursors: HashMap::new(),
            owners: Owners::default(),
            client_pixels: HashMap::new(),
            tile_cache: HashMap::new(),
            live_tiles: Vec::new(),
            primary_output: 0x50,
            retained: HashMap::new(),
            clients: HashMap::new(),
            started: Instant::now(),
            logged: HashSet::new(),
            display,
            dirty: Vec::new(),
            compose_buffer: Vec::new(),
            buffers_used_at: None,
            dirty_since: None,
            tick_wake: Rc::new(Notify::new()),
            last_request: None,
            hold_flush_until: None,
            pixel_bytes,
            input: InputState::new(),
            window_manager: false,
            maximize: false,
            cookie: None,
            monitors: 1,
            maximized: HashSet::new(),
        }
    }

    /// The size of the screen: the size of the root window.
    pub fn screen_size(&self) -> (u16, u16) {
        self.windows
            .get(&ROOT_WINDOW)
            .map_or((0, 0), |root| (root.width, root.height))
    }

    pub fn set_layout(&mut self, layout: super::keymap::Layout) {
        self.input.layout = layout;
        self.display.set_layout(layout.index());
    }

    pub fn set_monitors(&mut self, count: usize) {
        self.monitors = count.clamp(1, super::ext_randr::MAX_MONITORS);
    }

    pub fn set_cookie(&mut self, cookie: Option<Vec<u8>>) {
        self.cookie = cookie;
    }

    pub(super) fn x11_cookie(&self) -> Option<&[u8]> {
        self.cookie.as_deref()
    }

    pub fn set_maximize(&mut self, on: bool) {
        self.maximize = on;
    }

    pub fn set_xkb(&mut self, on: bool) {
        self.xkb_enabled = on;
        self.xkb_publish_rules_names();
    }

    pub(super) fn offers_xkb(&self, endian: super::codec::Endian) -> bool {
        self.xkb_enabled && super::ext_xkb::offered_to(endian)
    }

    pub(super) fn maximize_enabled(&self) -> bool {
        self.maximize
    }

    pub fn set_window_manager(&mut self, on: bool) {
        self.window_manager = on;
    }

    pub(super) fn window_manager_enabled(&self) -> bool {
        self.window_manager
    }

    /// Returns the client id and the base of its resource id range, or `None` when full.
    pub fn add_client(
        &mut self,
        endian: Endian,
        tx: UnboundedSender<Vec<u8>>,
        pending: PendingBytes,
        overflow_signal: Rc<Notify>,
        big_requests: Rc<Cell<bool>>,
    ) -> Option<(ClientId, u32)> {
        let id = (1..=MAX_CLIENTS)
            .find(|id| !self.clients.contains_key(id) && !self.retained.contains_key(id))?;
        let info = ClientInfo {
            endian,
            seq: 0,
            tx,
            pending,
            overflowed: Cell::new(false),
            overflow_signal,
            big_requests,
            uid: None,
            close_down: CloseDown::Destroy,
            fds: VecDeque::new(),
        };
        self.clients.insert(id, info);
        Some((id, id << RESOURCE_ID_SHIFT))
    }

    pub(super) fn set_client_close_down(&mut self, client: ClientId, mode: CloseDown) {
        if let Some(info) = self.clients.get_mut(&client) {
            info.close_down = mode;
        }
    }

    pub(super) fn client_close_down(&self, client: ClientId) -> CloseDown {
        self.clients
            .get(&client)
            .map_or(CloseDown::Destroy, |info| info.close_down)
    }

    pub fn set_client_uid(&mut self, client: ClientId, uid: Option<u32>) {
        if let Some(info) = self.clients.get_mut(&client) {
            info.uid = uid;
        }
    }

    /// Keeps the descriptors that came with a client's bytes. Returns false when too many wait:
    /// the connection then ends, and the descriptors are closed.
    pub(super) fn push_client_fds(&mut self, client: ClientId, fds: Vec<OwnedFd>) -> bool {
        let Some(info) = self.clients.get_mut(&client) else {
            return false;
        };
        info.fds.extend(fds);
        info.fds.len() <= MAX_QUEUED_FDS
    }

    /// The oldest descriptor that a client sent and no request has taken yet.
    pub(super) fn take_client_fd(&mut self, client: ClientId) -> Option<OwnedFd> {
        self.clients.get_mut(&client)?.fds.pop_front()
    }

    pub(super) fn client_uid(&self, client: ClientId) -> Option<u32> {
        self.clients.get(&client).and_then(|info| info.uid)
    }

    pub fn remove_client(&mut self, client: ClientId) {
        self.forget_selections(None, Some(client));
        self.forget_clipboard_pulls(client);
        self.rescue_saved_windows(client);
        if !self.retain_resources(client) {
            self.release_owned_resources(client);
        }
        for window in self.windows.values_mut() {
            window.listeners.remove(&client);
            window.shapes.listeners.remove(&client);
        }
        self.input.forget_client(client);
        self.randr_listeners.remove(&client);
        self.forget_shm_segments(client);
        if !self.retained.contains_key(&client) {
            self.forget_orphan_owner(client);
        }
        self.forget_selection_watchers(client);
        self.hidden_cursors.remove(&client);
        self.xkb.forget(client);
        self.clients.remove(&client);
        if !self.retained.contains_key(&client) {
            self.client_pixels.remove(&client);
        }
        self.after_request();
        self.flush_display();
    }

    pub fn client_endian(&self, client: ClientId) -> Option<Endian> {
        self.clients.get(&client).map(|info| info.endian)
    }

    pub(super) fn enable_big_requests(&self, client: ClientId) {
        if let Some(info) = self.clients.get(&client) {
            info.big_requests.set(true);
        }
    }

    /// The sequence number of the request that is being handled.
    pub fn current_sequence(&self, client: ClientId) -> Option<u16> {
        self.clients.get(&client).map(|info| info.seq)
    }

    /// Counts the request and returns its sequence number.
    pub fn next_sequence(&mut self, client: ClientId) -> Option<u16> {
        let info = self.clients.get_mut(&client)?;
        info.seq = info.seq.wrapping_add(1);
        Some(info.seq)
    }

    pub fn send_raw(&self, client: ClientId, bytes: Vec<u8>) {
        if let Some(info) = self.clients.get(&client) {
            info.send(bytes);
        }
    }

    /// Sends an event to one client, whatever its event masks are.
    pub fn send_event_to(&self, client: ClientId, event: &Event) {
        if let Some(info) = self.clients.get(&client) {
            info.send(event.encode(info.endian, info.seq));
        }
    }

    /// For the events that go to every client without a mask (MappingNotify).
    pub(super) fn send_event_to_all(&self, event: &Event) {
        for info in self.clients.values() {
            info.send(event.encode(info.endian, info.seq));
        }
    }

    #[cfg(test)]
    pub fn is_overflowed(&self, client: ClientId) -> bool {
        self.clients
            .get(&client)
            .is_some_and(|c| c.overflowed.get())
    }

    /// Prints a message the first time `key` is seen, so a client cannot flood the log.
    pub fn log_once(&mut self, key: String, message: impl FnOnce() -> String) {
        if self.logged.len() < MAX_LOGGED_KEYS && self.logged.insert(key) {
            eprintln!("{}", message());
        }
    }

    pub fn total_property_bytes(&self) -> usize {
        self.windows
            .values()
            .flat_map(|w| w.properties.values())
            .map(|p| p.data.len())
            .sum()
    }

    pub fn property_quota_left(&self) -> usize {
        MAX_TOTAL_PROPERTY_BYTES.saturating_sub(self.total_property_bytes())
    }

    pub fn owns_id(&self, client: ClientId, id: u32) -> bool {
        id & !RESOURCE_ID_MASK == client << RESOURCE_ID_SHIFT
    }

    pub fn window(&self, id: u32) -> Result<&Window, XError> {
        self.windows.get(&id).ok_or(XError::Window(id))
    }

    pub fn window_mut(&mut self, id: u32) -> Result<&mut Window, XError> {
        self.windows.get_mut(&id).ok_or(XError::Window(id))
    }

    pub fn window_exists(&self, id: u32) -> bool {
        self.windows.contains_key(&id)
    }

    pub fn window_count(&self) -> usize {
        self.windows.len()
    }

    pub fn insert_window(&mut self, id: u32, owner: ClientId, window: Window) {
        if let Some(parent) = window.parent.and_then(|p| self.windows.get_mut(&p)) {
            parent.children.push(id);
        }
        self.owners.insert(id, owner);
        self.windows.insert(id, window);
    }

    /// The number of ancestors of a window. The root has depth 0.
    pub fn window_depth(&self, id: u32) -> usize {
        let mut depth = 0;
        let mut current = self.windows.get(&id).and_then(|w| w.parent);
        while let Some(parent) = current {
            depth += 1;
            current = self.windows.get(&parent).and_then(|w| w.parent);
        }
        depth
    }

    pub fn is_viewable(&self, id: u32) -> bool {
        let mut current = Some(id);
        while let Some(window) = current.and_then(|c| self.windows.get(&c)) {
            if !window.mapped {
                return false;
            }
            current = window.parent;
        }
        true
    }

    pub fn timestamp(&self) -> u32 {
        self.started.elapsed().as_millis() as u32
    }

    pub fn notify_listeners(&self, window: u32, mask: u32, event: &Event) {
        let Some(window) = self.windows.get(&window) else {
            return;
        };
        for (&client, &listening) in &window.listeners {
            if listening & mask == 0 {
                continue;
            }
            if let Some(info) = self.clients.get(&client) {
                info.send(event.encode(info.endian, info.seq));
            }
        }
    }

    /// Sends a structure event to the window and to its parent. `make` gets the event window.
    pub fn notify_structure(&self, window: u32, make: impl Fn(u32) -> Event) {
        let Some(parent) = self.windows.get(&window).map(|w| w.parent) else {
            return;
        };
        self.notify_listeners(window, STRUCTURE_NOTIFY, &make(window));
        if let Some(parent) = parent {
            self.notify_listeners(parent, SUBSTRUCTURE_NOTIFY, &make(parent));
        }
    }

    /// Destroys a window and its inferiors. Only the named window gets an `UnmapNotify`.
    pub fn destroy_window(&mut self, id: u32) {
        self.mark_window_dirty(id);
        self.destroy_tree(id, true);
    }

    fn destroy_tree(&mut self, id: u32, announce_unmap: bool) {
        let Some(window) = self.windows.get(&id) else {
            return;
        };
        let (mapped, parent) = (window.mapped, window.parent);
        if mapped && announce_unmap {
            if let Some(window) = self.windows.get_mut(&id) {
                window.mapped = false;
            }
            self.notify_structure(id, |event| Event::UnmapNotify { event, window: id });
        }
        let children = self.windows.get(&id).map(|w| w.children.clone());
        for child in children.into_iter().flatten().rev() {
            self.destroy_tree(child, false);
        }
        self.notify_structure(id, |event| Event::DestroyNotify { event, window: id });
        if let Some(parent) = parent.and_then(|p| self.windows.get_mut(&p)) {
            parent.children.retain(|&c| c != id);
        }
        self.forget_window_input(id);
        self.forget_selections(Some(id), None);
        self.selection_watchers
            .retain(|(_, _, window), _| *window != id);
        self.forget_window_pictures(id);
        let owner = self.owners.get(&id).copied();
        if let Some(window) = self.windows.remove(&id) {
            self.release_owned_surface(owner, &window.surface);
        }
        self.owners.remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::unbounded_channel;

    #[test]
    fn client_that_never_reads_is_marked_and_woken_for_disconnect() {
        let mut state = State::new(Display::new(640, 480));
        let (tx, _rx) = unbounded_channel();
        let signal = Rc::new(Notify::new());
        let (client, _) = state
            .add_client(
                Endian::Little,
                tx,
                Rc::new(Cell::new(0)),
                Rc::clone(&signal),
                Rc::new(Cell::new(false)),
            )
            .unwrap();
        state.send_raw(client, vec![0; MAX_QUEUED_BYTES]);
        assert!(!state.is_overflowed(client));
        // Another client causes this event. The idle client gets no request that could notice it.
        state.send_event_to(
            client,
            &Event::NoExpose {
                drawable: 1,
                major_opcode: 62,
            },
        );
        assert!(state.is_overflowed(client));

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let woken = runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_millis(200), signal.notified()).await
        });
        assert!(woken.is_ok(), "the connection task was not woken up");
    }

    #[test]
    fn closed_client_id_is_reused_without_old_selections() {
        let mut state = State::new(Display::new(640, 480));
        let (tx, _rx) = unbounded_channel();
        let (first, _) = state
            .add_client(
                Endian::Little,
                tx,
                Rc::new(Cell::new(0)),
                Rc::new(Notify::new()),
                Rc::new(Cell::new(false)),
            )
            .unwrap();
        state
            .window_mut(ROOT_WINDOW)
            .unwrap()
            .listeners
            .insert(first, SUBSTRUCTURE_NOTIFY);
        state.remove_client(first);
        assert!(state.window(ROOT_WINDOW).unwrap().listeners.is_empty());
    }
}
