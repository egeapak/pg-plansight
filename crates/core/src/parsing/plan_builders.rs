//! Plan builder types extracted from models
//!
//! These builders handle the incremental construction of QueryPlan objects
//! during parsing, maintaining state as lines are processed.

use crate::QueryPlan;
use crate::parsing::errors::{ParseError, ParseResult};
use crate::parsing::parser_trait::{ParseMetadata, PlanParserCore};
use chrono::{DateTime, Utc};

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
            query_text: String::with_capacity(1024), // Pre-allocate 1KB for typical queries
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
            json_content: String::with_capacity(2048), // Pre-allocate 2KB for JSON plans
            depth: 0,
            in_string: false,
            escape_next: false,
            started: false,
        }
    }
}

/// Builder for text plans
#[derive(Debug, Clone, PartialEq)]
pub struct TextPlanBuilder {
    pub timestamp: DateTime<Utc>,
    pub duration_ms: f64,
    pub query_text: String,
    pub content_lines: Vec<String>,
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

    /// Force finalization of accumulated content using associated TextPlanParser
    pub fn finalize(self) -> ParseResult<QueryPlan> {
        if self.content_lines.is_empty() {
            return Err(ParseError::EmptyInput {
                expected: "plan content lines".to_string(),
            });
        }

        // Convert accumulated lines to raw plan text
        let raw_plan = self.content_lines.join("\n");

        // Create metadata
        let metadata =
            ParseMetadata::new(self.timestamp, self.duration_ms, self.query_text.clone());

        // Use the associated TextPlanParser directly (no format detection needed)
        let parser = crate::parsing::TextPlanParser::new()?;
        let parsed_result = parser.parse(&raw_plan, metadata)?;

        // Use the optimized factory method that accepts pre-parsed results
        crate::parsing::PlanFactory::create_query_plan_from_parsed(
            self.timestamp,
            self.duration_ms,
            self.query_text,
            raw_plan,
            parsed_result,
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
    /// Net bracket/brace nesting depth seen so far (outside string literals).
    depth: i32,
    /// Whether the scanner is currently inside a JSON string literal.
    in_string: bool,
    /// Whether the previous char was a backslash escape inside a string.
    escape_next: bool,
    /// Whether any opening bracket/brace has been seen yet.
    started: bool,
}

impl JsonPlanBuilder {
    /// Incrementally track bracket/brace depth over the newly appended bytes so
    /// we can detect a structurally complete JSON value in O(bytes) total,
    /// instead of re-parsing the whole accumulating buffer on every line
    /// (which was O(n^2)). Returns true once the top-level value has closed.
    fn scan_completion(&mut self, line: &str) -> bool {
        for &b in line.as_bytes() {
            if self.in_string {
                if self.escape_next {
                    self.escape_next = false;
                } else if b == b'\\' {
                    self.escape_next = true;
                } else if b == b'"' {
                    self.in_string = false;
                }
                continue;
            }
            match b {
                b'"' => self.in_string = true,
                b'[' | b'{' => {
                    self.depth += 1;
                    self.started = true;
                }
                b']' | b'}' => self.depth -= 1,
                _ => {}
            }
        }
        self.started && self.depth <= 0
    }

    /// Add a line to the JSON content
    /// Returns Ok((builder, Some(QueryPlan))) when JSON is complete and valid
    /// Returns Ok((builder, None)) when more lines are needed
    /// Returns Err(error) for malformed JSON or parsing errors
    pub fn add_line(mut self, line: &str) -> ParseResult<(Self, Option<QueryPlan>)> {
        if !self.json_content.is_empty() {
            self.json_content.push('\n');
        }
        self.json_content.push_str(line);

        // Cheap structural check first; only attempt a real parse once the
        // top-level brackets are balanced.
        if !self.scan_completion(line) {
            return Ok((self, None));
        }

        // Structure is closed; validate and build.
        match serde_json::from_str::<Vec<serde_json::Value>>(&self.json_content) {
            Ok(_) => {
                // JSON is syntactically valid, use associated JsonPlanParser directly
                let metadata =
                    ParseMetadata::new(self.timestamp, self.duration_ms, self.query_text.clone());
                let parser = crate::parsing::JsonPlanParser::new();

                match parser.parse(&self.json_content, metadata) {
                    Ok(parsed_result) => {
                        // Use the optimized factory method that accepts pre-parsed results
                        match crate::parsing::PlanFactory::create_query_plan_from_parsed(
                            self.timestamp,
                            self.duration_ms,
                            self.query_text.clone(),
                            self.json_content.clone(),
                            parsed_result,
                        ) {
                            Ok(query_plan) => Ok((self, Some(query_plan))),
                            Err(parse_err) => Err(parse_err),
                        }
                    }
                    Err(parse_err) => Err(parse_err),
                }
            }
            Err(_) => {
                // JSON is incomplete, need more lines
                Ok((self, None))
            }
        }
    }

    /// Force finalization of accumulated content using associated JsonPlanParser
    pub fn finalize(self) -> ParseResult<QueryPlan> {
        if self.json_content.is_empty() {
            return Err(ParseError::EmptyInput {
                expected: "JSON content".to_string(),
            });
        }

        // Create metadata
        let metadata =
            ParseMetadata::new(self.timestamp, self.duration_ms, self.query_text.clone());

        // Use the associated JsonPlanParser directly (no format detection needed)
        let parser = crate::parsing::JsonPlanParser::new();
        let parsed_result = parser.parse(&self.json_content, metadata)?;

        // Use the optimized factory method that accepts pre-parsed results
        crate::parsing::PlanFactory::create_query_plan_from_parsed(
            self.timestamp,
            self.duration_ms,
            self.query_text,
            self.json_content,
            parsed_result,
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

    /// Efficiently append a line to the query text without allocating new strings
    pub fn append_query_line(&mut self, line: &str) {
        let query_text = match self {
            Self::Untyped(builder) => &mut builder.query_text,
            Self::Text(builder) => &mut builder.query_text,
            Self::Json(builder) => &mut builder.query_text,
        };

        if !query_text.is_empty() {
            query_text.push('\n');
        }
        query_text.push_str(line);
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn test_efficient_query_append() {
        let mut builder = QueryPlanBuilder::new(Utc::now(), 100.0);

        // Test initial state
        assert_eq!(builder.query_text(), "");

        // Test appending to empty string
        builder.append_query_line("SELECT * FROM users");
        assert_eq!(builder.query_text(), "SELECT * FROM users");

        // Test appending with newline insertion
        builder.append_query_line("WHERE id = $1");
        assert_eq!(builder.query_text(), "SELECT * FROM users\nWHERE id = $1");

        // Test multiple appends
        builder.append_query_line("AND status = 'active'");
        assert_eq!(
            builder.query_text(),
            "SELECT * FROM users\nWHERE id = $1\nAND status = 'active'"
        );
    }

    #[test]
    fn test_query_append_maintains_builder_type() {
        let timestamp = Utc::now();
        let mut builder = QueryPlanBuilder::new(timestamp, 100.0);

        // Test with untyped builder
        builder.append_query_line("SELECT 1");
        assert_eq!(builder.current_state(), "Untyped");

        // Convert to text and test
        let mut text_builder = builder.convert_to_text();
        text_builder.append_query_line("FROM dual");
        assert_eq!(text_builder.current_state(), "Text");
        assert_eq!(text_builder.query_text(), "SELECT 1\nFROM dual");
    }
}
