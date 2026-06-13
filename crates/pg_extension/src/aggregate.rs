//! Bridges the embeddable core parser/analyzers to the cumulative-statistics
//! rows the extension persists. Pure Rust: no Postgres calls happen here, so it
//! is safe to run on the single backend thread.

use pg_loganalyze_core::analysis::analyzers::{
    IndexUsageAnalyzer, JoinAnalyzer, QueryPatternAnalyzer, RowEstimationAnalyzer, ScanAnalyzer,
    StartupCostAnalyzer,
};
use pg_loganalyze_core::analysis::{engine::AnalysisEngineBuilder, AnalysisContext};
use pg_loganalyze_core::{query_plan_from_capture, PostgreSQLLogParser, ProcessedQuery, QueryPlan};
use std::collections::HashMap;

/// One captured execution from the in-process hook (Phase 2b).
pub struct Capture {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub duration_ms: f64,
    pub query_text: String,
    /// EXPLAIN (FORMAT TEXT) output rendered in-process.
    pub plan_text: String,
    /// Core `queryId` (`compute_query_id`), 0 when unavailable (PG13, or the
    /// GUC is off). Lets rows join to `pg_stat_statements`.
    pub query_id: i64,
}

/// One hour-bucket of executions for a single fingerprint.
pub struct HistBucket {
    /// Unix epoch seconds (fractional) of the hour-truncated bucket start.
    pub bucket_epoch: f64,
    pub calls: i64,
    pub total_time_ms: f64,
    pub min_time_ms: f64,
    pub max_time_ms: f64,
}

/// One query group's aggregate, in a form whose timing fields merge trivially
/// (add for sums, min/max for extremes) into the cumulative tables.
pub struct StatRow {
    pub fingerprint: String,
    /// Core `queryId` of the representative execution (`None` when unavailable).
    pub query_id: Option<i64>,
    pub normalized_query: String,
    pub representative_sql: String,
    /// Raw plan text of the slowest-seen execution.
    pub representative_plan: String,
    pub calls: i64,
    pub total_time_ms: f64,
    /// Sum of squared durations, used to derive a population stddev later.
    pub sum_sq_time_ms: f64,
    pub min_time_ms: f64,
    pub max_time_ms: f64,
    /// Unix epoch seconds (fractional) for the earliest/latest execution.
    pub first_seen_epoch: f64,
    pub last_seen_epoch: f64,
    /// Rich analysis of the representative plan (same data the TUI shows),
    /// serialized as JSON. `None` if the analyzer produced nothing.
    pub complexity: Option<serde_json::Value>,
    pub metadata: Option<serde_json::Value>,
    pub plan_analysis: Option<serde_json::Value>,
    /// Per-hour execution histogram for this fingerprint in this batch.
    pub histogram: Vec<HistBucket>,
}

/// Parse a chunk of auto_explain log text and reduce it to one [`StatRow`] per
/// distinct query fingerprint, computing the same per-group analysis the TUI
/// renders (complexity, metadata, plan findings) for the representative plan.
pub fn aggregate_log(log_text: &str) -> Vec<StatRow> {
    if log_text.trim().is_empty() {
        return Vec::new();
    }

    let mut parser = PostgreSQLLogParser::new();
    let plans = match parser.parse_string_with_progress(log_text, |_, _| {}) {
        Ok(plans) => plans,
        // A malformed chunk yields no statistics rather than aborting.
        Err(_) => return Vec::new(),
    };
    if plans.is_empty() {
        return Vec::new();
    }

    // Log mode carries no core queryId, so the side table is empty.
    let qid_by_text = HashMap::new();
    let processed = parser.get_processed_queries(&plans);
    processed
        .into_iter()
        .map(|(fingerprint, pq)| build_stat_row(&parser, fingerprint, pq, &qid_by_text))
        .collect()
}

