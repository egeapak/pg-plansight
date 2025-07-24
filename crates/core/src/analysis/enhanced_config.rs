/// Enhanced Analysis Configuration System
/// 
/// This bridges the existing configuration with the new unified threshold system,
/// providing both backward compatibility and improved consistency.

use serde::{Deserialize, Serialize};
use super::config::*;
use super::unified_config::*;
use super::{Severity, FindingType};
use std::collections::HashMap;

/// Enhanced analysis configuration that combines unified thresholds with specific analyzer configs
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnhancedAnalysisConfig {
    /// Global settings
    pub global: GlobalConfig,
    
    /// Unified analysis context for consistent thresholds
    pub context: UnifiedAnalysisContext,
    
    /// Row estimation analyzer with unified thresholds
    pub row_estimation: EnhancedRowEstimationConfig,
    
    /// Scan analyzer with unified thresholds
    pub scan_analysis: EnhancedScanAnalysisConfig,
    
    /// Join analyzer with unified thresholds
    pub join_analysis: EnhancedJoinAnalysisConfig,
    
    /// Cost analyzer with unified thresholds
    pub cost_analysis: EnhancedCostAnalysisConfig,
    
    /// Memory analyzer with unified thresholds
    pub memory_analysis: EnhancedMemoryAnalysisConfig,
    
    /// Parallelization analyzer with unified thresholds
    pub parallelization: EnhancedParallelizationConfig,
    
    /// Custom configuration values
    pub custom: HashMap<String, serde_json::Value>,
}

impl Default for EnhancedAnalysisConfig {
    fn default() -> Self {
        let context = UnifiedAnalysisContext::default();
        
        Self {
            global: GlobalConfig::default(),
            context: context.clone(),
            row_estimation: EnhancedRowEstimationConfig::new(&context),
            scan_analysis: EnhancedScanAnalysisConfig::new(&context),
            join_analysis: EnhancedJoinAnalysisConfig::new(&context),
            cost_analysis: EnhancedCostAnalysisConfig::new(&context),
            memory_analysis: EnhancedMemoryAnalysisConfig::new(&context),
            parallelization: EnhancedParallelizationConfig::new(&context),
            custom: HashMap::new(),
        }
    }
}

/// Enhanced row estimation configuration using unified thresholds
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnhancedRowEstimationConfig {
    pub enabled_findings: Vec<FindingType>,
    pub thresholds: UnifiedThresholds,
    pub cartesian_product: CartesianProductConfig,
    pub min_rows_for_analysis: u64,
}

impl EnhancedRowEstimationConfig {
    pub fn new(context: &UnifiedAnalysisContext) -> Self {
        Self {
            enabled_findings: vec![
                FindingType::ExcessiveRowProcessing,
                FindingType::RowEstimationError,
                FindingType::CartesianProduct,
            ],
            thresholds: UnifiedThresholds::for_operation(OperationType::Scan, context),
            cartesian_product: CartesianProductConfig::default(),
            min_rows_for_analysis: 100,
        }
    }
    
    pub fn classify_row_severity(&self, row_count: u64) -> Severity {
        self.thresholds.row_count.classify_severity(row_count)
    }
    
    pub fn classify_error_severity(&self, error_ratio: f64) -> Severity {
        self.thresholds.error_ratios.classify_severity_above(error_ratio)
    }
}

/// Enhanced scan analysis configuration
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnhancedScanAnalysisConfig {
    pub enabled_findings: Vec<FindingType>,
    pub seq_scan_thresholds: UnifiedThresholds,
    pub index_scan_thresholds: UnifiedThresholds,
    pub bitmap_scan_thresholds: UnifiedThresholds,
    pub report_filtered_scans: bool,
}

