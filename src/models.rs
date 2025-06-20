use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::{format_plan_lines, get_indent_level};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

impl QueryPlan {
    pub fn parse_line(&mut self, line: &str, state: &ParsingState) {}

    pub fn finalize(mut self, plan_lines: &[PlanLine]) -> Self {
        self.plan = format_plan_lines(plan_lines);
        self
    }
}

#[derive(Debug, Clone)]
pub struct ProcessedQuery {
    pub original_query: String,
    pub plan: String,
    pub normalized_query: String,
    pub formatted_query: String,
    pub statistics: QueryGroupStatistics,
}

#[derive(Debug, Clone)]
pub struct QueryGroupStatistics {
    pub count: usize,
    pub total_duration_ms: f64,
    pub min_duration_ms: f64,
    pub max_duration_ms: f64,
    pub mean_duration_ms: f64,
    pub std_dev_ms: f64,
    pub executions: Vec<QueryPlan>,
}
