# Analyzers & Metrics Reference

This document describes the analysis findings and metrics that Plansight
produces, with worked examples. It covers three surfaces:

1. **Plan analyzers** — findings derived from the `EXPLAIN` tree (shown in the
   TUI and the JSON export).
2. **SQL anti-patterns** — smells detected from the query text.
3. **Accumulated metrics** — time-series exported by the daemon and the
   in-database extension.

For each item below: **what it flags**, an **example** that triggers it, and the
**suggested fix** Plansight emits.

> To get the per-node execution data these analyzers rely on, capture plans with
> `EXPLAIN (ANALYZE, BUFFERS)` — or, with `auto_explain`, set
> `auto_explain.log_analyze = on` and `auto_explain.log_buffers = on`.

---

## 1. Plan analyzers

Each analyzer emits zero or more **findings**. A finding has a severity
(`Low`/`Medium`/`High`/`Critical`), a title, a description, an actionable
suggestion, the affected node(s), and quantitative `evidence`.

### 1.1 SortMemoryAnalyzer — disk spills & hash-batch overflow

**Flags:** sorts that spilled to disk (`Sort Space Type: Disk` or an
`external merge` sort method) and hash operations that overflowed `work_mem`
into multiple batches (`Batches > 1`). Only *actual* spills are reported, so
findings don't depend on assuming the server's `work_mem`.

**Example — a sort that spills to disk:**

```
Sort  (cost=12500.00..12750.00 rows=100000 width=40) (actual time=210.4..240.1 rows=100000 loops=1)
  Sort Key: created_at
  Sort Method: external merge  Disk: 12000kB
```

Plansight maps `Sort Method: external merge` / `Sort Space Type: Disk` and the
`Sort Space Used` (12000 kB) to a **MemorySpill** finding:

> **Sort spilled to disk** (High)
> Operation 'Sort' performed an external (disk-based) sort using 12000 kB. It
> exceeded work_mem (4096 kB) and spilled to disk.
> **Fix:** Raise work_mem to fit the sort in memory, add an index providing the
> required sort order, or reduce the number of rows before sorting.

**Example — a hash join that overflows into batches:**

```
Hash  (actual time=...) 
  Buckets: 4096  Batches: 16  Memory Usage: 3072kB
```

`Batches: 16` → **HashJoinMemorySpill** (High, because batches > 8): *"used 16
hash batches (> 1), indicating the hash table did not fit in work_mem … Raise
work_mem so the hash table fits in a single batch."*

### 1.2 FilterEfficiencyAnalyzer — rows read then thrown away

**Flags:** nodes that inspect many rows and discard most of them in a filter
(low selectivity), weak index conditions that recheck many rows, and expensive
join filters. Selectivity = `rows_kept / (rows_kept + rows_removed)`.

**Example — a sequential scan that filters away 99.8% of rows:**

```
Seq Scan on orders  (actual time=0.02..95.3 rows=2000 loops=1)
  Filter: (status = 'cancelled')
  Rows Removed by Filter: 998000
```

`actual rows = 2000`, `Rows Removed by Filter = 998000` → selectivity 0.2% →
**ExcessiveRowProcessing** (High):

> **Filter discards most inspected rows**
> Operation 'Seq Scan on orders' discarded 998000 rows in a filter, keeping only
> 2000 (0.2% selectivity).
> **Fix:** Add or extend an index covering the filter predicate so these rows
> are never read, e.g. `CREATE INDEX ON orders (status);`

Related findings: `Rows Removed by Index Recheck` → **PoorIndexSelectivity**
(the index condition is imprecise); `Rows Removed by Join Filter` →
**IneffectiveJoinAlgorithm**.

### 1.3 IndexEfficiencyAnalyzer — stale visibility map & lossy bitmaps

**Flags:**
- An **index-only scan** doing many `Heap Fetches` — the visibility map is
  stale, so the scan still visits the heap.
- A **bitmap heap scan** that went **lossy** (`Heap Blocks: lossy`) because the
  bitmap outgrew `work_mem`.

**Example — index-only scan defeated by heap fetches:**

```
Index Only Scan using orders_pkey on orders  (actual time=... rows=210000 loops=1)
  Heap Fetches: 200000
```

→ **IndexOnlyScanHeapFetches** (High): *"did 200000 heap fetches during an
index-only scan. A stale visibility map forces the scan to visit the heap …
Run VACUUM (or VACUUM ANALYZE) on the table to refresh the visibility map."*

