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
-- plansight_check, plansight_format, plansight_capture_timings) are intentionally
-- left executable. Grant EXECUTE back to specific roles as needed.
REVOKE ALL ON FUNCTION plansight_reset() FROM PUBLIC;
REVOKE ALL ON FUNCTION plansight_ingest(text) FROM PUBLIC;
REVOKE ALL ON FUNCTION plansight_pgss_view() FROM PUBLIC;
REVOKE ALL ON FUNCTION plansight_reset_stats() FROM PUBLIC;
