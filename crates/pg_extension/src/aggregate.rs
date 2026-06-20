//! Bridges the embeddable core parser/analyzers to the cumulative-statistics
//! rows the extension persists. Pure Rust: no Postgres calls happen here, so it
//! is safe to run on the single backend thread.

use pg_plansight_core::analysis::analyzers::{
    BufferWalAnalyzer, EstimationHealthAnalyzer, FilterEfficiencyAnalyzer, IndexEfficiencyAnalyzer,
    IndexUsageAnalyzer, JoinAnalyzer, PlanShapeAnalyzer, QueryPatternAnalyzer,
    RowEstimationAnalyzer, ScanAnalyzer, SortMemoryAnalyzer, StartupCostAnalyzer,
};
use pg_plansight_core::analysis::{engine::AnalysisEngineBuilder, AnalysisContext};
use pg_plansight_core::{query_plan_from_capture, PostgreSQLLogParser, ProcessedQuery, QueryPlan};
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
    /// Count of captured executions whose duration exceeded the active
    /// `plansight.slo_threshold_ms` at capture time. Additive across merges; 0
    /// when the SLO GUC is disabled (threshold <= 0). Independent of timing
    /// counters so the threshold can change over the life of a fingerprint.
    pub slo_breaches: i64,
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

// These derived helpers mirror the `statements_summary` SQL view's formulas so
// the math is unit-tested in pure Rust; the canonical surfacing is the view, so
// outside tests they are unused (the build is otherwise warning-clean).
#[cfg_attr(not(any(test, feature = "pg_test")), allow(dead_code))]
impl StatRow {
    /// Population standard deviation of execution time (ms), derived from the
    /// stored sums: sqrt(max(0, E[X^2] - E[X]^2)). Returns 0.0 when there are
    /// no calls (zero guard) or when fewer than one effective sample exists.
    pub fn stddev_time_ms(&self) -> f64 {
        Self::stddev(self.calls, self.total_time_ms, self.sum_sq_time_ms)
    }

    /// Coefficient of variation = stddev / mean. Returns 0.0 when calls == 0 or
    /// when the mean is 0.0 (zero guard — avoids NaN/inf). Unitless.
    pub fn cv(&self) -> f64 {
        Self::coeff_of_variation(self.calls, self.total_time_ms, self.sum_sq_time_ms)
    }

    /// Fraction (0.0–1.0) of captured executions that breached the SLO.
    /// 0.0 when calls == 0.
    pub fn slo_breach_pct(&self) -> f64 {
        if self.calls <= 0 {
            return 0.0;
        }
        self.slo_breaches as f64 / self.calls as f64
    }

    /// Population stddev from the three stored sums, with a zero/negative guard.
    fn stddev(calls: i64, total: f64, sum_sq: f64) -> f64 {
        if calls <= 0 {
            return 0.0;
        }
        let n = calls as f64;
        let mean = total / n;
        // E[X^2] - E[X]^2, floored at 0 so float error can't yield a NaN sqrt.
        let var = (sum_sq / n - mean * mean).max(0.0);
        var.sqrt()
    }

    /// CV from the three stored sums; 0.0 when calls == 0 or mean == 0.
    fn coeff_of_variation(calls: i64, total: f64, sum_sq: f64) -> f64 {
        if calls <= 0 {
            return 0.0;
        }
        let mean = total / calls as f64;
        if mean == 0.0 {
            return 0.0;
        }
        Self::stddev(calls, total, sum_sq) / mean
    }

    /// Count durations strictly greater than `threshold_ms`. A threshold <= 0.0
    /// means "SLO disabled" → always 0 (so an unset GUC never inflates the
    /// count). Pure; unit-tested. NaN durations never count (NaN > x is false).
    pub fn count_slo_breaches(durations_ms: &[f64], threshold_ms: f64) -> i64 {
        if threshold_ms <= 0.0 {
            return 0;
        }
        durations_ms.iter().filter(|&&d| d > threshold_ms).count() as i64
    }

    /// Combine two partial breach counts (the additive merge the UPSERT
    /// performs). Trivial, but named so the accumulation invariant is
    /// unit-tested in Rust.
    pub fn merge_slo_breaches(a: i64, b: i64) -> i64 {
        a + b
    }
}

/// Parse a chunk of auto_explain log text and reduce it to one [`StatRow`] per
/// distinct query fingerprint, computing the same per-group analysis the TUI
/// renders (complexity, metadata, plan findings) for the representative plan.
pub fn aggregate_log(log_text: &str, slo_threshold_ms: f64) -> Vec<StatRow> {
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
    let qid_by_norm = HashMap::new();
    let processed = parser.get_processed_queries(&plans);
    processed
        .into_iter()
        .map(|(fingerprint, pq)| {
            build_stat_row(&parser, fingerprint, pq, &qid_by_norm, slo_threshold_ms)
        })
        .collect()
}

