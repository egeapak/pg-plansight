use crate::{ParsedPlan, PlanNode, NodeType, JoinType};
use super::super::{
    Analyzer, ConfigurableAnalyzer, AnalysisContext, AnalysisReport, Finding, 
    FindingType, Severity, NodePath
};
use super::super::enhanced_config::EnhancedJoinAnalysisConfig;
use super::super::unified_config::{OperationType, UnifiedAnalysis};
use super::super::traversal::{PlanTraversal, NodeVisitor};

/// Analyzer for join operation efficiency and algorithm selection
pub struct JoinAnalyzer {
    config: EnhancedJoinAnalysisConfig,
}

impl JoinAnalyzer {
    pub fn new() -> Self {
        let context = super::super::unified_config::UnifiedAnalysisContext::default();
        Self {
            config: EnhancedJoinAnalysisConfig::new(&context),
        }
    }
    
    pub fn with_config(config: EnhancedJoinAnalysisConfig) -> Self {
        Self { config }
    }
}

impl Default for JoinAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer for JoinAnalyzer {
    fn analyze(&self, plan: &ParsedPlan, context: &AnalysisContext) -> AnalysisReport {
        let mut report = AnalysisReport::new("JoinAnalyzer".to_string())
            .with_metadata("version", self.version())
            .with_metadata("config_version", "2.0");
        
        // Create a visitor to collect join-related findings
        let mut visitor = JoinAnalysisVisitor::new(&self.config, context);
        PlanTraversal::depth_first(plan, &mut visitor, context);
        
        // Add all findings to the report
        for finding in visitor.findings {
            report = report.add_finding(finding);
        }
        
        // Add aggregate metrics
        report = report
            .with_metric("nodes_analyzed", visitor.nodes_analyzed as f64)
            .with_metric("nested_loops_found", visitor.nested_loops as f64)
            .with_metric("hash_joins_found", visitor.hash_joins as f64)
            .with_metric("merge_joins_found", visitor.merge_joins as f64)
            .with_metric("large_joins_detected", visitor.large_joins as f64)
            .with_metric("ineffective_joins_detected", visitor.ineffective_joins as f64)
            .with_metric("max_join_rows", visitor.max_join_rows as f64);
        
        report
    }
    
    fn name(&self) -> &'static str {
        "JoinAnalyzer"
    }
    
    fn description(&self) -> &'static str {
        "Analyzes join operations for algorithm efficiency and memory usage"
    }
    
    fn version(&self) -> &'static str {
        "2.0.0"
    }
}

impl ConfigurableAnalyzer for JoinAnalyzer {
    type Config = EnhancedJoinAnalysisConfig;
    
    fn configure(&mut self, config: Self::Config) {
        self.config = config;
    }
    
    fn default_config() -> Self::Config {
        let context = super::super::unified_config::UnifiedAnalysisContext::default();
        EnhancedJoinAnalysisConfig::new(&context)
    }
    
    fn current_config(&self) -> &Self::Config {
        &self.config
    }
}

impl UnifiedAnalysis for JoinAnalyzer {
    fn get_operation_type(&self) -> OperationType {
        OperationType::Join
    }
}

/// Visitor implementation for collecting join analysis findings
struct JoinAnalysisVisitor<'a> {
    config: &'a EnhancedJoinAnalysisConfig,
    context: &'a AnalysisContext,
    findings: Vec<Finding>,
    // Metrics
    nodes_analyzed: usize,
    nested_loops: usize,
    hash_joins: usize,
    merge_joins: usize,
    large_joins: usize,
    ineffective_joins: usize,
    max_join_rows: u64,
}

impl<'a> JoinAnalysisVisitor<'a> {
    fn new(config: &'a EnhancedJoinAnalysisConfig, context: &'a AnalysisContext) -> Self {
        Self {
            config,
            context,
            findings: Vec::new(),
            nodes_analyzed: 0,
            nested_loops: 0,
            hash_joins: 0,
            merge_joins: 0,
            large_joins: 0,
            ineffective_joins: 0,
            max_join_rows: 0,
        }
    }
    
    fn analyze_nested_loop_join(&mut self, node: &PlanNode, path: &NodePath) {
        if !self.config.enabled_findings.contains(&FindingType::LargeNestedLoop) {
            return;
        }
        
        self.nested_loops += 1;
        let estimated_rows = node.cost.estimated_rows;
        let cost = node.cost.max_total_cost;
        
        self.max_join_rows = self.max_join_rows.max(estimated_rows);
        
        // Use unified threshold classification for nested loop joins
        let row_severity = self.config.nested_loop_thresholds.row_count.classify_severity(estimated_rows);
        let cost_severity = self.config.nested_loop_thresholds.cost.classify_severity(cost);
        let severity = std::cmp::max(row_severity.clone(), cost_severity);
        
        if matches!(severity, Severity::High | Severity::Critical) {
            self.large_joins += 1;
            
            // Check if children exist to analyze join inputs
            let left_rows = node.children.get(0).map(|c| c.cost.estimated_rows).unwrap_or(0);
            let right_rows = node.children.get(1).map(|c| c.cost.estimated_rows).unwrap_or(0);
            
            let finding = Finding::new(
                FindingType::LargeNestedLoop,
                severity,
                "Large nested loop join detected".to_string(),
                format!(
                    "Nested loop join processing {} rows with cost {:.0}. Joining {} left rows with {} right rows.",
                    estimated_rows, cost, left_rows, right_rows
                ),
                "Consider adding indexes on join columns, using hash/merge joins for large datasets, or restructuring the query".to_string(),
            )
            .with_node(path.clone())
            .with_evidence("result_rows", estimated_rows as f64)
            .with_evidence("cost", cost)
            .with_evidence("left_input_rows", left_rows as f64)
            .with_evidence("right_input_rows", right_rows as f64)
            .with_evidence("row_threshold", match row_severity {
                Severity::Critical => self.config.nested_loop_thresholds.row_count.critical as f64,
                Severity::High => self.config.nested_loop_thresholds.row_count.high as f64,
                _ => 0.0,
            })
            .with_metadata("join_algorithm", "nested_loop")
            .with_metadata("has_join_conditions", &node.get_property("Join Filter").is_some().to_string());
            
            self.findings.push(finding);
        }
    }
    
