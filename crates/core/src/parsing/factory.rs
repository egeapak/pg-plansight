//! Factory for creating QueryPlan instances
//!
//! This factory separates the complex logic for creating QueryPlan objects
//! from the data model itself, making the code more maintainable and testable.

use crate::parser_utils::format_sql_query;
use crate::parsing::errors::{ParseError, ParseResult};
use crate::parsing::parser_trait::{ParsedPlanResult, PlanSourceFormat};
use crate::sql_analysis::normalize_query_enhanced;
use crate::{JsonPlan, PlanLine, PlanSource, QueryPlan};
use chrono::{DateTime, Utc};
use std::cell::RefCell;
use std::collections::HashMap;

/// Soft cap on the per-thread normalize/format memo cache. Real workloads have
/// a small number of distinct query shapes, so this is generous; if it is ever
/// exceeded the cache is cleared wholesale (cheap, rare).
const NORM_CACHE_CAP: usize = 8192;

thread_local! {
    /// Memoizes the expensive `sqlparser` work (normalization + pretty-print)
    /// keyed by a hash of the raw query text. Logs are dominated by duplicate
    /// queries, so this turns O(plans) sqlparser invocations into O(distinct
    /// queries). A file is parsed on a single thread, so a thread-local cache
    /// is contention-free and needs no locking.
    static NORM_FORMAT_CACHE: RefCell<HashMap<u64, (String, String)>> =
        RefCell::new(HashMap::new());
}

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
        // Normalize + format the query, memoizing on the raw text so duplicate
        // queries (the common case in real logs) only pay the sqlparser cost
        // once per distinct query shape.
        let (normalized_query, formatted_query) = Self::normalize_and_format(&query_text)?;

        // Create appropriate PlanSource based on the detected format
        let source = match parsed_result.source_format {
            PlanSourceFormat::Json => {
                // Parse JSON to get the structured data for PlanSource
                let json_plans: Vec<JsonPlan> =
                    serde_json::from_str(&raw_plan).map_err(|e| ParseError::InvalidJsonFormat {
                        message: "Failed to parse JSON for PlanSource".to_string(),
                        json_error: e.to_string(),
                    })?;

                let parsed_json = json_plans.into_iter().next().ok_or_else(|| {
                    ParseError::MissingJsonPlanData {
                        message: "Empty JSON plan array".to_string(),
                        field: "Plan".to_string(),
                    }
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

    /// Build a JSON `QueryPlan` from a `JsonPlan` the caller has **already**
    /// deserialized, plus the raw JSON string kept for export/round-trip.
    ///
    /// The string-threaded path (`create_query_plan_from_parsed` for
    /// `PlanSourceFormat::Json`) re-runs `serde_json::from_str` to rebuild the
    /// `JsonPlan` for `PlanSource`, on top of the parses the parser already did.
    /// The streaming builder parses the plan document exactly once and calls
    /// this instead, so no `from_str`/`from_value` is repeated.
    pub fn create_json_query_plan_from_struct(
        timestamp: DateTime<Utc>,
        duration_ms: f64,
        query_text: String,
        raw_json: String,
        json_plan: JsonPlan,
    ) -> ParseResult<QueryPlan> {
        let (normalized_query, formatted_query) = Self::normalize_and_format(&query_text)?;

        // Convert the already-deserialized plan to the structured PlanNode tree
        // (no JSON re-parse).
        let parsed_plan = crate::plan_parser::ParsedPlan::from_json_plan_struct(&json_plan)
            .map_err(|e| ParseError::InvalidJsonFormat {
                message: "Failed to convert JSON plan to a structured plan".to_string(),
                json_error: format!("{:?}", e),
            })?;

        Ok(QueryPlan {
            timestamp,
            duration_ms,
            query_text,
            normalized_query,
            formatted_query,
            source: PlanSource::Json {
                raw_json,
                parsed_json: json_plan,
            },
            parsed: parsed_plan,
        })
    }

    /// Returns `(normalized_query, formatted_query)` for the given SQL, using a
    /// per-thread memo cache keyed on a hash of the text.
    fn normalize_and_format(query_text: &str) -> ParseResult<(String, String)> {
        let key = xxhash_rust::xxh3::xxh3_64(query_text.as_bytes());

        if let Some(hit) = NORM_FORMAT_CACHE.with(|c| c.borrow().get(&key).cloned()) {
            return Ok(hit);
        }

        let normalization_result =
            normalize_query_enhanced(query_text).map_err(|e| ParseError::NormalizationError {
                message: format!("Failed to normalize query: {}", e),
            })?;
        let pair = (
            normalization_result.normalized_sql,
            format_sql_query(query_text),
        );

        NORM_FORMAT_CACHE.with(|c| {
            let mut cache = c.borrow_mut();
            if cache.len() >= NORM_CACHE_CAP {
                cache.clear();
            }
            cache.insert(key, pair.clone());
        });

        Ok(pair)
    }

    /// Parse raw text into structured plan lines
    fn parse_text_lines(raw_text: &str) -> Vec<PlanLine> {
        raw_text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(PlanLine::new)
            .collect()
    }
}
