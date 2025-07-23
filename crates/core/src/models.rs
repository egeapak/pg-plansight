use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::{ParsedPlan, format_plan_lines, get_indent_level};

#[derive(Debug, PartialEq)]
pub enum ParsingState {
    None,
    WaitingForQuery(QueryPlan),
    ParsingQuery(QueryPlan),
    ParsingPlan(QueryPlan),
}

impl ParsingState {
    pub fn reset(&mut self, plan: QueryPlan) -> Option<QueryPlan> {
        std::mem::replace(self, ParsingState::WaitingForQuery(plan)).take_plan()
    }

    pub fn finish(&mut self) -> Option<QueryPlan> {
        std::mem::replace(self, ParsingState::None).take_plan()
    }

    fn take_plan(self) -> Option<QueryPlan> {
        match self {
            Self::None => None,
            Self::WaitingForQuery(query_plan) => Some(query_plan),
            Self::ParsingQuery(query_plan) => Some(query_plan),
            Self::ParsingPlan(query_plan) => Some(query_plan),
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
            query: line.trim().to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QueryPlan {
    pub timestamp: DateTime<Utc>,
    pub duration_ms: f64,
    pub query_text: String,
    pub plan: String,
    pub plan_lines: Vec<PlanLine>,
}

impl QueryPlan {
    pub fn parse_line(&mut self, _line: &str, _state: &ParsingState) {}

    pub fn finalize(mut self, plan_lines: &[PlanLine]) -> Self {
        self.plan = format_plan_lines(plan_lines);
        self.plan_lines = plan_lines.to_vec();
        self
    }
}

#[derive(Debug, Clone)]
pub struct ProcessedQuery {
    pub original_query: String,
    pub plan: String,
    pub parsed_plan: Option<ParsedPlan>,
    pub normalized_query: String,
    pub formatted_query: String,
    pub statistics: QueryGroupStatistics,
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
}
