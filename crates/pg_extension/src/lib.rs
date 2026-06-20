//! `pg_plansight` — a PostgreSQL extension that captures cumulative
//! auto_explain query statistics and exposes them via SQL.
//!
//! Phase 1 (this module) provides the SQL-queryable surface and a manual
//! ingest entry point: `plansight_ingest(text)` parses a chunk of
//! auto_explain log output with the shared core parser, groups it by query
//! fingerprint, and folds the per-group aggregates into the cumulative
//! `plansight.statements` table. Later phases add automatic in-process
//! capture (an `ExecutorEnd` hook + background-worker flush).

use pgrx::guc::{GucContext, GucFlags, GucRegistry, GucSetting};
use pgrx::prelude::*;
use std::ffi::CString;

::pgrx::pg_module_magic!(name, version);

// Ship the schema (tables + views) as part of the extension, before any
// function that references it.
extension_sql_file!("../sql/schema.sql", name = "plansight_schema", bootstrap);

mod aggregate;
mod bgworker;
mod hook;
mod ring;

use aggregate::StatRow;

// ---- GUCs (configuration), all reloadable on SIGHUP ------------------------

/// Capture source for the background worker.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaptureMode {
    /// No automatic capture (manual `plansight_ingest` still works).
    Off,
    /// Phase 2a: tail the auto_explain log file (`plansight.log_path`).
    Log,
    /// Phase 2b: in-process executor hook → shmem ring, drained by the worker.
    Hook,
}

impl CaptureMode {
    fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "log" => CaptureMode::Log,
            "hook" => CaptureMode::Hook,
            _ => CaptureMode::Off,
        }
    }
}

/// How the worker captures statistics: `off`, `log` (tail auto_explain log), or
/// `hook` (in-process executor hook — Phase 2b). The two capture sources are
/// mutually exclusive to avoid double-counting the same execution.
pub(crate) static GUC_CAPTURE_MODE: GucSetting<Option<CString>> =
    GucSetting::<Option<CString>>::new(Some(c"off"));
/// Absolute path to the auto_explain log file the worker tails (`log` mode).
pub(crate) static GUC_LOG_PATH: GucSetting<Option<CString>> =
    GucSetting::<Option<CString>>::new(None);
/// Database the worker connects to (must have the extension installed).
pub(crate) static GUC_DATABASE: GucSetting<Option<CString>> =
    GucSetting::<Option<CString>>::new(Some(c"postgres"));
/// How often (seconds) the worker drains new content.
pub(crate) static GUC_FLUSH_INTERVAL: GucSetting<i32> = GucSetting::<i32>::new(10);
/// In `hook` mode, skip capturing executions faster than this (milliseconds).
pub(crate) static GUC_MIN_DURATION_MS: GucSetting<f64> = GucSetting::<f64>::new(0.0);
/// Executions whose duration exceeds this (ms) are counted as SLO breaches in
/// StatRow.slo_breaches. 0 (default) disables breach counting.
pub(crate) static GUC_SLO_THRESHOLD_MS: GucSetting<f64> = GucSetting::<f64>::new(0.0);
/// In `hook` mode, UPSERT synchronously in the backend instead of via the
/// shared-memory ring + worker. Heavy on the hot path; for tests/debug only.
pub(crate) static GUC_SYNCHRONOUS: GucSetting<bool> = GucSetting::<bool>::new(false);
/// In `hook` mode, fraction of executions to capture (0.0–1.0). Decided in
/// ExecutorStart, so unsampled queries skip timing instrumentation entirely.
pub(crate) static GUC_SAMPLE_RATE: GucSetting<f64> = GucSetting::<f64>::new(1.0);
/// In `hook` mode, also capture per-node buffer and WAL usage in the plan, which
/// the BufferWal analyzer turns into temp-spill / cache-miss / WAL findings. On
/// by default; set off to shed the executor accounting overhead.
pub(crate) static GUC_TRACK_IO: GucSetting<bool> = GucSetting::<bool>::new(true);
/// In `hook` mode, also capture queries nested inside functions/triggers. Off by
/// default (top-level only, like pg_stat_statements) to avoid double-counting.
pub(crate) static GUC_TRACK_NESTED: GucSetting<bool> = GucSetting::<bool>::new(false);
/// When on, accumulate per-phase hot-path timings (see `plansight_capture_timings`).
/// A few ns/capture when on; a single branch when off.
pub(crate) static GUC_PROFILE: GucSetting<bool> = GucSetting::<bool>::new(false);
/// In `hook` mode, include non-default planner GUCs (EXPLAIN SETTINGS) in the
/// plan. On by default; `get_explain_guc_options` scans all GUCs per render, so
/// set off to shave that from the render hot path.
pub(crate) static GUC_TRACK_SETTINGS: GucSetting<bool> = GucSetting::<bool>::new(true);
/// In `hook` mode, include per-node *timing* (`INSTRUMENT_TIMER`) in the plan.
/// On by default. Off drops the per-node `gettimeofday` accounting during
/// execution — the dominant EXPLAIN-ANALYZE overhead — and the per-node time in
/// the render, while still recording per-node row counts; whole-query duration
/// is always measured (so the `min_duration_ms` gate is unaffected).
pub(crate) static GUC_TRACK_TIMING: GucSetting<bool> = GucSetting::<bool>::new(true);
/// In `hook` mode, include the estimated cost columns (EXPLAIN `COSTS`) in the
/// rendered plan. On by default; render-only (no execution cost).
pub(crate) static GUC_TRACK_COSTS: GucSetting<bool> = GucSetting::<bool>::new(true);
/// In `hook` mode, render with EXPLAIN `VERBOSE` (output columns, schema-
/// qualified names). Off by default; render-only.
pub(crate) static GUC_TRACK_VERBOSE: GucSetting<bool> = GucSetting::<bool>::new(false);
/// In `hook` mode, render and store the query plan. On by default. Off =
/// *stats-only*: skip the EXPLAIN render **and** all per-node instrumentation,
/// recording only timing/calls aggregates (no plan, no plan analysis) at minimal
/// hot-path cost. Whole-query duration is still measured for the gate.
pub(crate) static GUC_CAPTURE_PLAN: GucSetting<bool> = GucSetting::<bool>::new(true);
/// In `hook` mode, sampling strategy. `random` decides each execution
/// independently; `query_id` is stratified — the first execution of each
/// `queryId` is always captured and the rest sampled at `sample_rate`, so a
/// rarely-run query shape is not starved by a very frequent one. Falls back to
/// `random` when the queryId is unavailable (PG13, or compute_query_id off).
pub(crate) static GUC_SAMPLE_BY: GucSetting<Option<CString>> =
    GucSetting::<Option<CString>>::new(Some(c"random"));

