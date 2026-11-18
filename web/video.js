// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

// The screen as an H.264 stream (the server runs with XSRV_VIDEO=h264). The stream is Annex B.
// A browser without H.264 in WebCodecs cannot show it: the page logs that once.
export const VIDEO_KIND = 5;

const CODEC = "avc1.42E01F"; // Baseline, level 3.1. The server encodes Baseline.
const NAL_IDR = 5;
const NAL_SPS = 7;

// The types of the NAL units of an Annex B chunk (start codes 00 00 01 or 00 00 00 01).
export function nalTypes(bytes) {
  const types = [];
  for (let i = 0; i + 3 < bytes.length; i++) {
    if (bytes[i] === 0 && bytes[i + 1] === 0 && bytes[i + 2] === 1) {
      types.push(bytes[i + 3] & 0x1f);
      i += 2;
    }
  }
  return types;
}

// The pieces of the stream do not end on frame borders. The server asks x264 for an access unit
// delimiter (NAL type 9) at the start of each frame, so a frame is the bytes from one delimiter
// to the next. The last, unfinished frame waits for the next piece.
export function createVideo(canvas, VideoDecoderClass = globalThis.VideoDecoder) {
  const context = canvas.getContext("2d");
  let decoder = null;
  let waitingForKey = true;
  let warned = false;
  let timestamp = 0;
  let pending = new Uint8Array(0);

  const warnOnce = (message, error) => {
    if (warned) return;
    warned = true;
    console.warn(message, error ?? "");
  };

  const start = () => {
    decoder = new VideoDecoderClass({
      output: (frame) => {
        context.drawImage(frame, 0, 0);
        frame.close();
      },
      error: (error) => {
        warnOnce("xsrv: the video decoder stopped", error);
        decoder = null;
        waitingForKey = true;
      },
    });
    decoder.configure({ codec: CODEC, avc: { format: "annexb" }, optimizeForLatency: true });
    waitingForKey = true;
  };

  const decodeUnit = (unit) => {
    const types = nalTypes(unit);
    const isKey = types.includes(NAL_IDR) || types.includes(NAL_SPS);
    if (waitingForKey && !isKey) return;
    if (!decoder) start();
    waitingForKey = false;
    decoder.decode(new globalThis.EncodedVideoChunk({
      type: isKey ? "key" : "delta",
      timestamp: (timestamp += 33333),
      data: unit,
    }));
  };

  const NAL_AUD = 9;
  const splitFrames = (bytes) => {
    const begins = [];
    for (let i = 0; i + 3 < bytes.length; i++) {
      if (bytes[i] === 0 && bytes[i + 1] === 0 && bytes[i + 2] === 1) {
        if ((bytes[i + 3] & 0x1f) === NAL_AUD) begins.push(i > 0 && bytes[i - 1] === 0 ? i - 1 : i);
        i += 2;
      }
    }
    if (begins.length === 0) return { units: [], rest: bytes };
    const units = [];
    for (let k = 0; k + 1 < begins.length; k++) units.push(bytes.subarray(begins[k], begins[k + 1]));
    return { units, rest: bytes.subarray(begins[begins.length - 1]) };
  };

  return {
    push(chunk) {
      if (!VideoDecoderClass || !globalThis.EncodedVideoChunk) {
        warnOnce("xsrv: this browser cannot decode H.264 with WebCodecs");
        return;
      }
      const joined = new Uint8Array(pending.length + chunk.length);
      joined.set(pending);
      joined.set(chunk, pending.length);
      const { units, rest } = splitFrames(joined);
      pending = rest.slice();
      for (const unit of units) decodeUnit(unit);
    },
  };
}
