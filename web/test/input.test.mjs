// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

import { captureInput } from "../input.js";
import { keycodeFor } from "../keymap.js";

globalThis.WebSocket = { OPEN: 1 };
const handlers = {};
const windowHandlers = {};
const documentHandlers = {};
globalThis.window = { addEventListener: (name, fn) => { windowHandlers[name] = fn; } };
globalThis.document = {
  hidden: false,
  addEventListener: (name, fn) => { documentHandlers[name] = fn; },
};
const canvas = {
  width: 640,
  height: 480,
  addEventListener: (name, fn) => { handlers[name] = fn; },
  getBoundingClientRect: () => ({ left: 10, top: 20, width: 1280, height: 960 }),
  focus: () => {},
};
const sent = [];
const socket = { readyState: 1, send: (bytes) => sent.push(bytes) };
const tag = (name) => (...args) => [name, ...args];
const encoders = {
  encode_pointer: tag("pointer"), encode_button: tag("button"),
  encode_key: tag("key"), encode_pointer_left: tag("left"),
  encode_release_all: tag("release-all"), encode_locks: tag("locks"),
};
captureInput(canvas, socket, encoders);
const event = (extra) => ({ preventDefault() {}, ...extra });
const check = (label, actual, expected) => {
  const ok = JSON.stringify(actual) === JSON.stringify(expected);
  console.log(ok ? "ok  " : "FAIL", label, ok ? "" : JSON.stringify(actual) + " != " + JSON.stringify(expected));
  if (!ok) process.exitCode = 1;
};
const take = () => sent.splice(0);

handlers.mousemove(event({ clientX: 10 + 600, clientY: 20 + 400 }));
check("pointer is scaled by the canvas size", take(), [["pointer", 300, 200]]);
handlers.mousemove(event({ clientX: -50, clientY: 5000 }));
check("pointer is clamped to the screen", take(), [["pointer", 0, 479]]);
handlers.mousedown(event({ button: 2, clientX: 10, clientY: 20 }));
check("right button is X button 3", take(), [["pointer", 0, 0], ["button", 3, true]]);
windowHandlers.mouseup(event({ button: 2, clientX: 5000, clientY: 5000 }));
check("a release outside the canvas still arrives", take(), [["pointer", 639, 479], ["button", 3, false]]);
handlers.mousedown(event({ button: 0, clientX: 10, clientY: 20 }));
take();
windowHandlers.mouseup(event({ button: 0, clientX: 10, clientY: 20 }));
check("left release", take(), [["pointer", 0, 0], ["button", 1, false]]);
windowHandlers.mouseup(event({ button: 0, clientX: 10, clientY: 20 }));
check("a release without a press is ignored", take(), []);
handlers.mousedown(event({ button: 4, clientX: 10, clientY: 20 }));
check("unknown mouse button is ignored", take(), []);
handlers.wheel(event({ deltaY: -3, deltaX: 0, clientX: 10, clientY: 20 }));
check("wheel up is button 4 click", take(), [["pointer", 0, 0], ["button", 4, true], ["button", 4, false]]);
handlers.wheel(event({ deltaY: 0, deltaX: 5, clientX: 10, clientY: 20 }));
check("wheel right is button 7 click", take(), [["pointer", 0, 0], ["button", 7, true], ["button", 7, false]]);
handlers.keydown(event({ code: "KeyA", repeat: false, getModifierState: (name) => name === "NumLock" }));
check("KeyA is keycode 38, with the lock state first", take(), [["locks", false, true], ["key", 38, true]]);
handlers.keydown(event({ code: "KeyA", repeat: true }));
check("a repeat sends a release first", take(), [["key", 38, false], ["key", 38, true]]);
handlers.keyup(event({ code: "ShiftLeft" }));
check("ShiftLeft is keycode 50", take(), [["key", 50, false]]);
handlers.keydown(event({ code: "KeyB", repeat: false, getModifierState: (name) => name === "NumLock" }));
check("the lock state is sent only when it changes", take(), [["key", 56, true]]);
windowHandlers.blur();
check("losing the focus releases what is held", take(), [["release-all"]]);
windowHandlers.blur();
check("nothing is held, so nothing is sent", take(), []);
handlers.mousedown(event({ button: 0, clientX: 10, clientY: 20 }));
take();
document.hidden = true;
documentHandlers.visibilitychange();
check("hiding the tab releases what is held", take(), [["release-all"]]);
document.hidden = false;
handlers.keydown(event({ code: "Unidentified", repeat: false }));
check("unknown key is ignored", take(), []);
handlers.mouseleave(event({}));
check("leaving the canvas", take(), [["left"]]);
check("ArrowUp", keycodeFor("ArrowUp"), 111);
check("IntlBackslash (the < key of ISO keyboards)", keycodeFor("IntlBackslash"), 94);
check("PrintScreen", keycodeFor("PrintScreen"), 107);
check("F13", keycodeFor("F13"), 191);
check("Enter", keycodeFor("Enter"), 36);
check("Space", keycodeFor("Space"), 65);
socket.readyState = 0;
handlers.mousemove(event({ clientX: 10, clientY: 20 }));
check("nothing is sent while the socket is closed", take(), []);

