use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use super::{Severity, FindingType};

/// Configuration for analysis thresholds and behavior
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisConfig {
    /// Global settings that apply to all analyzers
    pub global: GlobalConfig,
    /// Row estimation analyzer configuration
    pub row_estimation: RowEstimationConfig,
    /// Scan analyzer configuration
    pub scan_analysis: ScanAnalysisConfig,
    /// Join analyzer configuration  
    pub join_analysis: JoinAnalysisConfig,
    /// Cost analyzer configuration
    pub cost_analysis: CostAnalysisConfig,
    /// Memory analyzer configuration
    pub memory_analysis: MemoryAnalysisConfig,
    /// Parallelization analyzer configuration
    pub parallelization: ParallelizationConfig,
    /// Custom configuration values for extensibility
    pub custom: HashMap<String, serde_json::Value>,
}

impl Default for AnalysisConfig {
    fn default() -> Self {
        Self {
            global: GlobalConfig::default(),
            row_estimation: RowEstimationConfig::default(),
            scan_analysis: ScanAnalysisConfig::default(),
            join_analysis: JoinAnalysisConfig::default(),
            cost_analysis: CostAnalysisConfig::default(),
            memory_analysis: MemoryAnalysisConfig::default(),
            parallelization: ParallelizationConfig::default(),
            custom: HashMap::new(),
        }
    }
}

/// Global configuration settings
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GlobalConfig {
    /// Minimum severity level to report findings
    pub min_severity: Severity,
    /// Whether to include suggestions in findings
    pub include_suggestions: bool,
    /// Whether to include detailed evidence in findings
    pub include_evidence: bool,
    /// Maximum number of findings per analyzer
    pub max_findings_per_analyzer: Option<usize>,
    /// Default work_mem size in KB (used when not provided in context)
    pub default_work_mem_kb: usize,
    /// Default max parallel workers (used when not provided in context)
    pub default_max_parallel_workers: usize,
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            min_severity: Severity::Low,
            include_suggestions: true,
            include_evidence: true,
            max_findings_per_analyzer: Some(50),
            default_work_mem_kb: 4096, // 4MB
            default_max_parallel_workers: 2,
        }
    }
}

/// Configuration for row estimation analysis
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowEstimationConfig {
    /// Enabled finding types
    pub enabled_findings: Vec<FindingType>,
    /// Row count thresholds for different severity levels
    pub row_thresholds: RowThresholds,
    /// Estimation error ratio thresholds
    pub estimation_error_thresholds: EstimationErrorThresholds,
    /// Cartesian product detection settings
    pub cartesian_product: CartesianProductConfig,
}

