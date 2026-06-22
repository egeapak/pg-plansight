# F9 — Exporter: remove `query_timestamp` label; export first/last seen as gauges

## Background (verified by inspection)
- The `query_timestamp` metric label is set from each batch's `min_timestamp`
  (recomputed every collection cycle) and is **consumed by nothing**. A per-query
  timestamp in a label is a Prometheus anti-pattern and mints a fresh series
  every cycle → unbounded cardinality.
- The state DB persists a stable `first_seen_at` (preserved via COALESCE) and a
  `last_seen_at` per query hash. `first_seen_at` currently has **no consumer**;
  `last_seen_at` is used only by the retention `DELETE`.

## Part A — drop `query_timestamp` from all per-query metrics
Time is the series' own axis; remove the label everywhere it appears. Keep all
other labels (`status`, `threshold`, `node_type`, `scan_type`, `join_type`,
`database`, `normalized_query_hash`).

### `metrics/prometheus_backend.rs`
Update both the declaration label arrays **and** the matching
`with_label_values(...)` calls (they must stay the same arity, or the prometheus
crate panics at runtime — the render tests catch this):

| Metric | New label set |
|--------|---------------|
| `query_duration` | `["normalized_query_hash","database"]` |
| `query_executions` | `["normalized_query_hash","database","status"]` |
| `slow_queries` | `["database","threshold"]` |
| `query_plan_cost` | `["normalized_query_hash","database"]` |
| `query_rows_examined` | `["normalized_query_hash","database"]` |
| `database_avg_duration` | `["database"]` |
| `database_queries_per_second` | `["database"]` |
| `database_unique_queries` | `["database"]` |
| `plan_node_types` | `["node_type","database"]` |
| `scan_types` | `["scan_type","database"]` |
| `join_types` | `["join_type","database"]` |
| `query_latency_cv` / `_total_time_share_pct` / `_p95_ms` / `_p99_ms` (F7) | `["normalized_query_hash","database"]` |

In each method body, delete the `labels.get("query_timestamp")...` element from
the `with_label_values(&[...])` array.

### `metrics/otel_backend.rs`
No declaration change — `labels_to_attributes` iterates the map. It drops
`query_timestamp` automatically once the collector stops inserting it.

### `metrics/composite_backend.rs`
Only a test inserts `query_timestamp` (line ~177) — remove that insert.

### `collector.rs`
- `update_query_metrics`: remove the `query_timestamp: &str` param, the
  `let _labels` line, and every `labels_map.insert("query_timestamp", …)`.
- `update_plan_metrics` / `count_node_metrics`: remove the `timestamp: &str`
  param and the `query_timestamp` inserts.
- `process_query_plans`: stop computing `query_timestamp`
  (`format_timestamp_for_labels(min_timestamp)`) and stop passing it.
  `format_timestamp_for_labels` may become unused → remove it.

### `metrics/tests.rs`
Remove every `labels.insert("query_timestamp", …)` (~10 sites). Existing
render/guard tests then assert the (now 2-label) series still render.

## Part B — proper first/last-seen gauges (give the persisted timestamps a consumer)
Export the *stable* timestamps as gauge **values** (unix epoch seconds), one
series per `(normalized_query_hash, database)` — the idiomatic, bounded way.

### `state.rs`
Change `record_query_hash` to return the persisted timestamps:
```rust
pub fn record_query_hash(&self, hash: &str, normalized_query: &str)
    -> Result<(DateTime<Utc>, DateTime<Utc>)> // (first_seen, last_seen)
```
Use `RETURNING first_seen_at, last_seen_at` (SQLite ≥ 3.35) on the existing
`INSERT OR REPLACE`, parse the RFC3339 strings back to `DateTime<Utc>`. Keep the
COALESCE-preserve semantics. Update the existing
`test_record_query_hash_preserves_first_seen_at` if its call shape changes.

### `metrics/prometheus_backend.rs`
Add two `GaugeVec`, registered, with set methods:
```
{ns}_query_first_seen_seconds  {normalized_query_hash, database}   # unix epoch
{ns}_query_last_seen_seconds   {normalized_query_hash, database}   # unix epoch
```

### `metrics/traits.rs`
Add `set_query_first_seen_seconds(&self, labels, secs: f64)` and
`set_query_last_seen_seconds(&self, labels, secs: f64)`.

### `metrics/otel_backend.rs` / `composite_backend.rs`
Implement (real `f64_gauge`) / forward the two new methods, mirroring the F7
gauges.

### `collector.rs`
In `update_query_metrics`, capture the return of `record_query_hash` and set the
two gauges with the per-query `{hash, database}` labels:
```rust
let (first_seen, last_seen) = self.state_manager.record_query_hash(&stable_hash, query.normalized_query())?;
// after building labels_map (hash + database):
self.metrics.set_query_first_seen_seconds(&labels_map, first_seen.timestamp() as f64);
self.metrics.set_query_last_seen_seconds(&labels_map, last_seen.timestamp() as f64);
```
(`record_query_hash` is currently called at collector.rs:475 with its result
discarded — thread the returned tuple through.)

## Tests
- Update `metrics/tests.rs` for the new label sets (Part A).
- Add a render test asserting `…_query_first_seen_seconds` and
  `…_query_last_seen_seconds` appear and are `GAUGE`.
- `state.rs`: extend the existing test (or add one) to assert the returned
  `first_seen` is stable across re-inserts and `last_seen` advances.

## Verify
`cargo fmt -p pg-plansight-exporter`,
`cargo clippy -p pg-plansight-exporter --all-features --all-targets -- -D warnings`,
`cargo test -p pg-plansight-exporter --all-features` (render tests catch any
label/`with_label_values` arity mismatch).

## Docs
Update `docs/ANALYZERS_AND_METRICS.md` §3: drop the `query_timestamp` mention,
note metrics are keyed by `(normalized_query_hash, database)`, and document the
two new first/last-seen gauges with a PromQL example (e.g. query age =
`time() - pg_plansight_query_first_seen_seconds`).
