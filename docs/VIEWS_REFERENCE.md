# SQL objects reference (pg_loganalyze extension)

Every table, view, and column in the `loganalyze` schema, with example rows.
The objects are created by `crates/pg_extension/sql/schema.sql` (shipped via
`extension_sql_file!` at `lib.rs:19`); `statements_with_pgss` is created at
runtime by `loganalyze_pgss_view()`. (The TUI does **not** read these — it is a
standalone log-file analyzer sharing only the `pg_loganalyze_core` library.)

The examples below use one consistent dataset of two query groups:

- **A** — a point query `SELECT * FROM orders WHERE id = $1`, 2 captured calls.
- **B** — an OLAP query `SELECT customer_id, count(*) FROM orders GROUP BY customer_id`, 1500 captured calls.

---

## Tables

### `loganalyze.statements`

The cumulative per-fingerprint store (one row per distinct query shape), folded
in by a single UPSERT per ingest batch. This is the source of truth; the views
below derive from it.

| Column | Type | Description |
|--------|------|-------------|
| `fingerprint` | `text` **PK** | Stable fingerprint of the normalized query (DB-agnostic grouping key from the core normalizer). |
| `query_id` | `bigint` | Core `compute_query_id` of the representative execution, to join `pg_stat_statements`. `NULL` on PG13, when `compute_query_id` is off, or for log-mode ingest. Meaningful only within the DB that produced the representative plan (queryId embeds relation OIDs). |
| `normalized_query` | `text` | Query with literals parameterized (`id = $1`). |
| `representative_sql` | `text` | The slowest-seen example query, raw. Pretty form is derived on demand via `loganalyze_format()`. |
| `representative_plan` | `text` | Raw `EXPLAIN` text of the slowest-seen execution. Empty for stats-only (`capture_plan=off`) captures. |
| `calls` | `bigint` | Cumulative count of **captured** executions (additive). With `sample_rate<1` this is the sampled count, not the true total. |
| `total_time_ms` | `double precision` | Sum of captured durations (additive). |
| `sum_sq_time_ms` | `double precision` | Sum of squared durations — lets the summary view derive a population stddev without storing every execution. |
| `min_time_ms` | `double precision` | Fastest captured execution (merged with `LEAST`). |
| `max_time_ms` | `double precision` | Slowest captured execution (merged with `GREATEST`); also gates when the representative plan/analysis is replaced. |
| `first_seen` | `timestamptz` | Earliest captured execution (merged with `LEAST`). |
| `last_seen` | `timestamptz` | Latest captured execution (merged with `GREATEST`). |
| `complexity` | `jsonb` | Core complexity analysis of the representative plan (see [JSONB columns](#jsonb-columns)). `NULL` for stats-only captures. |
| `metadata` | `jsonb` | Query metadata (operation, table/column/function references, hints). `NULL` for stats-only. |
| `plan_analysis` | `jsonb` | Combined analyzer findings (the data the TUI shows). `NULL` for stats-only. |

Example (scalar columns):

```
 fingerprint |      query_id       |        normalized_query         | calls | total_time_ms | sum_sq_time_ms | min_time_ms | max_time_ms |        first_seen         |         last_seen
-------------+---------------------+---------------------------------+-------+---------------+----------------+-------------+-------------+---------------------------+---------------------------
 a1b2c3d4    | 4823561902837461023 | SELECT * FROM orders WHERE id=$1 |     2 |          31.0 |          530.5 |        10.5 |        20.5 | 2025-06-25 00:11:04+00    | 2025-06-25 00:41:55+00
 e5f6a7b8    | 9182734615092837461 | SELECT customer_id, count(*) ...|  1500 |       30000.0 |       615360.0 |        16.0 |        42.0 | 2025-06-25 00:02:18+00    | 2025-06-25 01:59:50+00
```

### `loganalyze.query_histogram`

Per-fingerprint execution histogram, bucketed by hour. Additive across ingests;
the time series behind `query_timeline`, the TUI timeline chart, and (Phase 3)
regression detection.

| Column | Type | Description |
|--------|------|-------------|
| `fingerprint` | `text` (FK → `statements`, `ON DELETE CASCADE`) | Query group. |
| `bucket` | `timestamptz` **PK** part | Hour-truncated bucket start. |
| `calls` | `bigint` | Executions in this fingerprint+bucket. |
| `total_time_ms` | `double precision` | Sum of durations in the bucket. |
| `min_time_ms` | `double precision` | Min duration in the bucket. |
| `max_time_ms` | `double precision` | Max duration in the bucket. |

Example:

```
 fingerprint |         bucket          | calls | total_time_ms | min_time_ms | max_time_ms
-------------+-------------------------+-------+---------------+-------------+-------------
 a1b2c3d4    | 2025-06-25 00:00:00+00  |     2 |          31.0 |        10.5 |        20.5
 e5f6a7b8    | 2025-06-25 00:00:00+00  |   800 |       16000.0 |        16.0 |        41.0
 e5f6a7b8    | 2025-06-25 01:00:00+00  |   700 |       14000.0 |        16.5 |        42.0
```

### `loganalyze.ingest_offset`

Background-worker bookkeeping (not user-facing): how far each tailed log file has
been consumed, advanced in the same transaction as the stats it produced so
restarts never double-count.

| Column | Type | Description |
|--------|------|-------------|
| `log_path` | `text` **PK** | Absolute path of the tailed log file. |
| `byte_offset` | `bigint` | Bytes consumed so far. |
| `updated_at` | `timestamptz` | Last advance. |

```
            log_path                  | byte_offset |        updated_at
--------------------------------------+-------------+------------------------
 /var/log/postgresql/postgresql.log   |    104857600|  2025-06-25 02:00:03+00
```

---

## Views

### `loganalyze.statements_summary`

The primary human-facing view: every `statements` column **except**
`sum_sq_time_ms`, plus derived **`mean_time_ms`** and population
**`stddev_time_ms`** = `sqrt(max(0, E[X²] − E[X]²))`.

| Added column | Type | Formula |
|--------------|------|---------|
| `mean_time_ms` | `double precision` | `total_time_ms / calls` |
| `stddev_time_ms` | `double precision` | `sqrt(GREATEST(0, sum_sq_time_ms/calls − (total_time_ms/calls)²))` |

```
 fingerprint |        normalized_query          | calls | total_time_ms | mean_time_ms | min_time_ms | max_time_ms | stddev_time_ms
-------------+----------------------------------+-------+---------------+--------------+-------------+-------------+----------------
 a1b2c3d4    | SELECT * FROM orders WHERE id=$1  |     2 |          31.0 |        15.50 |        10.5 |        20.5 |           5.00
 e5f6a7b8    | SELECT customer_id, count(*) ...  |  1500 |       30000.0 |        20.00 |        16.0 |        42.0 |           3.20
```
(plus `query_id`, `representative_sql`, `representative_plan`, `first_seen`, `last_seen`, `complexity`, `metadata`, `plan_analysis`.)

### `loganalyze.top_by_total_time`

`SELECT * FROM statements_summary ORDER BY total_time_ms DESC` — same columns,
slowest groups first. The go-to "where is the time going" view.

```
 fingerprint |        normalized_query          | calls | total_time_ms | mean_time_ms | max_time_ms | stddev_time_ms
-------------+----------------------------------+-------+---------------+--------------+-------------+----------------
 e5f6a7b8    | SELECT customer_id, count(*) ...  |  1500 |       30000.0 |        20.00 |        42.0 |           3.20
 a1b2c3d4    | SELECT * FROM orders WHERE id=$1  |     2 |          31.0 |        15.50 |        20.5 |           5.00
```

### `loganalyze.query_timeline`

`query_histogram` per bucket with a derived mean, ordered by `(fingerprint, bucket)` —
for charting and regression.

| Column | Type | Description |
|--------|------|-------------|
| `fingerprint` | `text` | Query group. |
| `bucket` | `timestamptz` | Hour bucket. |
| `calls` | `bigint` | Executions in the bucket. |
| `total_time_ms` | `double precision` | Sum of durations. |
| `mean_time_ms` | `double precision` | `total_time_ms / calls` (derived). |
| `min_time_ms` / `max_time_ms` | `double precision` | Extremes in the bucket. |

```
 fingerprint |         bucket          | calls | total_time_ms | mean_time_ms | min_time_ms | max_time_ms
-------------+-------------------------+-------+---------------+--------------+-------------+-------------
 a1b2c3d4    | 2025-06-25 00:00:00+00  |     2 |          31.0 |        15.50 |        10.5 |        20.5
 e5f6a7b8    | 2025-06-25 00:00:00+00  |   800 |       16000.0 |        20.00 |        16.0 |        41.0
 e5f6a7b8    | 2025-06-25 01:00:00+00  |   700 |       14000.0 |        20.00 |        16.5 |        42.0
```

### `loganalyze.statements_with_pgss` (created on demand)

Run `SELECT loganalyze_pgss_view();` after `CREATE EXTENSION pg_stat_statements`
to (re)create this view. It joins `statements` to `pg_stat_statements` on
`p.queryid = s.query_id` — loganalyze's plan analysis next to pgss's execution
counters. (Returns `false` + warns if pgss isn't installed.)

| Column | Type | Source |
|--------|------|--------|
| `fingerprint` | `text` | `statements` |
| `query_id` | `bigint` | `statements` |
| `normalized_query` | `text` | `statements` |
| `representative_sql` | `text` | `statements` |
| `loganalyze_calls` | `bigint` | `statements.calls` (captured/sampled count) |
| `loganalyze_mean_ms` | `double precision` | `total_time_ms / calls` |
| `pgss_calls` | `bigint` | `pg_stat_statements.calls` (true total) |
| `pgss_total_exec_ms` | `double precision` | `pg_stat_statements.total_exec_time` |
| `pgss_mean_ms` | `double precision` | `pg_stat_statements.mean_exec_time` |
| `pgss_rows` | `bigint` | `pg_stat_statements.rows` |
| `pgss_shared_hit` | `bigint` | `pg_stat_statements.shared_blks_hit` |
| `pgss_shared_read` | `bigint` | `pg_stat_statements.shared_blks_read` |

```
 fingerprint | loganalyze_calls | loganalyze_mean_ms | pgss_calls | pgss_total_exec_ms | pgss_mean_ms | pgss_rows | pgss_shared_hit | pgss_shared_read
-------------+------------------+--------------------+------------+--------------------+--------------+-----------+-----------------+------------------
 a1b2c3d4    |                2 |              15.50 |       1532 |            23110.4 |        15.08 |      1532 |            6128 |               12
 e5f6a7b8    |             1500 |              20.00 |       1500 |            29880.0 |        19.92 |   7500000 |          540000 |             2200
```

> **`loganalyze_calls` vs `pgss_calls`:** pgss counts *every* execution; loganalyze
> counts only **captured** ones, so with `sample_rate < 1` (or `min_duration_ms`
> gating) loganalyze_calls is the smaller, sampled number. For group A above, pgss
> saw 1532 executions while loganalyze captured 2.

---

## Viewing the captured plan

The plan is stored as raw `EXPLAIN (FORMAT TEXT)` in the **`representative_plan`**
text column (the slowest-seen execution). It is already the indented plan tree —
just select it:

```sql
-- the plan for the costliest query group
SELECT representative_plan
FROM   loganalyze.top_by_total_time
LIMIT  1;

-- plan for a specific fingerprint, with the pretty-printed SQL
SELECT loganalyze_format(representative_sql) AS sql,
       representative_plan
FROM   loganalyze.statements
WHERE  fingerprint = 'a1b2c3d4';
```

```
                                          representative_plan
------------------------------------------------------------------------------------------------------
 HashAggregate  (cost=1234.00..1244.00 rows=1000 width=12) (actual time=18.4..19.9 rows=5000 loops=1)
   Group Key: customer_id
   ->  Seq Scan on orders  (cost=0.00..984.00 rows=50000 width=4) (actual time=0.01..6.2 rows=50000 ...)
 Planning Time: 0.10 ms
 Execution Time: 20.13 ms
```

Notes:
- With `track_timing=off` the per-node `actual time=` is omitted (row counts stay).
- For **stats-only** captures (`capture_plan=off`) `representative_plan` is empty.
- The *structured* version of the plan (findings, costs, node analysis) is in the
  `plan_analysis` JSONB column below; the **raw tree is only the text column** — the
  parsed tree is not stored as its own column.

## JSONB columns

`complexity`, `metadata`, and `plan_analysis` hold the core analyzers' output as
queryable `jsonb` (refreshed when the representative plan changes; `NULL` for
stats-only captures). Enum values serialize as their variant names (no rename),
e.g. `"Select"`, `"OLAP"`, `"MemorySpill"`. Query them with the usual operators,
e.g. `WHERE plan_analysis -> 'summary' ->> 'performance_assessment' = 'Poor'`.

### `complexity` — `ComplexityScore` (`sql_analysis/complexity.rs`)

| Field | Type | Notes |
|-------|------|-------|
| `total_score` | number | overall score |
| `classification` | enum | `Simple` / `Moderate` / `Complex` / `VeryComplex` |
| `components` | object | `join_complexity`, `subquery_complexity`, `function_complexity`, `condition_complexity`, `aggregation_complexity`, `window_complexity` (all numbers) |
| `breakdown` | object | `table_count`; `join_info {total_joins, inner_joins, outer_joins, cross_joins, self_joins, max_join_depth}`; `subquery_info {total_subqueries, correlated_subqueries, max_nesting_level, exists_subqueries, in_subqueries}`; `function_info {total_functions, aggregate_functions, window_functions, scalar_functions, unique_functions[]}`; `condition_info {where_conditions, having_conditions, join_conditions, complex_expressions, case_statements}`; `aggregation_info {group_by_columns, aggregate_functions, having_clause(bool), distinct_aggregates}` |

### `metadata` — `QueryMetadata` (`sql_analysis/metadata.rs`)

| Field | Type | Notes |
|-------|------|-------|
| `operation` | enum | `Select`/`Insert`/`Update`/`Delete`/`CreateTable`/`CreateIndex`/`DropTable`/`DropIndex`/`Analyze`/`Vacuum`/`Other(<text>)` |
| `table_references` | array | `{schema?, table, alias?, access_type: Primary\|Joined\|Subquery\|CTE, join_type?}` |
| `column_references` | array | `{table?, column, usage: Selected\|Filtered\|Joined\|Grouped\|Ordered\|Aggregated\|Updated\|Inserted, data_type?}` |
| `function_references` | array | `{name, category: Aggregate\|Window\|String\|Date\|Math\|Conversion\|System\|UserDefined\|Other, argument_count, has_distinct}` |
| `execution_pattern` | object | `{estimated_selectivity, likely_full_scan(bool), index_hints[]: {table, columns[], index_type: BTree\|Hash\|GIN\|GiST\|BRIN\|Partial\|Unique, reason}, parallel_potential: High\|Medium\|Low\|None}` |
| `access_pattern` | object | `{accesses_hot_data(bool), estimated_volume: Small\|Medium\|Large\|VeryLarge\|Unknown, read_write_ratio, temporal_pattern: Recent\|Historical\|Range\|All\|Unknown}` |
| `classification` | object | `{workload_type: OLTP\|OLAP\|Reporting\|ETL\|Maintenance\|Mixed, frequency_pattern: HighFrequency\|MediumFrequency\|LowFrequency\|OneTime, resource_pattern: CPUIntensive\|IOIntensive\|MemoryIntensive\|NetworkIntensive\|Balanced}` |
| `performance_hints` | array | `{category: Indexing\|QueryRewrite\|…, description, impact, difficulty}` |

### `plan_analysis` — `CombinedAnalysisResult` (`analysis/mod.rs`)

| Field | Type | Notes |
|-------|------|-------|
| `reports` | array | one per analyzer: `{analyzer_name, findings[]: Finding, metrics{<name>:number}, metadata{<key>:<text>}}` |
| `summary` | object | `{total_findings, severity_counts{<Severity>:count}, type_counts{<type>:count}, top_issues[]: Finding, performance_assessment: Excellent\|Good\|Fair\|Poor\|Critical}` |

A **`Finding`** (in `reports[].findings[]` and `summary.top_issues[]`):

| Field | Type | Notes |
|-------|------|-------|
| `finding_type` | enum | `ExcessiveRowProcessing`, `RowEstimationError`, `CartesianProduct`, `LargeSequentialScan`, `InefficiientScan` *(sic)*, `MissingIndex`, `PoorIndexSelectivity`, `IneffectiveJoinAlgorithm`, `LargeNestedLoop`, `HashJoinMemorySpill`, `HighStartupCost`, `ExpensiveOperation`, `HighCostVariability`, `MemorySpill`, `LargeSort`, `LargeAggregation`, `InefficientParallelism`, `MissedParallelization`, `Custom(<text>)` |
| `severity` | enum | `Low` / `Medium` / `High` / `Critical` |
| `title` | string | short label |
| `description` | string | what was found |
| `suggestion` | string | recommended fix |
| `affected_nodes` | array | `{path: [int…], description}` — locates the plan node(s) |
| `evidence` | object | `{<metric>: number}` (e.g. spilled MB, cache-hit ratio) |
| `metadata` | object | `{<key>: <text>}` extra context |

Example `plan_analysis` (one analyzer, one finding):
```json
{
  "reports": [
    { "analyzer_name": "BufferWalAnalyzer",
      "findings": [
        { "finding_type": "MemorySpill", "severity": "Medium",
          "title": "Sort spilled to disk",
          "description": "Sort node spilled 18 MB to temp files",
          "suggestion": "Raise work_mem to keep this sort in memory",
          "affected_nodes": [ { "path": [0, 1], "description": "Sort" } ],
          "evidence": { "spilled_mb": 18.0 },
          "metadata": { "sort_method": "external merge" } } ],
      "metrics": { "temp_read_mb": 18.0 },
      "metadata": {} } ],
  "summary": {
    "total_findings": 1,
    "severity_counts": { "Medium": 1 },
    "type_counts": { "MemorySpill": 1 },
    "top_issues": [ /* … same Finding … */ ],
    "performance_assessment": "Fair"
  }
}
```

> The field **names/types and all enum variants above are exact** (from
> `complexity.rs`, `metadata.rs`, `analysis/mod.rs`); the example *values* are
> illustrative — actual contents depend on the plan and which analyzers fired.

