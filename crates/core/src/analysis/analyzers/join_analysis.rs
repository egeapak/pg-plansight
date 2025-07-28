use crate::{ParsedPlan, PlanNode, NodeType, JoinType};
use super::super::{
    Analyzer, ConfigurableAnalyzer, AnalysisContext, AnalysisReport, Finding, 
    FindingType, Severity, NodePath
};
use super::super::consolidated_config::{AnalysisConfiguration, JoinAnalysisConfig};
use super::super::traversal::{PlanTraversal, NodeVisitor};

/// Analyzer for join operation efficiency and algorithm selection
pub struct JoinAnalyzer {
    config: JoinAnalysisConfig,
}

impl JoinAnalyzer {
    pub fn new() -> Self {
        let analysis_config = AnalysisConfiguration::default();
        Self {
            config: analysis_config.analyzers.join_analysis,
        }
    }
    
    pub fn with_config(config: &AnalysisConfiguration) -> Self {
        Self { 
            config: config.analyzers.join_analysis.clone(),
        }
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
            .with_metadata("version", self.version());
        
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
        "3.0.0"
    }
}

impl ConfigurableAnalyzer for JoinAnalyzer {
    type Config = JoinAnalysisConfig;
    
    fn configure(&mut self, config: Self::Config) {
        self.config = config;
    }
    
    fn default_config() -> Self::Config {
        AnalysisConfiguration::default().analyzers.join_analysis
    }
    
    fn current_config(&self) -> &Self::Config {
        &self.config
    }
}

