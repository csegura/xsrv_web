// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

use std::rc::Rc;
use std::time::{Duration, Instant};

use tokio::sync::Notify;

use wire::{Rect, BYTES_PER_PIXEL};

use super::font;
use super::input::Focus;
use super::region;
use super::resources::{pixel_bytes, IRect, Surface};
use super::screen::ROOT_WINDOW;
use super::state::{State, CLASS_INPUT_OUTPUT};
use super::wm::Frame;

impl State {
    /// Absolute position of the inner top-left corner of a window.
    pub(super) fn window_origin(&self, id: u32) -> Option<(i32, i32)> {
        let mut origin = (0, 0);
        let mut current = id;
        loop {
            let window = self.windows.get(&current)?;
            let Some(parent) = window.parent else {
                return Some(origin);
            };
            origin.0 += i32::from(window.x) + i32::from(window.border);
            origin.1 += i32::from(window.y) + i32::from(window.border);
            current = parent;
        }
    }

    pub(super) fn mark_dirty(&mut self, rect: IRect) {
        if self.dirty.is_empty() {
            self.start_burst();
        }
        add_dirty(&mut self.dirty, rect);
    }

    /// Draws the changed areas when the burst of requests that made them has settled: no request
    /// for `SETTLE`, or `MAX_DELAY` since the first change. A client often clears a cell and draws
    /// the glyph in two writes a few milliseconds apart. Waiting for the end of the burst sends
    /// one tile instead of two, which the page would show one after the other as a flicker.
    /// Starts the timer of a burst, unless one runs, and wakes the timer task.
    fn start_burst(&mut self) {
        if self.dirty_since.is_none() {
            self.dirty_since = Some(Instant::now());
            self.tick_wake.notify_one();
        }
    }

    /// Whether the timer task has work: changes that wait for a flush, or a held pointer release.
    pub fn needs_ticks(&self) -> bool {
        self.dirty_since.is_some() || self.input.replay_since.is_some()
    }

    pub fn tick_wake(&self) -> Rc<Notify> {
        Rc::clone(&self.tick_wake)
    }

    pub fn flush_display_if_settled(&mut self) {
        let Some(since) = self.dirty_since else {
            return;
        };
        let now = Instant::now();
        if self.hold_flush_until.is_some_and(|until| now < until) {
            return;
        }
        self.hold_flush_until = None;
        let quiet = self
            .last_request
            .is_none_or(|last| now.duration_since(last) >= SETTLE);
        if quiet || now.duration_since(since) >= MAX_DELAY {
            self.flush_display();
        }
    }

    /// Marks the window with its border and its frame, when the user can see it.
    pub(super) fn mark_window_dirty(&mut self, id: u32) {
        if !self.is_viewable(id) {
            return;
        }
        if let Some(rect) = self.outer_rect(id) {
            self.mark_dirty(rect);
        }
    }

    /// Marks an area given in window coordinates. The window's own pixels changed, so only the
    /// parts where the screen shows the window are marked: its children and the windows above it
    /// did not change (a new root background around open windows).
    pub(super) fn mark_area_dirty(&mut self, id: u32, area: IRect) {
        if !self.is_viewable(id) {
            return;
        }
        let Some((x, y)) = self.window_origin(id) else {
            return;
        };
        let area = area.translate(x, y);
        match self.shown_parts(id, area) {
            Some(parts) => parts.into_iter().for_each(|part| self.mark_dirty(part)),
            None => self.mark_dirty(area),
        }
    }

    /// Composes the changed areas and sends each one to the web page as its own tile.
    pub fn flush_display(&mut self) {
        self.dirty_since = None;
        let dirty = std::mem::take(&mut self.dirty);
        self.send_areas(dirty);
        // A scroll that drew nothing else still reaches the pages at the end of its burst.
        self.display.release_held();
    }

    /// Like `flush_display`, but only for the changed areas that touch `area`.
    fn flush_display_in(&mut self, area: IRect) {
        let (touching, rest) = std::mem::take(&mut self.dirty)
            .into_iter()
            .partition(|rect| rect.intersect(area).is_some());
        self.dirty = rest;
        // Held copy messages still wait for the end of the burst.
        if self.dirty.is_empty() && !self.display.holds_messages() {
            self.dirty_since = None;
        }
        self.send_areas(touching);
    }