/// Sampling strategy parsed from `plansight.sample_by`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SampleBy {
    Random,
    QueryId,
}

/// Current sampling strategy, parsed from the GUC (anything but `query_id` is
/// treated as `random`).
pub(crate) fn sample_by() -> SampleBy {
    let is_qid = GUC_SAMPLE_BY
        .get()
        .and_then(|c| {
            c.to_str()
                .ok()
                .map(|s| s.trim().eq_ignore_ascii_case("query_id"))
        })
        .unwrap_or(false);
    if is_qid {
        SampleBy::QueryId
    } else {
        SampleBy::Random
    }
}

/// Current capture mode, parsed from the GUC.
pub(crate) fn capture_mode() -> CaptureMode {
    GUC_CAPTURE_MODE
        .get()
        .and_then(|c| c.to_str().ok().map(CaptureMode::parse))
        .unwrap_or(CaptureMode::Off)
}

#[pg_guard]
pub extern "C-unwind" fn _PG_init() {
    GucRegistry::define_string_guc(
        c"plansight.capture_mode",
        c"Automatic capture source: off, log (tail auto_explain log), or hook (in-process).",
        c"log and hook are mutually exclusive. Manual plansight_ingest always works. \
          Superuser-settable per session (e.g. SET plansight.capture_mode='hook').",
        &GUC_CAPTURE_MODE,
        GucContext::Suset,
        GucFlags::default(),
    );
    GucRegistry::define_string_guc(
        c"plansight.log_path",
        c"Absolute path to the auto_explain log file to tail (log mode).",
        c"Empty disables log-mode capture. Requires auto_explain text logging.",
        &GUC_LOG_PATH,
        GucContext::Sighup,
        GucFlags::default(),
    );
    GucRegistry::define_string_guc(
        c"plansight.database",
        c"Database the background worker connects to (must have the extension).",
        c"The worker writes cumulative stats into this database. Sighup rather than \
          Postmaster so CREATE EXTENSION without shared_preload_libraries does not \
          FATAL the backend; the worker reads it once at startup, so changing it \
          requires restarting the worker (or the server).",
        &GUC_DATABASE,
        GucContext::Sighup,
        GucFlags::default(),
    );
    GucRegistry::define_int_guc(
        c"plansight.flush_interval",
        c"Seconds between background flushes.",
        c"",
        &GUC_FLUSH_INTERVAL,
        1,
        3600,
        GucContext::Sighup,
        GucFlags::default(),
    );
    GucRegistry::define_float_guc(
        c"plansight.min_duration_ms",
        c"In hook mode, skip capturing executions faster than this (milliseconds).",
        c"Superuser-settable per session.",
        &GUC_MIN_DURATION_MS,
        0.0,
        f64::MAX,
        GucContext::Suset,
        GucFlags::default(),
    );
    GucRegistry::define_float_guc(
        c"plansight.slo_threshold_ms",
        c"Executions slower than this (ms) are counted as SLO breaches; 0 disables.",
        c"Applied at aggregation time against the current threshold (not retroactive). \
          Superuser-settable per session.",
        &GUC_SLO_THRESHOLD_MS,
        0.0,
        f64::MAX,
        GucContext::Suset,
        GucFlags::default(),
    );
    GucRegistry::define_float_guc(
        c"plansight.sample_rate",
        c"In hook mode, fraction of executions to capture (0.0-1.0).",
        c"Decided in ExecutorStart so unsampled queries skip timing entirely. \
          Superuser-settable per session.",
        &GUC_SAMPLE_RATE,
        0.0,
        1.0,
        GucContext::Suset,
        GucFlags::default(),
    );
    GucRegistry::define_bool_guc(
        c"plansight.synchronous",
        c"In hook mode, UPSERT synchronously in the backend (tests/debug only).",
        c"Default off uses the shared-memory ring drained by the worker.",
        &GUC_SYNCHRONOUS,
        GucContext::Suset,
        GucFlags::default(),
    );
    GucRegistry::define_bool_guc(
        c"plansight.track_io",
        c"In hook mode, capture per-node buffer and WAL usage in the plan.",
        c"On by default (feeds the buffer/WAL analyzer); set off to drop the \
          executor accounting overhead. Superuser-settable per session.",
        &GUC_TRACK_IO,
        GucContext::Suset,
        GucFlags::default(),
    );
    GucRegistry::define_bool_guc(
        c"plansight.track_nested",
        c"In hook mode, also capture queries nested in functions/triggers.",
        c"Off by default (top-level only, like pg_stat_statements). \
          Superuser-settable per session.",
        &GUC_TRACK_NESTED,
        GucContext::Suset,
        GucFlags::default(),
    );
    GucRegistry::define_bool_guc(
        c"plansight.profile",
        c"Accumulate per-phase hot-path timings for plansight_capture_timings().",
        c"For benchmarking; a few ns/capture when on. Superuser-settable per session.",
        &GUC_PROFILE,
        GucContext::Suset,
        GucFlags::default(),
    );
    GucRegistry::define_bool_guc(
        c"plansight.track_settings",
        c"In hook mode, include non-default planner GUCs (EXPLAIN SETTINGS) in the plan.",
        c"On by default; set off to skip the per-render GUC scan. Superuser-settable.",
        &GUC_TRACK_SETTINGS,
        GucContext::Suset,
        GucFlags::default(),
    );
    GucRegistry::define_bool_guc(
        c"plansight.track_timing",
        c"In hook mode, capture per-node timing (EXPLAIN ANALYZE timing).",
        c"On by default. Off drops the per-node gettimeofday accounting (the main \
          ANALYZE overhead) and per-node times, keeping row counts; whole-query \
          duration is still measured. Superuser-settable per session.",
        &GUC_TRACK_TIMING,
        GucContext::Suset,
        GucFlags::default(),
    );
    GucRegistry::define_bool_guc(
        c"plansight.track_costs",
        c"In hook mode, include estimated cost columns (EXPLAIN COSTS) in the plan.",
        c"On by default; render-only. Superuser-settable per session.",
        &GUC_TRACK_COSTS,
        GucContext::Suset,
        GucFlags::default(),
    );
    GucRegistry::define_bool_guc(
        c"plansight.track_verbose",
        c"In hook mode, render with EXPLAIN VERBOSE (output columns, qualified names).",
        c"Off by default; render-only. Superuser-settable per session.",
        &GUC_TRACK_VERBOSE,
        GucContext::Suset,
        GucFlags::default(),
    );
    GucRegistry::define_bool_guc(
        c"plansight.capture_plan",
        c"In hook mode, render and store the plan. Off = stats-only (numbers, no plan).",
        c"On by default. Off skips the EXPLAIN render and per-node instrumentation \
          entirely, recording only timing/calls aggregates at minimal overhead. \
          Superuser-settable per session.",
        &GUC_CAPTURE_PLAN,
        GucContext::Suset,
        GucFlags::default(),
    );
    GucRegistry::define_string_guc(
        c"plansight.sample_by",
        c"In hook mode, sampling strategy: random or query_id.",
        c"random samples each execution independently; query_id is stratified \
          (first execution of each queryId always captured, the rest sampled at \
          sample_rate) so rare query shapes are not starved. Superuser-settable.",
        &GUC_SAMPLE_BY,
        GucContext::Suset,
        GucFlags::default(),
    );

    // The background worker, the executor hooks, and the shared-memory ring can
    // only be set up from a library loaded via shared_preload_libraries
    // (postmaster startup). When the extension is merely CREATE EXTENSION'd, skip
    // them; manual ingest and all SQL functions still work.
    if unsafe { pg_sys::process_shared_preload_libraries_in_progress } {
        ring::init_shmem();
        bgworker::register();
        hook::install();
        // Ask core to compute queryId so hook mode can record it (PG16+ has the
        // explicit enabler; on PG14/15 set compute_query_id=on; PG13 has none).
        #[cfg(any(feature = "pg16", feature = "pg17", feature = "pg18"))]
        unsafe {
            pg_sys::EnableQueryId();
        }
    }
}

