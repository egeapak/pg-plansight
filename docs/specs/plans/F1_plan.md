# F1 — SortMemoryAnalyzer: Implementation Plan

Convert spec `docs/specs/F1_sort_memory.md` into code. Detect operations that
spilled to disk (sorts, hash joins/aggregates) and report peak memory so users
can size `work_mem`.

All types/accessors below are verified against the real codebase:
- `Analyzer` trait + `Finding`/`AnalysisReport`/`Severity`/`FindingType` builders: `crates/core/src/analysis/mod.rs`
- `NodeVisitor` / `PlanTraversal::depth_first`: `crates/core/src/analysis/traversal.rs`
- Typed accessors `sort_space_type()`, `batches()`, `peak_memory_usage()`, plus `get("Sort Method")` / `get("Sort Space Used")`: `crates/core/src/plan_properties.rs`
- `PlanNode::properties()` / `properties_mut()` / `description()`: `crates/core/src/plan_parser.rs`
- Analyzer mirror pattern: `crates/core/src/analysis/analyzers/startup_cost.rs`

Relevant `FindingType` variants already exist (no enum edits): `MemorySpill`,
`HashJoinMemorySpill`, `LargeSort` (`crates/core/src/analysis/mod.rs:21-54`).
`AnalysisContext.work_mem_kb: usize` is available (mod.rs:206).

---

## 1. New file: `crates/core/src/analysis/analyzers/sort_memory.rs`

