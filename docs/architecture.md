# Architecture

xsrv-web is one process with three parts: an X11 server, an HTTP and WebSocket server, and a
page (HTML, JavaScript and a WASM module) that draws the screen in a canvas.

```
X clients --(X11 protocol, Unix socket)--> X11 server --(Display: framebuffer, messages)-->
    page sessions --(WebSocket: tiles, copies, cursor, clipboard)--> page (WASM decoder, canvas)
page --(WebSocket: keys, mouse, touch, resize, clipboard)--> X11 server --(events)--> X clients
```

## Process and threads

- One thread. `main` runs a current-thread Tokio runtime with a `LocalSet`: the HTTP server, each
  page session, each X11 session and the video task are tasks on it. The X11 state is in an
  `Rc<RefCell<State>>`, and no request handler runs at the same time as another.
- The only other process is `ffmpeg`, when `XSRV_VIDEO=h264` is set.
- The tasks talk through `Display` (`crates/xsrv-server/src/display.rs`): a broadcast channel of
  messages to the pages, and an ordered channel of page events (input and clipboard) to the X11
  server.

## Crates

| Crate | What it holds |
|---|---|
| `crates/wire` | The messages between server and page: screen size, tiles (raw, LZ4, zstd and palette tiles), copy, cursor, clipboard, video, and the input messages. `no_std`, shared by both sides. |
| `crates/xsrv-server` | The server: the X11 protocol (`src/x11/`), `Display`, the framebuffer, HTTP, WebSocket sessions, authentication, limits and video. The binary is `xsrv-web`. |
| `crates/xsrv-wasm` | The page side: decodes the messages, decompresses LZ4 and zstd tiles (`lz4_flex`, `ruzstd`), and draws on the canvas. |

The page scripts in `web/`: `main.js` (WebSocket, frame batching), `input.js` (keyboard, mouse,
touch, clipboard), `keymap.js`, `video.js` (H.264 with WebCodecs).

## The X11 server (`crates/xsrv-server/src/x11/`)

### Protocol

- `connection.rs` accepts a client, checks the cookie in the connection setup (`setup.rs`), and
  splits the stream into requests (BIG-REQUESTS included).
- `dispatch.rs` routes each request by opcode; `codec.rs` reads and writes both byte orders.
  Errors and replies go back on the same connection. An extension request (opcode 128 and up) goes
  to `extensions.rs`, which routes it to `ext_shape.rs`, `ext_shm.rs`, `ext_xfixes.rs`,
  `ext_randr.rs` or `render/`.
- `harness.rs` feeds raw bytes to the handlers without a socket, for the fuzzer.

### State and resources

- `state.rs`: windows, pixmaps, GCs, fonts, cursors, properties, selections, the clients and what
  each one owns. `remove_client` frees everything a client owned.
- `resources.rs`: `Surface`, a raster of 4-byte pixel values. Every window and pixmap has one; a
  window keeps its pixels (backing store), so an exposed window is drawn from its surface and not
  by the client. The depth says which bits count: 24 (BGRX), 32 (premultiplied BGRA), 8 and 1.
- Limits: resources and pixel bytes for each client and for the server, pixels per request, passive
  grabs, clip rectangles and glyph bytes. A request over a limit gets `BadAlloc`.

### Drawing

- `raster.rs` (lines, arcs, polygons, tiles, stipples), `text_requests.rs` with the built-in font
  (`font.rs`), `drawing_requests.rs` (PutImage, GetImage, CopyArea, CopyPlane), and `zpixmap.rs`
  for image formats of depth 8, 24 and 32.
- `render/`: RENDER 0.1. `format.rs` (a1, a8, x8r8g8b8, a8r8g8b8), `composite.rs` (the Porter-Duff
  operators on premultiplied pixels, pure functions), `picture.rs` (pictures, clips, and pixmaps
  that pictures keep alive), `draw.rs` (Composite, FillRectangles, a work budget for each request),
  `glyphs.rs` (glyph sets, CompositeGlyphs).

### Screen composition (`compose.rs`)