/// UPSERT that folds one batch's per-group aggregate into the running totals.
/// Timing counters are additive (or a min/max); the representative plan and its
/// analysis are replaced whenever a batch's slowest execution is at least as
/// slow as the stored representative.
const UPSERT_SQL: &str = r#"
INSERT INTO plansight.statements
    (fingerprint, normalized_query, representative_sql, representative_plan, calls,
     total_time_ms, sum_sq_time_ms, min_time_ms, max_time_ms, first_seen, last_seen,
     complexity, metadata, plan_analysis, query_id, slo_breaches)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, to_timestamp($10), to_timestamp($11),
        $12, $13, $14, $15, $16)
ON CONFLICT (fingerprint) DO UPDATE SET
    calls          = plansight.statements.calls + EXCLUDED.calls,
    total_time_ms  = plansight.statements.total_time_ms + EXCLUDED.total_time_ms,
    sum_sq_time_ms = plansight.statements.sum_sq_time_ms + EXCLUDED.sum_sq_time_ms,
    slo_breaches   = plansight.statements.slo_breaches + EXCLUDED.slo_breaches,
    min_time_ms    = LEAST(plansight.statements.min_time_ms, EXCLUDED.min_time_ms),
    max_time_ms    = GREATEST(plansight.statements.max_time_ms, EXCLUDED.max_time_ms),
    first_seen     = LEAST(plansight.statements.first_seen, EXCLUDED.first_seen),
    last_seen      = GREATEST(plansight.statements.last_seen, EXCLUDED.last_seen),
    -- Refresh the representative + its analysis only when this batch's slowest
    -- execution is at least as slow as the stored one.
    representative_sql  = CASE WHEN EXCLUDED.max_time_ms >= plansight.statements.max_time_ms
                              THEN EXCLUDED.representative_sql  ELSE plansight.statements.representative_sql  END,
    representative_plan = CASE WHEN EXCLUDED.max_time_ms >= plansight.statements.max_time_ms
                              THEN EXCLUDED.representative_plan ELSE plansight.statements.representative_plan END,
    complexity          = CASE WHEN EXCLUDED.max_time_ms >= plansight.statements.max_time_ms
                              THEN EXCLUDED.complexity          ELSE plansight.statements.complexity          END,
    metadata            = CASE WHEN EXCLUDED.max_time_ms >= plansight.statements.max_time_ms
                              THEN EXCLUDED.metadata            ELSE plansight.statements.metadata            END,
    plan_analysis       = CASE WHEN EXCLUDED.max_time_ms >= plansight.statements.max_time_ms
                              THEN EXCLUDED.plan_analysis       ELSE plansight.statements.plan_analysis       END,
    -- Keep the representative's queryId; never overwrite a known id with NULL.
    query_id            = CASE WHEN EXCLUDED.max_time_ms >= plansight.statements.max_time_ms
                              THEN COALESCE(EXCLUDED.query_id, plansight.statements.query_id)
                              ELSE COALESCE(plansight.statements.query_id, EXCLUDED.query_id) END
"#;

/// UPSERT for one (fingerprint, hour-bucket) histogram row. Additive.
const HISTOGRAM_UPSERT_SQL: &str = r#"
INSERT INTO plansight.query_histogram
    (fingerprint, bucket, calls, total_time_ms, min_time_ms, max_time_ms)
VALUES ($1, to_timestamp($2), $3, $4, $5, $6)
ON CONFLICT (fingerprint, bucket) DO UPDATE SET
    calls         = plansight.query_histogram.calls + EXCLUDED.calls,
    total_time_ms = plansight.query_histogram.total_time_ms + EXCLUDED.total_time_ms,
    min_time_ms   = LEAST(plansight.query_histogram.min_time_ms, EXCLUDED.min_time_ms),
    max_time_ms   = GREATEST(plansight.query_histogram.max_time_ms, EXCLUDED.max_time_ms)
"#;

/// Parse a chunk of auto_explain log output and fold its query statistics into
/// the cumulative `plansight.statements` table. Returns the number of distinct
/// query groups written.
///
/// This is the manual ingest path: useful for importing existing logs and for
/// testing. Automatic in-process capture arrives in a later phase.
#[pg_extern]
fn plansight_ingest(log_text: &str) -> i64 {
    let rows = aggregate::aggregate_log(log_text, GUC_SLO_THRESHOLD_MS.get());
    if rows.is_empty() {
        return 0;
    }
    Spi::connect_mut(|client| persist_rows(client, &rows))
        .unwrap_or_else(|e| error!("plansight_ingest: failed to persist statistics: {e}"))
}

