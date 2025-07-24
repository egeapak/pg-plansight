use crate::{ParsedPlan, PlanNode};
use super::super::{
    Analyzer, ConfigurableAnalyzer, AnalysisContext, AnalysisReport, Finding, 
    FindingType, Severity, NodePath
};
use super::super::enhanced_config::EnhancedRowEstimationConfig;
use super::super::unified_config::{OperationType, UnifiedAnalysis};
use super::super::traversal::{PlanTraversal, NodeVisitor};

/// Analyzer for row estimation accuracy and excessive row processing
pub struct RowEstimationAnalyzer {
    config: EnhancedRowEstimationConfig,
}

impl RowEstimationAnalyzer {
    pub fn new() -> Self {
        // Use default unified context for initialization
        let context = super::super::unified_config::UnifiedAnalysisContext::default();
        Self {
            config: EnhancedRowEstimationConfig::new(&context),
        }
    }
    
    pub fn with_config(config: EnhancedRowEstimationConfig) -> Self {
        Self { config }
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
            .with_metadata("version", self.version())
            .with_metadata("config_version", "1.0");
        
        // Create a visitor to collect row-related findings
        let mut visitor = RowEstimationVisitor::new(&self.config, context);
        PlanTraversal::depth_first(plan, &mut visitor, context);
        
        // Add all findings to the report
        for finding in visitor.findings {
            report = report.add_finding(finding);
        }
        
        // Add aggregate metrics
        report = report
            .with_metric("total_nodes_analyzed", visitor.nodes_analyzed as f64)
            .with_metric("estimation_errors_found", visitor.estimation_errors as f64)
            .with_metric("excessive_row_operations", visitor.excessive_row_ops as f64)
            .with_metric("cartesian_products_detected", visitor.cartesian_products as f64)
            .with_metric("max_row_count_seen", visitor.max_row_count as f64)
            .with_metric("avg_estimation_error_ratio", visitor.total_error_ratio / visitor.estimation_errors.max(1) as f64);
        
        report
    }
    
    fn name(&self) -> &'static str {
        "RowEstimationAnalyzer"
    }
    
    fn description(&self) -> &'static str {
        "Analyzes row estimation accuracy and identifies excessive row processing operations"
    }
    
    fn version(&self) -> &'static str {
        "1.0.0"
    }
}

impl ConfigurableAnalyzer for RowEstimationAnalyzer {
    type Config = EnhancedRowEstimationConfig;
    
    fn configure(&mut self, config: Self::Config) {
        self.config = config;
    }
    
    fn default_config() -> Self::Config {
        let context = super::super::unified_config::UnifiedAnalysisContext::default();
        EnhancedRowEstimationConfig::new(&context)
    }
    
    fn current_config(&self) -> &Self::Config {
        &self.config
    }
}

impl UnifiedAnalysis for RowEstimationAnalyzer {
    fn get_operation_type(&self) -> OperationType {
        OperationType::Scan
    }
}

/// Visitor implementation for collecting row estimation findings
struct RowEstimationVisitor<'a> {
    config: &'a EnhancedRowEstimationConfig,
    context: &'a AnalysisContext,
    findings: Vec<Finding>,
    // Metrics
    nodes_analyzed: usize,
    estimation_errors: usize,
    excessive_row_ops: usize,
    cartesian_products: usize,
    max_row_count: u64,
    total_error_ratio: f64,
}

impl<'a> RowEstimationVisitor<'a> {
    fn new(config: &'a EnhancedRowEstimationConfig, context: &'a AnalysisContext) -> Self {
        Self {
            config,
            context,
            findings: Vec::new(),
            nodes_analyzed: 0,
            estimation_errors: 0,
            excessive_row_ops: 0,
            cartesian_products: 0,
            max_row_count: 0,
            total_error_ratio: 0.0,
        }
    }
    
