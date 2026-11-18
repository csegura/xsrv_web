// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

use wire::{CursorShape, Input};

use super::event::{
    Event, MappingRequest, PointerEvent, BUTTON1_MOTION, BUTTON_MOTION, BUTTON_PRESS,
    BUTTON_RELEASE, ENTER_WINDOW, FOCUS_CHANGE, KEY_PRESS, KEY_RELEASE, LEAVE_WINDOW,
    POINTER_MOTION,
};
use super::keymap::{self, Layout, MAX_KEYCODE, MIN_KEYCODE};
use super::resources::IRect;
use super::screen::ROOT_WINDOW;
use super::state::{ClientId, State};
use super::wm::{Part, WmPress};

const BUTTON_STATE_BASE: u16 = 0x100;
const BUTTON_STATE_MASK: u16 = 0x1F00;
const MAX_STATE_BUTTON: u8 = 5;
pub const ANY_MODIFIER: u16 = 0x8000;

const OWNER_GRAB_BUTTON: u32 = 0x0100_0000;
const REVERT_TO_NONE: u8 = 0;
const REVERT_TO_POINTER_ROOT_VALUE: u8 = 1;
const REVERT_TO_PARENT: u8 = 2;

/// The `mode` of a crossing event: normal movement, a grab starts, a grab ends.
const CROSSING_NORMAL: u8 = 0;
pub(super) const CROSSING_GRAB: u8 = 1;
pub(super) const CROSSING_UNGRAB: u8 = 2;
const NOTIFY_ANCESTOR: u8 = 0;
const NOTIFY_VIRTUAL: u8 = 1;
const NOTIFY_INFERIOR: u8 = 2;
const NOTIFY_NONLINEAR: u8 = 3;
const NOTIFY_NONLINEAR_VIRTUAL: u8 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    None,
    PointerRoot,
    Window(u32),
}

#[derive(Clone, Copy)]
pub struct PointerGrab {
    pub client: ClientId,
    pub window: u32,
    /// `None` means that the client's own selection on the window decides.
    pub event_mask: Option<u32>,
    pub owner_events: bool,
    /// Implicit and passive grabs end when the last button is released.
    pub ends_with_buttons: bool,
}

#[derive(Clone, Copy)]
pub struct KeyboardGrab {
    pub client: ClientId,
    pub window: u32,
    pub owner_events: bool,
    pub release_key: Option<u8>,
}

#[derive(Clone, Copy)]
pub struct PassiveButton {
    pub client: ClientId,
    pub window: u32,
    pub button: u8,
    pub modifiers: u16,
    pub event_mask: u32,
    pub owner_events: bool,
    /// The grab froze the pointer events (`pointer_mode` Sync). AllowEvents can replay the press.
    pub sync: bool,
}

#[derive(Clone, Copy)]
pub struct PassiveKey {
    pub client: ClientId,
    pub window: u32,
    pub key: u8,
    pub modifiers: u16,
    pub owner_events: bool,
}

pub struct InputState {
    pub pointer: (i32, i32),
    buttons: u16,
    keys: [u8; 32],
    locks: u16,
    pub layout: keymap::Layout,
    /// The button press that a synchronous passive grab took: (client, button). The client may
    /// ask with AllowEvents ReplayPointer to give it to the windows below, as if there was no grab.
    pub replay: Option<(ClientId, u8)>,
    /// The button events (button, pressed) that came while a press waited for AllowEvents. They
    /// wait in order, as the events of a frozen pointer do, and run when the press is settled.
    pub frozen: Vec<(u8, bool)>,
    /// When the waiting press arrived, to end the wait of a manager that never answers.
    pub replay_since: Option<std::time::Instant>,
    /// The window that clients believe the pointer is in.
    pub pointer_window: u32,
    /// The window that the last enter and leave events named. It lags behind during a grab.
    pub crossing_window: u32,
    /// The page told us that the pointer left the screen.
    pub outside: bool,
    pub focus: Focus,
    pub revert_to: u8,
    pub pointer_grab: Option<PointerGrab>,
    pub keyboard_grab: Option<KeyboardGrab>,
    pub passive_buttons: Vec<PassiveButton>,
    pub passive_keys: Vec<PassiveKey>,
    /// A button press that the built-in window manager kept for itself.
    pub wm: Option<WmPress>,
    pub cursor_shown: CursorShape,
}

impl InputState {
    pub fn new() -> Self {
        Self {
            pointer: (0, 0),
            buttons: 0,
            keys: [0; 32],
            locks: 0,
            layout: keymap::Layout::default(),
            replay: None,
            frozen: Vec::new(),
            replay_since: None,
            pointer_window: ROOT_WINDOW,
            crossing_window: ROOT_WINDOW,
            outside: false,
            focus: Focus::PointerRoot,
            revert_to: 0,
            pointer_grab: None,
            keyboard_grab: None,
            passive_buttons: Vec::new(),
            passive_keys: Vec::new(),
            wm: None,
            cursor_shown: CursorShape::Default,
        }
    }

