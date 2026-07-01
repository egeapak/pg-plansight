use super::super::consolidated_config::AnalysisConfiguration;
use super::super::traversal::{NodeVisitor, PlanTraversal};
use super::super::{
    AnalysisContext, AnalysisReport, Analyzer, Finding, FindingType, NodePath, Severity,
};
use crate::{NodeType, ParsedPlan, PlanNode, UtilityType};

/// Flag plans deeper than this.
const MAX_DEPTH_THRESHOLD: usize = 12;
/// Flag plans with at least this many nodes.
const NODE_COUNT_THRESHOLD: usize = 40;
/// A single node is "dominant" when its self-cost is at least this fraction of
/// the whole plan's cost.
const DOMINANT_FRACTION_THRESHOLD: f64 = 0.7;
/// Don't flag dominance on trivially cheap plans.
const MIN_PLAN_COST_FOR_DOMINANCE: f64 = 1000.0;

/// Computes whole-plan structural metrics (depth, node count, node-type mix) and
/// flags two shape problems: an excessively deep/large plan, and a single node
/// whose own (self) cost dominates the plan — the hotspot to focus tuning on.
pub struct PlanShapeAnalyzer;

impl PlanShapeAnalyzer {
    pub fn new() -> Self {
        Self
    }

    pub fn with_config(_config: &AnalysisConfiguration) -> Self {
        Self::new()
    }
}

impl Default for PlanShapeAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer for PlanShapeAnalyzer {
    fn analyze(&self, plan: &ParsedPlan, context: &AnalysisContext) -> AnalysisReport {
        let mut report = AnalysisReport::new("PlanShapeAnalyzer".to_string())
            .with_metadata("version", self.version());

        let mut visitor = PlanShapeVisitor::new();
        PlanTraversal::depth_first(plan, &mut visitor, context);

        let total_plan_cost = plan.root.cost.total_cost();
        let dominant_fraction = if total_plan_cost > 0.0 {
            visitor.max_self_cost / total_plan_cost
        } else {
            0.0
        };

        // --- Rule 1: very deep / large plan -----------------------------------
        if visitor.max_depth >= MAX_DEPTH_THRESHOLD || visitor.node_count >= NODE_COUNT_THRESHOLD {
            report = report.add_finding(
                Finding::new(
                    FindingType::Custom("ComplexPlanShape".to_string()),
                    Severity::Medium,
                    "Structurally complex plan".to_string(),
                    format!(
                        "Plan has {} nodes and a maximum depth of {}. Deeply nested or very large \
                         plans are harder to optimize and often come from stacked CTEs/subqueries.",
                        visitor.node_count, visitor.max_depth
                    ),
                    "Consider simplifying or flattening CTEs/subqueries, or splitting the query."
                        .to_string(),
                )
                .with_node(NodePath::root())
                .with_evidence("max_depth", visitor.max_depth as f64)
                .with_evidence("node_count", visitor.node_count as f64),
            );
        }

        // --- Rule 2: cost-dominant node ---------------------------------------
        // Require at least two nodes: on a single-node plan the only node is
        // trivially "dominant" (fraction 1.0), which is not a useful finding.
        if visitor.node_count >= 2
            && total_plan_cost > MIN_PLAN_COST_FOR_DOMINANCE
            && dominant_fraction >= DOMINANT_FRACTION_THRESHOLD
        {
            let mut finding = Finding::new(
                FindingType::ExpensiveOperation,
                Severity::Medium,
                "One operation dominates plan cost".to_string(),
                format!(
                    "Operation '{}' accounts for {:.0}% of the plan's total cost ({:.0} of {:.0}). \
                     Focus tuning on this node.",
                    visitor.dominant_description,
                    dominant_fraction * 100.0,
                    visitor.max_self_cost,
                    total_plan_cost
                ),
                "Tune the dominant operation directly — e.g. add an index, reduce the rows it \
                 processes, or restructure the surrounding query."
                    .to_string(),
            )
            .with_evidence("dominant_cost_fraction", dominant_fraction)
            .with_evidence("most_expensive_self_cost", visitor.max_self_cost)
            .with_evidence("total_plan_cost", total_plan_cost)
            .with_metadata("dominant_node", &visitor.dominant_description);
            if let Some(path) = visitor.dominant_path.clone() {
                finding = finding.with_node(path);
            }
            report = report.add_finding(finding);
        }

        report = report
            .with_metric("node_count", visitor.node_count as f64)
            .with_metric("max_depth", visitor.max_depth as f64)
            .with_metric("scan_count", visitor.scan_count as f64)
            .with_metric("join_count", visitor.join_count as f64)
            .with_metric("aggregate_count", visitor.aggregate_count as f64)
            .with_metric("sort_count", visitor.sort_count as f64)
            .with_metric("most_expensive_self_cost", visitor.max_self_cost)
            .with_metric("total_plan_cost", total_plan_cost)
            .with_metric("dominant_cost_fraction", dominant_fraction);

        report
    }

    fn name(&self) -> &'static str {
        "PlanShapeAnalyzer"
    }

    fn description(&self) -> &'static str {
        "Computes plan shape metrics and flags deep/large plans and cost-dominant nodes"
    }

    fn version(&self) -> &'static str {
        "1.0.0"
    }
}

struct PlanShapeVisitor {
    node_count: usize,
    max_depth: usize,
    scan_count: usize,
    join_count: usize,
    aggregate_count: usize,
    sort_count: usize,
    max_self_cost: f64,
    dominant_description: String,
    dominant_path: Option<NodePath>,
}

