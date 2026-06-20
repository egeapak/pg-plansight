# F3 Implementation Plan — IndexEfficiencyAnalyzer

Converts `docs/specs/F3_index_efficiency.md` into a precise, ready-to-implement plan.

## Summary of what is built

A new analyzer `IndexEfficiencyAnalyzer` (+ `IndexEfficiencyVisitor`) living in
`crates/core/src/analysis/analyzers/index_efficiency.rs`. It walks every plan node and emits:

1. **`IndexOnlyScanHeapFetches`** — index-only scans hitting the heap because the visibility map is stale.
2. **`MemorySpill`** — bitmap heap scans that went lossy because the bitmap exceeded `work_mem`.

It registers like every other analyzer (constructed via `::new()`, follows the `Analyzer` trait,
uses `PlanTraversal::depth_first` + `NodeVisitor`), and is wired into the two TUI analysis-engine
builders.

---

## 1. Verified accessors / types (confirmed against source)

These are the REAL APIs used by the skeleton below — do not invent others.

- `node.properties()` -> `&PlanProperties` (`crates/core/src/plan_parser.rs:641`)
- `PlanProperties::heap_fetches(&self) -> Option<u64>` (`plan_properties.rs:499`)
- `PlanProperties::heap_blocks_lossy(&self) -> Option<u64>` (`plan_properties.rs:492`)
- `PlanProperties::heap_blocks_exact(&self) -> Option<u64>` (`plan_properties.rs:485`)
- `node.actuals: Option<PlanActuals>` (`plan_parser.rs:587`); `PlanActuals.actual_rows: Option<u64>` (`plan_parser.rs:55`)
- `node.description() -> String` (`plan_parser.rs:805`)
- `node.set_actuals(PlanActuals)` (`plan_parser.rs:651`) — used in tests
- `node.properties_mut().set_property(PlanProperty::HeapFetches(u64))` etc. — used in tests
- `PlanNode::new(node_type, cost, original_text: String)` (`plan_parser.rs:614`)
- `ParsedPlan::new(root)` — used in tests (see `startup_cost.rs:228`)
- Finding builder: `Finding::new(FindingType, Severity, title, description, suggestion)`
  then `.with_node(path.clone())`, `.with_evidence("k", f64)`, `.with_metadata("k","v")`
  (`analysis/mod.rs:104-138`)
- `FindingType::Custom(String)` and `FindingType::MemorySpill` (`analysis/mod.rs:44,53`)
- `Severity::{Medium, High}` (`analysis/mod.rs:11`)
- Traversal/visitor imports mirror `startup_cost.rs:1-6`.

> Note: heap properties live on the node regardless of `node_type`. The rules are gated by the
> *presence* of the relevant properties (per spec "Properties absent -> skip node"), not by matching
> `ScanType::IndexScan { only: true }` / `ScanType::BitmapHeapScan`. This keeps detection robust to
> parser variations and matches the spec's Inputs section (uses accessors, not node-type matching).

---

## 2. Full module skeleton — `crates/core/src/analysis/analyzers/index_efficiency.rs`

