// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

import init, {
  Display,
  encode_button,
  encode_key,
  encode_layout,
  encode_locks,
  encode_pointer,
  encode_pointer_left,
  encode_release_all,
  encode_resize,
  layout_name,
} from "./pkg/xsrv_wasm.js";
import { captureInput } from "./input.js";
import { CLIPBOARD_SET_KIND, parseSet, writeSet } from "./clipboard.js";
import { createSettings, LAYOUT_KIND } from "./settings.js";
import { createVideo, VIDEO_KIND } from "./video.js";

await init();
const canvas = document.getElementById("screen");
const display = new Display(canvas);
const video = createVideo(canvas);

// The browser may refuse the write (no permission, or the tab has no focus). Nothing else to do.
// Text that X copied is written to the browser clipboard. A browser may refuse a write that no
// user action started (Firefox and Safari do). The text then waits for the next click or key of
// the user, and is written inside that event. The message line tells the user when it waits.
const message = document.getElementById("message");
const showMessage = (text) => {
  if (!message) return;
  message.textContent = text;
  message.hidden = !text;
};
// A message that goes away after a few seconds, unless another one replaced it.
const FLASH_MS = 4000;
const flashMessage = (text) => {
  showMessage(text);
  setTimeout(() => {
    if (message?.textContent === text) showMessage("");
  }, FLASH_MS);
};
let pendingCopy = null;
// `set` is what X copied: `{ source, text, html, png }`. A refusal for lack of a user action
// makes it wait for the next click or key; any other failure is told to the user.
const writeClipboard = (set) =>
  writeSet(set).then(
    () => {
      pendingCopy = null;
      showMessage("");
    },
    (error) => {
      console.warn("xsrv: clipboard", error);
      if (error?.name === "NotAllowedError" || error?.name === "SecurityError") {
        pendingCopy = set;
        showMessage("Copied content is waiting: click or press a key on the page to put it in the clipboard. To skip this step, allow clipboard access in Settings > Clipboard.");
      } else {
        pendingCopy = null;
        flashMessage(error?.message ?? "The copied content could not be put in the clipboard.");
      }
    },
  );