    fn send_areas(&mut self, dirty: Vec<IRect>) {
        let (width, height) = self.screen_size();
        let screen = IRect::new(0, 0, width.into(), height.into());
        let mut buffer = std::mem::take(&mut self.compose_buffer);
        for rect in dirty {
            let Some(area) = rect.intersect(screen) else {
                continue;
            };
            let pixels = self.compose(area, &mut buffer);
            let rect = Rect {
                x: area.x0 as u16,
                y: area.y0 as u16,
                w: area.width() as u16,
                h: area.height() as u16,
            };
            if let Err(error) = self.display.update(rect, pixels) {
                eprintln!("xsrv-web: display update failed: {error:?}");
            }
        }
        buffer.truncate(KEPT_COMPOSE_BYTES);
        buffer.shrink_to(KEPT_COMPOSE_BYTES);
        self.compose_buffer = buffer;
        self.buffers_used_at = Some(Instant::now());
    }

    /// When the timer should give the work buffers back, if they are held.
    pub fn buffers_release_at(&self) -> Option<Instant> {
        self.buffers_used_at.map(|used| used + BUFFERS_QUIET)
    }

    /// The compose buffer and the buffers of the tile encoder are kept between updates, for
    /// speed. After `BUFFERS_QUIET` without an update they are given back to the system: a
    /// full-HD screen keeps several megabytes in them.
    pub fn release_buffers_if_quiet(&mut self) {
        if self
            .buffers_release_at()
            .is_some_and(|at| Instant::now() >= at)
        {
            self.compose_buffer = Vec::new();
            self.display.release_buffers();
            self.buffers_used_at = None;
        }
    }

    /// Moves the pixels of `from` (window coordinates) by `shift` on the screen and on the pages.
    /// Call it before the window content changes. Returns true when it took care of the screen:
    /// then the caller does not mark the target area dirty.
    ///
    /// When the screen shows all of the window there, it is one copy. When other windows cover a
    /// part of it, the parts of the target whose source pixels are shown too are copied, the
    /// other shown parts of the target are drawn again, and the covered parts need nothing.
    pub(super) fn copy_on_screen(&mut self, id: u32, from: IRect, shift: (i32, i32)) -> bool {
        let Some((x, y)) = self
            .window_origin(id)
            .filter(|_| shift != (0, 0) && !self.holding_flush())
        else {
            return false;
        };
        let from = from.translate(x, y);
        let to = from.translate(shift.0, shift.1);
        if self.shows_all_of(id, from.union(to)) {
            // The screen must show the composed window in `from` before its pixels move.
            self.flush_display_in(from);
            return self.copy_screen(from, to);
        }
        self.copy_shown_parts(id, from, shift)
    }

    /// The copy of `copy_on_screen` for a window that is partly covered. Only a straight shift
    /// (a scroll): the pieces are then copied one after the other in the direction of the shift,
    /// so that no piece reads pixels that an earlier one wrote.
    fn copy_shown_parts(&mut self, id: u32, from: IRect, shift: (i32, i32)) -> bool {
        if shift.0 != 0 && shift.1 != 0 {
            return false;
        }
        let to = from.translate(shift.0, shift.1);
        let Some(shown) = self.shown_parts(id, from.union(to)) else {
            return false;
        };
        let shown_at_target = region::intersect(&[to], &shown, MAX_SHOWN_RECTS);
        let source_shown = region::translate(&shown, shift.0, shift.1);
        let Some(mut pieces) = shown_at_target
            .as_deref()
            .and_then(|target| region::intersect(target, &source_shown, MAX_SHOWN_RECTS))
            .map(|pieces| region::normalize(&pieces))
            .filter(|pieces| !pieces.is_empty() && pieces.len() <= MAX_COPY_PIECES)
        else {
            return false;
        };
        let Some(redrawn) = shown_at_target
            .as_deref()
            .and_then(|target| region::subtract(target, &pieces, MAX_SHOWN_RECTS))
        else {
            return false;
        };
        match shift {
            (0, dy) if dy < 0 => pieces.sort_by_key(|piece| piece.y0),
            (0, _) => pieces.sort_by_key(|piece| std::cmp::Reverse(piece.y1)),
            (dx, _) if dx < 0 => pieces.sort_by_key(|piece| piece.x0),
            _ => pieces.sort_by_key(|piece| std::cmp::Reverse(piece.x1)),
        }
        // The screen must show the composed window where the pixels come from.
        self.flush_display_in(from);
        for piece in pieces {
            if !self.copy_screen(piece.translate(-shift.0, -shift.1), piece) {
                // The pages did not get it: the piece is drawn instead.
                self.mark_dirty(piece);
            }
        }
        for rect in redrawn {
            self.mark_dirty(rect);
        }
        true
    }

