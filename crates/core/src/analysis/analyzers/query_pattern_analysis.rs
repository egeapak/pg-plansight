use crate::{ParsedPlan, PlanNode, NodeType};
use super::super::{
    Analyzer, ConfigurableAnalyzer, AnalysisContext, AnalysisReport, Finding,
    FindingType, Severity, NodePath
};
use super::super::consolidated_config::AnalysisConfiguration;
use super::super::traversal::{PlanTraversal, NodeVisitor};

/// Configuration for query pattern analysis
#[derive(Debug, Clone, PartialEq)]
pub struct QueryPatternConfig {
    /// Enable N+1 pattern detection
    pub detect_nplusone: bool,
    /// Enable anti-pattern detection
    pub detect_antipatterns: bool,
    /// Threshold for query complexity score
    pub complexity_threshold: f64,
    /// Maximum acceptable join depth
    pub max_join_depth: usize,
}

impl Default for QueryPatternConfig {
    fn default() -> Self {
        Self {
            detect_nplusone: true,
            detect_antipatterns: true,
            complexity_threshold: 100.0,
            max_join_depth: 5,
        }
    }
}

/// Analyzer for detecting query anti-patterns and problematic patterns
pub struct QueryPatternAnalyzer {
    config: QueryPatternConfig,
}

impl QueryPatternAnalyzer {
    pub fn new() -> Self {
        Self {
            config: QueryPatternConfig::default(),
        }
    }

    pub fn with_config(_config: &AnalysisConfiguration) -> Self {
        Self {
            config: QueryPatternConfig::default(),
        }
    }
}

impl Default for QueryPatternAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer for QueryPatternAnalyzer {
    fn analyze(&self, plan: &ParsedPlan, context: &AnalysisContext) -> AnalysisReport {
        let mut report = AnalysisReport::new("QueryPatternAnalyzer".to_string())
            .with_metadata("version", self.version());

        // Create a visitor to collect pattern-related findings
        let mut visitor = QueryPatternVisitor::new(&self.config);
        PlanTraversal::depth_first(plan, &mut visitor, context);

        // Calculate complexity score before consuming findings
        let complexity_score = visitor.calculate_complexity_score();

        // Add all findings to the report
        for finding in visitor.findings {
            report = report.add_finding(finding);
        }

        // Add aggregate metrics
        report = report
            .with_metric("nodes_analyzed", visitor.nodes_analyzed as f64)
            .with_metric("join_count", visitor.join_count as f64)
            .with_metric("subquery_count", visitor.subquery_count as f64)
            .with_metric("aggregate_count", visitor.aggregate_count as f64)
            .with_metric("complexity_score", complexity_score)
            .with_metric("max_depth", visitor.max_depth as f64);

        // Check overall complexity
        if complexity_score > self.config.complexity_threshold {
            let finding = Finding::new(
                FindingType::Custom("OverlyComplexQuery".to_string()),
                if complexity_score > 200.0 { Severity::High } else { Severity::Medium },
                "Query complexity is very high".to_string(),
                format!(
                    "Query has a complexity score of {:.0}, which exceeds the threshold of {:.0}. Complex queries are harder to optimize and maintain.",
                    complexity_score, self.config.complexity_threshold
                ),
                "Consider breaking down the query into simpler parts, using CTEs for clarity, or creating intermediate tables".to_string(),
            )
            .with_evidence("complexity_score", complexity_score)
            .with_evidence("join_count", visitor.join_count as f64)
            .with_evidence("subquery_count", visitor.subquery_count as f64);

            report = report.add_finding(finding);
        }

        // Check join depth
        if visitor.max_depth > self.config.max_join_depth {
            let finding = Finding::new(
                FindingType::Custom("ExcessiveJoinDepth".to_string()),
                Severity::Medium,
                format!("Query has {} levels of nesting", visitor.max_depth),
                format!(
                    "Deep query nesting ({} levels) can make plans harder to optimize and maintain. Consider flattening the query structure.",
                    visitor.max_depth
                ),
                "Use CTEs or temporary tables to break down complex nested queries".to_string(),
            )
            .with_evidence("max_depth", visitor.max_depth as f64)
            .with_evidence("max_acceptable_depth", self.config.max_join_depth as f64);

            report = report.add_finding(finding);
        }

        report
    }

    fn name(&self) -> &'static str {
        "QueryPatternAnalyzer"
    }

    fn description(&self) -> &'static str {
        "Detects query anti-patterns, complexity issues, and problematic query structures"
    }

    fn version(&self) -> &'static str {
        "1.0.0"
    }
}

