// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

// Key names, macros and key substitutions. No DOM here, so node can test it.
import { keycodeFor } from "./keymap.js";

export const MAX_STEPS = 50;
export const MAX_WAIT_MS = 5000;
export const MAX_TOTAL_WAIT_MS = 30000;
export const MAX_PASTE_BYTES = 4096;
export const MAX_COMBO_KEYS = 6;
export const MAX_MACROS = 32;
export const MAX_SUBSTITUTIONS = 64;
export const MAX_NAME_LENGTH = 40;

// The names that a macro uses for keys, in lower case, and the KeyboardEvent.code they stand for.
const NAMED = {
  ctrl: "ControlLeft", control: "ControlLeft", lctrl: "ControlLeft", rctrl: "ControlRight",
  alt: "AltLeft", lalt: "AltLeft", ralt: "AltRight", altgr: "AltRight",
  shift: "ShiftLeft", lshift: "ShiftLeft", rshift: "ShiftRight",
  super: "MetaLeft", win: "MetaLeft", windows: "MetaLeft", meta: "MetaLeft", cmd: "MetaLeft",
  rsuper: "MetaRight", rwin: "MetaRight",
  enter: "Enter", return: "Enter", esc: "Escape", escape: "Escape", tab: "Tab", space: "Space",
  backspace: "Backspace", delete: "Delete", del: "Delete", insert: "Insert", ins: "Insert",
  home: "Home", end: "End", pageup: "PageUp", pgup: "PageUp", pagedown: "PageDown",
  pgdn: "PageDown", up: "ArrowUp", down: "ArrowDown", left: "ArrowLeft", right: "ArrowRight",
  menu: "ContextMenu", printscreen: "PrintScreen", pause: "Pause", capslock: "CapsLock",
  numlock: "NumLock", scrolllock: "ScrollLock",
  minus: "Minus", equal: "Equal", comma: "Comma", period: "Period", slash: "Slash",
  backslash: "Backslash", semicolon: "Semicolon", quote: "Quote", backquote: "Backquote",
  bracketleft: "BracketLeft", bracketright: "BracketRight",
};

const MODIFIER_CODES = new Set([
  "ShiftLeft", "ShiftRight", "ControlLeft", "ControlRight", "AltLeft", "AltRight",
  "MetaLeft", "MetaRight", "CapsLock", "NumLock",
]);

const LABELS = {
  ControlLeft: "Left Ctrl", ControlRight: "Right Ctrl", AltLeft: "Left Alt",
  AltRight: "Right Alt (AltGr)", ShiftLeft: "Left Shift", ShiftRight: "Right Shift",
  MetaLeft: "Win / Cmd", MetaRight: "Right Win", ContextMenu: "Menu", ArrowUp: "Up",
  ArrowDown: "Down", ArrowLeft: "Left", ArrowRight: "Right", Backquote: "`", Minus: "-",
  Equal: "=", Comma: ",", Period: ".", Slash: "/", Backslash: "\\", Semicolon: ";", Quote: "'",
  BracketLeft: "[", BracketRight: "]",
};

// The KeyboardEvent.code for a key name, or undefined: `ctrl`, `a`, `7`, `f5`, `Return`, `KeyA`.
export function codeFor(name) {
  const text = name.trim();
  const lower = text.toLowerCase();
  if (NAMED[lower]) return NAMED[lower];
  if (/^[a-z]$/.test(lower)) return `Key${lower.toUpperCase()}`;
  if (/^[0-9]$/.test(text)) return `Digit${text}`;
  if (/^f([1-9]|1[0-9]|2[0-4])$/.test(lower)) return lower.toUpperCase();
  return keycodeFor(text) === undefined ? undefined : text;
}

// The text that the user sees for a KeyboardEvent.code.
export function keyLabel(code) {
  if (LABELS[code]) return LABELS[code];
  if (/^Key[A-Z]$/.test(code)) return code.slice(3);
  if (/^Digit[0-9]$/.test(code)) return code.slice(5);
  return code;
}

// `Ctrl+Alt+t` as the codes that go down in this order.
export function parseCombo(text) {
  const names = text.split("+").map((name) => name.trim());
  if (names.length > MAX_COMBO_KEYS) return { error: `A combo has at most ${MAX_COMBO_KEYS} keys.` };
  const codes = [];
  for (const name of names) {
    const code = name === "" ? undefined : codeFor(name);
    if (code === undefined) return { error: `"${name}" is not a key name.` };
    codes.push(code);
  }
  return { codes };
}

const WAIT = /^wait\s+(\d+)\s*(ms)?$/i;
const PASTE = /^paste:\s?(.*)$/i;

