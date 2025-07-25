use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::{
    ParsedPlan, get_indent_level,
    parser_utils::{format_sql_query, normalize_query},
};

#[derive(Debug, PartialEq)]
pub enum ParsingState {
    None,
    WaitingForQuery(QueryPlanBuilder),
    ParsingQuery(QueryPlanBuilder),
    ParsingTextPlan(QueryPlanBuilder),
    ParsingJsonPlan(QueryPlanBuilder, String), // Accumulating JSON content
}

/// Intermediate builder before format is determined
#[derive(Debug, Clone, PartialEq)]
pub struct UntypedPlanBuilder {
    pub timestamp: DateTime<Utc>,
    pub duration_ms: f64,
    pub query_text: String,
}

/// Builder for text plans
#[derive(Debug, Clone, PartialEq)]
pub struct TextPlanBuilder {
    pub timestamp: DateTime<Utc>,
    pub duration_ms: f64,
    pub query_text: String,
    content_lines: Vec<String>,
}

/// Builder for JSON plans
#[derive(Debug, Clone, PartialEq)]
pub struct JsonPlanBuilder {
    pub timestamp: DateTime<Utc>,
    pub duration_ms: f64,
    pub query_text: String,
    json_content: String,
}

/// Typed builder enum for constructing QueryPlan variants
#[derive(Debug, Clone, PartialEq)]
pub enum QueryPlanBuilder {
    Untyped(UntypedPlanBuilder),
    Text(TextPlanBuilder),
    Json(JsonPlanBuilder),
}

#[derive(Debug, PartialEq, Clone)]
pub enum PlanFormat {
    Text,
    Json,
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

impl TextPlanBuilder {
    /// Add a line to the plan content
    /// Returns Ok(Some(QueryPlan)) when plan is complete
    /// Returns Ok(None) when more lines are needed
    /// Returns Err(error) for malformed input
    pub fn add_line(mut self, line: &str) -> anyhow::Result<(Self, Option<QueryPlan>)> {
        self.content_lines.push(line.to_string());

        // For text plans, we typically don't know when they're complete
        // until we see the next log entry or EOF. Return None to continue.
        Ok((self, None))
    }

    /// Force finalization of accumulated content
    pub fn finalize(self) -> anyhow::Result<QueryPlan> {
        if self.content_lines.is_empty() {
            anyhow::bail!("No content to finalize");
        }

        // Convert accumulated lines to PlanLines
        let plan_lines: Vec<PlanLine> = self
            .content_lines
            .iter()
            .filter(|line| !line.trim().is_empty())
            .map(|line| PlanLine::new(line))
            .collect();

        let plan_text = crate::parser_utils::format_plan_lines(&plan_lines);

        QueryPlan::new(self.timestamp, self.duration_ms, self.query_text, plan_text)
    }
}

impl JsonPlanBuilder {
    /// Add a line to the JSON content
    /// Returns Ok((builder, Some(QueryPlan))) when JSON is complete and valid
    /// Returns Ok((builder, None)) when more lines are needed
    /// Returns Err(error) for malformed JSON or parsing errors
    pub fn add_line(mut self, line: &str) -> anyhow::Result<(Self, Option<QueryPlan>)> {
        if !self.json_content.is_empty() {
            self.json_content.push('\n');
        }
        self.json_content.push_str(line);

        // Try to parse as complete JSON to check if we're done
        match serde_json::from_str::<Vec<serde_json::Value>>(&self.json_content) {
            Ok(_) => {
                // JSON is valid, try to parse as QueryPlan
                match serde_json::from_str::<Vec<JsonPlan>>(&self.json_content) {
                    Ok(json_plans) => {
                        if json_plans.is_empty() {
                            anyhow::bail!("Empty JSON plan array");
                        }

                        let query_plan = QueryPlan::new(
                            self.timestamp,
                            self.duration_ms,
                            self.query_text.clone(),
                            self.json_content.clone(),
                        )?;

                        Ok((self, Some(query_plan)))
                    }
                    Err(e) => {
                        // Valid JSON but doesn't match our schema
                        Err(anyhow::anyhow!("Invalid JSON plan schema: {}", e))
                    }
                }
            }
            Err(_) => {
                // JSON is incomplete, need more lines
                Ok((self, None))
            }
        }
    }

