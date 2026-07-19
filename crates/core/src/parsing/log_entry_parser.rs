//! Log entry parsing logic
//!
//! Handles parsing of PostgreSQL log entries to extract plan information

use crate::QueryPlan;
use crate::parser_utils::{parse_duration_from_line, parse_timestamp};
use crate::parsing::errors::{ParseError, ParseResult};
use crate::parsing::format_detection::looks_like_json_start;
use crate::parsing::plan_builders::{JsonLineOutcome, JsonPlanBuilder, QueryPlanBuilder};
use regex::Regex;

/// State machine for parsing log entries
#[derive(Debug, PartialEq)]
pub enum LogParsingState {
    None,
    WaitingForQuery(QueryPlanBuilder),
    ParsingQuery(QueryPlanBuilder),
    ParsingTextPlan(QueryPlanBuilder),
    ParsingJsonPlan(QueryPlanBuilder, String), // Accumulating JSON content
}

impl LogParsingState {
    /// Reset the state with a new builder, returning any completed plan
    pub fn reset_with_builder(&mut self, builder: QueryPlanBuilder) -> Option<QueryPlan> {
        let old_state = std::mem::replace(self, LogParsingState::WaitingForQuery(builder));
        old_state.finalize_plan()
    }

    /// Finish parsing and return any completed plan
    pub fn finish(&mut self) -> Option<QueryPlan> {
        let old_state = std::mem::replace(self, LogParsingState::None);
        old_state.finalize_plan()
    }

    /// Finalize the current state into a plan if possible
    fn finalize_plan(self) -> Option<QueryPlan> {
        match self {
            Self::None => None,
            Self::WaitingForQuery(_) => None, // Not ready to finalize
            Self::ParsingQuery(_) => None,    // Not ready to finalize
            Self::ParsingTextPlan(QueryPlanBuilder::Text(builder)) => builder.finalize().ok(),
            Self::ParsingJsonPlan(QueryPlanBuilder::Json(builder), _) => builder.finalize().ok(),
            _ => None, // Invalid state combinations
        }
    }

