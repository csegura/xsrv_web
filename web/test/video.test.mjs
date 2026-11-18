// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

import { readFileSync } from "node:fs";
import { createVideo, nalTypes, VIDEO_KIND } from "../video.js";

// A real x264 stream of 60 frames, a key frame every 30 (made with the arguments of video.rs).
const stream = readFileSync(new URL("./data/test.h264", import.meta.url));
const check = (label, actual, expected) => {
  const ok = JSON.stringify(actual) === JSON.stringify(expected);
  console.log(ok ? "ok  " : "FAIL", label, ok ? "" : JSON.stringify(actual) + " != " + JSON.stringify(expected));
  if (!ok) process.exitCode = 1;
};

const decoded = [];
const configured = [];
class FakeDecoder {
  constructor({ output }) { this.output = output; }
  configure(config) { configured.push(config); }
  decode(chunk) { decoded.push(chunk); }
}
globalThis.EncodedVideoChunk = class { constructor(init) { Object.assign(this, init); } };
const canvas = { getContext: () => ({ drawImage() {} }) };

check("the message kind of video", VIDEO_KIND, 5);
const types = nalTypes(stream);
check("the stream has headers, key slices and delta slices", [types.includes(7), types.includes(5), types.includes(1), types.includes(9)], [true, true, true, true]);

const video = createVideo(canvas, FakeDecoder);
// The server sends pieces of any size. Feed odd ones, as a network would.
for (let at = 0, size = 97; at < stream.length; at += size, size = (size * 7) % 500 + 31) {
  video.push(stream.subarray(at, Math.min(stream.length, at + size)));
}
check("59 of 60 frames are decoded (the last waits for the next delimiter)", decoded.length, 59);
check("the key frames are 0 and 30", decoded.map((c, i) => (c.type === "key" ? i : -1)).filter((i) => i >= 0), [0, 30]);
check("every chunk starts with a start code", decoded.every((c) => c.data[0] === 0 && c.data[1] === 0), true);
check("the decoder is Annex B baseline", [configured[0].avc.format, configured[0].codec], ["annexb", "avc1.42E01F"]);
check("timestamps grow", decoded.every((c, i) => i === 0 || c.timestamp > decoded[i - 1].timestamp), true);

// A page that joins in the middle waits for a key frame.
decoded.length = 0;
const late = createVideo(canvas, FakeDecoder);
const secondKey = stream.indexOf(Buffer.from([0, 0, 0, 1, 0x09]), 1000);
late.push(stream.subarray(secondKey > 0 ? Math.floor(stream.length / 2) : 0));
check("a late page decodes only from the next key frame", decoded.length === 0 || decoded[0].type === "key", true);

// No WebCodecs: nothing is decoded, and nothing throws.
const without = createVideo(canvas, undefined);
without.push(stream.subarray(0, 100));
check("a browser without WebCodecs does not throw", true, true);
