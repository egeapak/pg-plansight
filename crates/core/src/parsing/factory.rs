//! Factory for creating QueryPlan instances
//! 
//! This factory separates the complex logic for creating QueryPlan objects
//! from the data model itself, making the code more maintainable and testable.

use chrono::{DateTime, Utc};
use crate::{QueryPlan, ParsedPlan, PlanSource, JsonPlan, PlanLine};
use crate::parser_utils::{normalize_query, format_sql_query};
use crate::parsing::errors::{ParseError, ParseResult};
use crate::parsing::format_detection::{detect_plan_format, PlanFormat};
use crate::plan_parser::PlanParser;

pub struct PlanFactory;

impl PlanFactory {
    /// Creates a QueryPlan from raw components
    /// This replaces the complex logic that was in QueryPlan::new()
    pub fn create_query_plan(
        timestamp: DateTime<Utc>,
        duration_ms: f64,
        query_text: String,
        raw_plan: String,
    ) -> ParseResult<QueryPlan> {
        // Detect format
        let format = detect_plan_format(&raw_plan)?;
        
        // Process query text
        let regex = regex::Regex::new(r"\$\d+").unwrap();
        let normalized_query = normalize_query(&query_text, &regex).into_owned();
        let formatted_query = format_sql_query(&query_text);

        // Parse plan based on format
        let (source, parsed) = match format {
            PlanFormat::Json => Self::create_json_plan(&raw_plan)?,
            PlanFormat::Text => Self::create_text_plan(&raw_plan)?,
        };

        Ok(QueryPlan {
            timestamp,
            duration_ms,
            query_text,
            normalized_query,
            formatted_query,
            source,
            parsed,
        })
    }

    /// Creates JSON plan source and parsed representation
    fn create_json_plan(raw_json: &str) -> ParseResult<(PlanSource, ParsedPlan)> {
        let json_plans: Vec<JsonPlan> = serde_json::from_str(raw_json)
            .map_err(|e| ParseError::InvalidJsonFormat {
                message: "Failed to parse JSON plan array".to_string(),
                json_error: e.to_string(),
            })?;

        if json_plans.is_empty() {
            return Err(ParseError::MissingJsonPlanData {
                message: "JSON plan array is empty".to_string(),
                field: "Plan".to_string(),
            });
        }

        let parsed_json = json_plans.into_iter().next().unwrap();
        
        let source = PlanSource::Json {
            raw_json: raw_json.to_string(),
            parsed_json: parsed_json.clone(),
        };

        let parsed = ParsedPlan::from_json_plan(raw_json)
            .map_err(|e| ParseError::InvalidJsonFormat {
                message: "Failed to convert JSON to ParsedPlan".to_string(),
                json_error: format!("{:?}", e),
            })?;

        Ok((source, parsed))
    }

    /// Creates text plan source and parsed representation
    fn create_text_plan(raw_text: &str) -> ParseResult<(PlanSource, ParsedPlan)> {
        let plan_lines = Self::parse_text_lines(raw_text);
        
        let source = PlanSource::Text {
            raw_text: raw_text.to_string(),
            plan_lines: plan_lines.clone(),
        };

        let parser = PlanParser::new()
            .map_err(|e| ParseError::InvalidNodeStructure {
                message: "Failed to create plan parser".to_string(),
                context: format!("{:?}", e),
            })?;

        let parsed = parser.parse_plan_from_lines(&plan_lines)
            .map_err(|e| ParseError::InvalidNodeStructure {
                message: "Failed to parse text plan".to_string(),
                context: format!("{:?}", e),
            })?;

        Ok((source, parsed))
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