//! `pg_loganalyze` — a PostgreSQL extension that captures cumulative
//! auto_explain query statistics and exposes them via SQL.
//!
//! Phase 1 (this module) provides the SQL-queryable surface and a manual
//! ingest entry point: `loganalyze_ingest(text)` parses a chunk of
//! auto_explain log output with the shared core parser, groups it by query
//! fingerprint, and folds the per-group aggregates into the cumulative
//! `loganalyze.statements` table. Later phases add automatic in-process
//! capture (an `ExecutorEnd` hook + background-worker flush).

use pgrx::prelude::*;

::pgrx::pg_module_magic!(name, version);

// Ship the schema (tables + views) as part of the extension, before any
// function that references it.
extension_sql_file!("../sql/schema.sql", name = "loganalyze_schema", bootstrap);

mod aggregate;

/// UPSERT that folds one batch's per-group aggregate into the running totals.
/// Timing counters are additive (or a min/max); the representative plan and its
/// analysis are replaced whenever a batch's slowest execution is at least as
/// slow as the stored representative.
const UPSERT_SQL: &str = r#"
INSERT INTO loganalyze.statements
    (fingerprint, normalized_query, representative_sql, representative_plan, calls,
     total_time_ms, sum_sq_time_ms, min_time_ms, max_time_ms, first_seen, last_seen,
     complexity, metadata, plan_analysis)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, to_timestamp($10), to_timestamp($11),
        $12, $13, $14)
ON CONFLICT (fingerprint) DO UPDATE SET
    calls          = loganalyze.statements.calls + EXCLUDED.calls,
    total_time_ms  = loganalyze.statements.total_time_ms + EXCLUDED.total_time_ms,
    sum_sq_time_ms = loganalyze.statements.sum_sq_time_ms + EXCLUDED.sum_sq_time_ms,
    min_time_ms    = LEAST(loganalyze.statements.min_time_ms, EXCLUDED.min_time_ms),
    max_time_ms    = GREATEST(loganalyze.statements.max_time_ms, EXCLUDED.max_time_ms),
    first_seen     = LEAST(loganalyze.statements.first_seen, EXCLUDED.first_seen),
    last_seen      = GREATEST(loganalyze.statements.last_seen, EXCLUDED.last_seen),
    -- Refresh the representative + its analysis only when this batch's slowest
    -- execution is at least as slow as the stored one.
    representative_sql  = CASE WHEN EXCLUDED.max_time_ms >= loganalyze.statements.max_time_ms
                              THEN EXCLUDED.representative_sql  ELSE loganalyze.statements.representative_sql  END,
    representative_plan = CASE WHEN EXCLUDED.max_time_ms >= loganalyze.statements.max_time_ms
                              THEN EXCLUDED.representative_plan ELSE loganalyze.statements.representative_plan END,
    complexity          = CASE WHEN EXCLUDED.max_time_ms >= loganalyze.statements.max_time_ms
                              THEN EXCLUDED.complexity          ELSE loganalyze.statements.complexity          END,
    metadata            = CASE WHEN EXCLUDED.max_time_ms >= loganalyze.statements.max_time_ms
                              THEN EXCLUDED.metadata            ELSE loganalyze.statements.metadata            END,
    plan_analysis       = CASE WHEN EXCLUDED.max_time_ms >= loganalyze.statements.max_time_ms
                              THEN EXCLUDED.plan_analysis       ELSE loganalyze.statements.plan_analysis       END
"#;

/// UPSERT for one (fingerprint, hour-bucket) histogram row. Additive.
const HISTOGRAM_UPSERT_SQL: &str = r#"
INSERT INTO loganalyze.query_histogram
    (fingerprint, bucket, calls, total_time_ms, min_time_ms, max_time_ms)
