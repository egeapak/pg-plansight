use super::super::consolidated_config::{AnalysisConfiguration, RowEstimationConfig};
use super::super::traversal::{NodeVisitor, PlanTraversal};
use super::super::{
    AnalysisContext, AnalysisReport, Analyzer, ConfigurableAnalyzer, Finding, FindingType,
    NodePath, Severity,
};
use crate::{ParsedPlan, PlanNode};

/// Analyzer for row estimation accuracy and excessive row processing
pub struct RowEstimationAnalyzer {
    config: RowEstimationConfig,
}

impl RowEstimationAnalyzer {
    pub fn new() -> Self {
        let analysis_config = AnalysisConfiguration::default();
        Self {
            config: analysis_config.analyzers.row_estimation,
        }
    }

    pub fn with_config(config: &AnalysisConfiguration) -> Self {
        Self {
            config: config.analyzers.row_estimation.clone(),
        }
    }
}

impl Default for RowEstimationAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer for RowEstimationAnalyzer {
    fn analyze(&self, plan: &ParsedPlan, context: &AnalysisContext) -> AnalysisReport {
        let mut report = AnalysisReport::new("RowEstimationAnalyzer".to_string())
            .with_metadata("version", self.version());

        // Create a visitor to collect row-related findings
        let mut visitor = RowEstimationVisitor::new(&self.config);
        PlanTraversal::depth_first(plan, &mut visitor, context);

        // Add all findings to the report
        for finding in visitor.findings {
            report = report.add_finding(finding);
        }

        // Add aggregate metrics
        report = report
            .with_metric("nodes_analyzed", visitor.nodes_analyzed as f64)
            .with_metric(
                "excessive_row_operations",
                visitor.excessive_row_operations as f64,
            )
            .with_metric("estimation_errors", visitor.estimation_errors as f64)
            .with_metric("cartesian_products", visitor.cartesian_products as f64)
            .with_metric("max_processed_rows", visitor.max_processed_rows as f64);

        report
    }

    fn name(&self) -> &'static str {
        "RowEstimationAnalyzer"
    }

    fn description(&self) -> &'static str {
        "Analyzes row count estimates and identifies excessive row processing operations"
    }

    fn version(&self) -> &'static str {
        "2.0.0"
    }
}

impl ConfigurableAnalyzer for RowEstimationAnalyzer {
    type Config = RowEstimationConfig;

    fn configure(&mut self, config: Self::Config) {
        self.config = config;
    }

    fn default_config() -> Self::Config {
        AnalysisConfiguration::default().analyzers.row_estimation
    }

    fn current_config(&self) -> &Self::Config {
        &self.config
    }
}

/// Visitor implementation for collecting row estimation findings
struct RowEstimationVisitor<'a> {
    config: &'a RowEstimationConfig,
    findings: Vec<Finding>,
    // Metrics
    nodes_analyzed: usize,
    excessive_row_operations: usize,
    estimation_errors: usize,
    cartesian_products: usize,
    max_processed_rows: u64,
}

impl<'a> RowEstimationVisitor<'a> {
    fn new(config: &'a RowEstimationConfig) -> Self {
        Self {
            config,
            findings: Vec::new(),
            nodes_analyzed: 0,
            excessive_row_operations: 0,
            estimation_errors: 0,
            cartesian_products: 0,
            max_processed_rows: 0,
        }
    }

    fn analyze_excessive_rows(&mut self, node: &PlanNode, path: &NodePath) {
        if !self
            .config
            .enabled_findings
            .contains(&FindingType::ExcessiveRowProcessing)
        {
            return;
        }

        let estimated_rows = node.cost.estimated_rows;
        self.max_processed_rows = self.max_processed_rows.max(estimated_rows);

        let severity = self.config.thresholds.row_counts.classify(&estimated_rows);

        if matches!(
            severity,
            Severity::Medium | Severity::High | Severity::Critical
        ) {
            self.excessive_row_operations += 1;

            let finding = Finding::new(
                FindingType::ExcessiveRowProcessing,
                severity,
                format!("Excessive row processing detected ({} rows)", estimated_rows),
                format!(
                    "Operation {} is processing {} rows, which may indicate a performance bottleneck.",
                    node.description(), estimated_rows
                ),
                "Consider adding indexes, more selective WHERE conditions, or query restructuring to reduce the number of rows processed".to_string(),
            )
            .with_node(path.clone())
            .with_evidence("estimated_rows", estimated_rows as f64)
            .with_evidence("cost", node.cost.max_total_cost)
            .with_metadata("operation_type", &node.description());

            self.findings.push(finding);
        }
    }

