// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

import { keycodeFor } from "./keymap.js";
import { NOTHING_TO_PASTE, encodeSetIn, readClipboardApi, readDataTransfer, toPngBytes } from "./clipboard.js";
import { comboFromEvent } from "./macros.js";
import { wheelAxis } from "./wheel.js";

const LEFT_BUTTON = 1;
const MIDDLE_BUTTON = 2;
const RIGHT_BUTTON = 3;
const WHEEL_UP = 4;
const WHEEL_DOWN = 5;
const WHEEL_LEFT = 6;
const WHEEL_RIGHT = 7;
const BROWSER_BUTTONS = [LEFT_BUTTON, MIDDLE_BUTTON, RIGHT_BUTTON];
// More would let one big wheel event flood the server, which closes a page that sends too much.
const MAX_CLICKS_PER_WHEEL_EVENT = 10;

// Sends mouse and keyboard input of the canvas to the server through `socket`.
const MODIFIER_CODES = new Set([
  "ShiftLeft", "ShiftRight", "ControlLeft", "ControlRight", "AltLeft", "AltRight",
  "MetaLeft", "MetaRight", "CapsLock", "NumLock",
]);

// Pauses between the steps of a macro, and the time the clipboard text gets to reach the server
// before the paste key.
const MACRO_STEP_GAP_MS = 20;
const MACRO_PASTE_WAIT_MS = 60;

// Waits `ms`, and stops early when the macro is cancelled.
async function cancellableSleep(ms, state) {
  const end = performance.now() + ms;
  while (!state.cancelled && performance.now() < end) {
    await new Promise((resolve) => setTimeout(resolve, Math.min(50, end - performance.now())));
  }
}

