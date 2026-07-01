# F5 — EstimationHealthAnalyzer

## Goal
A **whole-plan** view of planner estimate accuracy that complements the existing
per-node `RowEstimationAnalyzer` (which flags individual bad nodes). This one
classifies the *pattern*: is the planner systematically off (stale stats → run
`ANALYZE`) or is it one anomalous node (data skew / correlated predicates)?
Predicts regressions.

## Coordination with existing analyzer
`row_estimation.rs` already flags per-node `ExcessiveRowProcessing`/
`RowEstimationError`. F5 must NOT re-flag every node. It emits at most a couple
of **plan-level** findings and a set of summary metrics. Use a distinct
`FindingType::Custom("EstimationPatternSystematic")` /
`Custom("EstimationPatternOutlier")` so they don't collide.

## File
`crates/core/src/analysis/analyzers/estimation_health.rs`
Struct `EstimationHealthAnalyzer` + `NodeVisitor`. Register in `mod.rs`.

## Inputs
Per node where `actuals.actual_rows` is present:
- `estimated = node.cost.estimated_rows`
- `actual = actuals.actual_rows` (multiply estimate considerations by loops? Use
  per-node totals; compare `actual` vs `estimated` directly — both are per-node
  PostgreSQL semantics where actual is per-loop; to stay simple and robust,
  compare `estimated` to `actual` and document the per-loop caveat.)
- `ratio = max(actual,1) / max(estimated,1)` and its inverse; track
  `over` (estimated > actual) vs `under` (actual > estimated) using a
  significance band (ratio outside [0.5, 2.0]).

## Computed metrics
- `nodes_with_actuals`, `nodes_overestimated`, `nodes_underestimated`,
  `max_misestimate_ratio` (worst of ratio or 1/ratio), `root_misestimate_ratio`.

## Detection rules → Findings
1. **Systematic misestimation** — `nodes_with_actuals >= 4` AND ≥ 80% of
   estimating nodes skew the **same** direction (all over or all under) with
   misestimate ratio ≥ 3 on average.
   - `Custom("EstimationPatternSystematic")`, Medium/High.
   - suggestion: stats look stale or a column needs extended statistics; run
     `ANALYZE` (or `CREATE STATISTICS`).
   - evidence: `nodes_overestimated`, `nodes_underestimated`, `avg_ratio`.
2. **Single-node outlier** — exactly one node with misestimate ratio ≥ 50 while
   the rest are within band.
   - `Custom("EstimationPatternOutlier")`, Medium.
   - affected node = outlier path.
   - suggestion: localized estimate error → likely data skew or correlated
     predicates on that relation; consider `CREATE STATISTICS` or a partial
     index. evidence: `max_misestimate_ratio`.

If no `actuals` anywhere → emit metrics only (no findings; this plan wasn't run
with ANALYZE).

## Tests
- `test_systematic_underestimation_flagged` — 5 nodes all actual≫estimated →
  Systematic finding.
- `test_single_outlier_flagged` — 4 accurate nodes + 1 with ratio 100 → Outlier.
- `test_accurate_plan_no_finding` — all ratios within [0.5,2.0] → no finding
  (negative).
- `test_no_actuals_metrics_only` — nodes without actuals → 0 findings, metrics
  present (negative).

## Edge cases
- Guard all divisions with `max(_,1)`.
- Mixed directions with no clear majority → no systematic finding.
