use crate::{ParsedPlan, PlanNode};
use super::super::{
    Analyzer, ConfigurableAnalyzer, AnalysisContext, AnalysisReport, Finding, 
    FindingType, Severity, NodePath
};
use super::super::enhanced_config::EnhancedCostAnalysisConfig;
use super::super::unified_config::{OperationType, UnifiedAnalysis};
use super::super::traversal::{PlanTraversal, NodeVisitor};

/// Analyzer for cost-related performance issues
pub struct CostAnalyzer {
    config: EnhancedCostAnalysisConfig,
}

impl CostAnalyzer {
    pub fn new() -> Self {
        let context = super::super::unified_config::UnifiedAnalysisContext::default();
        Self {
            config: EnhancedCostAnalysisConfig::new(&context),
        }
    }
    
    pub fn with_config(config: EnhancedCostAnalysisConfig) -> Self {
        Self { config }
    }
}

impl Default for CostAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer for CostAnalyzer {
    fn analyze(&self, plan: &ParsedPlan, context: &AnalysisContext) -> AnalysisReport {
        let mut report = AnalysisReport::new("CostAnalyzer".to_string())
            .with_metadata("version", self.version())
            .with_metadata("config_version", "2.0");
        
        // Create a visitor to collect cost-related findings
        let mut visitor = CostAnalysisVisitor::new(&self.config, context);
        PlanTraversal::depth_first(plan, &mut visitor, context);
        
        // Add all findings to the report
        for finding in visitor.findings {
            report = report.add_finding(finding);
        }
        
        // Add aggregate metrics
        report = report
            .with_metric("nodes_analyzed", visitor.nodes_analyzed as f64)
            .with_metric("expensive_operations_found", visitor.expensive_operations as f64)
            .with_metric("high_startup_costs_found", visitor.high_startup_costs as f64)
            .with_metric("max_total_cost_seen", visitor.max_total_cost)
            .with_metric("max_startup_cost_seen", visitor.max_startup_cost)
            .with_metric("total_plan_cost", plan.root.cost.max_total_cost);
        
        report
    }
    
    fn name(&self) -> &'static str {
        "CostAnalyzer"
    }
    
    fn description(&self) -> &'static str {
        "Analyzes query costs and identifies expensive operations"
    }
    
    fn version(&self) -> &'static str {
        "2.0.0"
    }
}

impl ConfigurableAnalyzer for CostAnalyzer {
    type Config = EnhancedCostAnalysisConfig;
    
    fn configure(&mut self, config: Self::Config) {
        self.config = config;
    }
    
    fn default_config() -> Self::Config {
        let context = super::super::unified_config::UnifiedAnalysisContext::default();
        EnhancedCostAnalysisConfig::new(&context)
    }
    
    fn current_config(&self) -> &Self::Config {
        &self.config
    }
}

impl UnifiedAnalysis for CostAnalyzer {
    fn get_operation_type(&self) -> OperationType {
        OperationType::Scan // Generic operation type for cost analysis
    }
}

/// Visitor implementation for collecting cost analysis findings
struct CostAnalysisVisitor<'a> {
    config: &'a EnhancedCostAnalysisConfig,
    context: &'a AnalysisContext,
    findings: Vec<Finding>,
    // Metrics
    nodes_analyzed: usize,
    expensive_operations: usize,
    high_startup_costs: usize,
    max_total_cost: f64,
    max_startup_cost: f64,
}

impl<'a> CostAnalysisVisitor<'a> {
    fn new(config: &'a EnhancedCostAnalysisConfig, context: &'a AnalysisContext) -> Self {
        Self {
            config,
            context,
            findings: Vec::new(),
            nodes_analyzed: 0,
            expensive_operations: 0,
            high_startup_costs: 0,
            max_total_cost: 0.0,
            max_startup_cost: 0.0,
        }
    }
    
    fn analyze_expensive_operation(&mut self, node: &PlanNode, path: &NodePath) {
        if !self.config.enabled_findings.contains(&FindingType::ExpensiveOperation) {
            return;
        }
        
        let total_cost = node.cost.max_total_cost;
        let startup_cost = node.cost.startup_cost;
        
        // Update max cost metrics
        self.max_total_cost = self.max_total_cost.max(total_cost);
        self.max_startup_cost = self.max_startup_cost.max(startup_cost);
        
        // Use unified threshold classification for cost
        let cost_severity = self.config.classify_cost_severity(total_cost);
        
        if matches!(cost_severity, Severity::High | Severity::Critical) {
            self.expensive_operations += 1;
            
            let finding = Finding::new(
                FindingType::ExpensiveOperation,
                cost_severity.clone(),
                format!("Expensive operation detected"),
                format!(
                    "Operation '{}' has high total cost ({:.0}), which may significantly impact query performance.",
                    node.description(), total_cost
                ),
                "Consider query optimization, indexing, or breaking down complex operations into simpler parts".to_string(),
            )
            .with_node(path.clone())
            .with_evidence("total_cost", total_cost)
            .with_evidence("startup_cost", startup_cost)
            .with_evidence("cost_threshold", match cost_severity {
                Severity::Critical => self.config.thresholds.cost.extreme,
                Severity::High => self.config.thresholds.cost.high,
                _ => 0.0,
            })
            .with_metadata("operation_type", &node.description())
            .with_metadata("estimated_rows", &node.cost.estimated_rows.to_string());
            
            self.findings.push(finding);
        }
    }
    
