// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

import { CORNERS, KEY_COMBOS, SUPER_KEYS } from "../settings.js";

const check = (label, actual, expected) => {
  const ok = JSON.stringify(actual) === JSON.stringify(expected);
  console.log(ok ? "ok  " : "FAIL", label, ok ? "" : JSON.stringify(actual) + " != " + JSON.stringify(expected));
  if (!ok) process.exitCode = 1;
};

check("the first key choice is the real Windows key", SUPER_KEYS[0].code, "");
check("Caps Lock is not offered", SUPER_KEYS.some((key) => key.code === "CapsLock"), false);
check("the buttons start at the top right", CORNERS[0].name, "top-right");
check("every combo button has a label and codes", KEY_COMBOS.every((c) => c.label && c.codes.length >= 2), true);