socket.readyState = 1;
// Ctrl+V: the browser clipboard goes to the server before the key, and a later key waits.
const useClipboard = (readText) =>
  Object.defineProperty(globalThis, "navigator", { value: { clipboard: { readText } }, configurable: true });
useClipboard(async () => "pegado");
handlers.keydown(event({ code: "KeyV", ctrlKey: true, repeat: false, getModifierState: () => false }));
handlers.keyup(event({ code: "KeyV", ctrlKey: true, getModifierState: () => false }));
check("nothing is sent before the clipboard is read", take(), []);
await new Promise((resolve) => setTimeout(resolve, 260));
const pasted = take();
check("the clipboard text comes first, then the key press and release",
  [pasted[0] instanceof Uint8Array ? [...pasted[0]] : pasted[0], pasted.slice(1)],
  [[23, ...new TextEncoder().encode("pegado")], [["locks", false, false], ["key", 55, true], ["key", 55, false]]]);
useClipboard(async () => { throw new Error("denied"); });
handlers.keydown(event({ code: "KeyV", ctrlKey: true, repeat: false, getModifierState: () => false }));
await new Promise((resolve) => setTimeout(resolve, 260));
check("a refused clipboard still sends the key", take(), [["key", 55, true]]);

handlers.keyup(event({ code: "KeyV", ctrlKey: true, getModifierState: () => false }));
take();
// Touch: one finger is the left button, two fingers scroll.
const finger = (x, y) => ({ clientX: x, clientY: y });
handlers.touchstart(event({ touches: [finger(10, 20)] }));
check("a finger down only moves the pointer", take(), [["pointer", 0, 0]]);
handlers.touchmove(event({ touches: [finger(110, 120)] }));
check("a moving finger presses the button and drags", take(), [["pointer", 50, 50], ["button", 1, true]]);
handlers.touchend(event({ touches: [] }));
check("lifting the finger releases the button", take(), [["button", 1, false]]);
handlers.touchstart(event({ touches: [finger(10, 100), finger(30, 100)] }));
take();
handlers.touchmove(event({ touches: [finger(10, 20), finger(30, 20)] }));
check("two fingers that move up scroll down in steps of 40 pixels",
  take().filter((m) => m[0] === "button").length, 4);
handlers.touchend(event({ touches: [] }));
check("a scroll does not leave a button down", take(), []);
handlers.touchstart(event({ touches: [finger(10, 20)] }));
handlers.touchmove(event({ touches: [finger(12, 22)] }));
take();
handlers.touchstart(event({ touches: [finger(10, 20), finger(30, 20)] }));
check("a second finger cancels the drag", take(), [["button", 1, false], ["pointer", 0, 0]]);
handlers.touchcancel(event({ touches: [] }));
check("a cancelled touch leaves nothing down", take(), []);

handlers.touchstart(event({ touches: [finger(10, 20)] }));
take();
handlers.touchend(event({ touches: [] }));
check("a tap is a click", take(), [["button", 1, true], ["button", 1, false]]);
handlers.touchstart(event({ touches: [finger(10, 20)] }));
handlers.touchstart(event({ touches: [finger(10, 20), finger(30, 20)] }));
take();
handlers.touchend(event({ touches: [] }));
check("two fingers make no click", take(), []);

