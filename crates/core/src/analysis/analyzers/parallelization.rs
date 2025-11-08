use super::super::consolidated_config::{AnalysisConfiguration, ParallelizationConfig};
use super::super::traversal::{NodeVisitor, PlanTraversal};
use super::super::{
    AnalysisContext, AnalysisReport, Analyzer, ConfigurableAnalyzer, Finding, FindingType,
    NodePath, Severity,
};
use crate::{JoinType, NodeType, ParsedPlan, PlanNode};

/// Analyzer for parallelization efficiency and opportunities
pub struct ParallelizationAnalyzer {
    config: ParallelizationConfig,
}

impl ParallelizationAnalyzer {
    pub fn new() -> Self {
        let analysis_config = AnalysisConfiguration::default();
        Self {
            config: analysis_config.analyzers.parallelization,
        }
    }

    pub fn with_config(config: &AnalysisConfiguration) -> Self {
        Self {
            config: config.analyzers.parallelization.clone(),
        }
    }
}

impl Default for ParallelizationAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer for ParallelizationAnalyzer {
    fn analyze(&self, plan: &ParsedPlan, context: &AnalysisContext) -> AnalysisReport {
        let mut report = AnalysisReport::new("ParallelizationAnalyzer".to_string())
            .with_metadata("version", self.version())
            .with_metadata(
                "max_parallel_workers",
                &context.max_parallel_workers.to_string(),
            );

        // Create a visitor to collect parallelization findings
        let mut visitor = ParallelizationVisitor::new(&self.config, context);
        PlanTraversal::depth_first(plan, &mut visitor, context);

        // Add all findings to the report
        for finding in visitor.findings {
            report = report.add_finding(finding);
        }

        // Add aggregate metrics
        report = report
            .with_metric("nodes_analyzed", visitor.nodes_analyzed as f64)
            .with_metric("parallel_operations", visitor.parallel_operations as f64)
            .with_metric(
                "sequential_operations",
                visitor.sequential_operations as f64,
            )
            .with_metric(
                "missed_parallelization_opportunities",
                visitor.missed_opportunities as f64,
            )
            .with_metric(
                "inefficient_parallelism",
                visitor.inefficient_parallelism as f64,
            );

        report
    }

    fn name(&self) -> &'static str {
        "ParallelizationAnalyzer"
    }

    fn description(&self) -> &'static str {
        "Analyzes parallel execution efficiency and identifies parallelization opportunities"
    }

    fn version(&self) -> &'static str {
        "3.0.0"
    }
}

impl ConfigurableAnalyzer for ParallelizationAnalyzer {
    type Config = ParallelizationConfig;

    fn configure(&mut self, config: Self::Config) {
        self.config = config;
    }

    fn default_config() -> Self::Config {
        AnalysisConfiguration::default().analyzers.parallelization
    }

    fn current_config(&self) -> &Self::Config {
        &self.config
    }
}

/// Visitor implementation for collecting parallelization findings
struct ParallelizationVisitor<'a> {
    config: &'a ParallelizationConfig,
    context: &'a AnalysisContext,
    findings: Vec<Finding>,
    // Metrics
    nodes_analyzed: usize,
    parallel_operations: usize,
    sequential_operations: usize,
    missed_opportunities: usize,
    inefficient_parallelism: usize,
}

impl<'a> ParallelizationVisitor<'a> {
    fn new(config: &'a ParallelizationConfig, context: &'a AnalysisContext) -> Self {
        Self {
            config,
            context,
            findings: Vec::new(),
            nodes_analyzed: 0,
            parallel_operations: 0,
            sequential_operations: 0,
            missed_opportunities: 0,
            inefficient_parallelism: 0,
        }
    }

