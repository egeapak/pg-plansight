-- Cumulative per-query-group statistics, analogous to pg_stat_statements but
-- with the richer analysis pg-plansight computes (plan findings, complexity,
-- metadata) and a per-fingerprint time-bucketed histogram.
--
-- Timing counters are stored as trivially-mergeable aggregates so each ingest
-- batch folds into the running totals with a single UPSERT. The representative
-- (slowest-seen) plan and its analysis are replaced whenever a slower example
-- arrives.

CREATE SCHEMA IF NOT EXISTS plansight;

CREATE TABLE plansight.statements (
    -- Stable fingerprint of the normalized query (from the core normalizer).
    fingerprint        text             PRIMARY KEY,
    -- Core queryId (compute_query_id) of the representative execution, for
    -- joining to pg_stat_statements. NULL when unavailable (PG13, GUC off, or
    -- log-mode ingest). Meaningful only within the database that produced the
    -- representative plan (queryId embeds relation OIDs; the grouping fingerprint
    -- is DB-agnostic).
    query_id           bigint,
    normalized_query   text             NOT NULL,
    -- Slowest-seen example query + its raw plan text. The pretty-printed form
    -- is derived on demand via plansight_format(representative_sql), so it is
    -- never stored redundantly.
    representative_sql  text            NOT NULL,
    representative_plan text            NOT NULL,
    calls              bigint           NOT NULL DEFAULT 0,
    total_time_ms      double precision NOT NULL DEFAULT 0,
    -- Sum of squared durations; lets the summary view derive a population
    -- standard deviation without storing every execution.
    sum_sq_time_ms     double precision NOT NULL DEFAULT 0,
    -- Always supplied by the ingest INSERT; NOT NULL enforces the invariant the
    -- LEAST/GREATEST merge in the UPSERT relies on.
    min_time_ms        double precision NOT NULL,
    max_time_ms        double precision NOT NULL,
    -- Cumulative count of captured executions that exceeded the active
    -- plansight.slo_threshold_ms at aggregation time (additive). 0 when the
    -- SLO GUC was disabled.
    slo_breaches       bigint           NOT NULL DEFAULT 0,
    first_seen         timestamptz      NOT NULL DEFAULT now(),
    last_seen          timestamptz      NOT NULL DEFAULT now(),
    -- Rich analysis of the representative plan (the same data the TUI shows),
    -- recomputed by the core analyzers and refreshed when the representative
    -- changes. JSONB so it is directly queryable.
    complexity         jsonb,
    metadata           jsonb,
    plan_analysis      jsonb
);

-- Per-fingerprint execution histogram, bucketed by hour. Additive, so it folds
-- across ingests and across the eventual background-worker flushes. Doubles as
-- the time series for regression analysis and the TUI timeline chart.
CREATE TABLE plansight.query_histogram (
    fingerprint   text             NOT NULL
                      REFERENCES plansight.statements(fingerprint) ON DELETE CASCADE,
    bucket        timestamptz      NOT NULL,  -- hour-truncated
    calls         bigint           NOT NULL,
    total_time_ms double precision NOT NULL,
    min_time_ms   double precision NOT NULL,
    max_time_ms   double precision NOT NULL,
    PRIMARY KEY (fingerprint, bucket)
);

CREATE INDEX query_histogram_bucket_idx ON plansight.query_histogram (bucket);

-- How far the background worker has consumed each tailed log file. Advanced in
-- the same transaction as the stats it produced, so restarts never double-count.
CREATE TABLE plansight.ingest_offset (
    log_path    text        PRIMARY KEY,
    byte_offset bigint      NOT NULL,
    updated_at  timestamptz NOT NULL DEFAULT now()
);

-- Human-friendly view that derives mean/stddev and carries the rich analysis.
CREATE VIEW plansight.statements_summary AS
SELECT
    fingerprint,
    query_id,
    normalized_query,
    representative_sql,
    representative_plan,
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
    -- Coefficient of variation = stddev / mean (unitless). NULL-safe via
    -- NULLIF on calls and on the mean (a 0 mean yields NULL, not div-by-zero).
    sqrt(
        GREATEST(
            0.0,
            sum_sq_time_ms / NULLIF(calls, 0)
                - power(total_time_ms / NULLIF(calls, 0), 2)
        )
    ) / NULLIF(total_time_ms / NULLIF(calls, 0), 0) AS cv,
    slo_breaches,
    slo_breaches::double precision / NULLIF(calls, 0) AS slo_breach_pct,
    first_seen,
    last_seen,
    complexity,
    metadata,
    plan_analysis
FROM plansight.statements;

-- Convenience: the slowest query groups by cumulative time.
CREATE VIEW plansight.top_by_total_time AS
SELECT *
FROM plansight.statements_summary
ORDER BY total_time_ms DESC;

-- Per-bucket timeline with derived mean, for charting and regression.
CREATE VIEW plansight.query_timeline AS
SELECT
    fingerprint,
    bucket,
    calls,
    total_time_ms,
    total_time_ms / NULLIF(calls, 0) AS mean_time_ms,
    min_time_ms,
    max_time_ms
FROM plansight.query_histogram
ORDER BY fingerprint, bucket;
