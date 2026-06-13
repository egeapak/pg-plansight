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

use aggregate::StatRow;

/// UPSERT that folds one batch's per-group aggregate into the running totals.
/// Every stored counter is additive (or a min/max), so merging is exact.
const UPSERT_SQL: &str = r#"
INSERT INTO loganalyze.statements
    (fingerprint, normalized_query, representative_sql, calls,
     total_time_ms, sum_sq_time_ms, min_time_ms, max_time_ms, first_seen, last_seen)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, to_timestamp($9), to_timestamp($10))
ON CONFLICT (fingerprint) DO UPDATE SET
    calls          = loganalyze.statements.calls + EXCLUDED.calls,
    total_time_ms  = loganalyze.statements.total_time_ms + EXCLUDED.total_time_ms,
    sum_sq_time_ms = loganalyze.statements.sum_sq_time_ms + EXCLUDED.sum_sq_time_ms,
    min_time_ms    = LEAST(loganalyze.statements.min_time_ms, EXCLUDED.min_time_ms),
    max_time_ms    = GREATEST(loganalyze.statements.max_time_ms, EXCLUDED.max_time_ms),
    representative_sql = CASE
        WHEN EXCLUDED.max_time_ms >= loganalyze.statements.max_time_ms
            THEN EXCLUDED.representative_sql
            ELSE loganalyze.statements.representative_sql
    END,
    first_seen = LEAST(loganalyze.statements.first_seen, EXCLUDED.first_seen),
    last_seen  = GREATEST(loganalyze.statements.last_seen, EXCLUDED.last_seen)
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
            let StatRow {
                fingerprint,
                normalized_query,
                representative_sql,
                calls,
                total_time_ms,
                sum_sq_time_ms,
                min_time_ms,
                max_time_ms,
                first_seen_epoch,
                last_seen_epoch,
            } = row;
            client.update(
                UPSERT_SQL,
                None,
                &[
                    fingerprint.into(),
                    normalized_query.into(),
                    representative_sql.into(),
                    (*calls).into(),
                    (*total_time_ms).into(),
                    (*sum_sq_time_ms).into(),
                    (*min_time_ms).into(),
                    (*max_time_ms).into(),
                    (*first_seen_epoch).into(),
                    (*last_seen_epoch).into(),
                ],
            )?;
            written += 1;
        }
        Ok::<(), spi::Error>(())
    })
    .unwrap_or_else(|e| error!("loganalyze_ingest: failed to persist statistics: {e}"));

    written
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
