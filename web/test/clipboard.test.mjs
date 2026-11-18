// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

import {
  CLIPBOARD_SET_KIND, MAX_PNG_BYTES, MAX_TEXT_BYTES, NO_CLIPBOARD, accessText, clipboardAccess, encodeSetIn, parseSet,
  readClipboardApi, readDataTransfer, requestClipboardAccess, toPngBytes, writeSet,
} from "../clipboard.js";

const check = (label, actual, expected) => {
  const ok = JSON.stringify(actual) === JSON.stringify(expected);
  console.log(ok ? "ok  " : "FAIL", label, ok ? "" : JSON.stringify(actual) + " != " + JSON.stringify(expected));
  if (!ok) process.exitCode = 1;
};
const decode = (bytes) => new TextDecoder().decode(bytes);

{
  const { message, problems, empty } = encodeSetIn({ text: "hola ñ" });
  check("text only is the short text message", [message[0], decode(message.subarray(1)), problems, empty], [23, "hola ñ", [], false]);
}
{
  const png = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 1, 2]);
  const { message } = encodeSetIn({ text: "t", html: "<b>h</b>", png });
  const view = new DataView(message.buffer);
  check("a set starts with its kind and part count", [message[0], message[1]], [25, 3]);
  check("the first part is the text with its length", [message[2], view.getUint32(3, true), decode(message.subarray(7, 8))], [1, 1, "t"]);
  check("the whole length adds up", message.length, 2 + (5 + 1) + (5 + 8) + (5 + 6));
}
{
  const { message, empty } = encodeSetIn({});
  check("nothing to paste is a set of zero parts", [[...message], empty], [[25, 0], true]);
}
{
  const big = encodeSetIn({ text: "a".repeat(MAX_TEXT_BYTES + 1) });
  check("a text over the limit is left out, with a reason", [big.empty, big.problems], [true, ["The text is too large to paste (4.0 MB; the limit is 4.0 MB)."]]);
  const image = encodeSetIn({ text: "ok", png: new Uint8Array(MAX_PNG_BYTES + 1) });
  check("the rest of the content still goes", [image.empty, image.message[0], decode(image.message.subarray(1)), image.problems.length], [false, 23, "ok", 1]);
}

const part = (format, bytes) => [format, bytes.length & 255, (bytes.length >> 8) & 255, 0, 0, ...bytes];
const enc = (text) => [...new TextEncoder().encode(text)];
{
  const message = new Uint8Array([CLIPBOARD_SET_KIND, 1, 2, ...part(1, enc("sel ñ")), ...part(3, [0x89, 0x50])]);
  const set = parseSet(message);
  check("a set from X", [set.source, set.text, [...set.png], set.html], ["primary", "sel ñ", [0x89, 0x50], undefined]);
  check("a cut set is refused", parseSet(message.subarray(0, message.length - 1)), null);
  check("bytes after the last part are refused", parseSet(new Uint8Array([...message, 0])), null);
  check("an unknown format is refused", parseSet(new Uint8Array([CLIPBOARD_SET_KIND, 0, 1, ...part(9, [1])])), null);
  check("text that is not UTF-8 is refused", parseSet(new Uint8Array([CLIPBOARD_SET_KIND, 0, 1, ...part(1, [0xc0])])), null);
  check("a source that does not exist is refused", parseSet(new Uint8Array([CLIPBOARD_SET_KIND, 5, 0])), null);
}

{
  const file = { type: "image/jpeg" };
  const data = {
    getData: (type) => ({ "text/plain": "t", "text/html": "<i>" }[type] ?? ""),
    items: [{ kind: "string", type: "text/plain" }, { kind: "file", type: "image/jpeg", getAsFile: () => file }],
    types: ["text/plain", "text/html", "Files"],
  };
  const read = readDataTransfer(data);
  check("a paste event gives text, HTML and the first image", [read.text, read.html, read.image === file, read.hasContent], ["t", "<i>", true, true]);
  check("a paste event with no types is empty", readDataTransfer({ getData: () => "", items: [], types: [] }).hasContent, false);
  check("a plain data object still works", readDataTransfer({ getData: (t) => (t === "text/plain" ? "x" : "") }).text, "x");
}

