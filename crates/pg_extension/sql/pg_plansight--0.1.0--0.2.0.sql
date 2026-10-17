/* pg_plansight 0.1.0 -> 0.2.0
 *
 * Adds plansight_fingerprint() and the `database` column, and re-keys the
 * captured statistics: the grouping fingerprint moved from Rust's unspecified
 * DefaultHasher to xxh3 so the id is stable across toolchains and joins to the
 * CLI export and the exporter, and rows are now keyed by (fingerprint,
 * database). Existing rows are re-keyed from their representative_sql, which is
 * what the capture path hashes, so new captures keep folding into them; their
 * database is current_database() (0.1.0 hook capture only persisted its own
 * database), or 'unknown' when capture_mode is 'log' (log-ingested rows were
 * never attributed; new rows get the %d of the server's log_line_prefix).
 *
 * Run ALTER EXTENSION UPDATE right after restarting onto the new module: until
 * then the new module's writes fail against the old columns.
 *
 * Rewrites both tables under an ACCESS EXCLUSIVE lock.
 *
 * Everything else in 0.2.0 is below SQL level (executor hooks in src/nesting.c,
 * capture defaults as GUCs).
 *
 * The file has to exist: `default_version` in the .control file tracks the
 * crate version, and without an update path ALTER EXTENSION UPDATE fails and
 * the only way forward is DROP EXTENSION, which discards every statistic.
 * cargo-pgrx and the deb/rpm asset globs pick up any
 * sql/pg_plansight--<old>--<new>.sql. version-check.yml fails a release that
 * has no upgrade script targeting it.
 */
CREATE FUNCTION plansight_fingerprint(sql text) RETURNS text
    IMMUTABLE PARALLEL SAFE STRICT
    LANGUAGE c AS 'MODULE_PATHNAME', 'plansight_fingerprint_wrapper';

/* plansight_check() gained a `code` column. */
DROP FUNCTION plansight_check();
CREATE FUNCTION plansight_check()
    RETURNS TABLE (severity text, code text, category text, message text)
    STRICT
    LANGUAGE c AS 'MODULE_PATHNAME', 'plansight_check_wrapper';

/* Re-key by merging, not renaming: several 0.1.0 rows can map to one new
 * fingerprint (the normalizer now collapses IN lists of different lengths).
 * Merged rows sum their counters, keep the slowest example's plan and analysis,
 * and span both seen-timestamps. Each DELETE feeds a grouped INSERT, which
 * consumes every deleted row before inserting. */
ALTER TABLE plansight.statements ADD COLUMN database text;
-- Log-mode rows were never attributed; hook-mode rows only ever came from here.
UPDATE plansight.statements SET database =
    CASE WHEN current_setting('plansight.capture_mode', true) = 'log'
         THEN 'unknown' ELSE current_database()::text END;
ALTER TABLE plansight.statements ALTER COLUMN database SET NOT NULL;

ALTER TABLE plansight.query_histogram ADD COLUMN database text;
UPDATE plansight.query_histogram SET database =
    CASE WHEN current_setting('plansight.capture_mode', true) = 'log'
         THEN 'unknown' ELSE current_database()::text END;
ALTER TABLE plansight.query_histogram ALTER COLUMN database SET NOT NULL;

ALTER TABLE plansight.query_histogram DROP CONSTRAINT query_histogram_fingerprint_fkey;
ALTER TABLE plansight.query_histogram DROP CONSTRAINT query_histogram_pkey;
ALTER TABLE plansight.statements DROP CONSTRAINT statements_pkey;

WITH moved AS (
    DELETE FROM plansight.query_histogram RETURNING *
), fingerprint_map AS (
    SELECT fingerprint AS old_fingerprint, database,
           plansight_fingerprint(representative_sql) AS new_fingerprint
      FROM plansight.statements
)
INSERT INTO plansight.query_histogram
    (fingerprint, bucket, calls, total_time_ms, min_time_ms, max_time_ms, database)
SELECT m.new_fingerprint, moved.bucket, sum(moved.calls), sum(moved.total_time_ms),
       min(moved.min_time_ms), max(moved.max_time_ms), moved.database
  FROM moved
  JOIN fingerprint_map m
    ON m.old_fingerprint = moved.fingerprint AND m.database = moved.database
 GROUP BY m.new_fingerprint, moved.database, moved.bucket;