```rust
use super::super::consolidated_config::AnalysisConfiguration;
use super::super::traversal::{NodeVisitor, PlanTraversal};
use super::super::{
    AnalysisContext, AnalysisReport, Analyzer, Finding, FindingType, NodePath, Severity,
};
use crate::{ParsedPlan, PlanNode};

// ── Thresholds (per spec §"Detection rules") ────────────────────────────────
/// Minimum absolute heap fetches before an index-only scan is flagged.
const MIN_HEAP_FETCHES: u64 = 1_000;
/// Heap fetches must be at least this fraction of actual_rows (only checked when actuals present).
const HEAP_FETCH_RATIO_THRESHOLD: f64 = 0.10;
/// At/above this many heap fetches the finding escalates to High severity.
const HIGH_HEAP_FETCHES: u64 = 100_000;
/// Minimum absolute lossy blocks before the bitmap rule even considers a node.
const MIN_LOSSY_BLOCKS: u64 = 1;
/// Lossy share of (lossy + exact) must exceed this fraction.
const LOSSY_FRACTION_THRESHOLD: f64 = 0.05;
/// Absolute guard: ignore a handful of lossy blocks on a huge otherwise-exact scan.
const MIN_ABSOLUTE_LOSSY_FOR_FLAG: u64 = 64;

/// Analyzer for detecting two concrete index inefficiencies:
/// stale-visibility-map heap fetches on index-only scans, and lossy bitmap heap scans.
pub struct IndexEfficiencyAnalyzer;

impl IndexEfficiencyAnalyzer {
    pub fn new() -> Self {
        Self
    }

    pub fn with_config(_config: &AnalysisConfiguration) -> Self {
        Self::new()
    }
}

impl Default for IndexEfficiencyAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer for IndexEfficiencyAnalyzer {
    fn analyze(&self, plan: &ParsedPlan, context: &AnalysisContext) -> AnalysisReport {
        let mut report = AnalysisReport::new("IndexEfficiencyAnalyzer".to_string())
            .with_metadata("version", self.version());

        let mut visitor = IndexEfficiencyVisitor::new();
        PlanTraversal::depth_first(plan, &mut visitor, context);

        for finding in visitor.findings {
            report = report.add_finding(finding);
        }

        report = report
            .with_metric("nodes_analyzed", visitor.nodes_analyzed as f64)
            .with_metric("total_heap_fetches", visitor.total_heap_fetches as f64)
            .with_metric("total_lossy_blocks", visitor.total_lossy_blocks as f64);

        report
    }

    fn name(&self) -> &'static str {
        "IndexEfficiencyAnalyzer"
    }

    fn description(&self) -> &'static str {
        "Detects index-only scans with stale visibility maps and lossy bitmap heap scans"
    }

    fn version(&self) -> &'static str {
        "1.0.0"
    }
}

struct IndexEfficiencyVisitor {
    findings: Vec<Finding>,
    nodes_analyzed: usize,
    total_heap_fetches: u64,
    total_lossy_blocks: u64,
}

impl IndexEfficiencyVisitor {
    fn new() -> Self {
        Self {
            findings: Vec::new(),
            nodes_analyzed: 0,
            total_heap_fetches: 0,
            total_lossy_blocks: 0,
        }
    }

    /// Rule 1: index-only scan doing many heap fetches (stale visibility map).
    fn detect_index_only_heap_fetches(&mut self, node: &PlanNode, path: &NodePath) {
        // Property absent → skip node.
        let Some(heap_fetches) = node.properties().heap_fetches() else {
            return;
        };
        self.total_heap_fetches += heap_fetches;

        if heap_fetches < MIN_HEAP_FETCHES {
            return;
        }

        // actual_rows context (optional). When present, require fetches to be a large
        // fraction of rows so we don't flag a scan that legitimately fetches a lot.
        let actual_rows = node.actuals.as_ref().and_then(|a| a.actual_rows);
        let heap_fetch_ratio = match actual_rows {
            Some(rows) if rows > 0 => {
                let ratio = heap_fetches as f64 / rows as f64;
                if ratio <= HEAP_FETCH_RATIO_THRESHOLD {
                    return; // fetches are a small share of rows → not the stale-VM pattern
                }
                ratio
            }
            // rows == 0 or actuals absent → ratio not computable; fall through on absolute count.
            _ => 0.0,
        };

        let severity = if heap_fetches >= HIGH_HEAP_FETCHES {
            Severity::High
        } else {
            Severity::Medium
        };

        let mut finding = Finding::new(
            FindingType::Custom("IndexOnlyScanHeapFetches".to_string()),
            severity,
            format!("Index-only scan performing {heap_fetches} heap fetches"),
            format!(
                "Operation '{}' performed {} heap fetches during an index-only scan, \
                 indicating a stale visibility map forcing heap access.",
                node.description(),
                heap_fetches
            ),
            "Run VACUUM (or VACUUM ANALYZE) on the table to refresh the visibility map. \
             Consider more aggressive autovacuum settings on hot tables."
                .to_string(),
        )
        .with_node(path.clone())
        .with_evidence("heap_fetches", heap_fetches as f64)
        .with_evidence("heap_fetch_ratio", heap_fetch_ratio);

        if let Some(rows) = actual_rows {
            finding = finding.with_evidence("actual_rows", rows as f64);
        }

        self.findings.push(finding);
    }

    /// Rule 2: lossy bitmap heap scan blocks (bitmap exceeded work_mem).
    fn detect_lossy_bitmap(&mut self, node: &PlanNode, path: &NodePath) {
        // Property absent → skip node.
        let Some(lossy) = node.properties().heap_blocks_lossy() else {
            return;
        };
        self.total_lossy_blocks += lossy;

        if lossy < MIN_LOSSY_BLOCKS {
            return;
        }

        let exact = node.properties().heap_blocks_exact().unwrap_or(0);
        let total = lossy + exact;
        if total == 0 {
            return; // lossy + exact == 0 → skip fraction calc.
        }

        let lossy_fraction = lossy as f64 / total as f64;

        // Flag only when lossy is a meaningful share AND the absolute lossy count is non-trivial.
        if lossy_fraction <= LOSSY_FRACTION_THRESHOLD || lossy < MIN_ABSOLUTE_LOSSY_FOR_FLAG {
            return;
        }

        let finding = Finding::new(
            FindingType::MemorySpill,
            Severity::Medium,
            format!("Lossy bitmap heap scan ({lossy} lossy blocks)"),
            format!(
                "Operation '{}' produced {} lossy heap blocks ({} exact); the bitmap exceeded \
                 work_mem and became lossy, forcing per-row rechecks.",
                node.description(),
                lossy,
                exact
            ),
            "Raise work_mem so the bitmap stays exact, or add a more selective index to shrink \
             the bitmap."
                .to_string(),
        )
        .with_node(path.clone())
        .with_evidence("heap_blocks_lossy", lossy as f64)
        .with_evidence("heap_blocks_exact", exact as f64)
        .with_evidence("lossy_fraction", lossy_fraction);

        self.findings.push(finding);
    }
}

impl NodeVisitor for IndexEfficiencyVisitor {
    fn visit_node(&mut self, node: &PlanNode, path: &NodePath, _context: &AnalysisContext) {
        self.nodes_analyzed += 1;
        self.detect_index_only_heap_fetches(node, path);
        self.detect_lossy_bitmap(node, path);
    }
}
```

