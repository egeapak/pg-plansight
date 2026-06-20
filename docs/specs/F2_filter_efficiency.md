# F2 — FilterEfficiencyAnalyzer

## Goal
Flag nodes that read/produce many rows only to throw most of them away in a
filter — the classic "scan reads 1M rows, keeps 2%" signal that points at a
missing or incomplete index, or a filter that should be pushed down. Not covered
today.

## File
`crates/core/src/analysis/analyzers/filter_efficiency.rs`
Struct `FilterEfficiencyAnalyzer` + `NodeVisitor`. Register in `mod.rs`.

## Inputs
Per node:
- `rows_removed_by_filter() -> Option<u64>`
- `rows_removed_by_join_filter() -> Option<u64>`
- `rows_removed_by_index_recheck() -> Option<u64>`
- `node.actuals.as_ref().and_then(|a| a.actual_rows)` — rows that *passed*.
- `node.actuals...actual_loops` — to understand per-loop vs total (report totals).

## Derived metric
For filter selectivity: `kept = actual_rows`, `removed = rows_removed_by_filter`
(or join filter). `selectivity = kept / (kept + removed)`. Low selectivity +
large `removed` = wasteful.

## Detection rules → Findings
1. **Low-selectivity filter** — `removed >= min_rows_removed` (default 10_000)
   AND `selectivity < 0.10`.
   - `FindingType::ExcessiveRowProcessing`, severity scaled by `removed`
     (Medium ≥ 10k, High ≥ 100k, Critical ≥ 1M).
   - suggestion: add/extend an index covering the filter predicate, or push the
     predicate earlier. Include the node's `Filter` text in metadata if present.
   - evidence: `rows_removed`, `rows_kept`, `selectivity`.
2. **Weak index (high recheck)** — `rows_removed_by_index_recheck >= min` (10k).
   - `FindingType::PoorIndexSelectivity`, Medium.
   - suggestion: the index condition is imprecise; consider a more selective or
     composite index.
3. **Expensive join filter** — `rows_removed_by_join_filter >= min` (10k).
   - `FindingType::IneffectiveJoinAlgorithm`, Medium.
   - suggestion: join condition not fully indexed / join produces a large
     intermediate set later filtered.

Metrics: `total_rows_removed`, `nodes_with_wasteful_filter`, `min_selectivity_seen`.

## Tests
- `test_low_selectivity_seq_scan_flagged` — actual_rows=2000,
  RowsRemovedByFilter=998_000 → ExcessiveRowProcessing, High/Critical.
- `test_selective_filter_no_finding` — actual_rows=9000, removed=1000
  (selectivity 0.9) → no finding (negative).
- `test_small_absolute_removed_no_finding` — removed=500 → below threshold,
  no finding (negative).
- `test_index_recheck_flagged` — RowsRemovedByIndexRecheck=50_000 →
  PoorIndexSelectivity.
- `test_join_filter_flagged` — RowsRemovedByJoinFilter=80_000 →
  IneffectiveJoinAlgorithm.

## Edge cases
- No `actuals` → cannot compute selectivity; if `removed` is large, still flag
  on absolute count with a note that selectivity is unknown. If both missing →
  skip.
- `kept + removed == 0` → skip (avoid div-by-zero).