impl Default for RowEstimationConfig {
    fn default() -> Self {
        Self {
            enabled_findings: vec![
                FindingType::ExcessiveRowProcessing,
                FindingType::RowEstimationError,
                FindingType::CartesianProduct,
            ],
            row_thresholds: RowThresholds::default(),
            estimation_error_thresholds: EstimationErrorThresholds::default(),
            cartesian_product: CartesianProductConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowThresholds {
    pub critical_row_count: u64,
    pub high_row_count: u64,
    pub medium_row_count: u64,
}

impl Default for RowThresholds {
    fn default() -> Self {
        Self {
            critical_row_count: 1_000_000,
            high_row_count: 100_000,
            medium_row_count: 50_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EstimationErrorThresholds {
    pub critical_error_ratio: f64,
    pub high_error_ratio: f64,
    pub medium_error_ratio: f64,
    pub min_row_count_for_analysis: u64,
}

impl Default for EstimationErrorThresholds {
    fn default() -> Self {
        Self {
            critical_error_ratio: 10.0,
            high_error_ratio: 3.0,
            medium_error_ratio: 1.0,
            min_row_count_for_analysis: 100,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CartesianProductConfig {
    pub enabled: bool,
    pub min_cartesian_ratio: f64,
    pub min_total_rows_for_detection: u64,
}

impl Default for CartesianProductConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_cartesian_ratio: 0.5,
            min_total_rows_for_detection: 10_000,
        }
    }
}

/// Configuration for scan analysis
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanAnalysisConfig {
    pub enabled_findings: Vec<FindingType>,
    pub sequential_scan: SequentialScanConfig,
    pub index_scan: IndexScanConfig,
    pub bitmap_scan: BitmapScanConfig,
}

impl Default for ScanAnalysisConfig {
    fn default() -> Self {
        Self {
            enabled_findings: vec![
                FindingType::LargeSequentialScan,
                FindingType::InefficiientScan,
                FindingType::MissingIndex,
                FindingType::PoorIndexSelectivity,
            ],
            sequential_scan: SequentialScanConfig::default(),
            index_scan: IndexScanConfig::default(),
            bitmap_scan: BitmapScanConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SequentialScanConfig {
    pub high_row_threshold: u64,
    pub high_cost_threshold: f64,
    pub medium_row_threshold: u64,
    pub medium_cost_threshold: f64,
    pub report_filtered_scans: bool,
}

impl Default for SequentialScanConfig {
    fn default() -> Self {
        Self {
            high_row_threshold: 100_000,
            high_cost_threshold: 10_000.0,
            medium_row_threshold: 10_000,
            medium_cost_threshold: 1_000.0,
            report_filtered_scans: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexScanConfig {
    pub high_cost_range_span: f64,
    pub medium_startup_cost: f64,
    pub startup_ratio_threshold: f64,
    pub min_rows_for_analysis: u64,
}

impl Default for IndexScanConfig {
    fn default() -> Self {
        Self {
            high_cost_range_span: 50_000.0,
            medium_startup_cost: 100.0,
            startup_ratio_threshold: 0.5,
            min_rows_for_analysis: 1_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BitmapScanConfig {
    pub cost_per_row_threshold: f64,
    pub min_rows_for_analysis: u64,
}

impl Default for BitmapScanConfig {
    fn default() -> Self {
        Self {
            cost_per_row_threshold: 50.0,
            min_rows_for_analysis: 1_000,
        }
    }
}

/// Configuration for join analysis
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JoinAnalysisConfig {
    pub enabled_findings: Vec<FindingType>,
    pub nested_loop: NestedLoopConfig,
    pub hash_join: HashJoinConfig,
}

impl Default for JoinAnalysisConfig {
    fn default() -> Self {
        Self {
            enabled_findings: vec![
                FindingType::IneffectiveJoinAlgorithm,
                FindingType::LargeNestedLoop,
                FindingType::HashJoinMemorySpill,
            ],
            nested_loop: NestedLoopConfig::default(),
            hash_join: HashJoinConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NestedLoopConfig {
    pub high_outer_row_threshold: u64,
    pub medium_outer_row_threshold: u64,
    pub inner_cost_threshold: f64,
}

impl Default for NestedLoopConfig {
    fn default() -> Self {
        Self {
            high_outer_row_threshold: 10_000,
            medium_outer_row_threshold: 1_000,
            inner_cost_threshold: 10.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HashJoinConfig {
    pub memory_spill_ratio: f64,
    pub memory_warning_ratio: f64,
    pub hash_overhead_factor: f64,
}

impl Default for HashJoinConfig {
    fn default() -> Self {
        Self {
            memory_spill_ratio: 2.0,
            memory_warning_ratio: 1.2,
            hash_overhead_factor: 1.3,
        }
    }
}

/// Configuration for cost analysis
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostAnalysisConfig {
    pub enabled_findings: Vec<FindingType>,
    pub startup_cost: StartupCostConfig,
    pub total_cost: TotalCostConfig,
    pub cost_variability: CostVariabilityConfig,
}

impl Default for CostAnalysisConfig {
    fn default() -> Self {
        Self {
            enabled_findings: vec![
                FindingType::HighStartupCost,
                FindingType::ExpensiveOperation,
                FindingType::HighCostVariability,
            ],
            startup_cost: StartupCostConfig::default(),
            total_cost: TotalCostConfig::default(),
            cost_variability: CostVariabilityConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StartupCostConfig {
    pub high_absolute_threshold: f64,
    pub medium_absolute_threshold: f64,
    pub high_ratio_threshold: f64,
}

impl Default for StartupCostConfig {
    fn default() -> Self {
        Self {
            high_absolute_threshold: 10_000.0,
            medium_absolute_threshold: 1_000.0,
            high_ratio_threshold: 0.8,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TotalCostConfig {
    pub high_cost_threshold: f64,
    pub medium_cost_threshold: f64,
    pub duration_correlation_enabled: bool,
    pub high_duration_ms: f64,
}

impl Default for TotalCostConfig {
    fn default() -> Self {
        Self {
            high_cost_threshold: 50_000.0,
            medium_cost_threshold: 10_000.0,
            duration_correlation_enabled: true,
            high_duration_ms: 5_000.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostVariabilityConfig {
    pub variability_ratio_threshold: f64,
    pub min_cost_range_span: f64,
}

impl Default for CostVariabilityConfig {
    fn default() -> Self {
        Self {
            variability_ratio_threshold: 0.9,
            min_cost_range_span: 1_000.0,
        }
    }
}

/// Configuration for memory analysis
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryAnalysisConfig {
    pub enabled_findings: Vec<FindingType>,
    pub sort_operations: SortMemoryConfig,
    pub hash_operations: HashMemoryConfig,
    pub aggregate_operations: AggregateMemoryConfig,
}

impl Default for MemoryAnalysisConfig {
    fn default() -> Self {
        Self {
            enabled_findings: vec![
                FindingType::MemorySpill,
                FindingType::LargeSort,
                FindingType::LargeAggregation,
            ],
            sort_operations: SortMemoryConfig::default(),
            hash_operations: HashMemoryConfig::default(),
            aggregate_operations: AggregateMemoryConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SortMemoryConfig {
    pub spill_ratio_threshold: f64,
    pub warning_ratio_threshold: f64,
    pub memory_overhead_factor: f64,
}

impl Default for SortMemoryConfig {
    fn default() -> Self {
        Self {
            spill_ratio_threshold: 2.0,
            warning_ratio_threshold: 1.2,
            memory_overhead_factor: 1.5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HashMemoryConfig {
    pub spill_ratio_threshold: f64,
    pub warning_ratio_threshold: f64,
    pub hash_overhead_factor: f64,
}

impl Default for HashMemoryConfig {
    fn default() -> Self {
        Self {
            spill_ratio_threshold: 2.0,
            warning_ratio_threshold: 1.2,
            hash_overhead_factor: 1.3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AggregateMemoryConfig {
    pub large_input_threshold: u64,
    pub poor_reduction_threshold: f64,
    pub memory_overhead_factor: f64,
}

impl Default for AggregateMemoryConfig {
    fn default() -> Self {
        Self {
            large_input_threshold: 1_000_000,
            poor_reduction_threshold: 10.0,
            memory_overhead_factor: 1.3,
        }
    }
}

/// Configuration for parallelization analysis
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParallelizationConfig {
    pub enabled_findings: Vec<FindingType>,
    pub efficiency_analysis: ParallelEfficiencyConfig,
    pub opportunity_analysis: ParallelOpportunityConfig,
}

impl Default for ParallelizationConfig {
    fn default() -> Self {
        Self {
            enabled_findings: vec![
                FindingType::InefficientParallelism,
                FindingType::MissedParallelization,
            ],
            efficiency_analysis: ParallelEfficiencyConfig::default(),
            opportunity_analysis: ParallelOpportunityConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParallelEfficiencyConfig {
    pub min_row_threshold_for_efficiency: u64,
    pub min_cost_threshold_for_efficiency: f64,
}

impl Default for ParallelEfficiencyConfig {
    fn default() -> Self {
        Self {
            min_row_threshold_for_efficiency: 10_000,
            min_cost_threshold_for_efficiency: 100.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParallelOpportunityConfig {
    pub opportunity_cost_threshold: f64,
    pub opportunity_row_threshold: u64,
}

impl Default for ParallelOpportunityConfig {
    fn default() -> Self {
        Self {
            opportunity_cost_threshold: 10_000.0,
            opportunity_row_threshold: 50_000,
        }
    }
}

/// Trait for getting configuration values with fallbacks
pub trait ConfigurationProvider {
    /// Get a configuration value with type conversion and fallback
    fn get_config<T>(&self, key: &str, default: T) -> T
    where
        T: Clone + for<'de> Deserialize<'de>;
    
    /// Check if a finding type is enabled for a specific analyzer
    fn is_finding_enabled(&self, analyzer: &str, finding_type: &FindingType) -> bool;
    
    /// Get the minimum severity level
    fn min_severity(&self) -> &Severity;
}

impl ConfigurationProvider for AnalysisConfig {
    fn get_config<T>(&self, key: &str, default: T) -> T
    where
        T: Clone + for<'de> Deserialize<'de>,
    {
        if let Some(value) = self.custom.get(key) {
            if let Ok(parsed) = serde_json::from_value(value.clone()) {
                return parsed;
            }
        }
        default
    }
    
    fn is_finding_enabled(&self, analyzer: &str, finding_type: &FindingType) -> bool {
        let enabled_findings = match analyzer {
            "row_estimation" => &self.row_estimation.enabled_findings,
            "scan_analysis" => &self.scan_analysis.enabled_findings,
            "join_analysis" => &self.join_analysis.enabled_findings,
            "cost_analysis" => &self.cost_analysis.enabled_findings,
            "memory_analysis" => &self.memory_analysis.enabled_findings,
            "parallelization" => &self.parallelization.enabled_findings,
            _ => return true, // Unknown analyzers default to enabled
        };
        
        enabled_findings.contains(finding_type)
    }
    
    fn min_severity(&self) -> &Severity {
        &self.global.min_severity
    }
}

/// Utility functions for configuration management
pub struct ConfigUtils;

impl ConfigUtils {
    /// Load configuration from TOML string
    pub fn from_toml(toml_str: &str) -> Result<AnalysisConfig, toml::de::Error> {
        toml::from_str(toml_str)
    }
    
    /// Save configuration to TOML string
    pub fn to_toml(config: &AnalysisConfig) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(config)
    }
    
    /// Load configuration from JSON string
    pub fn from_json(json_str: &str) -> Result<AnalysisConfig, serde_json::Error> {
        serde_json::from_str(json_str)
    }
    
    /// Save configuration to JSON string
    pub fn to_json(config: &AnalysisConfig) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(config)
    }
    
    /// Create a configuration with only high-severity findings enabled
    pub fn high_severity_only() -> AnalysisConfig {
        let mut config = AnalysisConfig::default();
        config.global.min_severity = Severity::High;
        config
    }
    
    /// Create a configuration for performance-focused analysis
    pub fn performance_focused() -> AnalysisConfig {
        let mut config = AnalysisConfig::default();
        
        // Focus on high-impact performance issues
        config.row_estimation.enabled_findings = vec![
            FindingType::ExcessiveRowProcessing,
            FindingType::CartesianProduct,
        ];
        
        config.scan_analysis.enabled_findings = vec![
            FindingType::LargeSequentialScan,
            FindingType::MissingIndex,
        ];
        
        config.join_analysis.enabled_findings = vec![
            FindingType::LargeNestedLoop,
            FindingType::HashJoinMemorySpill,
        ];
        
        config.cost_analysis.enabled_findings = vec![
            FindingType::ExpensiveOperation,
        ];
        
        config.memory_analysis.enabled_findings = vec![
            FindingType::MemorySpill,
        ];
        
        config
    }
    
    /// Create a configuration for development/debugging (all findings enabled)
    pub fn development_mode() -> AnalysisConfig {
        let mut config = AnalysisConfig::default();
        config.global.min_severity = Severity::Low;
        config.global.max_findings_per_analyzer = None; // No limit
        config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_default_configuration() {
        let config = AnalysisConfig::default();
        
        assert_eq!(config.global.min_severity, Severity::Low);
        assert!(config.global.include_suggestions);
        assert!(config.global.include_evidence);
        assert_eq!(config.global.default_work_mem_kb, 4096);
    }
    
    #[test]
    fn test_configuration_provider() {
        let config = AnalysisConfig::default();
        
        assert_eq!(config.min_severity(), &Severity::Low);
        assert!(config.is_finding_enabled("row_estimation", &FindingType::ExcessiveRowProcessing));
        assert!(!config.is_finding_enabled("unknown_analyzer", &FindingType::ExcessiveRowProcessing));
    }
    
    #[test]
    fn test_custom_configuration() {
        let mut config = AnalysisConfig::default();
        config.custom.insert("custom_threshold".to_string(), serde_json::json!(42.0));
        
        let threshold: f64 = config.get_config("custom_threshold", 0.0);
        assert_eq!(threshold, 42.0);
        
        let missing: f64 = config.get_config("missing_key", 99.0);
        assert_eq!(missing, 99.0);
    }
    
    #[test]
    fn test_toml_serialization() {
        let config = AnalysisConfig::default();
        let toml_str = ConfigUtils::to_toml(&config).unwrap();
        let parsed_config = ConfigUtils::from_toml(&toml_str).unwrap();
        
        assert_eq!(config, parsed_config);
    }
    
    #[test]
    fn test_json_serialization() {
        let config = AnalysisConfig::default();
        let json_str = ConfigUtils::to_json(&config).unwrap();
        let parsed_config = ConfigUtils::from_json(&json_str).unwrap();
        
        assert_eq!(config, parsed_config);
    }
    
    #[test]
    fn test_specialized_configurations() {
        let high_sev_config = ConfigUtils::high_severity_only();
        assert_eq!(high_sev_config.global.min_severity, Severity::High);
        
        let perf_config = ConfigUtils::performance_focused();
        assert_eq!(perf_config.row_estimation.enabled_findings.len(), 2);
        
        let dev_config = ConfigUtils::development_mode();
        assert_eq!(dev_config.global.min_severity, Severity::Low);
        assert!(dev_config.global.max_findings_per_analyzer.is_none());
    }
    
    #[test]
    fn test_threshold_configurations() {
        let config = AnalysisConfig::default();
        
        assert_eq!(config.row_estimation.row_thresholds.critical_row_count, 1_000_000);
        assert_eq!(config.scan_analysis.sequential_scan.high_row_threshold, 100_000);
        assert_eq!(config.join_analysis.nested_loop.high_outer_row_threshold, 10_000);
        assert_eq!(config.cost_analysis.startup_cost.high_absolute_threshold, 10_000.0);
    }
}