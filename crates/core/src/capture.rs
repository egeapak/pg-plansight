//! Build a [`QueryPlan`] directly from an in-process plan capture (Phase 2b),
//! bypassing the auto_explain log-line wrapper.
//!
//! The executor-hook capture path produces a `(query_text, plan_text, duration,
//! timestamp)` tuple per execution. This reuses the exact text-plan parsing and
//! query normalization the log path uses, so captured executions flow through
//! the same grouping/analysis/storage as log-ingested ones.

use crate::QueryPlan;
use crate::parsing::TextPlanBuilder;
use chrono::{DateTime, Utc};

/// Construct a [`QueryPlan`] from a single captured execution.
///
/// `plan_text` is the `EXPLAIN (FORMAT TEXT)` output (the same indented form the
/// auto_explain log carries). Returns an error if the plan text cannot be
/// parsed.
pub fn query_plan_from_capture(
    timestamp: DateTime<Utc>,
    duration_ms: f64,
    query_text: String,
    plan_text: &str,
) -> anyhow::Result<QueryPlan> {
    let builder = TextPlanBuilder {
        timestamp,
        duration_ms,
        query_text,
        content_lines: plan_text.lines().map(|s| s.to_string()).collect(),
    };
    builder
        .finalize()
        .map_err(|e| anyhow::anyhow!("failed to build query plan from capture: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAN: &str = "Index Scan using orders_pkey on orders  (cost=0.29..8.31 rows=1 width=20) (actual time=0.012..0.013 rows=1 loops=1)\n  Index Cond: (id = 42)";

    #[test]
    fn builds_query_plan_from_tuple() {
        let qp = query_plan_from_capture(
            Utc::now(),
            1.5,
            "SELECT * FROM orders WHERE id = 42".to_string(),
            PLAN,
        )
        .expect("should parse");

        assert_eq!(qp.query_text(), "SELECT * FROM orders WHERE id = 42");
        assert_eq!(qp.duration_ms(), 1.5);
        assert!(qp.is_text_plan());
        // The normalizer parameterized the literal, so the fingerprint is stable
        // across different id values.
        assert!(qp.normalized_query.contains("$1") || !qp.normalized_query.contains("42"));
    }

    #[test]
    fn matches_the_log_path_fingerprint() {
        // The same execution captured in-process vs parsed from a log line must
        // normalize identically (so capture and log modes group together).
        use crate::PostgreSQLLogParser;

        let log = format!(
            "2025-06-25 00:03:51.601 UTC [1] LOG:  duration: 1.5 ms  plan:\n\tQuery Text: SELECT * FROM orders WHERE id = 42\n\t{}\n",
            PLAN.replace('\n', "\n\t")
        );
        let mut parser = PostgreSQLLogParser::new();
        let plans = parser.parse_string_with_progress(&log, |_, _| {}).unwrap();
        assert_eq!(plans.len(), 1, "log line should parse to one plan");

        let captured = query_plan_from_capture(
            Utc::now(),
            1.5,
            "SELECT * FROM orders WHERE id = 42".to_string(),
            PLAN,
        )
        .unwrap();

        assert_eq!(
            plans[0].normalized_query, captured.normalized_query,
            "capture and log paths must normalize to the same query"
        );
    }
}