/// Reduce a batch of in-process captures (Phase 2b) to one [`StatRow`] per
/// distinct fingerprint, using the same grouping/analysis as [`aggregate_log`].
pub fn aggregate_captures(captures: Vec<Capture>, slo_threshold_ms: f64) -> Vec<StatRow> {
    let mut parser = PostgreSQLLogParser::new();
    // Map normalized query → core queryId (any non-zero in the group). Keying on
    // the normalized form (shared by all executions of a fingerprint) means the
    // representative row gets the group's id even if its own execution recorded
    // id 0, and avoids threading the id through the core parser.
    let mut qid_by_norm: HashMap<String, i64> = HashMap::new();
    let plans: Vec<QueryPlan> = captures
        .into_iter()
        .filter_map(|c| {
            let qid = c.query_id;
            let plan =
                query_plan_from_capture(c.timestamp, c.duration_ms, c.query_text, &c.plan_text)
                    .ok()?;
            if qid != 0 {
                qid_by_norm
                    .entry(plan.normalized_query.clone())
                    .or_insert(qid);
            }
            Some(plan)
        })
        .collect();
    if plans.is_empty() {
        return Vec::new();
    }
    let processed = parser.get_processed_queries(&plans);
    processed
        .into_iter()
        .map(|(fingerprint, pq)| {
            build_stat_row(&parser, fingerprint, pq, &qid_by_norm, slo_threshold_ms)
        })
        .collect()
}

