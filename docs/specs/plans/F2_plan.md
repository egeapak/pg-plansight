# F2 — FilterEfficiencyAnalyzer — Implementation Plan

Ready-to-code plan derived from `docs/specs/F2_filter_efficiency.md`. All
accessors below are verified against the real source.

## Verified facts (do not re-derive)

- `Analyzer` trait: `analyze(&self, plan: &ParsedPlan, context: &AnalysisContext) -> AnalysisReport`,
  plus `name()/description()/version()`. (`crates/core/src/analysis/mod.rs`)
- `Finding::new(FindingType, Severity, title: String, description: String, suggestion: String)`
  then builders `.with_node(NodePath)`, `.with_evidence(&str, f64)`, `.with_metadata(&str, &str)`.
- `FindingType` already has `ExcessiveRowProcessing`, `PoorIndexSelectivity`,
  `IneffectiveJoinAlgorithm` variants — no enum changes needed.
- `Severity`: `Low | Medium | High | Critical`.
- Traversal: `PlanTraversal::depth_first(plan, &mut visitor, context)` + `NodeVisitor::visit_node`.
- Node accessors:
  - `node.properties.rows_removed_by_filter() -> Option<u64>`
  - `node.properties.rows_removed_by_join_filter() -> Option<u64>`
  - `node.properties.rows_removed_by_index_recheck() -> Option<u64>`
  - `node.properties.filter() -> Option<&str>`
  - `node.actuals.as_ref().and_then(|a| a.actual_rows) -> Option<u64>`
  - `node.actuals.as_ref().and_then(|a| a.actual_loops) -> Option<u32>`
  - `node.description() -> String`
- `AnalysisReport::new(name).with_metadata(k,v).add_finding(f).with_metric(k, f64)`.
- `with_config(_config: &AnalysisConfiguration)` is the convention (mirrors `StartupCostAnalyzer`).

## Constants / thresholds

```rust
const MIN_ROWS_REMOVED: u64 = 10_000;     // absolute floor for all three rules
const SELECTIVITY_THRESHOLD: f64 = 0.10;  // flag filters keeping < 10%
const SEVERITY_HIGH: u64 = 100_000;       // removed >= 100k => High
const SEVERITY_CRITICAL: u64 = 1_000_000; // removed >= 1M  => Critical
```

Severity-by-`removed` helper (shared by rule 1; rules 2 & 3 are fixed Medium):

```rust
fn severity_for_removed(removed: u64) -> Severity {
    if removed >= SEVERITY_CRITICAL {
        Severity::Critical
    } else if removed >= SEVERITY_HIGH {
        Severity::High
    } else {
        Severity::Medium // removed >= MIN_ROWS_REMOVED guaranteed by caller
    }
}
```

## Selectivity formula (exact)

```rust
// kept = rows that passed the filter (per spec, actual_rows)
// removed = rows_removed_by_filter (or join filter for rule 3)
// selectivity = kept / (kept + removed)
fn selectivity(kept: u64, removed: u64) -> Option<f64> {
    let total = kept.checked_add(removed)?;
    if total == 0 {
        return None; // avoid div-by-zero (spec edge case)
    }
    Some(kept as f64 / total as f64)
}
```

Edge cases (spec):
- No `actuals` (`kept` unknown) → cannot compute selectivity. If `removed >=
  MIN_ROWS_REMOVED`, still flag rule 1 on absolute count, add
  `metadata("selectivity", "unknown")` and omit the `selectivity` evidence key.
- `kept + removed == 0` → skip.
- `removed` below `MIN_ROWS_REMOVED` → skip (all rules).

## Full module skeleton — `crates/core/src/analysis/analyzers/filter_efficiency.rs`