WITH moved AS (
    DELETE FROM plansight.statements RETURNING *
)
INSERT INTO plansight.statements
    (fingerprint, query_id, normalized_query, representative_sql, representative_plan,
     calls, total_time_ms, sum_sq_time_ms, min_time_ms, max_time_ms, slo_breaches,
     first_seen, last_seen, complexity, metadata, plan_analysis, database)
SELECT plansight_fingerprint(representative_sql),
       (array_agg(query_id ORDER BY max_time_ms DESC))[1],
       (array_agg(normalized_query ORDER BY max_time_ms DESC))[1],
       (array_agg(representative_sql ORDER BY max_time_ms DESC))[1],
       (array_agg(representative_plan ORDER BY max_time_ms DESC))[1],
       sum(calls), sum(total_time_ms), sum(sum_sq_time_ms),
       min(min_time_ms), max(max_time_ms), sum(slo_breaches),
       min(first_seen), max(last_seen),
       (array_agg(complexity ORDER BY max_time_ms DESC))[1],
       (array_agg(metadata ORDER BY max_time_ms DESC))[1],
       (array_agg(plan_analysis ORDER BY max_time_ms DESC))[1],
       database
  FROM moved
 GROUP BY plansight_fingerprint(representative_sql), database;

ALTER TABLE plansight.statements ADD PRIMARY KEY (fingerprint, database);
ALTER TABLE plansight.query_histogram ADD PRIMARY KEY (fingerprint, database, bucket);
ALTER TABLE plansight.query_histogram
    ADD CONSTRAINT query_histogram_fingerprint_fkey
    FOREIGN KEY (fingerprint, database)
    REFERENCES plansight.statements (fingerprint, database) ON DELETE CASCADE;

-- `database` is appended last so CREATE OR REPLACE keeps the existing columns.
CREATE OR REPLACE VIEW plansight.statements_summary AS
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

CREATE OR REPLACE VIEW plansight.top_by_total_time AS
SELECT *
FROM plansight.statements_summary
ORDER BY total_time_ms DESC;

CREATE OR REPLACE VIEW plansight.query_timeline AS
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

ALTER TABLE plansight.statements ADD COLUMN latency_buckets bigint[];

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
    WHERE w.minute >= now() - make_interval(
                mins => coalesce(nullif(current_setting('plansight.metrics_window_minutes', true), '')::int, 15))
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
    FROM plansight.metrics_window w
    WHERE w.minute >= now() - make_interval(
                mins => coalesce(nullif(current_setting('plansight.metrics_window_minutes', true), '')::int, 15))
    GROUP BY w.database, w.fingerprint
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

-- Metrics over plansight_capture_stats(); here because it needs the function.
CREATE VIEW plansight.metrics_capture AS
SELECT * FROM plansight_capture_stats();

-- The metrics views are readable by monitoring roles without granting access to
-- the statements (queries, plans) behind them: views run with the owner's rights.
GRANT USAGE ON SCHEMA plansight TO pg_read_all_stats;
GRANT SELECT ON plansight.metrics, plansight.metrics_latency_buckets,
                plansight.metrics_recent, plansight.metrics_capture
    TO pg_read_all_stats;

-- Install-time configuration check (CREATE EXTENSION and ALTER EXTENSION UPDATE):
-- warnings and info reach the client as WARNING / NOTICE, and an error-level
-- finding (capture is enabled but cannot work) aborts the statement. The same
-- findings are available later from plansight_check().
DO $plansight_check$
DECLARE
    finding record;
    problems text := '';
BEGIN
    FOR finding IN SELECT severity, category, message FROM plansight_check() LOOP
        IF finding.severity = 'error' THEN
            problems := problems || E'\n  - ' || finding.category || ': ' || finding.message;
        ELSIF finding.severity = 'warning' THEN
            RAISE WARNING 'pg_plansight: %: %', finding.category, finding.message;
        ELSIF finding.severity = 'info' THEN
            RAISE NOTICE 'pg_plansight: %: %', finding.category, finding.message;
        END IF;
    END LOOP;
    IF problems <> '' THEN
        RAISE EXCEPTION 'pg_plansight: configuration errors:%', problems
            USING HINT = 'Fix the settings above (see plansight_check()) and retry.';
    END IF;
END
$plansight_check$;
