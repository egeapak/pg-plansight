#!/usr/bin/env bash
# Host driver: run the end-to-end package test inside a distro container.
#
#   scripts/pkgtest/run.sh <image> <pg-major> <packages-dir> [docker-platform] [old-ext-package]
#
# SCRIPT=upgrade-ext.sh runs the v0.1.0 -> current extension upgrade test instead
# (pass the old package as the fifth argument).
#
# <packages-dir> holds the CLI, exporter and extension packages (all .deb or all
# .rpm) for the image's family. The packages are copied in (docker cp), not
# bind-mounted, so this also works against a remote or socket-mounted daemon.
set -euo pipefail

image="${1:?image, e.g. ubuntu:18.04 or almalinux:8}"
pg="${2:?pg major}"
pkgdir="${3:?packages dir}"
platform="${4:-}"
old_ext="${5:-}"
here="$(cd "$(dirname "$0")" && pwd)"

cid=$(docker create ${platform:+--platform "$platform"} -e PG="$pg" "$image" bash "/e2e/${SCRIPT:-e2e.sh}")
trap 'docker rm -f "$cid" >/dev/null 2>&1 || true' EXIT
docker cp "$here" "$cid:/e2e"
docker cp "$pkgdir" "$cid:/pkgs"
[ -z "$old_ext" ] || docker cp "$old_ext" "$cid:/old-ext.pkg"
docker start -a "$cid"