VALUES ($1, to_timestamp($2), $3, $4, $5, $6)
ON CONFLICT (fingerprint, bucket) DO UPDATE SET
    calls         = loganalyze.query_histogram.calls + EXCLUDED.calls,
    total_time_ms = loganalyze.query_histogram.total_time_ms + EXCLUDED.total_time_ms,
    min_time_ms   = LEAST(loganalyze.query_histogram.min_time_ms, EXCLUDED.min_time_ms),
    max_time_ms   = GREATEST(loganalyze.query_histogram.max_time_ms, EXCLUDED.max_time_ms)
"#;

/// Parse a chunk of auto_explain log output and fold its query statistics into
/// the cumulative `loganalyze.statements` table. Returns the number of distinct
/// query groups written.
///
/// This is the manual ingest path: useful for importing existing logs and for
/// testing. Automatic in-process capture arrives in a later phase.
#[pg_extern]
fn loganalyze_ingest(log_text: &str) -> i64 {
    let rows = aggregate::aggregate_log(log_text);
    if rows.is_empty() {
        return 0;
    }

    let mut written = 0i64;
    Spi::connect_mut(|client| {
        for row in &rows {
            // Statement row first so the histogram's FK is satisfied within the
            // same transaction.
            client.update(
                UPSERT_SQL,
                None,
                &[
                    row.fingerprint.as_str().into(),
                    row.normalized_query.as_str().into(),
                    row.representative_sql.as_str().into(),
                    row.representative_plan.as_str().into(),
                    row.calls.into(),
                    row.total_time_ms.into(),
                    row.sum_sq_time_ms.into(),
                    row.min_time_ms.into(),
                    row.max_time_ms.into(),
                    row.first_seen_epoch.into(),
                    row.last_seen_epoch.into(),
                    row.complexity.clone().map(pgrx::JsonB).into(),
                    row.metadata.clone().map(pgrx::JsonB).into(),
                    row.plan_analysis.clone().map(pgrx::JsonB).into(),
                ],
            )?;

            for b in &row.histogram {
                client.update(
                    HISTOGRAM_UPSERT_SQL,
                    None,
                    &[
                        row.fingerprint.as_str().into(),
                        b.bucket_epoch.into(),
                        b.calls.into(),
                        b.total_time_ms.into(),
                        b.min_time_ms.into(),
                        b.max_time_ms.into(),
                    ],
                )?;
            }
            written += 1;
        }
        Ok::<(), spi::Error>(())
    })
    .unwrap_or_else(|e| error!("loganalyze_ingest: failed to persist statistics: {e}"));

    written
}

/// Pretty-print a SQL statement using the same formatter the analyzer/TUI uses.
/// We store only the raw `representative_sql`; callers format on demand, e.g.
/// `SELECT loganalyze_format(representative_sql) FROM loganalyze.statements`.
#[pg_extern(immutable, parallel_safe)]
fn loganalyze_format(sql: &str) -> String {
    pg_loganalyze_core::format_sql_query(sql)
}

/// Discard all accumulated statistics, like `pg_stat_statements_reset()`.
#[pg_extern]
fn loganalyze_reset() {
    Spi::run("TRUNCATE loganalyze.statements").unwrap_or_else(|e| error!("loganalyze_reset: {e}"));
}

#[cfg(any(test, feature = "pg_test"))]
#[pg_schema]
mod tests {
    use pgrx::prelude::*;

    const SAMPLE_LOG: &str = "2025-06-25 00:03:51.601 UTC [1] LOG:  duration: 10.5 ms  plan:\n\
         Query Text: SELECT * FROM orders WHERE id = 1\n\
         Seq Scan on orders  (cost=0.00..1.10 rows=10 width=4)\n\
         2025-06-25 00:03:52.601 UTC [1] LOG:  duration: 20.5 ms  plan:\n\
         Query Text: SELECT * FROM orders WHERE id = 2\n\
         Seq Scan on orders  (cost=0.00..1.10 rows=10 width=4)\n";

