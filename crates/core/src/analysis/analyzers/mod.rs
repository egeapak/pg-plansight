// Phase 1: Core analyzers
pub mod cost_analysis;
pub mod join_analysis;
pub mod memory_analysis;
pub mod parallelization;
pub mod row_estimation;
pub mod scan_analysis;

// Phase 2: Enhanced analyzers
pub mod query_pattern_analysis;
pub mod resource_analysis;
pub mod temporal_analysis;

// Phase 3: Advanced analyzers
pub mod data_distribution;
pub mod index_effectiveness;
pub mod plan_stability;

// Re-export all analyzers for convenience
pub use cost_analysis::CostAnalyzer;
pub use data_distribution::DataDistributionAnalyzer;
pub use index_effectiveness::IndexEffectivenessAnalyzer;
pub use join_analysis::JoinAnalyzer;
pub use memory_analysis::MemoryAnalyzer;
pub use parallelization::ParallelizationAnalyzer;
pub use plan_stability::PlanStabilityAnalyzer;
pub use query_pattern_analysis::QueryPatternAnalyzer;
pub use resource_analysis::ResourceAnalyzer;
pub use row_estimation::RowEstimationAnalyzer;
pub use scan_analysis::ScanAnalyzer;
pub use temporal_analysis::TemporalAnalyzer;
