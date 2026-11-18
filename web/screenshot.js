// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

// Saves or copies what the screen shows now. The canvas holds the whole X screen, so its pixels are
// the screenshot. No DOM at import time, so node can test the parts.

// `xsrv-web-20261112-153045.png`, in the local time of the user.
export function screenshotName(date = new Date()) {
  const pad = (value) => String(value).padStart(2, "0");
  const day = `${date.getFullYear()}${pad(date.getMonth() + 1)}${pad(date.getDate())}`;
  const time = `${pad(date.getHours())}${pad(date.getMinutes())}${pad(date.getSeconds())}`;
  return `xsrv-web-${day}-${time}.png`;
}

export function canvasToBlob(canvas) {
  return new Promise((resolve, reject) => {
    canvas.toBlob(
      (blob) => (blob ? resolve(blob) : reject(new Error("The browser could not make an image of the screen."))),
      "image/png",
    );
  });
}

const REVOKE_AFTER_MS = 10000;

// Downloads the screenshot as a PNG file. `env` is for tests: `document`, `URL` and `now`.
export async function saveScreenshot(canvas, env = {}) {
  const doc = env.document ?? document;
  const urls = env.URL ?? URL;
  const blob = await canvasToBlob(canvas);
  const url = urls.createObjectURL(blob);
  const link = doc.createElement("a");
  link.href = url;
  link.download = screenshotName(env.now);
  doc.body.append(link);
  link.click();
  link.remove();
  setTimeout(() => urls.revokeObjectURL(url), REVOKE_AFTER_MS);
  return link.download;
}

// Puts the screenshot on the clipboard of the browser. Browsers without image clipboard support
// (or without permission) refuse, and the error says so.
export async function copyScreenshot(canvas, env = {}) {
  const clipboard = env.navigator?.clipboard ?? navigator.clipboard;
  const Item = env.ClipboardItem ?? globalThis.ClipboardItem;
  if (!clipboard?.write || !Item) {
    throw new Error("This browser cannot copy an image to the clipboard. Use the file instead.");
  }
  const blob = await canvasToBlob(canvas);
  await clipboard.write([new Item({ "image/png": blob })]);
}