{
  const png = new Uint8Array([1, 2, 3]);
  const blob = { type: "image/png", arrayBuffer: async () => png.buffer };
  check("a PNG goes on as it is", [...(await toPngBytes(blob))], [1, 2, 3]);
}

{
  const typed = (type, value) => ({ type, text: async () => value, arrayBuffer: async () => new TextEncoder().encode(value).buffer });
  const clipboard = {
    read: async () => [{ types: ["text/plain", "text/html", "image/png"], getType: async (type) => typed(type, `<${type}>`) }],
  };
  const content = await readClipboardApi(clipboard);
  check("the Clipboard API gives the three formats", [content.text, content.html, content.image.type, content.hasContent], ["<text/plain>", "<text/html>", "image/png", true]);
  const old = await readClipboardApi({ readText: async () => "solo texto" });
  check("a browser without read() gives the text", [old.text, old.image, old.hasContent], ["solo texto", null, true]);
  check("an empty API clipboard", (await readClipboardApi({ read: async () => [] })).hasContent, false);
}

{
  const written = [];
  class Item { constructor(data) { this.types = Object.keys(data); } }
  const clipboard = { write: async (items) => written.push(items[0].types), writeText: async (t) => written.push(t) };
  await writeSet({ text: "a", html: "<b>a</b>", png: new Uint8Array([1]) }, { clipboard, ClipboardItem: Item });
  check("all the formats are written as one item", written, [["text/plain", "text/html", "image/png"]]);
  written.length = 0;
  const strict = { write: async () => { throw new TypeError("Type text/html not supported"); }, writeText: async (t) => written.push(t) };
  await writeSet({ text: "a", html: "<b>a</b>" }, { clipboard: strict, ClipboardItem: Item });
  check("a browser that refuses a type gets the text", written, ["a"]);
  const refuse = { write: async () => { throw new DOMException("no gesture", "NotAllowedError"); } };
  check("a refusal for lack of a user action is not hidden", await writeSet({ text: "a" }, { clipboard: refuse, ClipboardItem: Item }).catch((e) => e.name), "NotAllowedError");
  written.length = 0;
  await writeSet({ text: "b" }, { clipboard: { writeText: async (t) => written.push(t) }, ClipboardItem: undefined });
  check("no ClipboardItem: the text is written", written, ["b"]);
  check(
    "an image with no way to write it is an error",
    await writeSet({ png: new Uint8Array([1]) }, { clipboard: { writeText: async () => {} }, ClipboardItem: undefined }).catch((e) => e.message),
    "This browser cannot copy an image or HTML to the clipboard.",
  );
  check("a set with no content writes nothing", await writeSet({}, { clipboard, ClipboardItem: Item }), undefined);
  check(
    "a page without a clipboard (plain HTTP) says why",
    await writeSet({ text: "a" }, { clipboard: null, ClipboardItem: undefined }).catch((e) => e.message),
    NO_CLIPBOARD,
  );
}

{
  const nav = (state) => ({ permissions: { query: async () => ({ state }) }, clipboard: { readText: async () => "x" } });
  check("the access state comes from the permission", await clipboardAccess(nav("prompt")), "prompt");
  check("a browser that cannot say is unsupported", await clipboardAccess({ permissions: { query: async () => { throw new TypeError("no such permission"); } } }), "unsupported");
  check("no Permissions API is unsupported", await clipboardAccess({}), "unsupported");
  let reads = 0;
  const asking = { permissions: { query: async () => ({ state: reads > 0 ? "granted" : "prompt" }) }, clipboard: { readText: async () => { reads += 1; return ""; } } };
  check("asking reads the clipboard and gives the new state", [await requestClipboardAccess(asking), reads], ["granted", 1]);
  const refused = { permissions: { query: async () => ({ state: "denied" }) }, clipboard: { readText: async () => { throw new DOMException("no", "NotAllowedError"); } } };
  check("a refusal is not an error", await requestClipboardAccess(refused), "denied");
  check("a page on plain HTTP is insecure", await clipboardAccess(nav("prompt"), false), "insecure");
  check("each state has its own text", new Set(["granted", "prompt", "denied", "unsupported", "insecure"].map(accessText)).size, 5);
}