// A blur while a paste waits must not jump ahead of the queued keys.
const slow = { resolve: null };
useClipboard(() => new Promise((resolve) => { slow.resolve = resolve; }));
const mod = { getModifierState: () => false };
handlers.keydown(event({ code: "ControlLeft", repeat: false, ...mod }));
take();
handlers.keydown(event({ code: "KeyV", ctrlKey: true, repeat: false, ...mod }));
handlers.keyup(event({ code: "KeyV", ctrlKey: true, ...mod }));
windowHandlers.blur();
check("nothing is sent while the paste waits", take(), []);
await new Promise((resolve) => setTimeout(resolve, 230));
slow.resolve("otro texto");
await new Promise((resolve) => setTimeout(resolve, 260));
const order = take().map((m) => (m instanceof Uint8Array ? "clipboard" : m[0] === "key" ? `key${m[2] ? "+" : "-"}` : m[0]));
check("the release comes after the paste and its keys", order, ["clipboard", "key+", "key-", "release-all"]);

// The same text is sent again (the server decides what changes), and a repeated Ctrl+V does not
// read the clipboard.
let reads = 0;
useClipboard(async () => { reads += 1; return "otro texto"; });
handlers.keydown(event({ code: "KeyV", ctrlKey: true, repeat: false, ...mod }));
await new Promise((resolve) => setTimeout(resolve, 260));
check("an unchanged clipboard is sent again: the server decides", take().some((m) => m instanceof Uint8Array), true);
handlers.keydown(event({ code: "KeyV", ctrlKey: true, repeat: true, ...mod }));
check("a repeated Ctrl+V does not read the clipboard", reads, 1);
handlers.keyup(event({ code: "KeyV", ctrlKey: true, ...mod }));
take();

// A touch that is cancelled during a drag lets go of the button.
handlers.touchstart(event({ touches: [finger(10, 20)] }));
handlers.touchmove(event({ touches: [finger(14, 24)] }));
take();
handlers.touchcancel(event({ touches: [] }));
check("a cancel during a drag releases the button", take(), [["release-all"]]);

// Shift+Insert is a paste key too.
useClipboard(async () => "con insert");
handlers.keydown(event({ code: "Insert", shiftKey: true, repeat: false, ...mod }));
await new Promise((resolve) => setTimeout(resolve, 260));
check("Shift+Insert sends the clipboard before the key", take().some((m) => m instanceof Uint8Array), true);
handlers.keyup(event({ code: "Insert", shiftKey: true, ...mod }));
take();

// Windows sends AltGr as ControlLeft and then AltRight, with the same time stamp. The server must
// see AltGr alone.
socket.readyState = 1;
const altgr = (extra) => event({ getModifierState: (name) => name === "AltGraph", ...extra });
handlers.keydown(altgr({ code: "ControlLeft", repeat: false, timeStamp: 1000 }));
take();
handlers.keydown(altgr({ code: "AltRight", repeat: false, timeStamp: 1001 }));
check("the fake Control is released before AltGr goes down", take(), [["key", 37, false], ["key", 108, true]]);
handlers.keyup(altgr({ code: "ControlLeft", timeStamp: 1100 }));
check("the key up of the fake Control is not sent", take(), []);
handlers.keyup(altgr({ code: "AltRight", timeStamp: 1101 }));
check("AltGr goes up", take(), [["key", 108, false]]);

// A real Ctrl+Alt (no AltGraph state, or the Control pressed long before) is left alone.
handlers.keydown(event({ code: "ControlLeft", repeat: false, timeStamp: 5000, getModifierState: () => false }));
take();
handlers.keydown(event({ code: "AltRight", repeat: false, timeStamp: 5400, getModifierState: () => false }));
check("a real Ctrl then Alt keeps the Control", take(), [["key", 108, true]]);
handlers.keyup(event({ code: "AltRight", timeStamp: 5500, getModifierState: () => false }));
handlers.keyup(event({ code: "ControlLeft", timeStamp: 5600, getModifierState: () => false }));
check("and both go up", take(), [["key", 108, false], ["key", 37, false]]);

