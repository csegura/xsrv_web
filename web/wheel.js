// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

// Turns browser wheel events into X wheel clicks (buttons 4 to 7).
//
// A mouse notch is about 100 pixels (or 3 lines), a touchpad sends many small deltas. The
// distance is added up and each 100 pixels, times the speed, is one click. The first event after
// a pause always gives one click, so a single notch scrolls even when the browser reports it as
// a few pixels (macOS).

const PIXELS_PER_CLICK = 100;
const PIXELS_PER_LINE = 33;
const PIXELS_PER_PAGE = 800;
const PAUSE_MS = 200;
const DOM_DELTA_LINE = 1;
const DOM_DELTA_PAGE = 2;

export const WHEEL_SPEEDS = [0.25, 0.5, 1, 2, 4];

function pixels(delta, mode) {
  if (mode === DOM_DELTA_LINE) return delta * PIXELS_PER_LINE;
  if (mode === DOM_DELTA_PAGE) return delta * PIXELS_PER_PAGE;
  return delta;
}

// One axis. `clicks` returns a signed count: negative is up (or left).
export function wheelAxis() {
  let sum = 0;
  let lastAt = -Infinity;
  return {
    clicks(delta, mode, speed, now) {
      if (delta === 0) return 0;
      const amount = pixels(delta, mode) * speed;
      const paused = now - lastAt > PAUSE_MS;
      lastAt = now;
      if (paused || Math.sign(amount) !== Math.sign(sum)) sum = 0;
      sum += amount;
      let count = Math.trunc(sum / PIXELS_PER_CLICK);
      if (paused && count === 0) {
        count = Math.sign(amount);
        sum = 0;
        return count;
      }
      sum -= count * PIXELS_PER_CLICK;
      return count;
    },
  };
}
