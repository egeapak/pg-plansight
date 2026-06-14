//! Build a [`QueryPlan`] directly from an in-process plan capture (Phase 2b),
//! bypassing the auto_explain log-line wrapper.
//!
//! The executor-hook capture path produces a `(query_text, plan_text, duration,
//! timestamp)` tuple per execution. This reuses the exact text-plan parsing and
//! query normalization the log path uses, so captured executions flow through
//! the same grouping/analysis/storage as log-ingested ones.

use crate::parser_utils::format_sql_query;
use crate::parsing::TextPlanBuilder;
use crate::sql_analysis::normalize_query_enhanced;
use crate::{NodeType, ParsedPlan, PlanCost, PlanNode, PlanSource, QueryPlan};
use chrono::{DateTime, Utc};

/// Construct a [`QueryPlan`] from a single captured execution.
///
/// `plan_text` is the `EXPLAIN (FORMAT TEXT)` output (the same indented form the
/// auto_explain log carries). When it is empty — the extension's *stats-only*
/// capture mode (`plansight.capture_plan = off`), which skips rendering to shed
/// the hot-path cost — a plan-less [`QueryPlan`] is built that still normalizes
/// and fingerprints by query text, so timing/calls aggregate exactly as usual
/// (just with no plan or plan analysis). Returns an error only if a *non-empty*
/// plan text cannot be parsed.
pub fn query_plan_from_capture(
    timestamp: DateTime<Utc>,
    duration_ms: f64,
    query_text: String,
    plan_text: &str,
) -> anyhow::Result<QueryPlan> {
    if plan_text.trim().is_empty() {
        return Ok(plan_less_query_plan(timestamp, duration_ms, query_text));
    }
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

/// Build a [`QueryPlan`] carrying only timing + (normalized) query text, with an
/// empty plan tree — for stats-only captures. Normalization drives grouping, so
/// these rows merge into the same fingerprints as fully-rendered captures of the
/// same query; the analyzers over the `Unknown` root simply produce no findings.
fn plan_less_query_plan(
    timestamp: DateTime<Utc>,
    duration_ms: f64,
    query_text: String,
) -> QueryPlan {
    let normalized_query = normalize_query_enhanced(&query_text)
        .map(|r| r.normalized_sql)
        // If sqlparser can't handle it, group on the raw text rather than drop it.
        .unwrap_or_else(|_| query_text.clone());
    let formatted_query = format_sql_query(&query_text);
    let root = PlanNode::new(
        NodeType::Unknown("(plan not captured)".to_string()),
        PlanCost {
            startup_cost: 0.0,
            min_total_cost: 0.0,
            max_total_cost: 0.0,
            estimated_rows: 0,
            estimated_width: 0,
        },
        String::new(),
    );
    QueryPlan {
        timestamp,
        duration_ms,
        query_text,
        normalized_query,
        formatted_query,
        source: PlanSource::Text {
            raw_text: String::new(),
            plan_lines: Vec::new(),
        },
        parsed: ParsedPlan {
            root,
            planning_time_ms: None,
            execution_time_ms: None,
        },
    }
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
    fn empty_plan_builds_plan_less_query_plan() {
        // Stats-only capture (plansight.capture_plan=off) renders no plan; the
        // capture must still produce a QueryPlan that normalizes/fingerprints by
        // text so it groups with rendered captures of the same query.
        let qp = query_plan_from_capture(
            Utc::now(),
            2.5,
            "SELECT * FROM orders WHERE id = 42".to_string(),
            "",
        )
        .expect("empty plan must not error");
        assert_eq!(qp.duration_ms(), 2.5);
        assert!(qp.raw_plan().is_empty(), "no plan text");
        // Same normalization as the rendered path, so fingerprints match.
        let rendered = query_plan_from_capture(
            Utc::now(),
            2.5,
            "SELECT * FROM orders WHERE id = 99".to_string(),
            PLAN,
        )
        .unwrap();
        assert_eq!(
            qp.normalized_query, rendered.normalized_query,
            "stats-only and rendered captures of the same shape must group together"
        );
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
