#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
# SPDX-License-Identifier: AGPL-3.0-only

# Builds dist/xsrv-web-<version>-linux-<machine>.tar.gz from the release build and web/.
# Run it with `make dist`, which builds both first.
set -euo pipefail
cd "$(dirname "$0")/.."
source packaging/common.sh

name="xsrv-web-${VERSION}-linux-${MACHINE}"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
root="$stage/$name"

install -Dm755 "$BINARY" "$root/bin/xsrv-web"
copy_page "$root/share/xsrv-web/web"
install -Dm755 packaging/start.sh "$root/start.sh"
install -Dm644 packaging/README.txt "$root/README.txt"
install -Dm644 LICENSE "$root/LICENSE"

mkdir -p dist
# The same files give the same archive: fixed owner, order and dates.
tar -C "$stage" --owner=0 --group=0 --numeric-owner --sort=name \
    --mtime="@${SOURCE_DATE_EPOCH:-0}" -czf "$stage/out.tar.gz" "$name"
mv "$stage/out.tar.gz" "dist/$name.tar.gz"
echo "dist/$name.tar.gz"
