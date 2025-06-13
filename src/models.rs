use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct QueryStatistics {
    pub total_queries: usize,
    pub total_duration_ms: f64,
    pub average_duration_ms: f64,
    pub slowest_query_duration_ms: f64,
    pub unique_queries: usize,
    pub most_frequent_queries: Vec<(String, usize)>,
    pub slowest_queries: Vec<QueryPlan>,
}

#[derive(Debug, PartialEq)]
pub enum ParsingState {
    None,
    WaitingForQuery,
    ParsingPlan,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp: DateTime<Utc>,
    pub process_id: u32,
    pub log_level: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryPlan {
    pub timestamp: DateTime<Utc>,
    pub duration_ms: f64,
    pub query_text: String,
    pub plan: String,
    pub parameters: Option<String>,
}
