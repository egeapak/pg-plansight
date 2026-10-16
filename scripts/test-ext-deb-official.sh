#!/usr/bin/env bash
# Install an extension .deb into the official postgres:<major> image and load it.
#   scripts/test-ext-deb-official.sh <pg-major> <path-to-deb>
set -euo pipefail

pg="${1:?pg major}"
deb="${2:?deb path}"
name="pgtest-$pg-$$"

docker run -d --name "$name" -e POSTGRES_PASSWORD=postgres "postgres:$pg" >/dev/null
trap 'docker rm -f "$name" >/dev/null 2>&1 || true' EXIT
for _ in $(seq 1 30); do
    docker exec "$name" pg_isready -U postgres >/dev/null 2>&1 && break
    sleep 2
done
docker exec "$name" pg_isready -U postgres
docker cp "$deb" "$name:/tmp/ext.deb"
docker exec "$name" bash -c 'apt-get update -qq && (dpkg -i /tmp/ext.deb || apt-get install -f -y)'
docker exec "$name" psql -U postgres -c 'CREATE EXTENSION pg_plansight;'
docker exec "$name" psql -U postgres -c 'SELECT * FROM plansight_check();'
