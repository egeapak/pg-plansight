pub mod analysis;
pub mod capture;
pub mod export;
pub mod grouping;
pub mod log_parser;
pub mod models;
pub mod parser_utils;
pub mod parsing;
pub mod plan_parser;
pub mod plan_properties;
pub mod simd_scan;
pub mod sql_analysis;

#[cfg(test)]
mod parsing_debug;

pub use analysis::*;
pub use capture::*;
pub use export::*;
pub use grouping::{FingerprintCache, QueryGrouper};
pub use log_parser::*;
pub use models::*;
pub use parser_utils::*;
pub use plan_parser::*;
pub use plan_properties::*;
// Re-export specific items from parsing to avoid conflicts
pub use parsing::{LogEntryParser, PlanFactory, detect_plan_format};
// Re-export new SQL analysis functionality
pub use sql_analysis::{
    AntiPattern, AntiPatternAnalyzer, AntiPatternKind, AntiPatternSeverity, LiteralInfo,
    LiteralType, NormalizationResult, QueryNormalizer, calculate_query_fingerprint,
    normalize_query_enhanced,
};
// Re-export configuration types directly
pub use analysis::consolidated_config::{
    ComplexityAnalysisConfig, MetadataExtractionConfig, NormalizationConfig,
    RegressionDetectionConfig, RegressionThresholds,
};
