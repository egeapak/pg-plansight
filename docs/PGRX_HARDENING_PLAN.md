# pg_loganalyze hardening plan (post full-team review)

Phased plan to resolve every finding from the three-reviewer audit (memory-safety/FFI,
PostgreSQL semantics, architecture/quality) plus the agreed enhancements. Each phase is
self-contained, ends with tests + `fmt`/`clippy`, and is committed and pushed before the
next begins.

## Execution model & constraints
- **Single shared environment:** one PG16 cluster, one `target/`, one git tree. Code edits
  for a phase may be delegated to a subagent, but **build → install → cluster-test → commit
  is serialized by the lead** to keep shared state consistent. Phases run sequentially.
- **Validation gate per phase:** `cargo build` (pg16) + `cargo fmt --check` +
  `cargo clippy --no-default-features --features pg16 --all-targets -D warnings` +
  the phase's unit/`#[pg_test]`/functional checks must pass before commit.
- **Multi-version:** a container compile (`pg13`/`17`/`18`) is run in Phase 7 (and after any
  change touching `cfg`-gated code) since the runtime test matrix lives in CI.
- **Severity legend:** 🔴 correctness bug · 🟠 hardening · 🟡 semantics/docs · 🟢 enhancement.

## Progress tracker
| Phase | Task | Sev | Status |
|------|------|-----|--------|
| 1 | P1.1 Per-node buffer/WAL deltas (cumulative double-count) | 🔴 | done (code+test), pending commit |
| 1 | P1.2 Parser: `local` blocks + `I/O Timings` | 🟢 | todo |
| 1 | P1.3 `track_io`→`MemorySpill` `#[pg_test]` | 🟢 | todo |
| 2 | P2.1 Worker always drains ring (per-session SET gap) | 🔴 | done (code), pending commit |
| 2 | P2.2 Worker panic isolation around `aggregate_captures` | 🔴 | todo |
| 2 | P2.3 `drain()` builds Strings outside the LWLock | 🔴 | todo |
| 2 | P2.4 Ring round-trip + overflow unit tests | 🟢 | todo |
| 3 | P3.1 Skip `EXEC_FLAG_EXPLAIN_ONLY` queries | 🔴 | todo |
| 3 | P3.2 Explicit per-query sampled-marker (drop `totaltime`-presence; auto_explain co-load) | 🟠 | todo |
| 3 | P3.3 Re-entrancy guard on the async path too | 🟠 | todo |
| 3 | P3.4 Snapshot push/pop balanced on the sync error path | 🟠 | todo |
| 3 | P3.5 Snapshot `track_io` at ExecutorStart; reuse at render | 🟠 | todo |
| 3 | P3.6 `sampled()` zero-seed guard; `static mut` via `&raw` | 🟠 | todo |
| 3 | P3.7 queryId side-map: fall back to any non-zero id in group | 🟠 | todo |
| 4 | P4.1 `database` GUC → not PGC_POSTMASTER (CREATE EXTENSION FATAL) | 🔴 | todo |
| 4 | P4.2 Nesting-level guard (top-level capture) + GUC | 🟡 | todo |
| 4 | P4.3 Docs: cross-DB queryId, track_io default, nesting | 🟡 | todo |
| 5 | P5.1 `pg_stat_statements` join view | 🟢 | todo |
| 5 | P5.2 BufferWal adopts `consolidated_config` pattern | 🟢 | todo |
| 5 | P5.3 `capture_stats`: last-drain time + gated counters | 🟢 | todo |
| 5 | P5.4 De-duplicate sync/async capture scaffolding | 🟢 | todo |
| 6 | P6.1 `#[pg_test]`: async ring drain, min_duration gating, queryId | 🟢 | todo |
| 6 | P6.2 Buffer-parser edge-case unit tests | 🟢 | todo |
| 7 | P7.1 Multi-version container compile (pg13/17/18) | — | todo |
| 7 | P7.2 Full pg16 end-to-end + benchmark re-measure | — | todo |
| 7 | P7.3 Docs sweep (README + design doc) | — | todo |

---