```rust
use super::super::consolidated_config::AnalysisConfiguration;
use super::super::traversal::{NodeVisitor, PlanTraversal};
use super::super::{
    AnalysisContext, AnalysisReport, Analyzer, Finding, FindingType, NodePath, Severity,
};
use crate::{ParsedPlan, PlanNode};

const MIN_ROWS_REMOVED: u64 = 10_000;
const SELECTIVITY_THRESHOLD: f64 = 0.10;
const SEVERITY_HIGH: u64 = 100_000;
const SEVERITY_CRITICAL: u64 = 1_000_000;

/// Detects nodes that read/produce many rows only to discard most of them in a
/// filter — the classic "scan reads 1M rows, keeps 2%" signal pointing at a
/// missing/incomplete index or a filter that should be pushed down.
pub struct FilterEfficiencyAnalyzer;

impl FilterEfficiencyAnalyzer {
    pub fn new() -> Self {
        Self
    }

    pub fn with_config(_config: &AnalysisConfiguration) -> Self {
        Self::new()
    }
}

impl Default for FilterEfficiencyAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer for FilterEfficiencyAnalyzer {
    fn analyze(&self, plan: &ParsedPlan, context: &AnalysisContext) -> AnalysisReport {
        let mut report = AnalysisReport::new("FilterEfficiencyAnalyzer".to_string())
            .with_metadata("version", self.version());

        let mut visitor = FilterEfficiencyVisitor::new();
        PlanTraversal::depth_first(plan, &mut visitor, context);

        for finding in visitor.findings {
            report = report.add_finding(finding);
        }

        report = report
            .with_metric("nodes_analyzed", visitor.nodes_analyzed as f64)
            .with_metric("total_rows_removed", visitor.total_rows_removed as f64)
            .with_metric(
                "nodes_with_wasteful_filter",
                visitor.nodes_with_wasteful_filter as f64,
            )
            // 1.0 sentinel = "no selectivity computed yet"
            .with_metric("min_selectivity_seen", visitor.min_selectivity_seen);

        report
    }

    fn name(&self) -> &'static str {
        "FilterEfficiencyAnalyzer"
    }

    fn description(&self) -> &'static str {
        "Detects filters that discard most of the rows they process (low selectivity)"
    }

    fn version(&self) -> &'static str {
        "1.0.0"
    }
}

struct FilterEfficiencyVisitor {
    findings: Vec<Finding>,
    nodes_analyzed: usize,
    total_rows_removed: u64,
    nodes_with_wasteful_filter: usize,
    min_selectivity_seen: f64,
}

impl FilterEfficiencyVisitor {
    fn new() -> Self {
        Self {
            findings: Vec::new(),
            nodes_analyzed: 0,
            total_rows_removed: 0,
            nodes_with_wasteful_filter: 0,
            min_selectivity_seen: 1.0,
        }
    }

    fn selectivity(kept: u64, removed: u64) -> Option<f64> {
        let total = kept.checked_add(removed)?;
        if total == 0 {
            return None;
        }
        Some(kept as f64 / total as f64)
    }

    fn severity_for_removed(removed: u64) -> Severity {
        if removed >= SEVERITY_CRITICAL {
            Severity::Critical
        } else if removed >= SEVERITY_HIGH {
            Severity::High
        } else {
            Severity::Medium
        }
    }

    // Rule 1: low-selectivity filter (ExcessiveRowProcessing)
    fn check_filter(&mut self, node: &PlanNode, path: &NodePath) {
        let removed = match node.properties.rows_removed_by_filter() {
            Some(r) => r,
            None => return,
        };
        if removed < MIN_ROWS_REMOVED {
            return;
        }
        self.total_rows_removed += removed;

        let kept = node.actuals.as_ref().and_then(|a| a.actual_rows);
        let selectivity = kept.and_then(|k| Self::selectivity(k, removed));

        // With actuals present: require BOTH large removed AND low selectivity.
        // Without actuals: flag on absolute count alone (selectivity unknown).
        let flag = match selectivity {
            Some(s) => s < SELECTIVITY_THRESHOLD,
            None => true,
        };
        if !flag {
            return;
        }

        if let Some(s) = selectivity {
            if s < self.min_selectivity_seen {
                self.min_selectivity_seen = s;
            }
        }
        self.nodes_with_wasteful_filter += 1;

        let severity = Self::severity_for_removed(removed);
        let mut finding = Finding::new(
            FindingType::ExcessiveRowProcessing,
            severity,
            format!("Low-selectivity filter removed {} rows", removed),
            format!(
                "Operation '{}' discarded {} rows in its filter{}. Most rows read are thrown away.",
                node.description(),
                removed,
                match (kept, selectivity) {
                    (Some(k), Some(s)) => {
                        format!(" (kept {}, selectivity {:.1}%)", k, s * 100.0)
                    }
                    _ => " (selectivity unknown — no ANALYZE actuals)".to_string(),
                }
            ),
            "Add or extend an index covering the filter predicate, or push the predicate earlier in the plan".to_string(),
        )
        .with_node(path.clone())
        .with_evidence("rows_removed", removed as f64);

        if let Some(k) = kept {
            finding = finding.with_evidence("rows_kept", k as f64);
        }
        match selectivity {
            Some(s) => finding = finding.with_evidence("selectivity", s),
            None => finding = finding.with_metadata("selectivity", "unknown"),
        }
        if let Some(filter_text) = node.properties.filter() {
            finding = finding.with_metadata("filter", filter_text);
        }

        self.findings.push(finding);
    }

    // Rule 2: weak index, high recheck (PoorIndexSelectivity)
    fn check_index_recheck(&mut self, node: &PlanNode, path: &NodePath) {
        let removed = match node.properties.rows_removed_by_index_recheck() {
            Some(r) => r,
            None => return,
        };
        if removed < MIN_ROWS_REMOVED {
            return;
        }
        self.total_rows_removed += removed;
        self.nodes_with_wasteful_filter += 1;

        let finding = Finding::new(
            FindingType::PoorIndexSelectivity,
            Severity::Medium,
            format!("Imprecise index condition rechecked {} rows", removed),
            format!(
                "Operation '{}' had {} rows removed by index recheck — the index condition is lossy/imprecise.",
                node.description(),
                removed
            ),
            "The index condition is imprecise; consider a more selective or composite index".to_string(),
        )
        .with_node(path.clone())
        .with_evidence("rows_removed_by_index_recheck", removed as f64);

        self.findings.push(finding);
    }

    // Rule 3: expensive join filter (IneffectiveJoinAlgorithm)
    fn check_join_filter(&mut self, node: &PlanNode, path: &NodePath) {
        let removed = match node.properties.rows_removed_by_join_filter() {
            Some(r) => r,
            None => return,
        };
        if removed < MIN_ROWS_REMOVED {
            return;
        }
        self.total_rows_removed += removed;
        self.nodes_with_wasteful_filter += 1;

        let finding = Finding::new(
            FindingType::IneffectiveJoinAlgorithm,
            Severity::Medium,
            format!("Join filter discarded {} rows", removed),
            format!(
                "Operation '{}' removed {} rows with a join filter — the join condition is not fully indexed or produces a large intermediate set that is later filtered.",
                node.description(),
                removed
            ),
            "Ensure the join condition is fully indexed, or restructure the join to reduce the intermediate set".to_string(),
        )
        .with_node(path.clone())
        .with_evidence("rows_removed_by_join_filter", removed as f64);

        self.findings.push(finding);
    }
}

impl NodeVisitor for FilterEfficiencyVisitor {
    fn visit_node(&mut self, node: &PlanNode, path: &NodePath, _context: &AnalysisContext) {
        self.nodes_analyzed += 1;
        self.check_filter(node, path);
        self.check_index_recheck(node, path);
        self.check_join_filter(node, path);
    }
}
```