    fn detect_cartesian_product(&mut self, node: &PlanNode, path: &NodePath) {
        if !self
            .config
            .enabled_findings
            .contains(&FindingType::CartesianProduct)
        {
            return;
        }

        // Look for nested loops without join conditions (potential cartesian products)
        if let crate::NodeType::Join(crate::JoinType::NestedLoop { .. }) = &node.node_type {
            let has_join_filter = node.get_property("Join Filter").is_some();
            let estimated_rows = node.cost.estimated_rows;

            if !has_join_filter && estimated_rows > 10000 {
                let left_rows = node
                    .children
                    .first()
                    .map(|c| c.cost.estimated_rows)
                    .unwrap_or(1);
                let right_rows = node
                    .children
                    .get(1)
                    .map(|c| c.cost.estimated_rows)
                    .unwrap_or(1);
                let expected_cartesian = left_rows * right_rows;

                // If result rows are close to the cartesian product, flag it
                let ratio = estimated_rows as f64 / expected_cartesian as f64;
                if ratio > 0.8 {
                    self.cartesian_products += 1;

                    let finding = Finding::new(
                        FindingType::CartesianProduct,
                        Severity::Critical,
                        "Potential cartesian product detected".to_string(),
                        format!(
                            "Nested loop join without explicit join conditions producing {} rows from {} × {} inputs (ratio: {:.2})",
                            estimated_rows, left_rows, right_rows, ratio
                        ),
                        "Add explicit JOIN conditions to prevent cartesian product. Check for missing WHERE clauses or JOIN predicates".to_string(),
                    )
                    .with_node(path.clone())
                    .with_evidence("result_rows", estimated_rows as f64)
                    .with_evidence("left_input_rows", left_rows as f64)
                    .with_evidence("right_input_rows", right_rows as f64)
                    .with_evidence("cartesian_ratio", ratio)
                    .with_metadata("has_join_filter", "false");

                    self.findings.push(finding);
                }
            }
        }
    }
}

impl<'a> NodeVisitor for RowEstimationVisitor<'a> {
    fn visit_node(&mut self, node: &PlanNode, path: &NodePath, _context: &AnalysisContext) {
        self.nodes_analyzed += 1;

        self.analyze_excessive_rows(node, path);
        self.detect_cartesian_product(node, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{JoinType, NodeType, ParsedPlan, PlanCost, PlanNode};

    #[test]
    fn test_excessive_row_detection() {
        let config = AnalysisConfiguration::default();
        let analyzer = RowEstimationAnalyzer::with_config(&config);
        let context = AnalysisContext::new();

        // Create a node with excessive rows
        let node = PlanNode::new(
            NodeType::Scan(crate::ScanType::SeqScan {
                table: crate::TableReference {
                    schema: None,
                    name: "large_table".to_string(),
                    alias: None,
                },
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 50000.0,
                estimated_rows: 1_000_000, // Should trigger excessive row warning
                estimated_width: 100,
            },
            "Seq Scan on large_table".to_string(),
        );

        let plan = ParsedPlan::new(node);
        let report = analyzer.analyze(&plan, &context);

        // Should detect excessive row processing
        assert!(!report.findings.is_empty());
        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f.finding_type, FindingType::ExcessiveRowProcessing))
        );
    }

    #[test]
    fn test_cartesian_product_detection() {
        let config = AnalysisConfiguration::default();
        let analyzer = RowEstimationAnalyzer::with_config(&config);
        let context = AnalysisContext::new();

        // Create a nested loop join without join conditions (cartesian product)
        let left_child = PlanNode::new(
            NodeType::Scan(crate::ScanType::SeqScan {
                table: crate::TableReference {
                    schema: None,
                    name: "table1".to_string(),
                    alias: None,
                },
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 100.0,
                estimated_rows: 1000,
                estimated_width: 50,
            },
            "Seq Scan on table1".to_string(),
        );

        let right_child = PlanNode::new(
            NodeType::Scan(crate::ScanType::SeqScan {
                table: crate::TableReference {
                    schema: None,
                    name: "table2".to_string(),
                    alias: None,
                },
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 100.0,
                estimated_rows: 500,
                estimated_width: 50,
            },
            "Seq Scan on table2".to_string(),
        );

        let mut join_node = PlanNode::new(
            NodeType::Join(JoinType::NestedLoop {
                inner_unique: false,
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 500000.0,
                estimated_rows: 450000, // Close to 1000 * 500 = 500000
                estimated_width: 100,
            },
            "Nested Loop".to_string(),
        );

        join_node.add_child(left_child);
        join_node.add_child(right_child);

        let plan = ParsedPlan::new(join_node);
        let report = analyzer.analyze(&plan, &context);

        // Should detect potential cartesian product
        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f.finding_type, FindingType::CartesianProduct))
        );
    }
}
