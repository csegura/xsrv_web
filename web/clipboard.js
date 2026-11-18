// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

// The clipboard between the browser and X: text, HTML and a PNG image. The formats of the
// messages are in crates/wire/src/clipboard.rs.

export const CLIPBOARD_SET_KIND = 12;
const CLIPBOARD_IN_KIND = 23;
const CLIPBOARD_SET_IN_KIND = 25;
const FORMAT = { text: 1, html: 2, png: 3 };
export const MAX_TEXT_BYTES = 4 * 1024 * 1024;
export const MAX_PNG_BYTES = 16 * 1024 * 1024;

export const NO_CLIPBOARD =
  "The browser gives this page no clipboard, because the page is not on HTTPS. Open it through HTTPS or through localhost (an SSH tunnel).";
export const NOTHING_TO_PASTE = "The clipboard has nothing that xsrv-web can paste: no text, HTML or image.";

const megabytes = (bytes) => (bytes / (1024 * 1024)).toFixed(1);

// The message for the server. `content` has `text` and `html` (strings, may be empty) and `png`
// (a Uint8Array or null). A part that is over its limit is left out and named in `problems`. A
// message with text only is the short text message; `empty` is true when no part is left (the
// message is then a set of zero parts, which makes X paste nothing).
export function encodeSetIn({ text = "", html = "", png = null }) {
  const encoder = new TextEncoder();
  const parts = [];
  const problems = [];
  const add = (format, bytes, limit, label) => {
    if (bytes.length === 0) return;
    if (bytes.length > limit) {
      problems.push(`The ${label} is too large to paste (${megabytes(bytes.length)} MB; the limit is ${megabytes(limit)} MB).`);
      return;
    }
    parts.push({ format, bytes });
  };
  add(FORMAT.text, encoder.encode(text), MAX_TEXT_BYTES, "text");
  add(FORMAT.html, encoder.encode(html), MAX_TEXT_BYTES, "HTML");
  add(FORMAT.png, png ?? new Uint8Array(0), MAX_PNG_BYTES, "image");
  if (parts.length === 1 && parts[0].format === FORMAT.text) {
    const message = new Uint8Array(1 + parts[0].bytes.length);
    message[0] = CLIPBOARD_IN_KIND;
    message.set(parts[0].bytes, 1);
    return { message, problems, empty: false };
  }
  const size = 2 + parts.reduce((sum, part) => sum + 5 + part.bytes.length, 0);
  const message = new Uint8Array(size);
  message[0] = CLIPBOARD_SET_IN_KIND;
  message[1] = parts.length;
  const view = new DataView(message.buffer);
  let at = 2;
  for (const { format, bytes } of parts) {
    message[at] = format;
    view.setUint32(at + 1, bytes.length, true);
    message.set(bytes, at + 5);
    at += 5 + bytes.length;
  }
  return { message, problems, empty: parts.length === 0 };
}

// A set that X sent: `{ source: "clipboard" | "primary", text, html, png }` (what is missing is
// undefined), or null when the message is not well formed.
export function parseSet(bytes) {
  if (bytes.length < 3 || bytes[0] !== CLIPBOARD_SET_KIND) return null;
  const source = bytes[1] === 0 ? "clipboard" : bytes[1] === 1 ? "primary" : null;
  if (source === null) return null;
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const decoder = new TextDecoder("utf-8", { fatal: true });
  const set = { source };
  let at = 3;
  for (let part = 0; part < bytes[2]; part++) {
    if (at + 5 > bytes.length) return null;
    const format = bytes[at];
    const length = view.getUint32(at + 1, true);
    const data = bytes.subarray(at + 5, at + 5 + length);
    if (data.length !== length) return null;
    try {
      if (format === FORMAT.text) set.text = decoder.decode(data);
      else if (format === FORMAT.html) set.html = decoder.decode(data);
      else if (format === FORMAT.png) set.png = data.slice();
      else return null;
    } catch {
      return null;
    }
    at += 5 + length;
  }
  return at === bytes.length ? set : null;
}

// What a paste event holds, read at once: the event data is only valid while the event runs.
// `image` is a Blob (or null) and `hasContent` is false for a clipboard with nothing at all.
export function readDataTransfer(data) {
  let image = null;
  for (const item of data?.items ?? []) {
    if (item.kind === "file" && item.type.startsWith("image/")) {
      image = item.getAsFile();
      break;
    }
  }
  return {
    text: data?.getData?.("text/plain") ?? "",
    html: data?.getData?.("text/html") ?? "",
    image,
    hasContent: (data?.types?.length ?? 1) > 0,
  };
}

