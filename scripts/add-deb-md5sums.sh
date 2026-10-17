#!/usr/bin/env bash
# cargo-deb 3.x writes no DEBIAN/md5sums, so `dpkg -V` and debsums verify nothing
# for the package. Rewrite each given .deb with the file and its checksum list.
#
#   scripts/add-deb-md5sums.sh <package.deb>...
set -euo pipefail

if ! command -v dpkg-deb >/dev/null 2>&1; then
    echo "warning: dpkg-deb not found; leaving $* without md5sums (CI validates their presence)" >&2
    exit 0
fi

for deb in "$@"; do
    tree="$(mktemp -d)"
    dpkg-deb -R "$deb" "$tree"
    (cd "$tree" && find . -path ./DEBIAN -prune -o -type f -print | sed 's|^\./||' | LC_ALL=C sort \
        | xargs -d '\n' md5sum > DEBIAN/md5sums)
    [ -z "${SOURCE_DATE_EPOCH:-}" ] || touch -d "@$SOURCE_DATE_EPOCH" "$tree/DEBIAN/md5sums"
    # xz: the dpkg on Ubuntu 18.04 / Debian 10 cannot read zstd members.
    dpkg-deb --root-owner-group -Zxz -b "$tree" "$deb.new" >/dev/null
    mv "$deb.new" "$deb"
    rm -rf "$tree"
done
