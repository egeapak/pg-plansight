use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::{ParsedPlan, get_indent_level};

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

#[allow(clippy::large_enum_variant)]
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
}

#[derive(Debug, Clone)]
pub struct ProcessedQuery {
    pub representative_plan: QueryPlan, // Best example (e.g., slowest execution)
    pub statistics: QueryGroupStatistics, // Only aggregated data

    // Phase 2: Advanced Analysis Features (pre-computed in post-processing)
    pub complexity_score: Option<crate::sql_analysis::ComplexityScore>,
    pub metadata: Option<crate::sql_analysis::QueryMetadata>,
    pub regression_analysis: Option<crate::sql_analysis::RegressionAnalysis>,

    // Phase 3: Plan Analysis Engine Results
    pub plan_analysis: Option<crate::analysis::engine::EngineResult>,

    // Store indices for lazy regression analysis
    pub execution_indices: Vec<usize>,
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

/// Lightweight execution record for statistics
#[derive(Debug, Clone)]
pub struct ExecutionRecord {
    pub timestamp: DateTime<Utc>,
    pub duration_ms: f64,
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
    pub executions: Vec<ExecutionRecord>, // Changed from Vec<QueryPlan> to Vec<ExecutionRecord>
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
        if let Some(since) = self.since
            && timestamp < since
        {
            return false;
        }

        if let Some(until) = self.until
            && timestamp > until
        {
            return false;
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

        // Test TextPlan variant using new parsing architecture
        use crate::parsing::{ParseMetadata, PlanFactory, PlanParserCore, TextPlanParser};

        let plan_text = r#"Index Scan using "PK_VentilatorHourlyCaches" on "Shared"."VentilatorHourlyCaches" v  (cost=0.42..851.21 rows=822 width=16)
  Output: "AcceptanceId", "MeasuredDate", "VentilatorId""#;
        let metadata = ParseMetadata::new(now, 3680.828, "SELECT v.AcceptanceId, v.MeasuredDate, v.VentilatorId FROM VentilatorHourlyCaches v".to_string());
        let parser = TextPlanParser::new().unwrap();
        let parsed_result = parser.parse(plan_text, metadata).unwrap();

        let text_plan = PlanFactory::create_query_plan_from_parsed(
            now,
            3680.828,
            "SELECT v.AcceptanceId, v.MeasuredDate, v.VentilatorId FROM VentilatorHourlyCaches v".to_string(),
            plan_text.to_string(),
            parsed_result,
        )
        .unwrap();
        assert!(text_plan.is_text_plan());
        assert!(!text_plan.is_json_plan());
        assert_eq!(text_plan.timestamp(), now);
        assert_eq!(text_plan.duration_ms(), 3680.828);
        assert!(text_plan.query_text().contains("VentilatorHourlyCaches"));
        assert_eq!(text_plan.plan_text(), plan_text);
        assert!(text_plan.as_text_plan().is_some());
        assert!(text_plan.as_json_plan().is_none());
    }
}