### Notes on rules → FindingType / Severity

| Rule | Trigger | FindingType | Severity |
|------|---------|-------------|----------|
| 1 Low-selectivity filter | `rows_removed_by_filter >= 10_000` AND (`selectivity < 0.10` OR no actuals) | `ExcessiveRowProcessing` | `severity_for_removed(removed)` → Medium ≥10k, High ≥100k, Critical ≥1M |
| 2 Weak index (recheck) | `rows_removed_by_index_recheck >= 10_000` | `PoorIndexSelectivity` | `Medium` |
| 3 Expensive join filter | `rows_removed_by_join_filter >= 10_000` | `IneffectiveJoinAlgorithm` | `Medium` |

A single node may emit more than one finding (e.g. a bitmap heap scan with both
filter and recheck removals). Each rule is checked independently.

## Registration edits

### 1. `crates/core/src/analysis/analyzers/mod.rs`

Add the module declaration (group with the "new reliable analyzers"):

```rust
// New reliable analyzers (replacing flaky ones)
pub mod filter_efficiency;
pub mod index_usage;
pub mod startup_cost;
```

Add the re-export (keep alphabetical with siblings — insert before `IndexUsageAnalyzer`):

```rust
pub use buffer_analysis::BufferWalAnalyzer;
pub use filter_efficiency::FilterEfficiencyAnalyzer;
pub use index_usage::IndexUsageAnalyzer;
```