    pub fn key_down(&self, keycode: u8) -> bool {
        self.keys[usize::from(keycode / 8)] >> (keycode % 8) & 1 == 1
    }

    /// The Caps Lock and Num Lock bits.
    pub fn locks(&self) -> u16 {
        self.locks
    }

    pub fn set_locks(&mut self, locks: u16) {
        self.locks = locks & (keymap::LOCK_MASK | keymap::MOD2_MASK);
    }

    pub fn pressed_keys(&self) -> [u8; 32] {
        self.keys
    }

    fn set_key(&mut self, keycode: u8, down: bool) {
        let (byte, bit) = (usize::from(keycode / 8), keycode % 8);
        if down {
            self.keys[byte] |= 1 << bit;
        } else {
            self.keys[byte] &= !(1 << bit);
        }
    }

    /// The modifier and button bits of the `state` field of events.
    pub fn state(&self) -> u16 {
        let held = keymap::modifier_keycodes(self.layout)
            .filter(|key| self.key_down(*key))
            .filter_map(|key| keymap::held_modifier(self.layout, key))
            .fold(0, |all, bit| all | bit);
        held | self.locks | self.buttons
    }

    pub fn forget_client(&mut self, client: ClientId) {
        if self.pointer_grab.is_some_and(|g| g.client == client) {
            self.pointer_grab = None;
        }
        if self.keyboard_grab.is_some_and(|g| g.client == client) {
            self.keyboard_grab = None;
        }
        self.passive_buttons.retain(|g| g.client != client);
        self.passive_keys.retain(|g| g.client != client);
    }

    pub fn forget_window(&mut self, window: u32) {
        if self.pointer_grab.is_some_and(|g| g.window == window) {
            self.pointer_grab = None;
        }
        if self.keyboard_grab.is_some_and(|g| g.window == window) {
            self.keyboard_grab = None;
        }
        self.passive_buttons.retain(|g| g.window != window);
        self.passive_keys.retain(|g| g.window != window);
    }
}

fn button_bit(button: u8) -> u16 {
    if button <= MAX_STATE_BUTTON {
        BUTTON_STATE_BASE << (button - 1)
    } else {
        0
    }
}

impl State {
    pub fn handle_input(&mut self, input: Input) {
        let (keycode, event_type) = match input {
            Input::Key { keycode, pressed } => (keycode, if pressed { 2 } else { 3 }),
            Input::Button { pressed, .. } => (0, if pressed { 4 } else { 5 }),
            _ => (0, 0),
        };
        match input {
            Input::Pointer { x, y } => {
                let (width, height) = self.screen_size();
                let x = i32::from(x).min(i32::from(width) - 1);
                let y = i32::from(y).min(i32::from(height) - 1);
                self.move_pointer(x, y);
            }
            Input::Button { button, pressed } => self.button_input(button, pressed),
            Input::Key { keycode, pressed } => self.key_input(keycode, pressed),
            Input::PointerLeft => {
                self.input.outside = true;
                self.refresh_pointer_window();
            }
            Input::ReleaseAll => self.release_all(),
            Input::Layout { layout } => match Layout::from_index(layout) {
                Some(layout) => self.switch_layout(layout),
                None => self.log_once("page layout".into(), || {
                    format!(
                        "xsrv-web: the page asked for keyboard layout {layout}, which is unknown"
                    )
                }),
            },
            Input::Resize { width, height } => {
                if let Err(error) = self.set_screen_size(width, height) {
                    self.log_once("page resize".into(), || {
                        format!(
                            "xsrv-web: the page asked for a screen of {width}x{height}: {error:?}"
                        )
                    });
                }
            }
            Input::Locks { caps, num } => {
                let lock = |on: bool, mask: u16| if on { mask } else { 0 };
                self.input
                    .set_locks(lock(caps, keymap::LOCK_MASK) | lock(num, keymap::MOD2_MASK));
            }
        }
        self.xkb_report_state(keycode, event_type);
        self.after_request();
        self.flush_display();
    }

    /// Windows that changed under the pointer or under the focus produce events.
    pub fn after_request(&mut self) {
        // A change by a request (AllowEvents lets a held button go, LatchLockState) is reported too.
        self.xkb_report_state(0, 0);
        self.check_grabs();
        self.check_focus_window();
        if !self.dirty.is_empty() {
            self.refresh_pointer_window();
        }
        self.update_cursor();
    }

