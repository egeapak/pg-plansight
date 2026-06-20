# F5 — EstimationHealthAnalyzer — Implementation Plan

Source spec: `docs/specs/F5_estimation_health.md`.

A **whole-plan** analyzer that classifies the *pattern* of planner row-estimate
error: systematic skew (stale stats → `ANALYZE`) vs a single anomalous node
(data skew / correlated predicates). It emits at most a couple of plan-level
findings plus summary metrics — it does NOT re-flag individual nodes.

---

## 1. How this differs from `row_estimation.rs` (do NOT duplicate)

`RowEstimationAnalyzer` (`analyzers/row_estimation.rs`) is **per-node**: its
`RowEstimationVisitor` pushes one finding *per offending node*
(`FindingType::ExcessiveRowProcessing`, `CartesianProduct`) and works off
`node.cost.estimated_rows` only (estimate-side; it does not compare against
actuals). It answers "which nodes process/estimate too many rows".

F5 is **plan-level and actual-vs-estimate**:
- It compares `node.cost.estimated_rows` against `node.actuals.actual_rows`
  (requires a plan run with `ANALYZE`; estimate-only plans yield no actuals →
  metrics only, no findings).
- It aggregates across the whole tree and emits **at most two** findings using
  brand-new `FindingType::Custom` strings that no other analyzer uses:
  `Custom("EstimationPatternSystematic")` and `Custom("EstimationPatternOutlier")`.
  These never collide with `RowEstimationError` / `ExcessiveRowProcessing`.
- It is **config-less** (a unit struct, modeled on `StartupCostAnalyzer`), so
  there is no `RowEstimationConfig` coupling and no `consolidated_config` edits.

Net: the two analyzers can both run in the same engine; F5 adds a diagnosis of
*pattern* on top of the per-node flags, without flagging the same nodes again.

---

## 2. Ratio math, bands, and detection rules

Per node, only when `node.actuals` is `Some` and `actuals.actual_rows` is
`Some`:

```text
estimated = node.cost.estimated_rows            // u64
actual    = actuals.actual_rows.unwrap()         // u64
est_f  = max(estimated, 1) as f64                // guard divide-by-zero
act_f  = max(actual, 1)    as f64
ratio  = act_f / est_f                            // >1 => under-estimate (actual exceeds est)
                                                  // <1 => over-estimate
misestimate = ratio.max(1.0 / ratio)             // always >= 1.0, direction-agnostic magnitude
```

**Per-loop caveat (documented in code):** PostgreSQL's `actual rows` is
per-loop while `estimated rows` is also per-loop, so a direct comparison is
correct for the common case. We do NOT multiply by `actual_loops`; this is
noted in a doc comment to keep the heuristic simple and robust (per spec).

**Significance band:** a node is "accurate enough" when `ratio` is within
`[0.5, 2.0]` (i.e. `misestimate < 2.0`). Constants:

```rust
const BAND_LOW: f64 = 0.5;
const BAND_HIGH: f64 = 2.0;          // ratio inside [BAND_LOW, BAND_HIGH] => not significant
const SYSTEMATIC_MIN_NODES: usize = 4;       // need >= 4 estimating nodes
const SYSTEMATIC_DIR_FRACTION: f64 = 0.8;    // >= 80% skew same direction
const SYSTEMATIC_AVG_RATIO: f64 = 3.0;       // avg misestimate magnitude >= 3
const OUTLIER_RATIO: f64 = 50.0;             // single-node misestimate >= 50
```

Direction classification (only for nodes OUTSIDE the band, i.e.
`misestimate >= BAND_HIGH`):
- `ratio > BAND_HIGH`  → **under-estimated** (`nodes_underestimated += 1`)
- `ratio < BAND_LOW`   → **over-estimated**  (`nodes_overestimated += 1`)

`significant_count = nodes_overestimated + nodes_underestimated`.

### Rule 1 — Systematic misestimation → `Custom("EstimationPatternSystematic")`
Fire when ALL of:
- `nodes_with_actuals >= SYSTEMATIC_MIN_NODES` (4), AND
- `significant_count >= 1` (at least one node out of band), AND
- the dominant direction count `max(nodes_overestimated, nodes_underestimated)`
  is `>= SYSTEMATIC_DIR_FRACTION * significant_count` (≥ 80% of *significant*
  nodes skew the same way), AND
