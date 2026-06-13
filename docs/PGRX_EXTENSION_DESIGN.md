# pg_loganalyze — PostgreSQL extension design

This documents the design and roadmap for turning the `pg-loganalyze-core`
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

## Data-capture mechanism (decision)

| Option | Verdict |
| --- | --- |
| (a) bgworker tails the log file | Secondary — fragile (rotation, log format/destination drift, permissions). Keep as an opt-in importer. |
| (b) in-process `ExecutorEnd` hook | **Primary** for automatic capture — structured, exact timing, independent of where logs go. |
| (c) user-called function | Helper / import path — this is **Phase 1**. |

For automatic capture, hooks are installed via the raw `pgrx_pg_sys` executor
hook function pointers in `_PG_init` (the high-level `PgHooks` abstraction was
removed in pgrx 0.16). The hot path stays tiny: normalize + push a compact
fixed-size record into a bounded shared-memory ring; a background worker drains
the ring on a timer and UPSERTs into the durable heap table off the hot path —
the same hot/cold split `pg_stat_statements` uses.

## Storage model

Durable cumulative stats live in a regular heap table (WAL-logged, crash-safe).
Counters are stored as **trivially-mergeable aggregates** so each batch folds in
with a single UPSERT:

- `calls`, `total_time_ms` (additive)
- `sum_sq_time_ms` = Σ(tᵢ²) (additive) → population stddev derived in a view
- `min_time_ms` / `max_time_ms` (min/max)
- `first_seen` / `last_seen` (min/max)

`loganalyze.statements_summary` derives `mean_time_ms` and `stddev_time_ms`;
`loganalyze.top_by_total_time` orders by cumulative time.

Shared memory is volatile and fixed-size (`heapless`), so it is only suitable as
a hot staging ring — durability comes from the heap table.

## Roadmap

- **Phase 0 — Embeddable core.** ✅ `parallel` / `file-io` features; core builds
  with `--no-default-features` free of rayon and I/O.
- **Phase 1 — Manual-ingest MVP.** ✅ `loganalyze_ingest(text)` +
  `loganalyze_reset()`, schema + summary/top views, `#[pg_test]` suite. Verified
  end-to-end on PostgreSQL 16.
- **Phase 2 — Automatic capture.** `ExecutorEnd` hook → bounded shmem ring →
  background-worker flush; GUCs (`enabled`, `flush_interval`, `min_duration_ms`,
  `sample_rate`); requires `shared_preload_libraries`.
- **Phase 3 — Deep analysis & regressions.** Run the core analysis engine and
  regression detector in the worker; populate a `last_analysis jsonb` column and
  a plan-shape history table; add approximate percentiles via a streaming sketch
  (t-digest) since exact percentiles cannot be kept cumulatively.
- **Phase 4 — Polish.** `loganalyze_import_file(path)` (core `file-io`), JSON
  export, eviction when a max-tracked cap is exceeded, packaging for PG 15–18.

## Risks / notes

- **Percentiles** cannot be maintained exactly from cumulative counters → switch
  to a t-digest/CKMS sketch per group in Phase 3.
- **Hot-path cost**: `normalize_query_enhanced` runs `sqlparser`; mitigate with a
  per-backend fingerprint cache and a `min_duration_ms` gate so only slow queries
  are normalized.
- **pgrx version churn**: pinned to `=0.18.1`; pgrx has no MSRV policy and tracks
  latest stable Rust.
- `cargo pgrx test` must run as a **non-root** user (Postgres refuses `initdb`
  as root).