impl ConfigurableAnalyzer for QueryPatternAnalyzer {
    type Config = QueryPatternConfig;

    fn configure(&mut self, config: Self::Config) {
        self.config = config;
    }

    fn default_config() -> Self::Config {
        QueryPatternConfig::default()
    }

    fn current_config(&self) -> &Self::Config {
        &self.config
    }
}

/// Visitor implementation for collecting query pattern findings
struct QueryPatternVisitor<'a> {
    config: &'a QueryPatternConfig,
    findings: Vec<Finding>,
    // Metrics
    nodes_analyzed: usize,
    join_count: usize,
    subquery_count: usize,
    aggregate_count: usize,
    max_depth: usize,
    current_depth: usize,
}

impl<'a> QueryPatternVisitor<'a> {
    fn new(config: &'a QueryPatternConfig) -> Self {
        Self {
            config,
            findings: Vec::new(),
            nodes_analyzed: 0,
            join_count: 0,
            subquery_count: 0,
            aggregate_count: 0,
            max_depth: 0,
            current_depth: 0,
        }
    }

    fn calculate_complexity_score(&self) -> f64 {
        // Simple complexity scoring based on various factors
        let mut score = 0.0;

        // Joins add significant complexity
        score += self.join_count as f64 * 10.0;

        // Subqueries add complexity
        score += self.subquery_count as f64 * 15.0;

        // Aggregates add some complexity
        score += self.aggregate_count as f64 * 5.0;

        // Deep nesting adds exponentially more complexity
        score += (self.max_depth as f64).powi(2) * 3.0;

        score
    }

    fn detect_unnecessary_distinct(&mut self, node: &PlanNode, path: &NodePath) {
        // Look for DISTINCT operations that might be unnecessary
        if let Some(operation) = node.get_property("Operation") {
            if operation.to_lowercase().contains("unique") {
                // Check if the data is already unique (e.g., selecting from a primary key)
                if let Some(index_cond) = node.get_property("Index Cond") {
                    if index_cond.contains("=") && !index_cond.contains("AND") {
                        // Simple single-column equality - likely already unique
                        let finding = Finding::new(
                            FindingType::Custom("UnnecessaryDistinct".to_string()),
                            Severity::Low,
                            "Potentially unnecessary DISTINCT operation".to_string(),
                            format!(
                                "DISTINCT operation on {} may be unnecessary if the index condition guarantees uniqueness",
                                node.description()
                            ),
                            "Verify if DISTINCT is needed; removing it can improve performance".to_string(),
                        )
                        .with_node(path.clone())
                        .with_metadata("index_condition", &index_cond);

                        self.findings.push(finding);
                    }
                }
            }
        }
    }

    fn detect_function_in_where(&mut self, node: &PlanNode, path: &NodePath) {
        // Detect functions applied to indexed columns in WHERE clauses (prevents index usage)
        if let Some(filter) = node.get_property("Filter") {
            // Look for common function patterns like LOWER(), UPPER(), DATE(), etc.
            let filter_lower = filter.to_lowercase();
            if filter_lower.contains("lower(") || filter_lower.contains("upper(")
                || filter_lower.contains("date(") || filter_lower.contains("substring(")
            {
                let finding = Finding::new(
                    FindingType::Custom("FunctionOnIndexedColumn".to_string()),
                    Severity::Medium,
                    "Function applied to column in WHERE clause".to_string(),
                    format!(
                        "Filter '{}' applies a function to a column, which prevents index usage",
                        filter
                    ),
                    "Consider using expression indexes, or restructure the query to avoid functions on indexed columns".to_string(),
                )
                .with_node(path.clone())
                .with_metadata("filter_condition", &filter);

                self.findings.push(finding);
            }
        }
    }

    fn detect_select_star(&mut self, node: &PlanNode, path: &NodePath) {
        // Detect SELECT * which may fetch unnecessary columns
        if node.cost.estimated_width > 500 {
            // Wide rows suggest many columns
            let finding = Finding::new(
                FindingType::Custom("WideRowSelection".to_string()),
                Severity::Low,
                "Selecting many columns".to_string(),
                format!(
                    "Row width is {} bytes. Consider selecting only needed columns instead of SELECT *",
                    node.cost.estimated_width
                ),
                "Select only the columns you need to reduce data transfer and improve cache efficiency".to_string(),
            )
            .with_node(path.clone())
            .with_evidence("row_width_bytes", node.cost.estimated_width as f64);

            self.findings.push(finding);
        }
    }
}

