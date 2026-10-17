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
    -- With `database` it is the row's key: one statement in two databases is
    -- two rows. The id is `plansight_fingerprint(sql)`.
    fingerprint        text             NOT NULL,
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
    plan_analysis      jsonb,
    -- Database the statement ran in: current_database() for hook capture, the
    -- %d of the server's log_line_prefix for log ingest ('unknown' without it).
    database           text             NOT NULL,
    -- Non-cumulative latency bucket counts (see plansight.latency_bucket_bounds()
    -- plus an overflow bucket). NULL for rows captured before 0.2.0.
    latency_buckets    bigint[],
    PRIMARY KEY (fingerprint, database)
);

-- Per-fingerprint execution histogram, bucketed by hour. Additive, so it folds
-- across ingests and across the eventual background-worker flushes. Doubles as
-- the time series for regression analysis and the TUI timeline chart.
CREATE TABLE plansight.query_histogram (
    fingerprint   text             NOT NULL,
    bucket        timestamptz      NOT NULL,  -- hour-truncated
    calls         bigint           NOT NULL,
    total_time_ms double precision NOT NULL,
    min_time_ms   double precision NOT NULL,
    max_time_ms   double precision NOT NULL,
    database      text             NOT NULL,
    PRIMARY KEY (fingerprint, database, bucket),
    CONSTRAINT query_histogram_fingerprint_fkey
        FOREIGN KEY (fingerprint, database)
        REFERENCES plansight.statements (fingerprint, database) ON DELETE CASCADE
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
    plan_analysis,
    database
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
    max_time_ms,
    database
FROM plansight.query_histogram
ORDER BY fingerprint, bucket;

-- ---------------------------------------------------------------------------
-- Metrics: label-safe views over the cumulative store, for the exporter's
-- [postgres] source or any SQL-based scraper. Latency is kept as fixed
-- non-cumulative bucket counts (one per bound in plansight.latency_bucket_bounds()
-- plus an overflow bucket); changing the bounds needs a data migration.
-- ---------------------------------------------------------------------------

CREATE FUNCTION plansight.latency_bucket_bounds() RETURNS double precision[]
    LANGUAGE sql IMMUTABLE PARALLEL SAFE
    AS $$ SELECT ARRAY[0.001, 0.01, 0.1, 1, 5, 10, 30, 60, 300]::double precision[] $$;

-- Element-wise sum of two bucket arrays; NULL on either side yields the other.
CREATE FUNCTION plansight.add_bucket_arrays(a bigint[], b bigint[]) RETURNS bigint[]
    LANGUAGE sql IMMUTABLE PARALLEL SAFE
    AS $$
        SELECT CASE
            WHEN a IS NULL THEN b
            WHEN b IS NULL THEN a
            ELSE (SELECT array_agg(x + y ORDER BY i)
                    FROM unnest(a, b) WITH ORDINALITY AS t(x, y, i))
        END
    $$;

-- Per-minute counts for the last plansight.metrics_window_minutes. The worker
-- purges older rows on each flush, so this stays small whatever the cardinality
-- of history in plansight.statements.
CREATE TABLE plansight.metrics_window (
    database        text             NOT NULL,
    fingerprint     text             NOT NULL,
    minute          timestamptz      NOT NULL,
    calls           bigint           NOT NULL,
    total_time_ms   double precision NOT NULL,
    latency_buckets bigint[]         NOT NULL,
    PRIMARY KEY (database, fingerprint, minute)
);

CREATE INDEX metrics_window_minute_idx ON plansight.metrics_window (minute);

-- One row per (database, fingerprint): cumulative counters in base units.
CREATE VIEW plansight.metrics AS
SELECT
    database,
    fingerprint,
    normalized_query,
    calls,
    total_time_ms / 1000.0                   AS total_seconds,
    min_time_ms / 1000.0                     AS min_seconds,
    max_time_ms / 1000.0                     AS max_seconds,
    slo_breaches,
    extract(epoch FROM first_seen)::double precision AS first_seen_seconds,
    extract(epoch FROM last_seen)::double precision  AS last_seen_seconds
FROM plansight.statements;

-- Cumulative histogram, one row per bucket (Prometheus `le` semantics, with
-- Infinity for the last). Statements captured before 0.2.0 have no buckets.
CREATE VIEW plansight.metrics_latency_buckets AS
SELECT
    s.database,
    s.fingerprint,
    CASE WHEN o.i <= array_length(plansight.latency_bucket_bounds(), 1)
         THEN (plansight.latency_bucket_bounds())[o.i]
         ELSE 'Infinity'::double precision END AS le,
    (sum(o.n) OVER (PARTITION BY s.database, s.fingerprint ORDER BY o.i))::bigint AS count
FROM plansight.statements s
CROSS JOIN LATERAL unnest(s.latency_buckets) WITH ORDINALITY AS o(n, i);

-- The recent window per (database, fingerprint): throughput and upper-bound
-- latency percentiles (the `le` of the bucket holding the percentile).
CREATE VIEW plansight.metrics_recent AS
WITH buckets AS (
    SELECT w.database, w.fingerprint, o.i, sum(o.n) AS n
    FROM plansight.metrics_window w
    CROSS JOIN LATERAL unnest(w.latency_buckets) WITH ORDINALITY AS o(n, i)
    GROUP BY w.database, w.fingerprint, o.i
), cumulative AS (
    SELECT database, fingerprint, i,
           sum(n) OVER (PARTITION BY database, fingerprint ORDER BY i) AS c,
           sum(n) OVER (PARTITION BY database, fingerprint) AS total
    FROM buckets
), percentiles AS (
    SELECT database, fingerprint,
           min(i) FILTER (WHERE c >= 0.50 * total) AS i50,
           min(i) FILTER (WHERE c >= 0.95 * total) AS i95,
           min(i) FILTER (WHERE c >= 0.99 * total) AS i99
    FROM cumulative
    GROUP BY database, fingerprint
), totals AS (
    SELECT database, fingerprint, sum(calls) AS calls, sum(total_time_ms) AS total_time_ms,
           min(minute) AS window_start
    FROM plansight.metrics_window
    GROUP BY database, fingerprint
)
SELECT
    t.database,
    t.fingerprint,
    t.calls::bigint AS calls,
    t.total_time_ms / 1000.0 AS total_seconds,
    (t.calls / greatest(extract(epoch FROM now() - t.window_start), 60.0))::double precision
        AS calls_per_second,
    CASE WHEN p.i50 <= array_length(plansight.latency_bucket_bounds(), 1)
         THEN (plansight.latency_bucket_bounds())[p.i50] ELSE 'Infinity'::double precision END AS p50_le_seconds,
    CASE WHEN p.i95 <= array_length(plansight.latency_bucket_bounds(), 1)
         THEN (plansight.latency_bucket_bounds())[p.i95] ELSE 'Infinity'::double precision END AS p95_le_seconds,
    CASE WHEN p.i99 <= array_length(plansight.latency_bucket_bounds(), 1)
         THEN (plansight.latency_bucket_bounds())[p.i99] ELSE 'Infinity'::double precision END AS p99_le_seconds
FROM totals t
JOIN percentiles p USING (database, fingerprint);
