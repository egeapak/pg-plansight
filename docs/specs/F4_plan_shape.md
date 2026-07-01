# F4 — PlanShapeAnalyzer

## Goal
Compute whole-plan structural metrics and flag two shape problems: an
excessively deep/large plan (often from stacked CTEs/subqueries) and a single
node that dominates total cost (the hotspot to focus tuning on). Provides the
baseline metrics later used for plan classification and diffing.

## File
`crates/core/src/analysis/analyzers/plan_shape.rs`
Struct `PlanShapeAnalyzer` + `NodeVisitor` (tracks depth via `enter`/`exit` or
via `NodePath.path.len()`). Register in `mod.rs`.

## Computed metrics (always emitted)
- `node_count` — total nodes.
- `max_depth` — deepest `NodePath.path.len()`.
- `scan_count`, `join_count`, `aggregate_count`, `sort_count` — by `NodeType`
  (and `UtilityType::Sort` for sorts).
- `most_expensive_cost` — max `node.cost.total_cost()` over nodes.
- `total_plan_cost` — `plan.root.cost.total_cost()`.
- `dominant_cost_fraction` — `most_expensive_cost / total_plan_cost`.

## Detection rules → Findings
1. **Very deep / large plan** — `max_depth >= max_depth_threshold`
   (default 12) OR `node_count >= node_count_threshold` (default 40).
   - `FindingType::Custom("ComplexPlanShape")`, Medium.
   - suggestion: the query is structurally complex (deep nesting / many nodes);
     consider simplifying CTEs/subqueries or splitting the query.
   - evidence: `max_depth`, `node_count`.
2. **Cost-dominant node** — `dominant_cost_fraction >= 0.7` AND
   `total_plan_cost > 1000` (avoid trivial plans).
   - `FindingType::ExpensiveOperation`, Medium.
   - affected node = the path of the dominant node.
   - suggestion: one operation accounts for most of the plan cost — focus tuning
     there. Include the node description in metadata.
   - evidence: `dominant_cost_fraction`, `most_expensive_cost`, `total_plan_cost`.

## Tests
- `test_metrics_counts` — a small handcrafted tree (root join + 2 scans + 1 sort)
  yields the expected counts/depth.
- `test_deep_plan_flagged` — build a 15-deep chain → ComplexPlanShape finding.
- `test_simple_plan_no_shape_finding` — 3-node plan, balanced costs → no
  ComplexPlanShape, no dominant finding (negative).
- `test_dominant_node_flagged` — root total_cost 10_000 with one child at 9_500
  → ExpensiveOperation with dominant_cost_fraction ≈ 0.95.
- `test_dominant_fraction_below_threshold_no_finding` — costs spread evenly → no
  dominant finding (negative).

## Edge cases
- `total_plan_cost == 0` → skip dominant-fraction calc (no div-by-zero).
- Use the iterative traversal helpers; do not recurse manually (deep plans).