    fn analyze_excessive_rows(&mut self, node: &PlanNode, path: &NodePath) {
        if !self.config.enabled_findings.contains(&FindingType::ExcessiveRowProcessing) {
            return;
        }
        
        let estimated_rows = node.cost.estimated_rows;
        let actual_rows = node.actuals.as_ref().and_then(|a| a.actual_rows);
        let row_count = actual_rows.unwrap_or(estimated_rows);
        
        // Skip analysis if below minimum threshold
        if row_count < self.config.min_rows_for_analysis {
            return;
        }
        
        // Update max row count metric
        self.max_row_count = self.max_row_count.max(row_count);
        
        // Use unified threshold classification
        let severity = self.config.classify_row_severity(row_count);
        
        let finding = match severity {
            Severity::Critical => {
                self.excessive_row_ops += 1;
                Some(Finding::new(
                    FindingType::ExcessiveRowProcessing,
                    Severity::Critical,
                    "Excessive row processing detected".to_string(),
                    format!("Node is processing {} rows, which may cause severe performance issues", row_count),
                    "Consider adding WHERE clauses to reduce the dataset, implementing pagination, or partitioning large tables".to_string(),
                )
                .with_node(path.clone())
                .with_evidence("row_count", row_count as f64)
                .with_evidence("estimated_rows", estimated_rows as f64)
                .with_evidence("severity_threshold", self.config.thresholds.row_count.critical as f64)
                .with_metadata("node_type", &node.description())
                .with_metadata("has_actual_data", &actual_rows.is_some().to_string()))
            },
            
            Severity::High => {
                self.excessive_row_ops += 1;
                Some(Finding::new(
                    FindingType::ExcessiveRowProcessing,
                    Severity::High,
                    "Large row processing operation".to_string(),
                    format!("Node is processing {} rows, which may impact query performance", row_count),
                    "Consider optimizing with indexes, additional WHERE conditions, or query restructuring".to_string(),
                )
                .with_node(path.clone())
                .with_evidence("row_count", row_count as f64)
                .with_evidence("estimated_rows", estimated_rows as f64)
                .with_evidence("severity_threshold", self.config.thresholds.row_count.high as f64)
                .with_metadata("node_type", &node.description()))
            },
            
            Severity::Medium => {
                self.excessive_row_ops += 1;
                Some(Finding::new(
                    FindingType::ExcessiveRowProcessing,
                    Severity::Medium,
                    "Moderate row processing detected".to_string(),
                    format!("Node is processing {} rows, which could be optimized", row_count),
                    "Review query conditions and consider indexing strategies to reduce row processing".to_string(),
                )
                .with_node(path.clone())
                .with_evidence("row_count", row_count as f64)
                .with_evidence("estimated_rows", estimated_rows as f64)
                .with_evidence("severity_threshold", self.config.thresholds.row_count.medium as f64)
                .with_metadata("node_type", &node.description()))
            },
            
            Severity::Low => None, // Below reporting threshold
        };
        
        if let Some(finding) = finding {
            self.findings.push(finding);
        }
    }
    
    fn analyze_estimation_accuracy(&mut self, node: &PlanNode, path: &NodePath) {
        if !self.config.enabled_findings.contains(&FindingType::RowEstimationError) {
            return;
        }
        
        if let Some(actuals) = &node.actuals {
            if let Some(actual_rows) = actuals.actual_rows {
                let estimated_rows = node.cost.estimated_rows as f64;
                let actual_rows_f = actual_rows as f64;
                
                // Skip analysis for very small row counts
                if estimated_rows < self.config.min_rows_for_analysis as f64 
                   || actual_rows_f < self.config.min_rows_for_analysis as f64 {
                    return;
                }
                
                let error_ratio = (actual_rows_f - estimated_rows).abs() / estimated_rows;
                self.total_error_ratio += error_ratio;
                
                // Use unified threshold classification for error ratios
                let severity = self.config.classify_error_severity(error_ratio);
                
                let finding = match severity {
                    Severity::Critical => {
                        self.estimation_errors += 1;
                        let factor = if actual_rows_f > estimated_rows {
                            actual_rows_f / estimated_rows
                        } else {
                            estimated_rows / actual_rows_f
                        };
                        
                        Some(Finding::new(
                            FindingType::RowEstimationError,
                            Severity::Critical,
                            "Severe row estimation error".to_string(),
                            format!(
                                "Estimated {} rows but actually processed {} rows ({:.1}x {})",
                                estimated_rows as u64,
                                actual_rows,
                                factor,
                                if actual_rows_f > estimated_rows { "underestimate" } else { "overestimate" }
                            ),
                            "Run ANALYZE on affected tables, check for data skew, consider column statistics, or update table statistics more frequently".to_string(),
                        )
                        .with_node(path.clone())
                        .with_evidence("error_ratio", error_ratio)
                        .with_evidence("estimated_rows", estimated_rows)
                        .with_evidence("actual_rows", actual_rows_f)
                        .with_evidence("estimation_factor", factor)
                        .with_evidence("severity_threshold", self.config.thresholds.error_ratios.severe)
                        .with_metadata("estimation_type", if actual_rows_f > estimated_rows { "underestimate" } else { "overestimate" })
                        .with_metadata("node_type", &node.description()))
                    },
                    
                    Severity::High => {
                        self.estimation_errors += 1;
                        Some(Finding::new(
                            FindingType::RowEstimationError,
                            Severity::High,
                            "Significant row estimation error".to_string(),
                            format!(
                                "Estimated {} rows but actually processed {} rows ({:.1}x error ratio)",
                                estimated_rows as u64,
                                actual_rows,
                                error_ratio
                            ),
                            "Consider running ANALYZE on the affected tables or checking for data distribution issues".to_string(),
                        )
                        .with_node(path.clone())
                        .with_evidence("error_ratio", error_ratio)
                        .with_evidence("estimated_rows", estimated_rows)
                        .with_evidence("actual_rows", actual_rows_f)
                        .with_evidence("severity_threshold", self.config.thresholds.error_ratios.high))
                    },
                    
                    Severity::Medium => {
                        self.estimation_errors += 1;
                        Some(Finding::new(
                            FindingType::RowEstimationError,
                            Severity::Medium,
                            "Moderate row estimation error".to_string(),
                            format!(
                                "Estimated {} rows but actually processed {} rows ({:.1}x error ratio)",
                                estimated_rows as u64,
                                actual_rows,
                                error_ratio
                            ),
                            "Monitor estimation accuracy and consider updating table statistics if this occurs frequently".to_string(),
                        )
                        .with_node(path.clone())
                        .with_evidence("error_ratio", error_ratio)
                        .with_evidence("estimated_rows", estimated_rows)
                        .with_evidence("actual_rows", actual_rows_f)
                        .with_evidence("severity_threshold", self.config.thresholds.error_ratios.medium))
                    },
                    
                    Severity::Low => None, // Below reporting threshold
                };
                
                if let Some(finding) = finding {
                    self.findings.push(finding);
                }
            }
        }
    }
    