// `options`: `sleep(ms, state)` for tests, `onMacroState(running)` for a button that shows a macro
// running, `isKeyboardLocked()` and `onMetaWithoutLock()` to tell the user that the system keeps
// the Windows key, and `onClipboardProblem(text)` for a clipboard that cannot be pasted.
export function captureInput(
  canvas,
  socket,
  encoders,
  remap = {},
  onSuperChange = () => {},
  options = {},
) {
  const sleep = options.sleep ?? cancellableSleep;
  const onMacroState = options.onMacroState ?? (() => {});
  const isKeyboardLocked = options.isKeyboardLocked ?? (() => true);
  const onMetaWithoutLock = options.onMetaWithoutLock ?? (() => {});
  const onClipboardProblem = options.onClipboardProblem ?? (() => {});
  const heldButtons = new Set();
  const heldKeys = new Set();
  let locks = { caps: undefined, num: undefined };
  // While a paste waits for the browser clipboard, later keys wait behind it.
  let pending = null;
  const enqueue = (action) => {
    const chained = (pending ?? Promise.resolve()).then(action).finally(() => {
      if (pending === chained) pending = null;
    });
    pending = chained;
  };
  // The time of the last ControlLeft press, to tell the fake one that Windows sends before AltGr.
  const FAKE_CONTROL_MS = 30;
  let controlDownAt = null;
  let ignoreControlUp = false;
  let swallowVUp = false;
  let superSticky = false;
  // Macro hotkeys by their `comboFromEvent` text. The keys that a hotkey took from the screen are
  // remembered, so that their key-ups do not reach it: `hotkeyUps` holds the KeyboardEvent.code of
  // the hotkey itself, `silenced` the keycodes of the keys that were down and went up for it.
  let hotkeys = new Map();
  const hotkeyUps = new Set();
  const silenced = new Set();
  let running = null;
  let wheelSpeed = 1;
  const wheelY = wheelAxis();
  const wheelX = wheelAxis();

  const send = (bytes) => {
    if (socket.readyState === WebSocket.OPEN) socket.send(bytes);
  };
  const position = (event) => {
    const rect = canvas.getBoundingClientRect();
    const x = ((event.clientX - rect.left) * canvas.width) / rect.width;
    const y = ((event.clientY - rect.top) * canvas.height) / rect.height;
    const clamp = (value, size) => Math.min(Math.max(Math.floor(value), 0), size - 1);
    return [clamp(x, canvas.width), clamp(y, canvas.height)];
  };
  const movePointer = (event) => send(encoders.encode_pointer(...position(event)));
  const click = (button) => {
    send(encoders.encode_button(button, true));
    send(encoders.encode_button(button, false));
  };
  // The page knows the lock keys of the user's keyboard. The server cannot guess them.
  const syncLocks = (event) => {
    if (typeof event.getModifierState !== "function") return;
    const caps = event.getModifierState("CapsLock");
    const num = event.getModifierState("NumLock");
    if (caps === locks.caps && num === locks.num) return;
    locks = { caps, num };
    send(encoders.encode_locks(caps, num));
  };
  // A key or button that is down when the page loses the focus would stay down for ever.
  const releaseNow = () => {
    hotkeyUps.clear();
    silenced.clear();
    if (running) running.cancelled = true;
    if (heldButtons.size === 0 && heldKeys.size === 0) return;
    heldButtons.clear();
    heldKeys.clear();
    send(encoders.encode_release_all());
  };
  // Keys that wait for a paste go first, so the release cannot jump ahead of them.
  const releaseAll = () => {
    // A running macro stops at once; the release itself waits behind it, so it cannot jump ahead.
    if (running) running.cancelled = true;
    return pending ? enqueue(releaseNow) : releaseNow();
  };

  canvas.addEventListener("mousemove", movePointer);
  canvas.addEventListener("mouseleave", () => send(encoders.encode_pointer_left()));
  canvas.addEventListener("mousedown", (event) => {
    canvas.focus();
    const button = BROWSER_BUTTONS[event.button];
    if (button === undefined) return;
    movePointer(event);
    heldButtons.add(button);
    send(encoders.encode_button(button, true));
    event.preventDefault();
  });
  // On the window, so that a release outside the canvas still reaches the server.
  window.addEventListener("mouseup", (event) => {
    const button = BROWSER_BUTTONS[event.button];
    if (button === undefined || !heldButtons.has(button)) return;
    movePointer(event);
    heldButtons.delete(button);
    send(encoders.encode_button(button, false));
    event.preventDefault();
  });
  // One finger is the left button. Two fingers that move up or down scroll.
  const SCROLL_STEP_PIXELS = 40;
  let touchMode = null;
  let scrollFrom = 0;
  const averageY = (touches) =>
    Array.from(touches).reduce((sum, t) => sum + t.clientY, 0) / touches.length;
  const endTouch = () => {
    if (touchMode === "pending") click(LEFT_BUTTON);
    if (touchMode === "drag") {
      heldButtons.delete(LEFT_BUTTON);
      send(encoders.encode_button(LEFT_BUTTON, false));
    }
    touchMode = null;
  };
  canvas.addEventListener("touchstart", (event) => {
    canvas.focus();
    const touches = event.touches;
    if (touches.length === 1 && touchMode === null) {
      // The button goes down at the first move, or as a click when the finger lifts. A second
      // finger that lands first makes a scroll, and no click comes out of it.
      movePointer(touches[0]);
      touchMode = "pending";
    } else if (touches.length === 2) {
      if (touchMode === "drag") endTouch();
      touchMode = "scroll";
      scrollFrom = averageY(touches);
      movePointer(touches[0]);
    }
    event.preventDefault();
  }, { passive: false });
  canvas.addEventListener("touchmove", (event) => {
    const touches = event.touches;
    if (touchMode === "pending" && touches.length === 1) {
      movePointer(touches[0]);
      heldButtons.add(LEFT_BUTTON);
      send(encoders.encode_button(LEFT_BUTTON, true));
      touchMode = "drag";
    } else if (touchMode === "drag" && touches.length === 1) {
      movePointer(touches[0]);
    } else if (touchMode === "scroll" && touches.length === 2) {
      const y = averageY(touches);
      while (Math.abs(y - scrollFrom) >= SCROLL_STEP_PIXELS) {
        click(y < scrollFrom ? WHEEL_DOWN : WHEEL_UP);
        scrollFrom += y < scrollFrom ? -SCROLL_STEP_PIXELS : SCROLL_STEP_PIXELS;
      }
    }
    event.preventDefault();
  }, { passive: false });
  canvas.addEventListener("touchend", (event) => {
    if (event.touches.length === 0) endTouch();
    event.preventDefault();
  }, { passive: false });
  canvas.addEventListener("touchcancel", () => {
    touchMode = null;
    releaseAll();
  });
  window.addEventListener("blur", releaseAll);
  document.addEventListener("visibilitychange", () => {
    if (document.hidden) releaseAll();
  });
  canvas.addEventListener("contextmenu", (event) => event.preventDefault());
  canvas.addEventListener(
    "wheel",
    (event) => {
      movePointer(event);
      const now = event.timeStamp ?? performance.now();
      const scroll = (axis, delta, back, forward) => {
        const count = axis.clicks(delta, event.deltaMode, wheelSpeed, now);
        const steps = Math.min(Math.abs(count), MAX_CLICKS_PER_WHEEL_EVENT);
        for (let i = 0; i < steps; i++) click(count < 0 ? back : forward);
      };
      scroll(wheelY, event.deltaY, WHEEL_UP, WHEEL_DOWN);
      scroll(wheelX, event.deltaX, WHEEL_LEFT, WHEEL_RIGHT);
      event.preventDefault();
    },
    { passive: false },
  );

  // The server gets the browser clipboard before the paste key, so the X client pastes the right
  // content: text, HTML and an image. The server decides whether it changes anything, so the same
  // content can be sent again. A browser that refuses to read the clipboard just sends the key.
  const CLIPBOARD_TIMEOUT_MS = 1500;
  // `content` is what `readDataTransfer` or `readClipboardApi` gave. A clipboard with nothing at
  // all is not sent (the X clipboard stays as it is). Content that cannot be pasted, or is too
  // big, makes X paste nothing, and the user is told why.
  const sendClipboard = async (content) => {
    if (!content.hasContent) return;
    let png = null;
    if (content.image) {
      try {
        png = await toPngBytes(content.image);
      } catch (error) {
        console.warn("xsrv: image", error);
      }
    }
    const { message, problems, empty } = encodeSetIn({ text: content.text, html: content.html, png });
    if (empty || problems.length > 0) {
      onClipboardProblem(problems.length > 0 ? problems.join(" ") : NOTHING_TO_PASTE);
    }
    send(message);
  };

  // The browser may ask the user for permission to read the clipboard. The keys do not wait longer
  // than the timeout, but the answer is still used when it comes: the content goes to the server
  // then, and the next paste finds the permission given.
  const pushClipboard = async () => {
    const reading = readClipboardApi(navigator.clipboard);
    const timeout = new Promise((resolve) => setTimeout(resolve, CLIPBOARD_TIMEOUT_MS, null));
    const content = await Promise.race([reading, timeout]);
    if (content === null) {
      reading.then(sendClipboard, () => {});
      return;
    }
    await sendClipboard(content);
  };

  // Ctrl+V and Shift+Insert are the paste keys of a browser. A held key repeats, and only the
  // first press reads the clipboard.
  const isPaste = (event, pressed) =>
    pressed &&
    !event.repeat &&
    ((event.ctrlKey && event.code === "KeyV") || (event.shiftKey && event.code === "Insert"));

  // The browser fires a `paste` event for these keys, and the event carries the clipboard text
  // without any permission prompt. That is the way to read the clipboard that works in every
  // browser, so the key is not cancelled and the page waits a moment for the event. Without the
  // event (some browsers and settings), the page reads the clipboard with the Clipboard API.
  const PASTE_EVENT_WAIT_MS = 150;
  let waitingPaste = null;
  // Firefox fires `paste` only at an editable element. A hidden text box takes the focus while the
  // paste keys are down, and the canvas gets it back when the text is in.
  const pasteTarget =
    typeof document.getElementById === "function" ? document.getElementById("paste-target") : null;
  document.addEventListener("paste", (event) => {
    if (!waitingPaste) return;
    event.preventDefault();
    const finish = waitingPaste;
    // The event data is read now: it is only valid while the event runs.
    const content = readDataTransfer(event.clipboardData);
    // Some browsers leave an image out of the event data. When the event is empty the Clipboard
    // API is asked (a clipboard that is really empty gives nothing there either).
    const sending = content.hasContent || !navigator.clipboard ? sendClipboard(content) : pushClipboard();
    sending.catch((error) => console.warn("xsrv: clipboard", error)).finally(finish);
  });
  const waitForPasteEvent = () =>
    new Promise((resolve) => {
      let timer = null;
      const finish = () => {
        clearTimeout(timer);
        waitingPaste = null;
        if (pasteTarget) {
          pasteTarget.value = "";
          canvas.focus();
        }
        resolve();
      };
      waitingPaste = finish;
      timer = setTimeout(() => {
        waitingPaste = null;
        if (pasteTarget) canvas.focus();
        const read = navigator.clipboard?.readText ? pushClipboard() : Promise.resolve();
        read.catch((error) => console.warn("xsrv: clipboard", error)).then(resolve);
      }, PASTE_EVENT_WAIT_MS);
    });

  const key = (event, pressed) => {
    if (pressed && (event.code === "MetaLeft" || event.code === "MetaRight") && !isKeyboardLocked()) {
      onMetaWithoutLock();
    }
    if (hotkeyUps.has(event.code)) {
      // The hotkey is still down (a repeat) or goes up: neither is for the screen.
      event.preventDefault();
      if (!pressed) hotkeyUps.delete(event.code);
      return;
    }
    const macro = pressed ? hotkeys.get(comboFromEvent(event)) : undefined;
    if (macro) {
      event.preventDefault();
      hotkeyUps.add(event.code);
      takeHeldKeys();
      runMacro(macro.steps);
      return;
    }
    // A key can stand for another one: `?super=ControlRight` makes the right Control key the Super
    // key, for a system that keeps the Windows key for itself.
    const keycode = keycodeFor(remap[event.code] ?? event.code);
    if (keycode === undefined) {
      console.warn(`xsrv: key ${event.code} has no X11 keycode and is ignored`);
      return;
    }
    if (!pressed && silenced.delete(keycode)) {
      event.preventDefault();
      return;
    }
    // The paste keys keep their default action, so that the browser fires the paste event.
    if (!isPaste(event, pressed)) event.preventDefault();
    // On Windows, AltGr is sent as ControlLeft and then AltRight, with the same time. The X server
    // must see AltGr alone, or Ctrl+AltGr+2 would type a control character and not `@`.
    const controlCode = keycodeFor("ControlLeft");
    if (pressed && event.code === "ControlLeft") controlDownAt = event.timeStamp;
    if (!pressed && event.code === "ControlLeft" && ignoreControlUp) {
      ignoreControlUp = false;
      return;
    }
    const isAltGr =
      pressed &&
      event.code === "AltRight" &&
      event.getModifierState?.("AltGraph") &&
      heldKeys.has(controlCode) &&
      controlDownAt !== null &&
      event.timeStamp - controlDownAt < FAKE_CONTROL_MS;
    // Ctrl+Shift+V is the paste key of terminals, but `xterm` and many other X programs paste only
    // with Shift+Insert. The text is already in the server, so the programs get Shift+Insert.
    const asShiftInsert =
      pressed && event.ctrlKey && event.shiftKey && event.code === "KeyV" && isPaste(event, pressed);
    if (!pressed && event.code === "KeyV" && swallowVUp) {
      swallowVUp = false;
      return;
    }
    const sendKey = () => {
      if (asShiftInsert) {
        for (const control of [keycodeFor("ControlLeft"), keycodeFor("ControlRight")]) {
          if (heldKeys.delete(control)) send(encoders.encode_key(control, false));
        }
        ignoreControlUp = true;
        swallowVUp = true;
        const insert = keycodeFor("Insert");
        send(encoders.encode_key(insert, true));
        send(encoders.encode_key(insert, false));
        return;
      }
      if (isAltGr) {
        heldKeys.delete(controlCode);
        send(encoders.encode_key(controlCode, false));
        ignoreControlUp = true;
      }
      syncLocks(event);
      // The server wants a release before each repeated press, as a real X server sends.
      if (pressed && event.repeat) send(encoders.encode_key(keycode, false));
      if (pressed) heldKeys.add(keycode);
      else heldKeys.delete(keycode);
      send(encoders.encode_key(keycode, pressed));
      // A one-shot Super (see `toggleSuper`) ends after the key that it was held for.
      if (!pressed && superSticky && !MODIFIER_CODES.has(event.code)) releaseSuper();
    };
    if (isPaste(event, pressed)) {
      // The focus moves now, while the key is down, so that the paste goes to the text box.
      pasteTarget?.focus({ preventScroll: true });
      enqueue(() => waitForPasteEvent().then(sendKey));
    } else if (pending) {
      enqueue(sendKey);
    } else {
      sendKey();
    }
  };
  canvas.addEventListener("keydown", (event) => key(event, true));
  canvas.addEventListener("keyup", (event) => key(event, false));
  // While the text box has the focus, its keys are the keys of the screen.
  pasteTarget?.addEventListener("keydown", (event) => key(event, true));
  pasteTarget?.addEventListener("keyup", (event) => key(event, false));
  // The page tells the input code which text came from X, so a paste does not send it back.
  // A Super key that stays down until the next key: a button on the page can set it, for systems
  // and browsers that keep the Windows key (and for a touch screen, which has no keyboard).
  const SUPER_KEYCODE = keycodeFor("MetaLeft");
  const releaseSuper = () => {
    if (!superSticky) return;
    superSticky = false;
    heldKeys.delete(SUPER_KEYCODE);
    send(encoders.encode_key(SUPER_KEYCODE, false));
    onSuperChange(false);
  };
  const toggleSuper = () => {
    if (superSticky) {
      releaseSuper();
      return;
    }
    superSticky = true;
    heldKeys.add(SUPER_KEYCODE);
    send(encoders.encode_key(SUPER_KEYCODE, true));
    onSuperChange(true);
  };
  // A key combination from a button of the page (keys that the browser keeps, such as Ctrl+W):
  // the keys go down in order and up in reverse. `codes` are KeyboardEvent.code names.
  const sendComboNow = (codes) => {
    const keycodes = codes.map(keycodeFor).filter((keycode) => keycode !== undefined);
    for (const keycode of keycodes) send(encoders.encode_key(keycode, true));
    for (const keycode of [...keycodes].reverse()) send(encoders.encode_key(keycode, false));
    releaseSuper();
  };
  const sendCombo = (codes) => (pending ? enqueue(() => sendComboNow(codes)) : sendComboNow(codes));
  // The keys that are down go up before a macro: its combos must not meet a Ctrl that the user
  // still holds for the hotkey. Their key-ups are dropped later.
  const takeHeldKeys = () => {
    releaseSuper();
    for (const keycode of heldKeys) {
      send(encoders.encode_key(keycode, false));
      silenced.add(keycode);
    }
    heldKeys.clear();
  };
  const playSteps = async (steps, state) => {
    for (const step of steps) {
      if (state.cancelled) return;
      if (step.type === "combo") {
        sendComboNow(step.codes);
      } else if (step.type === "wait") {
        await sleep(step.ms, state);
      } else {
        await sendClipboard({ text: step.text, html: "", image: null, hasContent: true });
        await sleep(MACRO_PASTE_WAIT_MS, state);
        if (state.cancelled) return;
        sendComboNow(["ShiftLeft", "Insert"]);
      }
      await sleep(MACRO_STEP_GAP_MS, state);
    }
  };
  // Runs `steps` (from `parseSteps`) in order with the other keys. A macro that is running when
  // another one is asked for, or when the page loses the focus, stops.
  const runMacro = (steps) => {
    if (running) {
      running.cancelled = true;
      return;
    }
    const state = { cancelled: false };
    running = state;
    onMacroState(true);
    enqueue(async () => {
      try {
        await playSteps(steps, state);
      } finally {
        running = null;
        onMacroState(false);
      }
    });
  };
  // `list` items have a `hotkey` text and parsed `steps`; items without a hotkey are ignored here.
  const setMacros = (list) => {
    const usable = list.filter((macro) => macro.hotkey && Array.isArray(macro.steps));
    hotkeys = new Map(usable.map((macro) => [macro.hotkey, macro]));
  };
  const setWheelSpeed = (speed) => {
    if (Number.isFinite(speed) && speed > 0) wheelSpeed = speed;
  };
  return {
    releaseAll,
    toggleSuper,
    sendCombo,
    setWheelSpeed,
    runMacro,
    setMacros,
  };
}
