# An external window manager (i3)

`xsrv-web` can run without its own window manager. Start it with `XSRV_WM=0`, then start any X
window manager on the socket. The manager selects SubstructureRedirect on the root window and gets
MapRequest and ConfigureRequest, as on a normal X server.

## i3 on a Debian VM (tested)

Setup that was tested on Debian 13 (trixie), 1 CPU, 512 MB, with `i3` 4.24, `xterm`, `x11-utils`:

```
XSRV_WM=0 XSRV_TOKEN=<token> XSRV_ADDR=0.0.0.0:8080 XSRV_X11_SOCKET=/tmp/.X11-unix/X1 xsrv-web &
DISPLAY=:1 i3 &
DISPLAY=:1 xterm &
```

The release binary was built on Ubuntu 24.04 (glibc 2.39) and runs on Debian 13 (glibc 2.41).
The page was opened through `ssh -L 18088:localhost:8080` in headless Chromium.

What worked:

- i3 starts, finds the output `xsrv-0` through RANDR, and manages the windows: `xwininfo -root -tree`
  shows the xterm inside an `i3-frame`.
- Typing in the page reaches the xterm. `Alt+Enter` (the i3 binding for a new terminal) opens a
  second xterm, and i3 tiles the two side by side with title bars.
- Changing the size of the browser window re-tiles the windows to the new size (i3 reacts to the
  RANDR screen change).

The same setup was repeated on a second VM (2 CPUs, 1.5 GB) and driven with **Firefox 153 ESR**
(headless, through Marionette) and with Chromium:

- Firefox loaded the page (`isSecureContext` true, `navigator.clipboard` present), the WASM module
  and the tiles drew the i3 screen, text typed in the page reached the xterm, and a resize of the
  window changed the canvas from 900x614 to 1100x674 with the xterm re-tiled.
- Ctrl+V with the browser clipboard unreadable (Firefox asks for permission) did not stop the page:
  it waited at most 1.5 s and sent the key.
- A real paste and a copy from X to the browser clipboard also worked in Firefox when the test
  preferences were set (see `docs/clipboard.md`). The normal permission prompt was not exercised.

Click-to-focus: i3 grabs the buttons of client windows with a synchronous passive grab and then
calls AllowEvents ReplayPointer. The server supports that replay (test
`replay_pointer_gives_the_click_to_the_window_below_the_grab`), so a click is seen by i3 and also
reaches the application. This was written after the VM test above, and it was **not** checked with
a real i3 clicking in the page: the VM was in use with its own i3 configuration (Mod4
instead of Alt, with a bar), so a scripted check there would have disturbed it.

## i3bar and i3status (checked on Debian 13, i3 4.24, cairo 1.18.4)

- With a Pango font in the `bar` block (`font pango:...`), the bar shows the workspace buttons but
  no i3status text. The cause is in cairo on the client: without RENDER, cairo-xcb draws in a
  local copy of the pixmap ("fallback" image). libi3 4.24 draws text without a flush after it, so
  the bar buffer keeps a fallback, and cairo then draws the copy of the status line into that
  image without adding it to the damaged area (`get_compositor` in `cairo-xcb-surface.c`). The
  copy is never sent. A replica of the libi3 calls in Python shows the same, and one flush after
  the text makes the copy appear.
- Workaround: an X core font in the `bar` block, for example `font fixed`. libi3 then draws text
  with core X requests, and the status text shows (checked).
- Fixed: the server now has RENDER 0.1 (see `extensions.md`), so cairo does not take this path.
  Checked on the same VM with the Pango font: the status text shows. `font fixed` is no longer
  needed.
- Key bindings: i3 grabs each binding about four times (with and without NumLock and CapsLock).
  With a config of about 60 bindings that is 240 passive grabs. The limit was 64 for each client,
  so most bindings were not grabbed; it is now 2048 for each client.

What did not work or was not checked:

- Fixed: typing reached no window when the i3 config had `bindsym XF86AudioMicMute` (the Debian
  default config has it) and the server had no XKEYBOARD. i3 then translated `bindsym` names with
  its own default keymap, where XF86AudioMicMute is also at keycode 256. An X keycode is one byte,
  so i3 grabbed keycode 0, which is AnyKey: every key press went to i3, which dropped it. Menus
  such as dmenu still worked, because they grab the whole keyboard. The i3 log showed
  `XF86AudioMicMute ... keycodes 198 ... 0` and `Grabbing keycode 0 with mods 0`. Now i3 reads
  the server keymap through XKEYBOARD, which has no XF86AudioMicMute, so nothing is grabbed for it
  (checked with the stock config). With `XSRV_XKB=0` the old workaround still applies: write the
  binding as `bindcode 198 ...`.
- The built-in manager (`XSRV_WM=1`) must be off: two managers cannot both select
  SubstructureRedirect. The built-in one steps aside by itself when another client selects it,
  but `XSRV_WM=0` is the clean way.
- `XSRV_WM_MAXIMIZE` has no effect with an external manager.
- Other managers (openbox, xfwm4, a stacking manager that draws with RENDER) were not tried.
- No test with a long session, many windows, or the keyboard shortcuts of i3 other than `Alt+Enter`.
