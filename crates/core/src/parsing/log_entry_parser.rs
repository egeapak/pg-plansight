//! Log entry parsing logic
//! 
//! Handles parsing of PostgreSQL log entries to extract plan information

use regex::Regex;
use crate::{QueryPlan};
use crate::parser_utils::{parse_timestamp, parse_duration_from_line};
use crate::parsing::errors::{ParseError, ParseResult};
use crate::parsing::plan_builders::QueryPlanBuilder;
use crate::parsing::format_detection::{detect_plan_format, PlanFormat};

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
    pub fn reset_with_builder(
        &mut self,
        builder: QueryPlanBuilder,
    ) -> Option<QueryPlan> {
        let old_state = std::mem::replace(self, LogParsingState::WaitingForQuery(builder));
        old_state.finalize_plan()
    }

    /// Finish parsing and return any completed plan
    pub fn finish(&mut self) -> Option<QueryPlan> {
        let old_state = std::mem::replace(self, LogParsingState::None);
        old_state.finalize_plan()
    }

    /// Finish parsing with content and return any completed plan (compatibility method)
    pub fn finish_with_content(&mut self, _content: &str) -> Option<QueryPlan> {
        // The new architecture doesn't use this content parameter since
        // content is already accumulated in the builders
        self.finish()
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
}

pub struct LogEntryParser {
    log_line_regex: Regex,
    duration_regex: Regex,
    plan_regex: Regex,
}

impl LogEntryParser {
    pub fn new() -> ParseResult<Self> {
        Ok(Self {
            log_line_regex: Regex::new(r"^(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3})(.*)")
                .map_err(|_| ParseError::RegexError {
                    message: "Failed to compile log line regex".to_string(),
                    pattern: r"^(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3})(.*)".to_string(),
                })?,
            duration_regex: Regex::new(r"duration: ([\d.]+) ms\s+plan:\s*$")
                .map_err(|_| ParseError::RegexError {
                    message: "Failed to compile duration regex".to_string(),
                    pattern: r"duration: ([\d.]+) ms\s+plan:\s*$".to_string(),
                })?,
            plan_regex: Regex::new(r"\(cost=[\d.]+\.\.[\d.]+\s+rows=\d+\s+width=\d+\)")
                .map_err(|_| ParseError::RegexError {
                    message: "Failed to compile plan regex".to_string(),
                    pattern: r"\(cost=[\d.]+\.\.[\d.]+\s+rows=\d+\s+width=\d+\)".to_string(),
                })?,
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
                let timestamp = parse_timestamp(timestamp_str)
                    .map_err(|e| ParseError::LogParsingError {
                        message: format!("Failed to parse timestamp: {}", e),
                        line_number,
                        line_content: line.to_string(),
                    })?;

                let new_builder = QueryPlanBuilder::new(timestamp, duration);
                return Ok(state.reset_with_builder(new_builder));
            }
            // Any other timestamped log line ends the current parsing
            else {
                return Ok(state.finish());
            }
        } else {
            // Handle continuation lines (lines that don't match the log format)
            return self.process_continuation_line(line_trimmed, line_number, state);
        }
    }

