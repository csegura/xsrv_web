// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! A small built-in window manager. It draws a frame with a title bar and a close button around
//! every top-level window, raises and focuses a window on a click, moves it when its title bar is
//! dragged, and closes it politely with `WM_DELETE_WINDOW`. The windows are not reparented: the
//! frame is painted around them by the compositor. It is off while another client has selected
//! SubstructureRedirect on the root window, because that client is then the window manager.
use super::codec::Endian;
use super::event::{Event, SUBSTRUCTURE_REDIRECT};
use super::input::Focus;
use super::resources::IRect;
use super::screen::ROOT_WINDOW;
use super::state::{State, CLASS_INPUT_OUTPUT};

pub const FRAME: i32 = 4;
const MIN_SIZE: i32 = 40;
pub const TITLE: i32 = 18;
const BUTTON: i32 = 14;
const KEEP_VISIBLE: i32 = 40;
const WM_NAME: u32 = 39;
const CLIENT_MESSAGE: u8 = 33;
const STACK_ABOVE: u8 = 0;
const REVERT_TO_POINTER_ROOT: u8 = 1;

#[derive(Clone, Copy)]
pub struct Frame {
    pub outer: IRect,
    pub title: IRect,
    pub close: IRect,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Part {
    Client,
    Title,
    Close,
    /// The edge of the frame. Dragging the right or the bottom one resizes the window.
    Border {
        right: bool,
        bottom: bool,
    },
}

#[derive(Clone, Copy)]
pub struct WmHit {
    pub window: u32,
    pub part: Part,
}

/// A button press that the window manager kept for itself, and the drag that it started.
#[derive(Clone, Copy)]
pub struct WmPress {
    /// The buttons that the manager kept, one bit for each button number.
    pub buttons: u8,
    /// The window that moves, and where inside the window the pointer holds it.
    pub drag: Option<(u32, (i32, i32))>,
    /// The window that is resized, and which of its edges follow the pointer.
    pub resize: Option<(u32, bool, bool)>,
}

impl State {
    /// True when the built-in window manager must not act: it is off, or another client has
    /// selected SubstructureRedirect on the root window and is the window manager.
    pub(super) fn builtin_wm_inactive(&self) -> bool {
        !self.window_manager_enabled()
            || self.windows.get(&ROOT_WINDOW).is_some_and(|root| {
                root.listeners
                    .values()
                    .any(|mask| mask & SUBSTRUCTURE_REDIRECT != 0)
            })
    }

    fn is_managed(&self, id: u32) -> bool {
        let Some(window) = self.windows.get(&id) else {
            return false;
        };
        window.parent == Some(ROOT_WINDOW)
            && window.mapped
            && window.class == CLASS_INPUT_OUTPUT
            && !window.override_redirect
            && !self.builtin_wm_inactive()
    }

    pub(super) fn frame_of(&self, id: u32) -> Option<Frame> {
        if !self.is_managed(id) {
            return None;
        }
        let window = self.windows.get(&id)?;
        let border = i32::from(window.border);
        let (x, y) = (i32::from(window.x), i32::from(window.y));
        let width = i32::from(window.width) + 2 * border;
        let height = i32::from(window.height) + 2 * border;
        Some(Frame {
            outer: IRect {
                x0: x - FRAME,
                y0: y - TITLE - FRAME,
                x1: x + width + FRAME,
                y1: y + height + FRAME,
            },
            title: IRect {
                x0: x,
                y0: y - TITLE,
                x1: x + width,
                y1: y,
            },
            close: IRect::new(x + width - BUTTON - 2, y - TITLE + 2, BUTTON, BUTTON),
        })
    }

    /// The title that the frame shows: the printable ASCII of the WM_NAME property.
    pub(super) fn window_title(&self, id: u32) -> Vec<u8> {
        let Some(property) = self
            .windows
            .get(&id)
            .and_then(|w| w.properties.get(&WM_NAME))
        else {
            return Vec::new();
        };
        if property.format != 8 {
            return Vec::new();
        }
        property
            .data
            .iter()
            .map(|&byte| {
                if (0x20..0x7F).contains(&byte) {
                    byte
                } else {
                    b'?'
                }
            })
            .collect()
    }

