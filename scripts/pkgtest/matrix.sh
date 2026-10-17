#!/usr/bin/env bash
# Run the end-to-end package test over every row of matrix.txt (or the rows whose
# image/pg match the optional filters). Packages for the host architecture are
# read from <packages-root>/{deb,rpm}/, flat:
#   pg-plansight_*.deb  pg-plansight-exporter_*.deb  postgresql-<pg>-plansight_*.deb
#   pg-plansight-[0-9]*.rpm  pg-plansight-exporter-*.rpm  plansight_<pg>-*.rpm
#
#   scripts/pkgtest/matrix.sh <packages-root> [docker-platform] [image-filter] [pg-filter]
set -uo pipefail

root="$(realpath "${1:?packages root}")"
platform="${2:-}"
want_image="${3:-}"
want_pg="${4:-}"
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

failed=()
total=0
while read -r family image pg; do
    case "$family" in '' | \#*) continue ;; esac
    [ -z "$want_image" ] || [ "$want_image" = "$image" ] || continue
    [ -z "$want_pg" ] || [ "$want_pg" = "$pg" ] || continue
    total=$((total + 1))

    stage="$work/$family-${image//[:\/]/_}-$pg"
    mkdir -p "$stage"
    if [ "$family" = deb ]; then
        cp "$root"/deb/pg-plansight_*.deb "$root"/deb/pg-plansight-exporter_*.deb "$root/deb/postgresql-$pg-plansight_"*.deb "$stage/" 2>/dev/null
    else
        cp "$root"/rpm/pg-plansight-[0-9]*.rpm "$root"/rpm/pg-plansight-exporter-[0-9]*.rpm "$root/rpm/plansight_$pg-"*.rpm "$stage/" 2>/dev/null
    fi
    if [ "$(ls "$stage" | wc -l)" -ne 3 ]; then
        echo "!! $image PG$pg: expected 3 packages in $root/$family, found: $(ls "$stage" | tr '\n' ' ')"
        failed+=("$image/PG$pg (missing packages)")
        continue
    fi

    echo "################ $image PG$pg ($family)"
    if "$here/run.sh" "$image" "$pg" "$stage" "$platform"; then
        echo ">> PASS $image PG$pg"
    else
        echo ">> FAIL $image PG$pg"
        failed+=("$image/PG$pg")
    fi
done < "$here/matrix.txt"

echo
echo "== $((total - ${#failed[@]}))/$total scenarios passed"
[ "${#failed[@]}" -eq 0 ] || { printf 'failed: %s\n' "${failed[@]}"; exit 1; }
