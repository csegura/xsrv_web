// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

// Measures how long the WASM module of the page needs to apply screen messages. The canvas is a
// fake, so the time is decode and pixel conversion, without the drawing of the browser.
// The messages are files that the server example `tiles` writes: what the server really sends.
// Run with `make bench-web`, or `node web/test/bench.mjs <directory of .tile files>`.
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { Display, initSync } from "../pkg/xsrv_wasm.js";

const MIN_ROUNDS = 20;
const MIN_TIME_MS = 200;

let drawnBytes = 0;
globalThis.ImageData = class {
  constructor(data, width, height) {
    drawnBytes += data.length;
    this.width = width;
    this.height = height;
  }
};
globalThis.CanvasRenderingContext2D = class {
  putImageData() {}
  drawImage() {}
  fillRect() {}
};
const canvas = { getContext: () => new globalThis.CanvasRenderingContext2D(), style: { setProperty() {} } };

const directory = process.argv[2];
if (!directory) {
  console.error("usage: node web/test/bench.mjs <directory of .tile files>");
  process.exit(2);
}
initSync({ module: readFileSync(new URL("../pkg/xsrv_wasm_bg.wasm", import.meta.url)) });
const display = new Display(canvas);

// "text-800x600.lz4.tile" sorts by size, then content, then codec.
const pixelCount = (name) => name.match(/(\d+)x(\d+)/).slice(1).reduce((a, b) => a * b, 1);
const files = readdirSync(directory)
  .filter((name) => name.endsWith(".tile"))
  .sort((a, b) => pixelCount(a) - pixelCount(b) || a.localeCompare(b));

console.log("tile".padEnd(28), "bytes".padStart(9), "mean ms".padStart(9), "MB/s out".padStart(9));
for (const name of files) {
  const bytes = new Uint8Array(readFileSync(join(directory, name)));
  for (let i = 0; i < 3; i++) display.apply(bytes);
  drawnBytes = 0;
  let rounds = 0;
  const start = performance.now();
  while (rounds < MIN_ROUNDS || performance.now() - start < MIN_TIME_MS) {
    display.apply(bytes);
    rounds++;
  }
  const ms = (performance.now() - start) / rounds;
  const rate = drawnBytes / rounds / 1e6 / (ms / 1000);
  const label = name.replace(/\.tile$/, "");
  console.log(label.padEnd(28), String(bytes.length).padStart(9), ms.toFixed(3).padStart(9), rate.toFixed(0).padStart(9));
}