1. Drawing on a viewable window marks a dirty area in screen coordinates.
2. At a request with a reply, or when a burst of requests settles (6 ms, at most 16 ms), the server
   composes each dirty area from the window tree (backgrounds, borders, frames of the built-in
   manager, bounding shapes) and hands the pixels to `Display::update`.
3. `Display::update` compares them with the framebuffer and sends only the bands of changed rows
   as tiles; tiles of 1 KiB or more are compressed (LZ4 by default, zstd with `XSRV_COMPRESSION=zstd`) once for all pages.
4. A scroll (CopyArea inside a fully visible window) and a window move are copy messages: the
   framebuffer moves its pixels, and the message waits for the tiles of the same burst, so that the
   page never shows half of a change.

### Input, focus and the window manager

- `input.rs` turns page events into X events: pointer motion and crossing events, buttons, keys
  through `keymap.rs` (us, es, gb, de), focus, grabs (active and passive, `AllowEvents
  ReplayPointer`). Touch is handled in the page: `input.js` turns taps and two-finger scrolls
  into mouse input.
- `wm.rs`, the built-in manager: frames with a title and a close button, moves and resizes by
  dragging, maximize (each top-level window fills the screen and follows its size), and focus. It
  steps aside when another client selects SubstructureRedirect on the root (i3).
- `clipboard.rs`: the page text is the CLIPBOARD owned by a hidden window; text from an X owner is
  pulled with UTF8_STRING and sent to the pages.

## The web side

- `main.rs` serves the page folder (`XSRV_WEB_DIR`) and `/ws`. `auth.rs` checks the token (a query
  parameter once, then a cookie) and refuses a network address without one; `limits.rs` caps the
  open pages and the input rate.
- `session.rs`, one task for each page: it subscribes to the messages of its display and takes a
  snapshot under the same lock, so that no message is applied twice; it forwards input and
  clipboard text. A page that falls behind the channel (64 messages) gets a new snapshot.
- After a screen resize the page keeps its old picture; the framebuffer is marked as not shown
  until the whole screen has been sent again (`Framebuffer::new_unsent`).
- `video.rs` (optional): `ffmpeg` encodes the framebuffer as H.264 when it changes, and the page
  decodes it with WebCodecs.

## Sessions

`XSRV_SESSIONS` runs up to 8 independent displays in the process. Each has its own X11 socket,
`State`, `Display` and framebuffer; the page chooses one with `?session=N`
([sessions.md](sessions.md)).

## Tests

| What | Where |
|---|---|
| Unit tests | next to the code (`#[cfg(test)]`), for example the composite operators and the framebuffer diff |
| Protocol tests | `crates/xsrv-server/tests/x11_core.rs`: a real server on a Unix socket, driven by x11rb; a page model checks that the page equals the framebuffer |
| Random requests | `tests/fuzz_requests.rs` (seeded) and the cargo-fuzz target `fuzz/fuzz_targets/requests.rs` |
| Page scripts | `make test-web` (node, fake canvas, a real H.264 stream) |
| Benchmark | `make bench` (`benches/updates.rs`) |
| Real clients | xterm, GTK 3, Chromium, i3 and i3bar, checked in headless Chromium against the server screen; see the docs of each feature |

## Main decisions

| Decision | Why |
|---|---|
| Backing store for every window | The server can compose any part of the screen at any time without asking the clients to draw it again. |
| One thread | The X11 protocol is serial for each client, and shared state needs no locks; the work per request is bounded instead. |
| Diff against the framebuffer | Clients draw the same pixels again often (Expose, cursor blink); only changes are sent. |
| Copy messages held until the end of a burst | A scroll or a move and the new pixels arrive in the same frame. |
| RENDER at version 0.1 | The lowest version at which cairo uses RENDER for fills, copies and text; without RENDER, cairo-xcb lost drawing (i3bar). |
| No GLX or XInputExtension | A partial extension makes clients take paths that then fail ([extensions.md](extensions.md)). |
| XKEYBOARD read-only, built from the core keymap | libxkbcommon clients (i3, Chromium) must read the server keymap, not compile their own; one source of truth keeps the core and XKB views the same. Replies come from `x11rb-protocol` instead of hand-written encoders. |
