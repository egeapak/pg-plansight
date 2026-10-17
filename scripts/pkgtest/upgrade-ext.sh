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
sql "CREATE TABLE up_in AS SELECT g AS id FROM generate_series(1,50) g" >/dev/null
for i in 1 2 3 4 5; do sql "SELECT count(*) FROM up_t WHERE id < $((i * 50))" >/dev/null; done
# IN lists of two lengths are two fingerprints under v0.1.0 and one under the new normalizer.
sql "SELECT count(*) FROM up_in WHERE id IN (1,2,3)" >/dev/null
sql "SELECT count(*) FROM up_in WHERE id IN (4,5,6,7,8)" >/dev/null
sleep 4
in_rows="$(sql "SELECT count(*) FROM plansight.statements WHERE representative_sql ILIKE 'select count(*) from up_in %'")"
[ "$in_rows" = 2 ] && pass "v0.1.0 keeps the two IN-list shapes as separate rows" || fail "v0.1.0 IN-list rows = '$in_rows', expected 2"
before=$(sql "SELECT coalesce(sum(calls),0) FROM plansight.statements WHERE representative_sql ILIKE 'select count(*) from up_t %'")
[ "$before" = 5 ] && pass "v0.1.0 captured 5 calls" || fail "v0.1.0 captured '$before' calls"

step "upgrade the package in place"
stop_cluster
dpkg -i /pkgs/postgresql-"$PG"-plansight_*.deb >/dev/null && pass "dpkg upgrades the package over v0.1.0" || fail "dpkg upgrade"
NEW_VER="$(sed -n "s/^default_version *= *'\(.*\)'/\1/p" "$EXTDIR/pg_plansight.control")"
check "installed package is $NEW_VER" sh -c "dpkg -s postgresql-$PG-plansight | grep -q '^Version: $NEW_VER'"
start_cluster pg_plansight && pass "cluster starts with the new module over the old catalog" || { fail "start after upgrade"; cat "$PGLOGDIR/ctl.log"; exit 1; }
check "old extension version still 0.1.0 before UPDATE" test "$(sql "SELECT extversion FROM pg_extension WHERE extname='pg_plansight'")" = 0.1.0
check "v0.1.0 SQL objects work under the new module" sql "SELECT count(*) FROM plansight.statements_summary"
check "captured data survived the module swap" test "$(sql "SELECT coalesce(sum(calls),0) FROM plansight.statements WHERE representative_sql ILIKE 'select count(*) from up_t %'")" = "$before"

step "ALTER EXTENSION UPDATE"
sql "ALTER EXTENSION pg_plansight UPDATE" >/dev/null && pass "ALTER EXTENSION UPDATE" || fail "ALTER EXTENSION UPDATE"
check "extension is at $NEW_VER" test "$(sql "SELECT extversion FROM pg_extension WHERE extname='pg_plansight'")" = "$NEW_VER"
check "data survived the UPDATE" test "$(sql "SELECT coalesce(sum(calls),0) FROM plansight.statements WHERE representative_sql ILIKE 'select count(*) from up_t %'")" = "$before"
check "fingerprints were re-keyed to plansight_fingerprint()" test "$(sql "SELECT count(*) FROM plansight.statements WHERE fingerprint <> plansight_fingerprint(representative_sql)")" = 0
check "both IN-list shapes merged into one row with 2 calls" test "$(sql "SELECT count(*)||'/'||coalesce(sum(calls),0) FROM plansight.statements WHERE representative_sql ILIKE 'select count(*) from up_in %'")" = 1/2
check "upgraded rows carry the database they were captured in" test "$(sql "SELECT count(*) FROM plansight.statements WHERE database <> current_database()")" = 0
check "views expose the database column" test "$(sql "SELECT count(*) FROM plansight.statements_summary WHERE database = current_database()")" -gt 0
check "histogram rows still reference their statements" test "$(sql "SELECT count(*) FROM plansight.query_histogram h LEFT JOIN plansight.statements s USING (fingerprint) WHERE s.fingerprint IS NULL")" = 0
check "plansight_check() answers and reports no error" plansight_check_ok
for i in 6 7; do sql "SELECT count(*) FROM up_t WHERE id < $((i * 50))" >/dev/null; done
sleep 4
after_calls="$(sql "SELECT coalesce(sum(calls),0) FROM plansight.statements WHERE representative_sql ILIKE 'select count(*) from up_t %'")"
[ "$after_calls" = 7 ] && pass "capture continues after the upgrade (7 calls)" || fail "calls after the upgrade = '$after_calls', expected 7"
# Columns and extension-owned functions of a fresh install vs the upgraded one.
catalog() {
    su postgres -c "$PGBIN/psql -v ON_ERROR_STOP=1 -X -At -d $1" <<'SQL'
SELECT 'col', table_name, column_name, data_type FROM information_schema.columns
 WHERE table_schema = 'plansight' ORDER BY 2, 3;
SELECT 'fn', p.proname, pg_get_function_arguments(p.oid)
  FROM pg_proc p JOIN pg_depend d ON d.objid = p.oid AND d.deptype = 'e'
  JOIN pg_extension e ON e.oid = d.refobjid AND e.extname = 'pg_plansight' ORDER BY 2, 3;
SQL
}
su postgres -c "$PGBIN/psql -X -At -d postgres -c 'CREATE DATABASE fresh'" >/dev/null
su postgres -c "$PGBIN/psql -X -At -d fresh -c 'CREATE EXTENSION pg_plansight'" >/dev/null
catalog postgres > /tmp/cat-upgraded.txt
catalog fresh > /tmp/cat-fresh.txt
if [ -s /tmp/cat-fresh.txt ] && diff -u /tmp/cat-fresh.txt /tmp/cat-upgraded.txt >/tmp/cat.diff; then
    pass "upgraded catalog matches a fresh install ($(wc -l < /tmp/cat-fresh.txt) columns/functions compared)"
else
    fail "upgraded catalog differs from a fresh install"; head -20 /tmp/cat.diff | sed 's/^/         | /'
fi

printf '\n%s\n' "== result: $failures failure(s)"
[ "$failures" -eq 0 ]
