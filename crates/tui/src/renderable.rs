//! Renderable trait for clean TUI display formatting
//! 
//! This module provides the `Renderable` trait and implementations for various 
//! analysis types to ensure clean, human-readable display in the TUI instead 
//! of verbose debug representations.

use pg_loganalyze_core::{
    NodeType, ScanType, JoinType, AggregateType, UtilityType, PlanNode,
    analysis::{Finding, FindingType, Severity, PerformanceAssessment},
};
use std::collections::HashMap;

/// Trait for types that can be rendered cleanly in the TUI
pub trait Renderable {
    /// Render this type as a clean, human-readable string
    fn render(&self) -> String;
    
    /// Render with context information if available
    fn render_with_context(&self, _context: &RenderContext) -> String {
        self.render()
    }
}

/// Context information for enhanced rendering
pub struct RenderContext {
    /// Include technical details
    pub include_details: bool,
    /// Maximum length for rendered output
    pub max_length: Option<usize>,
    /// Additional metadata for context
    pub metadata: HashMap<String, String>,
}

impl Default for RenderContext {
    fn default() -> Self {
        Self {
            include_details: false,
            max_length: None,
            metadata: HashMap::new(),
        }
    }
}

impl RenderContext {
    pub fn new() -> Self {
        Self::default()
    }
    
    pub fn with_details(mut self) -> Self {
        self.include_details = true;
        self
    }
    
    pub fn with_max_length(mut self, length: usize) -> Self {
        self.max_length = Some(length);
        self
    }
    
    pub fn with_metadata(mut self, key: String, value: String) -> Self {
        self.metadata.insert(key, value);
        self
    }
}

// Core enum implementations
impl Renderable for NodeType {
    fn render(&self) -> String {
        match self {
            NodeType::Scan(scan_type) => format!("Scan ({})", scan_type.render()),
            NodeType::Join(join_type) => format!("Join ({})", join_type.render()),
            NodeType::Aggregate(agg_type) => format!("Aggregate ({})", agg_type.render()),
            NodeType::Utility(util_type) => format!("Utility ({})", util_type.render()),
            NodeType::Unknown(desc) => format!("Operation ({})", desc),
        }
    }
}

impl Renderable for ScanType {
    fn render(&self) -> String {
        match self {
            ScanType::SeqScan { .. } => "Sequential".to_string(),
            ScanType::IndexScan { .. } => "Index".to_string(),
            ScanType::BitmapHeapScan { .. } => "Bitmap Heap".to_string(),
            ScanType::BitmapIndexScan { .. } => "Bitmap Index".to_string(),
            ScanType::ParallelBitmapHeapScan { .. } => "Parallel Bitmap Heap".to_string(),
        }
    }
}

impl Renderable for JoinType {
    fn render(&self) -> String {
        match self {
            JoinType::NestedLoop { .. } => "Nested Loop".to_string(),
            JoinType::NestedLoopLeftJoin { .. } => "Nested Loop Left".to_string(),
            JoinType::MergeJoin { .. } => "Merge".to_string(),
            JoinType::HashJoin { .. } => "Hash".to_string(),
        }
    }
}

impl Renderable for AggregateType {
    fn render(&self) -> String {
        match self {
            AggregateType::Aggregate { .. } => "Aggregate".to_string(),
            AggregateType::GroupAggregate { .. } => "Group Aggregate".to_string(),
            AggregateType::HashAggregate { .. } => "Hash Aggregate".to_string(),
        }
    }
}

impl Renderable for UtilityType {
    fn render(&self) -> String {
        match self {
            UtilityType::Sort { .. } => "Sort".to_string(),
            UtilityType::Limit { .. } => "Limit".to_string(),
            UtilityType::GatherMerge { .. } => "Gather Merge".to_string(),
            UtilityType::Materialize => "Materialize".to_string(),
            UtilityType::Memoize { .. } => "Memoize".to_string(),
            UtilityType::SubPlan { .. } => "SubPlan".to_string(),
            UtilityType::BitmapAnd => "Bitmap AND".to_string(),
            UtilityType::BitmapOr => "Bitmap OR".to_string(),
        }
    }
}

