#!/usr/bin/env bash
# Runs INSIDE a distro container (see run.sh). Installs the CLI, exporter and
# extension packages into a real PostgreSQL, drives a workload, and checks that
# the extension's SQL surface, the CLI's export and the exporter's /metrics all
# agree about what happened.
. "$(dirname "$0")/lib.sh"

# ---------------------------------------------------------------- install
step "install packages"
if [ "$FAMILY" = deb ]; then
    apt-get install -y -qq --no-install-recommends /pkgs/*.deb >/dev/null && pass "apt-get install of all three packages (dependencies resolved)" || fail "apt-get install"
else
    dnf -qy install /pkgs/*.rpm >/dev/null && pass "dnf install of all three packages (dependencies resolved)" || fail "dnf install"
fi
check "extension module at $LIBDIR/pg_plansight.so" test -f "$LIBDIR/pg_plansight.so"
check "extension control file at $EXTDIR" test -f "$EXTDIR/pg_plansight.control"
EXPECT_VER="$(sed -n "s/^default_version *= *'\(.*\)'/\1/p" "$EXTDIR/pg_plansight.control")"
check "base install script pg_plansight--$EXPECT_VER.sql at $EXTDIR" test -f "$EXTDIR/pg_plansight--$EXPECT_VER.sql"
check "pg-plansight --version" pg-plansight --version
check "pg-plansight-exporter --version" pg-plansight-exporter --version
check "exporter default config installed" test -f /etc/pg-plansight-exporter/config.toml
check "exporter systemd unit installed" sh -c 'test -f /lib/systemd/system/pg-plansight-exporter.service || test -f /usr/lib/systemd/system/pg-plansight-exporter.service'
if [ "$FAMILY" = deb ]; then
    check "dpkg -V clean for the extension" dpkg -V "postgresql-$PG-plansight"
else
    check "rpm -V clean for the extension" rpm -V "plansight_$PG"
fi

# ---------------------------------------------------------------- cluster
step "start PostgreSQL $PG with the extension preloaded and auto_explain logging"
start_cluster 'pg_plansight,auto_explain,pg_stat_statements' && pass "cluster starts with the module preloaded" || { fail "cluster start"; cat "$PGLOGDIR/ctl.log"; exit 1; }

# ---------------------------------------------------------------- extension
step "extension: create, check"
sql "CREATE EXTENSION pg_plansight" >/dev/null && pass "CREATE EXTENSION" || fail "CREATE EXTENSION"
check "plansight_check() answers and reports no error" plansight_check_ok
check "extension version is $EXPECT_VER" test "$(sql "SELECT extversion FROM pg_extension WHERE extname='pg_plansight'")" = "$EXPECT_VER"

step "workload: $WORKLOAD executions of one query shape"
sql "CREATE TABLE e2e_t AS SELECT g AS id, md5(g::text) AS v FROM generate_series(1,5000) g" >/dev/null
sql "ANALYZE e2e_t" >/dev/null
for i in $(seq 1 $WORKLOAD); do sql "SELECT count(*) FROM e2e_t WHERE id < $((i * 100))" >/dev/null; done
sql "SELECT v FROM e2e_t ORDER BY id DESC LIMIT 5" >/dev/null
sleep 4

step "extension: tables, views, functions"
ext_calls=$(sql "SELECT coalesce(sum(calls),0) FROM plansight.statements WHERE representative_sql ILIKE 'select count(*)%e2e_t%'")
[ "$ext_calls" = "$WORKLOAD" ] && pass "plansight.statements counted $WORKLOAD calls" || fail "plansight.statements calls = '$ext_calls', expected $WORKLOAD"
for view in statements_summary top_by_total_time query_timeline; do
    n=$(sql "SELECT count(*) FROM plansight.$view") && [ "${n:-0}" -ge 1 ] && pass "plansight.$view returns rows ($n)" || fail "plansight.$view"
done
probe_row="$(sql "SELECT calls || ' ' || (total_time_ms > 0) FROM plansight.statements_summary WHERE representative_sql ILIKE 'select count(*)%e2e_t%'")"
[ "$probe_row" = "$WORKLOAD true" ] && pass "statements_summary: probe has $WORKLOAD calls and positive total time" || fail "statements_summary probe row = '$probe_row'"
check "plansight_format() pretty-prints SQL" sh -c "[ -n \"\$(su postgres -c \"$PGBIN/psql -X -At -d postgres -c \\\"SELECT plansight_format('select a,b from t where x=1')\\\"\")\" ]"
check "plansight_capture_stats() reports hook mode" sh -c "$(declare -f sql); PGBIN=$PGBIN; sql 'SELECT * FROM plansight_capture_stats()' | grep -q '^hook|'"
sql "CREATE EXTENSION pg_stat_statements" >/dev/null && pass "pg_stat_statements created alongside" || fail "pg_stat_statements"
check "plansight_pgss_view() creates the join view" test "$(sql "SELECT plansight_pgss_view()")" = t
check "plansight.statements_with_pgss is selectable" sql "SELECT count(*) FROM plansight.statements_with_pgss"
sql "CREATE ROLE e2e_plain LOGIN" >/dev/null
as_plain() { su postgres -c "$PGBIN/psql -X -At -d postgres -U e2e_plain -c \"$1\"" 2>&1; }
check "unprivileged role cannot read plansight.statements" sh -c "$(declare -f as_plain); PGBIN=$PGBIN; as_plain 'SELECT count(*) FROM plansight.statements' | grep -qi 'permission denied'"
check "unprivileged role cannot call plansight_reset()" sh -c "$(declare -f as_plain); PGBIN=$PGBIN; as_plain 'SELECT plansight_reset()' | grep -qi 'permission denied'"

# ---------------------------------------------------------------- CLI
step "CLI: parse the server log"
cp "$PGLOG" /tmp/pg-copy.log
pg-plansight --export /tmp/cli-export.json /tmp/pg-copy.log >/tmp/cli.out 2>&1 && pass "pg-plansight --export succeeded" || { fail "pg-plansight --export"; cat /tmp/cli.out; }
read -r cli_calls cli_hash < <(python3 - <<'PY'
import json
d = json.load(open("/tmp/cli-export.json"))
total, qhash = 0, "none"
for q in d["queries"]:
    text = (q.get("normalized_query") or "").lower().replace(" ", "")
    if "e2e_t" in text and "count(*)" in text and q["statistics"]["count"] >= total:
        total, qhash = q["statistics"]["count"], q["query_hash"]
print(total, qhash)
PY
)
[ "$cli_calls" = "$WORKLOAD" ] && pass "CLI export grouped the probe into one query with $WORKLOAD executions" || fail "CLI export count = '$cli_calls', expected $WORKLOAD"
pg-plansight --export /tmp/cli-redacted.json --redact /tmp/pg-copy.log >/dev/null 2>&1 \
    && python3 -c "
import json, sys
d = json.load(open('/tmp/cli-redacted.json'))
raw = open('/tmp/cli-redacted.json').read()
ok = all(not q['original_query'] and not q['plan'] for q in d['queries']) and 'id < 700' not in raw
sys.exit(0 if ok else 1)" \
    && pass "--redact drops original query/plan text and literals (normalized text is kept by design)" || fail "--redact leaked query text or literals"

# ---------------------------------------------------------------- exporter
step "exporter: process and scrape the same log"
mkdir -p /tmp/exp
cat > /tmp/exp/config.toml <<CONF
[server]
bind_address = "127.0.0.1:19090"
metrics_path = "/metrics"

[log_parsing]
log_paths = ["/tmp/pg-copy.log"]
poll_interval = "2s"

[metrics]
namespace = "pg_plansight"
backends = ["prometheus"]

[state]
database_path = "/tmp/exp/state.db"
CONF
check "check-config accepts the config" pg-plansight-exporter --config /tmp/exp/config.toml check-config
check "state init" pg-plansight-exporter --config /tmp/exp/config.toml --state-db /tmp/exp/state.db state init
pg-plansight-exporter --config /tmp/exp/config.toml --state-db /tmp/exp/state.db daemon >/tmp/exp/daemon.log 2>&1 &
exporter_pid=$!
# The exporter labels series by xxh3(normalized text), the CLI exports its own
# fingerprint, so the two ids cannot be joined. The probe is the one series whose
# count is exactly $WORKLOAD (every other statement ran a different number of
# times); require that to be unambiguous and to match the duration histogram too.
probe_series() { grep '^pg_plansight_query_executions_total' /tmp/exp/metrics.txt | awk -v w=$WORKLOAD '$NF==w' ; }
for _ in $(seq 1 40); do
    curl -fs http://127.0.0.1:19090/metrics >/tmp/exp/metrics.txt 2>/dev/null && [ "$(probe_series | wc -l)" -eq 1 ] && break
    sleep 1
done
kill -0 $exporter_pid 2>/dev/null && pass "exporter daemon is running" || { fail "exporter daemon died"; cat /tmp/exp/daemon.log; }
check "/metrics serves exporter metrics" grep -q 'pg_plansight' /tmp/exp/metrics.txt
probe_line="$(probe_series)"
probe_hash="$(sed -n 's/.*normalized_query_hash="\([^"]*\)".*/\1/p' <<<"$probe_line")"
if [ "$(wc -l <<<"$probe_line")" -eq 1 ] && [ -n "$probe_hash" ]; then
    exp_calls=$WORKLOAD
    pass "exporter has exactly one series with $WORKLOAD executions (hash $probe_hash)"
    hist_count="$(grep "^pg_plansight_query_duration_seconds_count" /tmp/exp/metrics.txt | grep "normalized_query_hash=\"$probe_hash\"" | awk '{s+=$NF} END{print s+0}')"
    [ "$hist_count" = "$WORKLOAD" ] && pass "duration histogram for that series also counts $WORKLOAD" || fail "duration histogram count for $probe_hash = '$hist_count'"
