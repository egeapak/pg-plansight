# F7 — Exporter derived metrics — Implementation Plan

Derived from `docs/specs/F7_exporter_metrics.md`. Grounded against the actual
exporter and core code (file references below). No storage-schema change.

## 0. Grounding summary (what the code actually exposes)

Sources read:
- `crates/exporter/src/metrics/prometheus_backend.rs` — metric declarations are
  `HistogramVec` / `CounterVec` / `IntCounterVec` / `IntGauge` from the
  `prometheus` crate. There are currently **no Gauge/GaugeVec types imported**.
  Each per-query metric uses the label set
  `&["normalized_query_hash", "database", "query_timestamp"]`. Metrics are
  constructed in `PrometheusBackend::new`, each `clone()`d into
  `registry.register(Box::new(...))?`, and stored as struct fields.
- `crates/exporter/src/metrics/traits.rs` — `MetricsBackend` trait. Per-query
  record methods take `&HashMap<&str, String>` labels + an `f64` value (e.g.
  `record_query_duration`, `record_query_plan_cost`, `record_query_rows_examined`).
- `crates/exporter/src/metrics/mod.rs` — backend factory + module wiring;
  `tests` module is `#[cfg(test)] mod tests`.
- `crates/exporter/src/metrics/tests.rs` — existing test style: build
  `PrometheusBackend::new("test", buckets)`, call record methods, then
  `backend.registry.gather()` and assert `m.get_name() == "<namespace>_<name>"`.
- `crates/exporter/src/collector.rs` — `update_query_metrics(query_hash,
  query_timestamp, database, query: &ProcessedQuery)` is where per-query series
  are emitted. It already has `query.statistics` (a `QueryGroupStatistics`) in
  scope. Per-query emission loops live here.
- `crates/core/src/models.rs` — `QueryGroupStatistics` fields: `count: usize`,
  `total_duration_ms: f64`, `min_duration_ms`, `max_duration_ms`,
  `mean_duration_ms: f64`, `std_dev_ms: f64`, `min_timestamp`, `max_timestamp`,
  `percentiles: PerformancePercentiles`, `hourly_histogram`, `executions:
  Vec<ExecutionRecord>`. `PerformancePercentiles` = `{ p25, p50, p90, p95, p99 }`
  (all `f64`). `ExecutionRecord` = `{ timestamp, duration_ms }` only.

### Feasibility verdict (per the 4 spec metrics)

| # | Metric | Verdict | Reason |
|---|--------|---------|--------|
| 1 | `query_latency_cv` = `std_dev_ms / mean_duration_ms` | **FEASIBLE** | Both fields on `QueryGroupStatistics`. |
| 2 | `query_total_time_share_pct` = `total_duration_ms / SUM(total_duration_ms) * 100` | **FEASIBLE** | `total_duration_ms` present; grand total via pre-pass (Section 4). |
| 3 | `query_rows_per_call` = `rows / calls` | **DEFER emission** | `QueryGroupStatistics` has **no rows-examined aggregate** and `ExecutionRecord` carries only `timestamp`+`duration_ms`. `count` could serve as `calls`, but there is no per-query aggregate rows value (the existing `query_rows_examined` histogram is sourced from the parsed plan node, not aggregate stats). Add the pure helper + unit tests now; do **not** wire emission. |
| 4 | `query_latency_p95_ms` / `query_latency_p99_ms` | **FEASIBLE** | `query.statistics.percentiles.p95` / `.p99` already computed. |

Net: implement and emit **CV, share-of-total, p95, p99**. Implement and
unit-test the **rows_per_call** helper but leave emission deferred (documented
below) until a per-query aggregate rows field exists.

---

## 1. New Prometheus metric declarations

All new series are gauges keyed by the **same per-query label set** as existing
metrics: `&["normalized_query_hash", "database", "query_timestamp"]` (identical
cardinality — no new label dimensions).

### 1a. Import change
In `prometheus_backend.rs`, extend the `prometheus` import to add `GaugeVec`:

