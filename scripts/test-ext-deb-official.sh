#!/usr/bin/env bash
# Install an extension .deb into the official postgres:<major> image and load it.
#   scripts/test-ext-deb-official.sh <pg-major> <path-to-deb>
set -euo pipefail

pg="${1:?pg major}"
deb="${2:?deb path}"
name="pgtest-$pg-$$"

docker run -d --name "$name" -e POSTGRES_PASSWORD=postgres "postgres:$pg" >/dev/null
trap 'docker rm -f "$name" >/dev/null 2>&1 || true' EXIT
# The image entrypoint runs a throwaway server during initdb and then restarts;
# wait for the real one (the second "ready" line) before touching it.
for _ in $(seq 1 60); do
    [ "$(docker logs "$name" 2>&1 | grep -c 'database system is ready to accept connections')" -ge 2 ] && break
    sleep 2
done
docker exec "$name" pg_isready -U postgres
docker cp "$deb" "$name:/tmp/ext.deb"
docker exec "$name" bash -c 'apt-get update -qq && (dpkg -i /tmp/ext.deb || apt-get install -f -y)'
docker exec "$name" psql -U postgres -c 'CREATE EXTENSION pg_plansight;'
rows="$(docker exec "$name" psql -U postgres -At -c 'SELECT severity FROM plansight_check()')"
[ -n "$rows" ] && ! grep -qx error <<<"$rows" || { echo "plansight_check() unhealthy: $rows" >&2; exit 1; }
echo "plansight_check(): $rows"