impl EnhancedScanAnalysisConfig {
    pub fn new(context: &UnifiedAnalysisContext) -> Self {
        Self {
            enabled_findings: vec![
                FindingType::LargeSequentialScan,
                FindingType::InefficiientScan,
                FindingType::MissingIndex,
                FindingType::PoorIndexSelectivity,
            ],
            seq_scan_thresholds: UnifiedThresholds::for_operation(OperationType::Scan, context),
            index_scan_thresholds: UnifiedThresholds::for_operation(OperationType::Index, context),
            bitmap_scan_thresholds: UnifiedThresholds::for_operation(OperationType::Index, context),
            report_filtered_scans: true,
        }
    }
}

/// Enhanced join analysis configuration
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnhancedJoinAnalysisConfig {
    pub enabled_findings: Vec<FindingType>,
    pub nested_loop_thresholds: UnifiedThresholds,
    pub hash_join_thresholds: UnifiedThresholds,
    pub merge_join_thresholds: UnifiedThresholds,
}

impl EnhancedJoinAnalysisConfig {
    pub fn new(context: &UnifiedAnalysisContext) -> Self {
        Self {
            enabled_findings: vec![
                FindingType::IneffectiveJoinAlgorithm,
                FindingType::LargeNestedLoop,
                FindingType::HashJoinMemorySpill,
            ],
            nested_loop_thresholds: UnifiedThresholds::for_operation(OperationType::Join, context),
            hash_join_thresholds: UnifiedThresholds::for_operation(OperationType::Hash, context),
            merge_join_thresholds: UnifiedThresholds::for_operation(OperationType::Sort, context),
        }
    }
}

/// Enhanced cost analysis configuration
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnhancedCostAnalysisConfig {
    pub enabled_findings: Vec<FindingType>,
    pub thresholds: UnifiedThresholds,
    pub startup_ratio_threshold: f64,
    pub duration_correlation_enabled: bool,
}

impl EnhancedCostAnalysisConfig {
    pub fn new(context: &UnifiedAnalysisContext) -> Self {
        Self {
            enabled_findings: vec![
                FindingType::HighStartupCost,
                FindingType::ExpensiveOperation,
                FindingType::HighCostVariability,
            ],
            thresholds: UnifiedThresholds::for_operation(OperationType::Scan, context), // Generic operation
            startup_ratio_threshold: 0.5, // 50% startup cost is concerning
            duration_correlation_enabled: true,
        }
    }
    
    pub fn classify_cost_severity(&self, cost: f64) -> Severity {
        self.thresholds.cost.classify_severity(cost)
    }
    
    pub fn classify_duration_severity(&self, duration_ms: f64) -> Severity {
        self.thresholds.duration.classify_severity(duration_ms)
    }
}

/// Enhanced memory analysis configuration
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnhancedMemoryAnalysisConfig {
    pub enabled_findings: Vec<FindingType>,
    pub sort_thresholds: UnifiedThresholds,
    pub hash_thresholds: UnifiedThresholds,
    pub aggregate_thresholds: UnifiedThresholds,
}

impl EnhancedMemoryAnalysisConfig {
    pub fn new(context: &UnifiedAnalysisContext) -> Self {
        Self {
            enabled_findings: vec![
                FindingType::MemorySpill,
                FindingType::LargeSort,
                FindingType::LargeAggregation,
            ],
            sort_thresholds: UnifiedThresholds::for_operation(OperationType::Sort, context),
            hash_thresholds: UnifiedThresholds::for_operation(OperationType::Hash, context),
            aggregate_thresholds: UnifiedThresholds::for_operation(OperationType::Aggregate, context),
        }
    }
    
    pub fn classify_memory_spill_severity(&self, spill_ratio: f64) -> Severity {
        self.sort_thresholds.memory_ratios.classify_severity_above(spill_ratio)
    }
}

/// Enhanced parallelization configuration
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnhancedParallelizationConfig {
    pub enabled_findings: Vec<FindingType>,
    pub thresholds: UnifiedThresholds,
    pub efficiency_threshold: f64,
}