    fn analyze_parallelization_opportunity(&mut self, node: &PlanNode, path: &NodePath) {
        if !self
            .config
            .enabled_findings
            .contains(&FindingType::MissedParallelization)
        {
            return;
        }

        let estimated_rows = node.cost.estimated_rows;
        let is_parallel = self.is_parallel_operation(node);

        if is_parallel {
            self.parallel_operations += 1;
            self.analyze_parallel_efficiency(node, path);
        } else {
            self.sequential_operations += 1;

            // Check if this operation could benefit from parallelization
            let row_severity = self.config.thresholds.row_counts.classify(&estimated_rows);
            let cost_severity = self
                .config
                .thresholds
                .costs
                .classify(&node.cost.max_total_cost);

            let severity = std::cmp::max(row_severity, cost_severity);

            if matches!(severity, Severity::High | Severity::Critical)
                && self.could_be_parallelized(node)
            {
                self.missed_opportunities += 1;

                let finding = Finding::new(
                    FindingType::MissedParallelization,
                    severity,
                    "Operation could benefit from parallelization".to_string(),
                    format!(
                        "Operation {} processing {} rows (cost: {:.0}) is running sequentially but could potentially be parallelized",
                        node.description(), estimated_rows, node.cost.max_total_cost
                    ),
                    "Consider increasing max_parallel_workers_per_gather, enabling parallel operations, or checking if query meets parallelization requirements".to_string(),
                )
                .with_node(path.clone())
                .with_evidence("estimated_rows", estimated_rows as f64)
                .with_evidence("cost", node.cost.max_total_cost)
                .with_evidence("max_parallel_workers", self.context.max_parallel_workers as f64)
                .with_metadata("operation_type", &node.description())
                .with_metadata("is_parallel", "false");

                self.findings.push(finding);
            }
        }
    }

    fn analyze_parallel_efficiency(&mut self, node: &PlanNode, path: &NodePath) {
        if !self
            .config
            .enabled_findings
            .contains(&FindingType::InefficientParallelism)
        {
            return;
        }

        // Look for signs of inefficient parallelism
        let workers_launched = self.get_workers_launched(node);
        let workers_planned = self.get_workers_planned(node);

        if workers_launched > 0 && workers_planned > 0 {
            let efficiency_ratio = workers_launched as f64 / workers_planned as f64;

            // If significantly fewer workers were launched than planned, it might be inefficient
            if efficiency_ratio < 0.5 {
                self.inefficient_parallelism += 1;

                let finding = Finding::new(
                    FindingType::InefficientParallelism,
                    Severity::Medium,
                    "Inefficient parallel execution detected".to_string(),
                    format!(
                        "Parallel operation planned for {} workers but only launched {} workers (efficiency: {:.1}%)",
                        workers_planned, workers_launched, efficiency_ratio * 100.0
                    ),
                    "Check parallel query configuration, system resources, or consider if the operation is suitable for parallelization".to_string(),
                )
                .with_node(path.clone())
                .with_evidence("workers_planned", workers_planned as f64)
                .with_evidence("workers_launched", workers_launched as f64)
                .with_evidence("efficiency_ratio", efficiency_ratio)
                .with_metadata("operation_type", &node.description());

                self.findings.push(finding);
            }
        }
    }

    fn is_parallel_operation(&self, node: &PlanNode) -> bool {
        // Check node properties for parallel indicators
        node.get_property("Workers Planned").is_some()
            || node.get_property("Workers Launched").is_some()
            || node.description().contains("Parallel")
            || node.description().contains("Gather")
    }

    fn could_be_parallelized(&self, node: &PlanNode) -> bool {
        // Operations that can typically be parallelized in PostgreSQL
        matches!(
            &node.node_type,
            NodeType::Scan(_)
                | NodeType::Join(JoinType::HashJoin { .. })
                | NodeType::Join(JoinType::MergeJoin { .. })
                | NodeType::Aggregate(_)
        )
    }

    fn get_workers_planned(&self, node: &PlanNode) -> usize {
        node.get_property("Workers Planned")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0)
    }

    fn get_workers_launched(&self, node: &PlanNode) -> usize {
        node.get_property("Workers Launched")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0)
    }
}

impl<'a> NodeVisitor for ParallelizationVisitor<'a> {
    fn visit_node(&mut self, node: &PlanNode, path: &NodePath, _context: &AnalysisContext) {
        self.nodes_analyzed += 1;
        self.analyze_parallelization_opportunity(node, path);
    }
}
