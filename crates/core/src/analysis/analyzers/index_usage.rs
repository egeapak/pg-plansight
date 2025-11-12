use super::super::consolidated_config::AnalysisConfiguration;
use super::super::traversal::{NodeVisitor, PlanTraversal};
use super::super::{
    AnalysisContext, AnalysisReport, Analyzer, Finding, FindingType, NodePath, Severity,
};
use crate::{NodeType, ParsedPlan, PlanNode, ScanType};

/// Analyzer for index usage patterns and missing index opportunities
///
/// This analyzer focuses on reliable patterns that indicate index issues
/// without trying to calculate selectivity with arbitrary formulas.
pub struct IndexUsageAnalyzer;

impl IndexUsageAnalyzer {
    pub fn new() -> Self {
        Self
    }

    pub fn with_config(_config: &AnalysisConfiguration) -> Self {
        Self::new()
    }
}

impl Default for IndexUsageAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer for IndexUsageAnalyzer {
    fn analyze(&self, plan: &ParsedPlan, context: &AnalysisContext) -> AnalysisReport {
        let mut report = AnalysisReport::new("IndexUsageAnalyzer".to_string())
            .with_metadata("version", self.version());

        let mut visitor = IndexUsageVisitor::new();
        PlanTraversal::depth_first(plan, &mut visitor, context);

        // Add all findings to the report
        for finding in visitor.findings {
            report = report.add_finding(finding);
        }

        // Add aggregate metrics
        report = report
            .with_metric("nodes_analyzed", visitor.nodes_analyzed as f64)
            .with_metric("seq_scans", visitor.seq_scans as f64)
            .with_metric("index_scans", visitor.index_scans as f64)
            .with_metric(
                "seq_scans_with_filters",
                visitor.seq_scans_with_filters as f64,
            );

        if visitor.seq_scans + visitor.index_scans > 0 {
            let index_usage_ratio =
                visitor.index_scans as f64 / (visitor.seq_scans + visitor.index_scans) as f64;
            report = report.with_metric("index_usage_ratio", index_usage_ratio);
        }

        report
    }

    fn name(&self) -> &'static str {
        "IndexUsageAnalyzer"
    }

    fn description(&self) -> &'static str {
        "Analyzes index usage patterns and detects missing index opportunities"
    }

    fn version(&self) -> &'static str {
        "1.0.0"
    }
}

/// Visitor implementation for collecting index usage findings
struct IndexUsageVisitor {
    findings: Vec<Finding>,
    nodes_analyzed: usize,
    seq_scans: usize,
    index_scans: usize,
    seq_scans_with_filters: usize,
}

impl IndexUsageVisitor {
    fn new() -> Self {
        Self {
            findings: Vec::new(),
            nodes_analyzed: 0,
            seq_scans: 0,
            index_scans: 0,
            seq_scans_with_filters: 0,
        }
    }

