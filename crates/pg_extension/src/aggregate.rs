//! Bridges the embeddable core parser to the cumulative-statistics rows the
//! extension persists. Pure Rust: no Postgres calls happen here, so it is safe
//! to run on the single backend thread.

use pg_loganalyze_core::PostgreSQLLogParser;

/// One query group's aggregate, in a form whose every field merges trivially
/// (add for sums, min/max for extremes) into the cumulative table.
pub struct StatRow {
    pub fingerprint: String,
    pub normalized_query: String,
    pub representative_sql: String,
    pub calls: i64,
    pub total_time_ms: f64,
    /// Sum of squared durations, used to derive a population stddev later.
    pub sum_sq_time_ms: f64,
    pub min_time_ms: f64,
    pub max_time_ms: f64,
    /// Unix epoch seconds (fractional) for the earliest/latest execution.
    pub first_seen_epoch: f64,
    pub last_seen_epoch: f64,
}

/// Parse a chunk of auto_explain log text and reduce it to one [`StatRow`] per
/// distinct query fingerprint.
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

    parser
        .get_processed_queries(&plans)
        .into_iter()
        .map(|(fingerprint, pq)| {
            let stats = &pq.statistics;
            // Sum of squared durations, computed exactly from the per-execution
            // records (not reconstructed from mean/stddev). This is an additive
            // counter, so cumulative merges stay exact. NOTE: the summary view
            // derives stddev as E[X^2] - E[X]^2, which can lose precision for
            // pathologically large means with tiny variance; for realistic
            // query latencies (ms) this is well within f64's exact-integer
            // range. A streaming/Welford form is a Phase 3 option if needed.
            let sum_sq: f64 = stats
                .executions
                .iter()
                .map(|e| e.duration_ms * e.duration_ms)
                .sum();

            StatRow {
                fingerprint,
                normalized_query: pq.representative_plan.normalized_query.clone(),
                representative_sql: pq.representative_plan.query_text().to_string(),
                calls: stats.count as i64,
                total_time_ms: stats.total_duration_ms,
                sum_sq_time_ms: sum_sq,
                min_time_ms: stats.min_duration_ms,
                max_time_ms: stats.max_duration_ms,
                first_seen_epoch: epoch_secs(stats.min_timestamp),
                last_seen_epoch: epoch_secs(stats.max_timestamp),
            }
        })
        .collect()
}

fn epoch_secs(ts: chrono::DateTime<chrono::Utc>) -> f64 {
    ts.timestamp_micros() as f64 / 1_000_000.0
}