    /// The path from a window up to the root, the window first.
    pub(super) fn chain(&self, id: u32) -> Vec<u32> {
        let mut chain = Vec::new();
        let mut current = Some(id);
        while let Some(window) = current.filter(|w| self.windows.contains_key(w)) {
            chain.push(window);
            current = self.windows[&window].parent;
        }
        chain
    }

    /// The topmost mapped window that contains the point.
    pub(super) fn window_at(&self, x: i32, y: i32) -> u32 {
        let mut current = ROOT_WINDOW;
        let mut origin = (0, 0);
        loop {
            let Some(window) = self.windows.get(&current) else {
                return ROOT_WINDOW;
            };
            let hit = window.children.iter().rev().find_map(|child| {
                let child_window = self.windows.get(child).filter(|c| c.mapped)?;
                let border = i32::from(child_window.border);
                let outer = IRect::new(
                    origin.0 + i32::from(child_window.x),
                    origin.1 + i32::from(child_window.y),
                    i32::from(child_window.width) + 2 * border,
                    i32::from(child_window.height) + 2 * border,
                );
                let inside = x >= outer.x0 && x < outer.x1 && y >= outer.y0 && y < outer.y1;
                let origin = (outer.x0 + border, outer.y0 + border);
                (inside && self.shape_hit(*child, x - origin.0, y - origin.1))
                    .then_some((*child, origin))
            });
            match hit {
                Some((child, child_origin)) => {
                    current = child;
                    origin = child_origin;
                }
                None => return current,
            }
        }
    }

    /// The child of `window` on the way to the pointer, or 0.
    /// The child of `window` that is `target` or one of its ancestors. 0 when there is none.
    fn child_of_toward(&self, window: u32, target: u32) -> u32 {
        let chain = self.chain(target);
        match chain.iter().position(|w| *w == window) {
            Some(index) if index > 0 => chain[index - 1],
            _ => 0,
        }
    }

    fn child_toward_pointer(&self, window: u32) -> u32 {
        let chain = self.chain(self.input.pointer_window);
        match chain.iter().position(|w| *w == window) {
            Some(index) if index > 0 => chain[index - 1],
            _ => 0,
        }
    }

    pub(super) fn pointer_fields(&self, event_window: u32, detail: u8) -> PointerEvent {
        let (root_x, root_y) = self.input.pointer;
        let (origin_x, origin_y) = self.window_origin(event_window).unwrap_or((0, 0));
        PointerEvent {
            detail,
            time: self.timestamp(),
            root: ROOT_WINDOW,
            event: event_window,
            child: self.child_toward_pointer(event_window),
            root_x: root_x as i16,
            root_y: root_y as i16,
            event_x: (root_x - origin_x) as i16,
            event_y: (root_y - origin_y) as i16,
            state: self.input.state(),
        }
    }

    pub(super) fn listeners_with(
        &self,
        window: u32,
        mask: u32,
        only: Option<ClientId>,
    ) -> Vec<ClientId> {
        let Some(window) = self.windows.get(&window) else {
            return Vec::new();
        };
        window
            .listeners
            .iter()
            .filter(|(client, selected)| {
                **selected & mask != 0 && only.is_none_or(|wanted| wanted == **client)
            })
            .map(|(client, _)| *client)
            .collect()
    }

    /// Reports an event to the first window of `chain` that selected `mask`.
    /// It stops early at a window that does not propagate this kind of event.
    fn deliver_up(
        &self,
        chain: &[u32],
        mask: u32,
        only: Option<ClientId>,
        make: &dyn Fn(u32) -> Event,
    ) -> Option<(u32, ClientId)> {
        for &window in chain {
            let clients = self.listeners_with(window, mask, only);
            if let Some(&first) = clients.first() {
                let event = make(window);
                for client in clients {
                    self.send_event_to(client, &event);
                }
                return Some((window, first));
            }
            let blocked = self
                .windows
                .get(&window)
                .is_some_and(|w| u32::from(w.do_not_propagate) & mask != 0);
            if blocked {
                return None;
            }
        }
        None
    }

    /// Returns the window and client that got the event.
    fn deliver_pointer(&self, mask: u32, make: &dyn Fn(u32) -> Event) -> Option<(u32, ClientId)> {
        let chain = self.chain(self.input.pointer_window);
        let Some(grab) = self.input.pointer_grab else {
            return self.deliver_up(&chain, mask, None, make);
        };
        if grab.owner_events {
            if let Some(delivered) = self.deliver_up(&chain, mask, Some(grab.client), make) {
                return Some(delivered);
            }
        }
        let allowed = grab
            .event_mask
            .or_else(|| {
                let window = self.windows.get(&grab.window)?;
                window.listeners.get(&grab.client).copied()
            })
            .unwrap_or(0);
        if allowed & mask == 0 {
            return None;
        }
        self.send_event_to(grab.client, &make(grab.window));
        Some((grab.window, grab.client))
    }