    /// What the point is over, looking at the top-level windows from the top.
    pub(super) fn wm_hit(&self, x: i32, y: i32) -> Option<WmHit> {
        let root = self.windows.get(&ROOT_WINDOW)?;
        for &id in root.children.iter().rev() {
            let Some(window) = self.windows.get(&id).filter(|w| w.mapped) else {
                continue;
            };
            let border = i32::from(window.border);
            let body = IRect::new(
                i32::from(window.x),
                i32::from(window.y),
                i32::from(window.width) + 2 * border,
                i32::from(window.height) + 2 * border,
            );
            if body.contains(x, y) {
                return self.is_managed(id).then_some(WmHit {
                    window: id,
                    part: Part::Client,
                });
            }
            let Some(frame) = self.frame_of(id) else {
                continue;
            };
            if frame.outer.contains(x, y) {
                let part = if frame.close.contains(x, y) {
                    Part::Close
                } else if frame.title.contains(x, y) {
                    Part::Title
                } else {
                    Part::Border {
                        right: x >= body.x1,
                        bottom: y >= body.y1,
                    }
                };
                return Some(WmHit { window: id, part });
            }
        }
        None
    }

    /// The window of the built-in manager that is highest in the stack, other than `except`.
    pub(super) fn top_managed_window(&self, except: u32) -> Option<u32> {
        self.windows
            .get(&ROOT_WINDOW)?
            .children
            .iter()
            .rev()
            .copied()
            .find(|id| *id != except && self.is_managed(*id) && self.is_viewable(*id))
    }

    pub(super) fn raise_and_focus(&mut self, id: u32) {
        if self.restack(id, STACK_ABOVE, None).is_ok() {
            self.mark_window_dirty(id);
        }
        if self.is_viewable(id) {
            self.set_focus(Focus::Window(id), REVERT_TO_POINTER_ROOT);
        }
    }

    /// Asks the client to close, or destroys the window when it does not understand the request.
    pub(super) fn wm_close(&mut self, id: u32) {
        let Some(&owner) = self.owners.get(&id) else {
            return;
        };
        let (Some(protocols), Some(delete)) = (
            self.atoms.lookup(b"WM_PROTOCOLS"),
            self.atoms.lookup(b"WM_DELETE_WINDOW"),
        ) else {
            return self.destroy_window(id);
        };
        let endian = self.client_endian(owner).unwrap_or(Endian::Little);
        let supports_delete = self
            .windows
            .get(&id)
            .and_then(|w| w.properties.get(&protocols))
            .is_some_and(|property| {
                property.format == 32
                    && property.data.as_chunks::<4>().0.iter().any(|atom| {
                        let value = match endian {
                            Endian::Little => u32::from_le_bytes(*atom),
                            Endian::Big => u32::from_be_bytes(*atom),
                        };
                        value == delete
                    })
            });
        if !supports_delete {
            return self.destroy_window(id);
        }
        let mut bytes = [0u8; 32];
        bytes[0] = CLIENT_MESSAGE;
        bytes[1] = 32;
        for (index, field) in [id, protocols, delete, self.timestamp()].iter().enumerate() {
            let start = if index == 0 { 4 } else { 8 + (index - 1) * 4 };
            bytes[start..start + 4].copy_from_slice(&field.to_le_bytes());
        }
        let event = Event::Raw {
            bytes,
            from: Endian::Little,
        };
        self.send_event_to(owner, &event);
    }

    /// Moves a top-level window so that its title bar stays reachable.
    pub(super) fn move_managed(&mut self, id: u32, x: i32, y: i32) {
        self.maximized.remove(&id);
        let Some(window) = self.windows.get(&id) else {
            return;
        };
        let width = i32::from(window.width) + 2 * i32::from(window.border);
        let (screen_width, screen_height) = self.screen_size();
        // The bounds are ordered, so a small screen or a wide window cannot make `clamp` panic.
        let min_x = (KEEP_VISIBLE - width).max(i32::from(i16::MIN));
        let max_x = (i32::from(screen_width) - KEEP_VISIBLE).clamp(min_x, i32::from(i16::MAX));
        let min_y = TITLE + FRAME;
        let max_y = (i32::from(screen_height) - KEEP_VISIBLE).max(min_y);
        let (x, y) = (x.clamp(min_x, max_x), y.clamp(min_y, max_y));
        if (x, y) == (i32::from(window.x), i32::from(window.y)) {
            return;
        }
        self.move_with_screen_copy(id, |state| {
            if let Some(window) = state.windows.get_mut(&id) {
                window.x = x as i16;
                window.y = y as i16;
            }
        });
        self.notify_configure(id);
    }

