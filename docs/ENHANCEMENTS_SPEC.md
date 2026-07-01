# Plansight Enhancement Specification

This document catalogs **new extractions, analyzers, and accumulated metrics**
proposed for Plansight, with implementation analysis. It is the master index;
each feature slated for implementation has a detailed spec under
[`docs/specs/`](specs/).

The proposals come from a three-surface review:

1. **Plan extraction** — more signal from the `EXPLAIN` tree.
2. **Query-text analysis** — anti-patterns and structure from the SQL itself.
3. **Accumulated metrics** — time-series/aggregate signals in the exporter and
   the in-database extension.

---

## Architecture recap (where things plug in)

| Surface | Trait / entry point | Input | Output |
|---------|---------------------|-------|--------|
| Plan analyzer | `analysis::Analyzer` (`fn analyze(&ParsedPlan, &AnalysisContext) -> AnalysisReport`) | `ParsedPlan` tree + typed `PlanProperties` per node | `Finding`s + metrics |
| SQL analysis | `sql_analysis` modules (e.g. `ComplexityAnalyzer`, `MetadataExtractor`) | query text (+ `sqlparser` AST) | structs attached to `ProcessedQuery` |
| Exporter metric | `metrics::MetricsBackend` (`prometheus_backend`, `otel_backend`) | per-query stats from the collector | Prometheus/OTel series |
| Extension metric | `pg_extension` aggregate row + SQL views | in-process executor hook | SQL-queryable columns |

Plan analyzers are registered at two TUI call sites
(`crates/tui/src/ui/state/log_parsing_state.rs`,
`crates/tui/src/ui/state/query_detail_view.rs`) and in the exporter collector.

`PlanProperty` (in `crates/core/src/plan_properties.rs`) already parses the
typed fields the new analyzers need: `SortMethod`, `SortSpaceType`,
`SortSpaceUsed`, `PeakMemoryUsage`, `Batches`, `RowsRemovedByFilter`,
`RowsRemovedByJoinFilter`, `RowsRemovedByIndexRecheck`, `HeapFetches`,
`HeapBlocksExact`, `HeapBlocksLossy`, `WorkersPlanned`, `WorkersLaunched`,
`Loops`, `OneTimeFilter`, `SubplanName`.

---

## Implementation tiers

| Tier | Surface | Verifiable in this repo? |
|------|---------|--------------------------|
| 1 | New plan analyzers in `crates/core` | ✅ `cargo test -p pg-plansight-core` |
| 2 | SQL-text analysis in `crates/core/sql_analysis` | ✅ unit tests |
| 3a | Exporter derived metrics in `crates/exporter` | ✅ `cargo test -p pg-plansight-exporter` |
| 3b | Accumulated metrics in `crates/pg_extension` | ⚠️ needs `cargo-pgrx` + Postgres (`cargo pgrx test`) |

---

## Feature catalog

Each feature with a `spec` link is being implemented. Items marked
*(already covered)* exist today and are intentionally excluded to avoid
duplication.

### Tier 1 — Plan-tree analyzers

| ID | Feature | Source data | Spec |
|----|---------|-------------|------|
| F1 | **SortMemoryAnalyzer** — disk/external sorts, hash-batch spills, peak memory; `work_mem` guidance | `SortMethod`, `SortSpaceType`, `SortSpaceUsed`, `Batches`, `PeakMemoryUsage` | [F1_sort_memory.md](specs/F1_sort_memory.md) |
| F2 | **FilterEfficiencyAnalyzer** — rows discarded by filters; selectivity; weak-index recheck | `RowsRemovedByFilter`/`JoinFilter`/`IndexRecheck` + `actuals.actual_rows` | [F2_filter_efficiency.md](specs/F2_filter_efficiency.md) |
| F3 | **IndexEfficiencyAnalyzer** — index-only-scan heap fetches (VM/VACUUM), lossy bitmap blocks | `HeapFetches`, `HeapBlocksLossy`, `HeapBlocksExact` | [F3_index_efficiency.md](specs/F3_index_efficiency.md) |
| F4 | **PlanShapeAnalyzer** — depth, node count, type mix, dominant (most-expensive) node | tree traversal + `PlanCost` | [F4_plan_shape.md](specs/F4_plan_shape.md) |
| F5 | **EstimationHealthAnalyzer** — whole-plan estimate health: systematic over/under, worst offender, root skew | `PlanCost.estimated_rows` vs `actuals.actual_rows` | [F5_estimation_health.md](specs/F5_estimation_health.md) |
| — | Worker launch-gap *(already covered by `parallelization.rs`)* | — | — |
| — | Per-node row-estimation errors *(already covered by `row_estimation.rs`)* | — | — |

### Tier 2 — SQL-text analysis

| ID | Feature | Source data | Spec |
|----|---------|-------------|------|
| F6 | **AntiPatternAnalyzer** — `SELECT *`, leading-wildcard `LIKE`, function-wrapped predicate, `NOT IN`, `OFFSET` w/o `LIMIT`, `UNION` vs `UNION ALL`, correlated-subquery / CTE hints | query text + `sqlparser` AST | [F6_sql_anti_patterns.md](specs/F6_sql_anti_patterns.md) |

### Tier 3a — Exporter derived metrics

| ID | Feature | Source data | Spec |
|----|---------|-------------|------|
| F7 | **Derived query metrics** — coefficient of variation, share-of-total DB time, rows-per-call, p95/p99 gauges | collector per-query stats | [F7_exporter_metrics.md](specs/F7_exporter_metrics.md) |

### Tier 3b — Extension accumulated metrics

| ID | Feature | Source data | Spec |
|----|---------|-------------|------|
| F8 | **Accumulated stats** — SLO-breach counter (new GUC), cumulative WAL bytes, buffer-hit accumulation, distinct-plan / plan-change tracking | executor hook + aggregate row | [F8_extension_metrics.md](specs/F8_extension_metrics.md) |

---

## Deferred / future ideas (spec'd, not yet scheduled)

These appear in the source review but are lower-priority or need design work:

- **I/O-timing analyzer** (`I/O Read/Write Time`) and **JIT analyzer** — require
  parsing fields not yet in `PlanProperty`; add the property variants first.
- **Trigger-cost analyzer** — data lives on `JsonPlan.triggers`, not in
  `ParsedPlan`; needs a separate integration point than the `Analyzer` trait.
- **EXPLAIN SETTINGS capture** — needs a new `JsonPlan.settings` field + capture
  option.
- **Subplan/InitPlan materialization**, **plan-diff / regression-cause** — larger
  cross-cutting features building on F4/F5 + plan-stability (F8).
- **Dependency map** (tables↔columns↔queries), **predicate-indexability profile**
  — extensions of `MetadataExtractor`/F6.
- **Composite health score**, **table-level hot-spot accumulation** — build on
  F7/F8.

---

## Validation policy

Every implemented feature ships with **positive tests** (the condition is
detected / metric is correct) and **negative tests** (clean input produces no
false positive). Tier 1–3a run under `cargo test`; Tier 3b uses pgrx
`#[pg_test]` where a test cluster is available. The whole workspace must pass
`cargo fmt --all` and
`cargo clippy --workspace --all-features --all-targets -- -D warnings`.