    /// Force finalization of accumulated content
    pub fn finalize(self) -> anyhow::Result<QueryPlan> {
        if self.json_content.is_empty() {
            anyhow::bail!("No JSON content to finalize");
        }

        let json_plans: Vec<JsonPlan> = serde_json::from_str(&self.json_content)?;
        if json_plans.is_empty() {
            anyhow::bail!("Empty JSON plan array");
        }

        QueryPlan::new(
            self.timestamp,
            self.duration_ms,
            self.query_text,
            self.json_content,
        )
    }
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
}

impl ParsingState {
    pub fn reset_with_builder(
        &mut self,
        builder: QueryPlanBuilder,
        content: &str,
    ) -> Option<QueryPlan> {
        let old_state = std::mem::replace(self, ParsingState::WaitingForQuery(builder));
        old_state.finalize_plan(content)
    }

    pub fn finish(&mut self) -> Option<QueryPlan> {
        let old_state = std::mem::replace(self, ParsingState::None);
        old_state.finalize_plan("") // Pass empty content
    }

    pub fn finish_with_content(&mut self, content: &str) -> Option<QueryPlan> {
        let old_state = std::mem::replace(self, ParsingState::None);
        old_state.finalize_plan(content)
    }

    fn finalize_plan(self, _content: &str) -> Option<QueryPlan> {
        match self {
            Self::None => None,
            Self::WaitingForQuery(_) => None, // Not ready to finalize
            Self::ParsingQuery(_) => None,    // Not ready to finalize
            Self::ParsingTextPlan(QueryPlanBuilder::Text(builder)) => builder.finalize().ok(),
            Self::ParsingJsonPlan(QueryPlanBuilder::Json(builder), _) => builder.finalize().ok(),
            _ => None, // Invalid state combinations or untyped builders
        }
    }
}

#[derive(Debug)]
pub enum ParseProgress {
    Progress {
        file_index: usize,
        file_path: PathBuf,
        progress: f64,
        queries_parsed: usize,
    },
    Error {
        file_index: usize,
        file_path: PathBuf,
        error: String,
    },
    Complete {
        result: anyhow::Result<Vec<QueryPlan>>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp: DateTime<Utc>,
    pub process_id: u32,
    pub log_level: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlanLine {
    pub indentation: usize,
    pub query: String,
}

impl PlanLine {
    pub fn new(line: &str) -> Self {
        let indent = get_indent_level(line);
        Self {
            indentation: indent,
            query: line.trim().to_string(), // Back to original: trim and reconstruct
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlanSource {
    Text {
        raw_text: String,
        plan_lines: Vec<PlanLine>, // For backward compatibility
    },
    Json {
        raw_json: String,
        parsed_json: JsonPlan, // For JSON-specific access patterns
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueryPlan {
    // Execution metadata
    pub timestamp: DateTime<Utc>,
    pub duration_ms: f64,
    pub query_text: String,

    // Query processing (moved from ProcessedQuery)
    pub normalized_query: String,
    pub formatted_query: String,

    // Plan representation
    pub source: PlanSource,
    pub parsed: ParsedPlan,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextPlanData {
    pub timestamp: DateTime<Utc>,
    pub duration_ms: f64,
    pub query_text: String,
    pub plan_text: String,
    pub plan_lines: Vec<PlanLine>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonPlanData {
    pub timestamp: DateTime<Utc>,
    pub duration_ms: f64,
    pub query_text: String,
    pub raw_json: String,
    pub parsed_json: JsonPlan,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonPlan {
    #[serde(rename = "Plan")]
    pub plan: JsonPlanNode,
    #[serde(rename = "Planning Time")]
    pub planning_time: Option<f64>,
    #[serde(rename = "Execution Time")]
    pub execution_time: Option<f64>,
    #[serde(rename = "Triggers")]
    pub triggers: Option<Vec<JsonTrigger>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonPlanNode {
    #[serde(rename = "Node Type")]
    pub node_type: String,
    #[serde(rename = "Relation Name")]
    pub relation_name: Option<String>,
    #[serde(rename = "Schema")]
    pub schema: Option<String>,
    #[serde(rename = "Alias")]
    pub alias: Option<String>,
    #[serde(rename = "Startup Cost")]
    pub startup_cost: f64,
    #[serde(rename = "Total Cost")]
    pub total_cost: f64,
    #[serde(rename = "Plan Rows")]
    pub plan_rows: u64,
    #[serde(rename = "Plan Width")]
    pub plan_width: u32,

    // Actual execution data (when ANALYZE is enabled)
    #[serde(rename = "Actual Startup Time")]
    pub actual_startup_time: Option<f64>,
    #[serde(rename = "Actual Total Time")]
    pub actual_total_time: Option<f64>,
    #[serde(rename = "Actual Rows")]
    pub actual_rows: Option<u64>,
    #[serde(rename = "Actual Loops")]
    pub actual_loops: Option<u32>,

    // Child plans
    #[serde(rename = "Plans")]
    pub plans: Option<Vec<JsonPlanNode>>,

    // All other properties (Index Cond, Filter, etc.)
    #[serde(flatten)]
    pub properties: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonTrigger {
    #[serde(rename = "Trigger Name")]
    pub trigger_name: String,
    #[serde(rename = "Relation")]
    pub relation: String,
    #[serde(rename = "Time")]
    pub time: f64,
    #[serde(rename = "Calls")]
    pub calls: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PlanSourceFormat {
    Text,
    Json,
}

impl QueryPlan {
    // Single unified method for raw plan access
    pub fn raw_plan(&self) -> &str {
        match &self.source {
            PlanSource::Text { raw_text, .. } => raw_text,
            PlanSource::Json { raw_json, .. } => raw_json,
        }
    }

    // Access structured data (always available)
    pub fn parsed(&self) -> &ParsedPlan {
        &self.parsed
    }

    // Source format detection
    pub fn source_format(&self) -> PlanSourceFormat {
        match &self.source {
            PlanSource::Text { .. } => PlanSourceFormat::Text,
            PlanSource::Json { .. } => PlanSourceFormat::Json,
        }
    }

    // Backward compatibility methods
    pub fn timestamp(&self) -> DateTime<Utc> {
        self.timestamp
    }

    pub fn duration_ms(&self) -> f64 {
        self.duration_ms
    }

    pub fn query_text(&self) -> &str {
        &self.query_text
    }

    pub fn is_text_plan(&self) -> bool {
        matches!(self.source, PlanSource::Text { .. })
    }

    pub fn is_json_plan(&self) -> bool {
        matches!(self.source, PlanSource::Json { .. })
    }

    // For backwards compatibility - get plan text representation
    pub fn plan_text(&self) -> &str {
        self.raw_plan()
    }

    // For backwards compatibility - get plan lines (text format only)
    pub fn plan_lines(&self) -> &[PlanLine] {
        match &self.source {
            PlanSource::Text { plan_lines, .. } => plan_lines,
            PlanSource::Json { .. } => &[], // JSON doesn't have plan lines
        }
    }

    // Access format-specific data
    pub fn as_text_plan(&self) -> Option<(&str, &[PlanLine])> {
        match &self.source {
            PlanSource::Text {
                raw_text,
                plan_lines,
            } => Some((raw_text, plan_lines)),
            _ => None,
        }
    }

    pub fn as_json_plan(&self) -> Option<(&str, &JsonPlan)> {
        match &self.source {
            PlanSource::Json {
                raw_json,
                parsed_json,
            } => Some((raw_json, parsed_json)),
            _ => None,
        }
    }

    /// Universal constructor that parses during creation
    pub fn new(
        timestamp: DateTime<Utc>,
        duration_ms: f64,
        query_text: String,
        raw_plan: String,
    ) -> Result<Self, anyhow::Error> {
        // Process query text during construction
        let regex = regex::Regex::new(r"\$\d+").unwrap();
        let normalized_query = normalize_query(&query_text, &regex).into_owned();
        let formatted_query = format_sql_query(&query_text);

        // Parse plan based on format
        let source = if Self::is_json_format(&raw_plan) {
            let parsed_json: JsonPlan = serde_json::from_str(&raw_plan)
                .map_err(|e| anyhow::anyhow!("Failed to parse JSON plan: {}", e))?;
            PlanSource::Json {
                raw_json: raw_plan.clone(),
                parsed_json,
            }
        } else {
            let plan_lines = Self::parse_text_lines(&raw_plan);
            PlanSource::Text {
                raw_text: raw_plan.clone(),
                plan_lines,
            }
        };

        let parsed = match &source {
            PlanSource::Text { plan_lines, .. } => {
                // Use the existing structured plan_lines instead of re-parsing raw text
                let parser = crate::plan_parser::PlanParser::new()
                    .map_err(|e| anyhow::anyhow!("Failed to create plan parser: {}", e))?;
                parser
                    .parse_plan_from_lines(plan_lines)
                    .map_err(|e| anyhow::anyhow!("Failed to parse text plan: {}", e))?
            }
            PlanSource::Json { raw_json, .. } => ParsedPlan::from_json_plan(raw_json)
                .map_err(|e| anyhow::anyhow!("Failed to parse JSON plan: {}", e))?,
        };

        Ok(Self {
            timestamp,
            duration_ms,
            query_text,
            normalized_query,
            formatted_query,
            source,
            parsed,
        })
    }

    fn is_json_format(raw_plan: &str) -> bool {
        let trimmed = raw_plan.trim_start();
        trimmed.starts_with('[') || trimmed.starts_with('{')
    }

    fn parse_text_lines(raw_text: &str) -> Vec<PlanLine> {
        raw_text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| PlanLine::new(line))
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct ProcessedQuery {
    pub representative_plan: QueryPlan, // Best example (e.g., slowest execution)
    pub statistics: QueryGroupStatistics, // Only aggregated data
}

impl ProcessedQuery {
    // Convenience accessors that delegate to representative_plan
    pub fn normalized_query(&self) -> &str {
        &self.representative_plan.normalized_query
    }

    pub fn formatted_query(&self) -> &str {
        &self.representative_plan.formatted_query
    }

    pub fn raw_plan(&self) -> &str {
        self.representative_plan.raw_plan()
    }

    pub fn parsed_plan(&self) -> &ParsedPlan {
        self.representative_plan.parsed()
    }

    // Backward compatibility methods
    pub fn original_query(&self) -> &str {
        &self.representative_plan.query_text
    }

    pub fn plan(&self) -> &str {
        self.raw_plan()
    }
}

#[derive(Debug, Clone)]
pub struct PerformancePercentiles {
    pub p25: f64,
    pub p50: f64,
    pub p90: f64,
    pub p95: f64,
    pub p99: f64,
}

#[derive(Debug, Clone)]
pub struct HourlyMetrics {
    pub count: usize,
    pub total_duration_ms: f64,
    pub min_duration_ms: f64,
    pub max_duration_ms: f64,
    pub mean_duration_ms: f64,
}

#[derive(Debug, Clone)]
pub struct QueryGroupStatistics {
    pub count: usize,
    pub total_duration_ms: f64,
    pub min_duration_ms: f64,
    pub max_duration_ms: f64,
    pub mean_duration_ms: f64,
    pub std_dev_ms: f64,
    pub min_timestamp: DateTime<Utc>,
    pub max_timestamp: DateTime<Utc>,
    pub percentiles: PerformancePercentiles,
    pub hourly_histogram: HashMap<DateTime<Utc>, HourlyMetrics>, // Key: Hour-truncated UTC datetime
    pub executions: Vec<QueryPlan>,
}

#[derive(Debug, Clone)]
pub struct DateFilter {
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
}

impl DateFilter {
    pub fn new(since: Option<DateTime<Utc>>, until: Option<DateTime<Utc>>) -> Self {
        Self { since, until }
    }

    pub fn matches(&self, timestamp: DateTime<Utc>) -> bool {
        if let Some(since) = self.since {
            if timestamp < since {
                return false;
            }
        }

        if let Some(until) = self.until {
            if timestamp > until {
                return false;
            }
        }

        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn test_date_filter_matches() {
        let now = Utc::now();
        let one_hour_ago = now - chrono::Duration::hours(1);
        let two_hours_ago = now - chrono::Duration::hours(2);
        let one_hour_later = now + chrono::Duration::hours(1);

        // Test no filter (should match everything)
        let filter = DateFilter::new(None, None);
        assert!(filter.matches(two_hours_ago));
        assert!(filter.matches(now));
        assert!(filter.matches(one_hour_later));

        // Test since filter only
        let filter = DateFilter::new(Some(one_hour_ago), None);
        assert!(!filter.matches(two_hours_ago));
        assert!(filter.matches(now));
        assert!(filter.matches(one_hour_later));

        // Test until filter only
        let filter = DateFilter::new(None, Some(now));
        assert!(filter.matches(two_hours_ago));
        assert!(filter.matches(now));
        assert!(!filter.matches(one_hour_later));

        // Test both filters
        let filter = DateFilter::new(Some(one_hour_ago), Some(now));
        assert!(!filter.matches(two_hours_ago));
        assert!(filter.matches(now));
        assert!(!filter.matches(one_hour_later));
    }

    #[test]
    fn test_query_plan_enum_interface() {
        let now = Utc::now();

        // Test TextPlan variant
        let text_plan = QueryPlan::new(
            now,
            100.5,
            "SELECT * FROM users".to_string(),
            "Seq Scan on users".to_string(),
        )
        .unwrap();
        assert!(text_plan.is_text_plan());
        assert!(!text_plan.is_json_plan());
        assert_eq!(text_plan.timestamp(), now);
        assert_eq!(text_plan.duration_ms(), 100.5);
        assert_eq!(text_plan.query_text(), "SELECT * FROM users");
        assert_eq!(text_plan.plan_text(), "Seq Scan on users");
        assert!(text_plan.as_text_plan().is_some());
        assert!(text_plan.as_json_plan().is_none());
    }

    #[test]
    fn test_query_plan_builder() {
        let now = Utc::now();
        let builder = QueryPlanBuilder::new(now, 250.0);

        assert_eq!(builder.query_text().len(), 0);

        // Test text plan finalization
        let plan_content = "  Seq Scan on users  (cost=0.00..10.00 rows=100 width=8)";

        let mut builder_clone = builder.clone();
        builder_clone.set_query_text("SELECT * FROM users".to_string());
        let text_builder = builder_clone.convert_to_text();

        if let QueryPlanBuilder::Text(typed_builder) = text_builder {
            let (updated_builder, maybe_plan) = typed_builder.add_line(plan_content).unwrap();
            // Text plans don't auto-complete, so we should get None and need to finalize
            assert!(maybe_plan.is_none());

            let result = updated_builder.finalize();
            assert!(result.is_ok());
            let query_plan = result.unwrap();
            assert!(query_plan.is_text_plan());
            assert_eq!(query_plan.query_text(), "SELECT * FROM users");
        } else {
            panic!("Expected text builder");
        }
    }

    #[test]
    fn test_json_plan_builder() {
        let now = Utc::now();
        let mut builder = QueryPlanBuilder::new(now, 150.0);
        builder.set_query_text("SELECT id FROM users".to_string());
        let json_builder = builder.convert_to_json();

        // Test JSON plan with simple structure
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

        if let QueryPlanBuilder::Json(mut typed_builder) = json_builder {
            let mut final_plan = None;

            // Add JSON content line by line
            for line in json_content.lines() {
                let (updated_builder, maybe_plan) = typed_builder.add_line(line).unwrap();
                typed_builder = updated_builder;
                if let Some(plan) = maybe_plan {
                    final_plan = Some(plan);
                    break; // JSON parsing completed
                }
            }

            let query_plan = final_plan.expect("Should have parsed JSON plan");
            assert!(query_plan.is_json_plan());
            assert_eq!(query_plan.query_text(), "SELECT id FROM users");
            assert_eq!(query_plan.duration_ms(), 150.0);

            if let Some(json_data) = query_plan.as_json_plan() {
                assert_eq!(json_data.parsed_json.plan.node_type, "Seq Scan");
                assert_eq!(
                    json_data.parsed_json.plan.relation_name,
                    Some("users".to_string())
                );
                assert_eq!(json_data.parsed_json.plan.startup_cost, 0.0);
                assert_eq!(json_data.parsed_json.plan.total_cost, 10.0);
                assert_eq!(json_data.parsed_json.plan.plan_rows, 100);
                assert_eq!(json_data.parsed_json.plan.plan_width, 8);
            } else {
                panic!("Expected JsonPlan variant");
            }
        } else {
            panic!("Expected JSON builder");
        }
    }

    #[test]
    fn test_format_detection() {
        assert_eq!(PlanFormat::Text, PlanFormat::Text);
        assert_eq!(PlanFormat::Json, PlanFormat::Json);
        assert_ne!(PlanFormat::Text, PlanFormat::Json);
    }
}
