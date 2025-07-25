//! Unified plan parser interface
//! 
//! This module defines the trait-based interface for parsing different
//! PostgreSQL plan formats, providing a clean abstraction for extensibility.

use chrono::{DateTime, Utc};
use crate::{ParsedPlan};
use crate::parsing::errors::{ParseResult};

/// Metadata passed to parsers for context
#[derive(Debug, Clone)]
pub struct ParseMetadata {
    /// When the query was executed
    pub timestamp: DateTime<Utc>,
    /// Query execution duration in milliseconds  
    pub duration_ms: f64,
    /// The original SQL query text
    pub query_text: String,
    /// Optional additional context for debugging
    pub context: Option<String>,
}

impl ParseMetadata {
    pub fn new(timestamp: DateTime<Utc>, duration_ms: f64, query_text: String) -> Self {
        Self {
            timestamp,
            duration_ms,
            query_text,
            context: None,
        }
    }

    pub fn with_context(mut self, context: String) -> Self {
        self.context = Some(context);
        self
    }
}

/// Result of parsing a plan with format information
#[derive(Debug, Clone)]
pub struct ParsedPlanResult {
    /// The parsed plan structure
    pub parsed_plan: ParsedPlan,
    /// Source format that was detected/used
    pub source_format: PlanSourceFormat,
    /// Any warnings generated during parsing
    pub warnings: Vec<String>,
}

/// Enum representing the source format of a plan
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanSourceFormat {
    Text,
    Json,
}

/// Base trait for plan parsers (trait object compatible)
/// 
/// This trait provides the core parsing functionality without associated types,
/// making it compatible with trait objects for dynamic dispatch.
pub trait PlanParserCore: Send + Sync {
    /// Check if this parser can handle the given input
    /// 
    /// This method should be fast and only do basic format detection
    /// without full parsing.
    fn can_parse(&self, input: &str) -> bool;

    /// Parse the input into a structured plan
    /// 
    /// This method does the actual parsing work and should provide
    /// detailed error information if parsing fails.
    fn parse(&self, input: &str, metadata: ParseMetadata) -> ParseResult<ParsedPlanResult>;

    /// Get a human-readable name for this parser
    fn format_name(&self) -> &'static str;

    /// Get the priority of this parser for format detection
    /// 
    /// Higher numbers = higher priority. Used when multiple parsers
    /// claim they can parse the same input.
    fn priority(&self) -> u8 {
        100 // Default priority
    }

    /// Get additional information about this parser
    fn description(&self) -> &'static str {
        "PostgreSQL plan parser"
    }
}

/// Extended trait for parsing PostgreSQL execution plans with builder coupling
/// 
/// This trait extends PlanParserCore with associated types for tight coupling
/// between builders and their parsers. Use this for direct parser usage in builders.
pub trait PlanParser: PlanParserCore {
    /// The builder type that should use this parser
    /// This creates a tight coupling between builders and their parsers
    type Builder;
}


#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    // Mock parser for testing
    struct MockParser {
        name: &'static str,
        can_parse_result: bool,
    }

    impl PlanParserCore for MockParser {
        fn can_parse(&self, _input: &str) -> bool {
            self.can_parse_result
        }

        fn parse(&self, _input: &str, _metadata: ParseMetadata) -> ParseResult<ParsedPlanResult> {
            Err(ParseError::EmptyInput {
                expected: "mock parser test".to_string(),
            })
        }

        fn format_name(&self) -> &'static str {
            self.name
        }
    }

    impl PlanParser for MockParser {
        type Builder = (); // Mock builder type
    }

    #[test]
    fn test_parse_metadata_creation() {
        let timestamp = Utc::now();
        let metadata = ParseMetadata::new(
            timestamp,
            123.45,
            "SELECT * FROM users".to_string(),
        );

        assert_eq!(metadata.timestamp, timestamp);
        assert_eq!(metadata.duration_ms, 123.45);
        assert_eq!(metadata.query_text, "SELECT * FROM users");
        assert!(metadata.context.is_none());
    }

    #[test]
    fn test_parse_metadata_with_context() {
        let metadata = ParseMetadata::new(
            Utc::now(),
            100.0,
            "SELECT 1".to_string(),
        ).with_context("test context".to_string());

        assert_eq!(metadata.context, Some("test context".to_string()));
    }

    #[test]
    fn test_mock_parser() {
        let parser = MockParser {
            name: "test_parser",
            can_parse_result: true,
        };

        assert!(parser.can_parse("test input"));
        assert_eq!(parser.format_name(), "test_parser");
        assert_eq!(parser.priority(), 100); // Default priority
    }
}