    #[pg_test]
    fn ingest_then_query_summary() {
        // Two executions of the same normalized query => one group, calls = 2.
        let written = crate::loganalyze_ingest(SAMPLE_LOG);
        assert_eq!(written, 1, "two literals normalize to one group");

        let calls = Spi::get_one::<i64>("SELECT calls FROM loganalyze.statements")
            .expect("query failed")
            .expect("a row");
        assert_eq!(calls, 2);

        let total = Spi::get_one::<f64>("SELECT total_time_ms FROM loganalyze.statements")
            .expect("query failed")
            .expect("a row");
        assert!(
            (total - 31.0).abs() < 1e-6,
            "10.5 + 20.5 = 31.0, got {total}"
        );

        let max = Spi::get_one::<f64>("SELECT max_time_ms FROM loganalyze.statements_summary")
            .expect("query failed")
            .expect("a row");
        assert!((max - 20.5).abs() < 1e-6);
    }

    #[pg_test]
    fn ingest_is_cumulative_across_calls() {
        crate::loganalyze_ingest(SAMPLE_LOG);
        crate::loganalyze_ingest(SAMPLE_LOG);

        let calls = Spi::get_one::<i64>("SELECT calls FROM loganalyze.statements")
            .expect("query failed")
            .expect("a row");
        assert_eq!(calls, 4, "two ingests of two executions each");
    }

    #[pg_test]
    fn reset_clears_statistics() {
        crate::loganalyze_ingest(SAMPLE_LOG);
        crate::loganalyze_reset();
        let count = Spi::get_one::<i64>("SELECT count(*) FROM loganalyze.statements")
            .expect("query failed")
            .expect("a row");
        assert_eq!(count, 0);
    }

    #[pg_test]
    fn empty_input_writes_nothing() {
        assert_eq!(crate::loganalyze_ingest(""), 0);
    }

    #[pg_test]
    fn rich_analysis_is_persisted() {
        crate::loganalyze_ingest(SAMPLE_LOG);
        // The representative plan text and the analyzer outputs are stored.
        let has_plan = Spi::get_one::<bool>(
            "SELECT representative_plan LIKE '%Seq Scan%' FROM loganalyze.statements",
        )
        .expect("query failed")
        .expect("a row");
        assert!(has_plan, "representative plan text should be stored");

        let analysis_present = Spi::get_one::<bool>(
            "SELECT plan_analysis IS NOT NULL AND complexity IS NOT NULL \
             FROM loganalyze.statements",
        )
        .expect("query failed")
        .expect("a row");
        assert!(
            analysis_present,
            "complexity + plan_analysis should be populated"
        );
    }

    #[pg_test]
    fn histogram_is_populated_and_additive() {
        crate::loganalyze_ingest(SAMPLE_LOG);
        let buckets = Spi::get_one::<i64>("SELECT count(*) FROM loganalyze.query_histogram")
            .expect("query failed")
            .expect("a row");
        assert!(buckets >= 1, "at least one hour bucket expected");

        let total_calls =
            Spi::get_one::<i64>("SELECT coalesce(sum(calls),0) FROM loganalyze.query_histogram")
                .expect("query failed")
                .expect("a row");
        assert_eq!(total_calls, 2, "histogram calls match the 2 executions");

        // Re-ingest: histogram folds additively, not a new row per ingest.
        crate::loganalyze_ingest(SAMPLE_LOG);
        let total_calls2 =
            Spi::get_one::<i64>("SELECT coalesce(sum(calls),0) FROM loganalyze.query_histogram")
                .expect("query failed")
                .expect("a row");
        assert_eq!(total_calls2, 4);
    }

    #[pg_test]
    fn format_function_pretty_prints() {
        let formatted = crate::loganalyze_format("select a,b from t where x=1");
        // sqlparser uppercases keywords when pretty-printing.
        assert!(
            formatted.contains("SELECT") && formatted.contains("FROM"),
            "expected pretty-printed SQL, got: {formatted}"
        );
    }
}

/// Required by `cargo pgrx test`.
#[cfg(test)]
pub mod pg_test {
    pub fn setup(_options: Vec<&str>) {}

    #[must_use]
    pub fn postgresql_conf_options() -> Vec<&'static str> {
        vec![]
    }
}