    /// A new top-level window gets a visible title bar, the top of the stack and the focus.
    pub(super) fn place_new_window(&mut self, id: u32) {
        if !self.is_managed(id) {
            return;
        }
        let Some(window) = self.windows.get(&id) else {
            return;
        };
        let (x, y) = (i32::from(window.x), i32::from(window.y));
        if x < FRAME || y < TITLE + FRAME {
            self.mark_window_dirty(id);
            if let Some(window) = self.windows.get_mut(&id) {
                window.x = x.max(FRAME) as i16;
                window.y = y.max(TITLE + FRAME) as i16;
            }
            self.notify_configure(id);
        }
        if self.maximize_enabled() && !self.is_transient(id) {
            self.maximized.insert(id);
            self.fit_to_screen(id);
        }
        self.raise_and_focus(id);
        self.mark_window_dirty(id);
    }

    /// A dialog (WM_TRANSIENT_FOR is set) keeps the size that it asked for.
    fn is_transient(&self, id: u32) -> bool {
        const WM_TRANSIENT_FOR: u32 = 68;
        self.windows
            .get(&id)
            .is_some_and(|w| w.properties.contains_key(&WM_TRANSIENT_FOR))
    }

    /// Makes a window as large as the screen allows, under its title bar.
    fn fit_to_screen(&mut self, id: u32) {
        let (screen_width, screen_height) = self.screen_size();
        let Some(window) = self.windows.get(&id) else {
            return;
        };
        let border = 2 * i32::from(window.border);
        let width = (i32::from(screen_width) - 2 * FRAME - border).max(MIN_SIZE);
        let height = (i32::from(screen_height) - TITLE - 2 * FRAME - border).max(MIN_SIZE);
        let (x, y) = (FRAME, TITLE + FRAME);
        if (width, height, x, y)
            == (
                i32::from(window.width),
                i32::from(window.height),
                i32::from(window.x),
                i32::from(window.y),
            )
        {
            return;
        }
        self.mark_window_dirty(id);
        let (width, height) = (width as u16, height as u16);
        if self.resize_surface(id, Some(width), Some(height)).is_err() {
            return;
        }
        if let Some(window) = self.windows.get_mut(&id) {
            window.width = width;
            window.height = height;
            window.x = x as i16;
            window.y = y as i16;
        }
        self.mark_window_dirty(id);
        self.notify_configure(id);
        self.expose_tree(id);
    }

    /// The screen changed its size: the maximized windows follow it.
    pub(super) fn refit_maximized(&mut self) {
        let mut ids: Vec<u32> = self.maximized.iter().copied().collect();
        ids.sort_unstable();
        for id in ids {
            if self.is_managed(id) {
                self.fit_to_screen(id);
            }
        }
    }

    /// Keeps a press for the manager. A second button joins the first one and changes nothing else.
    fn keep_press(
        &mut self,
        button: u8,
        drag: Option<(u32, (i32, i32))>,
        resize: Option<(u32, bool, bool)>,
    ) {
        let bit = 1u8 << button;
        match &mut self.input.wm {
            Some(press) => press.buttons |= bit,
            None => {
                self.input.wm = Some(WmPress {
                    buttons: bit,
                    drag,
                    resize,
                });
            }
        }
    }

