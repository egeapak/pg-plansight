/* pg_plansight 0.1.0 -> 0.2.0
 *
 * Adds plansight_fingerprint() and the `database` column, and re-keys the
 * captured statistics: the grouping fingerprint moved from Rust's unspecified
 * DefaultHasher to xxh3 so the id is stable across toolchains and joins to the
 * CLI export and the exporter, and rows are now keyed by (fingerprint,
 * database). Existing rows are re-keyed from their representative_sql, which is
 * what the capture path hashes, so new captures keep folding into them; their
 * database is current_database() (0.1.0 hook capture only persisted its own
 * database; log-ingested rows were never attributed).
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

/* Re-key by merging, not renaming: several 0.1.0 rows can map to one new
 * fingerprint (the normalizer now collapses IN lists of different lengths).
 * Merged rows sum their counters, keep the slowest example's plan and analysis,
 * and span both seen-timestamps. Each DELETE feeds a grouped INSERT, which
 * consumes every deleted row before inserting. */
ALTER TABLE plansight.statements ADD COLUMN database text;
UPDATE plansight.statements SET database = current_database()::text;
ALTER TABLE plansight.statements ALTER COLUMN database SET NOT NULL;

ALTER TABLE plansight.query_histogram ADD COLUMN database text;
UPDATE plansight.query_histogram SET database = current_database()::text;
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
