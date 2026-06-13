# pg_loganalyze (PostgreSQL extension)

A PostgreSQL extension that captures cumulative `auto_explain` query statistics
and exposes them via SQL, analogous to `pg_stat_statements`. It reuses the
`pg-loganalyze-core` parser/normalizer as an embedded, single-threaded, no-IO
library.

> **Status: Phase 2a (automatic capture via a log-tailing background worker).**
> The SQL surface, cumulative-stats storage, and automatic capture are complete
> and validated end-to-end on PostgreSQL 16. A lower-latency in-process executor
> hook (Phase 2b) is designed in `docs/PGRX_PHASE2B_HOOK_DESIGN.md`.

## Automatic capture

`loganalyze.capture_mode` selects the source; a single background worker drains
it into the same cumulative tables. Modes are mutually exclusive (to avoid
double-counting one execution from two sources):

- `off` (default) — no automatic capture; manual `loganalyze_ingest` still works.
- `log` (**Phase 2a, implemented**) — tail the auto_explain log file.
- `hook` (**Phase 2b, implemented**) — in-process executor hooks render the plan
  and push a compact record into a bounded shared-memory ring; the background
  worker drains it off the query hot path. No auto_explain, no log file.
  `loganalyze.min_duration_ms` skips fast queries; `loganalyze.synchronous=on`
  UPSERTs inline (tests/debug). GUCs are superuser-settable per session.
  Measured hot-path overhead: ~+20% on a 14 ms query, +17 µs on a 0.09 ms point
  query, ~0 with `min_duration_ms` set.

### `hook` mode

```ini
# postgresql.conf
shared_preload_libraries = 'pg_loganalyze'
loganalyze.capture_mode = 'hook'
loganalyze.min_duration_ms = 0   # capture everything; raise to skip fast queries
```

Captured rows carry the same full analysis (plan, complexity, metadata,
findings, histogram) as the other modes. Stats accrue in whichever database the
queries run in (the extension must be installed there).

### `log` mode

```ini
# postgresql.conf
shared_preload_libraries = 'pg_loganalyze,auto_explain'
auto_explain.log_min_duration = 0      # log plans (text format)
loganalyze.capture_mode = 'log'
loganalyze.log_path   = '/var/log/postgresql/postgresql-16-main.log'
loganalyze.database   = 'postgres'     # must have CREATE EXTENSION pg_loganalyze
loganalyze.flush_interval = 10         # seconds
```

The worker tracks a durable byte offset (`loganalyze.ingest_offset`) so restarts
never double-count, and reuses the exact Phase 1 aggregation, so auto-captured
rows carry the same full analysis.

### Cost note

In `log` mode the per-query cost is auto_explain's, not the extension's (the
worker runs asynchronously in a separate process). Measured overhead of
auto_explain on PostgreSQL 16: **~15% on a heavy analytical query** (per-node
ANALYZE instrumentation) and **~30–44% on a sub-millisecond point query** (the
fixed plan render + log write), plus ~1–27 KB written to the log per query.
`hook` mode is designed to avoid the log write and skip fast queries via
`min_duration_ms`/`sample_rate` gating.

## What it does

- `loganalyze_ingest(text) -> bigint` — parse a chunk of `auto_explain` log
  output, group executions by normalized-query fingerprint, and fold the
  per-group aggregates into the cumulative tables. Returns the number of
  distinct query groups written.
- `loganalyze_format(text) -> text` — pretty-print a SQL statement with the same
  formatter the TUI uses. The raw `representative_sql` is stored; the formatted
  form is derived on demand (e.g.
  `SELECT loganalyze_format(representative_sql) FROM loganalyze.statements`).
- `loganalyze_reset()` — discard all accumulated statistics.
- `loganalyze.statements` — cumulative counters (mergeable aggregates: `calls`,
  `total_time_ms`, `sum_sq_time_ms`, `min/max_time_ms`, `first/last_seen`) plus
  the representative plan and the **full per-group analysis the TUI shows**:
  `representative_plan` text and `complexity` / `metadata` / `plan_analysis`
  (the analyzer findings) as queryable `jsonb`.
- `loganalyze.query_histogram` — per-fingerprint, hour-bucketed execution
  histogram (`calls`, `total/min/max_time_ms`). Additive across ingests; the
  time-series backbone for the timeline view and (Phase 3) regression analysis.
- `loganalyze.statements_summary` — derives `mean_time_ms` and population
  `stddev_time_ms`, and carries the rich analysis columns.
- `loganalyze.top_by_total_time` — the summary ordered by cumulative time.
- `loganalyze.query_timeline` — per-bucket view with derived mean.

### Parity with the TUI

The cumulative views carry the same per-query data the interactive TUI renders,
with two exceptions still on the roadmap (Phase 3): exact **percentiles**
(need a streaming t-digest sketch — they cannot be kept exactly from cumulative
counters) and **regression detection** (runs off the `query_histogram`
time series).

## Why it is a separate workspace

The crate requires the `pgrx` toolchain and a `pgNN` feature to compile, so it
is `exclude`d from the parent Cargo workspace. Plain `cargo build/clippy
--workspace` (and the main CI gate) never touch it; build it with `cargo pgrx`.

## Building & testing

Prerequisites: `cargo install cargo-pgrx --locked` and `cargo pgrx init`
(or point pgrx at an existing install, e.g.
`cargo pgrx init --pg16 $(which pg_config)` plus the matching
`postgresql-server-dev-NN` headers).

```bash
cd crates/pg_extension

# Compile against a specific major:
cargo build --no-default-features --features pg16

# Run the in-database test suite (must NOT run as root — Postgres refuses
# initdb as root):
cargo pgrx test pg16

# Install into a local cluster and try it:
cargo pgrx install --no-default-features --features pg16 -c $(which pg_config)
```

```sql
CREATE EXTENSION pg_loganalyze;
SELECT loganalyze_ingest($$<paste auto_explain log lines>$$);
SELECT * FROM loganalyze.statements_summary ORDER BY total_time_ms DESC;
```