### Rule → FindingType / Severity table

| Rule | Condition | FindingType | Severity |
|------|-----------|-------------|----------|
| Index-only heap fetches | `heap_fetches >= 1_000` AND (if `actual_rows` present & > 0) `heap_fetches/actual_rows > 0.10` | `FindingType::Custom("IndexOnlyScanHeapFetches")` | `Medium`, escalates to `High` when `heap_fetches >= 100_000` |
| Lossy bitmap | `heap_blocks_lossy >= 1` AND `lossy/(lossy+exact) > 0.05` AND `lossy >= 64` | `FindingType::MemorySpill` | `Medium` |

Metrics emitted: `nodes_analyzed`, `total_heap_fetches`, `total_lossy_blocks`.

---

## 3. Registration edits

### 3a. `crates/core/src/analysis/analyzers/mod.rs`

Add the module declaration (group with the "New reliable analyzers" block) and the re-export.

```rust
// New reliable analyzers (replacing flaky ones)
pub mod index_efficiency;   // <-- ADD
pub mod index_usage;
pub mod startup_cost;
```

```rust
pub use index_efficiency::IndexEfficiencyAnalyzer;   // <-- ADD (alphabetical near index_usage)
pub use index_usage::IndexUsageAnalyzer;
```

### 3b. `crates/tui/src/ui/state/log_parsing_state.rs`

In `start_heavy_processing` (the `use pg_plansight_core::analysis::{...}` import inside the
`par_iter_mut` closure, ~line 546-553) add `IndexEfficiencyAnalyzer` to the imported analyzer list:

