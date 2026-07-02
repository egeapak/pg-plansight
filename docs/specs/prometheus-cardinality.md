# Spec: Bounded Prometheus per-query series cardinality

Status: implemented (`crates/exporter/src/metrics/prometheus_backend.rs`,
`config.rs`, `metrics/mod.rs`, `main.rs`)

## Problem

The exporter emits per-query metrics labeled by `normalized_query_hash`
(9 two-label vectors: `[normalized_query_hash, database]`, plus the three-label
`query_executions_total` `[normalized_query_hash, database, status]`). The
Prometheus client registry never evicts label sets, so a long-running daemon
that observes many distinct query shapes grows series (memory + scrape cost)
without bound until the process restarts. SQLite retention trims *stored* state
but not the in-process registry.

## Goals

1. Cap the number of distinct `normalized_query_hash` values tracked; evict the
   least-recently-used hash's series when the cap is exceeded.
2. Config-driven, with a default and an explicit "unlimited" escape hatch that
   preserves the prior unbounded behavior.
3. Never touch global/fixed-cardinality metrics (up, memory, parse errors,
   per-database aggregates).
4. No new external dependencies (LRU implemented inline).

## Design

`metrics.max_query_cardinality: usize` (config, default **10000**; `0` =
unlimited → limiter skipped entirely, no lock taken).

`QueryCardinalityLimiter` behind a `Mutex` on `PrometheusBackend`:

- `order: BTreeMap<u64, String>` — recency tick → hash; first entry is the LRU.
- `entries: HashMap<String, QueryHashEntry>` — per hash: its current recency
  tick and the exact label tuples emitted (`databases`, `(database, status)`),
  so eviction removes precisely the series that were created.
- `tick: u64` — monotonic recency counter.

Every per-query record method calls `note_query(hash, database, status)` before
emitting. `touch`:

- Known hash → move to MRU (update `order`), remember any newly seen labels.
- New hash → if at cap, `evict_lru()` first; the current hash is inserted as MRU
  and therefore never the one evicted.

`remove_query_series` calls `remove_label_values` for the evicted hash across all
9 two-label vecs and the executions counter, using the tracked tuples so the
series actually disappear from `/metrics`.

Because the collector emits all of a query group's metrics consecutively, the
current hash is touched to MRU on its first metric, so no mid-group eviction of
the in-flight hash can occur.

## Known trade-offs (documented)

- **Counter reset on re-admission**: an evicted hash seen again is re-admitted as
  a fresh series; its counters restart from zero (a gap in the series timeline).
  This is inherent to any bounded-cardinality scheme and acceptable — churned-out
  query shapes are by definition low-frequency.
- `note_query` takes the mutex once per emitted metric per query per cycle. This
  is O(metrics) short critical sections; negligible next to serialization/scrape.

## Acceptance criteria & verification (`prometheus_backend.rs` cardinality_tests)

- `evicts_least_recently_used_query_hash_when_cap_exceeded` — cap 2, a/b/c →
  a evicted.
- `touching_a_hash_refreshes_its_recency` — cap 2, a/b/touch-a/c → b evicted, a
  survives (the LRU recency-refresh semantic).
- `eviction_removes_series_across_all_per_query_vecs` — evicted hash gone from
  duration, executions, and gauge families.
- `evicted_hash_can_be_readmitted` — cap 1, a/b/a → a re-admitted, b evicted.
- `unlimited_cap_keeps_all_query_hashes` — cap 0 evicts nothing.