// A key can stand for the Super key: this page was started with `remap` empty, so build a second one.
const remapped = {};
const remappedCanvas = { ...canvas, addEventListener: (name, fn) => { remapped[name] = fn; } };
const remappedSent = [];
captureInput(remappedCanvas, { readyState: 1, send: (bytes) => remappedSent.push(bytes) }, encoders, { ControlRight: "MetaLeft" });
remapped.keydown(event({ code: "ControlRight", repeat: false, getModifierState: () => false }));
remapped.keyup(event({ code: "ControlRight", getModifierState: () => false }));
check("a remapped key is sent as the Super key (keycode 133)", remappedSent.filter((m) => m[0] === "key"), [["key", 133, true], ["key", 133, false]]);

// A one-shot Super key from the button on the page: it goes down, stays down for the next key, and
// goes up after that key.
{
  const h = {};
  const cv = { ...canvas, addEventListener: (name, fn) => { h[name] = fn; } };
  const out = [];
  const changes = [];
  const control = captureInput(cv, { readyState: 1, send: (bytes) => out.push(bytes) }, encoders, {}, (on) => changes.push(on));
  const keys = () => out.splice(0).filter((m) => m[0] === "key");
  control.toggleSuper();
  check("the Super button presses Super_L (keycode 133)", keys(), [["key", 133, true]]);
  h.keydown(event({ code: "Enter", repeat: false, getModifierState: () => false }));
  check("the next key goes with Super down", keys(), [["key", 36, true]]);
  h.keyup(event({ code: "Enter", getModifierState: () => false }));
  check("Super goes up after that key", keys(), [["key", 36, false], ["key", 133, false]]);
  check("the button state followed", changes, [true, false]);
  control.toggleSuper();
  keys();
  control.toggleSuper();
  check("a second click cancels Super", keys(), [["key", 133, false]]);
  control.toggleSuper();
  keys();
  h.keydown(event({ code: "ShiftLeft", repeat: false, getModifierState: () => false }));
  h.keyup(event({ code: "ShiftLeft", getModifierState: () => false }));
  check("a modifier key does not end Super", keys(), [["key", 50, true], ["key", 50, false]]);
}

// The paste event carries the text without the Clipboard API: the page sends it before the key.
{
  const h = {};
  const cv = { ...canvas, addEventListener: (name, fn) => { h[name] = fn; } };
  const out = [];
  captureInput(cv, { readyState: 1, send: (bytes) => out.push(bytes) }, encoders);
  const pasteHandler = documentHandlers.paste; // the handler of this instance, the last one added
  useClipboard(async () => { throw new Error("must not be read"); });
  h.keydown(event({ code: "KeyV", ctrlKey: true, shiftKey: true, repeat: false, getModifierState: () => false }));
  await new Promise((resolve) => setTimeout(resolve, 5)); // the browser fires the event after the key handler
  pasteHandler({
    preventDefault() {},
    clipboardData: { getData: (type) => (type === "text/plain" ? "texto del evento paste" : "") },
  });
  await new Promise((resolve) => setTimeout(resolve, 30));
  const text = out.find((m) => m instanceof Uint8Array);
  check("a paste event sends its text, then the key",
    [text ? new TextDecoder().decode(text.subarray(1)) : null, out.some((m) => m[0] === "key")],
    ["texto del evento paste", true]);
  check("the text goes before the key", out.findIndex((m) => m instanceof Uint8Array) < out.findIndex((m) => m[0] === "key"), true);
}