    fn analyze_cartesian_product(&mut self, node: &PlanNode, path: &NodePath) {
        if !self.config.enabled_findings.contains(&FindingType::CartesianProduct) 
           || !self.config.cartesian_product.enabled {
            return;
        }
        
        // Only analyze join nodes
        if !node.is_join() || node.children.len() != 2 {
            return;
        }
        
        let left_rows = node.children[0].cost.estimated_rows as f64;
        let right_rows = node.children[1].cost.estimated_rows as f64;
        let result_rows = node.cost.estimated_rows as f64;
        
        let expected_cartesian = left_rows * right_rows;
        
        // Skip if below minimum threshold
        if expected_cartesian < self.config.cartesian_product.min_total_rows_for_detection as f64 {
            return;
        }
        
        let cartesian_ratio = result_rows / expected_cartesian;
        
        if cartesian_ratio >= self.config.cartesian_product.min_cartesian_ratio {
            self.cartesian_products += 1;
            
            let join_filter = node.get_property("Join Filter");
            let index_cond = node.get_property("Index Cond");
            let has_conditions = join_filter.is_some() || index_cond.is_some();
            
            // Use more nuanced severity based on cartesian ratio
            let severity = if cartesian_ratio >= 0.9 {
                Severity::Critical
            } else if cartesian_ratio >= 0.7 {
                Severity::High
            } else if cartesian_ratio >= 0.5 {
                Severity::Medium
            } else {
                Severity::Low // Still report but with lower severity
            };
            
            let suggestion = if !has_conditions {
                "Review join conditions - missing or incorrect ON clause may be causing a cartesian product".to_string()
            } else {
                "Join conditions exist but are not selective enough - consider adding additional filters or checking join logic".to_string()
            };
            
            let finding = Finding::new(
                FindingType::CartesianProduct,
                severity,
                "Potential cartesian product detected".to_string(),
                format!(
                    "Join producing {} rows from {} × {} input rows ({:.1}% of full cartesian product)",
                    result_rows as u64,
                    left_rows as u64,
                    right_rows as u64,
                    cartesian_ratio * 100.0
                ),
                suggestion,
            )
            .with_node(path.clone())
            .with_evidence("cartesian_ratio", cartesian_ratio)
            .with_evidence("expected_cartesian", expected_cartesian)
            .with_evidence("actual_rows", result_rows)
            .with_evidence("left_input_rows", left_rows)
            .with_evidence("right_input_rows", right_rows)
            .with_evidence("min_cartesian_ratio", self.config.cartesian_product.min_cartesian_ratio)
            .with_metadata("join_type", &format!("{:?}", node.node_type))
            .with_metadata("has_join_filter", &join_filter.is_some().to_string())
            .with_metadata("has_index_condition", &index_cond.is_some().to_string());
            
            self.findings.push(finding);
        }
    }
}

