#!/usr/bin/env bash
# Build the extension into a postgres image per major (13–18) and run the
# testcontainers throughput/overhead harness against each, printing a results
# table. Each image is removed after its run to bound disk use.
#
#   scripts/bench_versions.sh            # all of 13 14 15 16 17 18
#   scripts/bench_versions.sh "16 17"    # a subset
set -uo pipefail
VERSIONS="${1:-13 14 15 16 17 18}"
cd "$(dirname "$0")/.."

cargo build -p pg-loganalyze-bench-harness --release || exit 1
HARNESS=./target/release/pg-loganalyze-bench-harness

# Optional CA bundle for networks behind a TLS-inspecting proxy (see Dockerfile.bench).
CA_ARG=()
if [ -n "${EXTRA_CA:-}" ]; then CA_ARG=(--build-arg "EXTRA_CA=${EXTRA_CA}"); fi

echo "============================================================"
for v in $VERSIONS; do
  img="pg_loganalyze_bench:pg${v}"
  echo "=== [pg${v}] building $img ==="
  if ! docker build -f crates/pg_extension/docker/Dockerfile.bench \
        --build-arg PG_MAJOR="${v}" "${CA_ARG[@]}" -t "$img" . ; then
    echo "RESULT pg${v} BUILD FAILED"; continue
  fi
  echo "=== [pg${v}] running harness ==="
  PG_BENCH_IMAGE="$img" "$HARNESS" || echo "RESULT pg${v} HARNESS FAILED"
  docker rmi "$img" >/dev/null 2>&1 || true
done
echo "============================================================"
