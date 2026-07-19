//! JSON plan parser implementation
//!
//! Handles parsing of PostgreSQL JSON format execution plans

use crate::parsing::errors::{ParseError, ParseResult};
use crate::parsing::parser_trait::{
    ParseMetadata, ParsedPlanResult, PlanParser, PlanParserCore, PlanSourceFormat,
};
use crate::{JsonPlan, ParsedPlan};

/// Parser for JSON format PostgreSQL execution plans
pub struct JsonPlanParser;

impl JsonPlanParser {
    /// Create a new JSON plan parser
    pub fn new() -> Self {
        Self
    }

    /// Check if content looks like JSON format
    fn is_json_format(input: &str) -> bool {
        let trimmed = input.trim_start();
        trimmed.starts_with('[') || trimmed.starts_with('{')
    }

    /// Validate and parse JSON content.
    ///
    /// Accepts both shapes PostgreSQL produces: EXPLAIN (FORMAT JSON) emits an
    /// ARRAY of plan documents, while auto_explain.log_format=json emits a
    /// single top-level OBJECT.
    fn parse_json_content(input: &str) -> ParseResult<JsonPlan> {
        let value: serde_json::Value =
            serde_json::from_str(input).map_err(|e| ParseError::InvalidJsonFormat {
                message: "Input is not valid JSON".to_string(),
                json_error: e.to_string(),
            })?;

        let first = match value {
            serde_json::Value::Array(items) => {
                items
                    .into_iter()
                    .next()
                    .ok_or_else(|| ParseError::MissingJsonPlanData {
                        message: "JSON plan array is empty".to_string(),
                        field: "Plan".to_string(),
                    })?
            }
            object @ serde_json::Value::Object(_) => object,
            _ => {
                return Err(ParseError::InvalidJsonFormat {
                    message: "JSON does not match PostgreSQL plan schema".to_string(),
                    json_error: "expected a JSON array or object".to_string(),
                });
            }
        };

        serde_json::from_value(first).map_err(|e| ParseError::InvalidJsonFormat {
            message: "JSON does not match PostgreSQL plan schema".to_string(),
            json_error: e.to_string(),
        })
    }
}

impl Default for JsonPlanParser {
    fn default() -> Self {
        Self::new()
    }
}

impl PlanParserCore for JsonPlanParser {
    fn can_parse(&self, input: &str) -> bool {
        Self::is_json_format(input)
    }

    fn parse(&self, input: &str, _metadata: ParseMetadata) -> ParseResult<ParsedPlanResult> {
        if !self.can_parse(input) {
            return Err(ParseError::FormatDetectionError {
                message: "Input does not appear to be JSON format".to_string(),
                content_preview: input.chars().take(100).collect(),
            });
        }

        // Parse the JSON content
        let _json_plan = Self::parse_json_content(input)?;

        // Create ParsedPlan from JSON
        let parsed_plan =
            ParsedPlan::from_json_plan(input).map_err(|e| ParseError::InvalidJsonFormat {
                message: "Failed to convert JSON to ParsedPlan".to_string(),
                json_error: format!("{:?}", e),
            })?;

        Ok(ParsedPlanResult {
            parsed_plan,
            source_format: PlanSourceFormat::Json,
            warnings: vec![],
        })
    }

    fn format_name(&self) -> &'static str {
        "json"
    }

    fn priority(&self) -> u8 {
        120 // Higher priority than text parser since JSON is more structured
    }

    fn description(&self) -> &'static str {
        "PostgreSQL JSON format execution plan parser"
    }
}

impl PlanParser for JsonPlanParser {
    type Builder = crate::parsing::JsonPlanBuilder;
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn test_json_format_detection() {
        let parser = JsonPlanParser::new();

        assert!(parser.can_parse(r#"[{"Plan": {}}]"#));
        assert!(parser.can_parse(r#"{"Plan": {}}"#));
        assert!(parser.can_parse("  [{}]  ")); // With whitespace

        assert!(!parser.can_parse("Seq Scan on users"));
        assert!(!parser.can_parse(""));
    }

    #[test]
    fn test_parser_properties() {
        let parser = JsonPlanParser::new();

        assert_eq!(parser.format_name(), "json");
        assert_eq!(parser.priority(), 120);
        assert!(parser.description().contains("JSON"));
    }

    #[test]
    fn test_parse_simple_json_plan() {
        let parser = JsonPlanParser::new();

        let json_content = r#"[{
            "Plan": {
                "Node Type": "Seq Scan",
                "Relation Name": "users",
                "Startup Cost": 0.0,
                "Total Cost": 10.0,
                "Plan Rows": 100,
                "Plan Width": 8
            }
        }]"#;

        let metadata = ParseMetadata::new(Utc::now(), 150.0, "SELECT * FROM users".to_string());

        let result = parser.parse(json_content, metadata);
        assert!(result.is_ok());

        let parsed_result = result.unwrap();
        assert_eq!(parsed_result.source_format, PlanSourceFormat::Json);
        assert!(parsed_result.warnings.is_empty());
    }

    #[test]
    fn test_parse_invalid_json() {
        let parser = JsonPlanParser::new();

        let invalid_json = "[{invalid json}]";
        let metadata = ParseMetadata::new(Utc::now(), 100.0, "SELECT 1".to_string());

        let result = parser.parse(invalid_json, metadata);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::InvalidJsonFormat { .. }
        ));
    }

    #[test]
    fn test_parse_empty_json_array() {
        let parser = JsonPlanParser::new();

        let empty_json = "[]";
        let metadata = ParseMetadata::new(Utc::now(), 100.0, "SELECT 1".to_string());

        let result = parser.parse(empty_json, metadata);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingJsonPlanData { .. }
        ));
    }

    #[test]
    fn test_parse_non_json_input() {
        let parser = JsonPlanParser::new();

        let text_input = "Seq Scan on users";
        let metadata = ParseMetadata::new(Utc::now(), 100.0, "SELECT * FROM users".to_string());

        let result = parser.parse(text_input, metadata);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::FormatDetectionError { .. }
        ));
    }
}
