use super::super::consolidated_config::AnalysisConfiguration;
use super::super::traversal::{NodeVisitor, PlanTraversal};
use super::super::{
    AnalysisContext, AnalysisReport, Analyzer, Finding, FindingType, NodePath, Severity,
};
use crate::{ParsedPlan, PlanNode};

/// A node's estimate is "significantly off" once its misestimate ratio leaves
/// the band [0.5, 2.0] (i.e. ratio >= 2.0).
const SIGNIFICANT_MISESTIMATE: f64 = 2.0;
/// Minimum number of nodes-with-actuals before judging a *pattern*.
const MIN_NODES_FOR_PATTERN: usize = 4;
/// Minimum significantly-off nodes before calling it systematic (so a single
/// outlier doesn't trip the systematic rule).
const MIN_SIGNIFICANT_FOR_SYSTEMATIC: usize = 2;
/// Fraction of significant nodes that must skew the same way to be "systematic".
const SYSTEMATIC_AGREEMENT: f64 = 0.8;
/// Average misestimate among significant nodes required to flag systematic.
const SYSTEMATIC_AVG_RATIO: f64 = 3.0;
/// A lone node this far off (with the rest accurate) is an "outlier".
const OUTLIER_RATIO: f64 = 50.0;

/// Whole-plan view of planner estimate accuracy. Complements the per-node
/// `RowEstimationAnalyzer` by classifying the *pattern*: systematic skew (stale
/// stats → ANALYZE) vs a single anomalous node (data skew / correlated
/// predicates). Emits at most two plan-level findings using distinct custom
/// finding types so it never collides with the per-node analyzer.
pub struct EstimationHealthAnalyzer;

impl EstimationHealthAnalyzer {
    pub fn new() -> Self {
        Self
    }

    pub fn with_config(_config: &AnalysisConfiguration) -> Self {
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

        let mut visitor = EstimationVisitor::new();
        PlanTraversal::depth_first(plan, &mut visitor, context);

        let over = visitor.nodes_overestimated;
        let under = visitor.nodes_underestimated;
        let significant = over + under;

        // --- Rule 1: systematic misestimation ---------------------------------
        if visitor.nodes_with_actuals >= MIN_NODES_FOR_PATTERN
            && significant >= MIN_SIGNIFICANT_FOR_SYSTEMATIC
        {
            let dominant = over.max(under);
            let agreement = dominant as f64 / significant as f64;
            let avg_ratio = if significant > 0 {
                visitor.significant_ratio_sum / significant as f64
            } else {
                0.0
            };

            if agreement >= SYSTEMATIC_AGREEMENT && avg_ratio >= SYSTEMATIC_AVG_RATIO {
                let direction = if under >= over { "under" } else { "over" };
                let severity = if avg_ratio >= 10.0 {
                    Severity::High
                } else {
                    Severity::Medium
                };
                report = report.add_finding(
                    Finding::new(
                        FindingType::Custom("EstimationPatternSystematic".to_string()),
                        severity,
                        "Planner systematically misestimates row counts".to_string(),
                        format!(
                            "{} of {} nodes with measured rows skew the same way (the planner \
                             {}-estimates by ~{:.0}x on average). This usually means stale or \
                             missing statistics.",
                            dominant, visitor.nodes_with_actuals, direction, avg_ratio
                        ),
                        "Run ANALYZE on the involved tables; for correlated columns consider \
                         CREATE STATISTICS (extended statistics)."
                            .to_string(),
                    )
                    .with_node(NodePath::root())
                    .with_evidence("nodes_overestimated", over as f64)
                    .with_evidence("nodes_underestimated", under as f64)
                    .with_evidence("avg_misestimate_ratio", avg_ratio),
                );
            }
        }

        // --- Rule 2: single-node outlier --------------------------------------
        if significant == 1 && visitor.max_misestimate_ratio >= OUTLIER_RATIO {
            let mut finding = Finding::new(
                FindingType::Custom("EstimationPatternOutlier".to_string()),
                Severity::Medium,
                "One node has a severe estimate error".to_string(),
                format!(
                    "A single node is misestimated by ~{:.0}x while the rest of the plan is \
                     accurate. This points at localized data skew or correlated predicates on \
                     that relation rather than global stale stats.",
                    visitor.max_misestimate_ratio
                ),
                "Consider CREATE STATISTICS on the correlated columns, or a partial/expression \
                 index targeting that predicate."
                    .to_string(),
            )
            .with_evidence("max_misestimate_ratio", visitor.max_misestimate_ratio);
            if let Some(path) = visitor.worst_path.clone() {
                finding = finding.with_node(path);
            }
            report = report.add_finding(finding);
        }

        report = report
            .with_metric("nodes_with_actuals", visitor.nodes_with_actuals as f64)
            .with_metric("nodes_overestimated", over as f64)
            .with_metric("nodes_underestimated", under as f64)
            .with_metric("max_misestimate_ratio", visitor.max_misestimate_ratio)
            .with_metric("root_misestimate_ratio", visitor.root_misestimate_ratio);

        report
    }

