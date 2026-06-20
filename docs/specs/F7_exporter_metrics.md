# F7 — Exporter derived metrics

## Goal
Add high-value **derived** per-query metrics to the Prometheus/OTel exporter,
computed from stats the collector already has. No storage-schema change.

## Files (confirm exact names during planning)
- `crates/exporter/src/metrics/prometheus_backend.rs` — register/emit new series.
- `crates/exporter/src/metrics/traits.rs` — extend the backend trait only if a
  new method is genuinely needed; prefer reusing existing record methods.
- `crates/exporter/src/collector.rs` — compute the derived values; needs a
  full-set pass for share-of-total.
- `crates/exporter/src/metrics/tests.rs` (or backend unit tests) — coverage.

## Metrics
1. **Coefficient of variation** — `query_latency_cv = stddev_ms / mean_ms`
   (Gauge per query). Flags unstable / bimodal queries. Trivial; no new state.
2. **Share of total DB time** — `query_total_time_share_pct =
   query.total_time_ms / SUM(all queries.total_time_ms) * 100` (Gauge). Requires
   a pre-pass to sum total time across the exported set, then per-query emit.
3. **Rows per call** — `query_rows_per_call = rows_examined / calls` (Gauge),
   from existing row data if available; otherwise skip gracefully.
4. **Percentile gauges** — if the collector already computes percentiles
   (`PerformancePercentiles`: p95/p99), expose `query_latency_p95_ms` /
   `query_latency_p99_ms` (Gauge). If percentiles aren't available at export
   time, defer this sub-item.

Keep label cardinality identical to existing per-query metrics (reuse the same
label set: query id/fingerprint, database, etc.).

## Implementation notes
- Put the CV and share math in a small pure helper so it is unit-testable
  without a running registry, e.g.
  `fn coefficient_of_variation(mean: f64, stddev: f64) -> f64` and
  `fn time_share_pct(total: f64, grand_total: f64) -> f64`.
- Guard divide-by-zero (`mean == 0`, `grand_total == 0` → 0.0).

## Tests
- `test_cv_basic` — mean=100, stddev=50 → 0.5; mean=0 → 0.0 (negative/guard).
- `test_time_share` — total=25, grand=100 → 25.0; grand=0 → 0.0.
- `test_rows_per_call` — rows=1000, calls=10 → 100.0; calls=0 → 0.0.
- If feasible, an integration-style test that records a couple of queries and
  asserts the rendered Prometheus text contains the new metric names.