    /// The parts of `area` (absolute) where the screen shows the window `id`: `area` cut to the
    /// window, its ancestors and the screen, without its children and the windows painted after
    /// it. `None` when the window is not viewable, does not paint all its pixels, or when the
    /// parts would be too many.
    fn shown_parts(&self, id: u32, area: IRect) -> Option<Vec<IRect>> {
        let window = self.windows.get(&id)?;
        let (x, y) = self.window_origin(id)?;
        if !self.is_viewable(id)
            || window.class != CLASS_INPUT_OUTPUT
            || window.shapes.bounding.is_some()
        {
            return None;
        }
        let (width, height) = self.screen_size();
        let mut bounds = IRect::new(0, 0, width.into(), height.into())
            .intersect(IRect::new(x, y, window.width.into(), window.height.into()))?
            .intersect(area)?;
        // A covering window is a hole of its outer rect. One with a bounding shape shows what is
        // below it outside its shape, so it is not a rect: then there are no shown parts.
        let hole = |other: u32| -> Option<Option<IRect>> {
            if !self.covers(other, area) {
                return Some(None);
            }
            let shaped = self
                .windows
                .get(&other)
                .is_some_and(|w| w.shapes.bounding.is_some());
            (!shaped).then(|| self.outer_rect(other))
        };
        let mut holes: Vec<IRect> = Vec::new();
        for &child in &window.children {
            holes.extend(hole(child)?);
        }
        let mut current = id;
        while let Some(parent_id) = self.windows.get(&current).and_then(|w| w.parent) {
            let parent = self.windows.get(&parent_id)?;
            let position = parent.children.iter().position(|&child| child == current)?;
            // Later children are painted later, so they are higher.
            for &sibling in &parent.children[position + 1..] {
                holes.extend(hole(sibling)?);
            }
            if parent.parent.is_some() {
                if parent.class != CLASS_INPUT_OUTPUT || parent.shapes.bounding.is_some() {
                    return None;
                }
                let (px, py) = self.window_origin(parent_id)?;
                bounds = bounds.intersect(IRect::new(
                    px,
                    py,
                    parent.width.into(),
                    parent.height.into(),
                ))?;
            }
            current = parent_id;
        }
        region::subtract(&[bounds], &holes, MAX_SHOWN_RECTS)
    }

    /// Moves a window with `change` (which changes only its position). When the screen shows all
    /// of the window, with its border and its frame, before and after the move, its pixels move as
    /// a copy, and only the area that it leaves is drawn again. Otherwise both areas are drawn.
    pub(super) fn move_with_screen_copy(&mut self, id: u32, change: impl FnOnce(&mut State)) {
        let before = self.movable_rect(id);
        match before {
            // The screen must show the composed window before its pixels move.
            Some(old) => self.flush_display_in(old),
            None => self.mark_window_dirty(id),
        }
        change(self);
        let copied = before.is_some_and(|old| self.finish_screen_move(id, old));
        if !copied {
            if let Some(old) = before {
                self.mark_dirty(old);
            }
            self.mark_window_dirty(id);
        }
    }

    /// The outer rectangle of a window that the screen shows whole and that may move as a copy.
    /// Each of its pixels must be painted by the window: an InputOnly window paints nothing, a
    /// shaped one not all of it, and the close button of a very narrow frame lies outside it.
    fn movable_rect(&self, id: u32) -> Option<IRect> {
        let window = self.windows.get(&id)?;
        let paints_all = window.class == CLASS_INPUT_OUTPUT
            && window.shapes.bounding.is_none()
            && self
                .frame_of(id)
                .is_none_or(|frame| frame.close.intersect(frame.outer) == Some(frame.close));
        let outer = self.outer_rect(id)?;
        (paints_all && !self.holding_flush() && self.unobscured(id, outer)).then_some(outer)
    }

