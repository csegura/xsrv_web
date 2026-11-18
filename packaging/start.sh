#!/bin/sh
# SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
# SPDX-License-Identifier: AGPL-3.0-only

# Starts xsrv-web from the folder of this script, also when it is called through a link.
# The settings are XSRV_* variables (README.txt).
self=$(readlink -f -- "$0") || exit 1
here=$(dirname -- "$self")
export XSRV_WEB_DIR="${XSRV_WEB_DIR:-$here/share/xsrv-web/web}"
exec "$here/bin/xsrv-web" "$@"
