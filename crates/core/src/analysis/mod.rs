use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use crate::ParsedPlan;

pub mod analyzers;
pub mod engine;
pub mod traversal;
pub mod consolidated_config;

/// Severity levels for analysis findings
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

/// Categories of performance findings
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FindingType {
    // Row and estimation issues
    ExcessiveRowProcessing,
    RowEstimationError,
    CartesianProduct,
    
    // Index and scan issues
    LargeSequentialScan,
    InefficiientScan,
    MissingIndex,
    PoorIndexSelectivity,
    
    // Join issues
    IneffectiveJoinAlgorithm,
    LargeNestedLoop,
    HashJoinMemorySpill,
    
    // Cost and performance issues
    HighStartupCost,
    ExpensiveOperation,
    HighCostVariability,
    
    // Memory and resource issues
    MemorySpill,
    LargeSort,
    LargeAggregation,
    
    // Parallelization issues
    InefficientParallelism,
    MissedParallelization,
    
    // Custom analyzer findings
    Custom(String),
}

/// Path to a specific node in the plan tree
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodePath {
    /// Indices from root to target node (e.g., [0, 1, 2] = root.children[0].children[1].children[2])
    pub path: Vec<usize>,
    /// Human-readable description of the node location
    pub description: String,
}

impl NodePath {
    pub fn root() -> Self {
        Self {
            path: vec![],
            description: "Root node".to_string(),
        }
    }
    
    pub fn child_of(&self, index: usize, description: String) -> Self {
        let mut path = self.path.clone();
        path.push(index);
        Self {
            path,
            description: format!("{} -> {}", self.description, description),
        }
    }
}

/// Individual analysis finding with evidence and suggestions
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    /// Type of finding for categorization
    pub finding_type: FindingType,
    /// Security level indicating urgency
    pub severity: Severity,
    /// Short, descriptive title
    pub title: String,
    /// Detailed description of the issue
    pub description: String,
    /// Actionable suggestion for improvement
    pub suggestion: String,
    /// Path to the affected node(s) in the plan tree
    pub affected_nodes: Vec<NodePath>,
    /// Quantitative evidence supporting the finding
    pub evidence: HashMap<String, f64>,
    /// Additional metadata specific to this finding
    pub metadata: HashMap<String, String>,
}

impl Finding {
    pub fn new(
        finding_type: FindingType,
        severity: Severity,
        title: String,
        description: String,
        suggestion: String,
    ) -> Self {
        Self {
            finding_type,
            severity,
            title,
            description,
            suggestion,
            affected_nodes: vec![],
            evidence: HashMap::new(),
            metadata: HashMap::new(),
        }
    }
    
    pub fn with_node(mut self, node_path: NodePath) -> Self {
        self.affected_nodes.push(node_path);
        self
    }
    
    pub fn with_evidence(mut self, key: &str, value: f64) -> Self {
        self.evidence.insert(key.to_string(), value);
        self
    }
    
    pub fn with_metadata(mut self, key: &str, value: &str) -> Self {
        self.metadata.insert(key.to_string(), value.to_string());
        self
    }
}

/// Report from a single analyzer containing all findings and metrics
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisReport {
    /// Name of the analyzer that produced this report
    pub analyzer_name: String,
    /// All findings discovered by this analyzer
    pub findings: Vec<Finding>,
    /// Aggregate metrics calculated by this analyzer
    pub metrics: HashMap<String, f64>,
    /// Analyzer-specific metadata and configuration info
    pub metadata: HashMap<String, String>,
}

impl AnalysisReport {
    pub fn new(analyzer_name: String) -> Self {
        Self {
            analyzer_name,
            findings: vec![],
            metrics: HashMap::new(),
            metadata: HashMap::new(),
        }
    }
    
    pub fn add_finding(mut self, finding: Finding) -> Self {
        self.findings.push(finding);
        self
    }
    
    pub fn with_metric(mut self, key: &str, value: f64) -> Self {
        self.metrics.insert(key.to_string(), value);
        self
    }
    
    pub fn with_metadata(mut self, key: &str, value: &str) -> Self {
        self.metadata.insert(key.to_string(), value.to_string());
        self
    }
    
    /// Get the highest severity finding in this report
    pub fn max_severity(&self) -> Option<&Severity> {
        self.findings.iter().map(|f| &f.severity).max()
    }
    
    /// Count findings by severity level
    pub fn severity_counts(&self) -> HashMap<Severity, usize> {
        let mut counts = HashMap::new();
        for finding in &self.findings {
            *counts.entry(finding.severity.clone()).or_insert(0) += 1;
        }
        counts
    }
    
