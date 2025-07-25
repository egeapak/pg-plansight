//! Factory for creating QueryPlan instances
//! 
//! This factory separates the complex logic for creating QueryPlan objects
//! from the data model itself, making the code more maintainable and testable.

use chrono::{DateTime, Utc};
use crate::{QueryPlan, PlanSource, JsonPlan, PlanLine};
use crate::parser_utils::{normalize_query, format_sql_query};
use crate::parsing::errors::{ParseError, ParseResult};
use crate::parsing::parser_trait::{ParseMetadata, PlanSourceFormat, ParsedPlanResult, PlanParserCore};

pub struct PlanFactory;

impl PlanFactory {
    /// Creates a QueryPlan from pre-parsed components (preferred method)
    /// This avoids double parsing since builders already know their format
    pub fn create_query_plan_from_parsed(
        timestamp: DateTime<Utc>,
        duration_ms: f64,
        query_text: String,
        raw_plan: String,
        parsed_result: ParsedPlanResult,
    ) -> ParseResult<QueryPlan> {
        // Process query text
        let regex = regex::Regex::new(r"\$\d+").unwrap();
        let normalized_query = normalize_query(&query_text, &regex).into_owned();
        let formatted_query = format_sql_query(&query_text);

        // Create appropriate PlanSource based on the detected format
        let source = match parsed_result.source_format {
            PlanSourceFormat::Json => {
                // Parse JSON to get the structured data for PlanSource
                let json_plans: Vec<JsonPlan> = serde_json::from_str(&raw_plan)
                    .map_err(|e| ParseError::InvalidJsonFormat {
                        message: "Failed to parse JSON for PlanSource".to_string(),
                        json_error: e.to_string(),
                    })?;
                    
                let parsed_json = json_plans.into_iter().next()
                    .ok_or_else(|| ParseError::MissingJsonPlanData {
                        message: "Empty JSON plan array".to_string(),
                        field: "Plan".to_string(),
                    })?;

                PlanSource::Json {
                    raw_json: raw_plan,
                    parsed_json,
                }
            }
            PlanSourceFormat::Text => {
                let plan_lines = Self::parse_text_lines(&raw_plan);
                PlanSource::Text {
                    raw_text: raw_plan,
                    plan_lines,
                }
            }
        };

        Ok(QueryPlan {
            timestamp,
            duration_ms,
            query_text,
            normalized_query,
            formatted_query,
            source,
            parsed: parsed_result.parsed_plan,
        })
    }

    /// Creates a QueryPlan from raw components using simple format detection
    /// This is the legacy method for backward compatibility
    /// For efficiency, builders should use create_query_plan_from_parsed instead
    pub fn create_query_plan(
        timestamp: DateTime<Utc>,
        duration_ms: f64,
        query_text: String,
        raw_plan: String,
    ) -> ParseResult<QueryPlan> {
        // Create metadata for parsing
        let metadata = ParseMetadata::new(timestamp, duration_ms, query_text.clone());

        // Simple format detection and parsing
        let parse_result = if Self::looks_like_json(&raw_plan) {
            // Use JSON parser
            let parser = crate::parsing::JsonPlanParser::new();
            parser.parse(&raw_plan, metadata)?
        } else {
            // Use text parser
            let parser = crate::parsing::TextPlanParser::new()?;
            parser.parse(&raw_plan, metadata)?
        };

        // Delegate to the optimized method
        Self::create_query_plan_from_parsed(timestamp, duration_ms, query_text, raw_plan, parse_result)
    }


    /// Check if content looks like JSON format
    fn looks_like_json(input: &str) -> bool {
        let trimmed = input.trim_start();
        trimmed.starts_with('[') || trimmed.starts_with('{')
    }

    /// Parse raw text into structured plan lines
    fn parse_text_lines(raw_text: &str) -> Vec<PlanLine> {
        raw_text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| PlanLine::new(line))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn test_create_text_plan() {
        let plan_text = r#"Seq Scan on users  (cost=0.00..10.00 rows=100 width=8)
  Output: id, name
  Filter: (active = true)"#;

        let result = PlanFactory::create_query_plan(
            Utc::now(),
            100.5,
            "SELECT * FROM users WHERE active = true".to_string(),
            plan_text.to_string(),
        );

        assert!(result.is_ok());
        let query_plan = result.unwrap();
        assert!(query_plan.is_text_plan());
        assert_eq!(query_plan.duration_ms(), 100.5);
    }

    #[test]
    fn test_create_json_plan() {
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

        let result = PlanFactory::create_query_plan(
            Utc::now(),
            150.0,
            "SELECT * FROM users".to_string(),
            json_content.to_string(),
        );

        assert!(result.is_ok());
        let query_plan = result.unwrap();
        assert!(query_plan.is_json_plan());
        assert_eq!(query_plan.duration_ms(), 150.0);
    }

    #[test]
    fn test_invalid_json_plan() {
        let invalid_json = "[{invalid json}]";

        let result = PlanFactory::create_query_plan(
            Utc::now(),
            100.0,
            "SELECT 1".to_string(),
            invalid_json.to_string(),
        );

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ParseError::InvalidJsonFormat { .. }));
    }

    #[test]
    fn test_empty_json_array() {
        let empty_json = "[]";

        let result = PlanFactory::create_query_plan(
            Utc::now(),
            100.0,
            "SELECT 1".to_string(),
            empty_json.to_string(),
        );

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ParseError::MissingJsonPlanData { .. }));
    }
}