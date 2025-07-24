use crate::{ParsedPlan, PlanNode, NodeType, JoinType, AggregateType, UtilityType};
use super::super::{
    Analyzer, ConfigurableAnalyzer, AnalysisContext, AnalysisReport, Finding, 
    FindingType, Severity, NodePath
};
use super::super::enhanced_config::EnhancedMemoryAnalysisConfig;
use super::super::unified_config::{OperationType, UnifiedAnalysis};
use super::super::traversal::{PlanTraversal, NodeVisitor};

/// Analyzer for memory usage and spill detection
pub struct MemoryAnalyzer {
    config: EnhancedMemoryAnalysisConfig,
}

impl MemoryAnalyzer {
    pub fn new() -> Self {
        let context = super::super::unified_config::UnifiedAnalysisContext::default();
        Self {
            config: EnhancedMemoryAnalysisConfig::new(&context),
        }
    }
    
    pub fn with_config(config: EnhancedMemoryAnalysisConfig) -> Self {
        Self { config }
    }
}

impl Default for MemoryAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer for MemoryAnalyzer {
    fn analyze(&self, plan: &ParsedPlan, context: &AnalysisContext) -> AnalysisReport {
        let mut report = AnalysisReport::new("MemoryAnalyzer".to_string())
            .with_metadata("version", self.version())
            .with_metadata("config_version", "2.0")
            .with_metadata("work_mem_kb", &context.work_mem_kb.to_string());
        
        // Create a visitor to collect memory-related findings
        let mut visitor = MemoryAnalysisVisitor::new(&self.config, context);
        PlanTraversal::depth_first(plan, &mut visitor, context);
        
        // Add all findings to the report
        for finding in visitor.findings {
            report = report.add_finding(finding);
        }
        
        // Add aggregate metrics
        report = report
            .with_metric("nodes_analyzed", visitor.nodes_analyzed as f64)
            .with_metric("sort_operations_found", visitor.sort_operations as f64)
            .with_metric("hash_operations_found", visitor.hash_operations as f64)
            .with_metric("aggregate_operations_found", visitor.aggregate_operations as f64)
            .with_metric("potential_spills_detected", visitor.potential_spills as f64)
            .with_metric("large_memory_operations", visitor.large_memory_ops as f64)
            .with_metric("estimated_peak_memory_kb", visitor.estimated_peak_memory_kb);
        
        report
    }
    
    fn name(&self) -> &'static str {
        "MemoryAnalyzer"
    }
    
    fn description(&self) -> &'static str {
        "Analyzes memory usage patterns and identifies potential spill operations"
    }
    
    fn version(&self) -> &'static str {
        "2.0.0"
    }
}

impl ConfigurableAnalyzer for MemoryAnalyzer {
    type Config = EnhancedMemoryAnalysisConfig;
    
    fn configure(&mut self, config: Self::Config) {
        self.config = config;
    }
    
    fn default_config() -> Self::Config {
        let context = super::super::unified_config::UnifiedAnalysisContext::default();
        EnhancedMemoryAnalysisConfig::new(&context)
    }
    
    fn current_config(&self) -> &Self::Config {
        &self.config
    }
}

impl UnifiedAnalysis for MemoryAnalyzer {
    fn get_operation_type(&self) -> OperationType {
        OperationType::Memory
    }
}

/// Visitor implementation for collecting memory analysis findings
struct MemoryAnalysisVisitor<'a> {
    config: &'a EnhancedMemoryAnalysisConfig,
    context: &'a AnalysisContext,
    findings: Vec<Finding>,
    // Metrics
    nodes_analyzed: usize,
    sort_operations: usize,
    hash_operations: usize,
    aggregate_operations: usize,
    potential_spills: usize,
    large_memory_ops: usize,
    estimated_peak_memory_kb: f64,
}

impl<'a> MemoryAnalysisVisitor<'a> {
    fn new(config: &'a EnhancedMemoryAnalysisConfig, context: &'a AnalysisContext) -> Self {
        Self {
            config,
            context,
            findings: Vec::new(),
            nodes_analyzed: 0,
            sort_operations: 0,
            hash_operations: 0,
            aggregate_operations: 0,
            potential_spills: 0,
            large_memory_ops: 0,
            estimated_peak_memory_kb: 0.0,
        }
    }
    