```rust
use super::super::consolidated_config::AnalysisConfiguration;
use super::super::traversal::{NodeVisitor, PlanTraversal};
use super::super::{
    AnalysisContext, AnalysisReport, Analyzer, Finding, FindingType, NodePath, Severity,
};
use crate::{ParsedPlan, PlanNode};

/// Analyzer for detecting operations that exceeded `work_mem` and spilled to
/// disk (sorts, hash joins/aggregates), plus large in-memory sorts that are
/// approaching the `work_mem` limit.
pub struct SortMemoryAnalyzer;

impl SortMemoryAnalyzer {
    pub fn new() -> Self {
        Self
    }

    pub fn with_config(_config: &AnalysisConfiguration) -> Self {
        Self::new()
    }
}

impl Default for SortMemoryAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer for SortMemoryAnalyzer {
    fn analyze(&self, plan: &ParsedPlan, context: &AnalysisContext) -> AnalysisReport {
        let mut report = AnalysisReport::new("SortMemoryAnalyzer".to_string())
            .with_metadata("version", self.version());

        let mut visitor = SortMemoryVisitor::new();
        PlanTraversal::depth_first(plan, &mut visitor, context);

        for finding in visitor.findings {
            report = report.add_finding(finding);
        }

        report = report
            .with_metric("nodes_analyzed", visitor.nodes_analyzed as f64)
            .with_metric("nodes_with_sort_spill", visitor.nodes_with_sort_spill as f64)
            .with_metric("nodes_with_hash_spill", visitor.nodes_with_hash_spill as f64)
            .with_metric("max_sort_space_used_kb", visitor.max_sort_space_used_kb);

        report
    }

    fn name(&self) -> &'static str {
        "SortMemoryAnalyzer"
    }

    fn description(&self) -> &'static str {
        "Detects sorts/hashes that spilled to disk (exceeded work_mem) and reports peak memory"
    }

    fn version(&self) -> &'static str {
        "1.0.0"
    }
}

/// Parse a PostgreSQL kB string such as `"524288 kB"` or `"524288"` into a
/// numeric kB value. Returns `None` if no leading integer can be parsed.
fn parse_kb(value: &str) -> Option<f64> {
    value
        .split_whitespace()
        .next()
        .and_then(|tok| tok.parse::<f64>().ok())
}

struct SortMemoryVisitor {
    findings: Vec<Finding>,
    nodes_analyzed: usize,
    nodes_with_sort_spill: usize,
    nodes_with_hash_spill: usize,
    max_sort_space_used_kb: f64,
}

impl SortMemoryVisitor {
    fn new() -> Self {
        Self {
            findings: Vec::new(),
            nodes_analyzed: 0,
            nodes_with_sort_spill: 0,
            nodes_with_hash_spill: 0,
            max_sort_space_used_kb: 0.0,
        }
    }

    fn detect(&mut self, node: &PlanNode, path: &NodePath, context: &AnalysisContext) {
        let props = node.properties();

        // Pull typed/raw values up front. Missing -> None (skip silently).
        let sort_space_type = props.sort_space_type(); // Option<&str>: "Disk" | "Memory"
        let sort_method = props.get("Sort Method"); // Option<String>
        let space_used_kb = props
            .get("Sort Space Used")
            .as_deref()
            .and_then(parse_kb)
            .unwrap_or(0.0);
        let batches = props.batches(); // Option<u32>

        if space_used_kb > self.max_sort_space_used_kb {
            self.max_sort_space_used_kb = space_used_kb;
        }

        // --- Rule 1: Disk sort -------------------------------------------------
        let is_disk = sort_space_type == Some("Disk");
        let method_external = sort_method
            .as_deref()
            .map(|m| m.to_lowercase().contains("external"))
            .unwrap_or(false);

        if is_disk || method_external {
            self.nodes_with_sort_spill += 1;

            let severity = if space_used_kb > 100_000.0 {
                Severity::High
            } else {
                Severity::Medium
            };

            let mut finding = Finding::new(
                FindingType::MemorySpill,
                severity,
                "Sort spilled to disk".to_string(),
                format!(
                    "Operation '{}' performed an external (disk-based) sort using {:.0} kB. \
                     It exceeded work_mem ({} kB) and spilled to disk.",
                    node.description(),
                    space_used_kb,
                    context.work_mem_kb
                ),
                "Raise work_mem to fit the sort in memory, add an index providing the \
                 required sort order, or reduce the number of rows before sorting."
                    .to_string(),
            )
            .with_node(path.clone())
            .with_evidence("sort_space_used_kb", space_used_kb)
            .with_evidence("work_mem_kb", context.work_mem_kb as f64);

            if let Some(m) = sort_method.as_deref() {
                finding = finding.with_metadata("sort_method", m);
            }
            if let Some(t) = sort_space_type {
                finding = finding.with_metadata("sort_space_type", t);
            }

            self.findings.push(finding);
        }

        // --- Rule 2: Hash batch spill -----------------------------------------
        if let Some(n) = batches {
            if n > 1 {
                self.nodes_with_hash_spill += 1;

                let severity = if n > 8 {
                    Severity::High
                } else {
                    Severity::Medium
                };

                let finding = Finding::new(
                    FindingType::HashJoinMemorySpill,
                    severity,
                    "Hash operation spilled into multiple batches".to_string(),
                    format!(
                        "Operation '{}' used {} hash batches (> 1), indicating the hash table \
                         did not fit in work_mem ({} kB) and spilled to disk.",
                        node.description(),
                        n,
                        context.work_mem_kb
                    ),
                    "Raise work_mem so the hash table fits in a single batch, or reduce the \
                     number of rows on the hashed (build) side."
                        .to_string(),
                )
                .with_node(path.clone())
                .with_evidence("batches", n as f64)
                .with_evidence("work_mem_kb", context.work_mem_kb as f64);

                self.findings.push(finding);
            }
        }

        // --- Rule 3 (optional, Low): large in-memory sort near limit ----------
        let method_quicksort = sort_method
            .as_deref()
            .map(|m| m.to_lowercase().contains("quicksort"))
            .unwrap_or(false);

        if sort_space_type == Some("Memory")
            && method_quicksort
            && space_used_kb > 0.9 * context.work_mem_kb as f64
        {
            let finding = Finding::new(
                FindingType::LargeSort,
                Severity::Low,
                "In-memory sort approaching work_mem limit".to_string(),
                format!(
                    "Operation '{}' sorted in memory using {:.0} kB, within 10% of work_mem \
                     ({} kB). A slightly larger dataset would spill to disk.",
                    node.description(),
                    space_used_kb,
                    context.work_mem_kb
                ),
                "Consider a modest work_mem increase to keep this sort in memory as data grows."
                    .to_string(),
            )
            .with_node(path.clone())
            .with_evidence("sort_space_used_kb", space_used_kb)
            .with_evidence("work_mem_kb", context.work_mem_kb as f64);

            self.findings.push(finding);
        }
    }
}

impl NodeVisitor for SortMemoryVisitor {
    fn visit_node(&mut self, node: &PlanNode, path: &NodePath, context: &AnalysisContext) {
        self.nodes_analyzed += 1;
        self.detect(node, path, context);
    }
}
```