else
    exp_calls=none
    fail "expected exactly one exporter series with $WORKLOAD executions, got: $(grep -c . <<<"$probe_line")"
    grep '^pg_plansight_query_executions_total' /tmp/exp/metrics.txt | head -6 | sed 's/^/         | /'
fi
check "exporter exposes duration histogram" grep -q 'pg_plansight_query_duration_seconds' /tmp/exp/metrics.txt
kill -TERM $exporter_pid 2>/dev/null
wait $exporter_pid 2>/dev/null
exporter_rc=$?
[ "$exporter_rc" -eq 0 ] && pass "exporter exits 0 on SIGTERM" || { fail "exporter exit status $exporter_rc after SIGTERM"; tail -5 /tmp/exp/daemon.log | sed 's/^/         | /'; }

# ---------------------------------------------------------------- agreement
step "agreement across extension, CLI and exporter"
[ "$ext_calls" = "$cli_calls" ] && [ "$cli_calls" = "$exp_calls" ] && pass "all three report $WORKLOAD executions" || fail "disagreement: extension=$ext_calls cli=$cli_calls exporter=$exp_calls"

# ---------------------------------------------------------------- reset / drop / remove
step "extension: reset, drop, uninstall"
# plansight_ingest on the same log doubles what the hook captured (7 + 7).
ingested="$(sql "SELECT plansight_ingest(pg_read_file('$PGLOG'))")"
[ "${ingested:-0}" -ge 1 ] && pass "plansight_ingest() processed $ingested query groups from the server log" || fail "plansight_ingest() returned '$ingested'"
ingest_calls="$(sql "SELECT coalesce(sum(calls),0) FROM plansight.statements WHERE representative_sql ILIKE 'select count(*)%e2e_t%'")"
[ "$ingest_calls" = "$((WORKLOAD * 2))" ] && pass "hook + log ingest account for $((WORKLOAD * 2)) calls" || fail "calls after ingest = '$ingest_calls', expected $((WORKLOAD * 2))"
sleep 3
check "plansight_reset()" sql "SELECT plansight_reset()"
# The reset statement is itself captured a tick later, so assert on the probe rows.
check "probe rows gone after reset" test "$(sql "SELECT count(*) FROM plansight.statements WHERE representative_sql ILIKE '%e2e_t%'")" = 0
# plansight_pgss_view() creates a view that is not an extension member, so it
# blocks a plain DROP EXTENSION until it is removed.
check "DROP EXTENSION is blocked while the pgss join view exists" sh -c "! su postgres -c \"$PGBIN/psql -X -At -d postgres -c 'DROP EXTENSION pg_plansight'\""
sql "DROP VIEW plansight.statements_with_pgss" >/dev/null
sql "DROP EXTENSION pg_plansight" >/dev/null && pass "DROP EXTENSION" || fail "DROP EXTENSION"
check "schema removed by DROP EXTENSION" test "$(sql "SELECT count(*) FROM pg_namespace WHERE nspname='plansight'")" = 0
stop_cluster
if [ "$FAMILY" = deb ]; then apt-get remove -y -qq "postgresql-$PG-plansight" >/dev/null; else dnf -qy remove "plansight_$PG" >/dev/null; fi
check "module removed with the package" test ! -f "$LIBDIR/pg_plansight.so"
check "control file removed with the package" test ! -f "$EXTDIR/pg_plansight.control"
start_cluster 'auto_explain' && pass "cluster starts after removal (preload dropped)" || fail "cluster start after removal"

printf '\n%s\n' "== result: $failures failure(s)"
[ "$failures" -eq 0 ]