    fn detect_missing_index_opportunity(&mut self, node: &PlanNode, path: &NodePath) {
        // Look for sequential scans with selective filters
        if let NodeType::Scan(ScanType::SeqScan { table }) = &node.node_type
            && let Some(filter) = node.get_property("Filter")
        {
            self.seq_scans_with_filters += 1;

            let estimated_rows = node.cost.estimated_rows;

            // Simple equality filters are excellent index candidates
            if filter.contains('=') && !filter.to_lowercase().contains(" or ") {
                // Look for patterns that suggest good index candidates:
                // 1. Selective filter (returning moderate number of rows)
                // 2. Not too small (< 1000 rows means index overhead might not be worth it)
                // 3. Not the entire table (> 1M rows suggests filter isn't selective)

                if estimated_rows > 1000 && estimated_rows < 1_000_000 {
                    let severity = if estimated_rows > 100_000 {
                        Severity::High
                    } else if estimated_rows > 10_000 {
                        Severity::Medium
                    } else {
                        Severity::Low
                    };

                    let finding = Finding::new(
                        FindingType::MissingIndex,
                        severity,
                        "Potential missing index opportunity".to_string(),
                        format!(
                            "Sequential scan on '{}.{}' with selective filter: '{}'. Estimated {} rows. An index could improve performance.",
                            table.schema.as_deref().unwrap_or("public"),
                            table.name,
                            filter,
                            estimated_rows
                        ),
                        "Consider creating an index on the filtered column(s). For equality filters, a B-tree index is typically appropriate.".to_string(),
                    )
                    .with_node(path.clone())
                    .with_evidence("estimated_rows", estimated_rows as f64)
                    .with_metadata("filter_condition", &filter)
                    .with_metadata("table_name", &table.name);

                    self.findings.push(finding);
                }
            }

            // Function calls in WHERE clause prevent index usage
            let filter_lower = filter.to_lowercase();
            if filter_lower.contains("lower(")
                || filter_lower.contains("upper(")
                || filter_lower.contains("date(")
                || filter_lower.contains("substring(")
                || filter_lower.contains("cast(")
            {
                let finding = Finding::new(
                    FindingType::Custom("FunctionInFilter".to_string()),
                    Severity::Medium,
                    "Function prevents index usage".to_string(),
                    format!(
                        "Filter '{}' applies a function to a column, preventing index usage. Table: {}.{}",
                        filter,
                        table.schema.as_deref().unwrap_or("public"),
                        table.name
                    ),
                    "Consider using expression indexes (CREATE INDEX ... ON table ((LOWER(column)))), or restructure the query to avoid functions in WHERE clause".to_string(),
                )
                .with_node(path.clone())
                .with_metadata("filter_condition", &filter)
                .with_metadata("table_name", &table.name);

                self.findings.push(finding);
            }

            // LIKE with leading wildcard cannot use index
            if filter.contains("LIKE '%") || filter.contains("ILIKE '%") {
                let finding = Finding::new(
                    FindingType::Custom("LeadingWildcardLike".to_string()),
                    Severity::Low,
                    "LIKE with leading wildcard cannot use index".to_string(),
                    format!(
                        "Filter '{}' uses LIKE/ILIKE with leading wildcard, which prevents index usage. Table: {}.{}",
                        filter,
                        table.schema.as_deref().unwrap_or("public"),
                        table.name
                    ),
                    "Consider full-text search (GIN index) or pattern indexes if prefix searches are possible".to_string(),
                )
                .with_node(path.clone())
                .with_metadata("filter_condition", &filter)
                .with_metadata("table_name", &table.name);

                self.findings.push(finding);
            }
        }
    }

    fn analyze_index_scan(&mut self, node: &PlanNode, path: &NodePath) {
        if let NodeType::Scan(ScanType::IndexScan { table, index, .. }) = &node.node_type {
            let estimated_rows = node.cost.estimated_rows;

            // Index scan returning very large number of rows may be inefficient
            // PostgreSQL typically switches to seq scan around 5-10% of table
            if estimated_rows > 50_000 {
                let finding = Finding::new(
                    FindingType::Custom("LargeIndexScan".to_string()),
                    Severity::Low,
                    "Index scan returns many rows".to_string(),
                    format!(
                        "Index scan on '{}.{}' using '{}' returns {} rows. This may be less efficient than a sequential scan.",
                        table.schema.as_deref().unwrap_or("public"),
                        table.name,
                        index.as_ref().map(|i| i.name.as_str()).unwrap_or("unknown"),
                        estimated_rows
                    ),
                    "Review query selectivity. PostgreSQL may choose index scan due to ORDER BY or other factors, but for large result sets, sequential scan is often faster.".to_string(),
                )
                .with_node(path.clone())
                .with_evidence("estimated_rows", estimated_rows as f64)
                .with_metadata("table_name", &table.name)
                .with_metadata(
                    "index_name",
                    index.as_ref().map(|i| i.name.as_str()).unwrap_or("unknown"),
                );

                self.findings.push(finding);
            }
        }
    }