- `avg_misestimate_of_significant >= SYSTEMATIC_AVG_RATIO` (3.0), where the
  average is over the significant nodes only.

Severity: `High` if `avg_misestimate >= 10.0`, else `Medium`.
Suggestion: stats look stale or a column needs extended statistics — run
`ANALYZE` (or `CREATE STATISTICS` for correlated columns).
Evidence: `nodes_overestimated`, `nodes_underestimated`, `avg_ratio`,
`nodes_with_actuals`. Metadata: `direction` = "under" | "over". Plan-level
finding (no `with_node`, or attach root path `NodePath::root()`).

### Rule 2 — Single-node outlier → `Custom("EstimationPatternOutlier")`
Fire when:
- exactly ONE node has `misestimate >= OUTLIER_RATIO` (50), AND
- every OTHER node with actuals is within band (`misestimate < BAND_HIGH`).

(If Rule 1 already fired, still allowed to also fire Rule 2 only if its
"exactly one extreme + rest in band" precondition holds — in practice the two
are mutually exclusive because Rule 1 requires ≥4 significant-ish nodes while
Rule 2 requires the rest in-band. Evaluate Rule 2 independently; they will not
both trigger on the test fixtures.)

Severity: `Medium`.
Affected node: the outlier's `NodePath` (captured during traversal).
Suggestion: localized estimate error → likely data skew or correlated
predicates on that relation; consider `CREATE STATISTICS` or a partial index.
Evidence: `max_misestimate_ratio`.

### No actuals anywhere
If `nodes_with_actuals == 0` → push NO findings, only emit metrics (this plan
was not run with `ANALYZE`).

---

## 3. Computed metrics (always emitted)

- `nodes_with_actuals` (usize → f64)
- `nodes_overestimated`
- `nodes_underestimated`
- `max_misestimate_ratio` (worst `misestimate` magnitude seen; 0.0 if none)
- `root_misestimate_ratio` (`misestimate` of the root node, or 0.0 if root has
  no actuals)

---

## 4. Module skeleton — `crates/core/src/analysis/analyzers/estimation_health.rs`

Modeled on `startup_cost.rs` (config-less unit struct, visitor pattern). All
fields below are real and match the existing types.

