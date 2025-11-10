use super::super::consolidated_config::AnalysisConfiguration;
use super::super::traversal::{NodeVisitor, PlanTraversal};
use super::super::{
    AnalysisContext, AnalysisReport, Analyzer, ConfigurableAnalyzer, Finding, FindingType,
    NodePath, Severity,
};
use crate::{NodeType, ParsedPlan, PlanNode, ScanType};

/// Configuration for index effectiveness analysis
#[derive(Debug, Clone, PartialEq)]
pub struct IndexEffectivenessConfig {
    /// Minimum selectivity threshold for "good" indexes (e.g., 0.05 = 5%)
    pub min_selectivity: f64,
    /// Maximum selectivity for suggesting index removal (e.g., 0.95 = 95%)
    pub max_selectivity_for_removal: f64,
    /// Minimum rows for selectivity analysis
    pub min_rows_for_analysis: u64,
}

impl Default for IndexEffectivenessConfig {
    fn default() -> Self {
        Self {
            min_selectivity: 0.05,
            max_selectivity_for_removal: 0.95,
            min_rows_for_analysis: 1000,
        }
    }
}

/// Analyzer for index usage and effectiveness
pub struct IndexEffectivenessAnalyzer {
    config: IndexEffectivenessConfig,
}

impl IndexEffectivenessAnalyzer {
    pub fn new() -> Self {
        Self {
            config: IndexEffectivenessConfig::default(),
        }
    }

    pub fn with_config(_config: &AnalysisConfiguration) -> Self {
        Self {
            config: IndexEffectivenessConfig::default(),
        }
    }
}

impl Default for IndexEffectivenessAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer for IndexEffectivenessAnalyzer {
    fn analyze(&self, plan: &ParsedPlan, context: &AnalysisContext) -> AnalysisReport {
        let mut report = AnalysisReport::new("IndexEffectivenessAnalyzer".to_string())
            .with_metadata("version", self.version());

        // Create a visitor to collect index-related findings
        let mut visitor = IndexEffectivenessVisitor::new(&self.config);
        PlanTraversal::depth_first(plan, &mut visitor, context);

        // Add all findings to the report
        for finding in visitor.findings {
            report = report.add_finding(finding);
        }

        // Add aggregate metrics
        report = report
            .with_metric("nodes_analyzed", visitor.nodes_analyzed as f64)
            .with_metric("index_scans", visitor.index_scans as f64)
            .with_metric("seq_scans", visitor.seq_scans as f64)
            .with_metric("bitmap_scans", visitor.bitmap_scans as f64);

        if visitor.index_scans > 0 {
            let avg_selectivity = visitor.total_selectivity / visitor.index_scans as f64;
            report = report.with_metric("avg_index_selectivity", avg_selectivity);
        }

        report
    }

    fn name(&self) -> &'static str {
        "IndexEffectivenessAnalyzer"
    }

    fn description(&self) -> &'static str {
        "Analyzes index usage patterns and effectiveness, detecting poor selectivity and missing indexes"
    }

    fn version(&self) -> &'static str {
        "1.0.0"
    }
}

impl ConfigurableAnalyzer for IndexEffectivenessAnalyzer {
    type Config = IndexEffectivenessConfig;

    fn configure(&mut self, config: Self::Config) {
        self.config = config;
    }

    fn default_config() -> Self::Config {
        IndexEffectivenessConfig::default()
    }

    fn current_config(&self) -> &Self::Config {
        &self.config
    }
}

/// Visitor implementation for collecting index effectiveness findings
struct IndexEffectivenessVisitor<'a> {
    config: &'a IndexEffectivenessConfig,
    findings: Vec<Finding>,
    // Metrics
    nodes_analyzed: usize,
    index_scans: usize,
    seq_scans: usize,
    bitmap_scans: usize,
    total_selectivity: f64,
}

impl<'a> IndexEffectivenessVisitor<'a> {
    fn new(config: &'a IndexEffectivenessConfig) -> Self {
        Self {
            config,
            findings: Vec::new(),
            nodes_analyzed: 0,
            index_scans: 0,
            seq_scans: 0,
            bitmap_scans: 0,
            total_selectivity: 0.0,
        }
    }

    fn analyze_index_selectivity(&mut self, node: &PlanNode, path: &NodePath, table_rows: u64) {
        if table_rows < self.config.min_rows_for_analysis {
            return; // Too few rows for meaningful analysis
        }

        let selected_rows = node.cost.estimated_rows;
        let selectivity = selected_rows as f64 / table_rows as f64;
        self.total_selectivity += selectivity;

        // Poor selectivity - index not selective enough
        if selectivity > self.config.max_selectivity_for_removal {
            let finding = Finding::new(
                FindingType::PoorIndexSelectivity,
                Severity::Medium,
                format!("Index has poor selectivity ({:.1}%)", selectivity * 100.0),
                format!(
                    "Index on {} returns {:.1}% of table rows ({} out of {}). Consider sequential scan or different index.",
                    node.description(), selectivity * 100.0, selected_rows, table_rows
                ),
                "Review index design; high selectivity indexes may be slower than sequential scans".to_string(),
            )
            .with_node(path.clone())
            .with_evidence("selectivity", selectivity)
            .with_evidence("selected_rows", selected_rows as f64)
            .with_evidence("table_rows", table_rows as f64);

            self.findings.push(finding);
        }

        // Very good selectivity - highlight efficient index
        if selectivity < self.config.min_selectivity && selectivity > 0.0 {
            // This is actually good! But we'll track it for informational purposes
            // Don't add a finding, just note it in metrics
        }
    }

