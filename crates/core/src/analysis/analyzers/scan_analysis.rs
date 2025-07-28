use super::super::enhanced_config::EnhancedScanAnalysisConfig;
use super::super::traversal::{NodeVisitor, PlanTraversal};
use super::super::unified_config::{OperationType, UnifiedAnalysis};
use super::super::{
    AnalysisContext, AnalysisReport, Analyzer, ConfigurableAnalyzer, Finding, FindingType,
    NodePath, Severity,
};
use crate::{NodeType, ParsedPlan, PlanNode, ScanType};

/// Analyzer for scan operation efficiency and index usage
pub struct ScanAnalyzer {
    config: EnhancedScanAnalysisConfig,
}

impl ScanAnalyzer {
    pub fn new() -> Self {
        let context = super::super::unified_config::UnifiedAnalysisContext::default();
        Self {
            config: EnhancedScanAnalysisConfig::new(&context),
        }
    }

    pub fn with_config(config: EnhancedScanAnalysisConfig) -> Self {
        Self { config }
    }
}

impl Default for ScanAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer for ScanAnalyzer {
    fn analyze(&self, plan: &ParsedPlan, context: &AnalysisContext) -> AnalysisReport {
        let mut report = AnalysisReport::new("ScanAnalyzer".to_string())
            .with_metadata("version", self.version())
            .with_metadata("config_version", "2.0");

        // Create a visitor to collect scan-related findings
        let mut visitor = ScanAnalysisVisitor::new(&self.config, context);
        PlanTraversal::depth_first(plan, &mut visitor, context);

        // Add all findings to the report
        for finding in visitor.findings {
            report = report.add_finding(finding);
        }

        // Add aggregate metrics
        report = report
            .with_metric("nodes_analyzed", visitor.nodes_analyzed as f64)
            .with_metric("sequential_scans_found", visitor.sequential_scans as f64)
            .with_metric("index_scans_found", visitor.index_scans as f64)
            .with_metric("bitmap_scans_found", visitor.bitmap_scans as f64)
            .with_metric("large_scans_detected", visitor.large_scans as f64)
            .with_metric(
                "inefficient_scans_detected",
                visitor.inefficient_scans as f64,
            )
            .with_metric("max_scan_rows", visitor.max_scan_rows as f64);

        report
    }

    fn name(&self) -> &'static str {
        "ScanAnalyzer"
    }

    fn description(&self) -> &'static str {
        "Analyzes scan operations for efficiency and identifies potential index improvements"
    }

    fn version(&self) -> &'static str {
        "2.0.0"
    }
}

impl ConfigurableAnalyzer for ScanAnalyzer {
    type Config = EnhancedScanAnalysisConfig;

    fn configure(&mut self, config: Self::Config) {
        self.config = config;
    }

    fn default_config() -> Self::Config {
        let context = super::super::unified_config::UnifiedAnalysisContext::default();
        EnhancedScanAnalysisConfig::new(&context)
    }

    fn current_config(&self) -> &Self::Config {
        &self.config
    }
}

impl UnifiedAnalysis for ScanAnalyzer {
    fn get_operation_type(&self) -> OperationType {
        OperationType::Scan
    }
}

/// Visitor implementation for collecting scan analysis findings
struct ScanAnalysisVisitor<'a> {
    config: &'a EnhancedScanAnalysisConfig,
    context: &'a AnalysisContext,
    findings: Vec<Finding>,
    // Metrics
    nodes_analyzed: usize,
    sequential_scans: usize,
    index_scans: usize,
    bitmap_scans: usize,
    large_scans: usize,
    inefficient_scans: usize,
    max_scan_rows: u64,
}

impl<'a> ScanAnalysisVisitor<'a> {
    fn new(config: &'a EnhancedScanAnalysisConfig, context: &'a AnalysisContext) -> Self {
        Self {
            config,
            context,
            findings: Vec::new(),
            nodes_analyzed: 0,
            sequential_scans: 0,
            index_scans: 0,
            bitmap_scans: 0,
            large_scans: 0,
            inefficient_scans: 0,
            max_scan_rows: 0,
        }
    }

    fn analyze_sequential_scan(&mut self, node: &PlanNode, path: &NodePath) {
        if !self
            .config
            .enabled_findings
            .contains(&FindingType::LargeSequentialScan)
        {
            return;
        }

        self.sequential_scans += 1;
        let estimated_rows = node.cost.estimated_rows;
        let actual_rows = node.actuals.as_ref().and_then(|a| a.actual_rows);
        let row_count = actual_rows.unwrap_or(estimated_rows);

        self.max_scan_rows = self.max_scan_rows.max(row_count);

        // Use unified threshold classification for sequential scans
        let severity = self
            .config
            .seq_scan_thresholds
            .row_count
            .classify_severity(row_count);

        let finding = match severity {
            Severity::Critical | Severity::High => {
                self.large_scans += 1;

                let table_name = node.extract_table_name();

                Some(Finding::new(
                    FindingType::LargeSequentialScan,
                    severity.clone(),
                    format!("Large sequential scan on {}", table_name),
                    format!(
                        "Sequential scan processing {} rows on table '{}'. This may indicate missing indexes or inefficient query conditions.",
                        row_count, table_name
                    ),
                    "Consider adding appropriate indexes, using LIMIT clauses, or adding WHERE conditions to reduce the dataset".to_string(),
                )
                .with_node(path.clone())
                .with_evidence("row_count", row_count as f64)
                .with_evidence("estimated_rows", estimated_rows as f64)
                .with_evidence("cost", node.cost.max_total_cost)
                .with_evidence("severity_threshold", match severity {
                    Severity::Critical => self.config.seq_scan_thresholds.row_count.critical as f64,
                    Severity::High => self.config.seq_scan_thresholds.row_count.high as f64,
                    _ => 0.0,
                })
                .with_metadata("table_name", &table_name)
                .with_metadata("has_actual_data", &actual_rows.is_some().to_string()))
            }

            Severity::Medium => {
                self.large_scans += 1;

                let table_name = node.extract_table_name();

                Some(Finding::new(
                    FindingType::LargeSequentialScan,
                    Severity::Medium,
                    format!("Moderate sequential scan on {}", table_name),
                    format!(
                        "Sequential scan processing {} rows on table '{}'. Consider optimization if this query runs frequently.",
                        row_count, table_name
                    ),
                    "Review query patterns and consider indexing if this scan occurs frequently".to_string(),
                )
                .with_node(path.clone())
                .with_evidence("row_count", row_count as f64)
                .with_evidence("estimated_rows", estimated_rows as f64)
                .with_evidence("cost", node.cost.max_total_cost)
                .with_evidence("severity_threshold", self.config.seq_scan_thresholds.row_count.medium as f64)
                .with_metadata("table_name", &table_name))
            }

            Severity::Low => None, // Below reporting threshold
        };

        if let Some(finding) = finding {
            self.findings.push(finding);
        }
    }

