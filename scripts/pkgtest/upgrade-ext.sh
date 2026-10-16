#!/usr/bin/env bash
# Runs INSIDE a distro container (deb family, PG13+: v0.1.0 never shipped PG12
# and its RPMs used non-PGDG paths). Installs the released v0.1.0 extension,
# captures data, upgrades the package in place, and checks that the old SQL
# objects keep working under the new module and that ALTER EXTENSION UPDATE
# lands on 0.2.0 without losing the captured statistics.
. "$(dirname "$0")/lib.sh"

[ "$FAMILY" = deb ] || { echo "upgrade test is deb-only"; exit 0; }
[ -f /old-ext.pkg ] || { echo "no /old-ext.pkg supplied"; exit 2; }

step "install v0.1.0 and capture data"
cp /old-ext.pkg /tmp/old-ext.deb && apt-get install -y -qq --no-install-recommends /tmp/old-ext.deb >/dev/null && pass "v0.1.0 package installs" || fail "v0.1.0 install"
start_cluster pg_plansight && pass "cluster starts with the v0.1.0 module" || { fail "start"; cat "$PGLOGDIR/ctl.log"; exit 1; }
sql "CREATE EXTENSION pg_plansight" >/dev/null && pass "CREATE EXTENSION (v0.1.0)" || fail "CREATE EXTENSION"
check "extension is at 0.1.0" test "$(sql "SELECT extversion FROM pg_extension WHERE extname='pg_plansight'")" = 0.1.0
sql "CREATE TABLE up_t AS SELECT g AS id FROM generate_series(1,2000) g" >/dev/null
for i in 1 2 3 4 5; do sql "SELECT count(*) FROM up_t WHERE id < $((i * 50))" >/dev/null; done
sleep 4
before=$(sql "SELECT coalesce(sum(calls),0) FROM plansight.statements WHERE representative_sql ILIKE 'select count%up_t%'")
[ "$before" = 5 ] && pass "v0.1.0 captured 5 calls" || fail "v0.1.0 captured '$before' calls"

step "upgrade the package in place"
stop_cluster
dpkg -i /pkgs/postgresql-"$PG"-plansight_*.deb >/dev/null && pass "dpkg upgrades the package over v0.1.0" || fail "dpkg upgrade"
check "installed package version is 0.2.0" sh -c "dpkg -s postgresql-$PG-plansight | grep -q '^Version: 0.2.0'"
start_cluster pg_plansight && pass "cluster starts with the new module over the old catalog" || { fail "start after upgrade"; cat "$PGLOGDIR/ctl.log"; exit 1; }
check "old extension version still 0.1.0 before UPDATE" test "$(sql "SELECT extversion FROM pg_extension WHERE extname='pg_plansight'")" = 0.1.0
check "v0.1.0 SQL objects work under the new module" sql "SELECT count(*) FROM plansight.statements_summary"
check "captured data survived the module swap" test "$(sql "SELECT coalesce(sum(calls),0) FROM plansight.statements WHERE representative_sql ILIKE 'select count%up_t%'")" = "$before"

step "ALTER EXTENSION UPDATE"
sql "ALTER EXTENSION pg_plansight UPDATE" >/dev/null && pass "ALTER EXTENSION UPDATE" || fail "ALTER EXTENSION UPDATE"
check "extension is at 0.2.0" test "$(sql "SELECT extversion FROM pg_extension WHERE extname='pg_plansight'")" = 0.2.0
check "data survived the UPDATE" test "$(sql "SELECT coalesce(sum(calls),0) FROM plansight.statements WHERE representative_sql ILIKE 'select count%up_t%'")" = "$before"
check "plansight_check() reports no error" sh -c "! su postgres -c \"$PGBIN/psql -X -At -d postgres -c \\\"SELECT severity FROM plansight_check()\\\"\" | grep -qx error"
for i in 6 7; do sql "SELECT count(*) FROM up_t WHERE id < $((i * 50))" >/dev/null; done
sleep 4
check "capture continues after the upgrade (7 calls)" test "$(sql "SELECT coalesce(sum(calls),0) FROM plansight.statements WHERE representative_sql ILIKE 'select count%up_t%'")" = 7
check "a fresh CREATE EXTENSION matches the upgraded catalog" sh -c "su postgres -c \"$PGBIN/psql -X -At -d postgres -c 'CREATE DATABASE fresh'\" && su postgres -c \"$PGBIN/psql -X -At -d fresh -c 'CREATE EXTENSION pg_plansight'\""

printf '\n%s\n' "== result: $failures failure(s)"
[ "$failures" -eq 0 ]