impl PlanShapeVisitor {
    fn new() -> Self {
        Self {
            node_count: 0,
            max_depth: 0,
            scan_count: 0,
            join_count: 0,
            aggregate_count: 0,
            sort_count: 0,
            max_self_cost: 0.0,
            dominant_description: String::new(),
            dominant_path: None,
        }
    }
}

impl NodeVisitor for PlanShapeVisitor {
    fn visit_node(&mut self, node: &PlanNode, path: &NodePath, _context: &AnalysisContext) {
        self.node_count += 1;
        self.max_depth = self.max_depth.max(path.path.len());

        match &node.node_type {
            NodeType::Scan(_) => self.scan_count += 1,
            NodeType::Join(_) => self.join_count += 1,
            NodeType::Aggregate(_) => self.aggregate_count += 1,
            NodeType::Utility(UtilityType::Sort { .. }) => self.sort_count += 1,
            _ => {}
        }

        // Self cost = this node's total cost minus the cost of its children, so a
        // node that merely passes its children's work upward isn't counted as
        // doing that work itself.
        let children_cost: f64 = node.children.iter().map(|c| c.cost.total_cost()).sum();
        let self_cost = (node.cost.total_cost() - children_cost).max(0.0);
        if self_cost > self.max_self_cost {
            self.max_self_cost = self_cost;
            self.dominant_description = node.description();
            self.dominant_path = Some(path.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        JoinType, NodeType, PlanCost, PlanNode, ScanType, SortKey, TableReference, UtilityType,
    };

    fn cost(total: f64) -> PlanCost {
        PlanCost {
            startup_cost: 0.0,
            min_total_cost: total,
            max_total_cost: total,
            estimated_rows: 1000,
            estimated_width: 50,
        }
    }

    fn scan(total: f64) -> PlanNode {
        PlanNode::new(
            NodeType::Scan(ScanType::SeqScan {
                table: TableReference {
                    schema: None,
                    name: "t".to_string(),
                    alias: None,
                },
            }),
            cost(total),
            "Seq Scan on t".to_string(),
        )
    }

    fn analyze(node: PlanNode) -> AnalysisReport {
        let plan = ParsedPlan::new(node);
        PlanShapeAnalyzer::new().analyze(&plan, &AnalysisContext::new())
    }

    #[test]
    fn test_metrics_counts() {
        // root Join -> [ scan, Sort -> [ scan ] ]
        let mut sort = PlanNode::new(
            NodeType::Utility(UtilityType::Sort {
                sort_keys: vec![SortKey {
                    expression: "c".to_string(),
                    direction: None,
                }],
                sort_method: None,
            }),
            cost(200.0),
            "Sort".to_string(),
        );
        sort.add_child(scan(100.0));

        let mut root = PlanNode::new(
            NodeType::Join(JoinType::HashJoin {
                hash_condition: None,
                hash_buckets: None,
            }),
            cost(400.0),
            "Hash Join".to_string(),
        );
        root.add_child(scan(100.0));
        root.add_child(sort);

        let report = analyze(root);
        assert_eq!(report.metrics.get("node_count"), Some(&4.0));
        assert_eq!(report.metrics.get("max_depth"), Some(&2.0));
        assert_eq!(report.metrics.get("scan_count"), Some(&2.0));
        assert_eq!(report.metrics.get("join_count"), Some(&1.0));
        assert_eq!(report.metrics.get("sort_count"), Some(&1.0));
    }

    #[test]
    fn test_deep_plan_flagged() {
        // 15-deep chain of scans.
        let mut node = scan(10.0);
        for _ in 0..15 {
            let mut parent = scan(10.0);
            parent.add_child(node);
            node = parent;
        }
        let report = analyze(node);
        assert!(
            report.findings.iter().any(
                |f| matches!(&f.finding_type, FindingType::Custom(s) if s == "ComplexPlanShape"),
            )
        );
    }

    #[test]
    fn test_simple_plan_no_shape_finding() {
        // root 300 over two scans of 100 each: self-costs 100/100/100, max
        // fraction 0.33, total < 1000 -> no findings.
        let mut root = scan(300.0);
        root.add_child(scan(100.0));
        root.add_child(scan(100.0));

        let report = analyze(root);
        assert!(report.findings.is_empty());
    }

    #[test]
    fn test_dominant_node_flagged() {
        // root total 10000 with a single child scan total 9500: child self-cost
        // 9500 dominates (0.95).
        let mut root = scan(10_000.0);
        root.add_child(scan(9_500.0));

        let report = analyze(root);
        let f: Vec<_> = report
            .findings
            .iter()
            .filter(|f| matches!(f.finding_type, FindingType::ExpensiveOperation))
            .collect();
        assert_eq!(f.len(), 1);
        let frac = f[0]
            .evidence
            .get("dominant_cost_fraction")
            .copied()
            .unwrap();
        assert!((0.94..=0.96).contains(&frac), "fraction was {frac}");
    }

    #[test]
    fn test_dominant_fraction_below_threshold_no_finding() {
        // root 3000 over two children of 1000 each: self-costs 1000/1000/1000,
        // max fraction 0.33 -> no dominant finding (and not deep/large).
        let mut root = scan(3_000.0);
        root.add_child(scan(1_000.0));
        root.add_child(scan(1_000.0));

        let report = analyze(root);
        assert!(
            !report
                .findings
                .iter()
                .any(|f| matches!(f.finding_type, FindingType::ExpensiveOperation))
        );
    }
}
