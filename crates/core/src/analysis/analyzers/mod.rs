// Core analyzers - reliable and useful
pub mod join_analysis;
pub mod parallelization;
pub mod row_estimation;
pub mod scan_analysis;

// Enhanced analyzers
pub mod query_pattern_analysis;

// New reliable analyzers (replacing flaky ones)
pub mod index_usage;
pub mod startup_cost;

// Re-export all analyzers for convenience
pub use index_usage::IndexUsageAnalyzer;
pub use join_analysis::JoinAnalyzer;
pub use parallelization::ParallelizationAnalyzer;
pub use query_pattern_analysis::QueryPatternAnalyzer;
pub use row_estimation::RowEstimationAnalyzer;
pub use scan_analysis::ScanAnalyzer;
pub use startup_cost::StartupCostAnalyzer;