**Example — lossy bitmap:**

```
Bitmap Heap Scan on events  (actual time=...)
  Heap Blocks: exact=1000 lossy=5000
```

→ **MemorySpill**: *"produced 5000 lossy bitmap blocks (83% of 6000 total). The
bitmap exceeded work_mem and became lossy … Raise work_mem so the bitmap stays
exact, or add a more selective index."*

### 1.4 PlanShapeAnalyzer — structural metrics & cost hotspots

**Always emits metrics:** `node_count`, `max_depth`, `scan_count`, `join_count`,
`aggregate_count`, `sort_count`, `most_expensive_self_cost`, `total_plan_cost`,
`dominant_cost_fraction`.

**Flags:**
- **ComplexPlanShape** — `max_depth ≥ 12` or `node_count ≥ 40` (often stacked
  CTEs/subqueries). *"consider simplifying or flattening CTEs/subqueries, or
  splitting the query."*
- **ExpensiveOperation** — one node's **self-cost** (its cost minus its
  children's) is ≥ 70% of the whole plan's cost. This is the single hotspot to
  tune. *"Operation 'X' accounts for 95% of the plan's total cost … Focus tuning
  on this node."*

> Self-cost is used (not inclusive cost) so the root isn't trivially always the
> "dominant" node — the finding points at the operation actually doing the work.

### 1.5 EstimationHealthAnalyzer — planner estimate accuracy

Complements the per-node `RowEstimationAnalyzer` with a **whole-plan**
classification, comparing `estimated_rows` to `actual_rows` (a misestimate ratio
outside the band `[0.5, 2.0]` is "significant").

**Flags:**
- **EstimationPatternSystematic** — ≥ 4 measured nodes, ≥ 80% skewing the same
  direction with an average misestimate ≥ 3×. Points at stale/missing stats.
  *"the planner under-estimates by ~100x on average … Run ANALYZE on the
  involved tables; for correlated columns consider CREATE STATISTICS."*
- **EstimationPatternOutlier** — exactly one node off by ≥ 50× while the rest are
  accurate. Points at localized data skew or correlated predicates.
  *"Consider CREATE STATISTICS on the correlated columns, or a partial/expression
  index."*

**Example — systematic under-estimation:**

```
... rows=100 ... (actual ... rows=10000 ...)   <- repeated across the plan
```

Every node estimates 100 but returns 10000 (ratio 100×) → systematic finding,
recommending `ANALYZE`.

---

## 2. SQL anti-patterns (AntiPatternAnalyzer)

Detected from the query text via the SQL parser. Each returns a `kind`, a
`severity`, a `message`, and a `suggestion`. Available programmatically:

```rust
use pg_plansight_core::AntiPatternAnalyzer;

let findings = AntiPatternAnalyzer::new()
    .analyze("SELECT * FROM users WHERE lower(email) = 'a@b.com' OFFSET 100000");
for f in &findings {
    println!("[{:?}] {} — {}", f.severity, f.message, f.suggestion);
}
```

| Kind | Example query | Why / suggested rewrite |
|------|---------------|-------------------------|
| `SelectStar` | `SELECT * FROM users` | Fetches every column. List only the columns you need (enables index-only scans). |
| `LeadingWildcardLike` | `... WHERE name LIKE '%abc'` | A leading wildcard can't use a b-tree index. Use a `pg_trgm` index or restructure. |
| `FunctionWrappedPredicate` | `... WHERE lower(email) = 'a@b.com'` | The column is wrapped in a function/cast, preventing plain index use. Add an expression index, or move the function to the constant side. |
| `NotIn` | `... WHERE id NOT IN (1,2,3)` | `NOT IN` is NULL-unsafe and often slow. Prefer `NOT EXISTS`. |
| `OffsetWithoutLimit` | `... ORDER BY id OFFSET 50` (or a deep `OFFSET`) | OFFSET scans and discards rows. Use keyset ("seek") pagination. |
| `UnionInsteadOfUnionAll` | `SELECT ... UNION SELECT ...` | `UNION` does a dedup sort/hash. Use `UNION ALL` when duplicates are impossible. |
| `CorrelatedSubquery` | `... WHERE EXISTS (SELECT 1 FROM o WHERE o.uid = u.id)` | Correlated subquery may re-run per outer row. Consider a join or `LATERAL`. |