## Phase 1 — Buffer analyzer correctness
**P1.1 (🔴)** PG buffer/WAL counters are cumulative up the tree. Compute each node's own
contribution = its counters − Σ(direct children's), threshold on the delta. Fixes duplicate
findings at every ancestor and inflated report totals. *Done in `buffer_analysis.rs` with a
`attributes_spill_to_child_not_parent` regression test.*
**P1.2 (🟢)** Parse `local hit/read` (temp tables) into the read accounting and surface
`I/O Timings: read=… write=…` as evidence/a finding (disk-bound signal). Add parser unit tests.
**P1.3 (🟢)** `#[pg_test]`: `SET work_mem='64kB'; track_io=on`, run a spilling query, assert a
`MemorySpill` finding lands in `plan_analysis`.
**Validation:** `cargo test -p pg-loganalyze-core`; functional pg16 spill query shows one
spill per real operator (no parent duplication). **Commit.**

## Phase 2 — Worker & ring robustness
**P2.1 (🔴)** Worker drains the ring every tick regardless of its own `capture_mode`, so a
per-session `SET capture_mode='hook'` is actually persisted. *Done in `bgworker.rs`.*
**P2.2 (🔴)** Wrap `aggregate_captures` (heavy, runs on captured/possibly-malformed plan
text outside any catch) in `PgTryBuilder`; a panic degrades to a dropped batch + warning,
not a worker FATAL/restart.
**P2.3 (🔴)** `drain()` currently does `String`/UTF-8 allocation under the exclusive LWLock,
blocking all `push`es. Copy the used `Rec`s into a local `Vec<Rec>` under the lock, release,
then build `Capture`s.
**P2.4 (🟢)** Plain unit tests for `ring`: push/drain round-trip, truncation at caps, overflow
increments `dropped_total`.
**Validation:** unit tests; pg16 async-ring end-to-end still captures with parity. **Commit.**

## Phase 3 — Hook capture correctness & hardening
**P3.1 (🔴)** Early-return in `executor_start`/`maybe_capture` when
`eflags & EXEC_FLAG_EXPLAIN_ONLY != 0` (bare `EXPLAIN` was captured at ~0 ms).
**P3.2 (🟠)** Record the per-query sampling decision explicitly (a thread-local set keyed on
the `QueryDesc` pointer, or a flag) instead of inferring "we sampled" from `totaltime != NULL`,
which collides with `auto_explain`/other instrumenting extensions and risks double
`InstrEndLoop`. Only `InstrEndLoop` instrumentation we allocated.
**P3.3 (🟠)** Set the `CAPTURING` re-entrancy guard at the top of `maybe_capture` on both
paths so any nested executor call during render/persist is suppressed and the shared
`RENDER_CTX` can't be re-entered.
**P3.4 (🟠)** Move `PushActiveSnapshot`/`PopActiveSnapshot` into a scope that pops on every
exit (record stack depth; pop only what we pushed) so a caught SPI longjmp can't imbalance
the active-snapshot stack. (Sync/debug path.)
**P3.5 (🟠)** Snapshot `track_io` at `ExecutorStart` and reuse it at render, so a mid-query
GUC flip can't set `es.buffers=true` on an un-instrumented execution.
**P3.6 (🟠)** `sampled()`: force a non-zero seed (`x |= 1`). Replace `static mut` hook reads
with `&raw const`/`addr_of!` to satisfy the 2024 lint and avoid forming refs to `static mut`.
**P3.7 (🟠)** In `aggregate_captures`, fold the queryId side-map by fingerprint (or fall back
to any non-zero id in the group) so a representative whose own queryId was 0 still gets the
group's id.
**Validation:** `#[pg_test]` for EXPLAIN-only-not-captured and (where feasible) co-load
behavior; full suite; pg16 functional. **Commit per logical group (P3.1, P3.2–3.3, P3.4–3.7).**

## Phase 4 — Operational correctness & semantics
**P4.1 (🔴)** `loganalyze.database` is `PGC_POSTMASTER`; defining it when the lib loads
post-startup (`CREATE EXTENSION`/`LOAD` without preload) FATALs the backend. Make it
`Sighup` (worker reads it once at start; ALTER SYSTEM + worker restart still applies), or
skip Postmaster-context GUC registration outside preload. Add a graceful notice when not
preloaded.
**P4.2 (🟡)** Add an auto_explain-style nesting-level counter; capture top-level only by
default with `loganalyze.track_nested` (bool, default off) to opt into nested capture — stops
double-counting SPI-in-function and inner `EXPLAIN ANALYZE` plans.
**P4.3 (🟡)** Document: `query_id` is meaningful only within the representative's database
(cross-DB fingerprint collapse); the `track_io` default; nesting semantics.
**Validation:** pg16 — `CREATE EXTENSION` without preload no longer FATALs; nested-statement
capture count matches expectation. **Commit.**

## Phase 5 — Enhancements
**P5.1 (🟢)** `loganalyze.statements_with_pgss` view joining on `query_id = pg_stat_statements.queryid`
(guarded so it degrades when pgss absent). Fulfils the stated raison d'être.
**P5.2 (🟢)** Move BufferWal thresholds into `consolidated_config` + `ConfigurableAnalyzer`,
matching sibling analyzers; document the numbers.
**P5.3 (🟢)** `capture_stats`: add `last_drain` timestamp and a sampled-but-gated counter so
operators can tell "nothing matched" from "worker isn't draining".
**P5.4 (🟢)** Extract the shared render+scratch+`PgTryBuilder` scaffolding of
`capture_async`/`capture_synchronous` into one helper taking `FnOnce(&[u8])`.
**Validation:** unit/`#[pg_test]`; pg16 join view returns rows. **Commit per item.**

## Phase 6 — Test coverage
**P6.1 (🟢)** `#[pg_test]`s: async ring drain end-to-end (drive a manual drain), `min_duration_ms`
gating, queryId captured + equals pgss.
**P6.2 (🟢)** Buffer-parser edge cases: `local` blocks, missing `temp`, `WAL` without `bytes=`,
per-worker lines, parent/child subtraction.
**Validation:** full `cargo test` both crates green. **Commit.**

## Phase 7 — Final validation & docs
**P7.1** Container compile `--features pg13/17/18` (cfg-touched code in Phases 3/4).
**P7.2** Full pg16 end-to-end smoke of every feature + re-measure overhead (hot path unchanged).
**P7.3** README + `PGRX_EXTENSION_DESIGN.md` reflect all changes; mark this plan complete.
**Commit.**

---

## Out of scope (explicitly)
- **PG12 support** — dropped by pgrx 0.18 (upstream EOL Nov 2024); would lose PG17/18.
- **PG18 `ExecutorStart`→bool** — compiles clean against real pg18.4 headers via pgrx 0.18.1
  (void in its bindings); runtime is validated by the CI matrix. No action unless CI fails.
- **DSM-registry dynamic ring / custom cumulative-stats kinds** — at odds with the fixed-ring
  sampler design / not exposed in pgrx-pg-sys 0.18.1.
</content>
