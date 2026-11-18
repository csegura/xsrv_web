// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

import { canvasToBlob, copyScreenshot, saveScreenshot, screenshotName } from "../screenshot.js";

const check = (label, actual, expected) => {
  const ok = JSON.stringify(actual) === JSON.stringify(expected);
  console.log(ok ? "ok  " : "FAIL", label, ok ? "" : JSON.stringify(actual) + " != " + JSON.stringify(expected));
  if (!ok) process.exitCode = 1;
};

check("the name has the date and the time", screenshotName(new Date(2026, 10, 12, 9, 5, 7)), "xsrv-web-20261112-090507.png");
check("single digits are padded", screenshotName(new Date(2026, 0, 2, 3, 4, 5)), "xsrv-web-20260102-030405.png");

const blob = { size: 3 };
const canvasWith = (result) => ({ toBlob: (callback, type) => callback(type === "image/png" ? result : null) });

check("the canvas gives a PNG blob", await canvasToBlob(canvasWith(blob)), blob);
check(
  "a canvas that gives nothing is an error",
  await canvasToBlob(canvasWith(null)).catch((error) => error.message),
  "The browser could not make an image of the screen.",
);

{
  const events = [];
  const link = { click: () => events.push(["click", link.href, link.download]), remove: () => events.push(["remove"]) };
  const document = { createElement: (tag) => (events.push(["create", tag]), link), body: { append: () => events.push(["append"]) } };
  const URL = { createObjectURL: (b) => (events.push(["url", b]), "blob:x"), revokeObjectURL: () => {} };
  const name = await saveScreenshot(canvasWith(blob), { document, URL, now: new Date(2026, 10, 12, 1, 2, 3) });
  check("the file is saved through a link, then the link goes", events, [
    ["url", blob], ["create", "a"], ["append"], ["click", "blob:x", "xsrv-web-20261112-010203.png"], ["remove"],
  ]);
  check("the name is returned", name, "xsrv-web-20261112-010203.png");
}

{
  const written = [];
  class Item { constructor(data) { this.data = data; } }
  const navigator = { clipboard: { write: async (items) => written.push(items) } };
  await copyScreenshot(canvasWith(blob), { navigator, ClipboardItem: Item });
  check("the image goes to the clipboard as image/png", [written.length, written[0][0].data], [1, { "image/png": blob }]);
  check(
    "no image clipboard: a clear error",
    await copyScreenshot(canvasWith(blob), { navigator: { clipboard: {} }, ClipboardItem: Item }).catch((e) => e.message),
    "This browser cannot copy an image to the clipboard. Use the file instead.",
  );
}
