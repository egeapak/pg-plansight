use super::super::consolidated_config::AnalysisConfiguration;
use super::super::traversal::{NodeVisitor, PlanTraversal};
use super::super::{
    AnalysisContext, AnalysisReport, Analyzer, Finding, FindingType, NodePath, Severity,
};
use crate::{NodeType, ParsedPlan, PlanNode, UtilityType};

/// Analyzer for detecting high startup costs that delay query execution
///
/// This analyzer focuses on reliable detection of expensive startup operations
/// without trying to correlate cost units to time (which requires calibration).
pub struct StartupCostAnalyzer;

impl StartupCostAnalyzer {
    pub fn new() -> Self {
        Self
    }

    pub fn with_config(_config: &AnalysisConfiguration) -> Self {
        Self::new()
    }
}

impl Default for StartupCostAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer for StartupCostAnalyzer {
    fn analyze(&self, plan: &ParsedPlan, context: &AnalysisContext) -> AnalysisReport {
        let mut report = AnalysisReport::new("StartupCostAnalyzer".to_string())
            .with_metadata("version", self.version());

        let mut visitor = StartupCostVisitor::new();
        PlanTraversal::depth_first(plan, &mut visitor, context);

        // Add all findings to the report
        for finding in visitor.findings {
            report = report.add_finding(finding);
        }

        // Add aggregate metrics
        report = report
            .with_metric("nodes_analyzed", visitor.nodes_analyzed as f64)
            .with_metric("total_plan_cost", plan.root.cost.max_total_cost)
            .with_metric("max_startup_cost", visitor.max_startup_cost)
            .with_metric("nodes_with_high_startup", visitor.high_startup_nodes as f64);

        report
    }

    fn name(&self) -> &'static str {
        "StartupCostAnalyzer"
    }

    fn description(&self) -> &'static str {
        "Detects operations with high startup costs that delay initial results"
    }

    fn version(&self) -> &'static str {
        "1.0.0"
    }
}

/// Visitor implementation for collecting startup cost findings
struct StartupCostVisitor {
    findings: Vec<Finding>,
    nodes_analyzed: usize,
    max_startup_cost: f64,
    high_startup_nodes: usize,
}

impl StartupCostVisitor {
    fn new() -> Self {
        Self {
            findings: Vec::new(),
            nodes_analyzed: 0,
            max_startup_cost: 0.0,
            high_startup_nodes: 0,
        }
    }

    fn detect_high_startup_cost(&mut self, node: &PlanNode, path: &NodePath) {
        let startup_cost = node.cost.startup_cost;
        let total_cost = node.cost.max_total_cost;

        // Update max startup cost seen
        if startup_cost > self.max_startup_cost {
            self.max_startup_cost = startup_cost;
        }

        // High startup cost (absolute threshold - costs > 10000 are typically significant)
        if startup_cost > 10000.0 {
            self.high_startup_nodes += 1;

            let severity = if startup_cost > 100000.0 {
                Severity::Critical
            } else if startup_cost > 50000.0 {
                Severity::High
            } else {
                Severity::Medium
            };

            let finding = Finding::new(
                FindingType::HighStartupCost,
                severity,
                format!("High startup cost ({:.0})", startup_cost),
                format!(
                    "Operation '{}' has a startup cost of {:.0}, which will delay the return of initial results",
                    node.description(),
                    startup_cost
                ),
                "Consider materializing intermediate results, using LIMIT to reduce work, or restructuring the query".to_string(),
            )
            .with_node(path.clone())
            .with_evidence("startup_cost", startup_cost)
            .with_evidence("total_cost", total_cost)
            .with_evidence("startup_ratio", if total_cost > 0.0 { startup_cost / total_cost } else { 0.0 });

            self.findings.push(finding);
        }

        // Startup cost dominates total cost (> 90%)
        if total_cost > 0.0 && startup_cost / total_cost > 0.9 && startup_cost > 1000.0 {
            let finding = Finding::new(
                FindingType::Custom("StartupCostDominant".to_string()),
                Severity::Medium,
                "Startup cost dominates execution".to_string(),
                format!(
                    "Operation '{}' has startup cost ({:.0}) that is {:.0}% of total cost. Most work happens before returning results.",
                    node.description(),
                    startup_cost,
                    (startup_cost / total_cost) * 100.0
                ),
                "This pattern is typical for sorts and materializations. Consider if these operations are necessary.".to_string(),
            )
            .with_node(path.clone())
            .with_evidence("startup_cost", startup_cost)
            .with_evidence("total_cost", total_cost)
            .with_evidence("startup_percentage", (startup_cost / total_cost) * 100.0);

            self.findings.push(finding);
        }

        // Specific operation type insights
        if let NodeType::Utility(utility_type) = &node.node_type {
            match utility_type {
                UtilityType::Sort {
                    sort_method: Some(method),
                    ..
                } => {
                    if method.to_lowercase().contains("external") && startup_cost > 5000.0 {
                        let finding = Finding::new(
                            FindingType::Custom("ExternalSort".to_string()),
                            Severity::High,
                            "External disk-based sort detected".to_string(),
                            format!(
                                "Sort operation uses external merge sort (disk-based) with startup cost {:.0}. This spills to disk.",
                                startup_cost
                            ),
                            "Increase work_mem to allow in-memory sorting, or reduce the dataset size before sorting".to_string(),
                        )
                        .with_node(path.clone())
                        .with_evidence("startup_cost", startup_cost)
                        .with_metadata("sort_method", method);

                        self.findings.push(finding);
                    }
                }
                UtilityType::Materialize => {
                    if startup_cost > 5000.0 {
                        let finding = Finding::new(
                            FindingType::Custom("ExpensiveMaterialization".to_string()),
                            Severity::Medium,
                            "Expensive materialization node".to_string(),
                            format!(
                                "Materialize node with startup cost {:.0}. This caches intermediate results but delays execution.",
                                startup_cost
                            ),
                            "Materialization is often necessary for correctness, but high cost may indicate inefficient plan".to_string(),
                        )
                        .with_node(path.clone())
                        .with_evidence("startup_cost", startup_cost);

                        self.findings.push(finding);
                    }
                }
                _ => {}
            }
        }
    }
}