    fn move_pointer(&mut self, x: i32, y: i32) {
        let was_outside = std::mem::take(&mut self.input.outside);
        if self.input.pointer == (x, y) {
            if was_outside {
                self.refresh_pointer_window();
            }
            return;
        }
        self.input.pointer = (x, y);
        if self.wm_drag_to(x, y) {
            self.refresh_pointer_window();
            return;
        }
        self.refresh_pointer_window();
        let mut mask = POINTER_MOTION;
        if self.input.buttons & BUTTON_STATE_MASK != 0 {
            mask |= BUTTON_MOTION;
            for button in 1..=MAX_STATE_BUTTON {
                if self.input.buttons & button_bit(button) != 0 {
                    mask |= BUTTON1_MOTION << (button - 1);
                }
            }
        }
        self.deliver_pointer(mask, &|window| Event::Motion {
            fields: self.pointer_fields(window, 0),
        });
    }

    /// Finds the window under the pointer. Enter and leave events follow; during a grab only the grabbing client gets them.
    /// The window is tracked during a grab too, so that grab events use the real window.
    pub(super) fn refresh_pointer_window(&mut self) {
        self.refresh_pointer_window_with(CROSSING_NORMAL);
    }

    /// The same as `refresh_pointer_window`, with the `mode` that the crossing events carry.
    pub(super) fn refresh_pointer_window_with(&mut self, mode: u8) {
        let (x, y) = self.input.pointer;
        let new = if self.input.outside {
            ROOT_WINDOW
        } else {
            self.window_at(x, y)
        };
        let previous = std::mem::replace(&mut self.input.pointer_window, new);
        if let Some(grab) = self.input.pointer_grab {
            if new != previous {
                self.send_grabbed_crossings(grab, previous, new);
            }
            return;
        }
        let old = self.input.crossing_window;
        if new == old {
            return;
        }
        let steps = self.crossing_steps(old, new);
        self.input.crossing_window = new;
        for (window, enter, detail) in steps {
            let mask = if enter { ENTER_WINDOW } else { LEAVE_WINDOW };
            let event = self.crossing_event(window, enter, detail, (old, new), mode);
            self.notify_listeners(window, mask, &event);
        }
    }

    /// The crossing event for one step from window `old` to window `new`.
    fn crossing_event(
        &self,
        window: u32,
        enter: bool,
        detail: u8,
        (old, new): (u32, u32),
        mode: u8,
    ) -> Event {
        // The protocol: with detail Inferior, Virtual or NonlinearVirtual, `child` is the child
        // of the event window on the way to the window that is below it. The other details
        // have no child.
        let below = match (detail, enter) {
            (NOTIFY_INFERIOR, true) | (NOTIFY_VIRTUAL | NOTIFY_NONLINEAR_VIRTUAL, false) => {
                Some(old)
            }
            (NOTIFY_INFERIOR, false) | (NOTIFY_VIRTUAL | NOTIFY_NONLINEAR_VIRTUAL, true) => {
                Some(new)
            }
            _ => None,
        };
        let child = below.map_or(0, |target| self.child_of_toward(window, target));
        Event::Crossing {
            enter,
            fields: PointerEvent {
                child,
                ..self.pointer_fields(window, detail)
            },
            mode,
            focus: self.window_has_focus(window),
        }
    }

    /// The pointer goes from one window to another while a grab is active. Only the client that
    /// grabbed gets the crossing events (mode Normal): the windows that it selected them on when
    /// the grab has owner-events, and the grab window when the event mask of the grab has them.
    /// Toolkits such as GTK 3 highlight a menu item from these events.
    fn send_grabbed_crossings(&self, grab: PointerGrab, old: u32, new: u32) {
        let allowed_on_grab_window = grab
            .event_mask
            .or_else(|| {
                let window = self.windows.get(&grab.window)?;
                window.listeners.get(&grab.client).copied()
            })
            .unwrap_or(0);
        for (window, enter, detail) in self.crossing_steps(old, new) {
            let mask = if enter { ENTER_WINDOW } else { LEAVE_WINDOW };
            let selected = self
                .windows
                .get(&window)
                .and_then(|w| w.listeners.get(&grab.client))
                .copied()
                .unwrap_or(0);
            let by_selection = grab.owner_events && selected & mask != 0;
            let by_grab_mask = window == grab.window && allowed_on_grab_window & mask != 0;
            if by_selection || by_grab_mask {
                let event = self.crossing_event(window, enter, detail, (old, new), CROSSING_NORMAL);
                self.send_event_to(grab.client, &event);
            }
        }
    }