impl EnhancedParallelizationConfig {
    pub fn new(context: &UnifiedAnalysisContext) -> Self {
        Self {
            enabled_findings: vec![
                FindingType::InefficientParallelism,
                FindingType::MissedParallelization,
            ],
            thresholds: UnifiedThresholds::for_operation(OperationType::Scan, context), // Generic
            efficiency_threshold: 0.7, // 70% efficiency expected
        }
    }
    
    pub fn classify_efficiency_severity(&self, efficiency: f64) -> Severity {
        self.thresholds.efficiency_ratios.classify_severity_below(efficiency)
    }
}

/// Migration utilities to convert old config to new config
impl EnhancedAnalysisConfig {
    /// Create enhanced config from legacy config
    pub fn from_legacy(legacy: &AnalysisConfig) -> Self {
        let mut enhanced = Self::default();
        
        // Preserve global settings
        enhanced.global = legacy.global.clone();
        
        // Convert enabled findings
        enhanced.row_estimation.enabled_findings = legacy.row_estimation.enabled_findings.clone();
        enhanced.scan_analysis.enabled_findings = legacy.scan_analysis.enabled_findings.clone();
        enhanced.join_analysis.enabled_findings = legacy.join_analysis.enabled_findings.clone();
        enhanced.cost_analysis.enabled_findings = legacy.cost_analysis.enabled_findings.clone();
        enhanced.memory_analysis.enabled_findings = legacy.memory_analysis.enabled_findings.clone();
        enhanced.parallelization.enabled_findings = legacy.parallelization.enabled_findings.clone();
        
        // Preserve specific settings that don't have unified equivalents
        enhanced.row_estimation.cartesian_product = legacy.row_estimation.cartesian_product.clone();
        enhanced.scan_analysis.report_filtered_scans = legacy.scan_analysis.sequential_scan.report_filtered_scans;
        enhanced.cost_analysis.duration_correlation_enabled = legacy.cost_analysis.total_cost.duration_correlation_enabled;
        
        enhanced
    }
    
    /// Infer context from legacy thresholds (best effort)
    pub fn infer_context_from_legacy(legacy: &AnalysisConfig) -> UnifiedAnalysisContext {
        // Analyze the legacy thresholds to infer what context they were designed for
        let row_threshold = legacy.row_estimation.row_thresholds.high_row_count;
        let cost_threshold = legacy.cost_analysis.total_cost.high_cost_threshold;
        
        let database_size = match row_threshold {
            ..=10_000 => DatabaseSize::Small,
            10_001..=100_000 => DatabaseSize::Medium,
            100_001..=1_000_000 => DatabaseSize::Large,
            _ => DatabaseSize::VeryLarge,
        };
        
        let performance_target = match cost_threshold {
            ..=1_000.0 => PerformanceTarget::Interactive,
            1_001.0..=10_000.0 => PerformanceTarget::Fast,
            10_001.0..=100_000.0 => PerformanceTarget::Batch,
            _ => PerformanceTarget::Background,
        };
        
        UnifiedAnalysisContext {
            database_size,
            workload_type: WorkloadType::Mixed, // Can't infer this easily
            performance_target,
        }
    }
}

/// Configuration builder for common scenarios
pub struct ConfigurationBuilder;

impl ConfigurationBuilder {
    /// Configuration for small OLTP databases with strict performance requirements
    pub fn small_oltp_interactive() -> EnhancedAnalysisConfig {
        let context = UnifiedAnalysisContext {
            database_size: DatabaseSize::Small,
            workload_type: WorkloadType::OLTP,
            performance_target: PerformanceTarget::Interactive,
        };
        
        let mut config = EnhancedAnalysisConfig::default();
        config.context = context.clone();
        config.row_estimation = EnhancedRowEstimationConfig::new(&context);
        config.scan_analysis = EnhancedScanAnalysisConfig::new(&context);
        config.join_analysis = EnhancedJoinAnalysisConfig::new(&context);
        config.cost_analysis = EnhancedCostAnalysisConfig::new(&context);
        config.memory_analysis = EnhancedMemoryAnalysisConfig::new(&context);
        config.parallelization = EnhancedParallelizationConfig::new(&context);
        
        config
    }
    