    /// After a move from `old`: copies the screen pixels and marks the uncovered area dirty.
    /// Returns false, and changes nothing, when the new place is not shown whole.
    fn finish_screen_move(&mut self, id: u32, old: IRect) -> bool {
        let Some(new) = self.movable_rect(id) else {
            return false;
        };
        if new == old {
            return true;
        }
        if (new.width(), new.height()) != (old.width(), old.height()) || !self.copy_screen(old, new)
        {
            return false;
        }
        // The strips that the window leaves are drawn now, each on its own: as dirty areas they
        // touch, and the dirty list would join them into the whole old rectangle.
        let uncovered = region::subtract(&[old], &[new], MAX_UNCOVERED_RECTS).unwrap_or(vec![old]);
        self.send_areas(uncovered);
        true
    }

    /// Moves the screen pixels of `from` to `to` (same size) on the server and the pages.
    fn copy_screen(&mut self, from: IRect, to: IRect) -> bool {
        let rect = Rect {
            x: from.x0 as u16,
            y: from.y0 as u16,
            w: from.width() as u16,
            h: from.height() as u16,
        };
        let moved = self
            .display
            .copy(rect, (to.x0 as u16, to.y0 as u16))
            .unwrap_or_else(|error| {
                eprintln!("xsrv-web: display copy failed: {error:?}");
                false
            });
        // The copy message waits for the end of the burst, as a changed area does.
        if moved {
            self.start_burst();
        }
        moved
    }

    /// After a screen size change, drawing waits for the clients (see `SCREEN_CHANGE_HOLD`).
    fn holding_flush(&self) -> bool {
        self.hold_flush_until
            .is_some_and(|until| Instant::now() < until)
    }

    /// True when each pixel of `area` (absolute) on the screen shows the window `id`: the area is
    /// inside the window, no child or bounding shape covers or cuts it, and it is `unobscured`.
    fn shows_all_of(&self, id: u32, area: IRect) -> bool {
        let (Some(window), Some((x, y))) = (self.windows.get(&id), self.window_origin(id)) else {
            return false;
        };
        let inner = IRect::new(x, y, window.width.into(), window.height.into());
        window.class == CLASS_INPUT_OUTPUT
            && window.shapes.bounding.is_none()
            && inner.intersect(area) == Some(area)
            && !window
                .children
                .iter()
                .any(|&child| self.covers(child, area))
            && self.unobscured(id, area)
    }

    /// True when the window `id` is viewable and nothing painted after it covers `area`
    /// (absolute): the area is inside the screen and inside each ancestor, no ancestor has a
    /// bounding shape, and no higher sibling of the window or of an ancestor overlaps it.
    fn unobscured(&self, id: u32, area: IRect) -> bool {
        let inside = |outer: IRect| outer.intersect(area) == Some(area);
        let (width, height) = self.screen_size();
        if !self.is_viewable(id) || !inside(IRect::new(0, 0, width.into(), height.into())) {
            return false;
        }
        let mut current = id;
        loop {
            let Some(parent_id) = self.windows.get(&current).and_then(|w| w.parent) else {
                return true;
            };
            let Some(parent) = self.windows.get(&parent_id) else {
                return false;
            };
            let Some(position) = parent.children.iter().position(|&child| child == current) else {
                return false;
            };
            // Later children are painted later, so they are higher.
            if parent.children[position + 1..]
                .iter()
                .any(|&sibling| self.covers(sibling, area))
            {
                return false;
            }
            if parent.parent.is_some() {
                let Some((x, y)) = self.window_origin(parent_id) else {
                    return false;
                };
                let inner = IRect::new(x, y, parent.width.into(), parent.height.into());
                if parent.class != CLASS_INPUT_OUTPUT
                    || parent.shapes.bounding.is_some()
                    || !inside(inner)
                {
                    return false;
                }
            }
            current = parent_id;
        }
    }

    /// A window with its border, and its frame when the built-in manager draws one.
    pub(super) fn outer_rect(&self, id: u32) -> Option<IRect> {
        let window = self.windows.get(&id)?;
        let (x, y) = self.window_origin(id)?;
        let mut outer =
            IRect::new(x, y, window.width.into(), window.height.into()).grow(window.border.into());
        if let Some(frame) = self.frame_of(id) {
            // The close button of a very narrow window lies outside the frame.
            outer = outer.union(frame.outer).union(frame.close);
        }
        Some(outer)
    }

