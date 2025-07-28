pub mod analysis;
pub mod log_parser;
pub mod models;
pub mod parser_utils;
pub mod plan_parser;
pub mod plan_properties;
pub mod parsing;
pub mod sql_analysis;

#[cfg(test)]
mod parsing_debug;

pub use analysis::*;
pub use log_parser::*;
pub use models::*;
pub use parser_utils::*;
pub use plan_parser::*;
pub use plan_properties::*;
// Re-export specific items from parsing to avoid conflicts
pub use parsing::{PlanFactory, LogEntryParser, detect_plan_format};
// Re-export new SQL analysis functionality
pub use sql_analysis::{
    QueryNormalizer, NormalizationConfig, NormalizationResult, 
    LiteralInfo, LiteralType, normalize_query_enhanced, calculate_query_fingerprint
};