/// Build the cumulative-stats row for one fingerprint group. Shared by the log
/// and in-process capture paths so both persist identical data.
fn build_stat_row(
    parser: &PostgreSQLLogParser,
    fingerprint: String,
    pq: ProcessedQuery,
    qid_by_norm: &HashMap<String, i64>,
    slo_threshold_ms: f64,
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

    // Count executions whose duration exceeded the active SLO threshold. The
    // counting rule lives in one tested place (`count_slo_breaches`); a
    // threshold <= 0 disables it (yields 0).
    let durations: Vec<f64> = stats.executions.iter().map(|e| e.duration_ms).collect();
    let slo_breaches = StatRow::count_slo_breaches(&durations, slo_threshold_ms);

    // Rich analysis of the representative (slowest) plan — the same analyzers the
    // TUI runs. Failures degrade to NULL, never abort. Stats-only captures
    // (capture_plan=off) carry no plan, so there is nothing to analyze: store
    // NULL rather than an empty analysis, and skip the analyzer work entirely.
    let has_plan = !pq.representative_plan.raw_plan().trim().is_empty();
    let (complexity, metadata, plan_analysis) = if has_plan {
        (
            parser
                .analyze_complexity(&pq.representative_plan)
                .and_then(|c| serde_json::to_value(c).ok()),
            parser
                .extract_metadata(&pq.representative_plan)
                .and_then(|m| serde_json::to_value(m).ok()),
            run_plan_analysis(&pq.representative_plan),
        )
    } else {
        (None, None, None)
    };

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
    let query_id = qid_by_norm
        .get(&pq.representative_plan.normalized_query)
        .copied();

    StatRow {
        fingerprint,
        query_id,
        normalized_query: pq.representative_plan.normalized_query.clone(),
        representative_sql,
        representative_plan: pq.representative_plan.raw_plan().to_string(),
        calls: stats.count as i64,
        total_time_ms: stats.total_duration_ms,
        sum_sq_time_ms: sum_sq,
        min_time_ms: stats.min_duration_ms,
        max_time_ms: stats.max_duration_ms,
        slo_breaches,
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
fn run_plan_analysis(plan: &pg_plansight_core::QueryPlan) -> Option<serde_json::Value> {
    let engine = AnalysisEngineBuilder::new()
        .add_analyzer(RowEstimationAnalyzer::new())
        .add_analyzer(ScanAnalyzer::new())
        .add_analyzer(JoinAnalyzer::new())
        .add_analyzer(QueryPatternAnalyzer::new())
        .add_analyzer(StartupCostAnalyzer::new())
        .add_analyzer(IndexUsageAnalyzer::new())
        .add_analyzer(BufferWalAnalyzer::new())
        .add_analyzer(SortMemoryAnalyzer::new())
        .add_analyzer(FilterEfficiencyAnalyzer::new())
        .add_analyzer(IndexEfficiencyAnalyzer::new())
        .add_analyzer(PlanShapeAnalyzer::new())
        .add_analyzer(EstimationHealthAnalyzer::new())
        .build();
    let result = engine.analyze(&plan.parsed, &AnalysisContext::new());
    // EngineResult isn't Serialize, but its combined findings are.
    serde_json::to_value(result.combined_result).ok()
}

fn epoch_secs(ts: chrono::DateTime<chrono::Utc>) -> f64 {
    ts.timestamp_micros() as f64 / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_plan_does_not_panic() {
        // The worker runs aggregate_captures over captured (possibly truncated
        // or odd) plan text; it must never panic, only yield fewer/zero rows.
        let cap = Capture {
            timestamp: chrono::Utc::now(),
            duration_ms: 1.0,
            query_text: "definitely not sql ;;;".to_string(),
            plan_text: "\u{0}garbage\nnot a plan  (cost=??) actual\n  ->  ???".to_string(),
            query_id: 0,
        };
        let rows = aggregate_captures(vec![cap], 0.0);
        // No panic is the assertion; row count is unconstrained.
        let _ = rows.len();
    }

    #[test]
    fn stats_only_capture_records_row_without_plan_or_analysis() {
        // capture_plan=off pushes an empty plan; the capture must still produce a
        // stats row (timing/calls) but with no plan and no plan analysis.
        let cap = Capture {
            timestamp: chrono::Utc::now(),
            duration_ms: 12.5,
            query_text: "SELECT * FROM orders WHERE id = 7".to_string(),
            plan_text: String::new(),
            query_id: 42,
        };
        let rows = aggregate_captures(vec![cap], 0.0);
        assert_eq!(rows.len(), 1, "stats-only capture still yields a row");
        let r = &rows[0];
        assert_eq!(r.calls, 1);
        assert_eq!(r.max_time_ms, 12.5);
        assert!(r.representative_plan.is_empty(), "no plan stored");
        assert!(r.plan_analysis.is_none(), "no analysis without a plan");
        assert!(r.complexity.is_none());
        assert_eq!(r.query_id, Some(42));
    }

    #[test]
    fn test_cv_computation() {
        // Durations [10.0, 20.0]: calls=2, total=30.0, sum_sq=500.0.
        // mean = 15.0, var = 500/2 - 225 = 25.0, stddev = 5.0, cv = 5/15.
        assert_eq!(StatRow::stddev(2, 30.0, 500.0), 5.0);
        let cv = StatRow::coeff_of_variation(2, 30.0, 500.0);
        assert!((cv - (5.0 / 15.0)).abs() < 1e-9);

        // Single call: stddev 0 → cv 0.
        assert_eq!(StatRow::coeff_of_variation(1, 10.0, 100.0), 0.0);

        // Zero guard (calls == 0).
        assert_eq!(StatRow::coeff_of_variation(0, 0.0, 0.0), 0.0);
        assert_eq!(StatRow::stddev(0, 0.0, 0.0), 0.0);

        // Zero-mean guard (calls=2, total=0.0).
        assert_eq!(StatRow::coeff_of_variation(2, 0.0, 0.0), 0.0);

        // The public &self accessors must agree with the static helpers (these
        // mirror the SQL view's formulas).
        let mut row = make_empty_stat_row();
        row.calls = 2;
        row.total_time_ms = 30.0;
        row.sum_sq_time_ms = 500.0;
        assert_eq!(row.stddev_time_ms(), 5.0);
        assert!((row.cv() - (5.0 / 15.0)).abs() < 1e-9);
    }

    #[test]
    fn test_slo_breach_count() {
        // 200, 300 exceed 100 → 2.
        assert_eq!(StatRow::count_slo_breaches(&[10.0, 200.0, 300.0], 100.0), 2);
        // Disabled threshold (0) → 0.
        assert_eq!(StatRow::count_slo_breaches(&[10.0, 200.0, 300.0], 0.0), 0);
        // Negative threshold → 0.
        assert_eq!(StatRow::count_slo_breaches(&[200.0], -5.0), 0);
        // Strictly greater: a duration equal to the threshold does not count.
        assert_eq!(StatRow::count_slo_breaches(&[100.0], 100.0), 0);
        // Empty.
        assert_eq!(StatRow::count_slo_breaches(&[], 100.0), 0);
    }

    #[test]
    fn test_slo_breach_merge() {
        assert_eq!(StatRow::merge_slo_breaches(2, 3), 5);
        // End-to-end: counts over two partial slices sum to the count over the
        // concatenation.
        let a = StatRow::count_slo_breaches(&[10.0, 200.0], 100.0); // 1
        let b = StatRow::count_slo_breaches(&[300.0, 50.0], 100.0); // 1
        assert_eq!(StatRow::merge_slo_breaches(a, b), 2);
        // Zero/guard negatives are inert under the additive merge.
        assert_eq!(StatRow::merge_slo_breaches(0, 0), 0);
        assert_eq!(
            StatRow::merge_slo_breaches(
                StatRow::count_slo_breaches(&[200.0], 0.0),
                StatRow::count_slo_breaches(&[200.0], -1.0),
            ),
            0,
        );
    }

    #[test]
    fn test_slo_breach_pct() {
        let mut row = make_empty_stat_row();
        row.calls = 4;
        row.slo_breaches = 1;
        assert!((row.slo_breach_pct() - 0.25).abs() < 1e-9);

        row.calls = 0;
        assert_eq!(row.slo_breach_pct(), 0.0);
    }

    /// Build a bare `StatRow` for testing the derived helpers without parsing.
    fn make_empty_stat_row() -> StatRow {
        StatRow {
            fingerprint: String::new(),
            query_id: None,
            normalized_query: String::new(),
            representative_sql: String::new(),
            representative_plan: String::new(),
            calls: 0,
            total_time_ms: 0.0,
            sum_sq_time_ms: 0.0,
            min_time_ms: 0.0,
            max_time_ms: 0.0,
            slo_breaches: 0,
            first_seen_epoch: 0.0,
            last_seen_epoch: 0.0,
            complexity: None,
            metadata: None,
            plan_analysis: None,
            histogram: Vec::new(),
        }
    }
}