```rust
use prometheus::{
    CounterVec, GaugeVec, HistogramOpts, HistogramVec, IntCounterVec, IntGauge, Opts, Registry,
};
```

### 1b. New struct fields on `PrometheusBackend`
```rust
query_latency_cv: GaugeVec,
query_total_time_share_pct: GaugeVec,
query_latency_p95_ms: GaugeVec,
query_latency_p99_ms: GaugeVec,
// rows_per_call deferred — do NOT declare a series yet.
```

### 1c. Construction in `PrometheusBackend::new` (mirror existing GaugeVec style)
```rust
let query_latency_cv = GaugeVec::new(
    Opts::new(
        format!("{}_query_latency_cv", namespace),
        "Coefficient of variation of query latency (stddev/mean); flags unstable/bimodal queries",
    ),
    &["normalized_query_hash", "database", "query_timestamp"],
)?;

let query_total_time_share_pct = GaugeVec::new(
    Opts::new(
        format!("{}_query_total_time_share_pct", namespace),
        "Percent of total DB time across the exported set attributable to this query",
    ),
    &["normalized_query_hash", "database", "query_timestamp"],
)?;

let query_latency_p95_ms = GaugeVec::new(
    Opts::new(
        format!("{}_query_latency_p95_ms", namespace),
        "95th percentile query latency in milliseconds",
    ),
    &["normalized_query_hash", "database", "query_timestamp"],
)?;

let query_latency_p99_ms = GaugeVec::new(
    Opts::new(
        format!("{}_query_latency_p99_ms", namespace),
        "99th percentile query latency in milliseconds",
    ),
    &["normalized_query_hash", "database", "query_timestamp"],
)?;
```

### 1d. Registration (add alongside existing `registry.register` calls)
```rust
registry.register(Box::new(query_latency_cv.clone()))?;
registry.register(Box::new(query_total_time_share_pct.clone()))?;
registry.register(Box::new(query_latency_p95_ms.clone()))?;
registry.register(Box::new(query_latency_p99_ms.clone()))?;
```
Add the four field names to the `Ok(Self { ... })` initializer.

> Final metric names (with default namespace `pg_plansight`):
> `pg_plansight_query_latency_cv`, `pg_plansight_query_total_time_share_pct`,
> `pg_plansight_query_latency_p95_ms`, `pg_plansight_query_latency_p99_ms`.

---

## 2. New trait methods (`traits.rs`)

The existing record methods all take a single `f64`, so these four fit the
established shape. Add to `MetricsBackend` under a new "Derived per-query
metrics" comment block:

```rust
// Derived per-query metrics (F7)
fn set_query_latency_cv(&self, labels: &HashMap<&str, String>, cv: f64);
fn set_query_total_time_share_pct(&self, labels: &HashMap<&str, String>, pct: f64);
fn set_query_latency_p95_ms(&self, labels: &HashMap<&str, String>, p95_ms: f64);
fn set_query_latency_p99_ms(&self, labels: &HashMap<&str, String>, p99_ms: f64);
```

`set_` naming reflects gauge semantics (`.set(value)`), consistent with existing
`set_exporter_up` / `set_memory_usage`.

### 2a. Prometheus impl (`prometheus_backend.rs`)
Each follows the existing label-extraction pattern; e.g.:
```rust
fn set_query_latency_cv(&self, labels: &HashMap<&str, String>, cv: f64) {
    self.query_latency_cv
        .with_label_values(&[
            labels.get("normalized_query_hash").map(|s| s.as_str()).unwrap_or(""),
            labels.get("database").map(|s| s.as_str()).unwrap_or(""),
            labels.get("query_timestamp").map(|s| s.as_str()).unwrap_or(""),
        ])
        .set(cv);
}
```
Replicate for the other three (`query_total_time_share_pct`,
`query_latency_p95_ms`, `query_latency_p99_ms`).

