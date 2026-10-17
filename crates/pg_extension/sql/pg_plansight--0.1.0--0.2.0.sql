/* pg_plansight 0.1.0 -> 0.2.0
 *
 * Adds plansight_fingerprint() and re-keys the captured statistics: the
 * grouping fingerprint moved from Rust's unspecified DefaultHasher to xxh3 so
 * the id is stable across toolchains and joins to the CLI export and the
 * exporter. Existing rows are re-keyed from their representative_sql, which is
 * what the capture path hashes, so new captures keep folding into them.
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
 * fingerprint (the normalizer now collapses IN lists of different lengths), and
 * the new module may already have written rows under the new key between the
 * restart and this UPDATE. Merged rows sum their counters, keep the slowest
 * example's plan and analysis, and span both seen-timestamps. Each DELETE feeds
 * a grouped INSERT, which consumes every deleted row before inserting. */
ALTER TABLE plansight.query_histogram DROP CONSTRAINT query_histogram_fingerprint_fkey;

WITH moved AS (
    DELETE FROM plansight.query_histogram RETURNING *
), fingerprint_map AS (
    SELECT fingerprint AS old_fingerprint,
           plansight_fingerprint(representative_sql) AS new_fingerprint
      FROM plansight.statements
)
INSERT INTO plansight.query_histogram
    (fingerprint, bucket, calls, total_time_ms, min_time_ms, max_time_ms)
SELECT m.new_fingerprint, moved.bucket, sum(moved.calls), sum(moved.total_time_ms),
       min(moved.min_time_ms), max(moved.max_time_ms)
  FROM moved
  JOIN fingerprint_map m ON m.old_fingerprint = moved.fingerprint
 GROUP BY m.new_fingerprint, moved.bucket;

WITH moved AS (
    DELETE FROM plansight.statements RETURNING *
)
INSERT INTO plansight.statements
    (fingerprint, query_id, normalized_query, representative_sql, representative_plan,
     calls, total_time_ms, sum_sq_time_ms, min_time_ms, max_time_ms, slo_breaches,
     first_seen, last_seen, complexity, metadata, plan_analysis)
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
       (array_agg(plan_analysis ORDER BY max_time_ms DESC))[1]
  FROM moved
 GROUP BY plansight_fingerprint(representative_sql);

ALTER TABLE plansight.query_histogram
    ADD CONSTRAINT query_histogram_fingerprint_fkey
    FOREIGN KEY (fingerprint) REFERENCES plansight.statements(fingerprint) ON DELETE CASCADE;