```rust
use super::super::traversal::{NodeVisitor, PlanTraversal};
use super::super::{
    AnalysisContext, AnalysisReport, Analyzer, Finding, FindingType, NodePath, Severity,
};
use crate::{ParsedPlan, PlanNode};

// --- Tunable thresholds -----------------------------------------------------
const BAND_LOW: f64 = 0.5;
const BAND_HIGH: f64 = 2.0;
const SYSTEMATIC_MIN_NODES: usize = 4;
const SYSTEMATIC_DIR_FRACTION: f64 = 0.8;
const SYSTEMATIC_AVG_RATIO: f64 = 3.0;
const OUTLIER_RATIO: f64 = 50.0;

/// Plan-level analyzer that classifies the *pattern* of planner row-estimate
/// error (systematic stale-stats skew vs single anomalous node). Complements
/// the per-node `RowEstimationAnalyzer`; uses distinct Custom finding types.
pub struct EstimationHealthAnalyzer;

impl EstimationHealthAnalyzer {
    pub fn new() -> Self {
        Self
    }

    /// Provided for parity with other analyzers' construction call sites.
    pub fn with_config(_config: &super::super::consolidated_config::AnalysisConfiguration) -> Self {
        Self::new()
    }
}

impl Default for EstimationHealthAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer for EstimationHealthAnalyzer {
    fn analyze(&self, plan: &ParsedPlan, context: &AnalysisContext) -> AnalysisReport {
        let mut report = AnalysisReport::new("EstimationHealthAnalyzer".to_string())
            .with_metadata("version", self.version());

        let mut visitor = EstimationHealthVisitor::new();
        PlanTraversal::depth_first(plan, &mut visitor, context);

        // Capture root misestimate (root path is empty []).
        let root_ratio = visitor
            .root_misestimate
            .unwrap_or(0.0);

        // Build findings from aggregated state.
        for finding in visitor.build_findings() {
            report = report.add_finding(finding);
        }

        report = report
            .with_metric("nodes_with_actuals", visitor.nodes_with_actuals as f64)
            .with_metric("nodes_overestimated", visitor.nodes_overestimated as f64)
            .with_metric("nodes_underestimated", visitor.nodes_underestimated as f64)
            .with_metric("max_misestimate_ratio", visitor.max_misestimate)
            .with_metric("root_misestimate_ratio", root_ratio);

        report
    }

    fn name(&self) -> &'static str {
        "EstimationHealthAnalyzer"
    }

    fn description(&self) -> &'static str {
        "Classifies whole-plan estimate accuracy: systematic stale-stats skew vs single-node outliers"
    }

    fn version(&self) -> &'static str {
        "1.0.0"
    }
}

/// Per-significant-node record kept for averaging and direction tallies.
struct NodeStat {
    /// direction-agnostic magnitude, always >= 1.0
    misestimate: f64,
    /// true => actual > estimated (under-estimate)
    under: bool,
}

struct EstimationHealthVisitor {
    // running tallies
    nodes_with_actuals: usize,
    nodes_overestimated: usize,
    nodes_underestimated: usize,
    max_misestimate: f64,
    root_misestimate: Option<f64>,
    /// stats for nodes OUTSIDE the band, with their paths (for outlier id)
    significant: Vec<(NodePath, NodeStat)>,
}

impl EstimationHealthVisitor {
    fn new() -> Self {
        Self {
            nodes_with_actuals: 0,
            nodes_overestimated: 0,
            nodes_underestimated: 0,
            max_misestimate: 0.0,
            root_misestimate: None,
            significant: Vec::new(),
        }
    }

    fn build_findings(&self) -> Vec<Finding> {
        let mut findings = Vec::new();
        if self.nodes_with_actuals == 0 {
            return findings; // metrics only
        }

        let significant_count = self.significant.len();

        // --- Rule 1: Systematic ------------------------------------------
        if self.nodes_with_actuals >= SYSTEMATIC_MIN_NODES && significant_count >= 1 {
            let dominant = self.nodes_overestimated.max(self.nodes_underestimated);
            let same_dir_ok =
                dominant as f64 >= SYSTEMATIC_DIR_FRACTION * significant_count as f64;
            let avg_ratio = self.significant.iter().map(|(_, s)| s.misestimate).sum::<f64>()
                / significant_count as f64;

            if same_dir_ok && avg_ratio >= SYSTEMATIC_AVG_RATIO {
                let direction = if self.nodes_underestimated >= self.nodes_overestimated {
                    "under"
                } else {
                    "over"
                };
                let severity = if avg_ratio >= 10.0 {
                    Severity::High
                } else {
                    Severity::Medium
                };
                let finding = Finding::new(
                    FindingType::Custom("EstimationPatternSystematic".to_string()),
                    severity,
                    "Systematic row mis-estimation across plan".to_string(),
                    format!(
                        "{} of {} nodes with actuals skew the same direction ({}-estimated) \
                         with an average misestimate of {:.1}x. The planner's statistics are \
                         likely stale or a column lacks extended statistics.",
                        dominant, self.nodes_with_actuals, direction, avg_ratio
                    ),
                    "Run ANALYZE on the involved tables to refresh statistics; for correlated \
                     columns consider CREATE STATISTICS.".to_string(),
                )
                .with_node(NodePath::root())
                .with_evidence("nodes_overestimated", self.nodes_overestimated as f64)
                .with_evidence("nodes_underestimated", self.nodes_underestimated as f64)
                .with_evidence("avg_ratio", avg_ratio)
                .with_evidence("nodes_with_actuals", self.nodes_with_actuals as f64)
                .with_metadata("direction", direction);
                findings.push(finding);
            }
        }

        // --- Rule 2: Single-node outlier ---------------------------------
        // exactly one node with misestimate >= OUTLIER_RATIO, rest in band.
        let extreme: Vec<&(NodePath, NodeStat)> = self
            .significant
            .iter()
            .filter(|(_, s)| s.misestimate >= OUTLIER_RATIO)
            .collect();
        let rest_in_band = self
            .significant
            .iter()
            .filter(|(_, s)| s.misestimate < OUTLIER_RATIO)
            .all(|(_, s)| s.misestimate < BAND_HIGH); // (always true: significant means >=BAND_HIGH)

        // NOTE: because `significant` only holds out-of-band nodes, "rest in band"
        // means the only OTHER significant nodes are none. Enforce that directly:
        if extreme.len() == 1 && self.significant.len() == 1 {
            let (path, stat) = extreme[0];
            let _ = rest_in_band; // see note above; kept for clarity
            let finding = Finding::new(
                FindingType::Custom("EstimationPatternOutlier".to_string()),
                Severity::Medium,
                "Single-node row estimate outlier".to_string(),
                format!(
                    "One node is mis-estimated by {:.0}x while the rest of the plan is accurate. \
                     This points to localized data skew or correlated predicates on that relation.",
                    stat.misestimate
                ),
                "Consider CREATE STATISTICS on correlated columns, or a partial/expression index \
                 for the skewed predicate on that relation.".to_string(),
            )
            .with_node(path.clone())
            .with_evidence("max_misestimate_ratio", stat.misestimate);
            findings.push(finding);
        }

        findings
    }
}

impl NodeVisitor for EstimationHealthVisitor {
    fn visit_node(&mut self, node: &PlanNode, path: &NodePath, _context: &AnalysisContext) {
        let Some(actuals) = &node.actuals else {
            return;
        };
        let Some(actual) = actuals.actual_rows else {
            return;
        };

        self.nodes_with_actuals += 1;

        let est_f = node.cost.estimated_rows.max(1) as f64;
        let act_f = actual.max(1) as f64;
        let ratio = act_f / est_f;
        let misestimate = ratio.max(1.0 / ratio); // >= 1.0

        if misestimate > self.max_misestimate {
            self.max_misestimate = misestimate;
        }
        if path.path.is_empty() {
            self.root_misestimate = Some(misestimate);
        }

        // Out-of-band classification.
        if ratio > BAND_HIGH {
            self.nodes_underestimated += 1;
            self.significant
                .push((path.clone(), NodeStat { misestimate, under: true }));
        } else if ratio < BAND_LOW {
            self.nodes_overestimated += 1;
            self.significant
                .push((path.clone(), NodeStat { misestimate, under: false }));
        }
        // ratio within [BAND_LOW, BAND_HIGH] => accurate, ignored.
    }
}
```

