/// Consolidated Analysis Configuration System
/// 
/// This module provides a single, coherent configuration system that replaces
/// the three overlapping systems (config.rs, enhanced_config.rs, unified_config.rs).
/// 
/// This is the RECOMMENDED configuration system going forward.
/// The other systems are maintained for backward compatibility but are deprecated.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use super::{Severity, FindingType};

/// Main configuration for all analysis operations
/// This replaces EnhancedAnalysisConfig as the primary configuration interface
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisConfiguration {
    /// Global analysis settings
    pub global: GlobalSettings,
    /// Workload context for smart threshold selection
    pub workload: WorkloadContext,
    /// Analyzer-specific configurations
    pub analyzers: AnalyzerConfigurations,
    /// Custom thresholds (overrides workload-based defaults)
    pub custom_thresholds: Option<CustomThresholds>,
}

impl Default for AnalysisConfiguration {
    fn default() -> Self {
        let workload = WorkloadContext::default();
        Self {
            global: GlobalSettings::default(),
            workload: workload.clone(),
            analyzers: AnalyzerConfigurations::for_workload(&workload),
            custom_thresholds: None,
        }
    }
}

/// Global settings that affect all analyzers
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GlobalSettings {
    /// Minimum severity level to report
    pub min_severity: Severity,
    /// Maximum number of findings per analyzer
    pub max_findings_per_analyzer: usize,
    /// Whether to include detailed evidence in findings
    pub include_evidence: bool,
    /// Whether to generate performance suggestions
    pub generate_suggestions: bool,
}

impl Default for GlobalSettings {
    fn default() -> Self {
        Self {
            min_severity: Severity::Low,
            max_findings_per_analyzer: 50,
            include_evidence: true,
            generate_suggestions: true,
        }
    }
}

/// Workload context for intelligent threshold selection
/// This replaces UnifiedAnalysisContext with a simpler, more focused approach
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkloadContext {
    /// Type of workload (affects performance expectations)
    pub workload_type: WorkloadType,
    /// Approximate database size category
    pub database_size: DatabaseSize,
    /// Performance requirements
    pub performance_target: PerformanceTarget,
    /// PostgreSQL version (affects available features)
    pub postgres_version: String,
}