    /// A pointer grab starts: the pointer moves to the grab window for the clients, with crossing
    /// events of mode Grab. When the grab ends, `refresh_pointer_window_with` sends mode Ungrab.
    pub(super) fn grab_crossing(&mut self, grab_window: u32) {
        let old = self.input.crossing_window;
        if old == grab_window {
            return;
        }
        let steps = self.crossing_steps(old, grab_window);
        self.input.crossing_window = grab_window;
        for (window, enter, detail) in steps {
            let mask = if enter { ENTER_WINDOW } else { LEAVE_WINDOW };
            let event = Event::Crossing {
                enter,
                fields: PointerEvent {
                    child: 0,
                    ..self.pointer_fields(window, detail)
                },
                mode: CROSSING_GRAB,
                focus: self.window_has_focus(window),
            };
            self.notify_listeners(window, mask, &event);
        }
    }

    /// The pointer shape of the window under the pointer, or of its closest ancestor with a cursor.
    fn cursor_under_pointer(&self) -> CursorShape {
        if !self.hidden_cursors.is_empty() {
            return CursorShape::Hidden;
        }
        let (x, y) = self.input.pointer;
        if self.input.pointer_grab.is_none() && !self.input.outside {
            if let Some(hit) = self.wm_hit(x, y) {
                match hit.part {
                    Part::Border {
                        right: true,
                        bottom: true,
                    } => return CursorShape::ResizeNwse,
                    Part::Border { right: true, .. } => return CursorShape::ResizeEw,
                    Part::Border { bottom: true, .. } => return CursorShape::ResizeNs,
                    Part::Title | Part::Close => return CursorShape::Default,
                    _ => {}
                }
            }
        }
        self.chain(self.input.pointer_window)
            .into_iter()
            .filter_map(|id| self.windows.get(&id))
            .find(|window| window.cursor != 0)
            .and_then(|window| self.cursors.get(&window.cursor).copied())
            .unwrap_or_default()
    }

    /// Tells the pages when the pointer shape changes.
    fn update_cursor(&mut self) {
        let shape = self.cursor_under_pointer();
        if shape != self.input.cursor_shown {
            self.input.cursor_shown = shape;
            self.display.set_cursor(shape);
        }
    }

    /// True when the window is the focus window or one of its inferiors.
    fn window_has_focus(&self, window: u32) -> bool {
        match self.input.focus {
            Focus::None => false,
            Focus::PointerRoot => true,
            Focus::Window(focus) => self.chain(window).contains(&focus),
        }
    }

    /// The enter and leave events for a move from `old` to `new`, as (window, enter, detail).
    fn crossing_steps(&self, old: u32, new: u32) -> Vec<(u32, bool, u8)> {
        if old == new {
            return Vec::new();
        }
        let old_chain = self.chain(old);
        let new_chain = self.chain(new);
        let common = old_chain
            .iter()
            .copied()
            .find(|window| new_chain.contains(window))
            .unwrap_or(ROOT_WINDOW);
        let position = |chain: &[u32]| chain.iter().position(|w| *w == common).unwrap_or(0);
        let (old_common, new_common) = (position(&old_chain), position(&new_chain));
        // Windows strictly between a window and the common ancestor, starting at the window.
        let old_between = old_chain.get(1..old_common).unwrap_or(&[]);
        let new_between: Vec<u32> = new_chain
            .get(1..new_common)
            .unwrap_or(&[])
            .iter()
            .rev()
            .copied()
            .collect();

        let mut steps = Vec::new();
        if common == old {
            steps.push((old, false, NOTIFY_INFERIOR));
            steps.extend(new_between.iter().map(|w| (*w, true, NOTIFY_VIRTUAL)));
            steps.push((new, true, NOTIFY_ANCESTOR));
        } else if common == new {
            steps.push((old, false, NOTIFY_ANCESTOR));
            steps.extend(old_between.iter().map(|w| (*w, false, NOTIFY_VIRTUAL)));
            steps.push((new, true, NOTIFY_INFERIOR));
        } else {
            steps.push((old, false, NOTIFY_NONLINEAR));
            steps.extend(
                old_between
                    .iter()
                    .map(|w| (*w, false, NOTIFY_NONLINEAR_VIRTUAL)),
            );
            steps.extend(
                new_between
                    .iter()
                    .map(|w| (*w, true, NOTIFY_NONLINEAR_VIRTUAL)),
            );
            steps.push((new, true, NOTIFY_NONLINEAR));
        }
        steps
    }

