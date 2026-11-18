# Install and deploy xsrv-web

Three ways: the Debian package (a system service), the tarball (any folder, any user), or the
Docker image. All of them serve the page on one HTTP port and give X clients one Unix socket.

## Requirements

- Linux x86_64. The packages contain a binary built on Ubuntu 24.04, so they need glibc 2.39 or
  newer: Debian 13 and Ubuntu 24.04 work, Debian 12 does not. On older systems, build from source
  (`make release`) or use the Docker image, which builds inside the container.
- A browser with WebAssembly. Clipboard access and H.264 video need a secure origin (HTTPS, or
  `127.0.0.1` through an SSH tunnel).
- For `XSRV_VIDEO=h264`: `ffmpeg` with libx264.

## Build the packages

```
make dist   # dist/xsrv-web-<version>-linux-x86_64.tar.gz
make deb    # dist/xsrv-web_<version>_amd64.deb (needs dpkg-deb)
```

`MAINTAINER="Name <mail>" make deb` sets the Maintainer field of the package.

## Debian package (recommended)

Three commands on the machine that runs the X programs:

```
sudo apt install ./xsrv-web_0.1.0_amd64.deb
sudo systemctl enable --now xsrv-web
sudo xsrv-web token
```

The last command prints the page address with its token, for example
`http://127.0.0.1:8080/?token=hpzAEewFD44D7_HG4Badaiz4PCsdlmoR`. Open it in a browser on the same
machine, or through an SSH tunnel from another one (see "Open the page" below). The page sets a
cookie, so later visits need only `http://127.0.0.1:8080/`. Then start X programs on the screen:

```
DISPLAY=:1 xterm &
```

What the package does:

- Files: `/usr/bin/xsrv-web`, the page in `/usr/share/xsrv-web/web`, the settings in
  `/etc/default/xsrv-web` (kept on remove, deleted on purge), the unit
  `/usr/lib/systemd/system/xsrv-web.service`.
- The token: the install makes a random one in `/var/lib/xsrv-web/token` (root only), so the page
  is protected from other users of the machine from the start. `sudo xsrv-web token` prints it. To
  choose your own, set `XSRV_TOKEN=` in `/etc/default/xsrv-web`; that value wins. An upgrade keeps
  the token; a purge deletes it.
- The service runs as the system user `xsrv-web`, with no capabilities and a read-only system
  (`ProtectSystem=strict`); it may write only to `/tmp/.X11-unix`.
- The install does not start the service, because the X socket `:1` may be taken on a desktop
  (below). Remove stops and disables it; purge deletes the settings and the token. The system
  user `xsrv-web` stays, as Debian does for system users.
- The settings file is readable by root only, because it may hold the token and the X cookie.
  After a change: `sudo systemctl restart xsrv-web`. All settings are in
  `/usr/share/doc/xsrv-web/README.txt`.

Before you enable it:

- On a desktop, `:1` may already belong to a login screen or a second session. Then choose
  another number in `/etc/default/xsrv-web`, for example `XSRV_X11_SOCKET=/tmp/.X11-unix/X10`,
  and use `DISPLAY=:10`. The socket must stay in `/tmp/.X11-unix`, the only folder the service
  may write (systemd creates it at boot).
- The X socket accepts every local user, as an Xorg socket does. When other users share the
  machine, also set `XSRV_X11_COOKIE` (32 hex digits) and give it to your X programs with
  `xauth add :1 MIT-MAGIC-COOKIE-1 <cookie>`. The token protects the page; the cookie protects the
  X socket.
- The page listens on every interface (`0.0.0.0`) on port `XSRV_PORT` (8080) because the package
  sets a token. Without TLS the token goes over the network in clear text: outside a trusted
  network, set `XSRV_ADDR=127.0.0.1:8080` and use an SSH tunnel, or a reverse proxy with TLS and
  `XSRV_SECURE_COOKIE=1`. See `docs/security.md`.
- With a window manager such as i3: set `XSRV_WM=0` and start the manager on `DISPLAY=:1`
  (`docs/external-wm.md`).

Other notes:

- MIT-SHM for X clients of other users: a client that passes its memory as a file descriptor
  (`ShmAttachFd`, MIT-SHM 1.2) can use it. A client that uses a System V segment (cairo, and so
  GTK) cannot, because the service runs as another user and cannot read the segment; it then
  sends its pixels without MIT-SHM, which is slower.
- As with any X server, a local user who creates the socket file first can block the start; the
  server does not create a lock file. The server removes its socket when it stops.
- Logs: `journalctl -u xsrv-web`.

Checked on a fresh Debian 13 VM (Nov 2026): install, enable, `sudo xsrv-web token` (a normal user
gets "run: sudo xsrv-web token"), the page refuses a request without the token (401) and accepts
the printed address (200), a token in `/etc/default/xsrv-web` wins, a reinstall keeps the
generated token, and a purge removes the settings and the token. Earlier: `xdpyinfo` and `xterm`
as a normal user, and a client without the X cookie is refused.

## Tarball

```
tar xzf xsrv-web-0.1.0-linux-x86_64.tar.gz
cd xsrv-web-0.1.0-linux-x86_64
./start.sh
```

`start.sh` sets `XSRV_WEB_DIR` to the page next to it and passes the other `XSRV_*` variables
(`README.txt`) to the server. To start it with a session, write your own script, as the VM in
`docs/external-wm.md` does:

```
XSRV_WM=0 ./start.sh > server.log 2>&1 &
sleep 1
DISPLAY=:1 i3 &
```

## Docker

`make docker-build` builds the image `xsrv-web`; `docker/compose.yaml` starts it with an xterm in a
second container (`docs/docker.md`).

## Open the page

- Local: `http://127.0.0.1:8080/`. Click the page to give it the keyboard.
- Remote: `http://<host>:8080/?token=<token>`, or set `XSRV_ADDR=127.0.0.1:8080` and use an SSH
  tunnel (`ssh -L 8080:127.0.0.1:8080 host`) or a reverse proxy with TLS (set
  `XSRV_SECURE_COOKIE=1`). On a network address the server needs `XSRV_TOKEN`; open the page once with `?token=<token>`, which sets a cookie. See
  `docs/security.md`.

## Update

- Debian package: `sudo apt install ./xsrv-web_<new>_amd64.deb` (a running service restarts, so
  the X programs on it close). If you changed `/etc/default/xsrv-web` and the new package ships a
  changed one, apt asks which to keep: keep yours (the default, `N`); the new one is saved as
  `/etc/default/xsrv-web.dpkg-dist`. For a scripted update: `sudo apt install -o
  Dpkg::Options::=--force-confold ./xsrv-web_<new>_amd64.deb`.
- Tarball: unpack the new version next to the old one, stop the old server, start the new one. The
  X clients must reconnect: their windows close when the server stops.