    fn analyze_hash_join(&mut self, node: &PlanNode, path: &NodePath) {
        self.hash_joins += 1;
        
        // Check for potential memory spills in hash joins
        if self.config.enabled_findings.contains(&FindingType::HashJoinMemorySpill) {
            let estimated_rows = node.cost.estimated_rows;
            let cost = node.cost.max_total_cost;
            
            // Use hash join specific thresholds
            let row_severity = self.config.hash_join_thresholds.row_count.classify_severity(estimated_rows);
            let cost_severity = self.config.hash_join_thresholds.cost.classify_severity(cost);
            
            // Hash joins can handle large datasets better than nested loops,
            // but very large joins may still cause memory pressure
            if matches!(row_severity, Severity::Critical) || 
               (matches!(cost_severity, Severity::High | Severity::Critical) && estimated_rows > 1_000_000) {
                
                self.ineffective_joins += 1;
                
                let left_rows = node.children.get(0).map(|c| c.cost.estimated_rows).unwrap_or(0);
                let right_rows = node.children.get(1).map(|c| c.cost.estimated_rows).unwrap_or(0);
                
                // Estimate memory usage (rough heuristic)
                let smaller_input = std::cmp::min(left_rows, right_rows);
                let estimated_memory_kb = (smaller_input as f64) * 100.0; // Very rough estimate
                
                let severity = if estimated_memory_kb > (self.context.work_mem_kb * 10) as f64 {
                    Severity::Critical
                } else if estimated_memory_kb > (self.context.work_mem_kb * 3) as f64 {
                    Severity::High
                } else {
                    Severity::Medium
                };
                
                let finding = Finding::new(
                    FindingType::HashJoinMemorySpill,
                    severity,
                    "Potential hash join memory spill".to_string(),
                    format!(
                        "Hash join processing {} rows (left: {}, right: {}) may exceed work_mem ({} KB) causing disk spills.",
                        estimated_rows, left_rows, right_rows, self.context.work_mem_kb
                    ),
                    "Consider increasing work_mem, adding more selective WHERE conditions, or using merge joins for very large datasets".to_string(),
                )
                .with_node(path.clone())
                .with_evidence("result_rows", estimated_rows as f64)
                .with_evidence("left_input_rows", left_rows as f64)
                .with_evidence("right_input_rows", right_rows as f64)
                .with_evidence("estimated_memory_kb", estimated_memory_kb as f64)
                .with_evidence("work_mem_kb", self.context.work_mem_kb as f64)
                .with_evidence("cost", cost)
                .with_metadata("join_algorithm", "hash_join")
                .with_metadata("smaller_input_rows", &smaller_input.to_string());
                
                self.findings.push(finding);
            }
        }
    }
    
    fn analyze_merge_join(&mut self, node: &PlanNode, path: &NodePath) {
        self.merge_joins += 1;
        
        // Merge joins are generally efficient, but check for potential issues
        if self.config.enabled_findings.contains(&FindingType::IneffectiveJoinAlgorithm) {
            let estimated_rows = node.cost.estimated_rows;
            let cost = node.cost.max_total_cost;
            
            // Use merge join specific thresholds  
            let cost_severity = self.config.merge_join_thresholds.cost.classify_severity(cost);
            
            // Report if merge join has unexpectedly high cost
            if matches!(cost_severity, Severity::High | Severity::Critical) {
                self.ineffective_joins += 1;
                
                let finding = Finding::new(
                    FindingType::IneffectiveJoinAlgorithm,
                    cost_severity.clone(),
                    "High-cost merge join detected".to_string(),
                    format!(
                        "Merge join with cost {:.0} processing {} rows. May indicate sorting overhead or non-optimal join conditions.",
                        cost, estimated_rows
                    ),
                    "Check if input data is already sorted, verify join column indexes, or consider hash joins for unsorted data".to_string(),
                )
                .with_node(path.clone())
                .with_evidence("cost", cost)
                .with_evidence("estimated_rows", estimated_rows as f64)
                .with_evidence("cost_threshold", match cost_severity {
                    Severity::Critical => self.config.merge_join_thresholds.cost.extreme,
                    Severity::High => self.config.merge_join_thresholds.cost.high,
                    _ => 0.0,
                })
                .with_metadata("join_algorithm", "merge_join");
                
                self.findings.push(finding);
            }
        }
    }
}

impl<'a> NodeVisitor for JoinAnalysisVisitor<'a> {
    fn visit_node(&mut self, node: &PlanNode, path: &NodePath, _context: &AnalysisContext) {
        if let NodeType::Join(join_type) = &node.node_type {
            self.nodes_analyzed += 1;
            
            match join_type {
                JoinType::NestedLoop { .. } => {
                    self.analyze_nested_loop_join(node, path);
                },
                JoinType::NestedLoopLeftJoin { .. } => {
                    self.analyze_nested_loop_join(node, path);
                },
                JoinType::HashJoin { .. } => {
                    self.analyze_hash_join(node, path);
                },
                JoinType::MergeJoin { .. } => {
                    self.analyze_merge_join(node, path);
                },
            }
        }
    }
}