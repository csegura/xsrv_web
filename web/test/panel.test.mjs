// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

import { anchorPosition } from "../panel.js";

const check = (label, actual, expected) => {
  const ok = JSON.stringify(actual) === JSON.stringify(expected);
  console.log(ok ? "ok  " : "FAIL", label, ok ? "" : JSON.stringify(actual) + " != " + JSON.stringify(expected));
  if (!ok) process.exitCode = 1;
};
const view = { width: 1000, height: 700 };
const panel = { width: 340, height: 400 };
const bar = (left, top) => ({ left, top, right: left + 120, bottom: top + 30 });

check("a bar at the top right: the panel is below, its right edge in line", anchorPosition(bar(870, 4), panel, view), { left: 650, top: 40 });
check("a bar at the top left: the panel is below, its left edge in line", anchorPosition(bar(4, 4), panel, view), { left: 8, top: 40 });
check("a bar at the bottom: the panel opens above", anchorPosition(bar(870, 660), panel, view), { left: 650, top: 254 });
check("a bar in the middle: no side has room, so it is moved inside the window", anchorPosition(bar(400, 300), panel, view), { left: 400, top: 292 });
check("a bar low in the window: the panel goes above when only that side fits", anchorPosition(bar(400, 500), panel, view), { left: 400, top: 94 });
check("a panel taller than the room stays inside the window", anchorPosition(bar(4, 300), { width: 340, height: 690 }, view), { left: 8, top: 8 });