### 2. `crates/tui/src/ui/state/log_parsing_state.rs`

Import block (around lines 546–552) — add `FilterEfficiencyAnalyzer`:

```rust
    use pg_plansight_core::analysis::{
        AnalysisContext,
        analyzers::{
            FilterEfficiencyAnalyzer, IndexUsageAnalyzer, JoinAnalyzer, QueryPatternAnalyzer,
            RowEstimationAnalyzer, ScanAnalyzer, StartupCostAnalyzer,
        },
        engine::AnalysisEngineBuilder,
    };
```

Builder chain (around lines 557–562) — add the `add_analyzer` call after `IndexUsageAnalyzer`:

```rust
                        .add_analyzer(StartupCostAnalyzer::new())
                        .add_analyzer(IndexUsageAnalyzer::new())
                        .add_analyzer(FilterEfficiencyAnalyzer::new())
                        .build();
```

### 3. `crates/tui/src/ui/state/query_detail_view.rs`

Import block (lines 8–12) — add `FilterEfficiencyAnalyzer`:

```rust
use pg_plansight_core::analysis::{
    analyzers::{
        FilterEfficiencyAnalyzer, IndexUsageAnalyzer, JoinAnalyzer, QueryPatternAnalyzer,
        RowEstimationAnalyzer, ScanAnalyzer, StartupCostAnalyzer,
    },
    consolidated_config::AnalysisConfiguration,
    engine::{AnalysisEngine, AnalysisEngineBuilder, EngineResult},
```

Builder chain (lines 80–82) — add after `IndexUsageAnalyzer`:

```rust
            .add_analyzer(StartupCostAnalyzer::new())
            .add_analyzer(IndexUsageAnalyzer::new())
            .add_analyzer(FilterEfficiencyAnalyzer::new())
            .build();
```

## Test plan — `#[cfg(test)] mod tests` in `filter_efficiency.rs`

### Test helpers

Builders construct a `PlanNode` with a `SeqScan` (or `NestedLoop`) `NodeType`,
zeroed `PlanCost`, then set properties via `set_property` and actuals via
`set_actuals`. Imports: `use super::*;` and
`use crate::{NodeType, PlanActuals, PlanCost, PlanNode, ScanType, TableReference};`
(add `JoinType` for the join test).

```rust
fn seq_scan(name: &str) -> PlanNode {
    PlanNode::new(
        NodeType::Scan(ScanType::SeqScan {
            table: TableReference { schema: None, name: name.to_string(), alias: None },
        }),
        PlanCost {
            startup_cost: 0.0,
            min_total_cost: 0.0,
            max_total_cost: 1000.0,
            estimated_rows: 1000,
            estimated_width: 50,
        },
        format!("Seq Scan on {name}"),
    )
}

fn with_actual_rows(mut node: PlanNode, rows: u64) -> PlanNode {
    node.set_actuals(PlanActuals {
        actual_time_ms: Some(1.0),
        actual_rows: Some(rows),
        actual_loops: Some(1),
    });
    node
}

fn run(node: PlanNode) -> AnalysisReport {
    FilterEfficiencyAnalyzer::new().analyze(&ParsedPlan::new(node), &AnalysisContext::new())
}
```

`set_property` parses keys via `PlanProperty::from_key_value`, so the property
strings must match PostgreSQL labels exactly:
- `"Rows Removed by Filter"`
- `"Rows Removed by Index Recheck"`
- `"Rows Removed by Join Filter"`
- `"Filter"`

### Positive tests

