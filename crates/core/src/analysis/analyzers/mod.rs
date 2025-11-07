// Phase 1: Core analyzers
pub mod row_estimation;
pub mod scan_analysis;
pub mod join_analysis;
pub mod cost_analysis;
pub mod memory_analysis;
pub mod parallelization;

// Phase 2: Enhanced analyzers
pub mod temporal_analysis;
pub mod resource_analysis;
pub mod query_pattern_analysis;

// Phase 3: Advanced analyzers
pub mod index_effectiveness;
pub mod data_distribution;

// Re-export all analyzers for convenience
pub use row_estimation::RowEstimationAnalyzer;
pub use scan_analysis::ScanAnalyzer;
pub use join_analysis::JoinAnalyzer;
pub use cost_analysis::CostAnalyzer;
pub use memory_analysis::MemoryAnalyzer;
pub use parallelization::ParallelizationAnalyzer;
pub use temporal_analysis::TemporalAnalyzer;
pub use resource_analysis::ResourceAnalyzer;
pub use query_pattern_analysis::QueryPatternAnalyzer;
pub use index_effectiveness::IndexEffectivenessAnalyzer;
pub use data_distribution::DataDistributionAnalyzer;