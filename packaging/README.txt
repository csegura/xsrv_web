xsrv-web
========

An X11 server whose screen is a web page. X clients (xterm, GTK and Chromium-based programs,
window managers such as i3) draw on it as on any X server; the page shows the screen and sends
the keyboard and the mouse.

Requirements
------------
- Linux x86_64 with glibc 2.39 or newer (Debian 13, Ubuntu 24.04). For older systems, build from
  source or use the Docker image.
- A browser with WebAssembly. H.264 video (XSRV_VIDEO=h264) also needs ffmpeg with libx264.

Start (tarball)
---------------
    ./start.sh
Then open http://127.0.0.1:8080/ and click the page to give it the keyboard. X clients use
DISPLAY=:1 (the socket /tmp/.X11-unix/X1):
    DISPLAY=:1 xterm &

Debian package
--------------
    sudo apt install ./xsrv-web_<version>_amd64.deb
    sudo systemctl enable --now xsrv-web
    sudo xsrv-web token
The last command prints the page address with its token. The install made a random token in
/var/lib/xsrv-web/token; XSRV_TOKEN in /etc/default/xsrv-web (root only) wins over it. The
service is installed but not started: if :1 is taken on this machine, set XSRV_X11_SOCKET in
/etc/default/xsrv-web first. The service runs as the user xsrv-web. Its X socket and its page accept every local user. When
other users share the machine, set both XSRV_TOKEN (for the page) and XSRV_X11_COOKIE (for X
clients). X clients of other users can use MIT-SHM only when they pass the memory as a file
descriptor (ShmAttachFd); clients that use System V shared memory (cairo, and so GTK) send their
pixels without it, which is slower.

Settings (environment variables)
--------------------------------
XSRV_PORT          Port of the page. Default 8080.
XSRV_ADDR          Full HTTP address; it wins over XSRV_PORT. Default: 0.0.0.0 and XSRV_PORT when a
                   token is set, 127.0.0.1 and XSRV_PORT when there is none.
XSRV_TOKEN         16 to 128 characters of A-Z a-z 0-9 . _ ~ -. Required when XSRV_ADDR is not
                   a loopback address. The page then needs ?token=<token> once (it sets a
                   cookie). The Debian package makes one at install; `sudo xsrv-web token`
                   prints it.
XSRV_X11_SOCKET    X11 socket path. Default /tmp/.X11-unix/X1 (DISPLAY=:1).
XSRV_X11_COOKIE    32 hex digits: X clients must show this MIT-MAGIC-COOKIE-1 (add it with xauth).
XSRV_SESSIONS      1 to 8 independent screens; the page picks one with ?session=N.
XSRV_KEYMAP        us, es, gb or de.
XSRV_WM            0 turns off the built-in window manager (for i3 and other managers).
XSRV_WM_MAXIMIZE   0 keeps the size that a window asks for.
XSRV_XKB           0 turns off the XKEYBOARD extension (on by default).
XSRV_MONITORS      1 to 4 virtual monitors reported through RANDR.
XSRV_VIDEO         h264 sends the screen as video (needs ffmpeg).
XSRV_COMPRESSION   lz4 (the default) is the fastest for the server and the page. zstd sends about
                   a third of the bytes for text, and the page needs longer to decode them: use
                   it on a slow network.
XSRV_SECURE_COOKIE 1 when the page is served through TLS (a reverse proxy).
XSRV_WEB_DIR       The folder of the page. start.sh and the service set it.
XSRV_ALLOW_NO_AUTH 1 allows a network address without a token. Anyone who reaches the port then
                   controls the screen and the keyboard. Do not use it outside a closed network.

Security
--------
The page listens on every interface when a token is set. Without TLS the token goes over the
network in clear text: outside a trusted network, set XSRV_ADDR=127.0.0.1:8080 and put a reverse
proxy with TLS in front.