impl<'a> NodeVisitor for QueryPatternVisitor<'a> {
    fn visit_node(&mut self, node: &PlanNode, path: &NodePath, _context: &AnalysisContext) {
        self.nodes_analyzed += 1;
        self.current_depth = path.path.len();
        self.max_depth = self.max_depth.max(self.current_depth);

        // Count different node types
        match &node.node_type {
            NodeType::Join(_) => self.join_count += 1,
            NodeType::Aggregate(_) => self.aggregate_count += 1,
            _ => {}
        }

        // Check for subplans
        if node.get_property("Subplan Name").is_some() {
            self.subquery_count += 1;
        }

        // Run pattern detection
        if self.config.detect_antipatterns {
            self.detect_unnecessary_distinct(node, path);
            self.detect_function_in_where(node, path);
            self.detect_select_star(node, path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PlanNode, NodeType, ScanType, JoinType, AggregateType, PlanCost, TableReference};

    #[test]
    fn test_simple_query_low_complexity() {
        let config = AnalysisConfiguration::default();
        let analyzer = QueryPatternAnalyzer::with_config(&config);
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
                max_total_cost: 100.0,
                estimated_rows: 1000,
                estimated_width: 50,
            },
            "Seq Scan on test_table".to_string(),
        );

        let plan = ParsedPlan::new(node);
        let report = analyzer.analyze(&plan, &context);

        // Simple query should have low complexity
        let complexity = report.metrics.get("complexity_score").unwrap();
        assert!(*complexity < 50.0);
    }

    #[test]
    fn test_complex_query_high_complexity() {
        let config = AnalysisConfiguration::default();
        let analyzer = QueryPatternAnalyzer::with_config(&config);
        let context = AnalysisContext::new();

        // Create a complex query with multiple joins
        let left = PlanNode::new(
            NodeType::Scan(ScanType::SeqScan {
                table: TableReference {
                    schema: None,
                    name: "t1".to_string(),
                    alias: None,
                },
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 1000.0,
                estimated_rows: 1000,
                estimated_width: 50,
            },
            "Scan t1".to_string(),
        );

        let right = PlanNode::new(
            NodeType::Scan(ScanType::SeqScan {
                table: TableReference {
                    schema: None,
                    name: "t2".to_string(),
                    alias: None,
                },
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 1000.0,
                estimated_rows: 1000,
                estimated_width: 50,
            },
            "Scan t2".to_string(),
        );

        let mut join1 = PlanNode::new(
            NodeType::Join(JoinType::NestedLoop { inner_unique: false }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 1000.0,
                estimated_rows: 1000,
                estimated_width: 50,
            },
            "Join 1".to_string(),
        );
        join1.add_child(left);
        join1.add_child(right);

        let right2 = PlanNode::new(
            NodeType::Scan(ScanType::SeqScan {
                table: TableReference {
                    schema: None,
                    name: "t3".to_string(),
                    alias: None,
                },
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 1000.0,
                estimated_rows: 1000,
                estimated_width: 50,
            },
            "Scan t3".to_string(),
        );

        let mut join2 = PlanNode::new(
            NodeType::Join(JoinType::NestedLoop { inner_unique: false }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 1000.0,
                estimated_rows: 1000,
                estimated_width: 50,
            },
            "Join 2".to_string(),
        );
        join2.add_child(join1);
        join2.add_child(right2);

        let plan = ParsedPlan::new(join2);
        let report = analyzer.analyze(&plan, &context);

        // Complex query should have higher complexity
        let complexity = report.metrics.get("complexity_score").unwrap();
        assert!(*complexity > 20.0);
        assert_eq!(report.metrics.get("join_count"), Some(&2.0));
    }

    #[test]
    fn test_wide_row_detection() {
        let analyzer = QueryPatternAnalyzer::new();
        let context = AnalysisContext::new();

        let node = PlanNode::new(
            NodeType::Scan(ScanType::SeqScan {
                table: TableReference {
                    schema: None,
                    name: "wide_table".to_string(),
                    alias: None,
                },
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 1000.0,
                estimated_rows: 10000,
                estimated_width: 1000, // Very wide rows
            },
            "Seq Scan on wide_table".to_string(),
        );

        let plan = ParsedPlan::new(node);
        let report = analyzer.analyze(&plan, &context);

        // Should detect wide row selection
        assert!(report.findings.iter().any(|f|
            matches!(f.finding_type, FindingType::Custom(ref s) if s == "WideRowSelection")
        ));
    }
}