Notes / edge cases:
- Missing properties -> all the `Option`s are `None`, every rule's guard is
  false, no finding, no panic (spec edge case 1).
- `Sort Space Used` unparseable -> `parse_kb` returns `None` -> `space_used_kb`
  defaults to `0.0`, but Rule 1 still fires on `is_disk`/`method_external`
  (spec edge case 2).
- `parse_kb` uses `split_whitespace().next()` so it handles both `"524288"` and
  `"524288 kB"`.

---

## 2. Detection thresholds → FindingType / Severity (summary table)

| Rule | Condition | FindingType | Severity |
|------|-----------|-------------|----------|
| 1 Disk sort | `sort_space_type() == Some("Disk")` OR `Sort Method` contains `"external"` | `MemorySpill` | `High` if `space_used_kb > 100_000`, else `Medium` |
| 2 Hash spill | `batches() > 1` | `HashJoinMemorySpill` | `High` if `batches > 8`, else `Medium` |
| 3 Near-limit in-mem sort | `sort_space_type() == Some("Memory")` AND method contains `"quicksort"` AND `space_used_kb > 0.9 * work_mem_kb` | `LargeSort` | `Low` |

Metrics always emitted: `nodes_analyzed`, `nodes_with_sort_spill`,
`nodes_with_hash_spill`, `max_sort_space_used_kb`.

---

## 3. Registration edits

### 3a. `crates/core/src/analysis/analyzers/mod.rs`
Mirror existing `startup_cost` lines (mod.rs:13 and :23).

Add to the module declarations block (after `pub mod startup_cost;`, line 13):
```rust
pub mod sort_memory;
```
Add to the re-export block (after `pub use startup_cost::StartupCostAnalyzer;`, line 23):
```rust
pub use sort_memory::SortMemoryAnalyzer;
```

### 3b. `crates/tui/src/ui/state/log_parsing_state.rs`
Around lines 546-563. Add `SortMemoryAnalyzer` to the `analyzers::{...}` import
list (currently lines 549-551):
```rust
                        analyzers::{
                            IndexUsageAnalyzer, JoinAnalyzer, QueryPatternAnalyzer,
                            RowEstimationAnalyzer, ScanAnalyzer, SortMemoryAnalyzer,
                            StartupCostAnalyzer,
                        },
```
And add to the builder chain after line 562 (`.add_analyzer(IndexUsageAnalyzer::new())`):
```rust
                        .add_analyzer(SortMemoryAnalyzer::new())
```

### 3c. `crates/tui/src/ui/state/query_detail_view.rs`
Around lines 8-12 and 76-82. Add `SortMemoryAnalyzer` to the `analyzers::{...}`
import (currently lines 9-12):
```rust
    analyzers::{
        IndexUsageAnalyzer, JoinAnalyzer, QueryPatternAnalyzer, RowEstimationAnalyzer,
        ScanAnalyzer, SortMemoryAnalyzer, StartupCostAnalyzer,
    },
```
And add to the builder chain after line 81 (`.add_analyzer(IndexUsageAnalyzer::new())`):
```rust
            .add_analyzer(SortMemoryAnalyzer::new())
```

Keep import lists alphabetically ordered to satisfy `cargo fmt`.

---

## 4. Unit tests (in `sort_memory.rs`, `#[cfg(test)] mod tests`)