    /// Get the current state as a string for debugging
    pub fn state_name(&self) -> &'static str {
        match self {
            Self::None => "None",
            Self::WaitingForQuery(_) => "WaitingForQuery",
            Self::ParsingQuery(_) => "ParsingQuery",
            Self::ParsingTextPlan(_) => "ParsingTextPlan",
            Self::ParsingJsonPlan(_, _) => "ParsingJsonPlan",
        }
    }

    /// Advance the state machine by one **continuation** line (a line that is
    /// not itself a timestamped log line). This is the single implementation of
    /// the WaitingForQuery / ParsingQuery / ParsingTextPlan / ParsingJsonPlan
    /// transitions, shared by the streaming file parser
    /// (`PostgreSQLLogParser`) and the line-at-a-time [`LogEntryParser`] used by
    /// the in-database extension — so the two can no longer drift.
    ///
    /// `line_trimmed` is the line with its trailing newline removed (leading
    /// indentation is preserved for text-plan lines); `plan_regex` recognizes a
    /// text-plan node line. Callers must have already skipped blank lines.
    /// A text-plan finalization error is returned in
    /// [`ContinuationOutcome::error`] with the state reset to `None`, letting
    /// each caller choose to propagate it or warn and continue.
    pub fn advance_continuation(
        self,
        line_trimmed: &str,
        plan_regex: &Regex,
    ) -> ContinuationOutcome {
        let trimmed = line_trimmed.trim();
        match self {
            LogParsingState::WaitingForQuery(mut builder) => {
                if let Some(query_text) = trimmed.strip_prefix("Query Text:") {
                    builder.set_query_text(query_text.trim().to_string());
                    ContinuationOutcome::to(LogParsingState::ParsingQuery(builder))
                } else if looks_like_json_start(trimmed) {
                    // auto_explain.log_format=json entries are one JSON object
                    // with the query embedded as a "Query Text" key; there is no
                    // "Query Text:" text line.
                    Self::start_json(builder.convert_to_json(), trimmed)
                } else {
                    ContinuationOutcome::to(LogParsingState::WaitingForQuery(builder))
                }
            }
            LogParsingState::ParsingQuery(builder) => {
                if looks_like_json_start(trimmed) {
                    // Might be a JSON plan — or a JSON literal inside the query
                    // text; advance_json demotes back to query text if the
                    // closed JSON turns out not to be a plan document.
                    Self::start_json(builder.convert_to_json(), trimmed)
                } else if plan_regex.is_match(trimmed) {
                    Self::advance_text(builder.convert_to_text(), line_trimmed)
                } else {
                    // Continue accumulating query text.
                    let mut builder = builder;
                    builder.append_query_line(line_trimmed);
                    ContinuationOutcome::to(LogParsingState::ParsingQuery(builder))
                }
            }
            LogParsingState::ParsingTextPlan(builder) => Self::advance_text(builder, line_trimmed),
            LogParsingState::ParsingJsonPlan(builder, json_content) => {
                if let QueryPlanBuilder::Json(json_builder) = builder {
                    Self::advance_json_builder(json_builder, trimmed)
                } else {
                    ContinuationOutcome::to(LogParsingState::ParsingJsonPlan(builder, json_content))
                }
            }
            other => ContinuationOutcome::to(other),
        }
    }

    /// Feed the first JSON line to a freshly converted builder (or re-wrap if
    /// the conversion did not yield a JSON builder).
    fn start_json(typed_builder: QueryPlanBuilder, trimmed: &str) -> ContinuationOutcome {
        if let QueryPlanBuilder::Json(json_builder) = typed_builder {
            Self::advance_json_builder(json_builder, trimmed)
        } else {
            ContinuationOutcome::to(LogParsingState::ParsingJsonPlan(
                typed_builder,
                String::new(),
            ))
        }
    }

    /// Feed one line to a JSON builder and translate the outcome into the next
    /// state (shared by the WaitingForQuery, ParsingQuery, and ParsingJsonPlan
    /// arms).
    fn advance_json_builder(json_builder: JsonPlanBuilder, trimmed: &str) -> ContinuationOutcome {
        let (updated_builder, outcome) = json_builder.add_line(trimmed);
        match outcome {
            JsonLineOutcome::Complete(plan) => ContinuationOutcome {
                state: LogParsingState::None,
                plan: Some(*plan),
                error: None,
            },
            JsonLineOutcome::Incomplete => {
                ContinuationOutcome::to(LogParsingState::ParsingJsonPlan(
                    QueryPlanBuilder::Json(updated_builder),
                    String::new(),
                ))
            }
            JsonLineOutcome::NotAPlan => {
                // The opener was a JSON-ish literal inside the query text, not a
                // plan document: demote the accumulated lines back to query text
                // and resume query parsing.
                ContinuationOutcome::to(LogParsingState::ParsingQuery(QueryPlanBuilder::Untyped(
                    updated_builder.into_query_builder(),
                )))
            }
        }
    }

    /// Feed one line to a text-plan builder.
    fn advance_text(typed_builder: QueryPlanBuilder, line_trimmed: &str) -> ContinuationOutcome {
        let QueryPlanBuilder::Text(text_builder) = typed_builder else {
            return ContinuationOutcome::to(LogParsingState::ParsingTextPlan(typed_builder));
        };
        match text_builder.add_line(line_trimmed) {
            Ok((_, Some(plan))) => ContinuationOutcome {
                state: LogParsingState::None,
                plan: Some(plan),
                error: None,
            },
            Ok((updated_builder, None)) => ContinuationOutcome::to(
                LogParsingState::ParsingTextPlan(QueryPlanBuilder::Text(updated_builder)),
            ),
            Err(e) => ContinuationOutcome {
                state: LogParsingState::None,
                plan: None,
                error: Some(e),
            },
        }
    }
}