impl Renderable for FindingType {
    fn render(&self) -> String {
        match self {
            FindingType::ExcessiveRowProcessing => "Excessive Row Processing".to_string(),
            FindingType::RowEstimationError => "Row Estimation Error".to_string(),
            FindingType::CartesianProduct => "Cartesian Product".to_string(),
            FindingType::LargeSequentialScan => "Large Sequential Scan".to_string(),
            FindingType::InefficiientScan => "Inefficient Scan".to_string(),
            FindingType::MissingIndex => "Missing Index".to_string(),
            FindingType::PoorIndexSelectivity => "Poor Index Selectivity".to_string(),
            FindingType::IneffectiveJoinAlgorithm => "Ineffective Join Algorithm".to_string(),
            FindingType::LargeNestedLoop => "Large Nested Loop".to_string(),
            FindingType::HashJoinMemorySpill => "Hash Join Memory Spill".to_string(),
            FindingType::HighStartupCost => "High Startup Cost".to_string(),
            FindingType::ExpensiveOperation => "Expensive Operation".to_string(),
            FindingType::HighCostVariability => "High Cost Variability".to_string(),
            FindingType::MemorySpill => "Memory Spill".to_string(),
            FindingType::LargeSort => "Large Sort Operation".to_string(),
            FindingType::LargeAggregation => "Large Aggregation".to_string(),
            FindingType::InefficientParallelism => "Inefficient Parallelism".to_string(),
            FindingType::MissedParallelization => "Missed Parallelization".to_string(),
            FindingType::Custom(msg) => msg.clone(),
        }
    }
}

impl Renderable for Severity {
    fn render(&self) -> String {
        match self {
            Severity::Low => "Low".to_string(),
            Severity::Medium => "Medium".to_string(),
            Severity::High => "High".to_string(),
            Severity::Critical => "Critical".to_string(),
        }
    }
}

impl Renderable for PerformanceAssessment {
    fn render(&self) -> String {
        match self {
            PerformanceAssessment::Excellent => "Excellent".to_string(),
            PerformanceAssessment::Good => "Good".to_string(),
            PerformanceAssessment::Fair => "Fair".to_string(),
            PerformanceAssessment::Poor => "Poor".to_string(),
            PerformanceAssessment::Critical => "Critical".to_string(),
        }
    }
}

/// Enhanced Finding renderer with full context
impl Renderable for Finding {
    fn render(&self) -> String {
        format!("{}: {}", self.finding_type.render(), self.extract_clean_title())
    }
    
    fn render_with_context(&self, context: &RenderContext) -> String {
        if context.include_details {
            self.render_detailed()
        } else {
            self.render()
        }
    }
}

/// Extension trait to add rendering methods to Finding
pub trait FindingRenderer {
    /// Extract a clean title from the potentially verbose original title
    fn extract_clean_title(&self) -> String;
    /// Extract clean component names (tables, indexes, etc.) from title
    fn extract_components_from_title(&self, title: &str) -> String;
    /// Render detailed finding with evidence and thresholds
    fn render_detailed(&self) -> String;
    /// Render key evidence in a structured way
    fn render_key_evidence(&self) -> Option<String>;
    /// Render threshold context information
    fn render_threshold_context(&self) -> Option<String>;
    /// Get all available evidence as formatted strings
    fn render_all_evidence(&self) -> Vec<String>;
}

impl FindingRenderer for Finding {
    fn extract_clean_title(&self) -> String {
        // Remove the finding type prefix if it's redundant
        let finding_type_str = self.finding_type.render();
        let clean_title = if self.title.starts_with(&finding_type_str) {
            self.title.strip_prefix(&finding_type_str)
                .unwrap_or(&self.title)
                .trim_start_matches(":")
                .trim()
        } else {
            &self.title
        };
        
        // Extract clean component names from the title
        self.extract_components_from_title(clean_title)
    }
    
    /// Extract clean component names (tables, indexes, etc.) from title
    fn extract_components_from_title(&self, title: &str) -> String {
        // Look for table/index names in metadata first
        if let Some(table_name) = self.metadata.get("table_name") {
            if let Some(index_name) = self.metadata.get("index_name") {
                return format!("table '{}' using index '{}'", table_name, index_name);
            } else {
                return format!("table '{}'", table_name);
            }
        }
        
        // Fallback: clean up the original title
        title
            .replace("PlanNode {", "")
            .replace("node_type:", "")
            .replace("description:", "")
            .split("}")
            .next()
            .unwrap_or(title)
            .trim()
            .to_string()
    }
    
    fn render_detailed(&self) -> String {
        let mut parts = vec![self.render()];
        
        // Add key evidence
        if let Some(evidence_str) = self.render_key_evidence() {
            parts.push(evidence_str);
        }
        
        // Add threshold context
        if let Some(threshold_str) = self.render_threshold_context() {
            parts.push(format!("(threshold: {})", threshold_str));
        }
        
        parts.join(" ")
    }
    