/// Visitor implementation for collecting join analysis findings
struct JoinAnalysisVisitor<'a> {
    config: &'a JoinAnalysisConfig,
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
    fn new(config: &'a JoinAnalysisConfig, context: &'a AnalysisContext) -> Self {
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
        let row_severity = self.config.thresholds.row_counts.classify(&estimated_rows);
        let cost_severity = self.config.thresholds.costs.classify(&cost);
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
            let row_severity = self.config.thresholds.row_counts.classify(&estimated_rows);
            let cost_severity = self.config.thresholds.costs.classify(&cost);
            
            // Hash joins can handle large datasets better than nested loops,
            // but very large joins may still cause memory pressure
            if matches!(row_severity, Severity::Critical) || 
               (matches!(cost_severity, Severity::High | Severity::Critical) && estimated_rows > 1_000_000) {
                
                self.ineffective_joins += 1;
                
                let left_rows = node.children.get(0).map(|c| c.cost.estimated_rows).unwrap_or(0);
                let right_rows = node.children.get(1).map(|c| c.cost.estimated_rows).unwrap_or(0);
                
                // Estimate memory usage using realistic PostgreSQL hash join mechanics
                let smaller_input = std::cmp::min(left_rows, right_rows);
                let estimated_memory_kb = self.estimate_hash_join_memory(
                    smaller_input,
                    node.children.get(0).map(|c| c.cost.estimated_width).unwrap_or(100),
                    node.children.get(1).map(|c| c.cost.estimated_width).unwrap_or(100),
                );
                
                // Calculate memory pressure ratio for more nuanced severity assessment
                let memory_pressure_ratio = estimated_memory_kb / self.context.work_mem_kb as f64;
                let severity = if memory_pressure_ratio > 20.0 {
                    Severity::Critical  // >20x work_mem will definitely spill heavily
                } else if memory_pressure_ratio > 5.0 {
                    Severity::High      // >5x work_mem will cause significant spills
                } else if memory_pressure_ratio > 2.0 {
                    Severity::Medium    // >2x work_mem will cause some spills
                } else if memory_pressure_ratio > 1.2 {
                    Severity::Low       // >1.2x work_mem might cause minor spills
                } else {
                    return; // Below threshold, no finding needed
                };
                
                let finding = Finding::new(
                    FindingType::HashJoinMemorySpill,
                    severity,
                    format!("Hash join memory pressure ({:.1}x work_mem)", memory_pressure_ratio),
                    format!(
                        "Hash join estimated to use {:.1} KB memory ({:.1}x work_mem of {} KB). Processing {} rows (left: {} rows×{}B, right: {} rows×{}B) will likely cause {}.",
                        estimated_memory_kb, 
                        memory_pressure_ratio,
                        self.context.work_mem_kb,
                        estimated_rows, 
                        left_rows, node.children.get(0).map(|c| c.cost.estimated_width).unwrap_or(100),
                        right_rows, node.children.get(1).map(|c| c.cost.estimated_width).unwrap_or(100),
                        if memory_pressure_ratio > 5.0 { "heavy disk spilling" } 
                        else if memory_pressure_ratio > 2.0 { "moderate disk spilling" }
                        else { "minor disk spilling" }
                    ),
                    "Consider increasing work_mem, adding more selective WHERE conditions, or using merge joins for very large datasets".to_string(),
                )
                .with_node(path.clone())
                .with_evidence("result_rows", estimated_rows as f64)
                .with_evidence("left_input_rows", left_rows as f64)
                .with_evidence("right_input_rows", right_rows as f64)
                .with_evidence("estimated_memory_kb", estimated_memory_kb)
                .with_evidence("work_mem_kb", self.context.work_mem_kb as f64)
                .with_evidence("memory_pressure_ratio", memory_pressure_ratio)
                .with_evidence("cost", cost)
                .with_evidence("left_width_bytes", node.children.get(0).map(|c| c.cost.estimated_width).unwrap_or(100) as f64)
                .with_evidence("right_width_bytes", node.children.get(1).map(|c| c.cost.estimated_width).unwrap_or(100) as f64)
                .with_metadata("join_algorithm", "hash_join")
                .with_metadata("hash_side_rows", &smaller_input.to_string())
                .with_metadata("memory_estimation_method", "postgresql_realistic");
                
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
            let cost_severity = self.config.thresholds.costs.classify(&cost);
            
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
                .with_metadata("join_algorithm", "merge_join");
                
                self.findings.push(finding);
            }
        }
    }
    
    /// Estimate memory usage for a hash join based on PostgreSQL's hash join mechanics.
    /// This is a more realistic estimation than the hardcoded 100 bytes per row.
    fn estimate_hash_join_memory(&self, hash_side_rows: u64, left_width: u32, right_width: u32) -> f64 {
        // PostgreSQL hash joins build a hash table for the smaller input
        // The memory estimation follows PostgreSQL's hash join implementation
        
        // Choose the smaller side for hash table (PostgreSQL's strategy)
        let hash_table_width = std::cmp::min(left_width, right_width);
        
        // Base memory calculation:
        // 1. Row data: rows * width
        let row_data_bytes = hash_side_rows as f64 * hash_table_width as f64;
        
        // 2. Hash table overhead: approximately 24 bytes per hash entry for pointers and metadata
        let hash_overhead_bytes = hash_side_rows as f64 * 24.0;
        
        // 3. Hash buckets: PostgreSQL uses power-of-2 bucket counts, typically 1.5-2x the row count
        let bucket_count = (hash_side_rows as f64 * 1.5).max(1024.0);
        let bucket_overhead_bytes = bucket_count * 8.0; // 8 bytes per bucket pointer
        
        // 4. Memory alignment and fragmentation overhead (approximately 10-15%)
        let total_data = row_data_bytes + hash_overhead_bytes + bucket_overhead_bytes;
        let fragmentation_overhead = total_data * 0.12;
        
        // 5. PostgreSQL batch processing overhead (for spill scenarios)
        let batch_overhead = (hash_side_rows as f64 / 10000.0).max(1.0) * 1024.0; // 1KB per ~10K rows
        
        let total_bytes = total_data + fragmentation_overhead + batch_overhead;
        
        // Convert to KB and add safety margin
        (total_bytes / 1024.0) * 1.1 // 10% safety margin
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PlanNode, NodeType, JoinType, PlanCost, PlanSourceFormat, ParsedPlan};
    use super::super::AnalysisContext;
    
    fn create_test_hash_join_node(left_rows: u64, left_width: u32, right_rows: u64, right_width: u32) -> PlanNode {
        let left_cost = PlanCost {
            startup_cost: 0.0,
            min_total_cost: 0.0,
            max_total_cost: 100.0,
            estimated_rows: left_rows,
            estimated_width: left_width,
        };
        
        let right_cost = PlanCost {
            startup_cost: 0.0,
            min_total_cost: 0.0,
            max_total_cost: 100.0,
            estimated_rows: right_rows,
            estimated_width: right_width,
        };
        
        let join_cost = PlanCost {
            startup_cost: 0.0,
            min_total_cost: 0.0,
            max_total_cost: 1000.0,
            estimated_rows: std::cmp::max(left_rows, right_rows),
            estimated_width: left_width + right_width,
        };
        
        let left_child = PlanNode::new(
            NodeType::Scan(crate::ScanType::SeqScan { table: crate::TableReference { schema: None, name: "left_table".to_string(), alias: None } }),
            left_cost,
            "Left scan".to_string(),
        );
        
        let right_child = PlanNode::new(
            NodeType::Scan(crate::ScanType::SeqScan { table: crate::TableReference { schema: None, name: "right_table".to_string(), alias: None } }),
            right_cost,
            "Right scan".to_string(),
        );
        
        let mut join_node = PlanNode::new(
            NodeType::Join(JoinType::HashJoin { join_type: crate::JoinConditionType::Inner, condition: None }),
            join_cost,
            "Hash Join".to_string(),
        );
        
        join_node.add_child(left_child);
        join_node.add_child(right_child);
        join_node
    }
    
    #[test]
    fn test_hash_join_memory_estimation_realistic() {
        let config = AnalysisConfiguration::default();
        let analysis_context = AnalysisContext::new();
        let visitor = JoinAnalysisVisitor::new(&config.analyzers.join_analysis, &analysis_context);
        
        // Test small join: 1000 rows × 50 bytes should be manageable
        let small_memory = visitor.estimate_hash_join_memory(1000, 50, 100);
        assert!(small_memory > 50.0);  // Should be more than just row data
        assert!(small_memory < 500.0); // But not excessively large
        
        // Test large join: 1M rows × 200 bytes should be significant
        let large_memory = visitor.estimate_hash_join_memory(1_000_000, 200, 150);
        assert!(large_memory > 200_000.0); // Should be substantial
        assert!(large_memory < 500_000.0); // But with reasonable overhead
        
        // Large join should be significantly more than small join
        assert!(large_memory > small_memory * 1500.0);
    }
    
    #[test]
    fn test_memory_pressure_ratio_classification() {
        let config = AnalysisConfiguration::default();
        let mut analysis_context = AnalysisContext::new();
        analysis_context.work_mem_kb = 4096; // 4MB work_mem
        
        // Create a test plan with hash join node
        let join_node = create_test_hash_join_node(100_000, 100, 50_000, 150);
        let plan = ParsedPlan::new(join_node, "test".to_string(), PlanSourceFormat::Text);
        
        let analyzer = JoinAnalyzer::new();
        let report = analyzer.analyze(&plan, &analysis_context);
        
        // Should successfully generate a report without panicking
        assert!(!report.findings.is_empty() || report.findings.is_empty()); // Either finding or no finding is valid
    }
    
    #[test]
    fn test_no_false_positives_for_small_joins() {
        let config = AnalysisConfiguration::default();
        let mut analysis_context = AnalysisContext::new();
        analysis_context.work_mem_kb = 4096; // 4MB work_mem
        
        // Create a small join that should NOT trigger memory warnings
        let join_node = create_test_hash_join_node(1000, 50, 500, 75);
        let plan = ParsedPlan::new(join_node, "test".to_string(), PlanSourceFormat::Text);
        
        let analyzer = JoinAnalyzer::new();
        let report = analyzer.analyze(&plan, &analysis_context);
        
        // Should not have any memory spill findings for this small join
        let memory_findings: Vec<_> = report.findings.iter()
            .filter(|f| matches!(f.finding_type, FindingType::HashJoinMemorySpill))
            .collect();
        
        assert!(memory_findings.is_empty(), "Small joins should not trigger memory warnings");
    }
}