    fn estimate_memory_usage(&self, node: &PlanNode) -> f64 {
        // Rough heuristic for memory usage estimation based on operation type and row count
        let estimated_rows = node.cost.estimated_rows as f64;
        let estimated_width = node.cost.estimated_width as f64;
        
        match &node.node_type {
            NodeType::Utility(UtilityType::Sort { .. }) => {
                // Sort needs to hold all data in memory
                estimated_rows * estimated_width / 1024.0 // Convert to KB
            },
            NodeType::Join(JoinType::HashJoin { .. }) => {
                // Hash join needs to build hash table for smaller relation
                let left_rows = node.children.get(0).map(|c| c.cost.estimated_rows as f64).unwrap_or(0.0);
                let right_rows = node.children.get(1).map(|c| c.cost.estimated_rows as f64).unwrap_or(0.0);
                let smaller_relation = left_rows.min(right_rows);
                smaller_relation * estimated_width / 1024.0
            },
            NodeType::Aggregate(_) => {
                // Aggregate may need to hold group keys and partial results
                (estimated_rows * estimated_width * 0.5) / 1024.0 // Rough estimate
            },
            _ => 0.0, // Other operations don't typically use significant memory
        }
    }
    
    fn analyze_sort_operation(&mut self, node: &PlanNode, path: &NodePath) {
        if !self.config.enabled_findings.contains(&FindingType::LargeSort) {
            return;
        }
        
        self.sort_operations += 1;
        let estimated_rows = node.cost.estimated_rows;
        let estimated_memory_kb = self.estimate_memory_usage(node);
        
        self.estimated_peak_memory_kb = self.estimated_peak_memory_kb.max(estimated_memory_kb);
        
        // Use unified threshold classification for sort operations
        let row_severity = self.config.sort_thresholds.row_count.classify_severity(estimated_rows);
        
        // Check for potential memory spill
        let memory_spill_ratio = estimated_memory_kb / self.context.work_mem_kb as f64;
        let spill_severity = self.config.classify_memory_spill_severity(memory_spill_ratio);
        
        let severity = std::cmp::max(row_severity, spill_severity);
        
        if matches!(severity, Severity::Medium | Severity::High | Severity::Critical) {
            self.large_memory_ops += 1;
            
            if memory_spill_ratio > 1.0 {
                self.potential_spills += 1;
            }
            
            let finding_type = if memory_spill_ratio > 1.0 {
                FindingType::MemorySpill
            } else {
                FindingType::LargeSort
            };
            
            let title = if memory_spill_ratio > 1.0 {
                "Sort operation likely to spill to disk".to_string()
            } else {
                "Large sort operation detected".to_string()
            };
            
            let description = if memory_spill_ratio > 1.0 {
                format!(
                    "Sort operation processing {} rows (~{:.0} KB) will likely exceed work_mem ({} KB) by {:.1}x, causing disk spills.",
                    estimated_rows, estimated_memory_kb, self.context.work_mem_kb, memory_spill_ratio
                )
            } else {
                format!(
                    "Sort operation processing {} rows (~{:.0} KB memory usage). Monitor for performance impact.",
                    estimated_rows, estimated_memory_kb
                )
            };
            
            let suggestion = if memory_spill_ratio > 1.0 {
                "Consider increasing work_mem, adding ORDER BY to index scans, or reducing the dataset with WHERE clauses".to_string()
            } else {
                "Monitor sort performance and consider optimization if this query runs frequently".to_string()
            };
            
            let finding = Finding::new(
                finding_type,
                severity,
                title,
                description,
                suggestion,
            )
            .with_node(path.clone())
            .with_evidence("estimated_rows", estimated_rows as f64)
            .with_evidence("estimated_memory_kb", estimated_memory_kb)
            .with_evidence("work_mem_kb", self.context.work_mem_kb as f64)
            .with_evidence("memory_spill_ratio", memory_spill_ratio)
            .with_evidence("cost", node.cost.max_total_cost)
            .with_metadata("operation_type", "sort")
            .with_metadata("will_spill", &(memory_spill_ratio > 1.0).to_string());
            
            self.findings.push(finding);
        }
    }
    
