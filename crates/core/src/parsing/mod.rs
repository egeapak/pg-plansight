//! Parsing module for PostgreSQL execution plans
//! 
//! This module separates parsing logic from data models, providing
//! dedicated parsers for different plan formats.

pub mod factory;
pub mod format_detection;
pub mod log_entry_parser;
pub mod plan_builders;
pub mod errors;

pub use factory::PlanFactory;
pub use format_detection::{PlanFormat, detect_plan_format};
pub use log_entry_parser::{LogEntryParser, LogParsingState};
pub use plan_builders::{QueryPlanBuilder, TextPlanBuilder, JsonPlanBuilder, UntypedPlanBuilder};
pub use errors::{ParseError, ParseResult};