### 2b. OpenTelemetry impl (`otel_backend.rs`)
Add matching method bodies. Minimal-cost option: provide no-op or
observable-gauge stubs consistent with how that backend records existing
per-query values (confirm the file's existing pattern during implementation).
The composite backend (`composite_backend.rs`) must forward the four new methods
to each wrapped backend — mirror an existing forwarded method there.

### 2c. Test no-op backend
`collector.rs`'s `NoopMetrics` impl (and any other `MetricsBackend` impl) must
gain empty bodies for the four new methods, or the workspace won't compile.

---

## 3. Pure helper functions (unit-testable, divide-by-zero guarded)

Place these as **free functions** so they need no registry. Recommended home:
a new private module `crates/exporter/src/metrics/derived.rs` (declared
`mod derived;` in `metrics/mod.rs`), with the helpers `pub(crate)`. Keeping them
out of the backend struct makes them directly callable from both
`tests.rs`/`derived.rs` unit tests and `collector.rs`.

```rust
/// Coefficient of variation = stddev / mean. Guards mean == 0 -> 0.0.
pub(crate) fn coefficient_of_variation(mean: f64, stddev: f64) -> f64 {
    if mean == 0.0 { 0.0 } else { stddev / mean }
}

/// Share of grand total as a percentage. Guards grand_total == 0 -> 0.0.
pub(crate) fn time_share_pct(total: f64, grand_total: f64) -> f64 {
    if grand_total == 0.0 { 0.0 } else { total / grand_total * 100.0 }
}

/// Rows per call. Guards calls == 0 -> 0.0.
/// NOTE (F7): emission deferred — no per-query aggregate rows value exists in
/// QueryGroupStatistics today. Helper + tests land now for when one does.
pub(crate) fn rows_per_call(rows: f64, calls: f64) -> f64 {
    if calls == 0.0 { 0.0 } else { rows / calls }
}
```

Guard uses exact `== 0.0` to match the spec ("`mean == 0`, `grand_total == 0`
→ 0.0"). (Implementation note: this leaves NaN propagation to upstream stats; no
extra `is_finite` handling required by the spec.)

---

## 4. Collector change — grand-total pre-pass for share-of-total

`share-of-total` needs the sum of `total_duration_ms` across the **whole
exported set** before any per-query emit. Currently `process_query_plans`
(crates/exporter/src/collector.rs) iterates `processed_queries` once and calls
`update_query_metrics` per query. Restructure to two passes over the **filtered**
set:

1. **Pre-pass:** iterate `processed_queries`, apply the existing
   `should_include_query` filter, and accumulate
   `grand_total_ms += query.statistics.total_duration_ms`. (Collect the included
   `(stable_hash, query_timestamp, database, query)` tuples into a `Vec` to avoid
   re-filtering, or simply re-check the filter in pass 2 — collecting is
   cleaner and keeps filter logic single-sourced.)
2. **Emit pass:** for each included query call
   `update_query_metrics(..., grand_total_ms)` (extend its signature with
   `grand_total_ms: f64`).

Within `update_query_metrics`, after the existing emissions, build the standard
per-query `labels_map` (hash/database/timestamp — already constructed there) and
emit the derived gauges:

```rust
let stats = &query.statistics;
let cv = derived::coefficient_of_variation(stats.mean_duration_ms, stats.std_dev_ms);
let share = derived::time_share_pct(stats.total_duration_ms, grand_total_ms);
self.metrics.set_query_latency_cv(&labels_map, cv);
self.metrics.set_query_total_time_share_pct(&labels_map, share);
self.metrics.set_query_latency_p95_ms(&labels_map, stats.percentiles.p95);
self.metrics.set_query_latency_p99_ms(&labels_map, stats.percentiles.p99);
// rows_per_call: deferred — no aggregate rows source available.
```

Notes:
- Scope: `grand_total_ms` is the sum across the current
  `process_query_plans` batch (the natural "exported set" boundary here). Same
  scoping is used by the share computation; document this in a code comment.
- Guard: if `grand_total_ms == 0.0`, `time_share_pct` already returns `0.0`.
- The two-pass change is local to `process_query_plans` /
  `update_query_metrics`; no other call sites change.

---

## 5. Test plan (positive + negative)

### 5a. Helper unit tests — in `derived.rs` (`#[cfg(test)] mod tests`) or `tests.rs`
Plain unit tests, no feature gate (helpers are pure, not Prometheus-specific):

- `test_cv_basic` — `coefficient_of_variation(100.0, 50.0)` → `0.5`.
- `test_cv_zero_mean_guard` — `coefficient_of_variation(0.0, 50.0)` → `0.0` (guard).
- `test_cv_zero_stddev` — `coefficient_of_variation(100.0, 0.0)` → `0.0` (stable query).
- `test_time_share_basic` — `time_share_pct(25.0, 100.0)` → `25.0`.
- `test_time_share_zero_grand_guard` — `time_share_pct(25.0, 0.0)` → `0.0` (guard).
- `test_time_share_full` — `time_share_pct(100.0, 100.0)` → `100.0`.
- `test_rows_per_call_basic` — `rows_per_call(1000.0, 10.0)` → `100.0`.
- `test_rows_per_call_zero_calls_guard` — `rows_per_call(1000.0, 0.0)` → `0.0` (guard).

Use exact `assert_eq!` for clean values; these are all representable.

### 5b. Render test (feasible — follows existing `tests.rs` style)
Add under `#[cfg(feature = "prometheus")]` in `metrics/tests.rs`:

- `test_prometheus_backend_derived_metrics`:
  1. `let backend = PrometheusBackend::new("test", vec![1.0]).unwrap();`
  2. Build the standard per-query `labels` HashMap (hash/database/query_timestamp).
  3. Call `backend.set_query_latency_cv(&labels, 0.5)`,
     `set_query_total_time_share_pct(&labels, 25.0)`,
     `set_query_latency_p95_ms(&labels, 12.0)`,
     `set_query_latency_p99_ms(&labels, 30.0)`.
  4. `let metrics = backend.registry.gather();`
  5. Assert presence of each name:
     - `test_query_latency_cv`
     - `test_query_total_time_share_pct`
     - `test_query_latency_p95_ms`
     - `test_query_latency_p99_ms`
  6. (Optional) downcast one gauge value via the gathered metric families to
     assert the set value, mirroring `test_prometheus_backend_slow_queries`'s
     `get_field_type()` style — assert `MetricType::GAUGE`.

### 5c. Negative / guard coverage at the metric layer
- `test_derived_metrics_zero_guards` (prometheus-gated): call
  `set_query_latency_cv(&labels, coefficient_of_variation(0.0, 50.0))` and
  `set_query_total_time_share_pct(&labels, time_share_pct(25.0, 0.0))`; gather and
  assert both series exist with value `0.0` (guards don't break emission).

### 5d. Deferred metric
No render/emission test for `query_rows_per_call` (not emitted). The helper's
unit tests (`test_rows_per_call_*`) are the only coverage until a rows aggregate
exists.

---

## 6. Implementation order / checklist

1. Add `derived.rs` with the three pure helpers + their unit tests (Section 3, 5a).
2. Wire `mod derived;` into `metrics/mod.rs`.
3. Extend `MetricsBackend` trait with the four `set_*` methods (Section 2).
4. Implement them in `prometheus_backend.rs` (import `GaugeVec`, declare/register
   4 series, impl methods) (Sections 1, 2a).
5. Implement/forward in `otel_backend.rs` and `composite_backend.rs`; add no-op
   bodies to `NoopMetrics` in `collector.rs` (Sections 2b, 2c).
6. Two-pass grand-total + derived emission in `collector.rs` (Section 4).
7. Add render + guard tests in `metrics/tests.rs` (Sections 5b, 5c).
8. `cargo fmt --all` and
   `cargo clippy --workspace --all-features --all-targets -- -D warnings`; then
   `cargo test`.

## 7. Out of scope / explicitly deferred
- `query_rows_per_call` **emission** (helper + unit tests only) until
  `QueryGroupStatistics` carries a per-query aggregate rows-examined value.
- No changes to histogram bucket config, state schema, or label cardinality.
