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

ALTER TABLE plansight.query_histogram DROP CONSTRAINT query_histogram_fingerprint_fkey;

DO $rekey$
DECLARE
    stat record;
BEGIN
    FOR stat IN
        SELECT fingerprint AS old_fingerprint,
               plansight_fingerprint(representative_sql) AS new_fingerprint
          FROM plansight.statements
    LOOP
        UPDATE plansight.query_histogram SET fingerprint = stat.new_fingerprint
         WHERE fingerprint = stat.old_fingerprint;
        UPDATE plansight.statements SET fingerprint = stat.new_fingerprint
         WHERE fingerprint = stat.old_fingerprint;
    END LOOP;
END
$rekey$;

ALTER TABLE plansight.query_histogram
    ADD CONSTRAINT query_histogram_fingerprint_fkey
    FOREIGN KEY (fingerprint) REFERENCES plansight.statements(fingerprint) ON DELETE CASCADE;