/// Fold a batch of aggregated rows into the cumulative tables on an open SPI
/// connection. Shared by the manual ingest function and the background worker.
/// Returns the number of distinct query groups written.
pub(crate) fn persist_rows(
    client: &mut pgrx::spi::SpiClient<'_>,
    rows: &[StatRow],
) -> Result<i64, spi::Error> {
    let mut written = 0i64;
    for row in rows {
        // Statement row first so the histogram's FK is satisfied within the
        // same transaction.
        client.update(
            UPSERT_SQL,
            None,
            &[
                row.fingerprint.as_str().into(),
                row.normalized_query.as_str().into(),
                row.representative_sql.as_str().into(),
                row.representative_plan.as_str().into(),
                row.calls.into(),
                row.total_time_ms.into(),
                row.sum_sq_time_ms.into(),
                row.min_time_ms.into(),
                row.max_time_ms.into(),
                row.first_seen_epoch.into(),
                row.last_seen_epoch.into(),
                row.complexity.clone().map(pgrx::JsonB).into(),
                row.metadata.clone().map(pgrx::JsonB).into(),
                row.plan_analysis.clone().map(pgrx::JsonB).into(),
                row.query_id.into(),
                row.slo_breaches.into(),
            ],
        )?;

        for b in &row.histogram {
            client.update(
                HISTOGRAM_UPSERT_SQL,
                None,
                &[
                    row.fingerprint.as_str().into(),
                    b.bucket_epoch.into(),
                    b.calls.into(),
                    b.total_time_ms.into(),
                    b.min_time_ms.into(),
                    b.max_time_ms.into(),
                ],
            )?;
        }
        written += 1;
    }
    Ok(written)
}

/// Pretty-print a SQL statement using the same formatter the analyzer/TUI uses.
/// We store only the raw `representative_sql`; callers format on demand, e.g.
/// `SELECT plansight_format(representative_sql) FROM plansight.statements`.
#[pg_extern(immutable, parallel_safe)]
fn plansight_format(sql: &str) -> String {
    pg_plansight_core::format_sql_query(sql)
}

/// Discard all accumulated statistics, like `pg_stat_statements_reset()`.
/// CASCADE also clears the dependent `query_histogram`; the worker's tailing
/// offset is intentionally left intact so reset does not re-ingest the log.
#[pg_extern]
fn plansight_reset() {
    Spi::run("TRUNCATE plansight.statements CASCADE")
        .unwrap_or_else(|e| error!("plansight_reset: {e}"));
}

/// Observability for `hook` mode: current config, shared-ring counters, and the
/// extension's own per-query overhead (the latency it adds at ExecutorEnd —
/// render + ring push / sync persist, *excluding* per-node execution
/// instrumentation). `overhead_calls` is how many captures ran; the µs columns
/// show how cheap (or not) capture is. `dropped_total` rising means the ring
/// overflows between worker drains. The overhead counters reset with
/// `plansight_reset_stats()`.
#[pg_extern]
#[allow(clippy::type_complexity)] // pgrx needs the literal TableIterator type here
fn plansight_capture_stats() -> TableIterator<
    'static,
    (
        name!(capture_mode, String),
        name!(sample_rate, f64),
        name!(min_duration_ms, f64),
        name!(synchronous, bool),
        name!(ring_capacity, i64),
        name!(ring_pending, i64),
        name!(captured_total, i64),
        name!(dropped_total, i64),
        name!(last_drain_epoch, f64),
        name!(overhead_calls, i64),
        name!(overhead_mean_us, f64),
        name!(overhead_min_us, f64),
        name!(overhead_max_us, f64),
        name!(overhead_stddev_us, f64),
    ),
> {
    let mode = match capture_mode() {
        CaptureMode::Off => "off",
        CaptureMode::Log => "log",
        CaptureMode::Hook => "hook",
    };
    let (pending, captured, dropped, last_drain_epoch) = ring::stats();
    let (oc, osum, osumsq, omin, omax) = ring::overhead_stats();
    let (mean_us, min_us, max_us, stddev_us) = if oc == 0 {
        (0.0, 0.0, 0.0, 0.0)
    } else {
        let n = oc as f64;
        let mean_ns = osum as f64 / n;
        let var_ns2 = (osumsq as f64 / n - mean_ns * mean_ns).max(0.0);
        (
            mean_ns / 1000.0,
            omin as f64 / 1000.0,
            omax as f64 / 1000.0,
            var_ns2.sqrt() / 1000.0,
        )
    };
    TableIterator::once((
        mode.to_string(),
        GUC_SAMPLE_RATE.get(),
        GUC_MIN_DURATION_MS.get(),
        GUC_SYNCHRONOUS.get(),
        ring::capacity() as i64,
        pending as i64,
        captured as i64,
        dropped as i64,
        last_drain_epoch,
        oc as i64,
        mean_us,
        min_us,
        max_us,
        stddev_us,
    ))
}

/// Reset the self-overhead accumulator surfaced by `plansight_capture_stats()`
/// (the `overhead_*` columns). Independent of `plansight_reset()`, which clears
/// the cumulative `plansight.statements` data; the lifetime ring counters
/// (`captured_total`/`dropped_total`) are left intact.
#[pg_extern]
fn plansight_reset_stats() {
    ring::reset_overhead();
}

/// Configuration doctor: report inconsistencies between the `plansight.*`
/// settings, `shared_preload_libraries`, the active database, and (for log mode)
/// the co-loaded `auto_explain` GUCs. Each row is `(severity, category, message)`
/// where severity is `error` (capture won't work), `warning` (works, but likely
/// not as intended), `info` (a benign interaction), or `ok` (nothing found).
/// Run `SELECT * FROM plansight_check();` after configuring the extension.
#[pg_extern]
#[allow(clippy::type_complexity)]
fn plansight_check() -> TableIterator<
    'static,
    (
        name!(severity, String),
        name!(category, String),
        name!(message, String),
    ),