    /// Handles a button press over a frame. Returns true when the press is not for the client.
    pub(super) fn wm_button_press(&mut self, button: u8) -> bool {
        const LEFT: u8 = 1;
        const LAST_CLICK_BUTTON: u8 = 3;
        if self.input.pointer_grab.is_some() || self.builtin_wm_inactive() {
            return false;
        }
        if self.input.wm.is_some() && button <= LAST_CLICK_BUTTON {
            self.keep_press(button, None, None);
            return true;
        }
        let (x, y) = self.input.pointer;
        let Some(hit) = self.wm_hit(x, y) else {
            return false;
        };
        if button > LAST_CLICK_BUTTON {
            return false;
        }
        match hit.part {
            Part::Client => {
                self.raise_and_focus(hit.window);
                false
            }
            Part::Close if button == LEFT => {
                self.keep_press(button, None, None);
                self.wm_close(hit.window);
                true
            }
            Part::Title if button == LEFT => {
                self.raise_and_focus(hit.window);
                let held = self
                    .windows
                    .get(&hit.window)
                    .map(|w| (x - i32::from(w.x), y - i32::from(w.y)));
                self.keep_press(button, held.map(|offset| (hit.window, offset)), None);
                true
            }
            Part::Border { right, bottom } => {
                self.raise_and_focus(hit.window);
                let resize =
                    (button == LEFT && (right || bottom)).then_some((hit.window, right, bottom));
                self.keep_press(button, None, resize);
                true
            }
            _ => {
                self.raise_and_focus(hit.window);
                self.keep_press(button, None, None);
                true
            }
        }
    }

    /// Returns true when the release ends a press that the window manager kept.
    pub(super) fn wm_button_release(&mut self, button: u8) -> bool {
        let bit = 1u8 << button;
        let Some(press) = self.input.wm.as_mut() else {
            return false;
        };
        if press.buttons & bit == 0 {
            return false;
        }
        press.buttons &= !bit;
        if press.buttons == 0 {
            self.input.wm = None;
        }
        true
    }

    /// The window that a drag moves is gone, or is not managed any more. The press stays, so that
    /// its release is not sent to a client that never saw the press.
    pub(super) fn wm_forget_window(&mut self, window: u32) {
        self.maximized.remove(&window);
        if let Some(press) = self.input.wm.as_mut() {
            if press.drag.is_some_and(|(w, _)| w == window) {
                press.drag = None;
            }
            if press.resize.is_some_and(|(w, _, _)| w == window) {
                press.resize = None;
            }
        }
    }

    /// Returns true while a title bar is being dragged or an edge is being resized.
    pub(super) fn wm_drag_to(&mut self, x: i32, y: i32) -> bool {
        let Some(press) = self.input.wm else {
            return false;
        };
        if let Some((window, (grab_x, grab_y))) = press.drag {
            if !self.is_managed(window) {
                self.wm_forget_window(window);
                return false;
            }
            self.move_managed(window, x - grab_x, y - grab_y);
            return true;
        }
        if let Some((window, right, bottom)) = press.resize {
            if !self.is_managed(window) {
                self.wm_forget_window(window);
                return false;
            }
            self.resize_managed(window, (x, y), (right, bottom));
            return true;
        }
        false
    }

    /// Resizes a window so that its right and bottom edges follow the pointer.
    fn resize_managed(&mut self, id: u32, (x, y): (i32, i32), (right, bottom): (bool, bool)) {
        self.maximized.remove(&id);
        let (screen_width, screen_height) = self.screen_size();
        let Some(window) = self.windows.get(&id) else {
            return;
        };
        let border = 2 * i32::from(window.border);
        let limit = |wanted: i32, origin: i32, screen: u16| {
            let largest = (i32::from(screen) - origin - border - FRAME).max(MIN_SIZE);
            wanted.clamp(MIN_SIZE, largest)
        };
        let width = if right {
            limit(
                x - i32::from(window.x) - border,
                i32::from(window.x),
                screen_width,
            )
        } else {
            i32::from(window.width)
        };
        let height = if bottom {
            limit(
                y - i32::from(window.y) - border,
                i32::from(window.y),
                screen_height,
            )
        } else {
            i32::from(window.height)
        };
        if (width, height) == (i32::from(window.width), i32::from(window.height)) {
            return;
        }
        self.mark_window_dirty(id);
        let (width, height) = (width as u16, height as u16);
        if self.resize_surface(id, Some(width), Some(height)).is_err() {
            self.log_once("wm resize".into(), || {
                "xsrv-web: a window could not grow, because the pixel budget is used up".into()
            });
            return;
        }
        if let Some(window) = self.windows.get_mut(&id) {
            window.width = width;
            window.height = height;
        }
        self.mark_window_dirty(id);
        self.notify_configure(id);
        self.expose_tree(id);
    }
}

const _: () = assert!(BUTTON + 4 <= TITLE && FRAME > 0);
