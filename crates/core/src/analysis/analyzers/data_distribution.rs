use crate::{ParsedPlan, PlanNode, NodeType};
use super::super::{
    Analyzer, ConfigurableAnalyzer, AnalysisContext, AnalysisReport, Finding,
    FindingType, Severity, NodePath
};
use super::super::consolidated_config::AnalysisConfiguration;
use super::super::traversal::{PlanTraversal, NodeVisitor};

/// Configuration for data distribution analysis
#[derive(Debug, Clone, PartialEq)]
pub struct DataDistributionConfig {
    /// Threshold for detecting data skew (ratio of max to average)
    pub skew_threshold: f64,
    /// Minimum rows for skew analysis
    pub min_rows_for_skew_analysis: u64,
}

impl Default for DataDistributionConfig {
    fn default() -> Self {
        Self {
            skew_threshold: 10.0, // 10x difference indicates skew
            min_rows_for_skew_analysis: 10000,
        }
    }
}

/// Analyzer for detecting data distribution issues and skew
pub struct DataDistributionAnalyzer {
    config: DataDistributionConfig,
}

impl DataDistributionAnalyzer {
    pub fn new() -> Self {
        Self {
            config: DataDistributionConfig::default(),
        }
    }

    pub fn with_config(_config: &AnalysisConfiguration) -> Self {
        Self {
            config: DataDistributionConfig::default(),
        }
    }
}

impl Default for DataDistributionAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer for DataDistributionAnalyzer {
    fn analyze(&self, plan: &ParsedPlan, context: &AnalysisContext) -> AnalysisReport {
        let mut report = AnalysisReport::new("DataDistributionAnalyzer".to_string())
            .with_metadata("version", self.version());

        // Create a visitor to collect distribution-related findings
        let mut visitor = DataDistributionVisitor::new(&self.config);
        PlanTraversal::depth_first(plan, &mut visitor, context);

        // Add all findings to the report
        for finding in visitor.findings {
            report = report.add_finding(finding);
        }

        // Add aggregate metrics
        report = report
            .with_metric("nodes_analyzed", visitor.nodes_analyzed as f64)
            .with_metric("parallel_workers_used", visitor.parallel_workers_used as f64)
            .with_metric("skew_detected", if visitor.skew_detected { 1.0 } else { 0.0 });

        report
    }

    fn name(&self) -> &'static str {
        "DataDistributionAnalyzer"
    }

    fn description(&self) -> &'static str {
        "Detects data distribution issues, skew, and partition-related problems"
    }

    fn version(&self) -> &'static str {
        "1.0.0"
    }
}

impl ConfigurableAnalyzer for DataDistributionAnalyzer {
    type Config = DataDistributionConfig;

    fn configure(&mut self, config: Self::Config) {
        self.config = config;
    }

    fn default_config() -> Self::Config {
        DataDistributionConfig::default()
    }

    fn current_config(&self) -> &Self::Config {
        &self.config
    }
}

/// Visitor implementation for collecting data distribution findings
struct DataDistributionVisitor<'a> {
    config: &'a DataDistributionConfig,
    findings: Vec<Finding>,
    // Metrics
    nodes_analyzed: usize,
    parallel_workers_used: usize,
    skew_detected: bool,
}

impl<'a> DataDistributionVisitor<'a> {
    fn new(config: &'a DataDistributionConfig) -> Self {
        Self {
            config,
            findings: Vec::new(),
            nodes_analyzed: 0,
            parallel_workers_used: 0,
            skew_detected: false,
        }
    }

