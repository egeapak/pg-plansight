# pg_plansight — PostgreSQL extension design

This documents the design and roadmap for turning the `pg-plansight-core`
parser/analyzer into a PostgreSQL extension (`crates/pg_extension`) that
generates **cumulative query statistics queryable via SQL**, analogous to
`pg_stat_statements`.

## Goals

- Reuse the existing core (parser, query normalization/fingerprinting, plan
  analysis, statistics) inside a Postgres backend.
- Accumulate per-query-group statistics durably and expose them through SQL
  tables/views.
- Eventually capture statistics **automatically** as queries run, with no log
  scraping.

## Why the core had to become embeddable first

A Postgres backend (and background worker) is **strictly single-threaded**, and
its memory/error machinery is not thread-safe. Two core assumptions were
therefore unsafe to carry into the extension:

1. **rayon / `std::thread`** — spawning threads that touch backend state
   (palloc, SPI, the longjmp error path) is undefined behavior.
2. **Filesystem + decompression I/O** — unnecessary in-process and pulls heavy
   deps.

These are now behind Cargo features in the core (`parallel`, `file-io`, both on
by default). The extension depends on the core with `default-features = false`,
yielding a pure, single-threaded, no-IO library. `sqlparser`-based
normalization and `statrs` stats remain compiled in — they are pure-compute and
safe in a backend.

## Data-capture mechanism (two coexisting modes)

Capture is selected by `plansight.capture_mode` (`off` / `log` / `hook`); a
single background worker drains the selected source into the **same** UPSERT
path. The two automatic sources are **mutually exclusive** (running both would
double-count one execution).

| Mode | Mechanism | Status | Tradeoffs |
| --- | --- | --- | --- |
| `log` | bgworker tails the auto_explain log file | **Implemented (2a)** | Reuses Phase 1 verbatim; depends on auto_explain text logging + a readable log; writes every plan to disk; slight lag. |
| `hook` | in-process `ExecutorStart`/`ExecutorEnd` render the plan; default async path pushes a compact record to a bounded shmem ring drained by the worker (`synchronous=on` UPSERTs inline for tests) | **Implemented (2b)** | No log dependency, no per-query disk write, `min_duration_ms` gating. Hot path = instrument + render + memcpy. Measured overhead vs baseline: ~+20% on a 14 ms query, ~+17 µs (+19%) on a 0.09 ms point query, ~0 with `min_duration_ms` set. Needs `shared_preload_libraries`, unsafe FFI. |
| (manual) | `plansight_ingest(text)` SQL function | **Implemented (1)** | Import path / testing; always available. |

### Measured auto_explain cost (the per-query cost of `log` mode)

On PostgreSQL 16, decomposed via pgbench:

- Heavy analytical query (~14.6 ms): plan render+write ≈ noise; ANALYZE
  instrumentation **+~2.2 ms (~15%)**.
- OLTP point query (~0.079 ms): render+write **+0.026 ms (~33%)**, instrumentation
  **+0.009 ms**; total **+0.035 ms (~44%)** — fixed render+write dominates.
- ~1–27 KB written to the log per query (plan text).

`hook` mode targets this: shmem instead of disk, and skip fast queries before
any render. The worker, persistence, and analysis are shared by both modes.

For automatic capture, hooks are installed via the raw `pgrx_pg_sys` executor
hook function pointers in `_PG_init` (the high-level `PgHooks` abstraction was
removed in pgrx 0.16). The hot path stays tiny: normalize + push a compact
fixed-size record into a bounded shared-memory ring; a background worker drains
the ring on a timer and UPSERTs into the durable heap table off the hot path —
the same hot/cold split `pg_stat_statements` uses.

## Storage model

Durable cumulative stats live in regular heap tables (WAL-logged, crash-safe).
Counters are stored as **trivially-mergeable aggregates** so each batch folds in
with a single UPSERT:

`plansight.statements` (one row per fingerprint):
- `calls`, `total_time_ms` (additive)
- `sum_sq_time_ms` = Σ(tᵢ²) (additive) → population stddev derived in a view
- `min_time_ms` / `max_time_ms` (min/max), `first_seen` / `last_seen` (min/max)
- `representative_sql` + `representative_plan` — the slowest-seen example,
  replaced when a slower one arrives