> {
    let mut out: Vec<(String, String, String)> = Vec::new();
    macro_rules! add {
        ($sev:expr, $cat:expr, $msg:expr) => {
            out.push(($sev.to_string(), $cat.to_string(), $msg.to_string()))
        };
    }
    // Read a (possibly foreign / possibly unset) GUC; None if it doesn't exist.
    let get = |name: &str| -> Option<String> {
        Spi::get_one::<String>(&format!("SELECT current_setting('{name}', true)"))
            .ok()
            .flatten()
            .filter(|s| !s.is_empty())
    };
    let our_guc = |g: &GucSetting<Option<CString>>| -> String {
        g.get()
            .and_then(|c| c.to_str().ok().map(|s| s.to_string()))
            .unwrap_or_default()
    };

    let preloaded = get("shared_preload_libraries")
        .unwrap_or_default()
        .split(',')
        .any(|s| s.trim() == "pg_plansight");
    let mode = capture_mode();

    if !preloaded {
        match mode {
            CaptureMode::Off => add!(
                "info",
                "preload",
                "pg_plansight is not in shared_preload_libraries; only manual \
                 plansight_ingest() works (no background worker or executor hooks)."
            ),
            _ => add!(
                "error",
                "preload",
                "capture_mode is set but pg_plansight is not in \
                 shared_preload_libraries — the background worker and executor hooks \
                 are NOT running. Add it to shared_preload_libraries and restart."
            ),
        }
    }

    let want_db = our_guc(&GUC_DATABASE);
    // current_database() is of SQL type `name`; cast to text so get_one::<String>
    // doesn't silently fail (which would falsely look like a DB mismatch).
    let cur_db = Spi::get_one::<String>("SELECT current_database()::text")
        .ok()
        .flatten()
        .unwrap_or_default();
    if !want_db.is_empty() && want_db != cur_db {
        add!(
            "warning",
            "database",
            format!(
                "the background worker writes to database '{want_db}' \
                 (plansight.database), but this extension is in '{cur_db}'. Captures \
                 here won't be persisted — install the extension in '{want_db}', or \
                 set plansight.database = '{cur_db}'."
            )
        );
    }

    match mode {
        CaptureMode::Log => {
            if our_guc(&GUC_LOG_PATH).trim().is_empty() {
                add!(
                    "error",
                    "log",
                    "capture_mode=log but plansight.log_path is empty — nothing is tailed."
                );
            }
            if get("auto_explain.log_min_duration").is_none() {
                add!(
                    "error",
                    "log",
                    "capture_mode=log needs auto_explain loaded (shared_preload_libraries) \
                     to write plans to the log."
                );
            } else {
                if get("auto_explain.log_min_duration").as_deref() == Some("-1") {
                    add!(
                        "warning",
                        "log",
                        "auto_explain.log_min_duration=-1 — no plans are logged; set it to 0 \
                         (or a duration threshold)."
                    );
                }
                if get("auto_explain.log_analyze").as_deref() == Some("off") {
                    add!(
                        "warning",
                        "log",
                        "auto_explain.log_analyze=off — logged plans lack actual times, which \
                         plansight needs."
                    );
                }
                if let Some(fmt) = get("auto_explain.log_format") {
                    if fmt != "text" {
                        add!(
                            "warning",
                            "log",
                            format!(
                                "auto_explain.log_format={fmt} — plansight parses text plans; \
                                 set auto_explain.log_format=text."
                            )
                        );
                    }
                }
            }
        }
        CaptureMode::Hook => {
            if GUC_SAMPLE_RATE.get() <= 0.0 {
                add!(
                    "warning",
                    "hook",
                    "sample_rate=0 — hook mode captures nothing."
                );
            }
            if GUC_SYNCHRONOUS.get() {
                add!(
                    "warning",
                    "hook",
                    "synchronous=on UPSERTs inline on the query hot path — for tests/debug \
                     only; leave off in production (the worker drains the ring)."
                );
            }
            if sample_by() == SampleBy::QueryId {
                let qid_ok = if cfg!(any(feature = "pg16", feature = "pg17", feature = "pg18")) {
                    true
                } else if cfg!(feature = "pg13") {
                    false
                } else {
                    get("compute_query_id").map(|v| v != "off").unwrap_or(false)
                };
                if !qid_ok {
                    add!(
                        "info",
                        "hook",
                        "sample_by=query_id but no core queryId is available (PG13, or \
                         compute_query_id=off) — sampling falls back to random."
                    );
                }
            }
            if !GUC_CAPTURE_PLAN.get()
                && (GUC_TRACK_IO.get() || GUC_TRACK_SETTINGS.get() || GUC_TRACK_VERBOSE.get())
            {
                add!(
                    "info",
                    "hook",
                    "capture_plan=off (stats-only) — track_io/track_settings/track_verbose \
                     have no effect (no plan is rendered)."
                );
            }
        }
        CaptureMode::Off => add!(
            "info",
            "capture",
            "capture_mode=off — no automatic capture; only manual plansight_ingest()."
        ),
    }

    let (_pending, _captured, dropped, _last) = ring::stats();
    if dropped > 0 {
        add!(
            "warning",
            "ring",
            format!(
                "the capture ring has overflowed {dropped} times (lifetime) — lower \
                 sample_rate, raise min_duration_ms, or shorten flush_interval so the \
                 worker drains more often."
            )
        );
    }

    if out.is_empty() {
        add!("ok", "config", "no configuration inconsistencies detected.");
    }
    TableIterator::new(out)
}

/// Create (or replace) `plansight.statements_with_pgss`, a view joining the
/// cumulative stats to `pg_stat_statements` on the shared `queryid`. Call this
/// after installing pg_stat_statements (the join can't be shipped in the schema
/// because pgss may not be present at `CREATE EXTENSION` time). Returns false (and
/// warns) if pg_stat_statements isn't installed.
#[pg_extern]
fn plansight_pgss_view() -> bool {
    let has_pgss = Spi::get_one::<bool>("SELECT to_regclass('pg_stat_statements') IS NOT NULL")
        .ok()
        .flatten()
        .unwrap_or(false);
    if !has_pgss {
        warning!("pg_stat_statements is not installed; plansight.statements_with_pgss not created");
        return false;
    }
    Spi::run(
        "CREATE OR REPLACE VIEW plansight.statements_with_pgss AS \
         SELECT s.fingerprint, s.query_id, s.normalized_query, s.representative_sql, \
                s.calls AS plansight_calls, \
                s.total_time_ms / NULLIF(s.calls, 0) AS plansight_mean_ms, \
                p.calls AS pgss_calls, p.total_exec_time AS pgss_total_exec_ms, \
                p.mean_exec_time AS pgss_mean_ms, p.rows AS pgss_rows, \
                p.shared_blks_hit AS pgss_shared_hit, p.shared_blks_read AS pgss_shared_read \
         FROM plansight.statements s \
         JOIN pg_stat_statements p ON p.queryid = s.query_id",
    )
    .unwrap_or_else(|e| error!("plansight_pgss_view: {e}"));
    true
}