    fn button_input(&mut self, button: u8, pressed: bool) {
        if self.input.replay.is_some() {
            // A press waits for AllowEvents: the pointer is frozen, and this event waits behind it.
            self.input.frozen.push((button, pressed));
            return;
        }
        let bit = button_bit(button);
        if pressed {
            if self.wm_button_press(button) {
                self.input.buttons |= bit;
                return;
            }
            self.input.replay = None;
            if self.input.pointer_grab.is_none() {
                if let Some(grab) = self.activate_passive_button_grab(button) {
                    if grab.sync {
                        self.input.replay = Some((grab.client, button));
                        self.input.replay_since = Some(std::time::Instant::now());
                        self.tick_wake.notify_one();
                    }
                }
            }
            self.deliver_button_press(button);
            self.input.buttons |= bit;
            return;
        }
        if self.wm_button_release(button) {
            self.input.buttons &= !bit;
            return;
        }
        self.deliver_pointer(BUTTON_RELEASE, &|window| Event::Button {
            press: false,
            fields: self.pointer_fields(window, button),
        });
        self.input.buttons &= !bit;
        let released_all = self.input.buttons & BUTTON_STATE_MASK == 0;
        if released_all && self.input.pointer_grab.is_some_and(|g| g.ends_with_buttons) {
            self.input.pointer_grab = None;
            self.refresh_pointer_window();
        }
    }

    /// Gives a button press to the windows under the pointer. When nobody has a grab, the client
    /// that got the press holds an implicit grab until the buttons are released.
    fn deliver_button_press(&mut self, button: u8) {
        let delivered = self.deliver_pointer(BUTTON_PRESS, &|window| Event::Button {
            press: true,
            fields: self.pointer_fields(window, button),
        });
        if let (None, Some((window, client))) = (self.input.pointer_grab, delivered) {
            let selected = self
                .windows
                .get(&window)
                .and_then(|w| w.listeners.get(&client).copied())
                .unwrap_or(0);
            self.input.pointer_grab = Some(PointerGrab {
                client,
                window,
                event_mask: None,
                owner_events: selected & OWNER_GRAB_BUTTON != 0,
                ends_with_buttons: true,
            });
        }
    }

    /// AllowEvents ReplayPointer: the passive grab ends, and the press that it took goes to the
    /// windows below as if the grab had never been there. This is how a window manager can look
    /// at a click (to give the focus) and still pass it to the application.
    pub(super) fn replay_button_press(&mut self, client: ClientId) {
        let Some((owner, button)) = self.input.replay else {
            return;
        };
        if owner != client {
            return;
        }
        self.input.replay = None;
        self.input.pointer_grab = None;
        let bit = button_bit(button);
        // The replayed event must not show its own button as already down.
        self.input.buttons &= !bit;
        self.deliver_button_press(button);
        self.input.buttons |= bit;
        self.run_frozen_buttons();
    }

    /// The button events that waited behind a press run now, in order. One of them may start
    /// another wait, and the ones after it wait again.
    fn run_frozen_buttons(&mut self) {
        let queued = std::mem::take(&mut self.input.frozen);
        for (button, pressed) in queued {
            self.button_input(button, pressed);
        }
    }

    /// A manager that holds a press for too long (it never calls AllowEvents) gets the pointer
    /// back: the waiting events run as normal events, as if it had asked for AsyncPointer.
    /// Changes the keyboard layout of the session while clients run. Keys that are down go up
    /// first, under the old layout; then every client hears that the keyboard and the modifier
    /// mappings changed, and reads them again (Xlib does so on MappingNotify).
    pub fn switch_layout(&mut self, layout: Layout) {
        if layout == self.input.layout {
            return;
        }
        self.release_all();
        self.set_layout(layout);
        let keyboard = Event::MappingNotify {
            request: MappingRequest::Keyboard,
            first_keycode: MIN_KEYCODE,
            count: MAX_KEYCODE - MIN_KEYCODE + 1,
        };
        let modifier = Event::MappingNotify {
            request: MappingRequest::Modifier,
            first_keycode: 0,
            count: 0,
        };
        self.send_event_to_all(&modifier);
        self.send_event_to_all(&keyboard);
        self.xkb_keymap_changed();
    }

    pub fn thaw_stale_pointer(&mut self) {
        const LONGEST_WAIT: std::time::Duration = std::time::Duration::from_millis(500);
        if self
            .input
            .replay_since
            .is_some_and(|since| since.elapsed() >= LONGEST_WAIT)
        {
            self.input.replay_since = None;
            self.finish_held_release();
            self.xkb_report_state(0, 0);
        }
    }

    /// AllowEvents that lets the held events through to the client that holds the grab (not a
    /// replay): the release that was held back is delivered as a normal release.
    pub(super) fn finish_held_release(&mut self) {
        if self.input.replay.take().is_some() {
            self.run_frozen_buttons();
        }
    }