- `complexity`, `metadata`, `plan_analysis` (`jsonb`) — the **same per-group
  analysis the TUI renders**, recomputed by the core analyzers for the
  representative plan and refreshed alongside it

`plansight.query_histogram` (one row per fingerprint × hour bucket):
- `calls`, `total/min/max_time_ms` — additive per bucket. This is the timeline
  the TUI charts and the time series Phase 3 regression runs over.

Views: `statements_summary` (derives mean/stddev, carries the analysis),
`top_by_total_time`, `query_timeline`.

**Formatting is on demand**, not stored: `plansight_format(sql)` pretty-prints
the raw `representative_sql` using the core formatter, so there is no redundant
formatted column to keep in sync.

Shared memory is volatile and fixed-size (`heapless`), so it is only suitable as
a hot staging ring (Phase 2) — durability comes from the heap tables.

### TUI parity

The cumulative views carry the full per-query dataset the interactive TUI shows
(timing aggregates, representative plan, complexity, metadata, plan findings,
per-hour histogram). Two items remain for Phase 3 because they cannot be derived
from additive counters: exact **percentiles** (need a streaming t-digest sketch)
and **regression detection** (runs off the `query_histogram` time series).

## Roadmap

- **Phase 0 — Embeddable core.** ✅ `parallel` / `file-io` features; core builds
  with `--no-default-features` free of rayon and I/O.
- **Phase 1 — Manual-ingest MVP + TUI-parity storage.** ✅ `plansight_ingest`,
  `plansight_format`, `plansight_reset`; `statements` (timing + representative
  plan + complexity/metadata/plan-findings `jsonb`) and `query_histogram`
  tables; summary/top/timeline views; `#[pg_test]` suite. Verified end-to-end on
  PostgreSQL 16 against real auto_explain output.
- **Phase 2a — Automatic capture (log-tailing worker).** ✅ A background worker
  (registered in `_PG_init`, requires `shared_preload_libraries`) incrementally
  tails the auto_explain log file, advancing a durable byte offset
  (`plansight.ingest_offset`) so restarts never double-count, and feeds the
  unchanged Phase 1 pipeline. GUCs: `plansight.enabled`, `plansight.log_path`,
  `plansight.database`, `plansight.flush_interval`. Verified end-to-end on
  PostgreSQL 16: queries are captured with full parity (timing + rich analysis +
  histogram) with no manual ingest, incrementally and without double-counting.