1. **`test_low_selectivity_seq_scan_flagged`**
   - Node: `seq_scan("t")`, `set_property("Rows Removed by Filter", "998000")`,
     `set_property("Filter", "(status = 1)")`, actuals `actual_rows = 2000`.
   - Selectivity = 2000 / 1_000_000 = 0.002 < 0.10; removed 998_000 ≥ 100_000.
   - Assert: a finding with `FindingType::ExcessiveRowProcessing` exists, its
     `severity == Severity::High`, `evidence["rows_removed"] == 998000.0`,
     `evidence["rows_kept"] == 2000.0`, and `metadata["filter"] == "(status = 1)"`.

2. **`test_low_selectivity_critical_severity`** (covers the ≥1M boundary)
   - Node: `seq_scan("t")`, `Rows Removed by Filter = 1_500_000`,
     actuals `actual_rows = 2000`.
   - Assert: `ExcessiveRowProcessing` finding with `severity == Severity::Critical`.

3. **`test_index_recheck_flagged`**
   - Node: `seq_scan("t")` (or bitmap heap scan), `set_property("Rows Removed by
     Index Recheck", "50000")`.
   - Assert: exactly one finding, `FindingType::PoorIndexSelectivity`,
     `severity == Severity::Medium`,
     `evidence["rows_removed_by_index_recheck"] == 50000.0`.

4. **`test_join_filter_flagged`**
   - Node: `NodeType::Join(JoinType::NestedLoop { inner_unique: false })` with the
     same zeroed cost, `set_property("Rows Removed by Join Filter", "80000")`.
   - Assert: one finding, `FindingType::IneffectiveJoinAlgorithm`,
     `severity == Severity::Medium`,
     `evidence["rows_removed_by_join_filter"] == 80000.0`.

5. **`test_filter_no_actuals_flagged_unknown_selectivity`** (edge case)
   - Node: `seq_scan("t")`, `Rows Removed by Filter = 200000`, NO `set_actuals`.
   - Assert: one `ExcessiveRowProcessing` finding,
     `severity == Severity::High`, `metadata["selectivity"] == "unknown"`,
     and `evidence` does NOT contain key `"selectivity"`.

### Negative tests (assert no relevant finding)

6. **`test_selective_filter_no_finding`**
   - Node: `seq_scan("t")`, `Rows Removed by Filter = 1000`, actuals
     `actual_rows = 9000` (selectivity 0.9). Removed 1000 < 10_000.
   - Assert: `report.findings.is_empty()` (below absolute threshold → skipped).

7. **`test_high_selectivity_above_threshold_no_finding`**
   - Node: `seq_scan("t")`, `Rows Removed by Filter = 50000`, actuals
     `actual_rows = 950000` → selectivity 0.95 ≥ 0.10 despite large removed.
   - Assert: no `ExcessiveRowProcessing` finding (removed large but filter is
     effective; selectivity gate prevents the flag).

8. **`test_small_absolute_removed_no_finding`**
   - Node: `seq_scan("t")`, `Rows Removed by Filter = 500`, actuals
     `actual_rows = 10` (selectivity low but removed below 10_000).
   - Assert: `report.findings.is_empty()`.

9. **`test_no_filter_properties_no_finding`**
   - Node: `seq_scan("t")` with no rows-removed properties at all.
   - Assert: `report.findings.is_empty()`, and metric
     `metrics["nodes_analyzed"] == 1.0`.

10. **`test_zero_kept_and_removed_skipped`** (div-by-zero edge case)
    - Node: `seq_scan("t")`, `Rows Removed by Filter = 0`, actuals
      `actual_rows = 0`. Removed 0 < 10_000 so the threshold gate already skips;
      this test documents that the `kept+removed==0` path never panics.
    - Assert: `report.findings.is_empty()`.

### Assertion idiom (matches `startup_cost.rs`)

```rust
assert!(report.findings.iter().any(|f|
    matches!(f.finding_type, FindingType::ExcessiveRowProcessing)));
// severity / evidence:
let f = report.findings.iter()
    .find(|f| matches!(f.finding_type, FindingType::ExcessiveRowProcessing))
    .expect("expected finding");
assert_eq!(f.severity, Severity::High);
assert_eq!(f.evidence.get("rows_removed"), Some(&998_000.0));
```

## Post-implementation checks (from CLAUDE.md)

```bash
cargo fmt --all
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test -p pg-plansight-core filter_efficiency
```
