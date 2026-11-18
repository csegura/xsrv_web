# Security

## What xsrv protects

xsrv has two doors: the X11 socket and the HTTP/WebSocket port. A client on the X11 socket
can draw, read the screen (`GetImage`) and see all input. A user of the page can type and click
in every window. Treat both doors as full control of the session.

## HTTP and WebSocket

- `XSRV_TOKEN`: a secret of 16 to 128 characters from `A-Za-z0-9._~-`. When it is set, every
  page and WebSocket request needs it. The first request can carry `?token=...`; the server then
  sets the cookie `xsrv_token` (`HttpOnly`, `SameSite=Strict`). The compare takes the same time
  for any input.
- Bind policy: a loopback address (`127.0.0.1`) needs no token. Any other address needs
  `XSRV_TOKEN`, or `XSRV_ALLOW_NO_AUTH=1` to say that you accept the risk. The server refuses to
  start otherwise.
- The WebSocket checks the `Origin` header against the `Host` header. Without a token on a
  loopback address, the `Host` header must also be `127.0.0.1`, `localhost` or `[::1]`. This
  stops DNS rebinding.
- Every response has `X-Frame-Options: DENY` and a `Content-Security-Policy` with
  `frame-ancestors 'none'`, so another site cannot put the page in a frame.
- The server sends no TLS. For a remote use, put a reverse proxy with TLS in front of it
  and keep the token on. Without TLS the token goes over the network in clear text.
- The input from a page has a rate limit (3000 messages a second, and 10 resizes a second). When a page closes, all keys and buttons it held are
  released.

## X11 socket

- The socket is a Unix socket. Its file mode and the directory mode decide who can connect.
  The container image creates `/tmp/.X11-unix` with mode 1777 and runs xsrv as user 10001.
- `XSRV_X11_COOKIE` (32 hexadecimal digits) turns on MIT-MAGIC-COOKIE-1: a client must show the
  cookie in its connection setup, as with a normal X server. Give clients the same cookie with
  `xauth -f <file> add :1 MIT-MAGIC-COOKIE-1 <cookie>` and `XAUTHORITY=<file>`. Without the
  variable, any client that can open the socket may connect, so share the socket only with
  containers that you trust. Tested with x11rb (right, wrong, short and other-protocol cookies)
  and with the real `xauth` and `xdpyinfo`. The cookie is not encrypted on the way, and TCP
  bridges such as `socat` carry it in clear text.
- The server records the user id of each client (`SO_PEERCRED`). MIT-SHM `Attach` uses it: a
  System V segment is mapped only if that user could read it.
- MIT-SHM `AttachFd` needs no such check: the client passes the descriptor with `SCM_RIGHTS`, so
  it already gave the server access. The server accepts only a readable regular file in memory
  (a memfd, or tmpfs or hugetlbfs), of at most the segment size limit: a file on FUSE or NFS
  could block the one server thread in a read. It reads the file with `pread` instead of `mmap`:
  a client that shrinks the file gets `BadValue`, and the server cannot get `SIGBUS`.
- Descriptors are received with `MSG_CMSG_CLOEXEC`. A client is closed when it sends a
  descriptor before the connection setup, more than 16 in one message, or more than 16 that no
  request has taken yet (counted at each read, so one long request cannot collect more). A
  ShmAttachFd takes its descriptor even when the request is wrong, so the next one gets its own.
  The Debian unit sets `LimitNOFILE=8192`: 64 clients with 32 descriptors each, and 256
  segments, are more than the default limit of 1024.

## Limits

| Limit | Value |
| --- | --- |
| Clients | `MAX_CLIENTS` (see `x11/screen.rs`) |
| Resources per client | `MAX_RESOURCES_PER_CLIENT` |
| Pixel memory per client | `MAX_CLIENT_PIXEL_BYTES` |
| Pixels per request | `MAX_PIXELS_PER_REQUEST` |
| Request size | 4 MiB after BIG-REQUESTS |
| Output queue per client | 8 MiB; a client that does not read is disconnected |
| Shape rectangles | 2048 for each shape |
| Shared memory segment | `MAX_SEGMENT_BYTES` |
| Connections waiting for their setup | 32 (`MAX_PENDING_SETUPS`); more are closed at once |
| Time for the connection setup | 10 s |

A request that needs too much work is cut by a work budget instead of stalling the server.
Each request runs inside `catch_unwind`, so a bug in one request closes that client and not
the server.

## Container

- Non-root user, read-only root file system, `cap_drop: ALL`, `no-new-privileges` (see
  `docker/compose.yaml`). `compose.yaml` refuses to start without `XSRV_TOKEN`.
- Only `shm.rs` has `unsafe` code (`shmctl`, `shmat`, `shmdt`, the reads of a mapped System V segment, and `recvmsg`). The rest of the server
  forbids it.

## Tests

- `crates/xsrv-server/tests/fuzz_requests.rs` sends seeded random and mutated requests, random
  bytes and bad setups, and checks that the server still answers a normal client.
- `fuzz/` has a coverage-guided `cargo-fuzz` target (`requests`) that feeds a stream of bytes to
  the request handlers through `x11::Harness`. Run it with `cd fuzz && cargo +nightly fuzz run requests`.
  The longest run so far (12 minutes) found no crash. A fuzzer that starts with an empty corpus finds
  little in the extension requests; it is a first pass, not an audit.

## Known gaps

- MIT-SHM trusts the user id of the socket peer. Behind the `socat` TCP bridge of the Development section in `README.md`
  that is the user of `socat`, not of the real client. Use the bridge only on a single-user
  machine, for development.
- The container entrypoint sets `umask 000`, so the X socket accepts every user of the shared
  volume. Share the volume only with containers that you trust.
- The token stays in the address bar. The cookie has the `Secure` flag only with
  `XSRV_SECURE_COOKIE=1`, which you set behind a TLS proxy.
- `fuzz_requests.rs` does not fail when a handler panics (the panic is caught and logged), and
  most random requests stop at the first resource lookup.
- Selections: the last-change time is forgotten when the owner is set to None.

- No audit by an external person. Two internal reviews found issues that were fixed before the first release.
- The container image has been written but not built here (no Docker or podman on the machine).
- The container files were audited by reading only.