    /// Configuration for large OLAP databases with relaxed performance requirements
    pub fn large_olap_batch() -> EnhancedAnalysisConfig {
        let context = UnifiedAnalysisContext {
            database_size: DatabaseSize::Large,
            workload_type: WorkloadType::OLAP,
            performance_target: PerformanceTarget::Batch,
        };
        
        let mut config = EnhancedAnalysisConfig::default();
        config.context = context.clone();
        config.row_estimation = EnhancedRowEstimationConfig::new(&context);
        config.scan_analysis = EnhancedScanAnalysisConfig::new(&context);
        config.join_analysis = EnhancedJoinAnalysisConfig::new(&context);
        config.cost_analysis = EnhancedCostAnalysisConfig::new(&context);
        config.memory_analysis = EnhancedMemoryAnalysisConfig::new(&context);
        config.parallelization = EnhancedParallelizationConfig::new(&context);
        
        config
    }
    
    /// Configuration for development environments (more sensitive to catch issues early)
    pub fn development_sensitive() -> EnhancedAnalysisConfig {
        let context = UnifiedAnalysisContext {
            database_size: DatabaseSize::Small,
            workload_type: WorkloadType::Mixed,
            performance_target: PerformanceTarget::Interactive,
        };
        
        let mut config = EnhancedAnalysisConfig::default();
        config.context = context.clone();
        config.global.min_severity = Severity::Low; // Show even low-severity issues
        config.row_estimation = EnhancedRowEstimationConfig::new(&context);
        config.scan_analysis = EnhancedScanAnalysisConfig::new(&context);
        config.join_analysis = EnhancedJoinAnalysisConfig::new(&context);
        config.cost_analysis = EnhancedCostAnalysisConfig::new(&context);
        config.memory_analysis = EnhancedMemoryAnalysisConfig::new(&context);
        config.parallelization = EnhancedParallelizationConfig::new(&context);
        
        config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_enhanced_config_creation() {
        let config = EnhancedAnalysisConfig::default();
        
        // Should have unified thresholds for all analyzers
        assert!(config.row_estimation.thresholds.row_count.critical > 0);
        assert!(config.scan_analysis.seq_scan_thresholds.cost.extreme > 0.0);
        assert!(config.join_analysis.nested_loop_thresholds.row_count.high > 0);
    }
    
    #[test]
    fn test_severity_classification() {
        let config = EnhancedAnalysisConfig::default();
        
        // Test row count classification
        let high_rows = config.row_estimation.thresholds.row_count.high + 1;
        assert_eq!(
            config.row_estimation.classify_row_severity(high_rows),
            Severity::High
        );
        
        // Test error ratio classification  
        assert_eq!(
            config.row_estimation.classify_error_severity(15.0),
            Severity::High
        );
    }
    
    #[test]
    fn test_configuration_builder() {
        let small_config = ConfigurationBuilder::small_oltp_interactive();
        let large_config = ConfigurationBuilder::large_olap_batch();
        
        // Small OLTP should have lower thresholds than large OLAP
        assert!(
            small_config.row_estimation.thresholds.row_count.critical
            < large_config.row_estimation.thresholds.row_count.critical
        );
        
        assert!(
            small_config.cost_analysis.thresholds.cost.extreme
            < large_config.cost_analysis.thresholds.cost.extreme
        );
    }
    
    #[test]
    fn test_legacy_migration() {
        let legacy = AnalysisConfig::default();
        let enhanced = EnhancedAnalysisConfig::from_legacy(&legacy);
        
        // Should preserve enabled findings
        assert_eq!(
            enhanced.row_estimation.enabled_findings,
            legacy.row_estimation.enabled_findings
        );
        
        // Should preserve global settings
        assert_eq!(enhanced.global.min_severity, legacy.global.min_severity);
    }
}