// An image as PNG bytes. A PNG is passed on as it is; another type is drawn on a canvas.
export async function toPngBytes(blob) {
  if (blob.type === "image/png") return new Uint8Array(await blob.arrayBuffer());
  const bitmap = await createImageBitmap(blob);
  const canvas = new OffscreenCanvas(bitmap.width, bitmap.height);
  canvas.getContext("2d").drawImage(bitmap, 0, 0);
  const png = await canvas.convertToBlob({ type: "image/png" });
  return new Uint8Array(await png.arrayBuffer());
}

// What the Clipboard API holds (it may ask the user for permission): the same shape as
// `readDataTransfer`. A browser without `read()` gives the text only.
export async function readClipboardApi(clipboard) {
  if (typeof clipboard.read !== "function") {
    const text = await clipboard.readText();
    return { text, html: "", image: null, hasContent: text !== "" };
  }
  const items = await clipboard.read();
  const content = { text: "", html: "", image: null, hasContent: items.length > 0 };
  for (const item of items) {
    if (item.types.includes("text/plain") && !content.text) content.text = await (await item.getType("text/plain")).text();
    if (item.types.includes("text/html") && !content.html) content.html = await (await item.getType("text/html")).text();
    const imageType = item.types.find((type) => type.startsWith("image/"));
    if (imageType && !content.image) content.image = await item.getType(imageType);
  }
  return content;
}

// Whether the page may write the clipboard whenever X copies: "granted", "prompt" (the browser
// asks once, after a click on the button of the settings), "denied", "unsupported" (a browser
// that cannot say, such as Firefox), or "insecure" (a page on plain HTTP gets no clipboard at
// all). Without it a browser lets a page write the clipboard only within about five seconds
// after a click or a key on the page, which X copies often miss.
export async function clipboardAccess(nav = navigator, secure = globalThis.isSecureContext) {
  if (secure === false) return "insecure";
  try {
    return (await nav.permissions.query({ name: "clipboard-read" })).state;
  } catch {
    return "unsupported";
  }
}

// Asks the browser for the access. It must run in a click of the user. The answer is read back,
// because the question may stay open or be refused.
export async function requestClipboardAccess(nav = navigator) {
  try {
    await nav.clipboard.readText();
  } catch {
    // Refused, or the user has not answered yet: the state tells which.
  }
  return clipboardAccess(nav);
}

// What the settings say about each state.
export function accessText(state) {
  switch (state) {
    case "granted":
      return "Allowed: what you copy in an X program reaches your clipboard at any time.";
    case "prompt":
      return "Not allowed yet. A copy in an X program reaches your clipboard only within five seconds after you click or press a key on the page. Click the button and answer Allow.";
    case "denied":
      return "Blocked by the browser. Allow it in the site settings (the icon at the left of the address), then reload the page.";
    case "insecure":
      return `${NO_CLIPBOARD} Ctrl+V on the page still pastes into X programs.`;
    default:
      return "This browser does not say whether it is allowed. A copy in an X program reaches your clipboard when you click or press a key on the page soon after.";
  }
}

// Puts a set that X sent in the browser clipboard. Browsers that cannot hold HTML or an image
// get the text only; a set with no text and no way to write it is an error.
export async function writeSet(set, env = {}) {
  const clipboard = env.clipboard ?? navigator.clipboard;
  const Item = env.ClipboardItem ?? globalThis.ClipboardItem;
  const types = {};
  if (set.text) types["text/plain"] = new Blob([set.text], { type: "text/plain" });
  if (set.html) types["text/html"] = new Blob([set.html], { type: "text/html" });
  if (set.png) types["image/png"] = new Blob([set.png], { type: "image/png" });
  if (Object.keys(types).length === 0) return;
  if (!clipboard) throw new Error(NO_CLIPBOARD);
  if (typeof clipboard?.write === "function" && Item) {
    try {
      await clipboard.write([new Item(types)]);
      return;
    } catch (error) {
      // A type that the browser does not know: try again with the text alone. A refusal for lack
      // of a user action is not retried, the page waits for the next click.
      if (!(error instanceof TypeError) || !set.text) throw error;
    }
  }
  if (!set.text) throw new Error("This browser cannot copy an image or HTML to the clipboard.");
  await clipboard.writeText(set.text);
}
