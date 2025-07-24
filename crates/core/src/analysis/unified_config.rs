/// Unified Analysis Configuration System
/// 
/// This module provides a standardized approach to analysis thresholds across all analyzers,
/// ensuring consistent severity assignments and scale-appropriate thresholds.

use serde::{Deserialize, Serialize};

/// Operation types that have different natural scales
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OperationType {
    /// Sequential and index scans
    Scan,
    /// All join operations (nested loop, hash, merge)
    Join,
    /// Sort operations
    Sort,
    /// Hash operations
    Hash,
    /// Grouping and aggregation
    Aggregate,
    /// Index-specific operations
    Index,
    /// Memory-intensive operations
    Memory,
}

/// Database size categories for context-aware thresholds
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DatabaseSize {
    Small,    // < 1GB
    Medium,   // 1GB - 100GB  
    Large,    // 100GB - 1TB
    VeryLarge, // > 1TB
}

/// Workload type affects what's considered normal
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkloadType {
    OLTP,      // Online Transaction Processing
    OLAP,      // Online Analytical Processing  
    Mixed,     // Mixed workload
    Reporting, // Reporting/BI workload
}

/// Performance targets affect severity thresholds
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PerformanceTarget {
    Interactive, // < 100ms target
    Fast,        // < 1s target
    Batch,       // < 10s target
    Background,  // No strict target
}

/// Unified row count thresholds by operation type
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowCountThresholds {
    pub critical: u64,
    pub high: u64,
    pub medium: u64,
    pub low: u64,
}

impl RowCountThresholds {
    pub fn for_operation(op_type: OperationType, db_size: DatabaseSize) -> Self {
        let base_multiplier = match db_size {
            DatabaseSize::Small => 0.1,
            DatabaseSize::Medium => 1.0,
            DatabaseSize::Large => 5.0,
            DatabaseSize::VeryLarge => 10.0,
        };
        
        let (crit_base, high_base, med_base, low_base) = match op_type {
            OperationType::Scan => (10_000_000, 1_000_000, 100_000, 10_000),
            OperationType::Join => (5_000_000, 500_000, 50_000, 5_000),
            OperationType::Sort | OperationType::Hash => (2_000_000, 200_000, 20_000, 2_000),
            OperationType::Aggregate => (10_000_000, 1_000_000, 100_000, 10_000),
            OperationType::Index => (1_000_000, 100_000, 10_000, 1_000),
            OperationType::Memory => (5_000_000, 500_000, 50_000, 5_000),
        };
        
        Self {
            critical: (crit_base as f64 * base_multiplier) as u64,
            high: (high_base as f64 * base_multiplier) as u64,
            medium: (med_base as f64 * base_multiplier) as u64,
            low: (low_base as f64 * base_multiplier) as u64,
        }
    }
    
    pub fn classify_severity(&self, row_count: u64) -> super::Severity {
        if row_count >= self.critical {
            super::Severity::Critical
        } else if row_count >= self.high {
            super::Severity::High
        } else if row_count >= self.medium {
            super::Severity::Medium
        } else if row_count >= self.low {
            super::Severity::Low
        } else {
            super::Severity::Low // Below threshold
        }
    }
}

/// Unified cost thresholds (PostgreSQL cost units)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostThresholds {
    pub extreme: f64,
    pub high: f64,
    pub medium: f64,
    pub low: f64,
}

impl CostThresholds {
    pub fn for_operation(op_type: OperationType, target: PerformanceTarget) -> Self {
        let target_multiplier = match target {
            PerformanceTarget::Interactive => 0.1,
            PerformanceTarget::Fast => 1.0,
            PerformanceTarget::Batch => 10.0,
            PerformanceTarget::Background => 100.0,
        };
        
        let (ext_base, high_base, med_base, low_base) = match op_type {
            OperationType::Scan => (1_000_000.0, 100_000.0, 10_000.0, 1_000.0),
            OperationType::Join => (500_000.0, 50_000.0, 5_000.0, 500.0),
            OperationType::Sort | OperationType::Hash => (200_000.0, 20_000.0, 2_000.0, 200.0),
            OperationType::Aggregate => (1_000_000.0, 100_000.0, 10_000.0, 1_000.0),
            OperationType::Index => (50_000.0, 5_000.0, 500.0, 50.0),
            OperationType::Memory => (200_000.0, 20_000.0, 2_000.0, 200.0),
        };
        
        Self {
            extreme: ext_base * target_multiplier,
            high: high_base * target_multiplier,
            medium: med_base * target_multiplier,
            low: low_base * target_multiplier,
        }
    }
    