    fn analyze_index_scan_efficiency(&mut self, node: &PlanNode, path: &NodePath) {
        if !self
            .config
            .enabled_findings
            .contains(&FindingType::InefficiientScan)
        {
            return;
        }

        self.index_scans += 1;
        let estimated_rows = node.cost.estimated_rows;
        let cost = node.cost.max_total_cost;

        // Check for high-cost index scans that might be inefficient
        let cost_severity = self
            .config
            .index_scan_thresholds
            .cost
            .classify_severity(cost);
        let row_severity = self
            .config
            .index_scan_thresholds
            .row_count
            .classify_severity(estimated_rows);

        // Report if either cost or row count is concerning
        let severity = std::cmp::max(cost_severity, row_severity);

        if matches!(severity, Severity::High | Severity::Critical) {
            self.inefficient_scans += 1;

            let index_name = node.extract_index_name();
            let table_name = node.extract_table_name();

            let finding = Finding::new(
                FindingType::InefficiientScan,
                severity.clone(),
                format!("Inefficient index scan on {}", table_name),
                format!(
                    "Index scan on '{}' using index '{}' has high cost ({:.0}) or processes many rows ({}). Index may have poor selectivity.",
                    table_name, index_name, cost, estimated_rows
                ),
                "Review index selectivity, consider composite indexes, or check if query conditions match index columns effectively".to_string(),
            )
            .with_node(path.clone())
            .with_evidence("cost", cost)
            .with_evidence("row_count", estimated_rows as f64)
            .with_evidence("cost_threshold", match severity {
                Severity::Critical => self.config.index_scan_thresholds.cost.extreme,
                Severity::High => self.config.index_scan_thresholds.cost.high,
                _ => 0.0,
            })
            .with_metadata("index_name", &index_name)
            .with_metadata("table_name", &table_name)
            .with_metadata("scan_type", "index_scan");

            self.findings.push(finding);
        }
    }

    fn analyze_bitmap_scan(&mut self, node: &PlanNode, path: &NodePath) {
        self.bitmap_scans += 1;

        // Bitmap scans are generally good, but report if they're processing excessive rows
        let estimated_rows = node.cost.estimated_rows;
        let severity = self
            .config
            .bitmap_scan_thresholds
            .row_count
            .classify_severity(estimated_rows);

        if matches!(severity, Severity::High | Severity::Critical) {
            let table_name = node.extract_table_name();

            let finding = Finding::new(
                FindingType::InefficiientScan,
                severity.clone(),
                format!("Large bitmap scan on {}", table_name),
                format!(
                    "Bitmap scan processing {} rows on table '{}'. While bitmap scans are efficient, this volume suggests potential for further optimization.",
                    estimated_rows, table_name
                ),
                "Consider additional WHERE conditions, query restructuring, or partitioning for very large datasets".to_string(),
            )
            .with_node(path.clone())
            .with_evidence("row_count", estimated_rows as f64)
            .with_evidence("cost", node.cost.max_total_cost)
            .with_evidence("severity_threshold", match severity {
                Severity::Critical => self.config.bitmap_scan_thresholds.row_count.critical as f64,
                Severity::High => self.config.bitmap_scan_thresholds.row_count.high as f64,
                _ => 0.0,
            })
            .with_metadata("table_name", &table_name)
            .with_metadata("scan_type", "bitmap_scan");

            self.findings.push(finding);
        }
    }
}

impl<'a> NodeVisitor for ScanAnalysisVisitor<'a> {
    fn visit_node(&mut self, node: &PlanNode, path: &NodePath, _context: &AnalysisContext) {
        if let NodeType::Scan(scan_type) = &node.node_type {
            self.nodes_analyzed += 1;

            match scan_type {
                ScanType::SeqScan { .. } => {
                    self.analyze_sequential_scan(node, path);
                }
                ScanType::IndexScan { .. } => {
                    self.analyze_index_scan_efficiency(node, path);
                }
                ScanType::BitmapIndexScan { .. } | ScanType::BitmapHeapScan { .. } => {
                    self.analyze_bitmap_scan(node, path);
                }
                ScanType::ParallelBitmapHeapScan { .. } => {
                    self.analyze_bitmap_scan(node, path);
                }
            }
        }
    }
}