Implementation note for the implementer: the `rest_in_band` /
`_ = rest_in_band` lines are illustrative; the clean final condition is simply
`extreme.len() == 1 && self.significant.len() == 1`. Drop the unused `under`
field if clippy flags it, or keep it for the `direction` tie-break (it is used
via the over/under counters). Run `cargo fmt --all` and
`cargo clippy --workspace --all-features --all-targets -- -D warnings` after.

---

## 5. Registration edits (exact)

### 5a. `crates/core/src/analysis/analyzers/mod.rs`
Add the module declaration alongside the other "new reliable analyzers":

```rust
// New reliable analyzers (replacing flaky ones)
pub mod estimation_health;   // <-- add
pub mod index_usage;
pub mod startup_cost;
```

Add the re-export (keep alphabetical-ish grouping with the others):

```rust
pub use estimation_health::EstimationHealthAnalyzer;   // <-- add
pub use index_usage::IndexUsageAnalyzer;
```

### 5b. `crates/tui/src/ui/state/query_detail_view.rs`
Extend the import list (lines ~10-11):

```rust
        IndexUsageAnalyzer, JoinAnalyzer, QueryPatternAnalyzer, RowEstimationAnalyzer,
        ScanAnalyzer, StartupCostAnalyzer, EstimationHealthAnalyzer,
```
(or insert `EstimationHealthAnalyzer,` in alphabetical position — order is
cosmetic.)

Add to the builder chain (after line 81, `IndexUsageAnalyzer`):

```rust
            .add_analyzer(IndexUsageAnalyzer::new())
            .add_analyzer(EstimationHealthAnalyzer::new())   // <-- add
            .build();
```

### 5c. `crates/tui/src/ui/state/log_parsing_state.rs`
Extend the inner `use` (lines ~549-550):

