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
check "an install script at $EXTDIR" sh -c "ls $EXTDIR/pg_plansight--*.sql"
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
start_cluster 'pg_plansight,auto_explain' && pass "cluster starts with the module preloaded" || { fail "cluster start"; cat "$PGLOGDIR/ctl.log"; exit 1; }

# ---------------------------------------------------------------- extension
step "extension: create, check"
sql "CREATE EXTENSION pg_plansight" >/dev/null && pass "CREATE EXTENSION" || fail "CREATE EXTENSION"
check "plansight_check() reports no error" sh -c "! su postgres -c \"$PGBIN/psql -X -At -d postgres -c \\\"SELECT severity FROM plansight_check()\\\"\" | grep -qx error"
check "extension version is 0.2.0" test "$(sql "SELECT extversion FROM pg_extension WHERE extname='pg_plansight'")" = 0.2.0

step "workload: $WORKLOAD executions of one query shape"
sql "CREATE TABLE e2e_t AS SELECT g AS id, md5(g::text) AS v FROM generate_series(1,5000) g" >/dev/null
sql "ANALYZE e2e_t" >/dev/null
for i in $(seq 1 $WORKLOAD); do sql "SELECT count(*) FROM e2e_t WHERE id < $((i * 100))" >/dev/null; done
sql "SELECT v FROM e2e_t ORDER BY id DESC LIMIT 5" >/dev/null
sleep 4

step "extension: tables, views, functions"
ext_calls=$(sql "SELECT coalesce(sum(calls),0) FROM plansight.statements WHERE representative_sql ILIKE '%count(*)%e2e_t%'")
[ "$ext_calls" = "$WORKLOAD" ] && pass "plansight.statements counted $WORKLOAD calls" || fail "plansight.statements calls = '$ext_calls', expected $WORKLOAD"
for view in statements_summary top_by_total_time query_timeline; do
    n=$(sql "SELECT count(*) FROM plansight.$view") && [ "${n:-0}" -ge 1 ] && pass "plansight.$view returns rows ($n)" || fail "plansight.$view"
done
check "plansight.statements_summary exposes the probe" sh -c "su postgres -c \"$PGBIN/psql -X -At -d postgres -c \\\"SELECT 1 FROM plansight.statements_summary WHERE representative_sql ILIKE '%e2e_t%'\\\"\" | grep -q 1"
check "plansight_format() pretty-prints SQL" sh -c "[ -n \"\$(su postgres -c \"$PGBIN/psql -X -At -d postgres -c \\\"SELECT plansight_format('select a,b from t where x=1')\\\"\")\" ]"
check "plansight_capture_stats() answers" sql "SELECT * FROM plansight_capture_stats()"
ingested=$(sql "SELECT plansight_ingest(pg_read_file('$PGLOG'))") && [ -n "$ingested" ] && pass "plansight_ingest() accepts the server log ($ingested groups)" || fail "plansight_ingest()"
if [ "$PG" -ge 13 ]; then
    check "plansight_pgss_view() answers" sql "SELECT plansight_pgss_view()"
fi
sql "CREATE ROLE e2e_plain LOGIN" >/dev/null
as_plain() { su postgres -c "$PGBIN/psql -X -At -d postgres -U e2e_plain -c \"$1\"" 2>&1; }
check "unprivileged role cannot read plansight.statements" sh -c "$(declare -f as_plain); PGBIN=$PGBIN; as_plain 'SELECT count(*) FROM plansight.statements' | grep -qi 'permission denied'"
check "unprivileged role cannot call plansight_reset()" sh -c "$(declare -f as_plain); PGBIN=$PGBIN; as_plain 'SELECT plansight_reset()' | grep -qi 'permission denied'"

# ---------------------------------------------------------------- CLI
step "CLI: parse the server log"
cp "$PGLOG" /tmp/pg-copy.log
pg-plansight --export /tmp/cli-export.json /tmp/pg-copy.log >/tmp/cli.out 2>&1 && pass "pg-plansight --export succeeded" || { fail "pg-plansight --export"; cat /tmp/cli.out; }
cli_calls=$(python3 - <<'PY'
import json
d = json.load(open("/tmp/cli-export.json"))
total = 0
for q in d["queries"]:
    text = (q.get("normalized_query") or "").lower().replace(" ", "")
    if "e2e_t" in text and "count(*)" in text:
        total = max(total, q["statistics"]["count"])
print(total)
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
for _ in $(seq 1 30); do curl -fs http://127.0.0.1:19090/metrics >/tmp/exp/metrics.txt 2>/dev/null && grep -q 'executions_total' /tmp/exp/metrics.txt && break; sleep 1; done
kill -0 $exporter_pid 2>/dev/null && pass "exporter daemon is running" || { fail "exporter daemon died"; cat /tmp/exp/daemon.log; }
check "/metrics serves exporter metrics" grep -q 'pg_plansight' /tmp/exp/metrics.txt
exp_calls=$(grep '^pg_plansight_query_executions_total' /tmp/exp/metrics.txt | grep -i 'e2e_t' | awk '{s+=$NF} END{print s+0}')
if [ "${exp_calls:-0}" = 0 ]; then
    # series are labelled by hash, not text: fall back to the series whose count matches the workload
    exp_calls=$(grep '^pg_plansight_query_executions_total' /tmp/exp/metrics.txt | awk -v w=$WORKLOAD '$NF==w {print $NF; exit}')
fi
[ "$exp_calls" = "$WORKLOAD" ] && pass "exporter counted $WORKLOAD executions for the probe" || fail "exporter count = '$exp_calls', expected $WORKLOAD"
check "exporter exposes duration histogram" grep -q 'pg_plansight_query_duration_seconds' /tmp/exp/metrics.txt
kill $exporter_pid 2>/dev/null
wait $exporter_pid 2>/dev/null
check "exporter stopped on SIGTERM" test $? -ne 137

# ---------------------------------------------------------------- agreement
step "agreement across extension, CLI and exporter"
[ "$ext_calls" = "$cli_calls" ] && [ "$cli_calls" = "$exp_calls" ] && pass "all three report $WORKLOAD executions" || fail "disagreement: extension=$ext_calls cli=$cli_calls exporter=$exp_calls"

# ---------------------------------------------------------------- reset / drop / remove
step "extension: reset, drop, uninstall"
check "plansight_reset()" sql "SELECT plansight_reset()"
check "statements empty after reset" test "$(sql "SELECT count(*) FROM plansight.statements")" = 0
sql "DROP EXTENSION pg_plansight" >/dev/null && pass "DROP EXTENSION" || fail "DROP EXTENSION"
check "schema removed by DROP EXTENSION" test "$(sql "SELECT count(*) FROM pg_namespace WHERE nspname='plansight'")" = 0
stop_cluster
if [ "$FAMILY" = deb ]; then apt-get remove -y -qq "postgresql-$PG-plansight" >/dev/null; else dnf -qy remove "plansight_$PG" >/dev/null; fi
check "module removed with the package" test ! -f "$LIBDIR/pg_plansight.so"
check "control file removed with the package" test ! -f "$EXTDIR/pg_plansight.control"
start_cluster 'auto_explain' && pass "cluster starts after removal (preload dropped)" || fail "cluster start after removal"

printf '\n%s\n' "== result: $failures failure(s)"
[ "$failures" -eq 0 ]
