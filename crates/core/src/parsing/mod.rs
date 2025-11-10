//! Parsing module for PostgreSQL execution plans
//!
//! This module separates parsing logic from data models, providing
//! dedicated parsers for different plan formats.

pub mod errors;
pub mod factory;
pub mod format_detection;
pub mod json_parser;
pub mod log_entry_parser;
pub mod parser_trait;
pub mod plan_builders;
pub mod text_parser;

pub use errors::{ParseError, ParseResult};
pub use factory::PlanFactory;
pub use format_detection::{PlanFormat, detect_plan_format};
pub use json_parser::JsonPlanParser;
pub use log_entry_parser::{LogEntryParser, LogParsingState};
pub use parser_trait::{
    ParseMetadata, ParsedPlanResult, PlanParser, PlanParserCore, PlanSourceFormat,
};
pub use plan_builders::{JsonPlanBuilder, QueryPlanBuilder, TextPlanBuilder, UntypedPlanBuilder};
pub use text_parser::TextPlanParser;