/// Test-only: synchronously drain the capture ring and persist it in the
/// caller's transaction — i.e. do what the background worker does on its timer,
/// so async-path tests don't have to wait for the worker.
#[cfg(any(test, feature = "pg_test"))]
#[pg_extern]
fn plansight_drain_now() -> i64 {
    let (captures, _dropped) = ring::drain();
    if captures.is_empty() {
        return 0;
    }
    let rows = aggregate::aggregate_captures(captures, GUC_SLO_THRESHOLD_MS.get());
    if rows.is_empty() {
        return 0;
    }
    Spi::connect_mut(|client| persist_rows(client, &rows)).unwrap_or(0)
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
        let written = crate::plansight_ingest(SAMPLE_LOG);
        assert_eq!(written, 1, "two literals normalize to one group");

        let calls = Spi::get_one::<i64>("SELECT calls FROM plansight.statements")
            .expect("query failed")
            .expect("a row");
        assert_eq!(calls, 2);

        let total = Spi::get_one::<f64>("SELECT total_time_ms FROM plansight.statements")
            .expect("query failed")
            .expect("a row");
        assert!(
            (total - 31.0).abs() < 1e-6,
            "10.5 + 20.5 = 31.0, got {total}"
        );

        let max = Spi::get_one::<f64>("SELECT max_time_ms FROM plansight.statements_summary")
            .expect("query failed")
            .expect("a row");
        assert!((max - 20.5).abs() < 1e-6);
    }

    #[pg_test]
    fn ingest_is_cumulative_across_calls() {
        crate::plansight_ingest(SAMPLE_LOG);
        crate::plansight_ingest(SAMPLE_LOG);

        let calls = Spi::get_one::<i64>("SELECT calls FROM plansight.statements")
            .expect("query failed")
            .expect("a row");
        assert_eq!(calls, 4, "two ingests of two executions each");
    }

    #[pg_test]
    fn reset_clears_statistics() {
        crate::plansight_ingest(SAMPLE_LOG);
        crate::plansight_reset();
        let count = Spi::get_one::<i64>("SELECT count(*) FROM plansight.statements")
            .expect("query failed")
            .expect("a row");
        assert_eq!(count, 0);
    }

    #[pg_test]
    fn empty_input_writes_nothing() {
        assert_eq!(crate::plansight_ingest(""), 0);
    }

    #[pg_test]
    fn check_returns_rows_without_panicking() {
        // The doctor must always yield at least one row and never error.
        let n = Spi::get_one::<i64>("SELECT count(*) FROM plansight_check()")
            .expect("query failed")
            .expect("a row");
        assert!(n >= 1, "plansight_check() should report at least one row");
        // Severities are from the documented set.
        let bad = Spi::get_one::<i64>(
            "SELECT count(*) FROM plansight_check() \
             WHERE severity NOT IN ('error','warning','info','ok')",
        )
        .expect("query failed")
        .expect("a row");
        assert_eq!(bad, 0, "unexpected severity value");
    }

    #[pg_test]
    fn capture_stats_overhead_columns_and_reset() {
        // The overhead columns exist and reset_stats is callable.
        let calls = Spi::get_one::<i64>("SELECT overhead_calls FROM plansight_capture_stats()")
            .expect("query failed")
            .expect("a row");
        assert!(calls >= 0);
        crate::plansight_reset_stats();
        let after = Spi::get_one::<i64>("SELECT overhead_calls FROM plansight_capture_stats()")
            .expect("query failed")
            .expect("a row");
        // After a reset the counter is at most the handful of captures the read
        // itself may incur (0 when hooks aren't active in the test instance).
        assert!(after <= calls + 1);
    }

    #[pg_test]
    fn rich_analysis_is_persisted() {
        crate::plansight_ingest(SAMPLE_LOG);
        // The representative plan text and the analyzer outputs are stored.
        let has_plan = Spi::get_one::<bool>(
            "SELECT representative_plan LIKE '%Seq Scan%' FROM plansight.statements",
        )
        .expect("query failed")
        .expect("a row");
        assert!(has_plan, "representative plan text should be stored");

        let analysis_present = Spi::get_one::<bool>(
            "SELECT plan_analysis IS NOT NULL AND complexity IS NOT NULL \
             FROM plansight.statements",
        )
        .expect("query failed")
        .expect("a row");
        assert!(
            analysis_present,
            "complexity + plan_analysis should be populated"
        );
    }

    #[pg_test]
    fn histogram_is_populated_and_additive() {
        crate::plansight_ingest(SAMPLE_LOG);
        let buckets = Spi::get_one::<i64>("SELECT count(*) FROM plansight.query_histogram")
            .expect("query failed")
            .expect("a row");
        assert!(buckets >= 1, "at least one hour bucket expected");

        // sum(bigint) is numeric in PostgreSQL, so cast back to bigint for i64.
        let total_calls = Spi::get_one::<i64>(
            "SELECT coalesce(sum(calls),0)::bigint FROM plansight.query_histogram",
        )
        .expect("query failed")
        .expect("a row");
        assert_eq!(total_calls, 2, "histogram calls match the 2 executions");

        // Re-ingest: histogram folds additively, not a new row per ingest.
        crate::plansight_ingest(SAMPLE_LOG);
        let total_calls2 = Spi::get_one::<i64>(
            "SELECT coalesce(sum(calls),0)::bigint FROM plansight.query_histogram",
        )
        .expect("query failed")
        .expect("a row");
        assert_eq!(total_calls2, 4);
    }

    #[pg_test]
    fn format_function_pretty_prints() {
        let formatted = crate::plansight_format("select a,b from t where x=1");
        // sqlparser uppercases keywords when pretty-printing.
        assert!(
            formatted.contains("SELECT") && formatted.contains("FROM"),
            "expected pretty-printed SQL, got: {formatted}"
        );
    }

    #[pg_test]
    fn hook_mode_captures_in_process() {
        // capture_mode is superuser-settable, so we can enable hook capture for
        // just this session. synchronous=on makes the capture land immediately
        // (no waiting for the worker to drain the ring).
        Spi::run("SET plansight.capture_mode = 'hook'").unwrap();
        Spi::run("SET plansight.min_duration_ms = 0").unwrap();
        Spi::run("SET plansight.synchronous = on").unwrap();
        // The pgrx harness invokes each test as `SELECT "tests"."<fn>"()`, so the
        // probe below runs one level down (via SPI). Opt into nested capture so
        // the hook sees it; product default remains top-level-only.
        Spi::run("SET plansight.track_nested = on").unwrap();

        // A distinctive query that goes through the executor.
        let _ = Spi::get_one::<i64>(
            "SELECT count(*) FROM pg_class WHERE relname = 'hook_probe_marker'",
        )
        .unwrap();

        let captured = Spi::get_one::<i64>(
            "SELECT count(*) FROM plansight.statements \
             WHERE representative_sql LIKE '%hook_probe_marker%'",
        )
        .expect("query failed")
        .unwrap_or(0);

        Spi::run("SET plansight.capture_mode = 'off'").unwrap();
        Spi::run("SET plansight.track_nested = off").unwrap();
        assert!(captured >= 1, "hook mode should capture the executed query");
    }

    #[pg_test]
    fn hook_sample_rate_zero_captures_nothing() {
        Spi::run("SET plansight.capture_mode = 'hook'").unwrap();
        Spi::run("SET plansight.synchronous = on").unwrap();
        Spi::run("SET plansight.min_duration_ms = 0").unwrap();
        Spi::run("SET plansight.sample_rate = 0.0").unwrap();

        let _ =
            Spi::get_one::<i64>("SELECT count(*) FROM pg_class WHERE relname = 'unsampled_marker'")
                .unwrap();

        let captured = Spi::get_one::<i64>(
            "SELECT count(*) FROM plansight.statements \
             WHERE representative_sql LIKE '%unsampled_marker%'",
        )
        .expect("query failed")
        .unwrap_or(0);

        Spi::run("SET plansight.capture_mode = 'off'").unwrap();
        Spi::run("SET plansight.sample_rate = 1.0").unwrap();
        assert_eq!(captured, 0, "sample_rate=0 must capture nothing");
    }

    #[pg_test]
    fn capture_stats_reports_config() {
        // The shared ring is initialized at preload, so the stats function works.
        let mode = Spi::get_one::<String>("SELECT capture_mode FROM plansight_capture_stats()")
            .expect("query failed");
        assert!(mode.is_some(), "capture_stats should return a row");
        let cap = Spi::get_one::<i64>("SELECT ring_capacity FROM plansight_capture_stats()")
            .expect("query failed")
            .unwrap_or(0);
        assert!(cap > 0, "ring_capacity should be positive");
    }

    #[pg_test]
    fn track_io_produces_memory_spill_finding() {
        Spi::run("SET plansight.capture_mode = 'hook'").unwrap();
        Spi::run("SET plansight.synchronous = on").unwrap();
        Spi::run("SET plansight.min_duration_ms = 0").unwrap();
        Spi::run("SET plansight.track_io = on").unwrap();
        // Probe runs nested under the harness's `SELECT "tests"."<fn>"()`.
        Spi::run("SET plansight.track_nested = on").unwrap();
        Spi::run("SET work_mem = '64kB'").unwrap();
        Spi::run("TRUNCATE plansight.statements CASCADE").unwrap();

        // A sort over 200k rows with tiny work_mem spills to temp files.
        let _ = Spi::get_one::<i64>(
            "SELECT count(*) FROM (SELECT g FROM generate_series(1, 200000) g ORDER BY g) z",
        )
        .unwrap();

        let spills = Spi::get_one::<i64>(
            "SELECT count(*) FROM plansight.statements, \
             LATERAL jsonb_array_elements(plan_analysis->'reports') r, \
             LATERAL jsonb_array_elements(r->'findings') f \
             WHERE f->>'finding_type' = 'MemorySpill'",
        )
        .expect("query failed")
        .unwrap_or(0);

        Spi::run("SET plansight.capture_mode = 'off'").unwrap();
        Spi::run("SET plansight.track_nested = off").unwrap();
        assert!(
            spills >= 1,
            "track_io should yield a MemorySpill finding for a spilling sort"
        );
    }

    #[pg_test]
    fn async_ring_capture_drains_into_statements() {
        // Default async path: hook pushes to the shared ring; drive the drain
        // the worker would normally do on its timer.
        Spi::run("SET plansight.capture_mode = 'hook'").unwrap();
        Spi::run("SET plansight.synchronous = off").unwrap();
        Spi::run("SET plansight.min_duration_ms = 0").unwrap();
        // Probe runs nested under the harness's `SELECT "tests"."<fn>"()`.
        Spi::run("SET plansight.track_nested = on").unwrap();
        Spi::run("TRUNCATE plansight.statements CASCADE").unwrap();

        let _ =
            Spi::get_one::<i64>("SELECT count(*) FROM pg_class WHERE relname = 'ring_e2e_marker'")
                .unwrap();

        let persisted = super::plansight_drain_now();
        let captured = Spi::get_one::<i64>(
            "SELECT count(*) FROM plansight.statements \
             WHERE representative_sql LIKE '%ring_e2e_marker%'",
        )
        .expect("query failed")
        .unwrap_or(0);

        Spi::run("SET plansight.capture_mode = 'off'").unwrap();
        Spi::run("SET plansight.track_nested = off").unwrap();
        assert!(persisted >= 1, "drain should persist at least one group");
        assert!(
            captured >= 1,
            "the async-captured query should be in statements"
        );
    }

    #[pg_test]
    fn explain_only_not_captured() {
        Spi::run("SET plansight.capture_mode = 'hook'").unwrap();
        Spi::run("SET plansight.synchronous = on").unwrap();
        Spi::run("SET plansight.min_duration_ms = 0").unwrap();
        Spi::run("TRUNCATE plansight.statements CASCADE").unwrap();

        Spi::run("EXPLAIN SELECT count(*) FROM pg_class WHERE relname = 'exonly_marker'").unwrap();

        let n = Spi::get_one::<i64>(
            "SELECT count(*) FROM plansight.statements \
             WHERE representative_sql LIKE '%exonly_marker%'",
        )
        .expect("query failed")
        .unwrap_or(0);

        Spi::run("SET plansight.capture_mode = 'off'").unwrap();
        assert_eq!(n, 0, "a bare EXPLAIN (no ANALYZE) must not be captured");
    }

    #[pg_test]
    fn nested_query_not_captured_by_default() {
        Spi::run(
            "CREATE OR REPLACE FUNCTION nest_fn() RETURNS void LANGUAGE plpgsql AS $$ \
             BEGIN PERFORM count(*) FROM pg_class WHERE relname = 'nested_inner_marker'; END $$",
        )
        .unwrap();
        Spi::run("SET plansight.capture_mode = 'hook'").unwrap();
        Spi::run("SET plansight.synchronous = on").unwrap();
        Spi::run("SET plansight.min_duration_ms = 0").unwrap();
        Spi::run("TRUNCATE plansight.statements CASCADE").unwrap();

        let _ = Spi::run("SELECT nest_fn()");

        let inner = Spi::get_one::<i64>(
            "SELECT count(*) FROM plansight.statements \
             WHERE representative_sql LIKE '%nested_inner_marker%'",
        )
        .expect("query failed")
        .unwrap_or(0);

        Spi::run("SET plansight.capture_mode = 'off'").unwrap();
        assert_eq!(
            inner, 0,
            "a nested function query must not be captured by default"
        );
    }

    #[pg_test]
    fn queryid_captured() {
        Spi::run("SET plansight.capture_mode = 'hook'").unwrap();
        Spi::run("SET plansight.synchronous = on").unwrap();
        Spi::run("SET plansight.min_duration_ms = 0").unwrap();
        // Probe runs nested under the harness's `SELECT "tests"."<fn>"()`.
        Spi::run("SET plansight.track_nested = on").unwrap();
        Spi::run("TRUNCATE plansight.statements CASCADE").unwrap();

        let _ = Spi::get_one::<i64>("SELECT count(*) FROM pg_class WHERE relname = 'qid_marker'")
            .unwrap();

        let has_qid = Spi::get_one::<bool>(
            "SELECT bool_or(query_id IS NOT NULL) FROM plansight.statements \
             WHERE representative_sql LIKE '%qid_marker%'",
        )
        .ok()
        .flatten()
        .unwrap_or(false);

        Spi::run("SET plansight.capture_mode = 'off'").unwrap();
        Spi::run("SET plansight.track_nested = off").unwrap();
        assert!(
            has_qid,
            "queryId should be captured on PG16 (EnableQueryId)"
        );
    }

    #[pg_test]
    fn min_duration_gates_fast_queries() {
        Spi::run("SET plansight.capture_mode = 'hook'").unwrap();
        Spi::run("SET plansight.synchronous = on").unwrap();
        // 10s threshold — a trivial query is far below it.
        Spi::run("SET plansight.min_duration_ms = 10000").unwrap();
        Spi::run("TRUNCATE plansight.statements CASCADE").unwrap();

        let _ = Spi::get_one::<i64>("SELECT count(*) FROM pg_class WHERE relname = 'fast_marker'")
            .unwrap();

        let n = Spi::get_one::<i64>(
            "SELECT count(*) FROM plansight.statements \
             WHERE representative_sql LIKE '%fast_marker%'",
        )
        .expect("query failed")
        .unwrap_or(0);

        Spi::run("SET plansight.capture_mode = 'off'").unwrap();
        Spi::run("SET plansight.min_duration_ms = 0").unwrap();
        assert_eq!(
            n, 0,
            "a fast query below min_duration_ms must not be captured"
        );
    }

    #[pg_test]
    fn summary_exposes_cv_and_stddev() {
        // calls=2, total=31.0, sum_sq=530.5; mean=15.5,
        // var = 530.5/2 - 15.5^2 = 265.25 - 240.25 = 25.0, stddev=5.0.
        crate::plansight_ingest(SAMPLE_LOG);
        let stddev = Spi::get_one::<f64>("SELECT stddev_time_ms FROM plansight.statements_summary")
            .expect("query failed")
            .expect("a row");
        assert!((stddev - 5.0).abs() < 1e-6, "stddev = 5.0, got {stddev}");
        let cv = Spi::get_one::<f64>("SELECT cv FROM plansight.statements_summary")
            .expect("query failed")
            .expect("a row");
        assert!((cv - 5.0 / 15.5).abs() < 1e-6, "cv = stddev/mean, got {cv}");
    }

    #[pg_test]
    fn slo_breaches_counted_and_surfaced() {
        Spi::run("SET plansight.slo_threshold_ms = 15").unwrap();
        // durations 10.5, 20.5 → 1 breach (>15).
        crate::plansight_ingest(SAMPLE_LOG);
        let breaches = Spi::get_one::<i64>("SELECT slo_breaches FROM plansight.statements")
            .expect("query failed")
            .expect("a row");
        assert_eq!(breaches, 1);
        let pct = Spi::get_one::<f64>("SELECT slo_breach_pct FROM plansight.statements_summary")
            .expect("query failed")
            .expect("a row");
        assert!((pct - 0.5).abs() < 1e-6, "1 of 2 calls breached");
        Spi::run("SET plansight.slo_threshold_ms = 0").unwrap();
    }

    #[pg_test]
    fn slo_breaches_additive_across_ingests() {
        Spi::run("SET plansight.slo_threshold_ms = 15").unwrap();
        crate::plansight_ingest(SAMPLE_LOG);
        crate::plansight_ingest(SAMPLE_LOG);
        let breaches = Spi::get_one::<i64>("SELECT slo_breaches FROM plansight.statements")
            .expect("query failed")
            .expect("a row");
        assert_eq!(breaches, 2, "1 breach per ingest, summed");
        Spi::run("SET plansight.slo_threshold_ms = 0").unwrap();
    }

    #[pg_test]
    fn slo_threshold_zero_counts_no_breaches() {
        Spi::run("SET plansight.slo_threshold_ms = 0").unwrap();
        crate::plansight_ingest(SAMPLE_LOG);
        let breaches = Spi::get_one::<i64>("SELECT slo_breaches FROM plansight.statements")
            .expect("query failed")
            .expect("a row");
        assert_eq!(breaches, 0, "disabled SLO never counts");
    }
}

/// Required by `cargo pgrx test`.
#[cfg(test)]
pub mod pg_test {
    pub fn setup(_options: Vec<&str>) {}

    #[must_use]
    pub fn postgresql_conf_options() -> Vec<&'static str> {
        // Load the library at startup so the test server exercises _PG_init and
        // the background-worker registration path. The worker idles because
        // plansight.log_path is unset, so it does not interfere with tests.
        vec!["shared_preload_libraries = 'pg_plansight'"]
    }
}