    /// True when a mapped window that paints, with its border and its frame, overlaps `area`.
    fn covers(&self, id: u32, area: IRect) -> bool {
        let paints = self
            .windows
            .get(&id)
            .is_some_and(|window| window.mapped && window.class == CLASS_INPUT_OUTPUT);
        paints
            && self
                .outer_rect(id)
                .is_some_and(|outer| outer.intersect(area).is_some())
    }

    /// Paints `area` into the start of `buffer` and returns its pixels.
    fn compose<'a>(&self, area: IRect, buffer: &'a mut Vec<u8>) -> &'a [u8] {
        let len = area.width() as usize * area.height() as usize * BYTES_PER_PIXEL;
        if buffer.len() < len {
            buffer.resize(len, 0);
        }
        let pixels = &mut buffer[..len];
        // The buffer holds the pixels of an earlier area. The root window paints over all of
        // them, unless it does not cover the area.
        if !self.root_paints_all_of(area) {
            pixels.fill(0);
        }
        let mut canvas = Canvas { area, pixels };
        self.paint_window(ROOT_WINDOW, (0, 0), area, &mut canvas);
        canvas.pixels
    }

    fn root_paints_all_of(&self, area: IRect) -> bool {
        self.windows.get(&ROOT_WINDOW).is_some_and(|root| {
            let inner = IRect::new(0, 0, root.width.into(), root.height.into());
            root.class == CLASS_INPUT_OUTPUT
                && root.shapes.bounding.is_none()
                && inner.intersect(area) == Some(area)
        })
    }

    /// Paints a window, then its mapped children from bottom to top. `clip` is the visible part.
    fn paint_window(&self, id: u32, inner: (i32, i32), clip: IRect, canvas: &mut Canvas) {
        let Some(window) = self.windows.get(&id) else {
            return;
        };
        if window.class != CLASS_INPUT_OUTPUT {
            return;
        }
        let inner_rect = IRect::new(inner.0, inner.1, window.width.into(), window.height.into());
        let shape = self.bounding_shape_at(id, inner);
        // A window with a bounding shape only shows the parts of it that are inside the shape.
        let parts = |area: IRect| -> Vec<IRect> {
            match &shape {
                None => vec![area],
                Some(rects) => rects
                    .iter()
                    .filter_map(|rect| rect.intersect(area))
                    .collect(),
            }
        };
        if window.border > 0 {
            if let Some(visible) = inner_rect.grow(window.border.into()).intersect(clip) {
                for part in parts(visible) {
                    canvas.fill(part, window.border_pixel);
                }
            }
        }
        let Some(content) = inner_rect.intersect(clip) else {
            return;
        };
        for part in parts(content) {
            canvas.blit(part, &window.surface, inner);
        }
        for child_id in &window.children {
            let Some(child) = self.windows.get(child_id).filter(|c| c.mapped) else {
                continue;
            };
            let origin = (
                inner.0 + i32::from(child.x) + i32::from(child.border),
                inner.1 + i32::from(child.y) + i32::from(child.border),
            );
            if window.parent.is_none() {
                if let Some(frame) = self.frame_of(*child_id) {
                    self.paint_frame(*child_id, &frame, content, canvas);
                }
            }
            self.paint_window(*child_id, origin, content, canvas);
        }
    }
}

const FRAME_FOCUSED: u32 = 0x0034_65A4;
const FRAME_UNFOCUSED: u32 = 0x006B_6B6B;
const FRAME_TEXT: u32 = 0x00FF_FFFF;
const CLOSE_BUTTON: u32 = 0x00CC_0000;
const TITLE_MARGIN: i32 = 6;

impl State {
    /// Paints the frame of a top-level window: the border, the title and the close button.
    fn paint_frame(&self, id: u32, frame: &Frame, clip: IRect, canvas: &mut Canvas) {
        let focused = self.input.focus == Focus::Window(id);
        let color = if focused {
            FRAME_FOCUSED
        } else {
            FRAME_UNFOCUSED
        };
        if let Some(visible) = frame.outer.intersect(clip) {
            canvas.fill(visible, color);
        }
        if let Some(visible) = frame.close.intersect(clip) {
            canvas.fill(visible, CLOSE_BUTTON);
        }
        let glyph_x = frame.close.x0 + (frame.close.width() - 8) / 2;
        canvas.text(clip, (glyph_x, frame.close.y0 - 1), b"x", FRAME_TEXT);
        let room = (frame.close.x0 - frame.title.x0 - TITLE_MARGIN) / 8;
        let title = self.window_title(id);
        let shown = &title[..title.len().min(usize::try_from(room).unwrap_or(0))];
        canvas.text(
            clip,
            (frame.title.x0 + TITLE_MARGIN, frame.title.y0 + 1),
            shown,
            FRAME_TEXT,
        );
    }
}

