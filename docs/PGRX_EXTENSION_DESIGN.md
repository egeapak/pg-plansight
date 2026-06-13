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

## Data-capture mechanism (two coexisting modes)

Capture is selected by `loganalyze.capture_mode` (`off` / `log` / `hook`); a
single background worker drains the selected source into the **same** UPSERT
path. The two automatic sources are **mutually exclusive** (running both would
double-count one execution).

| Mode | Mechanism | Status | Tradeoffs |
| --- | --- | --- | --- |
| `log` | bgworker tails the auto_explain log file | **Implemented (2a)** | Reuses Phase 1 verbatim; depends on auto_explain text logging + a readable log; writes every plan to disk; slight lag. |
| `hook` | in-process `ExecutorStart`/`ExecutorEnd` render the plan and UPSERT it | **Implemented (2b, synchronous)** | No log dependency, no per-query disk write, `min_duration_ms` gating; needs `shared_preload_libraries`, unsafe FFI. The synchronous UPSERT runs on the query hot path; moving it to a bounded shmem ring drained by the worker is the next optimization. |
| (manual) | `loganalyze_ingest(text)` SQL function | **Implemented (1)** | Import path / testing; always available. |

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

`loganalyze.statements` (one row per fingerprint):
- `calls`, `total_time_ms` (additive)
- `sum_sq_time_ms` = Σ(tᵢ²) (additive) → population stddev derived in a view
- `min_time_ms` / `max_time_ms` (min/max), `first_seen` / `last_seen` (min/max)
- `representative_sql` + `representative_plan` — the slowest-seen example,
  replaced when a slower one arrives
- `complexity`, `metadata`, `plan_analysis` (`jsonb`) — the **same per-group
  analysis the TUI renders**, recomputed by the core analyzers for the
  representative plan and refreshed alongside it

`loganalyze.query_histogram` (one row per fingerprint × hour bucket):
- `calls`, `total/min/max_time_ms` — additive per bucket. This is the timeline
  the TUI charts and the time series Phase 3 regression runs over.

Views: `statements_summary` (derives mean/stddev, carries the analysis),
`top_by_total_time`, `query_timeline`.

**Formatting is on demand**, not stored: `loganalyze_format(sql)` pretty-prints
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
- **Phase 1 — Manual-ingest MVP + TUI-parity storage.** ✅ `loganalyze_ingest`,
  `loganalyze_format`, `loganalyze_reset`; `statements` (timing + representative
  plan + complexity/metadata/plan-findings `jsonb`) and `query_histogram`
  tables; summary/top/timeline views; `#[pg_test]` suite. Verified end-to-end on
  PostgreSQL 16 against real auto_explain output.
- **Phase 2a — Automatic capture (log-tailing worker).** ✅ A background worker
  (registered in `_PG_init`, requires `shared_preload_libraries`) incrementally
  tails the auto_explain log file, advancing a durable byte offset
  (`loganalyze.ingest_offset`) so restarts never double-count, and feeds the
  unchanged Phase 1 pipeline. GUCs: `loganalyze.enabled`, `loganalyze.log_path`,
  `loganalyze.database`, `loganalyze.flush_interval`. Verified end-to-end on
  PostgreSQL 16: queries are captured with full parity (timing + rich analysis +
  histogram) with no manual ingest, incrementally and without double-counting.
- **Phase 2b — Automatic capture (in-process executor hook).** ✅ Implemented in
  synchronous form: `ExecutorStart` allocates whole-query instrumentation,
  `ExecutorEnd` renders the plan via `explain.c` and folds it into the cumulative
  tables through the shared `aggregate_captures` + `persist_rows` path (with an
  active-snapshot push, a re-entrancy guard, error isolation, and a
  parallel-leader-only check). Verified end-to-end on PostgreSQL 16 with no
  auto_explain and no log file. **Next:** move the UPSERT off the hot path into a
  bounded shmem ring drained by the worker (`PGRX_PHASE2B_HOOK_DESIGN.md`), plus
  `sample_rate` and PG13–18 `cfg` for the `ExecutorStart` signature change.
- **Phase 3 — Percentiles & regressions (full parity).** Add streaming
  percentiles via a per-group t-digest sketch, and a regression view computed
  over the `query_histogram` time series — the two pieces that cannot be derived
  from additive counters.
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
