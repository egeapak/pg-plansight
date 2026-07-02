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

/// Outcome of feeding one line to [`JsonPlanBuilder::add_line`].
#[derive(Debug)]
pub enum JsonLineOutcome {
    /// The top-level JSON value has not closed yet; keep feeding lines.
    Incomplete,
    /// A complete, schema-valid plan document was parsed.
    Complete(Box<QueryPlan>),
    /// The accumulated content closed structurally but is not a plan document
    /// (invalid JSON, or valid JSON without a recognizable "Plan"). Appending
    /// more lines can never fix a closed top-level value, so this is terminal:
    /// the opener was most likely a JSON-ish literal inside the query text.
    /// Demote via [`JsonPlanBuilder::into_query_builder`].
    NotAPlan,
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

    /// Normalize the accumulated content to the array form the downstream
    /// plan schema expects, extracting the embedded query text when present.
    ///
    /// auto_explain's log_format=json emits ONE top-level OBJECT per entry
    /// with the query inside it as a "Query Text" key (auto_explain rewrites
    /// EXPLAIN's `[`/`]` to `{`/`}`), while EXPLAIN (FORMAT JSON) emits an
    /// ARRAY of plan objects. Returns None when the content is not valid JSON
    /// or not an object/array.
    fn normalized_content(&mut self) -> Option<String> {
        let value: serde_json::Value = serde_json::from_str(&self.json_content).ok()?;
        match value {
            serde_json::Value::Array(_) => Some(self.json_content.clone()),
            serde_json::Value::Object(ref obj) => {
                if self.query_text.is_empty()
                    && let Some(serde_json::Value::String(query)) = obj.get("Query Text")
                {
                    self.query_text = query.clone();
                }
                Some(serde_json::Value::Array(vec![value]).to_string())
            }
            _ => None,
        }
    }

    /// Add a line to the JSON content. See [`JsonLineOutcome`] for the
    /// possible results; `NotAPlan` is terminal and callers should demote the
    /// builder back to query-text parsing.
    pub fn add_line(mut self, line: &str) -> (Self, JsonLineOutcome) {
        if !self.json_content.is_empty() {
            self.json_content.push('\n');
        }
        self.json_content.push_str(line);

        // Cheap structural check first; only attempt a real parse once the
        // top-level brackets are balanced. (Re-parsing the growing buffer on
        // every line would be O(n²).)
        if !self.scan_completion(line) {
            return (self, JsonLineOutcome::Incomplete);
        }

        // The top-level value has closed: it either parses as a plan document
        // now or never will.
        let Some(normalized) = self.normalized_content() else {
            return (self, JsonLineOutcome::NotAPlan);
        };

        let metadata =
            ParseMetadata::new(self.timestamp, self.duration_ms, self.query_text.clone());
        let parser = crate::parsing::JsonPlanParser::new();

        let built = parser.parse(&normalized, metadata).and_then(|parsed| {
            crate::parsing::PlanFactory::create_query_plan_from_parsed(
                self.timestamp,
                self.duration_ms,
                self.query_text.clone(),
                normalized,
                parsed,
            )
        });
        match built {
            Ok(query_plan) => (self, JsonLineOutcome::Complete(Box::new(query_plan))),
            Err(_) => (self, JsonLineOutcome::NotAPlan),
        }
    }

    /// Demote the accumulated content back into query text, returning to the
    /// untyped (query-collecting) stage. Used when a line starting with
    /// '{'/'[' turned out to be a JSON literal inside the query rather than
    /// the start of a plan document.
    pub fn into_query_builder(self) -> UntypedPlanBuilder {
        let mut builder = UntypedPlanBuilder::new(self.timestamp, self.duration_ms);
        builder.query_text = self.query_text;
        for line in self.json_content.lines() {
            if !builder.query_text.is_empty() {
                builder.query_text.push('\n');
            }
            builder.query_text.push_str(line);
        }
        builder
    }

    /// Force finalization of accumulated content using associated JsonPlanParser
    pub fn finalize(mut self) -> ParseResult<QueryPlan> {
        if self.json_content.is_empty() {
            return Err(ParseError::EmptyInput {
                expected: "JSON content".to_string(),
            });
        }

        let normalized =
            self.normalized_content()
                .ok_or_else(|| ParseError::InvalidJsonFormat {
                    message: "Accumulated content is not a JSON plan document".to_string(),
                    json_error: "expected a JSON array or object".to_string(),
                })?;

        // Create metadata
        let metadata =
            ParseMetadata::new(self.timestamp, self.duration_ms, self.query_text.clone());

        // Use the associated JsonPlanParser directly (no format detection needed)
        let parser = crate::parsing::JsonPlanParser::new();
        let parsed_result = parser.parse(&normalized, metadata)?;

        // Use the optimized factory method that accepts pre-parsed results
        crate::parsing::PlanFactory::create_query_plan_from_parsed(
            self.timestamp,
            self.duration_ms,
            self.query_text,
            normalized,
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
