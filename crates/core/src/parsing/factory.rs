//! Factory for creating QueryPlan instances
//! 
//! This factory separates the complex logic for creating QueryPlan objects
//! from the data model itself, making the code more maintainable and testable.

use chrono::{DateTime, Utc};
use crate::{QueryPlan, PlanSource, JsonPlan, PlanLine};
use crate::parser_utils::format_sql_query;
use crate::sql_analysis::normalize_query_enhanced;
use crate::parsing::errors::{ParseError, ParseResult};
use crate::parsing::parser_trait::{PlanSourceFormat, ParsedPlanResult};

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
        // Process query text using enhanced normalization
        let normalization_result = normalize_query_enhanced(&query_text)
            .map_err(|e| ParseError::NormalizationError { 
                message: format!("Failed to normalize query: {}", e) 
            })?;
        
        let normalized_query = normalization_result.normalized_sql;
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




    /// Parse raw text into structured plan lines
    fn parse_text_lines(raw_text: &str) -> Vec<PlanLine> {
        raw_text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| PlanLine::new(line))
            .collect()
    }
}

