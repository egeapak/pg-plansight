-- Cumulative per-query-group statistics, analogous to pg_stat_statements.
-- Counters are stored as trivially-mergeable aggregates so each ingest batch
-- folds into the running totals with a single UPSERT.

CREATE SCHEMA IF NOT EXISTS loganalyze;

CREATE TABLE loganalyze.statements (
    -- Stable fingerprint of the normalized query (from the core normalizer).
    fingerprint        text             PRIMARY KEY,
    normalized_query   text             NOT NULL,
    -- Example SQL for the slowest execution seen so far.
    representative_sql text             NOT NULL,
    calls              bigint           NOT NULL DEFAULT 0,
    total_time_ms      double precision NOT NULL DEFAULT 0,
    -- Sum of squared durations; lets the summary view derive a population
    -- standard deviation without storing every execution.
    sum_sq_time_ms     double precision NOT NULL DEFAULT 0,
    -- Always supplied by the ingest INSERT; NOT NULL enforces the invariant the
    -- LEAST/GREATEST merge in the UPSERT relies on.
    min_time_ms        double precision NOT NULL,
    max_time_ms        double precision NOT NULL,
    first_seen         timestamptz      NOT NULL DEFAULT now(),
    last_seen          timestamptz      NOT NULL DEFAULT now()
);

-- Human-friendly view that derives mean/stddev from the stored aggregates.
CREATE VIEW loganalyze.statements_summary AS
SELECT
    fingerprint,
    normalized_query,
    representative_sql,
    calls,
    total_time_ms,
    total_time_ms / NULLIF(calls, 0)                AS mean_time_ms,
    min_time_ms,
    max_time_ms,
    sqrt(
        GREATEST(
            0.0,
            sum_sq_time_ms / NULLIF(calls, 0)
                - power(total_time_ms / NULLIF(calls, 0), 2)
        )
    )                                               AS stddev_time_ms,
    first_seen,
    last_seen
FROM loganalyze.statements;

-- Convenience: the slowest query groups by cumulative time.
CREATE VIEW loganalyze.top_by_total_time AS
SELECT *
FROM loganalyze.statements_summary
ORDER BY total_time_ms DESC;
