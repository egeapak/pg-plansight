# pg_loganalyze hardening plan (post full-team review, expert-revised)

Phased plan resolving every finding from the three-reviewer audit (memory-safety/FFI, PG
semantics, architecture/quality) plus enhancements. **Revised** per two plan-review experts
(PG-internals + architecture). Each phase ends with tests + `fmt`/`clippy`, committed and
pushed before the next.

## Execution model & constraints
- Single shared env (one PG16 cluster, one `target/`, one git tree). Phases sequential;
  build→install→cluster-test→commit serialized by the lead. Subagents used for self-contained
  code where safe.
- **Gate per phase:** `cargo build` (pg16) + `cargo fmt --check` + `clippy …pg16 --all-targets
  -D warnings` + the phase's tests. **Cross-version container compile (pg13/17/18) runs after
  any phase touching cfg-gated or hook code (Phases 3, 4) — not only at the end.**
- Severity: 🔴 correctness bug · 🟠 hardening · 🟡 semantics/docs · 🟢 enhancement.

## Key expert revisions folded in
- Nesting (P4.2) hooks **ExecutorRun + ProcessUtility** with RAII decrement; ExecutorEnd reads
  `level==0`. Sampling marker (P3.2) is a **nesting-indexed stack flag** sharing that structure
  (no QueryDesc-pointer map). `InstrEndLoop` only on instrumentation we allocated. → **Build the
  nesting/sampling infra (Phase 3) before the per-capture hardening.**
- **New 🔴:** skip capture when the transaction is aborting.
- P4.1: `database`→`Sighup` removes the FATAL but the worker can't re-read it live → **worker
  restart required**; document, add a warning on change.
- P3.4 uses an **RAII snapshot guard**; P3.6 also converts `#[no_mangle]`→`#[unsafe(no_mangle)]`
  and fixes `install()` write sites with `&raw mut`.