    fn analyze_bitmap_scan(&mut self, node: &PlanNode, path: &NodePath) {
        // Bitmap scans are used when multiple indexes can be combined
        // or when index selectivity is moderate
        if let NodeType::Scan(ScanType::BitmapHeapScan { table, .. }) = &node.node_type {
            let estimated_rows = node.cost.estimated_rows;

            // Very large bitmap scan might benefit from different approach
            if estimated_rows > 100_000 {
                let finding = Finding::new(
                    FindingType::Custom("LargeBitmapScan".to_string()),
                    Severity::Low,
                    "Large bitmap heap scan detected".to_string(),
                    format!(
                        "Bitmap heap scan on '{}.{}' returns {} rows. For very large result sets, consider if query can be made more selective.",
                        table.schema.as_deref().unwrap_or("public"),
                        table.name,
                        estimated_rows
                    ),
                    "Bitmap scans are efficient for moderate selectivity. If returning most of the table, query optimization may be needed.".to_string(),
                )
                .with_node(path.clone())
                .with_evidence("estimated_rows", estimated_rows as f64)
                .with_metadata("table_name", &table.name);

                self.findings.push(finding);
            }
        }
    }
}

impl NodeVisitor for IndexUsageVisitor {
    fn visit_node(&mut self, node: &PlanNode, path: &NodePath, _context: &AnalysisContext) {
        self.nodes_analyzed += 1;

        if let NodeType::Scan(scan_type) = &node.node_type {
            match scan_type {
                ScanType::SeqScan { .. } => {
                    self.seq_scans += 1;
                    self.detect_missing_index_opportunity(node, path);
                }
                ScanType::IndexScan { .. } => {
                    self.index_scans += 1;
                    self.analyze_index_scan(node, path);
                }
                ScanType::BitmapHeapScan { .. } => {
                    self.analyze_bitmap_scan(node, path);
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IndexReference, PlanCost, PlanNode, ScanType, TableReference};

    #[test]
    fn test_missing_index_detection() {
        let analyzer = IndexUsageAnalyzer::new();
        let context = AnalysisContext::new();

        let mut node = PlanNode::new(
            NodeType::Scan(ScanType::SeqScan {
                table: TableReference {
                    schema: Some("public".to_string()),
                    name: "users".to_string(),
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
            "Seq Scan on users".to_string(),
        );

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

    #[test]
    fn test_function_in_filter_detection() {
        let analyzer = IndexUsageAnalyzer::new();
        let context = AnalysisContext::new();

        let mut node = PlanNode::new(
            NodeType::Scan(ScanType::SeqScan {
                table: TableReference {
                    schema: Some("public".to_string()),
                    name: "users".to_string(),
                    alias: None,
                },
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 5000.0,
                estimated_rows: 10000,
                estimated_width: 100,
            },
            "Seq Scan on users".to_string(),
        );

        node.set_property(
            "Filter".to_string(),
            "LOWER(email) = 'test@example.com'".to_string(),
        );

        let plan = ParsedPlan::new(node);
        let report = analyzer.analyze(&plan, &context);

        // Should detect function in filter
        assert!(report.findings.iter().any(
            |f| matches!(f.finding_type, FindingType::Custom(ref s) if s == "FunctionInFilter")
        ));
    }

    #[test]
    fn test_index_scan_no_issues() {
        let analyzer = IndexUsageAnalyzer::new();
        let context = AnalysisContext::new();

        let node = PlanNode::new(
            NodeType::Scan(ScanType::IndexScan {
                table: TableReference {
                    schema: Some("public".to_string()),
                    name: "users".to_string(),
                    alias: None,
                },
                index: Some(IndexReference {
                    name: "idx_user_id".to_string(),
                }),
                backward: false,
                only: false,
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 100.0,
                estimated_rows: 100, // Small result set
                estimated_width: 50,
            },
            "Index Scan using idx_user_id".to_string(),
        );

        let plan = ParsedPlan::new(node);
        let report = analyzer.analyze(&plan, &context);

        // Should not flag efficient index scan
        assert_eq!(report.findings.len(), 0);
    }
}
