# SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
# SPDX-License-Identifier: AGPL-3.0-only

# What the packaging scripts share. Sourced from the project root.

# The packages get the same modes whatever the umask of the caller is.
umask 022

VERSION=$(cargo pkgid -p xsrv-server)
# The id ends in "#<version>" or "#<name>@<version>".
VERSION=${VERSION##*#}
VERSION=${VERSION##*@}
BINARY=target/release/xsrv-web
MACHINE=$(uname -m)

if [ ! -x "$BINARY" ] || [ ! -f web/pkg/xsrv_wasm_bg.wasm ]; then
    echo "packaging: build first (make dist or make deb do it)" >&2
    exit 1
fi

# The newest glibc symbol version that the binary uses, for example 2.39.
GLIBC=$(objdump -T "$BINARY" | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -n 1)
GLIBC=${GLIBC#GLIBC_}

# The files of the page, and only them: the server serves everything in its web folder.
copy_page() {
    mkdir -p "$1/pkg"
    cp web/index.html web/style.css web/*.js web/favicon-*.png "$1/"
    cp web/pkg/xsrv_wasm.js web/pkg/xsrv_wasm_bg.wasm "$1/pkg/"
}
