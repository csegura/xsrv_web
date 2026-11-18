// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

import { CORNER_POSITIONS, MARGIN, cleanPosition, toFractions, toPixels } from "../barpos.js";

const check = (label, actual, expected) => {
  const ok = JSON.stringify(actual) === JSON.stringify(expected);
  console.log(ok ? "ok  " : "FAIL", label, ok ? "" : JSON.stringify(actual) + " != " + JSON.stringify(expected));
  if (!ok) process.exitCode = 1;
};

const view = { width: 1000, height: 600 };
const size = { width: 100, height: 40 };
const free = { width: 1000 - 100 - 2 * MARGIN, height: 600 - 40 - 2 * MARGIN };

check(
  "the top left position is the margin",
  toPixels(CORNER_POSITIONS["top-left"], view, size),
  { left: MARGIN, top: MARGIN },
);
check(
  "the bottom right position touches the margin at the far side",
  toPixels(CORNER_POSITIONS["bottom-right"], view, size),
  { left: view.width - size.width - MARGIN, top: view.height - size.height - MARGIN },
);
{
  const pos = { x: 0.25, y: 0.75 };
  const { left, top } = toPixels(pos, view, size);
  const back = toFractions(left, top, view, size);
  check("pixels and fractions go back and forth", [back.x.toFixed(6), back.y.toFixed(6)], ["0.250000", "0.750000"]);
}
check("a drag outside the window is held at the edge", toFractions(-500, 9999, view, size), { x: 0, y: 1 });
{
  const pos = { x: 0.5, y: 0.5 };
  const small = toPixels(pos, { width: 500, height: 300 }, size);
  check("the same position fits a smaller window", small.left + size.width <= 500 - MARGIN, true);
  check("a bar that fills the window sits at the first corner", toFractions(30, 30, { width: 100, height: 40 }, size), { x: 0, y: 0 });
  check("free space is what is left after the margins", [free.width, free.height], [892, 552]);
}
check("a bad stored position", [cleanPosition(null), cleanPosition({ x: "a", y: 1 }), cleanPosition({ x: NaN, y: 0 })], [null, null, null]);
check("a stored position is held in range", cleanPosition({ x: 2, y: -1 }), { x: 1, y: 0 });