Notes: `LIKE 'abc%'` (trailing wildcard) and `UNION ALL` are **not** flagged.
`FunctionWrappedPredicate` and `CorrelatedSubquery` are heuristic. Unparseable
SQL yields no findings (never errors).

---

## 3. Accumulated metrics (exporter)

The `pg-plansight-exporter` daemon emits these **derived per-query gauges**
(namespace defaults to `pg_plansight`), keyed by `(normalized_query_hash,
database)`. Time is the time-series' own axis, so it is **not** carried in a
label — a per-query timestamp label would mint a fresh series every scrape and
blow up cardinality.

| Metric | Meaning |
|--------|---------|
| `pg_plansight_query_latency_cv` | Coefficient of variation (`stddev/mean`). High = unstable/bimodal latency (e.g. cache hits vs cold runs, plan flips). |
| `pg_plansight_query_total_time_share_pct` | Percent of total DB time (across the exported set) attributable to this query — the "top by total time" ranking. |
| `pg_plansight_query_latency_p95_ms` | 95th percentile latency. |
| `pg_plansight_query_latency_p99_ms` | 99th percentile latency. |
| `pg_plansight_query_first_seen_seconds` | Unix epoch seconds when this query fingerprint was **first** seen (stable across re-sightings). |
| `pg_plansight_query_last_seen_seconds` | Unix epoch seconds when this query fingerprint was **last** seen. |

The first/last-seen gauges export the stable timestamps the exporter persists in
its state DB, one series per `(normalized_query_hash, database)`. They give a
bounded, idiomatic way to reason about query age and recency.

A `normalized_query_hash` is not readable on its own, so the exporter also emits
`pg_plansight_query_info`, an *info metric* whose value is always `1` and whose
`query_shape` label carries the normalised statement. Join it on both labels:

```promql
pg_plansight_query_total_time_share_pct
  * on (normalized_query_hash, database) group_left(query_shape)
    pg_plansight_query_info
```

Only text that really went through normalisation is published; a statement
`sqlparser` could not parse becomes `<unparsed>` rather than putting its literals
into a label. See [METRICS.md](METRICS.md) for the full rules.

**Example alerts / dashboards (PromQL):**

```promql
# Queries with unstable latency (CV > 1 means stddev exceeds the mean)
pg_plansight_query_latency_cv > 1

# Any single query consuming more than 40% of total DB time
pg_plansight_query_total_time_share_pct > 40

# Tail-latency SLO breach
pg_plansight_query_latency_p99_ms > 100

# How long ago each query fingerprint was first seen (query age, in seconds)
time() - pg_plansight_query_first_seen_seconds
```

All derived values are divide-by-zero guarded (a query with zero mean or an
empty set yields `0.0`).

---

## 4. Accumulated metrics (in-database extension)

The `pg_plansight` extension accumulates per-fingerprint statistics inside the
server and exposes them through SQL views. Two accumulated signals were added:

### 4.1 Coefficient of variation (`cv`) and `stddev_time_ms`

Derived from the stored `calls`, `total_time_ms`, and `sum_sq_time_ms`
(population stddev = `sqrt(E[X²] − E[X]²)`; `cv = stddev / mean`). High `cv`
flags queries with unstable latency.

```sql
-- Unstable queries (stddev exceeds the mean), worst first
SELECT normalized_query, calls, mean_time_ms, stddev_time_ms, cv
FROM plansight.statements_summary
WHERE cv > 1
ORDER BY cv DESC;
```

### 4.2 SLO-breach counter (`slo_breaches`, `slo_breach_pct`)

Set the new GUC to a latency budget (milliseconds); executions slower than it are
counted per fingerprint. `0` (the default) disables counting.

```ini
# postgresql.conf (or: SET plansight.slo_threshold_ms = 100;)
plansight.slo_threshold_ms = 100
```

```sql
-- Queries breaching the 100ms SLO most often
SELECT normalized_query, calls, slo_breaches, slo_breach_pct
FROM plansight.statements_summary
WHERE slo_breaches > 0
ORDER BY slo_breach_pct DESC;
```

`slo_breaches` is additive across flushes (the upsert sums partial counts), so
the value is a running total since the last `plansight.reset()`.

> Deferred (not yet captured): cumulative WAL bytes and distinct-plan/plan-change
> tracking — the executor hook doesn't yet record WAL counters or a stable plan
> identifier, so these are documented in the spec but not implemented.

