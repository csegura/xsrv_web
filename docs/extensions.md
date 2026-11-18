# Extensions

`xsrv` answers `QueryExtension` for the extensions below. A client that asks for another one gets
`present = false` and uses the core protocol. The first query of each unknown name is logged once.

## Implemented

| Extension | Opcode | What works | What does not |
|---|---|---|---|
| BIG-REQUESTS | 133 | `BigReqEnable`. Requests up to 4 MiB (`MAX_BIG_REQUEST_BYTES`). | |
| XC-MISC | 134 | `GetVersion`, `GetXIDRange`, `GetXIDList`. | |
| SHAPE | 129 | Bounding and input shapes: rectangles, masks from bitmaps, combine, offset, extents, events. The compositor and the pointer hit test follow the shape. | The clip shape is stored and reported, and not used for drawing. |
| MIT-SHM | 130 | Version 1.2: `QueryVersion`, `Attach`, `AttachFd`, `Detach`, `PutImage` (ZPixmap of depth 8, 24 or 32, and one-bit images, which cairo sends for a mask) with the completion event. `AttachFd` takes a memfd or a file on tmpfs (`/dev/shm`) and reads it with `pread`, so a file that shrinks later gives `BadValue` and cannot crash the server. Errors as in Xorg: no descriptor or an empty file is `BadMatch`, any other file is `BadAccess`. | Shared pixmaps, `GetImage`, `CreatePixmap` and `CreateSegment` (`BadImplementation`, logged once). For `Attach` the server needs to see and read the System V segment: a client of another user or in another IPC namespace gets `BadAccess` and falls back to `PutImage`. `AttachFd` has no such check. |
| XFIXES | 138 | `QueryVersion` (5.0), `ChangeSaveSet`, `SelectSelectionInput` with `SelectionNotify` events, `SelectCursorInput`, `SetCursorName` (a theme name such as `xterm` or `hand2` chooses the page pointer), `HideCursor`, `ShowCursor`. | Regions, `GetCursorImage` and the other cursor-name requests. Cursor events are not sent. |
| RENDER | 139 | Version 0.1 (`x11/render/`): `QueryVersion`, `QueryPictFormats` (a1, a8, x8r8g8b8 for the root visual, a8r8g8b8), pictures with repeat (none, normal, pad, reflect), component alpha, clip rectangles and clip masks; `Composite` and `FillRectangles` with the operators Clear to Saturate (0 to 13); glyph sets and `CompositeGlyphs8/16/32`, with and without a mask format. Pixmaps of depth 8 and 32 exist for it. | Trapezoids and triangles (0.4), transforms and filters (0.6), gradients and solid fills (0.10), the Disjoint, Conjoint and PDF operators, alpha maps, cursors from pictures, `AddGlyphsFromPicture`. A client at 0.1 does not use them: cairo draws paths and gradients on the client and uploads them. The clip of a source or mask picture is not applied (only the destination clip). |
| XKEYBOARD | 135 | Version 1.0, read-only (`x11/ext_xkb.rs`, keymap data in `x11/xkb_map.rs`): `UseExtension`, `SelectEvents`, `GetState`, `GetControls`, `GetMap` (all parts, full and partial ranges), `GetCompatMap` (8 symbol interpretations), `GetIndicatorState`, `GetIndicatorMap`, `GetNamedIndicator`, `GetNames`, `GetGeometry` (always "not found"), `PerClientFlags`, `GetDeviceInfo` (the core keyboard, id 3). Events: `NewKeyboardNotify`, `NamesNotify` and `MapNotify` on a layout switch, `StateNotify` and `IndicatorStateNotify` when modifiers, locks or buttons change. The keymap is built from the core one, with one group per key and four-level types for AltGr layouts. The root property `_XKB_RULES_NAMES` says `evdev`, `pc105` and the layout. `LatchLockState` locks and unlocks Caps Lock and Num Lock (numlockx); the page sets them again when the browser keyboard changes them. `Bell` is silent, as the core Bell. `SetControls` is accepted and ignored (logged once): the browser repeats keys. Request checks are looser than in X.Org: requests parsed by x11rb may have extra bytes, an empty key range may start anywhere, and UseExtension is not required first. Replies are encoded by `x11rb-protocol`, so the extension is offered only to clients with the server's byte order. `XSRV_XKB=0` turns it off. | Changing the keymap from a client (`SetMap`, `SetCompatMap`, `SetNames`, `GetKbdByName`, so `setxkbmap us` does nothing useful), geometry, more than one group, latches and group locks (logged once), BellNotify, AccessX, `ListComponents`. These give `BadRequest`, logged once. |
| RANDR | 140 | Version 1.5 for one screen, one output, one CRTC and one mode. `GetScreenResources`, `GetOutputInfo`, `GetCrtcInfo`, `GetScreenSizeRange`, `SetScreenSize` (resizes the screen), `SelectInput`, screen and CRTC change events. | More than one output or mode, `CreateMode`, rotation, gamma, providers. |

