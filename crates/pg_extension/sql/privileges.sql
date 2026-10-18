-- Lock down the privileged / mutating functions so they are NOT EXECUTE-able by
-- PUBLIC. PostgreSQL grants EXECUTE to PUBLIC on every newly created function by
-- default; without these REVOKEs any role (or a future blanket schema GRANT)
-- could silently:
--   * plansight_reset()        -- TRUNCATE ... CASCADE (destroys all stats)
--   * plansight_ingest(text)   -- inject arbitrary "log" text into the stats
--   * plansight_pgss_view()    -- (re)create the pg_stat_statements join view
--   * plansight_reset_stats()  -- wipe the self-overhead accumulator
-- Emitted with `finalize` (see lib.rs) so this runs AFTER pgrx has created the
-- functions; the read-only observability functions (plansight_capture_stats,
-- plansight_check, plansight_format, plansight_fingerprint,
-- plansight_capture_timings) are intentionally left executable. Grant EXECUTE back to specific roles as needed.
REVOKE ALL ON FUNCTION plansight_reset() FROM PUBLIC;
REVOKE ALL ON FUNCTION plansight_ingest(text) FROM PUBLIC;
REVOKE ALL ON FUNCTION plansight_pgss_view() FROM PUBLIC;
REVOKE ALL ON FUNCTION plansight_reset_stats() FROM PUBLIC;

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