struct Canvas<'a> {
    area: IRect,
    pixels: &'a mut [u8],
}

impl Canvas<'_> {
    fn row_range(&self, y: i32, x0: i32, x1: i32) -> std::ops::Range<usize> {
        let row = (y - self.area.y0) as usize * self.area.width() as usize;
        let start = (row + (x0 - self.area.x0) as usize) * BYTES_PER_PIXEL;
        start..start + (x1 - x0) as usize * BYTES_PER_PIXEL
    }

    fn fill(&mut self, rect: IRect, pixel: u32) {
        let bytes = pixel_bytes(pixel);
        for y in rect.y0..rect.y1 {
            let range = self.row_range(y, rect.x0, rect.x1);
            for chunk in self.pixels[range].as_chunks_mut::<BYTES_PER_PIXEL>().0 {
                *chunk = bytes;
            }
        }
    }

    /// Draws ASCII text with the built-in font. Only the part inside `clip` is drawn.
    fn text(&mut self, clip: IRect, (x, y): (i32, i32), text: &[u8], pixel: u32) {
        for (index, &character) in text.iter().enumerate() {
            let left = x + index as i32 * font::GLYPH_WIDTH;
            for (row, bits) in font::glyph(u16::from(character)).into_iter().enumerate() {
                for column in 0..font::GLYPH_WIDTH {
                    if bits >> column & 1 == 0 {
                        continue;
                    }
                    let dot = IRect::new(left + column, y + row as i32, 1, 1);
                    if let Some(visible) = dot.intersect(clip) {
                        self.fill(visible, pixel);
                    }
                }
            }
        }
    }

    /// Copies `rect` (absolute) from a surface whose top-left corner is at `origin`.
    fn blit(&mut self, rect: IRect, surface: &Surface, origin: (i32, i32)) {
        for y in rect.y0..rect.y1 {
            let source = surface.row(y - origin.1, rect.x0 - origin.0, rect.x1 - origin.0);
            let range = self.row_range(y, rect.x0, rect.x1);
            self.pixels[range].copy_from_slice(source);
        }
    }
}
/// How long without a request ends a burst, and the longest that drawing may wait.
const SETTLE: Duration = Duration::from_millis(6);
const MAX_DELAY: Duration = Duration::from_millis(16);
/// How long the timer waits after a change of the screen size.
pub(super) const SCREEN_CHANGE_HOLD: Duration = Duration::from_millis(40);

/// The compose buffer goes back to this size after a larger area (a 1920x1080 screen).
const KEPT_COMPOSE_BYTES: usize = 1920 * 1080 * BYTES_PER_PIXEL;
const BUFFERS_QUIET: Duration = Duration::from_secs(10);

/// The most rectangles of the area that a moved window leaves (at most 4 for one rectangle).
const MAX_UNCOVERED_RECTS: usize = 16;
/// Limits for the copy of a partly covered window: the rects of its shown parts, and the copy
/// messages. A window under many others is drawn again instead.
const MAX_SHOWN_RECTS: usize = 32;
const MAX_COPY_PIECES: usize = 8;

/// More rectangles than this are merged into one, so that the list stays short.
const MAX_DIRTY_RECTS: usize = 8;

/// Adds `rect` to a list of changed areas that do not overlap. A rectangle that overlaps or touches
/// others is joined with them. With too many rectangles the list becomes one bounding rectangle.
/// Whether two dirty areas are better composed as one: they overlap (a pixel must not be composed
/// twice), or they touch and their bounding rectangle is not much larger than the two (adjacent
/// glyph cells). Two strips around a window that did not change stay apart, so the window is not
/// composed again.
fn joins(a: IRect, b: IRect) -> bool {
    let touches = a.x0 <= b.x1 && b.x0 <= a.x1 && a.y0 <= b.y1 && b.y0 <= a.y1;
    if !touches {
        return false;
    }
    if a.intersect(b).is_some() {
        return true;
    }
    let area = |r: IRect| i64::from(r.width()) * i64::from(r.height());
    area(a.union(b)) * 2 <= (area(a) + area(b)) * 3
}