- Parallel **Gather/per-worker Buffers**: delta math handled/tested (saturating, no double-sub).
- Async-path `#[pg_test]` + a test-only `loganalyze_drain_now()` pulled into Phase 2.
- P4.1 (no-preload FATAL) and P3.2 (auto_explain co-load) are **functional cluster checks**
  (the `pg_test` harness always preloads, so it can't reproduce them).
- PG18 `ExecutorStart`→bool: explicit verification + honest doc, not silent CI reliance.

## Progress tracker
| Phase | Task | Sev | Status |
|------|------|-----|--------|
| 1 | P1.1 Per-node buffer/WAL deltas | 🔴 | ✅ committed |
| 1 | P1.2 Parser: `local` blocks, `I/O Timings`, Gather/per-worker safety | 🟢 | todo |
| 1 | P1.3 `track_io`→`MemorySpill` `#[pg_test]` + WAL/parallel unit tests | 🟢 | todo |
| 2 | P2.1 Worker always drains ring | 🔴 | ✅ committed |
| 2 | P2.2 Worker panic isolation around `aggregate_captures` | 🔴 | todo |
| 2 | P2.3 `drain()` builds Strings outside the LWLock | 🔴 | todo |
| 2 | P2.4 `loganalyze_drain_now()` test helper + async-ring `#[pg_test]` | 🟢 | todo |
| 2 | P2.5 Ring round-trip/overflow + malformed-plan-no-panic unit tests | 🟢 | todo |
| 3 | P3.A Nesting infra: `ExecutorRun`+`ProcessUtility` hooks, level stack | 🔴 | todo |
| 3 | P3.B Top-level-only capture + `track_nested` GUC (was P4.2) | 🟡 | todo |
| 3 | P3.C Sampled-flag on the stack; `InstrEndLoop` only if we allocated (was P3.2) | 🟠 | todo |
| 3 | P3.D Skip `EXEC_FLAG_EXPLAIN_ONLY` (was P3.1) | 🔴 | todo |
| 3 | P3.E Skip capture during transaction abort (NEW) | 🔴 | todo |
| 3 | P3.X Cross-version compile checkpoint | — | todo |
| 4 | P4.A Re-entrancy guard on both paths (was P3.3) | 🟠 | todo |
| 4 | P4.B RAII snapshot guard, unwind-safe ctx restore (was P3.4) | 🟠 | todo |
| 4 | P4.C Snapshot `track_io` at start; reuse at render (was P3.5) | 🟠 | todo |
| 4 | P4.D `&raw` for `static mut`; `#[unsafe(no_mangle)]`; `sampled()` seed (was P3.6) | 🟠 | todo |
| 4 | P4.E queryId side-map fold by group (was P3.7) | 🟠 | todo |
| 4 | P4.X Cross-version compile checkpoint | — | todo |
| 5 | P5.A `database` GUC → Sighup + not-preloaded notice + restart warning (was P4.1) | 🔴 | todo |
| 5 | P5.B Swallowed-error logging (sync `persist_capture`, catches) | 🟢 | todo |
| 5 | P5.C Configurable ring caps via Postmaster GUCs (shmem-sized) | 🟢 | todo |
| 5 | P5.D Docs: cross-DB queryId, track_io default, nesting, PG14/15 queryId, held cursors | 🟡 | todo |
| 6 | P6.A `pg_stat_statements` join view | 🟢 | todo |
| 6 | P6.B BufferWal → `consolidated_config` pattern | 🟢 | todo |
| 6 | P6.C `capture_stats`: last-drain + gated counters | 🟢 | todo |
| 6 | P6.D De-duplicate sync/async capture scaffolding | 🟢 | todo |
| 7 | P7.A queryId `#[pg_test]`, min_duration gating `#[pg_test]`, parser edge unit tests | 🟢 | todo |
| 7 | P7.B Functional cluster checks: no-preload FATAL gone; auto_explain co-load; nesting | 🔴 | todo |
| 7 | P7.C PG18 `ExecutorStart`-bool verification + honest doc | 🟠 | todo |
| 7 | P7.D Full pg16 e2e + benchmark re-measure; README + design-doc sweep | — | todo |

---

## Phase 1 — Buffer analyzer correctness  *(P1.1 ✅)*
- **P1.2** Parse `local hit/read` into read accounting; surface `I/O Timings: read/write` as
  evidence. At **Gather/Gather Merge** nodes the leader's `Buffers` aggregates workers; keep
  `saturating_sub` so deltas never go negative, and add a test that a parallel node doesn't
  double-count or under-attribute.
- **P1.3** `#[pg_test]`: `track_io=on; work_mem='64kB'`, spilling query → one `MemorySpill` on
  the real operator; plus unit tests for the WAL parent/child subtraction branch.

## Phase 2 — Worker & ring robustness  *(P2.1 ✅)*
- **P2.2** Wrap `aggregate_captures` in `PgTryBuilder` (catches Rust panic on malformed plan
  text) → drop batch + `warning!`, no worker FATAL.
- **P2.3** `drain()`: under the lock copy the populated `Rec` prefix out (swap/`mem::take`-style,
  not full-capacity), release, then build `Capture`s + Strings.
- **P2.4** Add `#[cfg(any(test, feature="pg_test"))] loganalyze_drain_now()` SPI fn (drains +
  aggregates + persists synchronously); async-ring `#[pg_test]`: hook async capture → `drain_now`
  → assert row + `capture_stats` counters advanced.
- **P2.5** Plain unit tests: push/drain round-trip, truncation at caps, overflow `dropped_total`,
  and `aggregate_captures` on a malformed plan returns `[]` without panicking.

## Phase 3 — Nesting & sampling infrastructure  *(the core capture-correctness rework)*
- **P3.A** Add `ExecutorRun_hook` and `ProcessUtility_hook`; a thread-local **nesting stack**:
  push at Run/ProcessUtility entry, pop on exit via a Drop guard (so it decrements on error).
- **P3.B** Capture only at top level (`level==0`) unless `loganalyze.track_nested` (bool, default
  off). Stops double-counting SPI-in-function and inner `EXPLAIN ANALYZE` plans.
- **P3.C** Record the sampling decision as a flag on the nesting stack at ExecutorStart; at
  ExecutorEnd capture iff sampled. Track whether **we** allocated `totaltime` and only
  `InstrEndLoop`/read it then — never finalize foreign instrumentation (auto_explain co-load).
- **P3.D** Early-return when `eflags & EXEC_FLAG_EXPLAIN_ONLY != 0` (one logical unit with P3.C;
  EXPLAIN-only plans must never reach `InstrEndLoop`).
- **P3.E** Skip capture if the transaction is aborting (e.g. `!pg_sys::IsTransactionState()` or
  abort-state check) — never push a snapshot / run SPI during ExecutorEnd-on-abort.
- **P3.X** Container compile `--features pg13/17/18` (this phase adds hooks + may touch cfg).
- **Validation:** `#[pg_test]` EXPLAIN-only not captured; functional: a `DO`/function with inner
  queries captures only the top statement by default; full suite. **Commit per logical unit.**

## Phase 4 — Per-capture safety hardening
- **P4.A** Set `CAPTURING` at the top of `maybe_capture` (both paths).
- **P4.B** RAII guard: `PushActiveSnapshot` → guard whose `Drop` pops; memory-context
  switch/reset also in unwind-safe scope (not trailing statements).
- **P4.C** Snapshot `track_io` at ExecutorStart; reuse at render (no mid-query GUC-flip mismatch).
- **P4.D** Replace `static mut` reads/writes with `&raw const`/`&raw mut`; `#[no_mangle]` →
  `#[unsafe(no_mangle)]` on the bgworker entry; force non-zero `sampled()` seed. **Commit alone**
  (pure compile/lint; lowest risk).
- **P4.E** `aggregate_captures`: fold the queryId side-map by fingerprint / fall back to any
  non-zero id in the group (pure-Rust test).
- **P4.X** Container compile `--features pg13/17/18`.
- **Validation:** sync-path `#[pg_test]` that forces an SPI error inside capture and asserts the
  backend survives (P4.B); full suite. **Commit per unit (P4.D first, then P4.A–C, then P4.E).**

## Phase 5 — Operational correctness & docs
- **P5.A** `loganalyze.database` → `Sighup`; emit a `WARNING` if it changes under SIGHUP that a
  worker restart is required; ensure no FATAL on `CREATE EXTENSION` without preload.
- **P5.B** Log swallowed errors: sync `persist_capture` SPI failure → debug `log!`; keep
  best-effort behavior.
- **P5.C** `SQL_CAP`/`PLAN_CAP`/`RING_CAP` from **Postmaster-context** int GUCs read in
  `init_shmem` (shmem sizing is fixed at startup — must stay Postmaster, document why).
- **P5.D** Docs: query_id is per-DB (cross-DB fingerprint collapse); `track_io` default on;
  nesting semantics; PG14/15 need `compute_query_id=on`; held-cursor capture limitation;
  database-change-needs-restart.
- **Validation:** functional — no-preload `CREATE EXTENSION` doesn't FATAL. **Commit.**

## Phase 6 — Enhancements
- **P6.A** `loganalyze.statements_with_pgss` view (degrades when pgss absent).
- **P6.B** BufferWal thresholds → `consolidated_config` + `ConfigurableAnalyzer`.
- **P6.C** `capture_stats`: `last_drain` ts + sampled-but-gated counter.
- **P6.D** Extract shared render+scratch+`PgTryBuilder` scaffolding into one
  `FnOnce(&[u8])`-taking helper.
- **Validation:** `#[pg_test]`/unit; pg16 join view returns rows. **Commit per item.**

## Phase 7 — Final validation & docs
- **P7.A** Remaining tests: queryId `#[pg_test]` (== pgss), min_duration gating `#[pg_test]`,
  buffer-parser edge unit tests (local/missing-temp/WAL-no-bytes/per-worker).
- **P7.B** Functional cluster checklist: no-preload FATAL gone; auto_explain co-loaded doesn't
  cause double-capture/`InstrEndLoop`; nested-statement capture matches expectation.
- **P7.C** PG18: verify the real pg18 `ExecutorStart` hook ABI vs pgrx 0.18.1's binding; if the
  binding is `void` while PG18 is `bool`, document the limitation and gate the `pg18` claim.
- **P7.D** Full pg16 e2e of every feature + overhead re-measure (hot path unchanged); README +
  `PGRX_EXTENSION_DESIGN.md` sweep; mark plan complete.

## Out of scope
- **PG12** (dropped by pgrx 0.18; EOL). **DSM-registry dynamic ring / custom stats kinds**
  (not surfaced in pgrx-pg-sys 0.18.1 / against the fixed-ring design).
</content>
