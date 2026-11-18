#!/bin/sh
# SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
# SPDX-License-Identifier: AGPL-3.0-only
# Starts xsrv-web. X clients in the same container find it with DISPLAY=:1.
# With arguments, it starts that command as an X client and stops when the client ends.
set -eu

# Clients in other containers run as other users. They need write access to the socket.
umask 000

export DISPLAY="${DISPLAY:-:1}"

if [ "$#" -eq 0 ]; then
    exec xsrv-web
fi

xsrv-web &
server=$!
trap 'kill "$server" 2>/dev/null || true' EXIT INT TERM

# The socket appears when the server is ready to accept clients.
tries=0
until [ -S "$XSRV_X11_SOCKET" ]; do
    tries=$((tries + 1))
    if [ "$tries" -gt 100 ] || ! kill -0 "$server" 2>/dev/null; then
        echo "entrypoint: xsrv-web did not start" >&2
        exit 1
    fi
    sleep 0.1
done

"$@"