fn add_dirty(list: &mut Vec<IRect>, rect: IRect) {
    if rect.x0 >= rect.x1 || rect.y0 >= rect.y1 {
        return;
    }
    let mut merged = rect;
    loop {
        let before = list.len();
        list.retain(|other| {
            let join = joins(merged, *other);
            if join {
                merged = merged.union(*other);
            }
            !join
        });
        if list.len() == before {
            break;
        }
    }
    list.push(merged);
    if list.len() > MAX_DIRTY_RECTS {
        let all = list.iter().copied().reduce(IRect::union).unwrap_or(merged);
        list.clear();
        list.push(all);
    }
}

#[cfg(test)]
mod dirty_tests {
    use super::*;
    use crate::display::Display;

    #[test]
    fn work_buffers_go_back_after_a_quiet_time() {
        let mut state = State::new(Display::new(640, 480));
        state.mark_dirty(IRect::new(0, 0, 640, 480));
        state.flush_display();
        assert!(state.compose_buffer.capacity() >= 640 * 480 * 4);
        state.release_buffers_if_quiet();
        assert!(
            state.compose_buffer.capacity() > 0,
            "not yet: an update was just sent"
        );
        state.buffers_used_at = Some(Instant::now() - BUFFERS_QUIET);
        state.release_buffers_if_quiet();
        assert_eq!(state.compose_buffer.capacity(), 0);
        assert_eq!(
            state.buffers_release_at(),
            None,
            "no more wakes until the next update"
        );
    }

    #[test]
    fn far_apart_areas_stay_separate() {
        let mut list = Vec::new();
        add_dirty(&mut list, IRect::new(0, 0, 10, 10));
        add_dirty(&mut list, IRect::new(200, 200, 10, 10));
        assert_eq!(list.len(), 2, "two small changes are not one big rectangle");
    }

    #[test]
    fn overlapping_and_touching_areas_are_joined() {
        let mut list = Vec::new();
        add_dirty(&mut list, IRect::new(0, 0, 10, 10));
        add_dirty(&mut list, IRect::new(5, 5, 10, 10));
        add_dirty(&mut list, IRect::new(15, 0, 5, 5));
        assert_eq!(list, vec![IRect::new(0, 0, 20, 15)]);
    }

    #[test]
    fn strips_around_an_unchanged_window_stay_apart() {
        // The root around a window at (0, 0, 806, 686) on a 1280x800 screen.
        let mut list = Vec::new();
        add_dirty(&mut list, IRect::new(806, 0, 474, 686));
        add_dirty(&mut list, IRect::new(0, 686, 1280, 114));
        assert_eq!(list.len(), 2, "{list:?}");
        // Glyph cells side by side are still one area.
        let mut cells = Vec::new();
        add_dirty(&mut cells, IRect::new(0, 0, 8, 16));
        add_dirty(&mut cells, IRect::new(8, 0, 8, 16));
        assert_eq!(cells, vec![IRect::new(0, 0, 16, 16)]);
    }

    #[test]
    fn a_new_area_can_join_two_old_ones() {
        let mut list = Vec::new();
        add_dirty(&mut list, IRect::new(0, 0, 10, 10));
        add_dirty(&mut list, IRect::new(30, 0, 10, 10));
        add_dirty(&mut list, IRect::new(5, 0, 30, 10));
        assert_eq!(list, vec![IRect::new(0, 0, 40, 10)]);
    }

    #[test]
    fn too_many_areas_become_one_bounding_rectangle() {
        let mut list = Vec::new();
        for i in 0..=MAX_DIRTY_RECTS as i32 {
            add_dirty(&mut list, IRect::new(i * 100, 0, 10, 10));
        }
        assert_eq!(list.len(), 1, "{list:?}");
        assert_eq!(
            (list[0].x0, list[0].x1),
            (0, MAX_DIRTY_RECTS as i32 * 100 + 10)
        );
        add_dirty(&mut list, IRect::new(5000, 0, 10, 10));
        assert_eq!(list.len(), 2, "the list grows again after it was merged");
    }

    #[test]
    fn empty_areas_are_ignored() {
        let mut list = Vec::new();
        add_dirty(&mut list, IRect::new(5, 5, 0, 10));
        assert!(list.is_empty());
    }
}