impl NodeVisitor for StartupCostVisitor {
    fn visit_node(&mut self, node: &PlanNode, path: &NodePath, _context: &AnalysisContext) {
        self.nodes_analyzed += 1;
        self.detect_high_startup_cost(node, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NodeType, PlanCost, PlanNode, ScanType, SortKey, TableReference, UtilityType};

    #[test]
    fn test_high_startup_cost_detection() {
        let analyzer = StartupCostAnalyzer::new();
        let context = AnalysisContext::new();

        let node = PlanNode::new(
            NodeType::Utility(UtilityType::Sort {
                sort_keys: vec![SortKey {
                    expression: "column1".to_string(),
                    direction: None,
                }],
                sort_method: Some("external merge".to_string()),
            }),
            PlanCost {
                startup_cost: 50000.0,
                min_total_cost: 50000.0,
                max_total_cost: 55000.0,
                estimated_rows: 100000,
                estimated_width: 200,
            },
            "Sort".to_string(),
        );

        let plan = ParsedPlan::new(node);
        let report = analyzer.analyze(&plan, &context);

        // Should detect high startup cost
        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f.finding_type, FindingType::HighStartupCost))
        );
    }

    #[test]
    fn test_external_sort_detection() {
        let analyzer = StartupCostAnalyzer::new();
        let context = AnalysisContext::new();

        let node = PlanNode::new(
            NodeType::Utility(UtilityType::Sort {
                sort_keys: vec![SortKey {
                    expression: "column1".to_string(),
                    direction: None,
                }],
                sort_method: Some("external merge".to_string()),
            }),
            PlanCost {
                startup_cost: 10000.0,
                min_total_cost: 10000.0,
                max_total_cost: 12000.0,
                estimated_rows: 500000,
                estimated_width: 100,
            },
            "Sort".to_string(),
        );

        let plan = ParsedPlan::new(node);
        let report = analyzer.analyze(&plan, &context);

        // Should detect external sort
        assert!(
            report.findings.iter().any(
                |f| matches!(f.finding_type, FindingType::Custom(ref s) if s == "ExternalSort")
            )
        );
    }

    #[test]
    fn test_low_startup_cost_no_findings() {
        let analyzer = StartupCostAnalyzer::new();
        let context = AnalysisContext::new();

        let node = PlanNode::new(
            NodeType::Scan(ScanType::SeqScan {
                table: TableReference {
                    schema: Some("public".to_string()),
                    name: "test_table".to_string(),
                    alias: None,
                },
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 1000.0,
                estimated_rows: 10000,
                estimated_width: 100,
            },
            "Seq Scan on test_table".to_string(),
        );

        let plan = ParsedPlan::new(node);
        let report = analyzer.analyze(&plan, &context);

        // Should not flag low startup cost operations
        assert_eq!(report.findings.len(), 0);
    }
}