// What a mouse selection copies is the choice of the user (settings, Mouse tab).
const copyToBrowser = (set) => {
  if (set.source === "primary" && !settings.copyOnSelect()) return;
  writeClipboard(set);
};
for (const name of ["mousedown", "keydown", "touchend"]) {
  document.addEventListener(name, () => {
    if (pendingCopy !== null) writeClipboard(pendingCopy);
  }, true);
}
const scheme = location.protocol === "https:" ? "wss" : "ws";
// `?session=2` shows the second screen of a server that runs several.
const session = new URLSearchParams(location.search).get("session");
const query = session ? `?session=${encodeURIComponent(session)}` : "";
const socket = new WebSocket(`${scheme}://${location.host}/ws${query}`);
socket.binaryType = "arraybuffer";
// Drawing messages wait for the next frame of the browser and are applied together. A client often
// clears a cell and draws the glyph in two writes a few milliseconds apart. Painted one by one,
// the browser could show the cleared cell between them, which looks like a flicker.
const FRAME_FALLBACK_MS = 50;
let waiting = [];
let frameScheduled = false;
// A new screen size clears the canvas. The old picture is kept and drawn again at the top left, so
// the page does not flash blank until the new picture arrives (a window resize, full screen).
const SCREEN_KIND = 0;
const keepPicture = (bytes) => {
  if (bytes[0] !== SCREEN_KIND || canvas.width === 0 || canvas.height === 0) return null;
  const copy = document.createElement("canvas");
  copy.width = canvas.width;
  copy.height = canvas.height;
  copy.getContext("2d").drawImage(canvas, 0, 0);
  return copy;
};
const applyWaiting = () => {
  frameScheduled = false;
  const batch = waiting;
  waiting = [];
  for (const bytes of batch) {
    try {
      const kept = keepPicture(bytes);
      const text = display.apply(bytes);
      if (kept) canvas.getContext("2d").drawImage(kept, 0, 0);
      // Text-only clipboard message (kept for older servers). Empty text copies nothing.
      if (text !== undefined && text !== "") copyToBrowser({ source: "clipboard", text });
    } catch (error) {
      console.error("xsrv: bad message", error);
    }
  }
};
const scheduleFrame = () => {
  if (frameScheduled) return;
  frameScheduled = true;
  // A tab that is not shown gets no frames, so a timer applies the messages anyway.
  requestAnimationFrame(applyWaiting);
  setTimeout(() => frameScheduled && applyWaiting(), FRAME_FALLBACK_MS);
};
socket.onmessage = (event) => {
  const bytes = new Uint8Array(event.data);
  if (bytes[0] === VIDEO_KIND) {
    video.push(bytes.subarray(1));
    return;
  }
  if (bytes[0] === LAYOUT_KIND && bytes.length === 2) {
    settings.onLayout(bytes[1]);
    return;
  }
  if (bytes[0] === CLIPBOARD_SET_KIND) {
    const set = parseSet(bytes);
    if (set) copyToBrowser(set);
    else console.warn("xsrv: bad clipboard message");
    return;
  }
  waiting.push(bytes);
  scheduleFrame();
};
socket.onclose = () => console.error("xsrv: connection closed");
// `?super=<KeyboardEvent.code>` uses that key as the Super (Windows) key, for example
// `?super=ControlRight`, until a key is chosen in the settings (which set `remap`).
const superKey = new URLSearchParams(location.search).get("super");
const remap = {};
const input = captureInput(canvas, socket, {
  encode_button,
  encode_key,
  encode_locks,
  encode_pointer,
  encode_pointer_left,
  encode_release_all,
}, remap, (active) => {
  for (const button of document.querySelectorAll(".super-key")) button.classList.toggle("on", active);
}, {
  onMacroState: (running) => settings.setMacroRunning(running),
  isKeyboardLocked: () => keyboardLocked,
  onMetaWithoutLock: () => tellAboutWinKey(),
  onClipboardProblem: (text) => flashMessage(text),
});
const layouts = [];
for (let index = 0; layout_name(index) !== undefined; index++) layouts.push(layout_name(index));
const settings = createSettings({
  layouts,
  sendLayout: (index) => {
    if (socket.readyState === WebSocket.OPEN) socket.send(encode_layout(index));
  },
  input,
  canvas,
  remap,
  superFromUrl: superKey ?? "",
  showMessage,
  flash: flashMessage,
  // The browser tells that the access changed a moment before it applies it: the waiting copy is
  // tried at once and again a little later, until it goes out.
  onClipboardAccess: () => {
    for (const delay of [0, 300, 1000, 2500, 6000]) {
      setTimeout(() => {
        if (pendingCopy !== null) writeClipboard(pendingCopy);
      }, delay);
    }
  },
});
canvas.focus();

// The screen follows the size of the browser window. `?fit=0` keeps the size of the server.
if (new URLSearchParams(location.search).get("fit") !== "0") {
  let timer;
  const fit = () => {
    if (socket.readyState !== WebSocket.OPEN) return;
    const width = Math.max(64, Math.min(4096, Math.floor(window.innerWidth)));
    const height = Math.max(64, Math.min(4096, Math.floor(window.innerHeight)));
    socket.send(encode_resize(width, height));
  };
  socket.addEventListener("open", fit);
  window.addEventListener("resize", () => {
    clearTimeout(timer);
    timer = setTimeout(fit, 200);
  });
}

