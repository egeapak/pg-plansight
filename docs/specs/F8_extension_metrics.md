# F8 — Extension accumulated metrics

## Goal
Add accumulated, queryable signals to the in-database extension. Two layers:
- **Pure-Rust aggregation math** in `aggregate.rs` (`StatRow`), unit-testable
  with `cargo test` (no Postgres) — this is where we add derived/accumulated
  fields.
- **Surfacing** via the SQL view(s) + any new GUC, verified with
  `cargo pgrx test --no-default-features --features pg16`.

## Confirmed structure
- `aggregate.rs` is "Pure Rust: no Postgres calls" → directly unit-testable.
- `StatRow` fields today: `calls`, `total_time_ms`, `sum_sq_time_ms`,
  `min_time_ms`, `max_time_ms`, `first_seen_epoch`, `last_seen_epoch`, plus
  plan/analysis. (Planning agent: confirm `Capture` fields — whether WAL/buffer
  counters and a plan identifier are already captured by the hook.)
- GUCs live in `lib.rs` via `GucRegistry::define_*`. Existing examples:
  `plansight.min_duration_ms` (f64), `plansight.track_io` (bool).

## Features
1. **Coefficient of variation (derived, pure Rust)** — add a method/field
   computing `stddev / mean` from existing `calls`, `total_time_ms`,
   `sum_sq_time_ms`. (stddev = sqrt(max(0, sum_sq/n - mean^2))). Expose in the
   summary view as `cv`. Fully unit-testable.
2. **SLO-breach counter** — new GUC `plansight.slo_threshold_ms` (f64, default
   0 = disabled). When > 0, the capture path counts executions whose duration
   exceeds it into a new `StatRow.slo_breaches: i64` (accumulated, mergeable).
   Surface `slo_breaches` + `slo_breach_pct` in the view.
   - Pure-Rust part: the counting/merge logic given a threshold and durations.
   - pgrx part: reading the GUC + column in the view.
3. **Cumulative WAL bytes** *(only if `Capture` already carries WAL bytes; else
   defer)* — add `StatRow.total_wal_bytes: i64`, summed across captures, with
   `wal_bytes_per_call` in the view. Planning agent confirms availability.
4. **Distinct-plan / plan-change tracking** *(if a plan identifier exists)* —
   accumulate a count of distinct plan hashes per fingerprint and a
   `last_plan_change_epoch`. If no stable plan id is captured, defer and note it.

Start with #1 and #2 (always feasible); add #3/#4 only if the captured data
supports them without a capture-path redesign.

## Tests
- Pure Rust (`cargo test -p` via `#[cfg(test)] mod tests` in `aggregate.rs`):
  - `test_cv_computation` — known durations → expected CV; single call /
    zero-mean → 0.0 (negative/guard).
  - `test_slo_breach_count` — durations `[10, 200, 300]`, threshold 100 → 2
    breaches; threshold 0 → 0 (disabled, negative).
  - `test_slo_breach_merge` — merging two partial rows sums breaches.
- pgrx (`#[pg_test]`, run under `cargo pgrx test` when a cluster is available):
  - create extension, ingest sample rows, `SELECT cv, slo_breaches FROM
    plansight.statements_summary` returns expected values.

## Build/verify
- `cd crates/pg_extension && cargo pgrx test --no-default-features --features pg16`
- Pure-Rust tests also run via the normal `cargo test` inside the crate
  (`cargo test --no-default-features --features pg16 --lib`) — confirm during
  implementation.

## Risk
This crate cannot be built by the root workspace; it requires the pgrx
toolchain (now installed) and a PG16 cluster (now initialized). If
`cargo pgrx test` is unavailable, the pure-Rust aggregation tests still validate
the accumulation math; the pgrx-surfacing is then code-reviewed but unrun.