```rust
        analyzers::{
            IndexEfficiencyAnalyzer, IndexUsageAnalyzer, JoinAnalyzer, QueryPatternAnalyzer,
            RowEstimationAnalyzer, ScanAnalyzer, StartupCostAnalyzer,
        },
```

And in the builder chain (~line 556-563) add the analyzer:

```rust
                    let analysis_engine = AnalysisEngineBuilder::new()
                        .add_analyzer(RowEstimationAnalyzer::new())
                        .add_analyzer(ScanAnalyzer::new())
                        .add_analyzer(JoinAnalyzer::new())
                        .add_analyzer(QueryPatternAnalyzer::new())
                        .add_analyzer(StartupCostAnalyzer::new())
                        .add_analyzer(IndexUsageAnalyzer::new())
                        .add_analyzer(IndexEfficiencyAnalyzer::new())   // <-- ADD
                        .build();
```

### 3c. `crates/tui/src/ui/state/query_detail_view.rs`

Update the top-of-file import (lines 8-15) to include `IndexEfficiencyAnalyzer`:

```rust
use pg_plansight_core::analysis::{
    analyzers::{
        IndexEfficiencyAnalyzer, IndexUsageAnalyzer, JoinAnalyzer, QueryPatternAnalyzer,
        RowEstimationAnalyzer, ScanAnalyzer, StartupCostAnalyzer,
    },
    consolidated_config::AnalysisConfiguration,
    engine::{AnalysisEngine, AnalysisEngineBuilder, EngineResult},
};
```

And in `QueryDetailView::new()` builder (lines 75-82) add the analyzer:

```rust
        let analysis_engine = AnalysisEngineBuilder::new()
            .add_analyzer(RowEstimationAnalyzer::new())
            .add_analyzer(ScanAnalyzer::new())
            .add_analyzer(JoinAnalyzer::new())
            .add_analyzer(QueryPatternAnalyzer::new())
            .add_analyzer(StartupCostAnalyzer::new())
            .add_analyzer(IndexUsageAnalyzer::new())
            .add_analyzer(IndexEfficiencyAnalyzer::new())   // <-- ADD
            .build();
```

> No other registration site exists — confirmed only these two TUI builders plus the `analyzers/mod.rs`
> re-export reference the analyzer set. The exporter crate and core engine do not hardcode this list.

---

## 4. Tests — `#[cfg(test)] mod tests` at the bottom of `index_efficiency.rs`

### Test helpers

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        NodeType, PlanActuals, PlanCost, PlanNode, ScanType, TableReference,
    };
    use crate::plan_properties::PlanProperty;

    fn cost() -> PlanCost {
        PlanCost {
            startup_cost: 0.0,
            min_total_cost: 1.0,
            max_total_cost: 100.0,
            estimated_rows: 1000,
            estimated_width: 32,
        }
    }

    /// Index-only scan node with the given heap fetches and optional actual rows.
    fn index_only_node(heap_fetches: u64, actual_rows: Option<u64>) -> PlanNode {
        let mut node = PlanNode::new(
            NodeType::Scan(ScanType::IndexScan {
                table: TableReference { schema: None, name: "t".to_string(), alias: None },
                index: None,
                backward: false,
                only: true,
            }),
            cost(),
            "Index Only Scan using ix on t".to_string(),
        );
        node.properties_mut().set_property(PlanProperty::HeapFetches(heap_fetches));
        if let Some(rows) = actual_rows {
            node.set_actuals(PlanActuals {
                actual_time_ms: Some(1.0),
                actual_rows: Some(rows),
                actual_loops: Some(1),
            });
        }
        node
    }

    /// Bitmap heap scan node with given lossy / exact heap blocks.
    fn bitmap_node(lossy: u64, exact: u64) -> PlanNode {
        let mut node = PlanNode::new(
            NodeType::Scan(ScanType::BitmapHeapScan {
                table: TableReference { schema: None, name: "t".to_string(), alias: None },
                recheck_condition: None,
            }),
            cost(),
            "Bitmap Heap Scan on t".to_string(),
        );
        node.properties_mut().set_property(PlanProperty::HeapBlocksLossy(lossy));
        node.properties_mut().set_property(PlanProperty::HeapBlocksExact(exact));
        node
    }

    fn analyze(node: PlanNode) -> AnalysisReport {
        let analyzer = IndexEfficiencyAnalyzer::new();
        let context = AnalysisContext::new();
        analyzer.analyze(&ParsedPlan::new(node), &context)
    }

    fn has_finding(report: &AnalysisReport, ft: &FindingType) -> bool {
        report.findings.iter().any(|f| &f.finding_type == ft)
    }