## Left out on purpose

| Extension | Why |
|---|---|
| XInputExtension | Not offered. A stub of XI 1.5 was tried, so that Xlib would not print `extension "XInputExtension" missing` (`xeyes` does). It made Chromium, and so Electron and VS Code, stop at start: they take the extension to mean XI 2, and XIQueryDevice failed. Without the extension, GTK 3, Qt and Chromium use the core pointer and keyboard events. A real XI 2 needs the XI 2 input events, because GTK 3 then stops using the core events. The `xeyes` warning is only text. |
| GLX | There is no GPU. Clients fall back to software or to EGL. |
| Composite, DAMAGE, SYNC, Generic Event Extension | Not asked for by a program that matters here. |

## XKEYBOARD and its clients

Without XKEYBOARD, libxkbcommon clients compile a default keymap of their own instead of reading the
server's. i3 did that, and in that keymap XF86AudioMicMute is also at keycode 256, which wraps to 0
(AnyKey) in a GrabKey: i3 grabbed every key and typing reached no window. xdotool 3.20160805
crashed, because it does not check the result of `XkbGetMap`. With the extension both read the
server keymap.

Checked with XKEYBOARD on (Debian 13 VM unless noted):

- `xkbcomp :N out.xkb` (Xlib) reads and writes the whole keymap, locally and on the VM; for the
  Spanish layout it shows four-level keys (`2 " @`, `e E €`) and RALT as `ISO_Level3_Shift` on Mod5.
- i3 4.24 with the stock Debian config (with `bindsym XF86AudioMicMute`): it loads the keymap from
  the server (`xkb_base 70`), no grab of keycode 0, Alt+Enter opens a terminal, typing reaches
  urxvt, and a layout switch from the page makes i3 translate its bindings again.
- GTK 3 entry under i3 with the Spanish layout: x, Shift+x, AltGr+2 and the `ñ` key type `xX@ñ`.
- Chromium 153 locally, Spanish layout: the same keys type `xX@ñ` in an input field.
- xev locally (Xlib with XKB): the same keys, with `ISO_Level3_Shift` for AltGr.
- xdotool: `search` and `key --window` work (it warns that XTEST is missing, which is true).
- `setxkbmap -query`: rules `evdev`, model `pc105`, the layout of the session.
- VS Code and Qt were not checked.

## What the target clients ask for

Seen in the server log when each client starts. "Answer" is what the server says. These were seen
before XKEYBOARD existed; with it, XKEYBOARD is present for clients with the server's byte order.

| Client | Extensions it queries | Answer |
|---|---|---|
| xdpyinfo | BIG-REQUESTS, XKEYBOARD | present, absent |
| xterm | BIG-REQUESTS, XFree86-Bigfont, XKEYBOARD | present, absent, absent |
| xmessage | BIG-REQUESTS, SHAPE, XKEYBOARD | present, present, absent |
| GTK 3 (via `ctypes`) | BIG-REQUESTS, MIT-SHM, RANDR, XFIXES, SHAPE, Composite, DAMAGE, GLX, Generic Event Extension, SYNC, XInputExtension, XKEYBOARD | present for the first five, absent for the rest |

## RENDER and its clients

RENDER is at version 0.1 because that is the lowest version at which cairo-xcb uses it for fills,
surface copies and text (checked in the cairo 1.18.4 source). Without RENDER, cairo-xcb draws in a
local copy of each pixmap and can lose drawing: i3bar showed no i3status text. At 0.1 cairo draws
paths and gradients on the client and uploads them as images, which is correct but slower. cairo
needs pixmaps of depth 1, 8, 24 and 32, and turns RENDER off when one of them is missing.

Checked with RENDER on:

- i3 4.24 with i3bar and a Pango font (`font pango:DejaVu Sans Mono, FontAwesome 11`) on Debian
  13: the status text shows, also when the screen is narrower than the status line.
- xterm with an Xft font (`xterm -fa "DejaVu Sans Mono"`), which needs RENDER: antialiased text.
- GTK 3 (a window with a label and a button, through `ctypes`, with `GDK_BACKEND=x11`).
- Chromium 153 over the Unix socket: it draws, and clicks and keys arrive.

## Chromium and Electron (VS Code)

Checked with Chromium 153 (Chrome for Testing) as an X client, with the sandbox on: the window,
the tabs and the page draw (MIT-SHM `ShmPutImage`), a click and typed text reach the page.

- With the old XI 1.5 stub, Chromium stopped at start (SIGTRAP) right after XIQueryDevice failed.
  Electron, and so VS Code, has the same X11 code.
- Connect these clients through the Unix socket (`DISPLAY=:1` with `/tmp/.X11-unix/X1`). Through
  TCP (`DISPLAY=127.0.0.1:N`, for example the `socat` bridge in the Development section of `README.md`), the sandboxed GPU
  process cannot draw, and the window stays empty. `--no-sandbox` or `--in-process-gpu` avoids it.
- VS Code itself was not run in this check.