// The browser and the system keep some keys for themselves (Escape, the Windows key). In full screen,
// Chromium lets a page take them with the Keyboard Lock API. The button in the corner or F11 on
// the page starts it. Firefox and Safari have no keyboard lock: there the keys stay with the browser.
const LOCKED_KEYS = ["Escape", "MetaLeft", "MetaRight", "AltLeft", "AltRight", "Tab", "ContextMenu"];
let keyboardLocked = false;
let fullscreenAskedAt = 0;
const enterFullscreen = async (reason, hint = "Leave it with the button or by holding Escape.") => {
  fullscreenAskedAt = Date.now();
  try {
    await document.documentElement.requestFullscreen();
    // Says who asked, so that a full screen that nobody asked for can be told from this one.
    flashMessage(`Full screen started by ${reason}. ${hint}`);
    if (navigator.keyboard?.lock) {
      await navigator.keyboard.lock(LOCKED_KEYS);
      keyboardLocked = true;
    }
  } catch (error) {
    console.warn("xsrv: full screen or keyboard lock refused", error);
  }
};
const OWN_REQUEST_MS = 5000;
document.addEventListener("fullscreenchange", () => {
  if (document.fullscreenElement) {
    // A full screen that this page did not ask for: the browser or the system did it.
    if (Date.now() - fullscreenAskedAt > OWN_REQUEST_MS) {
      flashMessage("Full screen was not started by this page: it came from the browser or the system (F11, a shortcut or a gesture).");
    }
    return;
  }
  navigator.keyboard?.unlock?.();
  keyboardLocked = false;
});
// The first click of a double click must not leave the page in full screen (a double click on a
// window of the X screen, or on the button bar that sits over it). The request waits a little, and a
// second click or a double click cancels it. A browser accepts the request a moment after the click.
const FULLSCREEN_DELAY_MS = 350;
let fullscreenTimer = null;
const cancelFullscreen = () => {
  clearTimeout(fullscreenTimer);
  fullscreenTimer = null;
};
let fullscreenEntered = false;
const fullscreenSoon = (reason, hint) => {
  cancelFullscreen();
  fullscreenTimer = setTimeout(() => {
    fullscreenTimer = null;
    fullscreenEntered = true;
    enterFullscreen(reason, hint);
  }, FULLSCREEN_DELAY_MS);
};
// With the option "full screen at my first click" (settings), the first click on the screen starts
// the full screen and the lock: a browser accepts them only from a click or a key of the user.
canvas.addEventListener("click", (event) => {
  if (event.detail > 1) return cancelFullscreen();
  // Once for each load of the page, so that leaving full screen is not undone by the next click.
  if (fullscreenEntered || fullscreenTimer !== null || document.fullscreenElement) return;
  if (settings.fullscreenOnClick()) fullscreenSoon(
      "the option \"full screen at my first click\"",
      "Leave it by holding Escape. Turn the option off in Settings > Keyboard.",
    );
});
canvas.addEventListener("dblclick", cancelFullscreen);
// The system takes the Windows key before the page unless the keyboard is locked. The page cannot
// change that, so it says what to do, once.
let winKeyHinted = false;
const WIN_KEY_HINT_MS = 10000;
function tellAboutWinKey() {
  if (winKeyHinted) return;
  winKeyHinted = true;
  const hint = navigator.keyboard?.lock
    ? "The system keeps the Windows key here. Use full screen (the \u26F6 button), or replace a key in Settings > Keyboard."
    : "This browser cannot keep the Windows key for the page. Use the Win button, or replace a key in Settings > Keyboard.";
  showMessage(hint);
  setTimeout(() => {
    if (message?.textContent === hint) showMessage("");
  }, WIN_KEY_HINT_MS);
}
// The button toggles. A double click on it does nothing.
const fullscreenButton = document.getElementById("fullscreen");
fullscreenButton?.addEventListener("click", (event) => {
  if (document.fullscreenElement) document.exitFullscreen();
  else if (event.detail > 1) cancelFullscreen();
  else fullscreenSoon("the full screen button");
  canvas.focus();
});
fullscreenButton?.addEventListener("dblclick", cancelFullscreen);

