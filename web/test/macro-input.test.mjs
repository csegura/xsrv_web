// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

import { captureInput } from "../input.js";
import { keycodeFor } from "../keymap.js";
import { parseSteps } from "../macros.js";

globalThis.WebSocket = { OPEN: 1 };
const handlers = {};
const windowHandlers = {};
globalThis.window = { addEventListener: (name, fn) => { windowHandlers[name] = fn; } };
globalThis.document = { hidden: false, addEventListener: () => {} };
const canvas = {
  width: 640, height: 480, focus: () => {},
  addEventListener: (name, fn) => { handlers[name] = fn; },
  getBoundingClientRect: () => ({ left: 0, top: 0, width: 640, height: 480 }),
};
const sent = [];
const socket = { readyState: 1, send: (bytes) => sent.push(bytes) };
const tag = (name) => (...args) => [name, ...args];
const encoders = {
  encode_pointer: tag("pointer"), encode_button: tag("button"), encode_key: tag("key"),
  encode_pointer_left: tag("left"), encode_release_all: tag("release-all"), encode_locks: tag("locks"),
};
const check = (label, actual, expected) => {
  const ok = JSON.stringify(actual) === JSON.stringify(expected);
  console.log(ok ? "ok  " : "FAIL", label, ok ? "" : JSON.stringify(actual) + " != " + JSON.stringify(expected));
  if (!ok) process.exitCode = 1;
};
const take = () => sent.splice(0);
const tick = () => new Promise((resolve) => setTimeout(resolve, 5));
const code = (name) => keycodeFor(name);
const event = (extra) => ({
  preventDefault() {}, ctrlKey: false, altKey: false, shiftKey: false, metaKey: false, ...extra,
});
const down = (name, extra) => handlers.keydown(event({ code: name, ...extra }));
const up = (name, extra) => handlers.keyup(event({ code: name, ...extra }));

const waits = [];
let release = null;
const states = [];
let locked = false;
let metaHints = 0;
const input = captureInput(canvas, socket, encoders, {}, () => {}, {
  sleep: async (ms, state) => {
    waits.push(ms);
    if (ms === 777) await new Promise((resolve) => { release = resolve; });
    return state;
  },
  onMacroState: (running) => states.push(running),
  isKeyboardLocked: () => locked,
  onMetaWithoutLock: () => { metaHints++; },
});
const macro = (hotkey, text) => ({ name: "m", hotkey, steps: parseSteps(text).steps });
const keyEvent = (name, pressed) => ["key", code(name), pressed];

input.setMacros([macro("Ctrl+Alt+KeyM", "Super+e, wait 100, Return")]);
down("ControlLeft", { ctrlKey: true });
down("AltLeft", { ctrlKey: true, altKey: true });
take();
down("KeyM", { ctrlKey: true, altKey: true });
await tick();
await tick();
check(
  "a hotkey releases the held keys, then plays the macro; the hotkey key is not sent",
  take(),
  [
    keyEvent("ControlLeft", false), keyEvent("AltLeft", false),
    keyEvent("MetaLeft", true), keyEvent("KeyE", true), keyEvent("KeyE", false), keyEvent("MetaLeft", false),
    keyEvent("Enter", true), keyEvent("Enter", false),
  ],
);
check("the waits and the gaps between steps", waits, [20, 100, 20, 20]);
check("the button state follows the macro", states, [true, false]);
up("KeyM", { ctrlKey: true, altKey: true });
up("AltLeft", { ctrlKey: true });
up("ControlLeft");
check("the key-ups of the hotkey and of the released modifiers are dropped", take(), []);
down("ControlLeft", { ctrlKey: true });
up("ControlLeft");
check("the next press of a modifier works again", take(), [keyEvent("ControlLeft", true), keyEvent("ControlLeft", false)]);

down("KeyM", { ctrlKey: true });
up("KeyM", { ctrlKey: true });
check("without Alt held the hotkey does not match", take(), [keyEvent("KeyM", true), keyEvent("KeyM", false)]);
down("KeyA", { ctrlKey: true });
up("KeyA", { ctrlKey: true });
check("other keys still go through", take(), [keyEvent("KeyA", true), keyEvent("KeyA", false)]);

// Paste: the text goes to the server first, then Shift+Insert.
input.setMacros([macro("F9", "paste: héllo")]);
take();
down("F9");
await tick();
await tick();
const messages = take();
check("paste sends the text, then Shift+Insert", [
  messages[0] instanceof Uint8Array ? messages[0][0] : null,
  new TextDecoder().decode(messages[0].subarray(1)),
  messages.slice(1),
], [23, "héllo", [keyEvent("ShiftLeft", true), keyEvent("Insert", true), keyEvent("Insert", false), keyEvent("ShiftLeft", false)]]);
up("F9");

// The same hotkey while the macro runs stops it.
input.setMacros([macro("F10", "wait 777, Return")]);
take();
states.length = 0;
down("F10");
await tick();
up("F10");
down("F10");
up("F10");
release();
await tick();
await tick();
check("a second press stops the macro before its next step", take(), []);
check("the state ends", states, [true, false]);

// Losing the focus stops it too.
down("F10");
await tick();
windowHandlers.blur();
release();
await tick();
await tick();
check("blur stops the macro", take(), []);

// The Windows key without the keyboard lock tells the user.
down("MetaLeft");
check("a Win key press without the lock gives the hint", metaHints, 1);
locked = true;
down("MetaRight");
check("no hint with the lock", metaHints, 1);