    fn analyze_startup_cost(&mut self, node: &PlanNode, path: &NodePath) {
        if !self.config.enabled_findings.contains(&FindingType::HighStartupCost) {
            return;
        }
        
        let total_cost = node.cost.max_total_cost;
        let startup_cost = node.cost.startup_cost;
        
        // Skip if total cost is very low (startup cost ratio less meaningful)
        if total_cost < 100.0 {
            return;
        }
        
        let startup_ratio = startup_cost / total_cost;
        
        if startup_ratio >= self.config.startup_ratio_threshold {
            self.high_startup_costs += 1;
            
            let severity = if startup_ratio >= 0.8 {
                Severity::High
            } else if startup_ratio >= 0.6 {
                Severity::Medium
            } else {
                Severity::Low
            };
            
            let finding = Finding::new(
                FindingType::HighStartupCost,
                severity,
                "High startup cost detected".to_string(),
                format!(
                    "Operation '{}' has high startup cost ({:.0}) relative to total cost ({:.0}) - {:.1}% of total cost.",
                    node.description(), startup_cost, total_cost, startup_ratio * 100.0
                ),
                "Review initialization costs, consider materialized views, or optimize sort/join operations".to_string(),
            )
            .with_node(path.clone())
            .with_evidence("startup_cost", startup_cost)
            .with_evidence("total_cost", total_cost)
            .with_evidence("startup_ratio", startup_ratio)
            .with_evidence("startup_threshold", self.config.startup_ratio_threshold)
            .with_metadata("operation_type", &node.description());
            
            self.findings.push(finding);
        }
    }
    
    fn analyze_duration_correlation(&mut self, node: &PlanNode, path: &NodePath) {
        if !self.config.duration_correlation_enabled {
            return;
        }
        
        // Only analyze if we have actual timing data and query duration context
        if let (Some(actuals), Some(query_duration_ms)) = (&node.actuals, self.context.query_duration_ms) {
            if let Some(actual_time_ms) = actuals.actual_time_ms {
                let cost = node.cost.max_total_cost;
                
                // Check if duration is significantly higher than cost would suggest
                // This is a heuristic - PostgreSQL cost units roughly correlate to milliseconds
                let expected_duration_rough = cost / 100.0; // Very rough heuristic
                
                if actual_time_ms > expected_duration_rough * 5.0 && actual_time_ms > 100.0 {
                    let duration_severity = self.config.classify_duration_severity(actual_time_ms);
                    
                    if matches!(duration_severity, Severity::High | Severity::Critical) {
                        let finding = Finding::new(
                            FindingType::HighCostVariability,
                            duration_severity,
                            "Cost vs duration mismatch".to_string(),
                            format!(
                                "Operation '{}' took {:.0}ms but has relatively low cost ({:.0}). This suggests system resource constraints or outdated statistics.",
                                node.description(), actual_time_ms, cost
                            ),
                            "Check for I/O bottlenecks, memory pressure, or run ANALYZE on affected tables".to_string(),
                        )
                        .with_node(path.clone())
                        .with_evidence("actual_duration_ms", actual_time_ms)
                        .with_evidence("estimated_cost", cost)
                        .with_evidence("duration_cost_ratio", actual_time_ms / cost.max(1.0))
                        .with_evidence("query_total_duration_ms", query_duration_ms)
                        .with_metadata("operation_type", &node.description());
                        
                        self.findings.push(finding);
                    }
                }
            }
        }
    }
}

impl<'a> NodeVisitor for CostAnalysisVisitor<'a> {
    fn visit_node(&mut self, node: &PlanNode, path: &NodePath, _context: &AnalysisContext) {
        self.nodes_analyzed += 1;
        
        // Analyze different cost-related aspects
        self.analyze_expensive_operation(node, path);
        self.analyze_startup_cost(node, path);
        self.analyze_duration_correlation(node, path);
    }
}