    /// Get findings of a specific type
    pub fn findings_of_type(&self, finding_type: &FindingType) -> Vec<&Finding> {
        self.findings.iter().filter(|f| &f.finding_type == finding_type).collect()
    }
}

/// Environment context provided to analyzers
#[derive(Debug, Clone, PartialEq)]
pub struct AnalysisContext {
    /// Available memory for work operations (in KB)
    pub work_mem_kb: usize,
    /// Maximum parallel workers available
    pub max_parallel_workers: usize,
    /// PostgreSQL version (affects available features)
    pub pg_version: String,
    /// Query execution duration if available (in milliseconds)
    pub query_duration_ms: Option<f64>,
    /// Additional configuration parameters
    pub config_params: HashMap<String, String>,
}

impl Default for AnalysisContext {
    fn default() -> Self {
        Self {
            work_mem_kb: 4096, // 4MB default
            max_parallel_workers: 2,
            pg_version: "14.0".to_string(),
            query_duration_ms: None,
            config_params: HashMap::new(),
        }
    }
}

impl AnalysisContext {
    pub fn new() -> Self {
        Self::default()
    }
    
    pub fn with_work_mem_kb(mut self, work_mem_kb: usize) -> Self {
        self.work_mem_kb = work_mem_kb;
        self
    }
    
    pub fn with_parallel_workers(mut self, max_parallel_workers: usize) -> Self {
        self.max_parallel_workers = max_parallel_workers;
        self
    }
    
    pub fn with_pg_version(mut self, pg_version: String) -> Self {
        self.pg_version = pg_version;
        self
    }
    
    pub fn with_query_duration(mut self, duration_ms: f64) -> Self {
        self.query_duration_ms = Some(duration_ms);
        self
    }
    
    pub fn with_config_param(mut self, key: &str, value: &str) -> Self {
        self.config_params.insert(key.to_string(), value.to_string());
        self
    }
}

/// Base trait for all plan analyzers
pub trait Analyzer: Send + Sync {
    /// Analyze the given plan and return findings
    fn analyze(&self, plan: &ParsedPlan, context: &AnalysisContext) -> AnalysisReport;
    
    /// Human-readable name of this analyzer
    fn name(&self) -> &'static str;
    
    /// Description of what this analyzer does
    fn description(&self) -> &'static str;
    
    /// Check if this analyzer supports the given plan type
    fn supports_plan(&self, _plan: &ParsedPlan) -> bool {
        // By default, support all plan types
        true
    }
    
    /// Get the version of this analyzer (for compatibility/debugging)
    fn version(&self) -> &'static str {
        "1.0.0"
    }
}

/// Trait for analyzers that can be configured
pub trait ConfigurableAnalyzer: Analyzer {
    type Config: Clone + Send + Sync;
    
    /// Configure this analyzer with the given configuration
    fn configure(&mut self, config: Self::Config);
    
    /// Get the default configuration for this analyzer
    fn default_config() -> Self::Config;
    
    /// Get the current configuration
    fn current_config(&self) -> &Self::Config;
}

/// Combined result from multiple analyzers
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CombinedAnalysisResult {
    /// Reports from individual analyzers
    pub reports: Vec<AnalysisReport>,
    /// Overall analysis summary
    pub summary: AnalysisSummary,
}

/// High-level summary of analysis results
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisSummary {
    /// Total number of findings across all analyzers
    pub total_findings: usize,
    /// Count of findings by severity
    pub severity_counts: HashMap<Severity, usize>,
    /// Count of findings by type
    pub type_counts: HashMap<String, usize>, // Using String for serialization
    /// Most critical issues (top 5 by severity)
    pub top_issues: Vec<Finding>,
    /// Overall performance assessment
    pub performance_assessment: PerformanceAssessment,
}

/// Overall performance assessment based on analysis
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PerformanceAssessment {
    Excellent,  // No significant issues
    Good,       // Minor optimizations possible
    Fair,       // Some performance issues present
    Poor,       // Significant performance problems
    Critical,   // Severe performance issues requiring immediate attention
}

impl CombinedAnalysisResult {
    pub fn new(reports: Vec<AnalysisReport>) -> Self {
        let summary = Self::calculate_summary(&reports);
        Self { reports, summary }
    }
    
