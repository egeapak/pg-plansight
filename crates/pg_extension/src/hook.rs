//! Phase 2b: in-process query capture via executor hooks.
//!
//! `ExecutorStart` enables timing instrumentation; `ExecutorEnd` renders the
//! plan. In the default async path the rendered bytes are copied straight into a
//! bounded shared-memory ring (`ring.rs`) and the background worker does the
//! heavy parse/analyze/UPSERT off the query hot path. `synchronous=on` UPSERTs
//! inline for deterministic tests.
//!
//! Capture is best-effort and isolated:
//! - **Sampling:** `sample_rate` is decided in `ExecutorStart`, *before* timing
//!   instrumentation is requested, so unsampled queries pay nothing — not even
//!   the per-node timing overhead. The decision is recorded implicitly: only
//!   sampled queries get a whole-query `totaltime`, which is exactly what
//!   `ExecutorEnd` keys on.
//! - **Error isolation:** the render + persist run inside `PgTryBuilder`, so a
//!   capture failure can never turn a successful user query into an error.
//! - **Re-entrancy guard:** the synchronous UPSERT is itself a query that
//!   re-enters these hooks; a thread-local flag suppresses capturing it.
//! - **Leader only:** parallel workers are skipped to avoid double counting.

use crate::aggregate::{aggregate_captures, Capture};
use crate::{
    capture_mode, persist_rows, ring, CaptureMode, GUC_MIN_DURATION_MS, GUC_SAMPLE_RATE,
    GUC_SYNCHRONOUS,
};
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
    /// Per-backend xorshift state for `sample_rate` (lazily seeded).
    static RNG: Cell<u64> = const { Cell::new(0) };
}

/// Reset the re-entrancy flag on drop, even if the persist path unwinds.
struct ReentryGuard;
impl Drop for ReentryGuard {
    fn drop(&mut self) {
        CAPTURING.with(|c| c.set(false));
    }
}

/// Cheap per-backend Bernoulli sample at probability `rate` (xorshift64).
fn sampled(rate: f64) -> bool {
    if rate >= 1.0 {
        return true;
    }
    if rate <= 0.0 {
        return false;
    }
    RNG.with(|c| {
        let mut x = c.get();
        if x == 0 {
            // Seed from the address of this cell ⊕ a constant — distinct per
            // backend, never zero.
            x = 0x9E37_79B9_7F4A_7C15 ^ (c as *const _ as u64);
        }
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        c.set(x);
        // Top 53 bits → [0, 1).
        ((x >> 11) as f64) / ((1u64 << 53) as f64) < rate
    })
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
    // Decide capture (incl. sampling) up front, so unsampled queries skip the
    // timing instrumentation entirely — that overhead is paid during execution
    // and cannot be recovered later.
    let want = capture_mode() == CaptureMode::Hook
        && !query_desc.is_null()
        && !CAPTURING.with(Cell::get)
        && sampled(GUC_SAMPLE_RATE.get());
    if want {
        (*query_desc).instrument_options |= pg_sys::InstrumentOption::INSTRUMENT_TIMER as i32;
    }

    match PREV_EXECUTOR_START {
        Some(prev) => prev(query_desc, eflags),
        None => pg_sys::standard_ExecutorStart(query_desc, eflags),
    }

    // standard_ExecutorStart instruments the plan nodes but does not allocate a
    // whole-query Instrumentation; do it ourselves (as auto_explain does) so
    // `totaltime` is finalized at ExecutorEnd. Allocating it only when `want`
    // also makes `totaltime` the implicit "this query was sampled" marker that
    // ExecutorEnd keys on.
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
    // A non-null `totaltime` means ExecutorStart sampled this query; otherwise
    // skip (unsampled, or instrumented by something other than us).
    if qd.totaltime.is_null() || qd.planstate.is_null() || qd.sourceText.is_null() {
        return;
    }
    pg_sys::InstrEndLoop(qd.totaltime);
    let duration_ms = (*qd.totaltime).total * 1000.0;
    if duration_ms < GUC_MIN_DURATION_MS.get() {
        return;
    }
    // `sourceText` is a live NUL-terminated C string; borrow its bytes with no
    // allocation.
    let sql = CStr::from_ptr(qd.sourceText).to_bytes();

    if GUC_SYNCHRONOUS.get() {
        capture_synchronous(query_desc, duration_ms, sql);
    } else {
        capture_async(query_desc, duration_ms, sql);
    }
}

/// Async (default): render and copy the bytes straight into the shared ring —
/// no heap `String`, no SPI, no recursion. Wrapped in `PgTryBuilder` so an
/// `ereport` inside the render can never escape into the user's finished query.
unsafe fn capture_async(query_desc: *mut pg_sys::QueryDesc, duration_ms: f64, sql: &[u8]) {
    let epoch_secs = chrono::Utc::now().timestamp_micros() as f64 / 1_000_000.0;
    PgTryBuilder::new(|| {
        if let Some((ptr, len)) = render_plan(query_desc) {
            let plan = std::slice::from_raw_parts(ptr, len);
            ring::push(epoch_secs, duration_ms, sql, plan);
        }
    })
    .catch_others(|_| { /* best-effort: capture never breaks the query */ })
    .execute();
}

/// Synchronous (tests/debug): render, build an owned `Capture`, and UPSERT
/// inline. ExecutorEnd runs as the query's portal is torn down, so there may be
/// no active snapshot for our UPSERT — push one (as a bgworker txn does) and pop
/// it after. The re-entrancy guard stops our UPSERT from being captured.
unsafe fn capture_synchronous(query_desc: *mut pg_sys::QueryDesc, duration_ms: f64, sql: &[u8]) {
    CAPTURING.with(|c| c.set(true));
    let _guard = ReentryGuard;
    pg_sys::PushActiveSnapshot(pg_sys::GetTransactionSnapshot());
    PgTryBuilder::new(|| {
        if let Some((ptr, len)) = render_plan(query_desc) {
            let plan = std::slice::from_raw_parts(ptr, len);
            let cap = Capture {
                timestamp: chrono::Utc::now(),
                duration_ms,
                query_text: String::from_utf8_lossy(sql).into_owned(),
                plan_text: String::from_utf8_lossy(plan).into_owned(),
            };
            persist_capture(cap);
        }
    })
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
/// the core text parser already consumes. Returns a borrowed view `(ptr, len)`
/// into the `ExplainState`'s palloc'd StringInfo buffer; the caller must copy
/// the bytes before the surrounding memory context is reset.
unsafe fn render_plan(query_desc: *mut pg_sys::QueryDesc) -> Option<(*const u8, usize)> {
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
    Some((data as *const u8, len))
}
