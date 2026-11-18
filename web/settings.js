// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

// The settings: the keyboard layout of the session, the wheel speed, the key that acts as Win /
// Cmd, key replacements and macros, buttons for keys that the browser keeps for itself, and where
// the button bar is. The choices are saved in this browser.
import { CORNER_POSITIONS, cleanPosition, makeDraggable, place, toFractions } from "./barpos.js";
import { applyRemap, cleanMacros, cleanSubstitutions } from "./macros.js";
import { accessText, clipboardAccess, requestClipboardAccess } from "./clipboard.js";
import { createMacroUi } from "./macro-ui.js";
import { createPanel } from "./panel.js";
import { copyScreenshot, saveScreenshot } from "./screenshot.js";
import { WHEEL_SPEEDS } from "./wheel.js";

// The first byte of the server message that carries the layout of the session (`wire::LAYOUT_PREFIX`).
export const LAYOUT_KIND = 7;

const STORAGE_KEY = "xsrv-web.settings";
const LAYOUT_LABELS = { us: "English (US)", es: "Spanish", gb: "English (UK)", de: "German" };
const CUSTOM = "custom";

// KeyboardEvent.code names, pressed in this order and released in reverse.
export const KEY_COMBOS = [
  { label: "Ctrl+Alt+Del", codes: ["ControlLeft", "AltLeft", "Delete"] },
  { label: "Ctrl+W", codes: ["ControlLeft", "KeyW"] },
  { label: "Ctrl+T", codes: ["ControlLeft", "KeyT"] },
  { label: "Ctrl+N", codes: ["ControlLeft", "KeyN"] },
  { label: "Ctrl+Tab", codes: ["ControlLeft", "Tab"] },
  { label: "Alt+Tab", codes: ["AltLeft", "Tab"] },
  { label: "Alt+F4", codes: ["AltLeft", "F4"] },
];

// Keys that can act as the Windows (Cmd) key, for a system or browser that keeps that key. Caps
// Lock is not offered: the system would still switch the caps lock with it.
export const SUPER_KEYS = [
  { code: "", label: "The Windows / Cmd key" },
  { code: "ControlRight", label: "Right Ctrl" },
  { code: "ContextMenu", label: "Menu key" },
  { code: "AltRight", label: "Right Alt (then no AltGr)" },
];

export const CORNERS = [
  { name: "top-right", label: "Top right" },
  { name: "top-left", label: "Top left" },
  { name: "bottom-right", label: "Bottom right" },
  { name: "bottom-left", label: "Bottom left" },
];

// Storage can be missing or refuse (private windows, blocked site data): the page works without it.
function load() {
  try {
    return JSON.parse(localStorage.getItem(STORAGE_KEY)) ?? {};
  } catch {
    return {};
  }
}

function save(settings) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(settings));
  } catch {
    // Not saved; the setting still applies to this page.
  }
}

// What is kept in storage, with the lists cleaned. `dropped` counts the entries that were left out.
function loadSaved() {
  const saved = load();
  const subs = cleanSubstitutions(saved.substitutions);
  const macros = cleanMacros(saved.macros);
  saved.substitutions = subs.items;
  saved.macros = macros.items;
  return { saved, dropped: subs.dropped + macros.dropped };
}

const byId = (id) => document.getElementById(id);