    pub fn classify_severity(&self, cost: f64) -> super::Severity {
        if cost >= self.extreme {
            super::Severity::Critical
        } else if cost >= self.high {
            super::Severity::High
        } else if cost >= self.medium {
            super::Severity::Medium
        } else if cost >= self.low {
            super::Severity::Low
        } else {
            super::Severity::Low // Below threshold
        }
    }
}

/// Unified ratio thresholds for various performance ratios
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RatioThresholds {
    pub severe: f64,     // > 100x error
    pub high: f64,       // 10-100x error
    pub medium: f64,     // 3-10x error  
    pub low: f64,        // 1.5-3x error
}

impl RatioThresholds {
    /// For error ratios (estimation errors, etc.)
    pub fn error_ratios() -> Self {
        Self {
            severe: 100.0,
            high: 10.0,
            medium: 3.0,
            low: 1.5,
        }
    }
    
    /// For memory spill ratios (relative to work_mem)
    pub fn memory_ratios() -> Self {
        Self {
            severe: 10.0,
            high: 3.0,
            medium: 1.5,
            low: 1.2,
        }
    }
    
    /// For efficiency ratios (selectivity, parallelism efficiency)
    pub fn efficiency_ratios() -> Self {
        Self {
            severe: 0.05,  // < 5% efficiency
            high: 0.20,    // < 20% efficiency
            medium: 0.50,  // < 50% efficiency  
            low: 0.80,     // < 80% efficiency
        }
    }
    
    pub fn classify_severity_above(&self, ratio: f64) -> super::Severity {
        if ratio >= self.severe {
            super::Severity::Critical
        } else if ratio >= self.high {
            super::Severity::High
        } else if ratio >= self.medium {
            super::Severity::Medium
        } else if ratio >= self.low {
            super::Severity::Low
        } else {
            super::Severity::Low
        }
    }
    
    pub fn classify_severity_below(&self, ratio: f64) -> super::Severity {
        if ratio <= self.severe {
            super::Severity::Critical
        } else if ratio <= self.high {
            super::Severity::High
        } else if ratio <= self.medium {
            super::Severity::Medium
        } else if ratio <= self.low {
            super::Severity::Low
        } else {
            super::Severity::Low
        }
    }
}

/// Duration thresholds for time-based analysis
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DurationThresholds {
    pub critical_ms: f64,
    pub high_ms: f64,
    pub medium_ms: f64,
    pub low_ms: f64,
}

impl DurationThresholds {
    pub fn for_target(target: PerformanceTarget) -> Self {
        match target {
            PerformanceTarget::Interactive => Self {
                critical_ms: 1000.0,   // > 1s is critical for interactive
                high_ms: 500.0,        // > 500ms is high
                medium_ms: 100.0,      // > 100ms is medium
                low_ms: 50.0,          // > 50ms is low
            },
            PerformanceTarget::Fast => Self {
                critical_ms: 10_000.0, // > 10s is critical  
                high_ms: 5_000.0,      // > 5s is high
                medium_ms: 1_000.0,    // > 1s is medium
                low_ms: 500.0,         // > 500ms is low
            },
            PerformanceTarget::Batch => Self {
                critical_ms: 300_000.0, // > 5min is critical
                high_ms: 60_000.0,      // > 1min is high
                medium_ms: 10_000.0,    // > 10s is medium
                low_ms: 5_000.0,        // > 5s is low
            },
            PerformanceTarget::Background => Self {
                critical_ms: 3_600_000.0, // > 1hr is critical
                high_ms: 600_000.0,       // > 10min is high
                medium_ms: 60_000.0,      // > 1min is medium
                low_ms: 10_000.0,         // > 10s is low
            },
        }
    }
    