/// Reduce a batch of in-process captures (Phase 2b) to one [`StatRow`] per
/// distinct fingerprint, using the same grouping/analysis as [`aggregate_log`].
pub fn aggregate_captures(captures: Vec<Capture>) -> Vec<StatRow> {
    let mut parser = PostgreSQLLogParser::new();
    // Side table of query text → core queryId, so the representative row can
    // carry the queryId without threading it through the core parser.
    let mut qid_by_text: HashMap<String, i64> = HashMap::new();
    let plans: Vec<QueryPlan> = captures
        .into_iter()
        .filter_map(|c| {
            if c.query_id != 0 {
                qid_by_text.insert(c.query_text.clone(), c.query_id);
            }
            query_plan_from_capture(c.timestamp, c.duration_ms, c.query_text, &c.plan_text).ok()
        })
        .collect();
    if plans.is_empty() {
        return Vec::new();
    }
    let processed = parser.get_processed_queries(&plans);
    processed
        .into_iter()
        .map(|(fingerprint, pq)| build_stat_row(&parser, fingerprint, pq, &qid_by_text))
        .collect()
}

/// Build the cumulative-stats row for one fingerprint group. Shared by the log
/// and in-process capture paths so both persist identical data.
fn build_stat_row(
    parser: &PostgreSQLLogParser,
    fingerprint: String,
    pq: ProcessedQuery,
    qid_by_text: &HashMap<String, i64>,
) -> StatRow {
    let stats = &pq.statistics;
    // Exact sum of squares from the per-execution records, so cumulative merges
    // stay exact. (The summary view's E[X^2]-E[X]^2 stddev is fine for realistic
    // ms-scale latencies; a Welford form is a Phase 3 option.)
    let sum_sq: f64 = stats
        .executions
        .iter()
        .map(|e| e.duration_ms * e.duration_ms)
        .sum();

    // Rich analysis of the representative (slowest) plan — the same analyzers the
    // TUI runs. Failures degrade to NULL, never abort.
    let complexity = parser
        .analyze_complexity(&pq.representative_plan)
        .and_then(|c| serde_json::to_value(c).ok());
    let metadata = parser
        .extract_metadata(&pq.representative_plan)
        .and_then(|m| serde_json::to_value(m).ok());
    let plan_analysis = run_plan_analysis(&pq.representative_plan);

    let histogram = stats
        .hourly_histogram
        .iter()
        .map(|(bucket, m)| HistBucket {
            bucket_epoch: epoch_secs(*bucket),
            calls: m.count as i64,
            total_time_ms: m.total_duration_ms,
            min_time_ms: m.min_duration_ms,
            max_time_ms: m.max_duration_ms,
        })
        .collect();

    let representative_sql = pq.representative_plan.query_text().to_string();
    let query_id = qid_by_text.get(&representative_sql).copied();

    StatRow {
        fingerprint,
        query_id,
        normalized_query: pq.representative_plan.normalized_query.clone(),
        representative_sql: representative_sql.clone(),
        representative_plan: pq.representative_plan.raw_plan().to_string(),
        calls: stats.count as i64,
        total_time_ms: stats.total_duration_ms,
        sum_sq_time_ms: sum_sq,
        min_time_ms: stats.min_duration_ms,
        max_time_ms: stats.max_duration_ms,
        first_seen_epoch: epoch_secs(stats.min_timestamp),
        last_seen_epoch: epoch_secs(stats.max_timestamp),
        complexity,
        metadata,
        plan_analysis,
        histogram,
    }
}

/// Run the plan analysis engine (the same analyzer set the TUI uses) over a
/// representative plan and serialize the combined findings.
fn run_plan_analysis(plan: &pg_loganalyze_core::QueryPlan) -> Option<serde_json::Value> {
    let engine = AnalysisEngineBuilder::new()
        .add_analyzer(RowEstimationAnalyzer::new())
        .add_analyzer(ScanAnalyzer::new())
        .add_analyzer(JoinAnalyzer::new())
        .add_analyzer(QueryPatternAnalyzer::new())
        .add_analyzer(StartupCostAnalyzer::new())
        .add_analyzer(IndexUsageAnalyzer::new())
        .build();
    let result = engine.analyze(&plan.parsed, &AnalysisContext::new());
    // EngineResult isn't Serialize, but its combined findings are.
    serde_json::to_value(result.combined_result).ok()
}

fn epoch_secs(ts: chrono::DateTime<chrono::Utc>) -> f64 {
    ts.timestamp_micros() as f64 / 1_000_000.0
}