impl Default for WorkloadContext {
    fn default() -> Self {
        Self {
            workload_type: WorkloadType::Mixed,
            database_size: DatabaseSize::Medium,
            performance_target: PerformanceTarget::Balanced,
            postgres_version: "14.0".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkloadType {
    /// High-frequency, low-complexity queries (sub-second response times)
    OLTP,
    /// Low-frequency, high-complexity queries (seconds to minutes acceptable)
    OLAP,
    /// Time-series and analytical workloads
    Analytics,
    /// Mixed workload with varying patterns
    Mixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DatabaseSize {
    /// < 1GB total data
    Small,
    /// 1GB - 100GB total data
    Medium,
    /// 100GB - 1TB total data
    Large,
    /// > 1TB total data
    VeryLarge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PerformanceTarget {
    /// Optimize for lowest latency (strictest thresholds)
    Latency,
    /// Optimize for highest throughput (moderate thresholds)
    Throughput,
    /// Balance between latency and throughput (balanced thresholds)
    Balanced,
    /// Focus on resource efficiency (relaxed thresholds)
    Efficiency,
}

/// Configuration for all analyzers
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalyzerConfigurations {
    pub row_estimation: RowEstimationConfig,
    pub scan_analysis: ScanAnalysisConfig,
    pub join_analysis: JoinAnalysisConfig,
    pub cost_analysis: CostAnalysisConfig,
    pub memory_analysis: MemoryAnalysisConfig,
    pub parallelization: ParallelizationConfig,
}

impl AnalyzerConfigurations {
    pub fn for_workload(workload: &WorkloadContext) -> Self {
        Self {
            row_estimation: RowEstimationConfig::for_workload(workload),
            scan_analysis: ScanAnalysisConfig::for_workload(workload),
            join_analysis: JoinAnalysisConfig::for_workload(workload),
            cost_analysis: CostAnalysisConfig::for_workload(workload),
            memory_analysis: MemoryAnalysisConfig::for_workload(workload),
            parallelization: ParallelizationConfig::for_workload(workload),
        }
    }
}

/// Simplified threshold system using workload-appropriate defaults
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SmartThresholds {
    /// Row count thresholds for different severity levels
    pub row_counts: ThresholdLevels<u64>,
    /// Cost thresholds (PostgreSQL cost units)
    pub costs: ThresholdLevels<f64>,
    /// Duration thresholds (milliseconds)
    pub durations: ThresholdLevels<f64>,
    /// Ratio thresholds (for error rates, efficiency, etc.)
    pub ratios: ThresholdLevels<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThresholdLevels<T> {
    pub low: T,
    pub medium: T,
    pub high: T,
    pub critical: T,
}

impl<T> ThresholdLevels<T> {
    pub fn classify(&self, value: &T) -> Severity 
    where
        T: PartialOrd,
    {
        if value >= &self.critical {
            Severity::Critical
        } else if value >= &self.high {
            Severity::High
        } else if value >= &self.medium {
            Severity::Medium
        } else if value >= &self.low {
            Severity::Low
        } else {
            Severity::Low
        }
    }
}

impl SmartThresholds {
    pub fn for_workload(workload: &WorkloadContext) -> Self {
        let (row_base, cost_base, duration_base) = Self::base_values_for_workload(workload);
        
        Self {
            row_counts: ThresholdLevels {
                low: (row_base * 0.1) as u64,
                medium: row_base as u64,
                high: (row_base * 10.0) as u64,
                critical: (row_base * 100.0) as u64,
            },
            costs: ThresholdLevels {
                low: cost_base * 0.1,
                medium: cost_base,
                high: cost_base * 10.0,
                critical: cost_base * 100.0,
            },
            durations: ThresholdLevels {
                low: duration_base * 0.1,
                medium: duration_base,
                high: duration_base * 10.0,
                critical: duration_base * 100.0,
            },
            ratios: ThresholdLevels {
                low: 1.5,     // 50% deviation
                medium: 3.0,  // 3x deviation  
                high: 10.0,   // 10x deviation
                critical: 100.0, // 100x deviation
            },
        }
    }
    
    fn base_values_for_workload(workload: &WorkloadContext) -> (f64, f64, f64) {
        let size_multiplier = match workload.database_size {
            DatabaseSize::Small => 0.1,
            DatabaseSize::Medium => 1.0,
            DatabaseSize::Large => 10.0,
            DatabaseSize::VeryLarge => 100.0,
        };
        
        let (workload_row_base, workload_cost_base, workload_duration_base) = match workload.workload_type {
            WorkloadType::OLTP => (10_000.0, 1_000.0, 100.0),      // Small, fast operations
            WorkloadType::OLAP => (1_000_000.0, 100_000.0, 10_000.0), // Large, complex operations
            WorkloadType::Analytics => (500_000.0, 50_000.0, 5_000.0), // Medium operations
            WorkloadType::Mixed => (100_000.0, 10_000.0, 1_000.0),     // Balanced
        };
        
        let performance_multiplier = match workload.performance_target {
            PerformanceTarget::Latency => 0.1,    // Very strict
            PerformanceTarget::Throughput => 2.0, // More lenient  
            PerformanceTarget::Balanced => 1.0,   // Standard
            PerformanceTarget::Efficiency => 5.0, // Most lenient
        };
        
        (
            workload_row_base * size_multiplier,
            workload_cost_base * size_multiplier * performance_multiplier,
            workload_duration_base * performance_multiplier,
        )
    }
}

/// Individual analyzer configurations (simplified from the complex hierarchy)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowEstimationConfig {
    pub enabled_findings: Vec<FindingType>,
    pub thresholds: SmartThresholds,
}

impl RowEstimationConfig {
    pub fn for_workload(workload: &WorkloadContext) -> Self {
        Self {
            enabled_findings: vec![
                FindingType::ExcessiveRowProcessing,
                FindingType::RowEstimationError,
                FindingType::CartesianProduct,
            ],
            thresholds: SmartThresholds::for_workload(workload),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanAnalysisConfig {
    pub enabled_findings: Vec<FindingType>,
    pub thresholds: SmartThresholds,
}

impl ScanAnalysisConfig {
    pub fn for_workload(workload: &WorkloadContext) -> Self {
        Self {
            enabled_findings: vec![
                FindingType::LargeSequentialScan,
                FindingType::InefficiientScan,
                FindingType::MissingIndex,
                FindingType::PoorIndexSelectivity,
            ],
            thresholds: SmartThresholds::for_workload(workload),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JoinAnalysisConfig {
    pub enabled_findings: Vec<FindingType>,
    pub thresholds: SmartThresholds,
}

impl JoinAnalysisConfig {
    pub fn for_workload(workload: &WorkloadContext) -> Self {
        Self {
            enabled_findings: vec![
                FindingType::IneffectiveJoinAlgorithm,
                FindingType::LargeNestedLoop,
                FindingType::HashJoinMemorySpill,
            ],
            thresholds: SmartThresholds::for_workload(workload),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostAnalysisConfig {
    pub enabled_findings: Vec<FindingType>,
    pub thresholds: SmartThresholds,
    pub startup_ratio_threshold: f64,
    pub duration_correlation_enabled: bool,
}

impl CostAnalysisConfig {
    pub fn for_workload(workload: &WorkloadContext) -> Self {
        Self {
            enabled_findings: vec![
                FindingType::HighStartupCost,
                FindingType::ExpensiveOperation,
                FindingType::HighCostVariability,
            ],
            thresholds: SmartThresholds::for_workload(workload),
            startup_ratio_threshold: 0.5, // 50% of total cost for startup
            duration_correlation_enabled: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryAnalysisConfig {
    pub enabled_findings: Vec<FindingType>,
    pub thresholds: SmartThresholds,
}

impl MemoryAnalysisConfig {
    pub fn for_workload(workload: &WorkloadContext) -> Self {
        Self {
            enabled_findings: vec![
                FindingType::MemorySpill,
                FindingType::LargeSort,
                FindingType::LargeAggregation,
            ],
            thresholds: SmartThresholds::for_workload(workload),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParallelizationConfig {
    pub enabled_findings: Vec<FindingType>,
    pub thresholds: SmartThresholds,
}

impl ParallelizationConfig {
    pub fn for_workload(workload: &WorkloadContext) -> Self {
        Self {
            enabled_findings: vec![
                FindingType::InefficientParallelism,
                FindingType::MissedParallelization,
            ],
            thresholds: SmartThresholds::for_workload(workload),
        }
    }
}

/// Custom threshold overrides for advanced users
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomThresholds {
    pub overrides: HashMap<String, SmartThresholds>,
}

/// Configuration builders for common scenarios
pub struct ConfigurationBuilder;

impl ConfigurationBuilder {
    /// High-performance OLTP configuration
    pub fn high_performance_oltp() -> AnalysisConfiguration {
        let workload = WorkloadContext {
            workload_type: WorkloadType::OLTP,
            database_size: DatabaseSize::Large,
            performance_target: PerformanceTarget::Latency,
            postgres_version: "15.0".to_string(),
        };
        
        AnalysisConfiguration {
            global: GlobalSettings {
                min_severity: Severity::Low, // Catch everything for OLTP
                max_findings_per_analyzer: 100,
                include_evidence: true,
                generate_suggestions: true,
            },
            workload: workload.clone(),
            analyzers: AnalyzerConfigurations::for_workload(&workload),
            custom_thresholds: None,
        }
    }
    
    /// Large-scale analytics configuration
    pub fn analytics_warehouse() -> AnalysisConfiguration {
        let workload = WorkloadContext {
            workload_type: WorkloadType::OLAP,
            database_size: DatabaseSize::VeryLarge,
            performance_target: PerformanceTarget::Throughput,
            postgres_version: "15.0".to_string(),
        };
        
        AnalysisConfiguration {
            global: GlobalSettings {
                min_severity: Severity::Medium, // Focus on significant issues
                max_findings_per_analyzer: 25,
                include_evidence: true,
                generate_suggestions: true,
            },
            workload: workload.clone(),
            analyzers: AnalyzerConfigurations::for_workload(&workload),
            custom_thresholds: None,
        }
    }
    
    /// Development environment configuration (more sensitive)
    pub fn development_environment() -> AnalysisConfiguration {
        let workload = WorkloadContext {
            workload_type: WorkloadType::Mixed,
            database_size: DatabaseSize::Small,
            performance_target: PerformanceTarget::Balanced,
            postgres_version: "14.0".to_string(),
        };
        
        AnalysisConfiguration {
            global: GlobalSettings {
                min_severity: Severity::Low, // Show all issues in development
                max_findings_per_analyzer: 200,
                include_evidence: true,
                generate_suggestions: true,
            },
            workload: workload.clone(),
            analyzers: AnalyzerConfigurations::for_workload(&workload),
            custom_thresholds: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_default_configuration() {
        let config = AnalysisConfiguration::default();
        assert_eq!(config.global.min_severity, Severity::Low);
        assert_eq!(config.workload.workload_type, WorkloadType::Mixed);
        assert_eq!(config.workload.database_size, DatabaseSize::Medium);
        assert!(!config.analyzers.row_estimation.enabled_findings.is_empty());
    }
    
    #[test]
    fn test_workload_specific_thresholds() {
        let oltp_workload = WorkloadContext {
            workload_type: WorkloadType::OLTP,
            database_size: DatabaseSize::Small,
            performance_target: PerformanceTarget::Latency,
            postgres_version: "15.0".to_string(),
        };
        
        let olap_workload = WorkloadContext {
            workload_type: WorkloadType::OLAP,
            database_size: DatabaseSize::Large,
            performance_target: PerformanceTarget::Throughput,
            postgres_version: "15.0".to_string(),
        };
        
        let oltp_thresholds = SmartThresholds::for_workload(&oltp_workload);
        let olap_thresholds = SmartThresholds::for_workload(&olap_workload);
        
        // OLTP should have stricter (lower) thresholds than OLAP
        assert!(oltp_thresholds.row_counts.high < olap_thresholds.row_counts.high);
        assert!(oltp_thresholds.durations.high < olap_thresholds.durations.high);
    }
    
    #[test]
    fn test_threshold_classification() {
        let workload = WorkloadContext::default();
        let thresholds = SmartThresholds::for_workload(&workload);
        
        assert_eq!(thresholds.row_counts.classify(&thresholds.row_counts.low), Severity::Low);
        assert_eq!(thresholds.row_counts.classify(&thresholds.row_counts.critical), Severity::Critical);
    }
    
    #[test]
    fn test_configuration_builders() {
        let oltp_config = ConfigurationBuilder::high_performance_oltp();
        let analytics_config = ConfigurationBuilder::analytics_warehouse();
        let dev_config = ConfigurationBuilder::development_environment();
        
        // OLTP should be more sensitive (lower min severity)
        assert_eq!(oltp_config.global.min_severity, Severity::Low);
        assert_eq!(analytics_config.global.min_severity, Severity::Medium);
        assert_eq!(dev_config.global.min_severity, Severity::Low);
        
        // Different workload types
        assert_eq!(oltp_config.workload.workload_type, WorkloadType::OLTP);
        assert_eq!(analytics_config.workload.workload_type, WorkloadType::OLAP);
        assert_eq!(dev_config.workload.workload_type, WorkloadType::Mixed);
    }
}