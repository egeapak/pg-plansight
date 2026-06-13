//! Phase 2b: in-process query capture via executor hooks (synchronous mode).
//!
//! `ExecutorStart` enables timing instrumentation; `ExecutorEnd` renders the
//! plan and, when `capture_mode='hook'`, folds it straight into the cumulative
//! tables through the shared aggregate+persist path.
//!
//! Capture is best-effort and isolated:
//! - **Re-entrancy guard:** the UPSERT we issue is itself a query that re-enters
//!   these hooks; a thread-local flag suppresses capturing it.
//! - **Error isolation:** the persist path runs inside `PgTryBuilder` so a
//!   capture failure can never turn a successful user query into an error.
//! - **Leader only:** parallel workers are skipped to avoid double counting.
//!
//! A bounded shared-memory ring drained by the background worker (avoiding
//! hot-path SPI) is the planned optimization; this synchronous path is the
//! correctness baseline and the deterministic test mode.

use crate::aggregate::{aggregate_captures, Capture};
use crate::{capture_mode, persist_rows, ring, CaptureMode, GUC_MIN_DURATION_MS, GUC_SYNCHRONOUS};
use pgrx::pg_sys::pg_try::PgTryBuilder;
use pgrx::prelude::*;
use std::cell::Cell;
use std::ffi::CStr;

static mut PREV_EXECUTOR_START: pg_sys::ExecutorStart_hook_type = None;
static mut PREV_EXECUTOR_END: pg_sys::ExecutorEnd_hook_type = None;

thread_local! {
    /// Set while persisting a capture so the UPSERT we run (which re-enters
    /// these hooks) is not captured recursively.
    static CAPTURING: Cell<bool> = const { Cell::new(false) };
}

/// Reset the re-entrancy flag on drop, even if the persist path unwinds.
struct ReentryGuard;
impl Drop for ReentryGuard {
    fn drop(&mut self) {
        CAPTURING.with(|c| c.set(false));
    }
}

/// Chain our executor hooks. Call only from `_PG_init` during
/// `shared_preload_libraries` processing.
pub(crate) fn install() {
    unsafe {
        PREV_EXECUTOR_START = pg_sys::ExecutorStart_hook;
        pg_sys::ExecutorStart_hook = Some(executor_start);
        PREV_EXECUTOR_END = pg_sys::ExecutorEnd_hook;
        pg_sys::ExecutorEnd_hook = Some(executor_end);
    }
}

#[pg_guard]
unsafe extern "C-unwind" fn executor_start(query_desc: *mut pg_sys::QueryDesc, eflags: i32) {
    // Request timing so the plan tree is instrumented — only when capturing.
    let want =
        capture_mode() == CaptureMode::Hook && !query_desc.is_null() && !CAPTURING.with(Cell::get);
    if want {
        (*query_desc).instrument_options |= pg_sys::InstrumentOption::INSTRUMENT_TIMER as i32;
    }

    match PREV_EXECUTOR_START {
        Some(prev) => prev(query_desc, eflags),
        None => pg_sys::standard_ExecutorStart(query_desc, eflags),
    }

    // standard_ExecutorStart instruments the plan nodes but does not allocate a
    // whole-query Instrumentation; do it ourselves (as auto_explain does) so
    // `totaltime` is finalized at ExecutorEnd.
    if want && !query_desc.is_null() {
        let qd = &mut *query_desc;
        if qd.totaltime.is_null() && !qd.estate.is_null() {
            let cxt = (*qd.estate).es_query_cxt;
            let old = pg_sys::MemoryContextSwitchTo(cxt);
            qd.totaltime = pg_sys::InstrAlloc(1, qd.instrument_options, false);
            pg_sys::MemoryContextSwitchTo(old);
        }
    }
}

#[pg_guard]
unsafe extern "C-unwind" fn executor_end(query_desc: *mut pg_sys::QueryDesc) {
    maybe_capture(query_desc);
    match PREV_EXECUTOR_END {
        Some(prev) => prev(query_desc),
        None => pg_sys::standard_ExecutorEnd(query_desc),
    }
}

unsafe fn maybe_capture(query_desc: *mut pg_sys::QueryDesc) {
    if capture_mode() != CaptureMode::Hook || query_desc.is_null() || CAPTURING.with(Cell::get) {
        return;
    }
    // Only the parallel leader records; workers would double count.
    if pg_sys::ParallelWorkerNumber >= 0 {
        return;
    }
    let qd = &*query_desc;
    if qd.totaltime.is_null() || qd.planstate.is_null() || qd.sourceText.is_null() {
        return;
    }
    pg_sys::InstrEndLoop(qd.totaltime);
    let duration_ms = (*qd.totaltime).total * 1000.0;
    if duration_ms < GUC_MIN_DURATION_MS.get() {
        return;
    }
    let query_text = CStr::from_ptr(qd.sourceText).to_string_lossy().into_owned();
    let Some(plan_text) = render_plan(query_desc) else {
        return;
    };

    let cap = Capture {
        timestamp: chrono::Utc::now(),
        duration_ms,
        query_text,
        plan_text,
    };

    if GUC_SYNCHRONOUS.get() {
        persist_synchronously(cap);
    } else {
        // Async (default): push a compact record to the shared-memory ring and
        // return immediately. The worker does the parse/analyze/UPSERT off the
        // hot path. A `memcpy` under a brief lock — no SPI, no recursion.
        ring::push(&cap);
    }
}

/// Synchronous (tests/debug) path: UPSERT in the backend at ExecutorEnd.
unsafe fn persist_synchronously(cap: Capture) {
    // ExecutorEnd runs as the query's portal is torn down, so there may be no
    // active snapshot for our UPSERT to use. Push one (as a bgworker txn does)
    // and pop it afterwards. The re-entrancy guard stops our UPSERT from being
    // captured; errors are swallowed so capture never breaks the user query.
    CAPTURING.with(|c| c.set(true));
    let _guard = ReentryGuard;
    pg_sys::PushActiveSnapshot(pg_sys::GetTransactionSnapshot());
    PgTryBuilder::new(|| persist_capture(cap))
        .catch_others(|_| { /* best-effort: capture never breaks the query */ })
        .execute();
    pg_sys::PopActiveSnapshot();
}

fn persist_capture(cap: Capture) {
    let rows = aggregate_captures(vec![cap]);
    if rows.is_empty() {
        return;
    }
    let _ = Spi::connect_mut(|client| persist_rows(client, &rows));
}

/// Render the executed plan as `EXPLAIN (ANALYZE) FORMAT TEXT` — the same form
/// the core text parser already consumes.
unsafe fn render_plan(query_desc: *mut pg_sys::QueryDesc) -> Option<String> {
    let es = pg_sys::NewExplainState();
    if es.is_null() {
        return None;
    }
    (*es).analyze = true;
    (*es).timing = true;
    (*es).verbose = false;
    (*es).format = pg_sys::ExplainFormat::EXPLAIN_FORMAT_TEXT;

    pg_sys::ExplainBeginOutput(es);
    pg_sys::ExplainPrintPlan(es, query_desc);
    pg_sys::ExplainEndOutput(es);

    let s = (*es).str_;
    if s.is_null() {
        return None;
    }
    let len = (*s).len as usize;
    let data = (*s).data;
    if data.is_null() || len == 0 {
        return None;
    }
    let bytes = std::slice::from_raw_parts(data as *const u8, len);
    Some(String::from_utf8_lossy(bytes).into_owned())
}