impl<'a> NodeVisitor for RowEstimationVisitor<'a> {
    fn visit_node(&mut self, node: &PlanNode, path: &NodePath, _context: &AnalysisContext) {
        self.nodes_analyzed += 1;
        
        // Analyze different aspects of row estimation
        self.analyze_excessive_rows(node, path);
        self.analyze_estimation_accuracy(node, path);
        self.analyze_cartesian_product(node, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PlanNode, NodeType, ScanType, JoinType, PlanCost, ParsedPlan, PlanSourceFormat, PlanActuals};
    
    fn create_test_node(
        node_type: NodeType,
        estimated_rows: u64,
        actual_rows: Option<u64>,
    ) -> PlanNode {
        let cost = PlanCost {
            startup_cost: 0.0,
            min_total_cost: 0.0,
            max_total_cost: 100.0,
            estimated_rows,
            estimated_width: 50,
        };
        
        let mut node = PlanNode::new(node_type, cost, "Test node".to_string());
        
        if let Some(actual) = actual_rows {
            let actuals = PlanActuals {
                actual_time_ms: Some(100.0),
                actual_rows: Some(actual),
                actual_loops: Some(1),
            };
            node.set_actuals(actuals);
        }
        
        node
    }
    
    #[test]
    fn test_excessive_row_detection() {
        let analyzer = RowEstimationAnalyzer::new();
        let context = AnalysisContext::new();
        
        // Create a plan with excessive rows
        let root = create_test_node(
            NodeType::Scan(ScanType::SeqScan),
            2_000_000, // Above critical threshold
            None,
        );
        
        let plan = ParsedPlan::new(root, "test".to_string(), PlanSourceFormat::Text);
        let report = analyzer.analyze(&plan, &context);
        
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].finding_type, FindingType::ExcessiveRowProcessing);
        assert_eq!(report.findings[0].severity, Severity::Critical);
        assert_eq!(report.metrics.get("excessive_row_operations"), Some(&1.0));
    }
    
    #[test]
    fn test_estimation_error_detection() {
        let analyzer = RowEstimationAnalyzer::new();
        let context = AnalysisContext::new();
        
        // Create a plan with estimation error
        let root = create_test_node(
            NodeType::Scan(ScanType::IndexScan),
            1_000,     // Estimated
            Some(50_000), // Actual (50x underestimate)
        );
        
        let plan = ParsedPlan::new(root, "test".to_string(), PlanSourceFormat::Text);
        let report = analyzer.analyze(&plan, &context);
        
        assert_eq!(report.findings.len(), 2); // Both excessive rows and estimation error
        
        let estimation_error = report.findings.iter()
            .find(|f| f.finding_type == FindingType::RowEstimationError)
            .unwrap();
        
        assert_eq!(estimation_error.severity, Severity::Critical);
        assert!(estimation_error.evidence.get("error_ratio").unwrap() > &10.0);
    }
    
    #[test]
    fn test_cartesian_product_detection() {
        let analyzer = RowEstimationAnalyzer::new();
        let context = AnalysisContext::new();
        
        // Create a join that produces near-cartesian product
        let left_child = create_test_node(
            NodeType::Scan(ScanType::SeqScan),
            1_000,
            None,
        );
        
        let right_child = create_test_node(
            NodeType::Scan(ScanType::SeqScan),
            2_000,
            None,
        );
        
        let mut root = create_test_node(
            NodeType::Join(JoinType::NestedLoop),
            1_800_000, // 90% of 1000 * 2000 = cartesian product
            None,
        );
        
        root.add_child(left_child);
        root.add_child(right_child);
        
        let plan = ParsedPlan::new(root, "test".to_string(), PlanSourceFormat::Text);
        let report = analyzer.analyze(&plan, &context);
        
        let cartesian_finding = report.findings.iter()
            .find(|f| f.finding_type == FindingType::CartesianProduct);
        
        assert!(cartesian_finding.is_some());
        assert_eq!(report.metrics.get("cartesian_products_detected"), Some(&1.0));
    }
    
    #[test]
    fn test_configurable_thresholds() {
        let mut config = RowEstimationConfig::default();
        config.row_thresholds.critical_row_count = 500_000; // Lower threshold
        
        let mut analyzer = RowEstimationAnalyzer::new();
        analyzer.configure(config);
        
        let context = AnalysisContext::new();
        
        // Create a plan that would be "high" with default config but "critical" with custom config
        let root = create_test_node(
            NodeType::Scan(ScanType::SeqScan),
            750_000,
            None,
        );
        
        let plan = ParsedPlan::new(root, "test".to_string(), PlanSourceFormat::Text);
        let report = analyzer.analyze(&plan, &context);
        
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].severity, Severity::Critical);
    }
    
    #[test]
    fn test_disabled_findings() {
        let mut config = RowEstimationConfig::default();
        config.enabled_findings = vec![FindingType::ExcessiveRowProcessing]; // Only this one enabled
        
        let analyzer = RowEstimationAnalyzer::with_config(config);
        let context = AnalysisContext::new();
        
        // Create a plan with both excessive rows and estimation error
        let root = create_test_node(
                NodeType::Scan(ScanType::SeqScan),
            200_000, // Above high threshold
            Some(2_000_000), // Huge estimation error
        );
        
        let plan = ParsedPlan::new(root, "test".to_string(), PlanSourceFormat::Text);
        let report = analyzer.analyze(&plan, &context);
        
        // Should only find excessive rows, not estimation error
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].finding_type, FindingType::ExcessiveRowProcessing);
    }
}