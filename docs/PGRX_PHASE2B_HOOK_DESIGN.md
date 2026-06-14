# Phase 2b — In-process query capture via executor hooks (design)

Status: **design only** (Phase 2a, the log-tailing background worker, is
implemented and validated). Phase 2b is an **additional capture mode, not a
replacement**: it is selected by `plansight.capture_mode = 'hook'` (vs `'log'`
for 2a), the two sources being mutually exclusive. The existing background
worker dispatches on `capture_mode` and drains the chosen source into the
**same** Phase 1 UPSERT path. It is the lower-latency, log-format-independent
option (no per-query disk write; hot-path `min_duration_ms`/`sample_rate`
gating).

## Goal & reuse

Inside each backend, produce `(query_text, plan_text, duration_ms, timestamp)`
from a finished `QueryDesc` and route it through the existing grouping →
analyzers → `UPSERT_SQL` / `HISTOGRAM_UPSERT_SQL`. The only new core API needed
is a wrapper-free constructor:

```rust
// crates/core — reuses PlanParser::parse_plan + PlanFactory::create_query_plan_from_parsed
pub fn query_plan_from_capture(
    timestamp: DateTime<Utc>, duration_ms: f64,
    query_text: String, plan_text: String,   // EXPLAIN (FORMAT TEXT) output
) -> anyhow::Result<QueryPlan>;
```

Everything downstream (fingerprinting, complexity/metadata/plan analysis,
`StatRow`, the two UPSERTs) is reused verbatim.

## 1. Hook installation (raw `pg_sys` pointers)

pgrx removed the high-level `PgHooks` trait in 0.16, so install hooks by writing
the global function pointers in `pgrx::pg_sys` from `_PG_init`, saving and
chaining the previous pointer (or `standard_*`). Wrappers are
`#[pg_guard] unsafe extern "C-unwind"`. We need `ExecutorStart` (to enable
instrumentation) and `ExecutorEnd` (to read results); `ExecutorRun`/`Finish`
are optional pass-throughs used only to maintain a nesting level.

Version-sensitive signatures (gate with `#[cfg(feature="pgNN")]`):
- **PG18 `ExecutorStart` returns `bool`** (plan may be invalidated in `InitPlan`,
  leaving `planstate`/`totaltime` null — null-check before rendering).
- **PG18 `ExecutorRun` drops `execute_once`.**

Requires the library in `shared_preload_libraries`; guard registration with
`process_shared_preload_libraries_in_progress` so a plain `CREATE EXTENSION`
degrades to "SQL functions only, no capture."

## 2. Enable instrumentation in `ExecutorStart`

Before `standard_ExecutorStart` builds the PlanState tree, OR the flags so
timing exists at `ExecutorEnd`:

```rust
(*query_desc).instrument_options |= INSTRUMENT_TIMER | INSTRUMENT_ROWS;
// INSTRUMENT_BUFFERS behind a GUC (adds overhead)
```

Gate on `plansight.enabled` so disabled = zero instrumentation cost.

## 3. Render plan text at `ExecutorEnd`

Produce the same indented text the core parser already consumes, via `explain.c`
in a short-lived memory context (copy bytes out, then delete the context):
`NewExplainState` → set `analyze/timing/format = EXPLAIN_FORMAT_TEXT` →
`ExplainBeginOutput` → `ExplainPrintPlan` → `ExplainPrintTriggers` →
`ExplainEndOutput` → read `es->str_` (`StringInfo`). Duration =
`queryDesc->totaltime->total * 1000.0` (seconds→ms, after `InstrEndLoop`);
SQL = `queryDesc->sourceText`. Null-check `planstate`/`totaltime` (PG18).

## 4. The hot/cold split & the plan-text problem (key decision)

Rich analysis needs variable-size plan text, which can't live in fixed-size
shared memory. Options evaluated:

- (a) full parse+analyze in the backend, push finished `StatRow` — rejected
  (variable-size jsonb; sqlparser+engine on the hot path).
- (b) compact timing records + capture plan only for a new representative —
  needs hot-path shared state + SPI; partial win, high complexity.
- (c) SPI-UPSERT directly in `ExecutorEnd` — simplest, reuses Phase 1, but table
  locks + WAL on the hot path. **Keep as a `synchronous` debug/test mode.**
- **(d) RECOMMENDED: bounded shmem ring of `heapless::String<N>` plan snippets +
  compact timing, drained by a bgworker** that parses/analyzes/UPSERTs off the
  hot path.

Hot path does only: GUC gate → `min_duration_ms` gate → `sample_rate` gate →
render plan → `memcpy` (sql + truncated plan + duration + ts) into one ring slot
under a brief `exclusive()` LWLock. No sqlparser/engine/SPI/table-locks on the
hot path. Ring full → drop newest + bump a `dropped` counter (we are a sampler).

GUCs: `enabled`, `min_duration_ms` (primary cost bound), `sample_rate`,
`flush_interval`, `max_pending` (ring capacity, `PGC_POSTMASTER`),
`track_buffers`, `track` (none/top/all), `synchronous` (test mode).

## 5. Shared memory (pgrx 0.18)

`static RING: PgLwLock<CaptureRing>`; request space in a `shmem_request_hook`
(PG15+) or directly (PG13/14) with `RequestAddinShmemSpace` +
`RequestNamedLWLockTranche`; finalize with `pg_shmem_init!` in the shmem startup
hook. `CaptureRing { records: heapless::Vec<CaptureRecord, N>, dropped: u64 }`
with `CaptureRecord { epoch_secs, duration_ms, sql: HString<SQL_CAP>,
plan: HString<PLAN_CAP>, truncated }`. ~1024 × 8 KiB ≈ 8 MiB.

## 6. Coexistence & safety

- Always chain the previous hook; OR (never overwrite) `instrument_options`.
- **Parallel workers:** capture only in the leader (`!IsParallelWorker()`); the
  leader's `totaltime` already aggregates workers. Avoids double counting.
- **Nesting:** maintain `NESTING_LEVEL`; with `track=top` capture only at level 0.
- **Utility/DDL** bypass the executor hooks (handled via `ProcessUtility`) — good.
- Never run SPI in `ExecutorEnd` (async mode); SPI only in the bgworker txn.

## 7. Testing

- Pure-Rust unit tests for `query_plan_from_capture` (core) and the ring
  (enqueue/overflow/drain).
- `#[pg_test]` with `postgresql_conf_options()` setting
  `shared_preload_libraries = 'pg_plansight'` + `plansight.synchronous = on`
  for deterministic assertions; an async test forces a drain via a
  `plansight_flush()` helper. PG13–18 matrix.

## 8. Implementation checklist

1. core `query_plan_from_capture` + tests.
2. factor the per-group `StatRow` builder out of `aggregate_log`; add
   `aggregate_captures`.
3. GUCs → shmem ring → hooks (instrument + render + enqueue) → nesting/parallel
   guards → bgworker drainer → `synchronous` mode → tests/docs.

Sources: PostgreSQL `executor.h` hook typedefs; pgrx 0.18 `pg_guard`/shmem docs;
PG18 `ExecutorStart`→bool (pgsql-hackers); `explain.c` API.