Mirror `startup_cost.rs` tests. Use `PlanNode::new(...)` then
`node.properties_mut().set("<Key>", "<Value>")`, wrap in
`ParsedPlan::new(node)`, run `SortMemoryAnalyzer::new().analyze(&plan, &context)`.
Test header:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NodeType, PlanCost, PlanNode, ScanType, TableReference};

    fn sort_node() -> PlanNode {
        // A generic node to attach sort/hash properties to. Node type is
        // irrelevant — the analyzer keys off properties, not NodeType.
        PlanNode::new(
            NodeType::Scan(ScanType::SeqScan {
                table: TableReference { schema: None, name: "t".to_string(), alias: None },
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 100.0,
                estimated_rows: 1000,
                estimated_width: 50,
            },
            "Sort".to_string(),
        )
    }
    // ... tests below
}
```

| Test name | Constructs | Asserts |
|-----------|-----------|---------|
| `test_external_sort_detected` (positive) | `sort_node()`; set `"Sort Space Type" = "Disk"`, `"Sort Method" = "external merge"`, `"Sort Space Used" = "120000 kB"` | exactly one finding with `finding_type == MemorySpill`; its `severity == High` (120000 > 100000); `evidence["sort_space_used_kb"] == 120000.0`; `metadata["sort_method"] == "external merge"`; `metadata["sort_space_type"] == "Disk"` |
| `test_external_sort_medium_severity` (positive) | set `"Sort Space Type" = "Disk"`, `"Sort Space Used" = "5000 kB"` (no Sort Method) | one `MemorySpill` finding with `severity == Medium` (5000 < 100000) |
| `test_hash_batch_spill_detected` (positive) | set `"Batches" = "16"` | one finding with `finding_type == HashJoinMemorySpill`, `severity == High` (16 > 8); `evidence["batches"] == 16.0` |
| `test_hash_batch_spill_medium` (positive) | set `"Batches" = "4"` | one `HashJoinMemorySpill` finding with `severity == Medium` |
| `test_in_memory_quicksort_no_finding` (negative) | set `"Sort Space Type" = "Memory"`, `"Sort Method" = "quicksort"`, `"Sort Space Used" = "64"`; default context (`work_mem_kb == 4096`) | `report.findings.is_empty()` — Memory type not Disk, method not external, 64 kB not > 0.9*4096 |
| `test_single_batch_no_finding` (negative) | set `"Batches" = "1"` | `report.findings.is_empty()` (1 is not > 1) |
| `test_no_properties_no_finding` (negative) | bare `sort_node()`, no properties set | `report.findings.is_empty()`; `report.metrics["nodes_analyzed"] == 1.0` |
| `test_quicksort_near_limit_low_finding` (positive, Rule 3) | `context = AnalysisContext::new().with_work_mem_kb(1000)`; set `"Sort Space Type" = "Memory"`, `"Sort Method" = "quicksort"`, `"Sort Space Used" = "950"` (> 0.9*1000) | one finding with `finding_type == LargeSort`, `severity == Low` |
| `test_kb_parsing` (helper, no plan) | call `parse_kb` directly | `parse_kb("524288 kB") == Some(524288.0)`; `parse_kb("524288") == Some(524288.0)`; `parse_kb("not-a-number") == None`; `parse_kb("") == None` |
| `test_unparseable_space_still_flags_disk` (edge case) | set `"Sort Space Type" = "Disk"`, `"Sort Space Used" = "unknown"` | one `MemorySpill` finding; `severity == Medium` (space_used defaults to 0.0); `evidence["sort_space_used_kb"] == 0.0` |
| `test_metrics_emitted` (metrics) | two-node plan: root with `"Sort Space Used" = "5000 kB"`+`"Sort Space Type"="Disk"`, child with `"Sort Space Used" = "300000 kB"`+`"Sort Space Type"="Disk"` (build via `root.add_child(child)`) | `report.metrics["max_sort_space_used_kb"] == 300000.0`; `report.metrics["nodes_with_sort_spill"] == 2.0`; `report.metrics["nodes_analyzed"] == 2.0` |

Assertion idioms (from `startup_cost.rs`): use
`report.findings.iter().any(|f| matches!(f.finding_type, FindingType::MemorySpill))`
for presence and `report.findings.len()` / `is_empty()` for counts; read
`report.metrics.get("...")` for metric checks.

---

## 5. Post-implementation
Run from repo root (per CLAUDE.md):
```bash
cargo fmt --all
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test
```
