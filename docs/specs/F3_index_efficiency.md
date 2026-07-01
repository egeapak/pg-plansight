# F3 — IndexEfficiencyAnalyzer

## Goal
Surface two specific, common index inefficiencies that have concrete fixes and
are not covered today:
1. **Index-Only Scan doing many heap fetches** — the visibility map is stale, so
   the "index-only" scan still hits the heap. Fix: `VACUUM` the table.
2. **Lossy bitmap heap scan blocks** — the bitmap exceeded `work_mem` and went
   lossy, forcing rechecks. Fix: raise `work_mem` or narrow the scan.

## File
`crates/core/src/analysis/analyzers/index_efficiency.rs`
Struct `IndexEfficiencyAnalyzer` + `NodeVisitor`. Register in `mod.rs`.

## Inputs
- `heap_fetches() -> Option<u64>`
- `heap_blocks_lossy() -> Option<u64>`
- `heap_blocks_exact() -> Option<u64>`
- node type / `actuals.actual_rows` for context and severity scaling.

## Detection rules → Findings
1. **High heap fetches on index-only scan** — `heap_fetches >= min_heap_fetches`
   (default 1_000) AND (if actuals present) `heap_fetches` is a large fraction
   (`> 0.10`) of `actual_rows`.
   - `FindingType::Custom("IndexOnlyScanHeapFetches")`, Medium (High if fetches
     ≥ 100k).
   - suggestion: run `VACUUM` / `VACUUM ANALYZE` on the table to refresh the
     visibility map; consider more aggressive autovacuum on hot tables.
   - evidence: `heap_fetches`, `actual_rows`, `heap_fetch_ratio`.
2. **Lossy bitmap blocks** — `heap_blocks_lossy >= min_lossy_blocks`
   (default 1) AND lossy is a meaningful share of total
   (`lossy / (lossy + exact) > 0.05`), guarded so a couple of lossy blocks on a
   huge scan still flags only when absolute lossy is non-trivial (≥ 64 blocks).
   - `FindingType::MemorySpill`, Medium.
   - suggestion: raise `work_mem` so the bitmap stays exact, or add a more
     selective index to shrink the bitmap.
   - evidence: `heap_blocks_lossy`, `heap_blocks_exact`, `lossy_fraction`.

Metrics: `total_heap_fetches`, `total_lossy_blocks`, `nodes_analyzed`.

## Tests
- `test_index_only_scan_heap_fetches_flagged` — HeapFetches=200_000,
  actual_rows=210_000 → IndexOnlyScanHeapFetches, High.
- `test_index_only_scan_few_fetches_no_finding` — HeapFetches=5 → no finding.
- `test_lossy_bitmap_flagged` — HeapBlocksLossy=5_000, HeapBlocksExact=1_000 →
  MemorySpill finding.
- `test_mostly_exact_bitmap_no_finding` — HeapBlocksLossy=2,
  HeapBlocksExact=100_000 → no finding (below absolute + fraction guard).

## Edge cases
- Properties absent → skip node.
- `lossy + exact == 0` → skip fraction calc.