```rust
                            EstimationHealthAnalyzer, IndexUsageAnalyzer, JoinAnalyzer,
                            QueryPatternAnalyzer, RowEstimationAnalyzer, ScanAnalyzer,
                            StartupCostAnalyzer,
```

Add to the builder chain (after line 562, `IndexUsageAnalyzer`):

```rust
                        .add_analyzer(IndexUsageAnalyzer::new())
                        .add_analyzer(EstimationHealthAnalyzer::new())   // <-- add
                        .build();
```

No `consolidated_config.rs` edits (analyzer is config-less). No engine.rs edits
(`add_analyzer<A: Analyzer + 'static>` accepts it generically).

---

## 6. Tests (in `estimation_health.rs`, `#[cfg(test)] mod tests`)

Helper to build a scan node with an estimate and optional actual:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NodeType, ParsedPlan, PlanActuals, PlanCost, PlanNode, ScanType, TableReference};

    fn scan_node(name: &str, estimated: u64, actual: Option<u64>) -> PlanNode {
        let mut node = PlanNode::new(
            NodeType::Scan(ScanType::SeqScan {
                table: TableReference { schema: None, name: name.to_string(), alias: None },
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 100.0,
                estimated_rows: estimated,
                estimated_width: 50,
            },
            format!("Seq Scan on {name}"),
        );
        if let Some(a) = actual {
            node.set_actuals(PlanActuals {
                actual_time_ms: Some(1.0),
                actual_rows: Some(a),
                actual_loops: Some(1),
            });
        }
        node
    }

    /// Build a root with `children` chained as a linear stack (root -> c0 -> c1 ...).
    /// Simpler: make root the first node and attach the rest as direct children.
    fn plan_with(root: PlanNode, children: Vec<PlanNode>) -> ParsedPlan {
        let mut r = root;
        for c in children {
            r.add_child(c);
        }
        ParsedPlan::new(r)
    }
```

### Test 1 — `test_systematic_underestimation_flagged` (positive)
5 nodes, every one with `actual >> estimated` (ratio ~100, all under). Expect a
`Custom("EstimationPatternSystematic")` finding; expect NO outlier finding
(more than one extreme node).

```rust
    #[test]
    fn test_systematic_underestimation_flagged() {
        let analyzer = EstimationHealthAnalyzer::new();
        let ctx = AnalysisContext::new();
        // root + 4 children, all under-estimated 100x
        let root = scan_node("t0", 10, Some(1000));
        let kids = vec![
            scan_node("t1", 10, Some(1000)),
            scan_node("t2", 20, Some(2000)),
            scan_node("t3", 5, Some(500)),
            scan_node("t4", 50, Some(5000)),
        ];
        let plan = plan_with(root, kids);
        let report = analyzer.analyze(&plan, &ctx);

        assert!(report.findings.iter().any(|f|
            matches!(f.finding_type, FindingType::Custom(ref s) if s == "EstimationPatternSystematic")));
        assert!(!report.findings.iter().any(|f|
            matches!(f.finding_type, FindingType::Custom(ref s) if s == "EstimationPatternOutlier")));
        assert_eq!(report.metrics.get("nodes_with_actuals"), Some(&5.0));
        assert_eq!(report.metrics.get("nodes_underestimated"), Some(&5.0));
    }
```

### Test 2 — `test_single_outlier_flagged` (positive)
4 accurate nodes (ratio ~1.0, in band) + 1 node with ratio 100. Expect a
`Custom("EstimationPatternOutlier")` finding, and NO systematic finding (only
one significant node, so 80%-same-direction is trivially met but
`significant_count`=1 with the rest in band — verify it does NOT fire Rule 1
because `avg_ratio`>=3 AND `nodes_with_actuals`>=4 hold; per the design,
mutual-exclusivity is enforced by Rule 2 needing the rest in-band while Rule 1's
significant set has size 1 → KEEP both rules' fixtures distinct).

> Implementer caution: with 4 in-band + 1 extreme, `significant_count == 1`, so
> Rule 1's "dominant >= 0.8 * 1" and "avg_ratio >= 3" are BOTH satisfied and
> `nodes_with_actuals == 5 >= 4`. To keep Rule 1 from firing here, add the
> guard `significant_count >= 2` to Rule 1 (a single significant node is by
> definition an outlier, not a systematic pattern). Update §2 Rule 1 to require
> `significant_count >= 2`. The test asserts only-outlier:

```rust
    #[test]
    fn test_single_outlier_flagged() {
        let analyzer = EstimationHealthAnalyzer::new();
        let ctx = AnalysisContext::new();
        // root accurate + 3 accurate kids + 1 extreme (100x under)
        let root = scan_node("t0", 1000, Some(1000));
        let kids = vec![
            scan_node("t1", 1000, Some(1100)), // ratio 1.1, in band
            scan_node("t2", 1000, Some(900)),  // ratio 0.9, in band
            scan_node("t3", 1000, Some(1000)), // ratio 1.0, in band
            scan_node("t4", 10, Some(1000)),   // ratio 100, EXTREME
        ];
        let plan = plan_with(root, kids);
        let report = analyzer.analyze(&plan, &ctx);

        assert!(report.findings.iter().any(|f|
            matches!(f.finding_type, FindingType::Custom(ref s) if s == "EstimationPatternOutlier")));
        assert!(!report.findings.iter().any(|f|
            matches!(f.finding_type, FindingType::Custom(ref s) if s == "EstimationPatternSystematic")));
        assert!(report.metrics.get("max_misestimate_ratio").unwrap() >= &50.0);
    }
```

### Test 3 — `test_accurate_plan_no_finding` (negative)
All nodes have actuals within `[0.5, 2.0]`. Expect ZERO findings; metrics
present.

```rust
    #[test]
    fn test_accurate_plan_no_finding() {
        let analyzer = EstimationHealthAnalyzer::new();
        let ctx = AnalysisContext::new();
        let root = scan_node("t0", 1000, Some(1000));
        let kids = vec![
            scan_node("t1", 1000, Some(1200)), // 1.2
            scan_node("t2", 1000, Some(800)),  // 0.8
            scan_node("t3", 2000, Some(2000)), // 1.0
            scan_node("t4", 500, Some(700)),   // 1.4
        ];
        let plan = plan_with(root, kids);
        let report = analyzer.analyze(&plan, &ctx);

        assert_eq!(report.findings.len(), 0);
        assert_eq!(report.metrics.get("nodes_with_actuals"), Some(&5.0));
        assert_eq!(report.metrics.get("nodes_overestimated"), Some(&0.0));
        assert_eq!(report.metrics.get("nodes_underestimated"), Some(&0.0));
    }
```

### Test 4 — `test_no_actuals_metrics_only` (negative)
Nodes have NO actuals (estimate-only plan). Expect ZERO findings, and
`nodes_with_actuals == 0` metric present.

```rust
    #[test]
    fn test_no_actuals_metrics_only() {
        let analyzer = EstimationHealthAnalyzer::new();
        let ctx = AnalysisContext::new();
        let root = scan_node("t0", 1000, None);
        let kids = vec![
            scan_node("t1", 10, None),
            scan_node("t2", 1_000_000, None),
        ];
        let plan = plan_with(root, kids);
        let report = analyzer.analyze(&plan, &ctx);

        assert_eq!(report.findings.len(), 0);
        assert_eq!(report.metrics.get("nodes_with_actuals"), Some(&0.0));
        assert_eq!(report.metrics.get("max_misestimate_ratio"), Some(&0.0));
        assert_eq!(report.metrics.get("root_misestimate_ratio"), Some(&0.0));
    }
}
```

---

## 7. Spec amendment captured here

To make Rule 1 and Rule 2 mutually exclusive on real plans, **Rule 1 requires
`significant_count >= 2`** (a lone significant node is an outlier, not a
pattern). This is the one deviation from the literal spec text and is reflected
in §2 and the Test 2 caution above. Apply it in `build_findings`.

## 8. Definition of done
- New file `estimation_health.rs` with analyzer, visitor, 4 tests.
- 3 registration edits (mod.rs + 2 TUI files); no engine/config changes.
- `cargo fmt --all` clean; `cargo clippy --workspace --all-features
  --all-targets -- -D warnings` clean; `cargo test` passes (new + existing).