    /// Process lines that are part of a plan (continuation lines)
    fn process_continuation_line(
        &self,
        line: &str,
        line_number: Option<usize>,
        state: &mut LogParsingState,
    ) -> ParseResult<Option<QueryPlan>> {
        let trimmed = line.trim();

        if trimmed.is_empty() {
            return Ok(None);
        }

        let old_state = std::mem::replace(state, LogParsingState::None);
        
        *state = match old_state {
            LogParsingState::WaitingForQuery(mut builder) => {
                if let Some(query_text) = trimmed.strip_prefix("Query Text:") {
                    builder.set_query_text(query_text.trim().to_string());
                    LogParsingState::ParsingQuery(builder)
                } else {
                    LogParsingState::WaitingForQuery(builder)
                }
            }
            LogParsingState::ParsingQuery(builder) => {
                // Detect format based on first plan content line
                let format = detect_plan_format(trimmed)?;
                
                match format {
                    PlanFormat::Text => {
                        if self.plan_regex.is_match(trimmed) {
                            let typed_builder = builder.convert_to_text();
                            if let QueryPlanBuilder::Text(text_builder) = typed_builder {
                                match text_builder.add_line(line) {
                                    Ok((updated_builder, maybe_plan)) => {
                                        if let Some(plan) = maybe_plan {
                                            *state = LogParsingState::None;
                                            return Ok(Some(plan));
                                        } else {
                                            LogParsingState::ParsingTextPlan(QueryPlanBuilder::Text(updated_builder))
                                        }
                                    }
                                    Err(e) => {
                                        return Err(ParseError::LogParsingError {
                                            message: format!("Text plan parsing error: {}", e),
                                            line_number,
                                            line_content: line.to_string(),
                                        });
                                    }
                                }
                            } else {
                                LogParsingState::ParsingTextPlan(typed_builder)
                            }
                        } else {
                            // Continue parsing query text
                            let mut updated_builder = builder;
                            let current_query = updated_builder.query_text().to_string();
                            let new_query = if current_query.is_empty() {
                                line.to_string()
                            } else {
                                format!("{}\n{}", current_query, line)
                            };
                            updated_builder.set_query_text(new_query);
                            LogParsingState::ParsingQuery(updated_builder)
                        }
                    }
                    PlanFormat::Json => {
                        let typed_builder = builder.convert_to_json();
                        if let QueryPlanBuilder::Json(json_builder) = typed_builder {
                            match json_builder.add_line(trimmed) {
                                Ok((updated_builder, maybe_plan)) => {
                                    if let Some(plan) = maybe_plan {
                                        *state = LogParsingState::None;
                                        return Ok(Some(plan));
                                    } else {
                                        LogParsingState::ParsingJsonPlan(QueryPlanBuilder::Json(updated_builder), String::new())
                                    }
                                }
                                Err(e) => {
                                    return Err(ParseError::LogParsingError {
                                        message: format!("JSON plan parsing error: {}", e),
                                        line_number,
                                        line_content: line.to_string(),
                                    });
                                }
                            }
                        } else {
                            LogParsingState::ParsingJsonPlan(typed_builder, String::new())
                        }
                    }
                }
            }
            LogParsingState::ParsingTextPlan(builder) => {
                if let QueryPlanBuilder::Text(text_builder) = builder {
                    match text_builder.add_line(line) {
                        Ok((updated_builder, maybe_plan)) => {
                            if let Some(plan) = maybe_plan {
                                *state = LogParsingState::None;
                                return Ok(Some(plan));
                            } else {
                                LogParsingState::ParsingTextPlan(QueryPlanBuilder::Text(updated_builder))
                            }
                        }
                        Err(e) => {
                            return Err(ParseError::LogParsingError {
                                message: format!("Text plan parsing error: {}", e),
                                line_number,
                                line_content: line.to_string(),
                            });
                        }
                    }
                } else {
                    LogParsingState::ParsingTextPlan(builder)
                }
            }
            LogParsingState::ParsingJsonPlan(builder, json_content) => {
                if let QueryPlanBuilder::Json(json_builder) = builder {
                    match json_builder.add_line(trimmed) {
                        Ok((updated_builder, maybe_plan)) => {
                            if let Some(plan) = maybe_plan {
                                *state = LogParsingState::None;
                                return Ok(Some(plan));
                            } else {
                                LogParsingState::ParsingJsonPlan(QueryPlanBuilder::Json(updated_builder), json_content)
                            }
                        }
                        Err(e) => {
                            return Err(ParseError::LogParsingError {
                                message: format!("JSON plan parsing error: {}", e),
                                line_number,
                                line_content: line.to_string(),
                            });
                        }
                    }
                } else {
                    LogParsingState::ParsingJsonPlan(builder, json_content)
                }
            }
            other_state => other_state,
        };

        Ok(None)
    }
}

impl Default for LogEntryParser {
    fn default() -> Self {
        Self::new().expect("Failed to create default LogEntryParser")
    }
}