    fn calculate_summary(reports: &[AnalysisReport]) -> AnalysisSummary {
        let mut all_findings = vec![];
        let mut severity_counts = HashMap::new();
        let mut type_counts = HashMap::new();
        
        // Collect all findings
        for report in reports {
            all_findings.extend(report.findings.iter().cloned());
        }
        
        // Count by severity and type
        for finding in &all_findings {
            *severity_counts.entry(finding.severity.clone()).or_insert(0) += 1;
            let type_key = format!("{:?}", finding.finding_type);
            *type_counts.entry(type_key).or_insert(0) += 1;
        }
        
        // Get top 5 most critical issues
        let mut top_issues = all_findings.clone();
        top_issues.sort_by(|a, b| b.severity.cmp(&a.severity));
        top_issues.truncate(5);
        
        // Assess overall performance
        let performance_assessment = Self::assess_performance(&severity_counts);
        
        AnalysisSummary {
            total_findings: all_findings.len(),
            severity_counts,
            type_counts,
            top_issues,
            performance_assessment,
        }
    }
    
    fn assess_performance(severity_counts: &HashMap<Severity, usize>) -> PerformanceAssessment {
        let critical_count = severity_counts.get(&Severity::Critical).unwrap_or(&0);
        let high_count = severity_counts.get(&Severity::High).unwrap_or(&0);
        let medium_count = severity_counts.get(&Severity::Medium).unwrap_or(&0);
        
        match (*critical_count, *high_count, *medium_count) {
            (c, _, _) if c >= 3 => PerformanceAssessment::Critical,
            (c, h, _) if c >= 1 || h >= 5 => PerformanceAssessment::Poor,
            (0, h, m) if h >= 2 || m >= 5 => PerformanceAssessment::Fair,
            (0, h, m) if h >= 1 || m >= 2 => PerformanceAssessment::Good,
            _ => PerformanceAssessment::Excellent,
        }
    }
    
    /// Get all findings across all reports
    pub fn all_findings(&self) -> Vec<&Finding> {
        self.reports.iter().flat_map(|r| &r.findings).collect()
    }
    
    /// Get findings of a specific severity
    pub fn findings_by_severity(&self, severity: &Severity) -> Vec<&Finding> {
        self.all_findings().into_iter().filter(|f| &f.severity == severity).collect()
    }
    
    /// Get findings of a specific type
    pub fn findings_by_type(&self, finding_type: &FindingType) -> Vec<&Finding> {
        self.all_findings().into_iter().filter(|f| &f.finding_type == finding_type).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_finding_creation() {
        let finding = Finding::new(
            FindingType::ExcessiveRowProcessing,
            Severity::High,
            "Large scan detected".to_string(),
            "Scanning 1M rows".to_string(),
            "Add indexes".to_string(),
        )
        .with_evidence("row_count", 1_000_000.0)
        .with_metadata("table_name", "users");
        
        assert_eq!(finding.severity, Severity::High);
        assert_eq!(finding.evidence.get("row_count"), Some(&1_000_000.0));
        assert_eq!(finding.metadata.get("table_name"), Some(&"users".to_string()));
    }
    
    #[test]
    fn test_analysis_report() {
        let report = AnalysisReport::new("TestAnalyzer".to_string())
            .add_finding(Finding::new(
                FindingType::ExcessiveRowProcessing,
                Severity::High,
                "Test finding".to_string(),
                "Test description".to_string(),
                "Test suggestion".to_string(),
            ))
            .with_metric("total_cost", 1000.0);
        
        assert_eq!(report.analyzer_name, "TestAnalyzer");
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.max_severity(), Some(&Severity::High));
        assert_eq!(report.metrics.get("total_cost"), Some(&1000.0));
    }
    
    #[test]
    fn test_node_path() {
        let root = NodePath::root();
        let child = root.child_of(0, "Index Scan".to_string());
        let grandchild = child.child_of(1, "Sort".to_string());
        
        assert_eq!(root.path, Vec::<usize>::new());
        assert_eq!(child.path, vec![0usize]);
        assert_eq!(grandchild.path, vec![0usize, 1usize]);
        assert_eq!(grandchild.description, "Root node -> Index Scan -> Sort");
    }
    
    #[test]
    fn test_analysis_context() {
        let context = AnalysisContext::new()
            .with_work_mem_kb(8192)
            .with_parallel_workers(4)
            .with_config_param("enable_hashjoin", "on");
        
        assert_eq!(context.work_mem_kb, 8192);
        assert_eq!(context.max_parallel_workers, 4);
        assert_eq!(context.config_params.get("enable_hashjoin"), Some(&"on".to_string()));
    }
}