    fn name(&self) -> &'static str {
        "EstimationHealthAnalyzer"
    }

    fn description(&self) -> &'static str {
        "Classifies whole-plan estimate accuracy: systematic skew vs single-node outlier"
    }

    fn version(&self) -> &'static str {
        "1.0.0"
    }
}

struct EstimationVisitor {
    nodes_with_actuals: usize,
    nodes_overestimated: usize,
    nodes_underestimated: usize,
    significant_ratio_sum: f64,
    max_misestimate_ratio: f64,
    root_misestimate_ratio: f64,
    worst_path: Option<NodePath>,
}

impl EstimationVisitor {
    fn new() -> Self {
        Self {
            nodes_with_actuals: 0,
            nodes_overestimated: 0,
            nodes_underestimated: 0,
            significant_ratio_sum: 0.0,
            max_misestimate_ratio: 0.0,
            root_misestimate_ratio: 0.0,
            worst_path: None,
        }
    }
}

impl NodeVisitor for EstimationVisitor {
    fn visit_node(&mut self, node: &PlanNode, path: &NodePath, _context: &AnalysisContext) {
        let Some(actual) = node.actuals.as_ref().and_then(|a| a.actual_rows) else {
            return;
        };
        self.nodes_with_actuals += 1;

        let actual_f = actual.max(1) as f64;
        let estimated_f = node.cost.estimated_rows.max(1) as f64;
        let ratio = actual_f / estimated_f;
        let misestimate = ratio.max(1.0 / ratio);

        if path.path.is_empty() {
            self.root_misestimate_ratio = misestimate;
        }
        if misestimate > self.max_misestimate_ratio {
            self.max_misestimate_ratio = misestimate;
            self.worst_path = Some(path.clone());
        }

        if misestimate >= SIGNIFICANT_MISESTIMATE {
            self.significant_ratio_sum += misestimate;
            if ratio > 1.0 {
                self.nodes_underestimated += 1; // actual > estimated
            } else {
                self.nodes_overestimated += 1; // estimated > actual
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NodeType, PlanActuals, PlanCost, PlanNode, ScanType, TableReference};

    fn node(estimated: u64, actual: Option<u64>) -> PlanNode {
        let mut n = PlanNode::new(
            NodeType::Scan(ScanType::SeqScan {
                table: TableReference {
                    schema: None,
                    name: "t".to_string(),
                    alias: None,
                },
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 100.0,
                estimated_rows: estimated,
                estimated_width: 50,
            },
            "Seq Scan on t".to_string(),
        );
        if let Some(a) = actual {
            n.set_actuals(PlanActuals {
                actual_time_ms: Some(1.0),
                actual_rows: Some(a),
                actual_loops: Some(1),
            });
        }
        n
    }

    fn chain(mut nodes: Vec<PlanNode>) -> ParsedPlan {
        // Build a linear chain root -> child -> ... from the given nodes.
        let mut iter = nodes.drain(..).rev();
        let mut current = iter.next().expect("at least one node");
        for mut parent in iter {
            parent.add_child(current);
            current = parent;
        }
        ParsedPlan::new(current)
    }

    fn analyze(plan: ParsedPlan) -> AnalysisReport {
        EstimationHealthAnalyzer::new().analyze(&plan, &AnalysisContext::new())
    }

    #[test]
    fn test_systematic_underestimation_flagged() {
        // 5 nodes, all actual (10000) >> estimated (100): ratio 100, all "under".
        let plan = chain((0..5).map(|_| node(100, Some(10_000))).collect());
        let report = analyze(plan);
        assert!(report.findings.iter().any(
            |f| matches!(&f.finding_type, FindingType::Custom(s) if s == "EstimationPatternSystematic"),
        ));
    }

    #[test]
    fn test_single_outlier_flagged() {
        // 4 accurate nodes (ratio 1.0) + 1 wildly off (ratio 100).
        let mut nodes: Vec<PlanNode> = (0..4).map(|_| node(1000, Some(1000))).collect();
        nodes.push(node(100, Some(10_000)));
        let report = analyze(chain(nodes));
        let outliers: Vec<_> = report
            .findings
            .iter()
            .filter(
                |f| matches!(&f.finding_type, FindingType::Custom(s) if s == "EstimationPatternOutlier"),
            )
            .collect();
        assert_eq!(outliers.len(), 1);
        // It must NOT also be flagged as systematic (single significant node).
        assert!(!report.findings.iter().any(
            |f| matches!(&f.finding_type, FindingType::Custom(s) if s == "EstimationPatternSystematic"),
        ));
    }

    #[test]
    fn test_accurate_plan_no_finding() {
        // All within band [0.5, 2.0].
        let plan = chain((0..5).map(|_| node(1000, Some(1200))).collect());
        let report = analyze(plan);
        assert!(report.findings.is_empty());
        assert_eq!(report.metrics.get("nodes_with_actuals"), Some(&5.0));
    }

    #[test]
    fn test_no_actuals_metrics_only() {
        let plan = chain((0..5).map(|_| node(1000, None)).collect());
        let report = analyze(plan);
        assert!(report.findings.is_empty());
        assert_eq!(report.metrics.get("nodes_with_actuals"), Some(&0.0));
    }
}