    fn render_key_evidence(&self) -> Option<String> {
        let mut evidence_parts = Vec::new();
        
        // Prioritized evidence display
        if let Some(rows) = self.evidence.get("row_count") {
            evidence_parts.push(format!("{:.0} rows", rows));
        }
        if let Some(cost) = self.evidence.get("total_cost") {
            evidence_parts.push(format!("cost: {:.1}", cost));
        }
        if let Some(error_ratio) = self.evidence.get("error_ratio") {
            evidence_parts.push(format!("{:.1}x error", error_ratio));
        }
        if let Some(duration) = self.evidence.get("duration_ms") {
            evidence_parts.push(format!("{:.1}ms", duration));
        }
        if let Some(memory) = self.evidence.get("memory_usage_kb") {
            evidence_parts.push(format!("{:.0}KB", memory));
        }
        
        if evidence_parts.is_empty() {
            None
        } else {
            Some(evidence_parts.join(", "))
        }
    }
    
    fn render_threshold_context(&self) -> Option<String> {
        let mut threshold_parts = Vec::new();
        
        if let Some(threshold) = self.evidence.get("cost_threshold") {
            threshold_parts.push(format!("cost > {:.1}", threshold));
        }
        if let Some(threshold) = self.evidence.get("row_threshold") {
            threshold_parts.push(format!("rows > {:.0}", threshold));
        }
        if let Some(threshold) = self.evidence.get("severity_threshold") {
            threshold_parts.push(format!("severity > {:.1}", threshold));
        }
        if let Some(threshold) = self.evidence.get("error_threshold") {
            threshold_parts.push(format!("error > {:.1}x", threshold));
        }
        
        if threshold_parts.is_empty() {
            None
        } else {
            Some(threshold_parts.join(", "))
        }
    }
    
    fn render_all_evidence(&self) -> Vec<String> {
        self.evidence
            .iter()
            .map(|(key, value)| {
                match key.as_str() {
                    "row_count" | "estimated_rows" => format!("{}: {:.0} rows", key, value),
                    "cost" | "total_cost" | "startup_cost" => format!("{}: {:.1}", key, value),
                    "duration_ms" | "actual_time_ms" => format!("{}: {:.1}ms", key, value),
                    "memory_usage_kb" => format!("{}: {:.0}KB", key, value),
                    "error_ratio" => format!("{}: {:.1}x", key, value),
                    _ => format!("{}: {:.2}", key, value),
                }
            })
            .collect()
    }
}

/// Extension trait to add rendering methods to PlanNode
pub trait PlanNodeRenderer {
    /// Extract a clean table name for display
    fn extract_table_name(&self) -> String;
    /// Extract a clean index name for display
    fn extract_index_name(&self) -> String;
    /// Extract table name from node description
    fn extract_table_from_description(&self) -> Option<String>;
    /// Extract index name from node description
    fn extract_index_from_description(&self) -> Option<String>;
}

impl PlanNodeRenderer for PlanNode {
    fn extract_table_name(&self) -> String {
        // Delegate to the core library's implementation
        // Note: This is a trait method delegating to the inherent method with the same name
        PlanNode::extract_table_name(self)
    }
    
    /// Extract table name from node description
    fn extract_table_from_description(&self) -> Option<String> {
        // Common patterns in PostgreSQL plan descriptions
        let desc = &self.description();
        
        // Pattern: "Seq Scan on table_name"
        if let Some(captures) = regex::Regex::new(r"(?:Seq Scan|Index Scan|Index Only Scan|Bitmap Heap Scan) on (\w+)")
            .ok()?.captures(desc) {
            return Some(captures.get(1)?.as_str().to_string());
        }
        
        // Pattern: "using index_name on table_name"
        if let Some(captures) = regex::Regex::new(r"using \w+ on (\w+)")
            .ok()?.captures(desc) {
            return Some(captures.get(1)?.as_str().to_string());
        }
        
        None
    }
    
    fn extract_index_name(&self) -> String {
        // Delegate to the core library's implementation
        PlanNode::extract_index_name(self)
    }
    
    /// Extract index name from node description
    fn extract_index_from_description(&self) -> Option<String> {
        let desc = &self.description();
        
        // Pattern: "using index_name"
        if let Some(captures) = regex::Regex::new(r"using (\w+)")
            .ok()?.captures(desc) {
            return Some(captures.get(1)?.as_str().to_string());
        }
        
        None
    }
}