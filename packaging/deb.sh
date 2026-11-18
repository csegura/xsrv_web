#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
# SPDX-License-Identifier: AGPL-3.0-only

# Builds dist/xsrv-web_<version>_<arch>.deb from the release build and web/.
# Run it with `make deb`, which builds both first. MAINTAINER sets the Maintainer field.
set -euo pipefail
cd "$(dirname "$0")/.."
source packaging/common.sh

maintainer=${MAINTAINER:-"xsrv-web packaging <root@localhost>"}
arch=$(dpkg --print-architecture)
# Debian sorts "~" before the release: 0.2.0~rc.1 comes before 0.2.0.
version=${VERSION//-/\~}
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
root="$stage/root"
files=packaging/deb
doc="$root/usr/share/doc/xsrv-web"

install -Dm755 "$BINARY" "$root/usr/bin/xsrv-web"
copy_page "$root/usr/share/xsrv-web/web"
install -Dm644 "$files/xsrv-web.service" "$root/usr/lib/systemd/system/xsrv-web.service"
# The settings hold the token and the X cookie: root only.
install -Dm600 "$files/xsrv-web.default" "$root/etc/default/xsrv-web"
install -Dm644 "$files/copyright" "$doc/copyright"
install -Dm644 LICENSE "$doc/LICENSE"
install -Dm644 packaging/README.txt "$doc/README.txt"
printf 'xsrv-web (%s) unstable; urgency=low\n\n  * Release %s.\n\n -- %s  %s\n' \
    "$version" "$version" "$maintainer" "$(date -R -d "@${SOURCE_DATE_EPOCH:-0}")" |
    gzip -9n > "$doc/changelog.gz"
chmod 644 "$doc/changelog.gz"

mkdir -p "$root/DEBIAN"
(cd "$root" && find . -path ./DEBIAN -prune -o -path ./etc -prune -o -type f -printf '%P\0' |
    sort -z | xargs -0 md5sum) > "$root/DEBIAN/md5sums"
size=$(du -s --apparent-size -k --exclude=DEBIAN "$root" | cut -f1)
control=$(<"$files/control")
control=${control//@VERSION@/"$version"}
control=${control//@ARCH@/"$arch"}
control=${control//@GLIBC@/"$GLIBC"}
control=${control//@MAINTAINER@/"$maintainer"}
control=${control//@SIZE@/"$size"}
printf '%s\n' "$control" > "$root/DEBIAN/control"
echo /etc/default/xsrv-web > "$root/DEBIAN/conffiles"
for script in postinst prerm postrm; do
    install -m755 "$files/$script" "$root/DEBIAN/$script"
done

mkdir -p dist
package="dist/xsrv-web_${version}_${arch}.deb"
dpkg-deb --root-owner-group -Zxz --build "$root" "$stage/out.deb" >/dev/null
mv "$stage/out.deb" "$package"
echo "$package"