// A macro as text: steps on separate lines or separated by commas. `wait 300` pauses for 300 ms,
// `paste: text` puts the text on the clipboard and pastes it (it takes the rest of the line), and
// anything else is a key combo.
export function parseSteps(text) {
  const steps = [];
  let totalWait = 0;
  const lines = text.split(/\r?\n/);
  for (const line of lines) {
    let rest = line.trim();
    while (rest !== "") {
      const paste = PASTE.exec(rest);
      let part;
      if (paste) {
        part = rest;
        rest = "";
      } else {
        const comma = rest.indexOf(",");
        part = (comma < 0 ? rest : rest.slice(0, comma)).trim();
        rest = comma < 0 ? "" : rest.slice(comma + 1).trim();
        if (part === "") continue;
      }
      const number = steps.length + 1;
      const pasteStep = PASTE.exec(part);
      const wait = WAIT.exec(part);
      if (pasteStep) {
        const body = pasteStep[1];
        if (body === "") return { error: `Step ${number}: paste: needs a text.` };
        if (new TextEncoder().encode(body).length > MAX_PASTE_BYTES) {
          return { error: `Step ${number}: the text is longer than ${MAX_PASTE_BYTES} bytes.` };
        }
        steps.push({ type: "paste", text: body });
      } else if (wait) {
        const ms = Number(wait[1]);
        if (ms > MAX_WAIT_MS) return { error: `Step ${number}: a wait is at most ${MAX_WAIT_MS} ms.` };
        totalWait += ms;
        steps.push({ type: "wait", ms });
      } else {
        const combo = parseCombo(part);
        if (combo.error) return { error: `Step ${number}: ${combo.error}` };
        steps.push({ type: "combo", codes: combo.codes });
      }
      if (steps.length > MAX_STEPS) return { error: `A macro has at most ${MAX_STEPS} steps.` };
    }
  }
  if (steps.length === 0) return { error: "A macro needs at least one step." };
  if (totalWait > MAX_TOTAL_WAIT_MS) {
    return { error: `The waits add up to more than ${MAX_TOTAL_WAIT_MS / 1000} seconds.` };
  }
  return { steps };
}

// The hotkey of an event as `Ctrl+Alt+KeyM` (modifiers in a fixed order, then the key). A
// modifier alone, or a lock key, is not a hotkey: the result is null.
export function comboFromEvent(event) {
  if (MODIFIER_CODES.has(event.code)) return null;
  const mods = [];
  if (event.ctrlKey) mods.push("Ctrl");
  if (event.altKey) mods.push("Alt");
  if (event.shiftKey) mods.push("Shift");
  if (event.metaKey) mods.push("Meta");
  return [...mods, event.code].join("+");
}

const FUNCTION_KEY = /^F([1-9]|1[0-9]|2[0-4])$/;

// Why a hotkey cannot be used, or an empty text. A key without a modifier would stop that key
// from reaching the screen, so only the function keys may stand alone.
export function hotkeyProblem(hotkey) {
  const parts = hotkey.split("+");
  const code = parts.at(-1);
  const mods = parts.slice(0, -1);
  if (keycodeFor(code) === undefined || MODIFIER_CODES.has(code)) return "That is not a usable key.";
  if (mods.some((mod) => !["Ctrl", "Alt", "Shift", "Meta"].includes(mod))) return "Bad modifier.";
  if (mods.length === 0 && !FUNCTION_KEY.test(code)) {
    return "Use at least one of Ctrl, Alt, Shift or Win with this key.";
  }
  return "";
}

export function formatHotkey(hotkey) {
  const parts = hotkey.split("+");
  const code = parts.pop();
  return [...parts.map((mod) => (mod === "Meta" ? "Win" : mod)), keyLabel(code)].join("+");
}

// Rebuilds `remap` (KeyboardEvent.code to the code that is sent) in place, because the input code
// holds the same object. The substitutions go first, and the key chosen for Win / Cmd wins over
// them when both name the same key.
export function applyRemap(remap, superKey, substitutions) {
  for (const key of Object.keys(remap)) delete remap[key];
  for (const { from, to } of substitutions) remap[from] = to;
  if (superKey) remap[superKey] = "MetaLeft";
  return remap;
}

const known = (code) => typeof code === "string" && keycodeFor(code) !== undefined;

// Substitutions from storage: unknown keys, a key sent to itself and a second entry for the same
// key are dropped. `dropped` counts what was left out.
export function cleanSubstitutions(list) {
  const items = new Map();
  let dropped = 0;
  for (const entry of Array.isArray(list) ? list : []) {
    if (known(entry?.from) && known(entry?.to) && entry.from !== entry.to) {
      items.set(entry.from, { from: entry.from, to: entry.to });
    } else {
      dropped++;
    }
  }
  const kept = [...items.values()].slice(0, MAX_SUBSTITUTIONS);
  return { items: kept, dropped: dropped + items.size - kept.length };
}

// Macros from storage. A macro whose steps do not parse is dropped, a bad hotkey is cleared, and a
// hotkey that an earlier macro has is cleared too.
export function cleanMacros(list) {
  const items = [];
  const used = new Set();
  let dropped = 0;
  for (const entry of Array.isArray(list) ? list : []) {
    const name = typeof entry?.name === "string" ? entry.name.trim() : "";
    const steps = typeof entry?.steps === "string" ? entry.steps : "";
    if (name === "" || name.length > MAX_NAME_LENGTH || parseSteps(steps).error || items.length >= MAX_MACROS) {
      dropped++;
      continue;
    }
    let hotkey = typeof entry.hotkey === "string" ? entry.hotkey : "";
    if (hotkey !== "" && (hotkeyProblem(hotkey) || used.has(hotkey))) hotkey = "";
    if (hotkey !== "") used.add(hotkey);
    items.push({ name, hotkey, steps, bar: entry.bar === true });
  }
  return { items, dropped };
}
