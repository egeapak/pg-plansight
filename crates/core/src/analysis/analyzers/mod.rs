// Core analyzers - reliable and useful
pub mod buffer_analysis;
pub mod join_analysis;
pub mod parallelization;
pub mod row_estimation;
pub mod scan_analysis;

// Enhanced analyzers
pub mod query_pattern_analysis;

// New reliable analyzers (replacing flaky ones)
pub mod index_usage;
pub mod startup_cost;

// Enhancement analyzers (execution-property driven)
pub mod estimation_health;
pub mod filter_efficiency;
pub mod index_efficiency;
pub mod plan_shape;
pub mod sort_memory;

// Re-export all analyzers for convenience
pub use buffer_analysis::BufferWalAnalyzer;
pub use estimation_health::EstimationHealthAnalyzer;
pub use filter_efficiency::FilterEfficiencyAnalyzer;
pub use index_efficiency::IndexEfficiencyAnalyzer;
pub use index_usage::IndexUsageAnalyzer;
pub use join_analysis::JoinAnalyzer;
pub use parallelization::ParallelizationAnalyzer;
pub use plan_shape::PlanShapeAnalyzer;
pub use query_pattern_analysis::QueryPatternAnalyzer;
pub use row_estimation::RowEstimationAnalyzer;
pub use scan_analysis::ScanAnalyzer;
pub use sort_memory::SortMemoryAnalyzer;
pub use startup_cost::StartupCostAnalyzer;
