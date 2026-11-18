# Docker

The files are in `docker/`.

## What was tested

On a test VM (Debian 13, Docker 26.1.5 from `docker.io` and `docker-cli`), with a **test-only
variant**: the runtime stage of `docker/Dockerfile`, with `FROM debian:trixie-slim` and the
release binary built outside the container (the binary needs glibc 2.39, and `bookworm-slim` has
2.36). `docker/Dockerfile.client` was built with `trixie-slim` too. The two containers ran with the
settings of `compose.yaml` (`docker run` flags, because the compose plugin is not installed):

- The server container runs as `uid=10001(xsrv)`, with `--read-only` (a write to `/app` and
  `/etc` fails with "Read-only file system"), `--cap-drop ALL` (all capability sets are 0) and
  `no-new-privileges` (`NoNewPrivs: 1`).
- The socket `/tmp/.X11-unix/X1` on the shared named volume is created with mode 0777 (`umask 000`
  of the entrypoint), and the `xterm` client container (uid 10002, its own read-only root with
  tmpfs for `/tmp` and `/home/client`) connected to it. Its window showed in the page.
- Without the token the page gives 401, with the token 200. The page was opened in headless
  Chromium through an `ssh -L` tunnel, text typed in it reached the xterm, and a resize of the
  browser window resized the screen.
- The containers, the volume and the images were removed afterwards.

## Decision of the owner

The owner said to forget Docker: the full `docker/Dockerfile` build in a container is not a requirement
any more. The files stay in the repository as an example, with the tests listed above.

## Your own image from the release tarball

The README has a Dockerfile that adds xsrv-web to another image from the release tarball. Checked
without Docker (2026-11-18, no Docker on the machines at hand), on the files of the tarball
`xsrv-web-0.1.0-linux-x86_64.tar.gz`, laid out as that Dockerfile lays them out
(`bin/xsrv-web` to `/usr/local/bin`, `share/xsrv-web/web` to the web folder):

- The program needs glibc 2.39 (`objdump -T` shows `GLIBC_2.39` as the newest symbol version)
  and `libgcc_s`, `libm` and `libc`. Debian 13 and Ubuntu 24.04 have them; Debian 12 does not.
- `docker/entrypoint.sh` with a command started the server, waited for the socket, ran the
  command as a client, and stopped the server when the command ended. The socket had mode 777, the
  page gave 401 without the token and 200 with it.

Not built: the Dockerfile of the README itself, and an image with a real client such as a browser.

## What was not tested

- **The real `docker/Dockerfile` was never built.** Its build stage (`FROM rust:1-bookworm`,
  `cargo install wasm-bindgen-cli`, two release builds) needs more disk and memory than the test VM
  has (about 800 MB of disk and 464 MB of RAM), and a larger VM could not be created. The commands of
  that stage were run outside a container instead (see below).
- `docker compose` itself (`compose.yaml` was only parsed), `bookworm-slim` as the runtime image,
  the `VIDEO=1` build argument, and `docker-up` / `docker-build` in the Makefile.

What was checked for the build stage, outside a container (a copy of the repository in a scratch
directory): the stub build for the dependencies, the real release build of `xsrv-web` (crate `xsrv-server`), the WASM
build, `wasm-bindgen`, and the removal of `web/test`. All pass.

## Build and run

```
export XSRV_TOKEN=<16 to 128 characters>
make docker-build
make docker-up
```

Open `http://127.0.0.1:8080/?token=$XSRV_TOKEN`.

## Clients in other containers

The server listens on `/tmp/.X11-unix/X1` inside the volume `x11-socket`. A client container
mounts the same volume and sets `DISPLAY=:1`. `docker/Dockerfile.client` is an example with
`xterm`.

## Settings

| Variable | Default in the image | Meaning |
| --- | --- | --- |
| `XSRV_ADDR` | `0.0.0.0:8080` | HTTP address |
| `XSRV_TOKEN` | none | Required for a non-loopback address |
| `XSRV_X11_SOCKET` | `/tmp/.X11-unix/X1` | X11 socket path |
| `XSRV_WM` | `1` | `0` turns off the built-in window manager |
| `XSRV_XKB` | `1` | `0` turns off the XKEYBOARD extension |
| `XSRV_X11_COOKIE` | none | 32 hex digits: clients must show this cookie (see `docs/security.md`) |
| `XSRV_VIDEO` | off | `h264` needs `ffmpeg` with libx264, which the image has only when built with `--build-arg VIDEO=1` (not built or tested) |
