# F1 — SortMemoryAnalyzer

## Goal
Detect operations that exceeded `work_mem` and spilled to disk (sorts, hash
aggregates, hash joins), and report peak memory so users can size `work_mem` or
restructure the query. This is the single most common, most actionable tuning
signal and is **not** covered today (the `StartupCostAnalyzer` only does a
narrow cost-threshold check on the word "external").

## File
`crates/core/src/analysis/analyzers/sort_memory.rs`
Struct `SortMemoryAnalyzer` implementing `Analyzer`, plus a `NodeVisitor`.
Register in `analyzers/mod.rs` (`pub mod` + `pub use`).

## Inputs (already-parsed typed properties)
Per node, via `node.properties()`:
- `sort_space_type() -> Option<&str>` — `"Disk"` or `"Memory"`.
- `PlanProperty::SortMethod` — accessed via `get("Sort Method")`; values like
  `"external merge"`, `"external sort"`, `"quicksort"`, `"top-N heapsort"`.
- `PlanProperty::SortSpaceUsed` — `get("Sort Space Used")`, e.g. `"12345"` (kB).
- `batches() -> Option<u32>` — hash batches; `> 1` means the hash spilled.
- `peak_memory_usage() -> Option<&str>` — kB string.

Parse kB strings with a small helper (`"524288 kB"` or `"524288"` → `524288`).

## Detection rules → Findings
1. **Disk sort** — `sort_space_type == "Disk"` OR `SortMethod` contains
   `"external"`.
   - `FindingType::MemorySpill`, severity: High if space_used_kb > 100_000 else
     Medium.
   - suggestion: raise `work_mem` (cite observed kB), or add an index providing
     the sort order, or reduce rows before sorting.
   - evidence: `sort_space_used_kb`, metadata `sort_method`, `sort_space_type`.
2. **Hash batch spill** — `batches() > 1`.
   - `FindingType::HashJoinMemorySpill`, severity: Medium (High if batches > 8).
   - evidence: `batches`. suggestion: raise `work_mem`; spilled into N batches.
3. **Large in-memory sort approaching limit** *(optional, Low)* — quicksort with
   `space_used_kb > 0.9 * context.work_mem_kb`. Low severity heads-up.

Always emit metrics: `nodes_with_sort_spill`, `nodes_with_hash_spill`,
`max_sort_space_used_kb`, `nodes_analyzed`.

## Tests (positive + negative)
Build synthetic `PlanNode`s (see `startup_cost.rs` tests for the pattern). Set
properties via `node.properties_mut().set("Sort Space Type", "Disk")` etc.
- `test_external_sort_detected` — Disk sort + space used → MemorySpill finding.
- `test_hash_batch_spill_detected` — `Batches = 16` → HashJoinMemorySpill, High.
- `test_in_memory_quicksort_no_finding` — `Sort Space Type = Memory`,
  `Sort Method = quicksort`, small space → **no** findings (negative).
- `test_kb_parsing` — helper parses `"524288 kB"` and `"524288"`.
- `test_metrics_emitted` — `max_sort_space_used_kb` reflects the largest node.

## Edge cases
- Missing properties → skip the node (no panic, no finding).
- `Sort Space Used` unpar_seable → treat as 0, still flag on space_type.