```

### Positive + negative test cases

```rust
    // ── Rule 1: index-only scan heap fetches ───────────────────────────────

    /// Spec: HeapFetches=200_000, actual_rows=210_000 → IndexOnlyScanHeapFetches, High.
    #[test]
    fn test_index_only_scan_heap_fetches_flagged() {
        let report = analyze(index_only_node(200_000, Some(210_000)));
        let want = FindingType::Custom("IndexOnlyScanHeapFetches".to_string());
        assert!(has_finding(&report, &want));
        let f = report.findings.iter().find(|f| f.finding_type == want).unwrap();
        assert_eq!(f.severity, Severity::High); // 200_000 >= 100_000
        assert_eq!(f.evidence.get("heap_fetches"), Some(&200_000.0));
        assert_eq!(f.evidence.get("actual_rows"), Some(&210_000.0));
        // ratio ≈ 0.952 (> 0.10)
        assert!(f.evidence.get("heap_fetch_ratio").unwrap() > &0.10);
    }

    /// Negative spec case: HeapFetches=5 → no finding (below MIN_HEAP_FETCHES).
    #[test]
    fn test_index_only_scan_few_fetches_no_finding() {
        let report = analyze(index_only_node(5, Some(1_000)));
        let want = FindingType::Custom("IndexOnlyScanHeapFetches".to_string());
        assert!(!has_finding(&report, &want));
    }

    /// Medium-severity boundary: 5_000 fetches over 6_000 rows (ratio 0.83) → Medium.
    #[test]
    fn test_index_only_scan_medium_severity() {
        let report = analyze(index_only_node(5_000, Some(6_000)));
        let want = FindingType::Custom("IndexOnlyScanHeapFetches".to_string());
        let f = report.findings.iter().find(|f| f.finding_type == want).unwrap();
        assert_eq!(f.severity, Severity::Medium); // 5_000 < 100_000
    }

    /// Large absolute fetches but tiny ratio of rows → suppressed by ratio guard.
    #[test]
    fn test_index_only_scan_low_ratio_no_finding() {
        // 2_000 fetches over 1_000_000 rows → ratio 0.002 (< 0.10)
        let report = analyze(index_only_node(2_000, Some(1_000_000)));
        let want = FindingType::Custom("IndexOnlyScanHeapFetches".to_string());
        assert!(!has_finding(&report, &want));
    }

    /// Actuals absent: falls back to absolute threshold only → flagged.
    #[test]
    fn test_index_only_scan_no_actuals_uses_absolute() {
        let report = analyze(index_only_node(50_000, None));
        let want = FindingType::Custom("IndexOnlyScanHeapFetches".to_string());
        assert!(has_finding(&report, &want));
    }

    /// No heap-fetches property at all → node skipped, no finding.
    #[test]
    fn test_no_heap_fetches_property_no_finding() {
        let node = PlanNode::new(
            NodeType::Scan(ScanType::IndexScan {
                table: TableReference { schema: None, name: "t".to_string(), alias: None },
                index: None,
                backward: false,
                only: true,
            }),
            cost(),
            "Index Only Scan using ix on t".to_string(),
        );
        let want = FindingType::Custom("IndexOnlyScanHeapFetches".to_string());
        assert!(!has_finding(&analyze(node), &want));
    }

    // ── Rule 2: lossy bitmap heap scan ─────────────────────────────────────

    /// Spec: HeapBlocksLossy=5_000, HeapBlocksExact=1_000 → MemorySpill, Medium.
    #[test]
    fn test_lossy_bitmap_flagged() {
        let report = analyze(bitmap_node(5_000, 1_000));
        assert!(has_finding(&report, &FindingType::MemorySpill));
        let f = report
            .findings
            .iter()
            .find(|f| f.finding_type == FindingType::MemorySpill)
            .unwrap();
        assert_eq!(f.severity, Severity::Medium);
        assert_eq!(f.evidence.get("heap_blocks_lossy"), Some(&5_000.0));
        assert_eq!(f.evidence.get("heap_blocks_exact"), Some(&1_000.0));
        // fraction = 5000/6000 ≈ 0.833
        assert!(f.evidence.get("lossy_fraction").unwrap() > &0.05);
    }

    /// Spec negative: HeapBlocksLossy=2, HeapBlocksExact=100_000 → no finding
    /// (fails both the 0.05 fraction guard and the 64-block absolute guard).
    #[test]
    fn test_mostly_exact_bitmap_no_finding() {
        let report = analyze(bitmap_node(2, 100_000));
        assert!(!has_finding(&report, &FindingType::MemorySpill));
    }

    /// Fraction passes (high lossy share) but absolute lossy < 64 → suppressed.
    #[test]
    fn test_small_absolute_lossy_no_finding() {
        // 10 lossy / 10 exact → fraction 0.5 (> 0.05) but 10 < 64 absolute guard.
        let report = analyze(bitmap_node(10, 10));
        assert!(!has_finding(&report, &FindingType::MemorySpill));
    }

    /// Absolute count passes (>= 64) and fraction passes → flagged.
    #[test]
    fn test_lossy_at_absolute_threshold_flagged() {
        // 64 lossy / 100 exact → fraction ≈ 0.39 (> 0.05), 64 >= 64.
        let report = analyze(bitmap_node(64, 100));
        assert!(has_finding(&report, &FindingType::MemorySpill));
    }

    /// No bitmap properties → node skipped, no MemorySpill.
    #[test]
    fn test_no_bitmap_properties_no_finding() {
        let node = PlanNode::new(
            NodeType::Scan(ScanType::BitmapHeapScan {
                table: TableReference { schema: None, name: "t".to_string(), alias: None },
                recheck_condition: None,
            }),
            cost(),
            "Bitmap Heap Scan on t".to_string(),
        );
        assert!(!has_finding(&analyze(node), &FindingType::MemorySpill));
    }

    /// Metrics are populated.
    #[test]
    fn test_metrics_populated() {
        let report = analyze(bitmap_node(5_000, 1_000));
        assert_eq!(report.metrics.get("nodes_analyzed"), Some(&1.0));
        assert_eq!(report.metrics.get("total_lossy_blocks"), Some(&5_000.0));
    }
}
```

> The four spec-named tests (`test_index_only_scan_heap_fetches_flagged`,
> `test_index_only_scan_few_fetches_no_finding`, `test_lossy_bitmap_flagged`,
> `test_mostly_exact_bitmap_no_finding`) are present verbatim; the remainder cover the severity
> boundary, ratio guard, absent-actuals fallback, absent-property skips, the 64-block absolute guard,
> and metric emission.

---

## 5. Implementation order & validation

1. Create `index_efficiency.rs` with the skeleton (§2) and tests (§4).
2. Edit `analyzers/mod.rs` (§3a).
3. Edit the two TUI builders (§3b, §3c).
4. `cargo test -p pg-plansight-core index_efficiency`
5. `cargo build` (verifies TUI wiring).
6. `cargo fmt --all`
7. `cargo clippy --workspace --all-features --all-targets -- -D warnings`

### Edge cases covered (per spec §"Edge cases")
- Properties absent → `let Some(...) else { return };` in both rules.
- `lossy + exact == 0` → explicit `if total == 0 { return; }` guard.
- `actual_rows == 0` or absent → ratio not computed; rule falls back to the absolute fetch threshold.