// `layouts` are the layout names by index; `sendLayout(index)` asks the server to switch;
// `input` is what captureInput returned; `remap` is the key map that it reads; `superFromUrl` is
// the key of `?super=`, used until a choice is saved; `showMessage(text)` writes to the message line.
// `flash(text)` shows a message for a few seconds. `onClipboardAccess()` runs when the browser lets
// the page write the clipboard at any time.
export function createSettings({
  layouts,
  sendLayout,
  input,
  canvas,
  remap,
  superFromUrl = "",
  showMessage = () => {},
  flash = showMessage,
  onClipboardAccess = () => {},
}) {
  const { saved, dropped } = loadSaved();
  if (dropped > 0) {
    showMessage(`${dropped} saved key replacement${dropped === 1 ? "" : "s"} or macro${dropped === 1 ? "" : "s"} could not be used and ${dropped === 1 ? "was" : "were"} left out.`);
  }
  const persist = () => save(saved);
  const bar = byId("bar");

  const panel = createPanel({
    panel: byId("settings"),
    toggle: byId("settings-button"),
    close: byId("settings-close"),
    bar,
    tabs: byId("tabs"),
    canvas,
    firstTab: "keyboard",
  });

  // Keyboard
  const layoutSelect = byId("layout");
  layouts.forEach((name, index) => layoutSelect.add(new Option(LAYOUT_LABELS[name] ?? name, index)));
  layoutSelect.addEventListener("change", () => {
    const index = Number(layoutSelect.value);
    saved.layout = layouts[index];
    persist();
    sendLayout(index);
  });

  const superSelect = byId("super-key");
  const superCodes = SUPER_KEYS.map((key) => key.code);
  for (const key of SUPER_KEYS) superSelect.add(new Option(key.label, key.code));
  // A key from `?super=` that is not in the list is shown as it is, so the setting tells the truth.
  if (superFromUrl && !superCodes.includes(superFromUrl)) {
    superSelect.add(new Option(`${superFromUrl} (from the address)`, superFromUrl));
    superCodes.push(superFromUrl);
  }
  let superKey = superCodes.includes(saved.superKey) ? saved.superKey : superFromUrl;
  superSelect.value = superKey;
  // A key that is down now would go up as another key and stay down on the server.
  const keysChanged = () => {
    input.releaseAll();
    applyRemap(remap, superKey, saved.substitutions);
  };
  applyRemap(remap, superKey, saved.substitutions);
  superSelect.addEventListener("change", () => {
    superKey = superSelect.value;
    saved.superKey = superKey;
    persist();
    keysChanged();
  });

  const fullscreenClick = byId("fullscreen-click");
  fullscreenClick.checked = saved.fullscreenOnClick === true;
  fullscreenClick.addEventListener("change", () => {
    saved.fullscreenOnClick = fullscreenClick.checked;
    persist();
  });

  const keys = byId("keys");
  for (const combo of KEY_COMBOS) {
    const button = document.createElement("button");
    button.type = "button";
    button.textContent = combo.label;
    button.addEventListener("click", () => {
      input.sendCombo(combo.codes);
      panel.close();
    });
    keys.append(button);
  }
  byId("super").addEventListener("click", () => {
    input.toggleSuper();
    panel.close();
  });
  byId("super-corner").addEventListener("click", () => {
    input.toggleSuper();
    canvas.focus();
  });

  // Mouse
  const speedSelect = byId("wheel-speed");
  for (const speed of WHEEL_SPEEDS) speedSelect.add(new Option(`${speed}x`, speed));
  const speed = WHEEL_SPEEDS.includes(saved.wheelSpeed) ? saved.wheelSpeed : 1;
  speedSelect.value = speed;
  input.setWheelSpeed(speed);
  speedSelect.addEventListener("change", () => {
    saved.wheelSpeed = Number(speedSelect.value);
    persist();
    input.setWheelSpeed(saved.wheelSpeed);
  });

  // Clipboard tab: may the page write the clipboard whenever X copies? A browser allows it only
  // after the user said Allow once; the button asks, and the state follows the answer.
  const clipStatus = byId("clip-status");
  const clipAllow = byId("clip-allow");
  const showAccess = (state) => {
    clipStatus.textContent = accessText(state);
    clipAllow.hidden = state !== "prompt";
    if (state === "granted") onClipboardAccess();
  };
  clipboardAccess().then(showAccess);
  navigator.permissions?.query?.({ name: "clipboard-read" }).then(
    (status) => {
      status.onchange = () => showAccess(status.state);
    },
    () => {},
  );
  clipAllow.addEventListener("click", async () => showAccess(await requestClipboardAccess()));

  // What X copies by mouse selection (PRIMARY) goes to the browser clipboard only when asked.
  const copyOnSelect = byId("copy-on-select");
  copyOnSelect.checked = saved.copyOnSelect !== false;
  copyOnSelect.addEventListener("change", () => {
    saved.copyOnSelect = copyOnSelect.checked;
    persist();
  });

  // Buttons: the bar goes to a corner or wherever it was dragged.
  const cornerSelect = byId("corner-place");
  for (const corner of CORNERS) cornerSelect.add(new Option(corner.label, corner.name));
  cornerSelect.add(new Option("Where I put it", CUSTOM));
  const grip = byId("grip");
  const customPosition = () => cleanPosition(saved.barPos);
  const currentPosition = () =>
    (saved.corner === CUSTOM ? customPosition() : CORNER_POSITIONS[saved.corner]) ?? CORNER_POSITIONS["top-right"];
  const placeBar = (pos = currentPosition()) => {
    place(bar, pos);
    panel.anchor();
  };
  if (saved.corner !== CUSTOM && !CORNER_POSITIONS[saved.corner]) saved.corner = "top-right";
  if (saved.corner === CUSTOM && !customPosition()) saved.corner = "top-right";
  cornerSelect.value = saved.corner;
  cornerSelect.addEventListener("change", () => {
    saved.corner = cornerSelect.value;
    if (saved.corner === CUSTOM && !customPosition()) saved.barPos = currentPositionFromBar();
    persist();
    placeBar();
  });
  // The bar's own position, for choosing "Where I put it" before any drag.
  const currentPositionFromBar = () => {
    const rect = bar.getBoundingClientRect();
    const view = { width: window.innerWidth, height: window.innerHeight };
    return toFractions(rect.left, rect.top, view, { width: rect.width, height: rect.height });
  };
  makeDraggable({
    bar,
    grip,
    getPos: currentPosition,
    setPos: (pos, final) => {
      bar.classList.toggle("active", !final);
      placeBar(pos);
      if (!final) return;
      saved.corner = CUSTOM;
      saved.barPos = pos;
      cornerSelect.value = CUSTOM;
      persist();
    },
  });
  byId("bar-reset").addEventListener("click", () => {
    saved.corner = "top-right";
    delete saved.barPos;
    cornerSelect.value = saved.corner;
    persist();
    placeBar();
  });
  window.addEventListener("resize", () => placeBar());

  const cornerSuper = byId("corner-super");
  const cornerSuperButton = byId("super-corner");
  const showCornerSuper = (shown) => {
    cornerSuper.checked = shown;
    cornerSuperButton.hidden = !shown;
    placeBar();
  };
  cornerSuper.addEventListener("change", () => {
    saved.cornerSuper = cornerSuper.checked;
    persist();
    showCornerSuper(saved.cornerSuper);
  });

  // The screenshot button: a click saves a PNG file, Shift+click copies the image.
  const shotCheck = byId("corner-screenshot");
  const shotButton = byId("screenshot");
  const showShot = (shown) => {
    shotCheck.checked = shown;
    shotButton.hidden = !shown;
    placeBar();
  };
  shotCheck.addEventListener("change", () => {
    saved.cornerScreenshot = shotCheck.checked;
    persist();
    showShot(saved.cornerScreenshot);
  });
  shotButton.addEventListener("click", async (event) => {
    try {
      if (event.shiftKey) {
        await copyScreenshot(canvas);
        flash("Screenshot copied to the clipboard.");
      } else {
        flash(`Screenshot saved as ${await saveScreenshot(canvas)}.`);
      }
    } catch (error) {
      console.warn("xsrv: screenshot", error);
      flash(error.message);
    }
    canvas.focus();
  });

  // Replacements and macros
  const macroUi = createMacroUi({
    saved,
    persist,
    input,
    canvas,
    onKeysChanged: keysChanged,
    onBarChanged: () => placeBar(),
    afterRun: () => panel.close(),
  });
  showCornerSuper(saved.cornerSuper !== false);
  showShot(saved.cornerScreenshot !== false);

  // The saved layout is sent once, when the session first says which layout it has.
  let savedLayoutChecked = false;
  return {
    onLayout(index) {
      if (!savedLayoutChecked) {
        savedLayoutChecked = true;
        const wanted = layouts.indexOf(saved.layout);
        if (wanted >= 0 && wanted !== index) {
          sendLayout(wanted);
          return;
        }
      }
      layoutSelect.value = index;
    },
    fullscreenOnClick: () => saved.fullscreenOnClick === true,
    copyOnSelect: () => saved.copyOnSelect !== false,
    setMacroRunning: macroUi.setRunning,
  };
}