    fn activate_passive_button_grab(&mut self, button: u8) -> Option<PassiveButton> {
        let state = self.input.state() & 0x00FF;
        let chain = self.chain(self.input.pointer_window);
        let found = chain.iter().rev().find_map(|window| {
            self.input.passive_buttons.iter().find(|grab| {
                grab.window == *window
                    && (grab.button == 0 || grab.button == button)
                    && (grab.modifiers == ANY_MODIFIER || grab.modifiers == state)
            })
        });
        let grab = found.copied()?;
        self.input.pointer_grab = Some(PointerGrab {
            client: grab.client,
            window: grab.window,
            event_mask: Some(grab.event_mask),
            owner_events: grab.owner_events,
            ends_with_buttons: true,
        });
        Some(grab)
    }

    /// The windows that a key event can reach, from the first to the last.
    fn key_chain(&self) -> Vec<u32> {
        let pointer_chain = self.chain(self.input.pointer_window);
        match self.input.focus {
            Focus::None => Vec::new(),
            Focus::PointerRoot => pointer_chain,
            Focus::Window(focus) => match pointer_chain.iter().position(|w| *w == focus) {
                Some(index) => pointer_chain[..=index].to_vec(),
                None => vec![focus],
            },
        }
    }

    fn key_input(&mut self, keycode: u8, pressed: bool) {
        if pressed && self.input.keyboard_grab.is_none() {
            self.activate_passive_key_grab(keycode);
        }
        let mask = if pressed { KEY_PRESS } else { KEY_RELEASE };
        let make = |window: u32| Event::Key {
            press: pressed,
            fields: self.pointer_fields(window, keycode),
        };
        match self.input.keyboard_grab {
            Some(grab) => {
                let reported = grab.owner_events
                    && self
                        .deliver_up(&self.key_chain(), mask, Some(grab.client), &make)
                        .is_some();
                if !reported {
                    self.send_event_to(grab.client, &make(grab.window));
                }
            }
            None => {
                self.deliver_up(&self.key_chain(), mask, None, &make);
            }
        }
        self.input.set_key(keycode, pressed);
        let ends_grab = self
            .input
            .keyboard_grab
            .is_some_and(|g| g.release_key == Some(keycode));
        if !pressed && ends_grab {
            self.input.keyboard_grab = None;
        }
    }

    /// Lets go of every key and button that is down, with the events that go with it.
    fn release_all(&mut self) {
        for button in 1..=MAX_STATE_BUTTON {
            if self.input.buttons & button_bit(button) != 0 {
                self.button_input(button, false);
            }
        }
        for keycode in keymap::MIN_KEYCODE..=keymap::MAX_KEYCODE {
            if self.input.key_down(keycode) {
                self.key_input(keycode, false);
            }
        }
    }

    fn activate_passive_key_grab(&mut self, keycode: u8) {
        let state = self.input.state() & 0x00FF;
        let ancestry = match self.input.focus {
            Focus::None => Vec::new(),
            Focus::PointerRoot => self.chain(self.input.pointer_window),
            Focus::Window(focus) => {
                let pointer_chain = self.chain(self.input.pointer_window);
                if pointer_chain.contains(&focus) {
                    pointer_chain
                } else {
                    self.chain(focus)
                }
            }
        };
        let found = ancestry.iter().rev().find_map(|window| {
            self.input.passive_keys.iter().find(|grab| {
                grab.window == *window
                    && (grab.key == 0 || grab.key == keycode)
                    && (grab.modifiers == ANY_MODIFIER || grab.modifiers == state)
            })
        });
        if let Some(grab) = found.copied() {
            self.input.keyboard_grab = Some(KeyboardGrab {
                client: grab.client,
                window: grab.window,
                owner_events: grab.owner_events,
                release_key: Some(keycode),
            });
        }
    }

    /// Moves the keyboard focus and tells the windows that lose and gain it.
    pub(super) fn set_focus(&mut self, focus: Focus, revert_to: u8) {
        let old = self.input.focus;
        self.input.focus = focus;
        self.input.revert_to = revert_to;
        if old == focus {
            return;
        }
        for window in [old, focus] {
            if let Focus::Window(window) = window {
                self.mark_window_dirty(window);
            }
        }
        // Between two windows the events follow the same path as the pointer crossing events:
        // Ancestor, Virtual, Inferior, Nonlinear and NonlinearVirtual.
        self.send_focus_events(old, focus, CROSSING_NORMAL);
    }

    /// The focus events for a move of the focus from `old` to `new`, with the given `mode`.
    pub(super) fn send_focus_events(&mut self, old: Focus, new: Focus, mode: u8) {
        for (window, focus_in, detail) in self.focus_steps(old, new) {
            let event = Event::Focus {
                focus_in,
                event: window,
                detail,
                mode,
            };
            self.notify_listeners(window, FOCUS_CHANGE, &event);
        }
    }

