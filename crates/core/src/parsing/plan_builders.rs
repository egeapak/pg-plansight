//! Plan builder types extracted from models
//! 
//! These builders handle the incremental construction of QueryPlan objects
//! during parsing, maintaining state as lines are processed.

use chrono::{DateTime, Utc};
use crate::{QueryPlan, JsonPlan, PlanLine};
use crate::parsing::errors::{ParseError, ParseResult};

/// Intermediate builder before format is determined
#[derive(Debug, Clone, PartialEq)]
pub struct UntypedPlanBuilder {
    pub timestamp: DateTime<Utc>,
    pub duration_ms: f64,
    pub query_text: String,
}

impl UntypedPlanBuilder {
    pub fn new(timestamp: DateTime<Utc>, duration_ms: f64) -> Self {
        Self {
            timestamp,
            duration_ms,
            query_text: String::new(),
        }
    }

    pub fn into_text_builder(self) -> TextPlanBuilder {
        TextPlanBuilder {
            timestamp: self.timestamp,
            duration_ms: self.duration_ms,
            query_text: self.query_text,
            content_lines: Vec::new(),
        }
    }

    pub fn into_json_builder(self) -> JsonPlanBuilder {
        JsonPlanBuilder {
            timestamp: self.timestamp,
            duration_ms: self.duration_ms,
            query_text: self.query_text,
            json_content: String::new(),
        }
    }
}

/// Builder for text plans
#[derive(Debug, Clone, PartialEq)]
pub struct TextPlanBuilder {
    pub timestamp: DateTime<Utc>,
    pub duration_ms: f64,
    pub query_text: String,
    content_lines: Vec<String>,
}

impl TextPlanBuilder {
    /// Add a line to the plan content
    /// Returns Ok(Some(QueryPlan)) when plan is complete
    /// Returns Ok(None) when more lines are needed
    /// Returns Err(error) for malformed input
    pub fn add_line(mut self, line: &str) -> ParseResult<(Self, Option<QueryPlan>)> {
        self.content_lines.push(line.to_string());

        // For text plans, we typically don't know when they're complete
        // until we see the next log entry or EOF. Return None to continue.
        Ok((self, None))
    }

    /// Force finalization of accumulated content
    pub fn finalize(self) -> ParseResult<QueryPlan> {
        if self.content_lines.is_empty() {
            return Err(ParseError::EmptyInput {
                expected: "plan content lines".to_string(),
            });
        }

        // Convert accumulated lines to raw plan text
        let raw_plan = self.content_lines.join("\n");

        // Use the factory to create the QueryPlan
        crate::parsing::PlanFactory::create_query_plan(
            self.timestamp,
            self.duration_ms,
            self.query_text,
            raw_plan,
        )
    }
}

/// Builder for JSON plans
#[derive(Debug, Clone, PartialEq)]
pub struct JsonPlanBuilder {
    pub timestamp: DateTime<Utc>,
    pub duration_ms: f64,
    pub query_text: String,
    json_content: String,
}

impl JsonPlanBuilder {
    /// Add a line to the JSON content
    /// Returns Ok((builder, Some(QueryPlan))) when JSON is complete and valid
    /// Returns Ok((builder, None)) when more lines are needed
    /// Returns Err(error) for malformed JSON or parsing errors
    pub fn add_line(mut self, line: &str) -> ParseResult<(Self, Option<QueryPlan>)> {
        if !self.json_content.is_empty() {
            self.json_content.push('\n');
        }
        self.json_content.push_str(line);

        // Try to parse as complete JSON to check if we're done
        match serde_json::from_str::<Vec<serde_json::Value>>(&self.json_content) {
            Ok(_) => {
                // JSON is syntactically valid, try to create QueryPlan
                match crate::parsing::PlanFactory::create_query_plan(
                    self.timestamp,
                    self.duration_ms,
                    self.query_text.clone(),
                    self.json_content.clone(),
                ) {
                    Ok(query_plan) => Ok((self, Some(query_plan))),
                    Err(parse_err) => Err(parse_err),
                }
            }
            Err(_) => {
                // JSON is incomplete, need more lines
                Ok((self, None))
            }
        }
    }

    /// Force finalization of accumulated content
    pub fn finalize(self) -> ParseResult<QueryPlan> {
        if self.json_content.is_empty() {
            return Err(ParseError::EmptyInput {
                expected: "JSON content".to_string(),
            });
        }

        crate::parsing::PlanFactory::create_query_plan(
            self.timestamp,
            self.duration_ms,
            self.query_text,
            self.json_content,
        )
    }
}

/// Typed builder enum for constructing QueryPlan variants
#[derive(Debug, Clone, PartialEq)]
pub enum QueryPlanBuilder {
    Untyped(UntypedPlanBuilder),
    Text(TextPlanBuilder),
    Json(JsonPlanBuilder),
}

impl QueryPlanBuilder {
    pub fn new(timestamp: DateTime<Utc>, duration_ms: f64) -> Self {
        Self::Untyped(UntypedPlanBuilder::new(timestamp, duration_ms))
    }

    pub fn set_query_text(&mut self, query_text: String) {
        match self {
            Self::Untyped(builder) => builder.query_text = query_text,
            Self::Text(builder) => builder.query_text = query_text,
            Self::Json(builder) => builder.query_text = query_text,
        }
    }

    pub fn query_text(&self) -> &str {
        match self {
            Self::Untyped(builder) => &builder.query_text,
            Self::Text(builder) => &builder.query_text,
            Self::Json(builder) => &builder.query_text,
        }
    }

    pub fn convert_to_text(self) -> QueryPlanBuilder {
        match self {
            Self::Untyped(builder) => Self::Text(builder.into_text_builder()),
            _ => self, // Already typed or wrong type
        }
    }

    pub fn convert_to_json(self) -> QueryPlanBuilder {
        match self {
            Self::Untyped(builder) => Self::Json(builder.into_json_builder()),
            _ => self, // Already typed or wrong type
        }
    }

    /// Get the current state as a string for error reporting
    pub fn current_state(&self) -> &'static str {
        match self {
            Self::Untyped(_) => "Untyped",
            Self::Text(_) => "Text",
            Self::Json(_) => "Json",
        }
    }
}