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

    /// Parse the accumulated JSON **once** and build the finished `QueryPlan`.
    ///
    /// auto_explain's log_format=json emits ONE top-level OBJECT per entry with
    /// the query inside it as a "Query Text" key (auto_explain rewrites
    /// EXPLAIN's `[`/`]` to `{`/`}`), while EXPLAIN (FORMAT JSON) emits an ARRAY
    /// of plan objects. Both are accepted.
    ///
    /// Returns `Ok(None)` when the content, though structurally closed, is not a
    /// JSON plan document (invalid JSON, or not an object/array) — the caller
    /// demotes it to query text. `Err` means it parsed as JSON and matched the
    /// object/array shape but failed the plan schema.
    ///
    /// This replaces the old parse→re-serialize→re-parse→re-parse chain: the
    /// plan document is deserialized exactly once (`from_str` + one
    /// `from_value`); `raw_json` keeps the normalized single-element array form
    /// so the stored/exported shape is unchanged.
    fn build_query_plan(&mut self) -> ParseResult<Option<QueryPlan>> {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&self.json_content) else {
            return Ok(None);
        };

        let plan_obj = match value {
            serde_json::Value::Array(items) => match items.into_iter().next() {
                Some(obj) => obj,
                None => return Ok(None),
            },
            serde_json::Value::Object(map) => {
                if self.query_text.is_empty()
                    && let Some(serde_json::Value::String(query)) = map.get("Query Text")
                {
                    self.query_text = query.clone();
                }
                serde_json::Value::Object(map)
            }
            _ => return Ok(None),
        };

        // Keep the stored/exported form identical to before (a one-element
        // array); serialize once from the value we already hold.
        let raw_json = serde_json::Value::Array(vec![plan_obj.clone()]).to_string();

        let json_plan: crate::JsonPlan =
            serde_json::from_value(plan_obj).map_err(|e| ParseError::InvalidJsonFormat {
                message: "JSON does not match the PostgreSQL plan schema".to_string(),
                json_error: e.to_string(),
            })?;

        let plan = crate::parsing::PlanFactory::create_json_query_plan_from_struct(
            self.timestamp,
            self.duration_ms,
            self.query_text.clone(),
            raw_json,
            json_plan,
        )?;
        Ok(Some(plan))
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
        match self.build_query_plan() {
            Ok(Some(query_plan)) => (self, JsonLineOutcome::Complete(Box::new(query_plan))),
            Ok(None) => (self, JsonLineOutcome::NotAPlan),
            Err(e) => {
                // Demotion to query text is the right call for JSON literals
                // inside queries — but a document that carries a "Plan" key is
                // a real plan failing the schema, and losing it silently
                // (recorded as a garbage "query") would be invisible data
                // loss. Surface it.
                if self.json_content.contains("\"Plan\"") {
                    tracing::warn!(
                        error = %e,
                        "JSON document looks like a plan but failed to parse; \
                         treating it as query text"
                    );
                }
                (self, JsonLineOutcome::NotAPlan)
            }
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

    /// Force finalization of accumulated content, parsing the plan once.
    pub fn finalize(mut self) -> ParseResult<QueryPlan> {
        if self.json_content.is_empty() {
            return Err(ParseError::EmptyInput {
                expected: "JSON content".to_string(),
            });
        }

        self.build_query_plan()?
            .ok_or_else(|| ParseError::InvalidJsonFormat {
                message: "Accumulated content is not a JSON plan document".to_string(),
                json_error: "expected a JSON array or object".to_string(),
            })
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

    /// Feed a whole JSON document to the builder line by line and return the
    /// outcome of the line that closed it.
    fn drive_json(builder: JsonPlanBuilder, doc: &str) -> (JsonPlanBuilder, JsonLineOutcome) {
        let mut b = builder;
        let mut last = JsonLineOutcome::Incomplete;
        for line in doc.lines() {
            let (nb, outcome) = b.add_line(line);
            b = nb;
            last = outcome;
        }
        (b, last)
    }

    #[test]
    fn test_json_object_form_parses_once_and_extracts_query() {
        // auto_explain log_format=json object form: the query lives in a
        // "Query Text" key and must be extracted; the plan must build.
        let doc = r#"{
  "Query Text": "SELECT * FROM users WHERE id = 42",
  "Plan": { "Node Type": "Seq Scan", "Relation Name": "users", "Alias": "users",
            "Startup Cost": 0.0, "Total Cost": 1.1, "Plan Rows": 1, "Plan Width": 4 }
}"#;
        let builder = UntypedPlanBuilder::new(Utc::now(), 12.0).into_json_builder();
        let (_b, outcome) = drive_json(builder, doc);
        match outcome {
            JsonLineOutcome::Complete(plan) => {
                assert_eq!(plan.query_text, "SELECT * FROM users WHERE id = 42");
                assert!(plan.raw_plan().contains("Seq Scan"));
                // Storage form stays the normalized single-element array.
                assert!(plan.raw_plan().trim_start().starts_with('['));
            }
            other => panic!("expected a completed plan, got {other:?}"),
        }
    }

    #[test]
    fn test_json_array_form_parses() {
        // EXPLAIN (FORMAT JSON) array form.
        let doc = r#"[{ "Plan": { "Node Type": "Result", "Startup Cost": 0.0,
            "Total Cost": 0.01, "Plan Rows": 1, "Plan Width": 4 } }]"#;
        let builder = UntypedPlanBuilder::new(Utc::now(), 1.0).into_json_builder();
        let (_b, outcome) = drive_json(builder, doc);
        assert!(matches!(outcome, JsonLineOutcome::Complete(_)));
    }

    #[test]
    fn test_json_literal_in_query_is_demoted_not_a_plan() {
        // A closed JSON object with no "Plan" key is a JSON literal from the
        // query text, not a plan — demote silently.
        let doc = r#"{"status": "active", "limit": 10}"#;
        let builder = UntypedPlanBuilder::new(Utc::now(), 1.0).into_json_builder();
        let (_b, outcome) = drive_json(builder, doc);
        assert!(matches!(outcome, JsonLineOutcome::NotAPlan));
    }
}