    fn analyze_hash_operation(&mut self, node: &PlanNode, path: &NodePath) {
        self.hash_operations += 1;
        
        // Hash operations are analyzed in join_analysis.rs, but we check memory usage here
        if !self.config.enabled_findings.contains(&FindingType::MemorySpill) {
            return;
        }
        
        let estimated_memory_kb = self.estimate_memory_usage(node);
        self.estimated_peak_memory_kb = self.estimated_peak_memory_kb.max(estimated_memory_kb);
        
        let memory_spill_ratio = estimated_memory_kb / self.context.work_mem_kb as f64;
        let spill_severity = self.config.classify_memory_spill_severity(memory_spill_ratio);
        
        if matches!(spill_severity, Severity::High | Severity::Critical) && memory_spill_ratio > 1.0 {
            self.potential_spills += 1;
            self.large_memory_ops += 1;
            
            let finding = Finding::new(
                FindingType::MemorySpill,
                spill_severity,
                "Hash operation likely to spill to disk".to_string(),
                format!(
                    "Hash operation (~{:.0} KB memory) will likely exceed work_mem ({} KB) by {:.1}x, causing performance degradation.",
                    estimated_memory_kb, self.context.work_mem_kb, memory_spill_ratio
                ),
                "Consider increasing work_mem or adding more selective WHERE conditions to reduce hash table size".to_string(),
            )
            .with_node(path.clone())
            .with_evidence("estimated_memory_kb", estimated_memory_kb)
            .with_evidence("work_mem_kb", self.context.work_mem_kb as f64)
            .with_evidence("memory_spill_ratio", memory_spill_ratio)
            .with_metadata("operation_type", "hash");
            
            self.findings.push(finding);
        }
    }
    
    fn analyze_aggregate_operation(&mut self, node: &PlanNode, path: &NodePath) {
        if !self.config.enabled_findings.contains(&FindingType::LargeAggregation) {
            return;
        }
        
        self.aggregate_operations += 1;
        let estimated_rows = node.cost.estimated_rows;
        let estimated_memory_kb = self.estimate_memory_usage(node);
        
        self.estimated_peak_memory_kb = self.estimated_peak_memory_kb.max(estimated_memory_kb);
        
        // Use unified threshold classification for aggregate operations
        let row_severity = self.config.aggregate_thresholds.row_count.classify_severity(estimated_rows);
        
        if matches!(row_severity, Severity::High | Severity::Critical) {
            self.large_memory_ops += 1;
            
            let memory_spill_ratio = estimated_memory_kb / self.context.work_mem_kb as f64;
            
            let finding = Finding::new(
                FindingType::LargeAggregation,
                row_severity,
                "Large aggregation operation detected".to_string(),
                format!(
                    "Aggregation processing {} rows (~{:.0} KB memory usage). May cause performance issues if many groups are involved.",
                    estimated_rows, estimated_memory_kb
                ),
                "Consider pre-filtering data with WHERE clauses, using partial aggregation, or reviewing GROUP BY cardinality".to_string(),
            )
            .with_node(path.clone())
            .with_evidence("estimated_rows", estimated_rows as f64)
            .with_evidence("estimated_memory_kb", estimated_memory_kb)
            .with_evidence("memory_spill_ratio", memory_spill_ratio)
            .with_evidence("cost", node.cost.max_total_cost)
            .with_metadata("operation_type", "aggregate");
            
            self.findings.push(finding);
        }
    }
}

impl<'a> NodeVisitor for MemoryAnalysisVisitor<'a> {
    fn visit_node(&mut self, node: &PlanNode, path: &NodePath, _context: &AnalysisContext) {
        self.nodes_analyzed += 1;
        
        match &node.node_type {
            NodeType::Utility(UtilityType::Sort { .. }) => {
                self.analyze_sort_operation(node, path);
            },
            NodeType::Join(JoinType::HashJoin { .. }) => {
                self.analyze_hash_operation(node, path);
            },
            NodeType::Aggregate(_) => {
                self.analyze_aggregate_operation(node, path);
            },
            _ => {
                // Other operations don't typically have significant memory implications
            }
        }
    }
}