    /// The FocusOut and FocusIn events of a focus change, as (window, focus_in, detail). The root
    /// window stands for PointerRoot and None, with the details NotifyPointerRoot and NotifyDetailNone.
    fn focus_steps(&self, old: Focus, new: Focus) -> Vec<(u32, bool, u8)> {
        const NOTIFY_POINTER_ROOT: u8 = 6;
        const NOTIFY_DETAIL_NONE: u8 = 7;
        let special = |focus: Focus| match focus {
            Focus::PointerRoot => NOTIFY_POINTER_ROOT,
            _ => NOTIFY_DETAIL_NONE,
        };
        match (old, new) {
            (Focus::Window(from), Focus::Window(to)) => self.crossing_steps(from, to),
            (Focus::Window(from), other) => {
                let mut steps = vec![(from, false, NOTIFY_NONLINEAR)];
                let chain = self.chain(from);
                steps.extend(
                    chain
                        .iter()
                        .skip(1)
                        .map(|w| (*w, false, NOTIFY_NONLINEAR_VIRTUAL)),
                );
                steps.push((ROOT_WINDOW, true, special(other)));
                steps
            }
            (other, Focus::Window(to)) => {
                let mut steps = vec![(ROOT_WINDOW, false, special(other))];
                let chain = self.chain(to);
                steps.extend(
                    chain
                        .iter()
                        .skip(1)
                        .rev()
                        .filter(|w| **w != ROOT_WINDOW)
                        .map(|w| (*w, true, NOTIFY_NONLINEAR_VIRTUAL)),
                );
                steps.push((to, true, NOTIFY_NONLINEAR));
                steps
            }
            (from, to) => vec![
                (ROOT_WINDOW, false, special(from)),
                (ROOT_WINDOW, true, special(to)),
            ],
        }
    }

    /// Ends the grabs whose window is not viewable any more.
    fn check_grabs(&mut self) {
        if self
            .input
            .pointer_grab
            .is_some_and(|g| !self.is_viewable(g.window))
        {
            self.input.pointer_grab = None;
        }
        if self
            .input
            .keyboard_grab
            .is_some_and(|g| !self.is_viewable(g.window))
        {
            self.input.keyboard_grab = None;
        }
    }

    /// A focus window that is gone or not viewable gives the focus back.
    fn check_focus_window(&mut self) {
        let Focus::Window(window) = self.input.focus else {
            return;
        };
        if !self.windows.contains_key(&window) || !self.is_viewable(window) {
            self.revert_focus(window);
        }
    }

    /// Moves the focus as `revert_to` says. Afterwards `revert_to` is None, as the protocol says.
    fn revert_focus(&mut self, window: u32) {
        let target = match self.input.revert_to {
            REVERT_TO_PARENT => self
                .viewable_ancestor(window)
                .map_or(Focus::PointerRoot, Focus::Window),
            REVERT_TO_NONE => Focus::None,
            _ => Focus::PointerRoot,
        };
        // With the built-in manager, the focus goes to the window that is on top, as it does in
        // most managers, and not to the root where no window gets the keys.
        if let (Focus::PointerRoot, Some(top)) = (target, self.top_managed_window(window)) {
            self.set_focus(Focus::Window(top), REVERT_TO_POINTER_ROOT_VALUE);
            return;
        }
        self.set_focus(target, REVERT_TO_NONE);
    }

    /// The closest ancestor that is viewable and not the root.
    fn viewable_ancestor(&self, window: u32) -> Option<u32> {
        let mut current = self.windows.get(&window)?.parent;
        while let Some(ancestor) = current.filter(|w| *w != ROOT_WINDOW) {
            if self.is_viewable(ancestor) {
                return Some(ancestor);
            }
            current = self.windows.get(&ancestor)?.parent;
        }
        None
    }

    /// Called when a window is being destroyed, before it is removed.
    pub(super) fn forget_window_input(&mut self, window: u32) {
        self.input.forget_window(window);
        self.wm_forget_window(window);
        if self.input.focus == Focus::Window(window) {
            self.revert_focus(window);
        }
        let parent = self
            .windows
            .get(&window)
            .and_then(|w| w.parent)
            .unwrap_or(ROOT_WINDOW);
        let pointer_was_in_it = self.input.crossing_window == window;
        for tracked in [
            &mut self.input.pointer_window,
            &mut self.input.crossing_window,
        ] {
            if *tracked == window {
                *tracked = parent;
            }
        }
        // The pointer is now in the parent, as if it had moved up out of the window: the parent
        // gets EnterNotify with detail Inferior. The window is gone, so it gets no LeaveNotify.
        if pointer_was_in_it && self.input.pointer_grab.is_none() && !self.input.outside {
            let event = Event::Crossing {
                enter: true,
                fields: PointerEvent {
                    child: 0,
                    ..self.pointer_fields(parent, NOTIFY_INFERIOR)
                },
                mode: 0,
                focus: self.window_has_focus(parent),
            };
            self.notify_listeners(parent, ENTER_WINDOW, &event);
        }
    }
}