/// Outcome of [`LogParsingState::advance_continuation`]: the next state, any
/// completed plan, and a text-plan finalization error (state is `None` then).
pub struct ContinuationOutcome {
    pub state: LogParsingState,
    pub plan: Option<QueryPlan>,
    pub error: Option<ParseError>,
}

impl ContinuationOutcome {
    /// A plain state transition with no completed plan and no error.
    fn to(state: LogParsingState) -> Self {
        Self {
            state,
            plan: None,
            error: None,
        }
    }
}

pub struct LogEntryParser {
    log_line_regex: Regex,
    duration_regex: Regex,
    plan_regex: Regex,
}

impl LogEntryParser {
    pub fn new() -> ParseResult<Self> {
        Ok(Self {
            log_line_regex: Regex::new(crate::parser_utils::LOG_LINE_PATTERN).map_err(|_| {
                ParseError::RegexError {
                    message: "Failed to compile log line regex".to_string(),
                    pattern: crate::parser_utils::LOG_LINE_PATTERN.to_string(),
                }
            })?,
            duration_regex: Regex::new(r"duration: ([\d.]+) ms\s+plan:\s*$").map_err(|_| {
                ParseError::RegexError {
                    message: "Failed to compile duration regex".to_string(),
                    pattern: r"duration: ([\d.]+) ms\s+plan:\s*$".to_string(),
                }
            })?,
            plan_regex: Regex::new(r"\(cost=[\d.]+\.\.[\d.]+\s+rows=\d+\s+width=\d+\)").map_err(
                |_| ParseError::RegexError {
                    message: "Failed to compile plan regex".to_string(),
                    pattern: r"\(cost=[\d.]+\.\.[\d.]+\s+rows=\d+\s+width=\d+\)".to_string(),
                },
            )?,
        })
    }

    /// Process a single log line and update the parsing state
    pub fn process_line(
        &self,
        line: &str,
        line_number: Option<usize>,
        state: &mut LogParsingState,
    ) -> ParseResult<Option<QueryPlan>> {
        let line_trimmed = line.trim_end();

        // Check if this is a timestamped log line
        if let Some(captures) = self.log_line_regex.captures(line_trimmed) {
            let timestamp_str = captures.get(1).unwrap().as_str();
            let message = captures.get(2).unwrap().as_str();

            // Check for "duration: X ms plan:" which starts auto_explain output
            if let Some(duration) = parse_duration_from_line(message, &self.duration_regex) {
                let timestamp =
                    parse_timestamp(timestamp_str).map_err(|e| ParseError::LogParsingError {
                        message: format!("Failed to parse timestamp: {}", e),
                        line_number,
                        line_content: line.to_string(),
                    })?;

                let new_builder = QueryPlanBuilder::new(timestamp, duration);
                Ok(state.reset_with_builder(new_builder))
            }
            // Any other timestamped log line ends the current parsing
            else {
                Ok(state.finish())
            }
        } else {
            // Handle continuation lines (lines that don't match the log format)
            self.process_continuation_line(line_trimmed, line_number, state)
        }
    }

    /// Process lines that are part of a plan (continuation lines) by delegating
    /// to the shared [`LogParsingState::advance_continuation`], propagating any
    /// text-plan error with this line's number and content.
    fn process_continuation_line(
        &self,
        line: &str,
        line_number: Option<usize>,
        state: &mut LogParsingState,
    ) -> ParseResult<Option<QueryPlan>> {
        if line.trim().is_empty() {
            return Ok(None);
        }

        let old_state = std::mem::replace(state, LogParsingState::None);
        let outcome = old_state.advance_continuation(line, &self.plan_regex);
        *state = outcome.state;

        if let Some(e) = outcome.error {
            return Err(ParseError::LogParsingError {
                message: format!("Text plan parsing error: {}", e),
                line_number,
                line_content: line.to_string(),
            });
        }
        Ok(outcome.plan)
    }
}

impl Default for LogEntryParser {
    fn default() -> Self {
        Self::new().expect("Failed to create default LogEntryParser")
    }
}
