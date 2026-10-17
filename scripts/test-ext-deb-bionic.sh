#!/usr/bin/env bash
# Install an extension .deb into a real PostgreSQL on ubuntu:18.04, preload it,
# and prove it captures queries. Shared by GitHub Actions, Woodpecker and local
# runs. The deb is copied in (docker cp) rather than bind-mounted so it also works
# when the Docker daemon is remote or reached through a mounted socket.
#
#   scripts/test-ext-deb-bionic.sh <pg-major> <path-to-deb> [docker-platform]
set -euo pipefail

pg="${1:?pg major}"
deb="${2:?deb path}"
platform="${3:-}"

read -r -d '' inner <<'INNER' || true
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends curl ca-certificates gnupg locales >/dev/null
# bionic is past PGDG main-repo support; its packages live in the archive.
curl -fsSL https://www.postgresql.org/media/keys/ACCC4CF8.asc | apt-key add - >/dev/null 2>&1
echo "deb https://apt-archive.postgresql.org/pub/repos/apt bionic-pgdg main" \
    > /etc/apt/sources.list.d/pgdg.list
apt-get update -qq
apt-get install -y -qq --no-install-recommends "postgresql-$PG" >/dev/null
dpkg -i /tmp/ext.deb
dpkg -s "postgresql-$PG-plansight" | grep -E "^(Version|Depends):"

cat >> "/etc/postgresql/$PG/main/postgresql.conf" <<CONF
shared_preload_libraries = 'pg_plansight'
plansight.capture_mode = 'hook'
plansight.min_duration_ms = 0
plansight.flush_interval = 1
plansight.sample_rate = 1.0
CONF
pg_ctlcluster "$PG" main start
for _ in $(seq 1 30); do pg_isready -q && break; sleep 1; done
pg_isready

sql() { su postgres -c "psql -v ON_ERROR_STOP=1 -X -At -c \"$1\""; }
sql "CREATE EXTENSION pg_plansight"
sql "SELECT severity, category, message FROM plansight_check()"
sql "CREATE TABLE t AS SELECT g AS id FROM generate_series(1,1000) g"
for _ in 1 2 3; do sql "SELECT count(*) FROM t WHERE id < 500" >/dev/null; done
sleep 4
sql "SELECT * FROM plansight_capture_stats()"
captured=$(sql "SELECT coalesce(sum(calls),0) FROM plansight.statements WHERE representative_sql ILIKE 'select count%from t where%'")
echo "captured calls for the probe query: $captured"
if [ "${captured:-0}" -ne 3 ]; then
    echo "extension loaded but captured nothing" >&2
    tail -30 "/var/log/postgresql/postgresql-$PG-main.log" >&2
    exit 1
fi
echo "OK: PG$PG extension installed, preloaded and capturing on ubuntu:18.04"
INNER

cid=$(docker create ${platform:+--platform "$platform"} -e PG="$pg" ubuntu:18.04 bash -c "$inner")
trap 'docker rm -f "$cid" >/dev/null 2>&1 || true' EXIT
docker cp "$deb" "$cid:/tmp/ext.deb"
docker start -a "$cid"