    fn detect_missing_index_opportunity(&mut self, node: &PlanNode, path: &NodePath) {
        // Look for sequential scans with filters that could benefit from indexes
        if let NodeType::Scan(ScanType::SeqScan { .. }) = &node.node_type {
            if let Some(filter) = node.get_property("Filter") {
                // Check if there's a simple equality filter
                if filter.contains("=") && !filter.contains("OR") {
                    let estimated_rows = node.cost.estimated_rows;

                    // If the scan returns a small percentage of rows, index could help
                    if estimated_rows > 10000 && estimated_rows < 100000 {
                        let finding = Finding::new(
                            FindingType::MissingIndex,
                            Severity::Medium,
                            "Potential missing index opportunity".to_string(),
                            format!(
                                "Sequential scan on {} with selective filter: '{}'. An index could improve performance.",
                                node.description(), filter
                            ),
                            "Consider creating an index on the filtered column(s)".to_string(),
                        )
                        .with_node(path.clone())
                        .with_evidence("estimated_rows", estimated_rows as f64)
                        .with_metadata("filter_condition", &filter);

                        self.findings.push(finding);
                    }
                }
            }
        }
    }

    fn analyze_bitmap_scan_efficiency(&mut self, node: &PlanNode, path: &NodePath) {
        // Bitmap scans that return most of the table might be better as seq scans
        if let NodeType::Scan(ScanType::BitmapHeapScan { .. }) = &node.node_type {
            // Check if a child node shows the total table size
            for child in &node.children {
                if let NodeType::Scan(ScanType::BitmapIndexScan { .. }) = &child.node_type {
                    let selected_rows = node.cost.estimated_rows;

                    // If bitmap scan returns a very high percentage, might be inefficient
                    if selected_rows > 100000 {
                        let finding = Finding::new(
                            FindingType::Custom("InefficientBitmapScan".to_string()),
                            Severity::Low,
                            "Bitmap scan may be inefficient for large result sets".to_string(),
                            format!(
                                "Bitmap scan returning {} rows. Consider if sequential scan or index-only scan would be better.",
                                selected_rows
                            ),
                            "Review query selectivity and consider alternative access methods".to_string(),
                        )
                        .with_node(path.clone())
                        .with_evidence("selected_rows", selected_rows as f64);

                        self.findings.push(finding);
                    }
                }
            }
        }
    }
}

impl<'a> NodeVisitor for IndexEffectivenessVisitor<'a> {
    fn visit_node(&mut self, node: &PlanNode, path: &NodePath, _context: &AnalysisContext) {
        self.nodes_analyzed += 1;

        match &node.node_type {
            NodeType::Scan(scan_type) => {
                match scan_type {
                    ScanType::IndexScan { .. } => {
                        self.index_scans += 1;
                        // Try to estimate table size from parent or properties
                        // For now, use a heuristic based on cost
                        let estimated_table_rows = (node.cost.max_total_cost * 10.0) as u64;
                        self.analyze_index_selectivity(node, path, estimated_table_rows);
                    }
                    ScanType::SeqScan { .. } => {
                        self.seq_scans += 1;
                        self.detect_missing_index_opportunity(node, path);
                    }
                    ScanType::BitmapHeapScan { .. } | ScanType::BitmapIndexScan { .. } => {
                        self.bitmap_scans += 1;
                        self.analyze_bitmap_scan_efficiency(node, path);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IndexReference, NodeType, PlanCost, PlanNode, ScanType, TableReference};

    #[test]
    fn test_index_effectiveness_analyzer() {
        let analyzer = IndexEffectivenessAnalyzer::new();
        let context = AnalysisContext::new();

        let node = PlanNode::new(
            NodeType::Scan(ScanType::IndexScan {
                table: TableReference {
                    schema: Some("public".to_string()),
                    name: "test_table".to_string(),
                    alias: None,
                },
                index: Some(IndexReference {
                    name: "idx_test".to_string(),
                }),
                backward: false,
                only: false,
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 100.0,
                estimated_rows: 10,
                estimated_width: 50,
            },
            "Index Scan using idx_test".to_string(),
        );

        let plan = ParsedPlan::new(node);
        let report = analyzer.analyze(&plan, &context);

        assert_eq!(report.analyzer_name, "IndexEffectivenessAnalyzer");
        assert_eq!(report.metrics.get("index_scans"), Some(&1.0));
    }

    #[test]
    fn test_missing_index_detection() {
        let analyzer = IndexEffectivenessAnalyzer::new();
        let context = AnalysisContext::new();

        let mut node = PlanNode::new(
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
                max_total_cost: 5000.0,
                estimated_rows: 50000,
                estimated_width: 100,
            },
            "Seq Scan on large_table".to_string(),
        );

        // Add a filter that could benefit from an index
        node.set_property("Filter".to_string(), "(user_id = 123)".to_string());

        let plan = ParsedPlan::new(node);
        let report = analyzer.analyze(&plan, &context);

        // Should detect potential for index
        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f.finding_type, FindingType::MissingIndex))
        );
    }
}
