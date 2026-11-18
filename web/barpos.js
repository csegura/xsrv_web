// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

// Where the button bar sits. A position is two fractions (0 to 1) of the free space that the
// bar has in the window, so it stays on screen when the window changes size.

export const MARGIN = 4;
export const KEY_STEP_PIXELS = 10;
export const BIG_KEY_STEP_PIXELS = 50;

export const CORNER_POSITIONS = {
  "top-right": { x: 1, y: 0 },
  "top-left": { x: 0, y: 0 },
  "bottom-right": { x: 1, y: 1 },
  "bottom-left": { x: 0, y: 1 },
};

const clamp01 = (value) => Math.min(1, Math.max(0, value));

// A position from storage, or null when it is not two finite numbers.
export function cleanPosition(value) {
  if (!Number.isFinite(value?.x) || !Number.isFinite(value?.y)) return null;
  return { x: clamp01(value.x), y: clamp01(value.y) };
}

const freeSpace = (view, size) => Math.max(0, view - size - 2 * MARGIN);

// The pixel position of the top left corner of a bar of `size` in a window of `view`.
export function toPixels(pos, view, size) {
  return {
    left: MARGIN + pos.x * freeSpace(view.width, size.width),
    top: MARGIN + pos.y * freeSpace(view.height, size.height),
  };
}

// The position for a bar whose top left corner is at (`left`, `top`). A bar that fills the window
// has no free space and sits at the first corner.
export function toFractions(left, top, view, size) {
  const free = { x: freeSpace(view.width, size.width), y: freeSpace(view.height, size.height) };
  return {
    x: free.x === 0 ? 0 : clamp01((left - MARGIN) / free.x),
    y: free.y === 0 ? 0 : clamp01((top - MARGIN) / free.y),
  };
}

const viewport = () => ({ width: window.innerWidth, height: window.innerHeight });
const sizeOf = (element) => ({ width: element.offsetWidth, height: element.offsetHeight });

export function place(bar, pos) {
  const { left, top } = toPixels(pos, viewport(), sizeOf(bar));
  bar.style.left = `${Math.round(left)}px`;
  bar.style.top = `${Math.round(top)}px`;
}

// The grip moves the bar with a mouse, a finger or a pen; the arrow keys move it while the grip
// has the focus (Shift for bigger steps). `getPos` gives the position now. `setPos(pos, final)`
// is called while moving (`final` false) and when the move ends (`final` true, to save it).
export function makeDraggable({ bar, grip, getPos, setPos }) {
  let drag = null;
  grip.addEventListener("pointerdown", (event) => {
    if (event.button !== undefined && event.button !== 0) return;
    const rect = bar.getBoundingClientRect();
    drag = { dx: event.clientX - rect.left, dy: event.clientY - rect.top };
    grip.setPointerCapture?.(event.pointerId);
    event.preventDefault();
  });
  grip.addEventListener("pointermove", (event) => {
    if (!drag) return;
    setPos(toFractions(event.clientX - drag.dx, event.clientY - drag.dy, viewport(), sizeOf(bar)), false);
  });
  const end = (event) => {
    if (!drag) return;
    const pos = toFractions(event.clientX - drag.dx, event.clientY - drag.dy, viewport(), sizeOf(bar));
    drag = null;
    setPos(pos, true);
  };
  grip.addEventListener("pointerup", end);
  grip.addEventListener("pointercancel", () => {
    drag = null;
    setPos(getPos(), true);
  });
  const steps = { ArrowLeft: [-1, 0], ArrowRight: [1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1] };
  grip.addEventListener("keydown", (event) => {
    const step = steps[event.key];
    if (!step) return;
    const size = event.shiftKey ? BIG_KEY_STEP_PIXELS : KEY_STEP_PIXELS;
    const now = toPixels(getPos(), viewport(), sizeOf(bar));
    setPos(
      toFractions(now.left + step[0] * size, now.top + step[1] * size, viewport(), sizeOf(bar)),
      true,
    );
    event.preventDefault();
  });
}