    fn detect_parallel_worker_skew(&mut self, node: &PlanNode, path: &NodePath) {
        // Look for parallel operations with worker information
        if let Some(workers_planned_str) = node.get_property("Workers Planned") {
            if let Some(workers_launched_str) = node.get_property("Workers Launched") {
                if let (Ok(planned), Ok(launched)) = (
                    workers_planned_str.parse::<u32>(),
                    workers_launched_str.parse::<u32>(),
                ) {
                    self.parallel_workers_used += launched as usize;

                    // Check if workers were underutilized
                    if launched < planned && planned > 2 {
                        let utilization = (launched as f64 / planned as f64) * 100.0;

                        let finding = Finding::new(
                            FindingType::Custom("ParallelWorkerUnderutilization".to_string()),
                            Severity::Low,
                            "Parallel workers underutilized".to_string(),
                            format!(
                                "Only {}/{} parallel workers were launched ({:.0}% utilization) for {}",
                                launched, planned, utilization, node.description()
                            ),
                            "Check max_parallel_workers_per_gather and system resources. Data skew may also prevent full parallelization.".to_string(),
                        )
                        .with_node(path.clone())
                        .with_evidence("workers_planned", planned as f64)
                        .with_evidence("workers_launched", launched as f64)
                        .with_evidence("utilization_percent", utilization);

                        self.findings.push(finding);
                    }

                    // Check for potential data skew in parallel operations
                    if launched > 0 {
                        // Look for "Rows Removed by" which might indicate skew
                        if let Some(rows_removed_str) = node.get_property("Rows Removed by Filter") {
                            if let Ok(rows_removed) = rows_removed_str.parse::<u64>() {
                                let rows_returned = node.cost.estimated_rows;
                                if rows_removed > rows_returned * 3 {
                                    self.skew_detected = true;

                                    let skew_ratio = rows_removed as f64 / (rows_returned as f64 + 1.0);

                                    let finding = Finding::new(
                                        FindingType::Custom("DataSkew".to_string()),
                                        Severity::Medium,
                                        "Potential data skew detected".to_string(),
                                        format!(
                                            "Parallel operation filtered out {:.1}x more rows than returned, suggesting uneven data distribution",
                                            skew_ratio
                                        ),
                                        "Consider partitioning strategy or investigate data distribution patterns".to_string(),
                                    )
                                    .with_node(path.clone())
                                    .with_evidence("rows_removed", rows_removed as f64)
                                    .with_evidence("rows_returned", rows_returned as f64)
                                    .with_evidence("skew_ratio", skew_ratio);

                                    self.findings.push(finding);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn detect_partition_inefficiency(&mut self, node: &PlanNode, path: &NodePath) {
        // Look for partition-related properties
        if let Some(partitions_str) = node.get_property("Partitions Removed") {
            // Partitions being removed is actually good - means partition pruning works
            // We're more interested in scans across many partitions
        }

        // Check for append nodes which often indicate partition scans
        if let Some(subplans_str) = node.get_property("Subplans") {
            if let Ok(subplan_count) = subplans_str.parse::<usize>() {
                if subplan_count > 10 {
                    let finding = Finding::new(
                        FindingType::Custom("ManyPartitionScans".to_string()),
                        Severity::Low,
                        format!("Scanning {} partitions", subplan_count),
                        format!(
                            "Query scans {} partitions. Consider adding partition key to WHERE clause for partition pruning.",
                            subplan_count
                        ),
                        "Use partition key filters to limit the number of partitions scanned".to_string(),
                    )
                    .with_node(path.clone())
                    .with_evidence("partitions_scanned", subplan_count as f64);

                    self.findings.push(finding);
                }
            }
        }
    }

    fn detect_uneven_join_distribution(&mut self, node: &PlanNode, path: &NodePath) {
        // Look for joins where one side is much larger than the other
        if let NodeType::Join(_) = &node.node_type {
            if node.children.len() >= 2 {
                let left_rows = node.children[0].cost.estimated_rows;
                let right_rows = node.children[1].cost.estimated_rows;

                if left_rows > 0 && right_rows > 0 {
                    let ratio = if left_rows > right_rows {
                        left_rows as f64 / right_rows as f64
                    } else {
                        right_rows as f64 / left_rows as f64
                    };

                    // Significant imbalance in join inputs
                    if ratio > self.config.skew_threshold
                        && left_rows.max(right_rows) > self.config.min_rows_for_skew_analysis
                    {
                        let finding = Finding::new(
                            FindingType::Custom("UnevenJoinInputs".to_string()),
                            Severity::Low,
                            "Uneven join input sizes detected".to_string(),
                            format!(
                                "Join has significantly unbalanced inputs: {} vs {} rows ({:.1}x difference)",
                                left_rows, right_rows, ratio
                            ),
                            "Consider whether join order could be optimized or if data distribution is intentional".to_string(),
                        )
                        .with_node(path.clone())
                        .with_evidence("left_rows", left_rows as f64)
                        .with_evidence("right_rows", right_rows as f64)
                        .with_evidence("imbalance_ratio", ratio);

                        self.findings.push(finding);
                    }
                }
            }
        }
    }
}

impl<'a> NodeVisitor for DataDistributionVisitor<'a> {
    fn visit_node(&mut self, node: &PlanNode, path: &NodePath, _context: &AnalysisContext) {
        self.nodes_analyzed += 1;

        self.detect_parallel_worker_skew(node, path);
        self.detect_partition_inefficiency(node, path);
        self.detect_uneven_join_distribution(node, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PlanNode, NodeType, ScanType, JoinType, PlanCost, TableReference};

    #[test]
    fn test_data_distribution_analyzer() {
        let analyzer = DataDistributionAnalyzer::new();
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

        assert_eq!(report.analyzer_name, "DataDistributionAnalyzer");
        assert_eq!(report.metrics.get("nodes_analyzed"), Some(&1.0));
    }

    #[test]
    fn test_uneven_join_detection() {
        let analyzer = DataDistributionAnalyzer::new();
        let context = AnalysisContext::new();

        let left = PlanNode::new(
            NodeType::Scan(ScanType::SeqScan {
                table: TableReference {
                    schema: None,
                    name: "large_table".to_string(),
                    alias: None,
                },
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 10000.0,
                estimated_rows: 1_000_000, // Very large
                estimated_width: 100,
            },
            "Seq Scan on large_table".to_string(),
        );

        let right = PlanNode::new(
            NodeType::Scan(ScanType::SeqScan {
                table: TableReference {
                    schema: None,
                    name: "small_table".to_string(),
                    alias: None,
                },
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 100.0,
                estimated_rows: 100, // Very small
                estimated_width: 50,
            },
            "Seq Scan on small_table".to_string(),
        );

        let mut join = PlanNode::new(
            NodeType::Join(JoinType::NestedLoop { inner_unique: false }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 20000.0,
                estimated_rows: 50000,
                estimated_width: 150,
            },
            "Nested Loop".to_string(),
        );

        join.add_child(left);
        join.add_child(right);

        let plan = ParsedPlan::new(join);
        let report = analyzer.analyze(&plan, &context);

        // Should detect uneven join inputs
        assert!(report.findings.iter().any(|f|
            matches!(f.finding_type, FindingType::Custom(ref s) if s == "UnevenJoinInputs")
        ));
    }
}
