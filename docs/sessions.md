# Several sessions

One server can run up to 8 independent screens. Set `XSRV_SESSIONS=N`. The X11 socket name must end
in `X<number>` (the default `/tmp/.X11-unix/X1` does). Session 1 uses that socket, and the next
sessions use `X2`, `X3`, and so on, in the same directory.

- Each session has its own screen, windows, clipboard, keyboard state and window manager.
- A page opens a session with `?session=N` (default 1). An unknown number gets HTTP 404.
- A client uses `DISPLAY=:N`.
- The token, the page limit and the host checks are shared by all sessions.

Tested: two sessions answer on their own sockets, a resize in session 2 does not reach a page of
session 1, and session 3 is refused. Not tested: many clients on several sessions at once under load.

## Virtual monitors

`XSRV_MONITORS=N` (1 to 4) makes RANDR report N monitors in one screen. The screen is cut into
N columns of equal width (the last one takes the rest), and each column is one output with one
CRTC and one mode. The columns follow the screen when the browser window changes size. Clients
that place windows by monitor (for example with `xrandr --listmonitors` style tools, or Qt) see
N monitors.

- Tested: integration test with x11rb, and the real `xrandr` tool, which lists `xsrv-0` at
  `320x480+0+0` and `xsrv-1` at `320x480+320+0` on a 640x480 screen.
- The monitors are fixed: SetCrtcConfig accepts only the current layout, and gamma, transforms
  and panning are stubs (identity or off). RANDR 1.5 `GetMonitors` is not offered, because the
  server reports RANDR 1.3.
- The built-in window manager still maximizes a window to the whole screen, not to one monitor.
- The page shows one canvas. The monitors are not separate windows.
