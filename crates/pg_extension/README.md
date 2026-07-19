# pg_plansight (PostgreSQL extension)

A PostgreSQL extension that captures cumulative `auto_explain` query statistics
and exposes them via SQL, analogous to `pg_stat_statements`. It reuses the
`pg-plansight-core` parser/normalizer as an embedded, single-threaded, no-IO
library.

> **Status: Phase 2b (automatic capture via in-process executor hooks).**
> The SQL surface, cumulative-stats storage, and both capture sources (`log`
> tailing and the in-process `hook` + shared-memory ring) are complete and
> validated end-to-end on PostgreSQL 16. See `docs/PGRX_PHASE2B_HOOK_DESIGN.md`.

## Automatic capture

`plansight.capture_mode` selects the source; a single background worker drains
it into the same cumulative tables. Modes are mutually exclusive (to avoid
double-counting one execution from two sources):

- `off` (default) — no automatic capture; manual `plansight_ingest` still works.
- `log` (**Phase 2a, implemented**) — tail the auto_explain log file.
- `hook` (**Phase 2b, implemented**) — in-process executor hooks render the plan
  and push a compact record into a bounded shared-memory ring; the background
  worker drains it off the query hot path. No auto_explain, no log file.

  Tuning GUCs (all superuser-settable per session; full reference in
  [`docs/CONFIGURATION.md`](../../docs/CONFIGURATION.md), measured costs in
  [`docs/PGRX_BENCHMARKS.md`](../../docs/PGRX_BENCHMARKS.md)):
  - `plansight.sample_rate` (0.0–1.0) — fraction of executions to capture.
    Decided in `ExecutorStart`, so **unsampled queries skip timing
    instrumentation entirely** — the primary *average*-overhead lever.
  - `plansight.sample_by` (`random` default / `query_id`) — `query_id` is
    *stratified*: the first execution of each queryId is always captured (rare
    query shapes aren't starved by frequent ones), the rest sampled at `sample_rate`.
  - `plansight.capture_plan` (default `on`) — `off` is **stats-only**: skip the
    EXPLAIN render *and* per-node instrumentation, recording only `calls`/timing
    (no plan, no analysis). ≈ baseline overhead — the lever for high-QPS/OLTP.
  - `plansight.track_timing` (default `on`) — per-node `ANALYZE` timing. `off`
    drops the per-node `gettimeofday` loop (the dominant analytical overhead) and
    keeps actual row counts — the lever for OLAP.
  - `plansight.min_duration_ms` — skip capturing executions faster than this.
  - `plansight.track_nested` (default `off`) — also capture queries nested in
    functions/triggers. Off captures top-level statements only, like
    `pg_stat_statements`' default, so `calls` doesn't double-count SPI-in-function.
  - `plansight.synchronous` (`off` default) — UPSERT inline instead of via the
    ring (deterministic; tests/debug only — heavy on the hot path).
  - `plansight.track_io` (default `on`) — capture per-node `Buffers:` and `WAL:`
    usage (cache hits/reads, temp spills, WAL bytes). The **BufferWal analyzer**
    turns this into findings: temp-file spills (`MemorySpill`, with the spilled
    MB and a "raise work_mem" hint), heavy disk reads (`HighBufferReads`, with the
    cache-hit ratio), and high WAL volume (`HighWalVolume`). Set `off` to drop the
    executor accounting overhead. The analysis itself runs in the worker, off the
    hot path.
  - `plansight.track_settings` (default `on`) / `track_costs` (`on`) /
    `track_verbose` (`off`) — render-only `EXPLAIN SETTINGS`/`COSTS`/`VERBOSE`
    toggles (µs-scale; `track_settings=off` skips a ~3.6 µs/render GUC scan).

  Per-knob overhead is measured in `docs/PGRX_BENCHMARKS.md`: at `sample_rate=1`
  the default captures cost ~+39% on a heavy OLAP query and ~+8 µs on a point
  query, of which per-node timing is ~⅔ of the OLAP cost and the render is
  ~all of the point-query cost. `track_timing=off` cuts OLAP to ~+12%;
  `capture_plan=off` (stats-only) is ≈ baseline. `sample_rate` scales the
  *average* linearly. The render reuses a per-backend memory context (reset, not
  freed, between captures), cutting allocator churn under sustained load.

### `hook` mode

```ini
# postgresql.conf
shared_preload_libraries = 'pg_plansight'
plansight.capture_mode = 'hook'
plansight.sample_rate = 1.0       # capture every execution (lower for high QPS)
plansight.min_duration_ms = 0     # also skip fast queries by raising this
# Cost knobs (optional; see docs/CONFIGURATION.md):
#plansight.track_timing = on      # off -> shed per-node timing (OLAP overhead)
#plansight.capture_plan = on      # off -> stats-only (numbers, no plan; cheapest)
#plansight.sample_by    = random  # query_id -> guarantee rare query shapes
```

Captured rows carry the same full analysis (plan, complexity, metadata,
findings, histogram) as the other modes. `SELECT * FROM plansight_capture_stats()`
reports the live config plus the ring counters (`ring_pending`,
`captured_total`, `dropped_total`); a rising `dropped_total` means the ring
overflows between drains — lower `sample_rate` or flush more often.

#### Operational notes
- **`pg_stat_statements` join:** after installing pg_stat_statements, call
  `SELECT plansight_pgss_view();` to (re)create `plansight.statements_with_pgss`,
  which joins the cumulative stats to pgss on `queryid` (plansight's plan
  analysis alongside pgss's execution counters). It returns false and warns if
  pgss isn't present.
- **`plansight_capture_stats()`** also reports `last_drain_epoch`
  (`to_timestamp(last_drain_epoch)`), so a stale value distinguishes "the worker
  isn't draining" from "nothing matched".
- **`query_id`** is PostgreSQL's `compute_query_id` value, stored so rows can join
  `pg_stat_statements` on `queryid`. It is enabled automatically on PG16+; on
  PG14/15 set `compute_query_id = on`; PG13 has no core query id (falls back to
  the internal fingerprint, and `query_id` is `NULL`). Because the *grouping* key
  is a DB-agnostic fingerprint while `query_id` embeds relation OIDs, the stored
  `query_id` is meaningful only within the database that produced the
  representative plan.
- **`plansight.database`** (where the worker writes) is read once at worker
  start; changing it requires restarting the worker (or the server). It is a
  `Sighup` GUC so `CREATE EXTENSION` without `shared_preload_libraries` no longer
  FATALs — the SQL functions and manual `plansight_ingest` work without preload;
  only the worker/hooks/ring need it.
- **Ring sizing** (`SQL_CAP`/`PLAN_CAP`/`RING_CAP`) is fixed at compile time (the
  shared segment is a pointer-free fixed array, sized at postmaster start). Tuning
  it means recompiling; a dynamically-sized segment is intentionally out of scope
  (the fixed ring is a bounded sampler by design).

### `log` mode

```ini
# postgresql.conf
shared_preload_libraries = 'pg_plansight,auto_explain'
auto_explain.log_min_duration = 0      # log plans (text format)
plansight.capture_mode = 'log'
plansight.log_path   = '/var/log/postgresql/postgresql-16-main.log'
plansight.database   = 'postgres'     # must have CREATE EXTENSION pg_plansight
plansight.flush_interval = 10         # seconds
```

The worker tracks a durable byte offset (`plansight.ingest_offset`) so restarts
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

## SQL surface

Every table, view, and column — with example rows — is documented in
[`docs/VIEWS_REFERENCE.md`](../../docs/VIEWS_REFERENCE.md). Summary below.

## What it does

- `plansight_ingest(text) -> bigint` — parse a chunk of `auto_explain` log
  output, group executions by normalized-query fingerprint, and fold the
  per-group aggregates into the cumulative tables. Returns the number of
  distinct query groups written.
- `plansight_format(text) -> text` — pretty-print a SQL statement with the same
  formatter the TUI uses. The raw `representative_sql` is stored; the formatted
  form is derived on demand (e.g.
  `SELECT plansight_format(representative_sql) FROM plansight.statements`).
- `plansight_reset()` — discard all accumulated statistics.
- `plansight_check()` — **configuration doctor**. Returns `(severity, category,
  message)` rows flagging inconsistencies: capture mode set but the library isn't
  in `shared_preload_libraries`; `log` mode without `auto_explain` (or with
  `log_analyze=off` / `log_format!=text` / `log_min_duration=-1`); the extension
  installed in a different database than `plansight.database`; `synchronous=on` or
  `sample_rate=0`; `sample_by=query_id` without a core queryId; ring overflow.
  `SELECT * FROM plansight_check();`
- `plansight_capture_stats()` also reports the extension's **own per-query
  overhead** — `overhead_calls` and `overhead_{mean,min,max,stddev}_us`, the
  microseconds it adds at `ExecutorEnd` (render + ring push / sync persist) — so
  you can see how cheap capture is. `plansight_reset_stats()` clears those
  counters (independent of `plansight_reset()`, which clears the stored data).
- `plansight.statements` — cumulative counters (mergeable aggregates: `calls`,
  `total_time_ms`, `sum_sq_time_ms`, `min/max_time_ms`, `first/last_seen`) plus
  the representative plan and the **full per-group analysis the TUI shows**:
  `representative_plan` text and `complexity` / `metadata` / `plan_analysis`
  (the analyzer findings) as queryable `jsonb`.
- `plansight.query_histogram` — per-fingerprint, hour-bucketed execution
  histogram (`calls`, `total/min/max_time_ms`). Additive across ingests; the
  time-series backbone for the timeline view and (Phase 3) regression analysis.
- `plansight.statements_summary` — derives `mean_time_ms` and population
  `stddev_time_ms`, and carries the rich analysis columns.
- `plansight.top_by_total_time` — the summary ordered by cumulative time.
- `plansight.query_timeline` — per-bucket view with derived mean.

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

### Testing in Docker (no local pgrx/PostgreSQL toolchain)

To run the exact checks the pgrx CI job runs — `cargo fmt --check`, `cargo
clippy -D warnings`, and `cargo pgrx test` against a real PostgreSQL — without
installing the pgrx toolchain or PostgreSQL headers locally, use the Docker
harness (`docker/Dockerfile.test`):

```bash
# From the repository root (the build context must be the repo root):
just ext-test-docker 16          # PG major (default 16); first run is slow
                                 # (it compiles cargo-pgrx in the image)

# Or directly:
docker build -f crates/pg_extension/docker/Dockerfile.test \
  --build-arg PG_MAJOR=16 -t pg_plansight_test:pg16 .
docker run --rm pg_plansight_test:pg16
```

The image installs PostgreSQL + `cargo-pgrx` and runs everything as a non-root
user (Postgres refuses `initdb` as root). To iterate on your working tree
without rebuilding, mount it over the baked-in copy:

```bash
docker run --rm -v "$PWD:/home/pgrx/src" pg_plansight_test:pg16
```

```sql
CREATE EXTENSION pg_plansight;
SELECT plansight_ingest($$<paste auto_explain log lines>$$);
SELECT * FROM plansight.statements_summary ORDER BY total_time_ms DESC;
```

## Release packages

Tagging `vX.Y.Z` (which must equal both the `pg-plansight` and `pg_plansight`
crate versions — they move in lockstep) publishes one `.deb` and one `.rpm` per
PostgreSQL major (13–18) for `x86_64` and `arm64`, attached to the GitHub
release. The `x86_64` `.deb` for each major is smoke-tested (`CREATE EXTENSION
pg_plansight` in a real `postgres:NN` container) before publish.

```bash
# Debian/Ubuntu, e.g. PG16 on x86_64:
sudo dpkg -i postgresql-16-plansight_X.Y.Z_amd64.deb
# then, in the target database:
#   CREATE EXTENSION pg_plansight;
```

To preload the worker/hooks, add `pg_plansight` to `shared_preload_libraries`
and restart (see "Automatic capture" above).