    pub fn classify_severity(&self, duration_ms: f64) -> super::Severity {
        if duration_ms >= self.critical_ms {
            super::Severity::Critical
        } else if duration_ms >= self.high_ms {
            super::Severity::High
        } else if duration_ms >= self.medium_ms {
            super::Severity::Medium
        } else if duration_ms >= self.low_ms {
            super::Severity::Low
        } else {
            super::Severity::Low
        }
    }
}

/// Unified analysis context for all analyzers
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnifiedAnalysisContext {
    pub database_size: DatabaseSize,
    pub workload_type: WorkloadType,
    pub performance_target: PerformanceTarget,
}

impl Default for UnifiedAnalysisContext {
    fn default() -> Self {
        Self {
            database_size: DatabaseSize::Medium,
            workload_type: WorkloadType::Mixed,
            performance_target: PerformanceTarget::Fast,
        }
    }
}

/// Unified thresholds for a specific context
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnifiedThresholds {
    pub row_count: RowCountThresholds,
    pub cost: CostThresholds,
    pub error_ratios: RatioThresholds,
    pub memory_ratios: RatioThresholds,
    pub efficiency_ratios: RatioThresholds,
    pub duration: DurationThresholds,
}

impl UnifiedThresholds {
    pub fn for_operation(
        op_type: OperationType,
        context: &UnifiedAnalysisContext,
    ) -> Self {
        Self {
            row_count: RowCountThresholds::for_operation(op_type, context.database_size),
            cost: CostThresholds::for_operation(op_type, context.performance_target),
            error_ratios: RatioThresholds::error_ratios(),
            memory_ratios: RatioThresholds::memory_ratios(),
            efficiency_ratios: RatioThresholds::efficiency_ratios(),
            duration: DurationThresholds::for_target(context.performance_target),
        }
    }
}

/// Helper trait for unified threshold application
pub trait UnifiedAnalysis {
    fn get_operation_type(&self) -> OperationType;
    
    fn get_unified_thresholds(&self, context: &UnifiedAnalysisContext) -> UnifiedThresholds {
        UnifiedThresholds::for_operation(self.get_operation_type(), context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_row_count_thresholds() {
        let thresholds = RowCountThresholds::for_operation(
            OperationType::Scan,
            DatabaseSize::Medium,
        );
        
        assert_eq!(thresholds.critical, 10_000_000);
        assert_eq!(thresholds.high, 1_000_000);
        assert_eq!(thresholds.medium, 100_000);
        assert_eq!(thresholds.low, 10_000);
    }
    
    #[test]
    fn test_severity_classification() {
        let thresholds = RowCountThresholds::for_operation(
            OperationType::Join,
            DatabaseSize::Small,
        );
        
        assert_eq!(thresholds.classify_severity(1_000_000), super::Severity::Critical);
        assert_eq!(thresholds.classify_severity(100_000), super::Severity::High);
        assert_eq!(thresholds.classify_severity(10_000), super::Severity::Medium);
        assert_eq!(thresholds.classify_severity(1_000), super::Severity::Low);
    }
    
    #[test]
    fn test_cost_thresholds_by_target() {
        let interactive = CostThresholds::for_operation(
            OperationType::Scan,
            PerformanceTarget::Interactive,
        );
        let batch = CostThresholds::for_operation(
            OperationType::Scan,
            PerformanceTarget::Batch,
        );
        
        assert!(batch.extreme > interactive.extreme);
        assert!(batch.high > interactive.high);
    }
    
    #[test]
    fn test_ratio_thresholds() {
        let error_ratios = RatioThresholds::error_ratios();
        
        assert_eq!(error_ratios.classify_severity_above(50.0), super::Severity::High);
        assert_eq!(error_ratios.classify_severity_above(5.0), super::Severity::Medium);
        
        let efficiency_ratios = RatioThresholds::efficiency_ratios();
        assert_eq!(efficiency_ratios.classify_severity_below(0.1), super::Severity::High);
    }
}