// A paste event with HTML and an image sends one set; content that cannot be pasted sends an empty
// set and tells the user; a clipboard with nothing sends nothing.
{
  const problems = [];
  const run = async (data) => {
    const h = {};
    const cv = { ...canvas, addEventListener: (name, fn) => { h[name] = fn; } };
    const out = [];
    captureInput(cv, { readyState: 1, send: (bytes) => out.push(bytes) }, encoders, {}, () => {}, {
      onClipboardProblem: (text) => problems.push(text),
    });
    const pasteHandler = documentHandlers.paste;
    useClipboard(async () => { throw new Error("must not be read"); });
    h.keydown(event({ code: "KeyV", ctrlKey: true, shiftKey: true, repeat: false, getModifierState: () => false }));
    await new Promise((resolve) => setTimeout(resolve, 5));
    pasteHandler({ preventDefault() {}, clipboardData: data });
    await new Promise((resolve) => setTimeout(resolve, 30));
    return out.filter((m) => m instanceof Uint8Array);
  };
  const png = { type: "image/png", arrayBuffer: async () => new Uint8Array([0x89, 0x50, 0x4e, 0x47]).buffer };
  const rich = await run({
    getData: (type) => ({ "text/plain": "t", "text/html": "<b>t</b>" }[type] ?? ""),
    items: [{ kind: "file", type: "image/png", getAsFile: () => png }],
    types: ["text/plain", "text/html", "Files"],
  });
  check("text, HTML and an image go as one set", [rich.length, rich[0][0], rich[0][1]], [1, 25, 3]);
  const unsupported = await run({ getData: () => "", items: [], types: ["Files"] });
  check("content that cannot be pasted sends an empty set", [unsupported.length, [...unsupported[0]]], [1, [25, 0]]);
  check("and tells the user", problems.at(-1), "The clipboard has nothing that xsrv-web can paste: no text, HTML or image.");
  const nothing = await run({ getData: () => "", items: [], types: [] });
  check("a clipboard with nothing at all sends nothing", nothing.length, 0);
  const huge = await run({ getData: (type) => (type === "text/plain" ? "a".repeat(4 * 1024 * 1024 + 1) : ""), items: [], types: ["text/plain"] });
  check("a text over the limit sends an empty set and says why", [[...huge[0]], problems.at(-1).startsWith("The text is too large to paste")], [[25, 0], true]);
}

// Ctrl+Shift+V pastes with Shift+Insert, which is the key that xterm knows.
{
  const h = {};
  const cv = { ...canvas, addEventListener: (name, fn) => { h[name] = fn; } };
  const out = [];
  captureInput(cv, { readyState: 1, send: (bytes) => out.push(bytes) }, encoders);
  const pasteHandler = documentHandlers.paste;
  const keysOnly = () => out.splice(0).filter((m) => m[0] === "key");
  const calm = { getModifierState: () => false };
  h.keydown(event({ code: "ControlLeft", ctrlKey: true, repeat: false, ...calm }));
  h.keydown(event({ code: "ShiftLeft", ctrlKey: true, shiftKey: true, repeat: false, ...calm }));
  keysOnly();
  h.keydown(event({ code: "KeyV", ctrlKey: true, shiftKey: true, repeat: false, ...calm }));
  await new Promise((resolve) => setTimeout(resolve, 5));
  pasteHandler({ preventDefault() {}, clipboardData: { getData: () => "x" } });
  await new Promise((resolve) => setTimeout(resolve, 30));
  check("Ctrl+Shift+V releases Control and sends Insert (keycode 118) with Shift held",
    keysOnly(), [["key", 37, false], ["key", 118, true], ["key", 118, false]]);
  h.keyup(event({ code: "KeyV", ctrlKey: true, shiftKey: true, ...calm }));
  h.keyup(event({ code: "ControlLeft", shiftKey: true, ...calm }));
  check("the key ups of V and of Control are not sent", keysOnly(), []);
  h.keyup(event({ code: "ShiftLeft", ...calm }));
  check("Shift goes up as usual", keysOnly(), [["key", 50, false]]);
}


// A key combination from a button of the page: down in order, up in reverse, and the wheel speed.
{
  const h = {};
  const cv = { ...canvas, addEventListener: (name, fn) => { h[name] = fn; } };
  const out = [];
  const control = captureInput(cv, { readyState: 1, send: (bytes) => out.push(bytes) }, encoders);
  control.sendCombo(["ControlLeft", "AltLeft", "Delete"]);
  check("Ctrl+Alt+Del goes down in order and up in reverse", out.splice(0), [
    ["key", 37, true], ["key", 64, true], ["key", 119, true],
    ["key", 119, false], ["key", 64, false], ["key", 37, false],
  ]);
  control.toggleSuper();
  out.splice(0);
  control.sendCombo(["KeyE"]);
  check("a combination ends the sticky Super", out.splice(0), [["key", 26, true], ["key", 26, false], ["key", 133, false]]);
  control.setWheelSpeed(4);
  h.wheel(event({ deltaY: 100, deltaX: 0, deltaMode: 0, timeStamp: 0, clientX: 10, clientY: 20 }));
  check("at speed 4 a notch is four clicks", out.splice(0).filter((m) => m[0] === "button").length, 8);
}