- **Phase 2b — Automatic capture (in-process executor hook).** ✅ Implemented.
  `ExecutorStart` allocates whole-query instrumentation; `ExecutorEnd` renders the
  plan via `explain.c`. The default async path copies the rendered bytes straight
  into a bounded shared-memory ring (`ring.rs`) — no intermediate heap `String` —
  and the background worker drains it and runs the heavy `aggregate_captures` +
  `persist_rows` off the hot path. A `synchronous=on` mode UPSERTs inline (with an
  active-snapshot push, re-entrancy guard, error isolation) for deterministic
  tests. Parallel workers are skipped; the async render is wrapped in
  `PgTryBuilder` so a render `ereport` can never escape into the user query.

  **Overhead has two parts and both are now tunable** (per-knob numbers in
  `docs/PGRX_BENCHMARKS.md`): per-node execution instrumentation (the
  `gettimeofday` loop — most of the OLAP cost) and the `EXPLAIN` render (a fixed
  cost that dominates point queries, carrying the per-node `actual time` the
  analyzers consume). Levers, hottest first: `sample_rate` (decided in
  `ExecutorStart` before timing is requested, so unsampled queries skip
  instrumentation entirely — the *average*-overhead lever); `capture_plan=off`
  (stats-only — skips render *and* instrumentation, ≈ baseline); `track_timing=off`
  (drops the per-node timer loop, keeps row counts — cuts a heavy-OLAP query from
  ~+39 % to ~+12 %); `track_io`/`track_settings`/`track_costs` trims. At
  `sample_rate=1` the default captures cost ~+39 % (heavy OLAP) / ~+8 µs (point).
  `plansight_capture_stats()` exposes config + ring counters (pending / captured /
  dropped). The render allocates into a reusable per-backend memory context that
  is reset (not freed) after each capture, so the StringInfo buffer is reused
  instead of palloc/repalloc-grown every time — ~36 % faster render for large
  plans under sustained throughput.

  **Richer capture.** The hook also records, from data already on hand:
  - **Core `queryId`** (`PlannedStmt.queryId`, read uniformly as `i64`;
    `EnableQueryId()` on PG16+, `compute_query_id=on` on PG14/15, absent on PG13)
    — stored as a `query_id` column so rows join to `pg_stat_statements`
    (verified equal on PG16).
  - **`EXPLAIN (SETTINGS)`** behind `plansight.track_settings` (default on;
    non-default planner GUCs behind the representative plan — a ~3.6 µs/render GUC
    scan), **`BUFFERS`/`WAL`** behind `plansight.track_io` (default on; feeds the
    BufferWal analyzer), and per-node **timing** behind `plansight.track_timing`
    (default on). All ExplainState flags are uniform PG13–18. The `Buffers:`/`WAL:`
    lines feed the cache-miss / temp-spill / WAL analyzer in `crates/core`.

  **Portability:** the hook code is `cfg`-gated for PG13–18 (the supported range
  of `pgrx-pg-sys` 0.18.1). The only signature that differs is `InstrAlloc` (PG13
  two-arg; PG14+ added `async_mode`); `ExecutorStart`/`standard_ExecutorStart` are
  `void` in all of 13–18, so the hook signature is uniform. Compile-verified
  against PG13/17/18 headers in containers. **PG12 is not supported** — it was
  dropped by pgrx 0.18 and reached upstream EOL in Nov 2024; adding it would mean
  downgrading pgrx and losing PG17/18, so it is out of scope. **T6:** the
  `.github/workflows/pgrx.yml` `cargo pgrx test` matrix now spans PG13–18.

  **Hardening (post full-team review — see `PGRX_HARDENING_PLAN.md`):** the hook
  is top-level-only by default (executor-nesting depth, reset at transaction end;
  `track_nested` opts in), records per-query sampling *ownership* so it only ever
  finalizes instrumentation it allocated (safe to co-load with auto_explain),
  skips bare `EXPLAIN` and aborting transactions, sets the re-entrancy guard on
  both paths, balances the active snapshot via RAII, and snapshots `track_io` at
  ExecutorStart. The worker isolates the analysis in `PgTryBuilder` (a malformed
  plan can't crash it) and drains the ring regardless of its own `capture_mode`
  (so per-session `SET capture_mode='hook'` works); `drain()` builds owned strings
  outside the LWLock. The buffer/WAL analyzer attributes per-node *deltas* (PG
  buffer counts are cumulative up the tree). `plansight.database` is `Sighup`, so
  `CREATE EXTENSION` without preload no longer FATALs. Enhancements:
  `plansight_pgss_view()` (join to `pg_stat_statements`) and a `last_drain`
  column in `plansight_capture_stats()`.
- **Phase 3 — Percentiles & regressions (full parity).** Add streaming
  percentiles via a per-group t-digest sketch, and a regression view computed
  over the `query_histogram` time series — the two pieces that cannot be derived
  from additive counters.
- **Phase 4 — Polish.** `plansight_import_file(path)` (core `file-io`), JSON
  export, eviction when a max-tracked cap is exceeded, packaging for PG 15–18.

## Risks / notes

- **Percentiles** cannot be maintained exactly from cumulative counters → switch
  to a t-digest/CKMS sketch per group in Phase 3.
- **Hot-path cost**: `normalize_query_enhanced` runs `sqlparser`; mitigate with a
  per-backend fingerprint cache and a `min_duration_ms` gate so only slow queries
  are normalized.
- **pgrx version churn**: pinned to `=0.19.1`; pgrx has no MSRV policy and tracks
  latest stable Rust (0.19 requires Rust 1.96), so bumping it moves the
  extension's MSRV with it.
- `cargo pgrx test` must run as a **non-root** user (Postgres refuses `initdb`
  as root).
