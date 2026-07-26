//! Phase 2b: in-process query capture via executor hooks.
//!
//! `ExecutorStart` enables timing instrumentation; `ExecutorEnd` renders the
//! plan. In the default async path the rendered bytes are copied straight into a
//! bounded shared-memory ring (`ring.rs`) and the background worker does the
//! heavy parse/analyze/UPSERT off the query hot path. `synchronous=on` UPSERTs
//! inline for deterministic tests.
//!
//! Capture is best-effort and isolated:
//! - **Sampling & ownership:** `sample_rate` is decided in `ExecutorStart`,
//!   *before* timing instrumentation is requested, so unsampled queries pay
//!   nothing. The decision (and whether *we* allocated the query's `totaltime`)
//!   is recorded per QueryDesc (keyed, so interleaved cursor portals pair
//!   correctly) and consumed at `ExecutorEnd`, so we only ever finalize
//!   instrumentation we own — never another extension's (e.g. auto_explain
//!   co-loaded).
//! - **Top-level only:** queries nested in functions/triggers are skipped by
//!   default (like pg_stat_statements); `track_nested` opts in. Nesting is
//!   tracked around ExecutorRun/Finish (pg_stat_statements-style) rather than
//!   Start/End, because cursor portals run ExecutorStart at DECLARE and
//!   ExecutorEnd at CLOSE — a Start/End counter would stay raised for every
//!   statement executed while a cursor is open. Those two hooks live in
//!   `nesting.c`, **not** here: they sit on the executor error path, and a Rust
//!   frame there strips `constraint_name`/`table_name`/`cursorpos` from every
//!   error the cluster raises. See `src/nesting.c`.
//! - **Abort-safe:** capture is skipped while the transaction is aborting.
//! - **Error isolation:** the render + persist run inside an internal
//!   subtransaction wrapped in `PgTryBuilder`: a capture failure can never
//!   turn a successful user query into an error, and the rollback releases
//!   any locks/pins the failed render held.
//! - **Re-entrancy guard:** the synchronous UPSERT is itself a query that
//!   re-enters these hooks; a thread-local flag suppresses capturing it.
//! - **Leader only:** parallel workers are skipped to avoid double counting.
//! - **Reusable render context:** the plan is rendered into a long-lived
//!   per-backend memory context that is reset (not freed) after each capture, so
//!   the StringInfo buffer is reused instead of palloc/repalloc-grown every time
//!   — cutting allocator churn under sustained throughput (measured ~36% faster
//!   render for large plans).

use crate::aggregate::{aggregate_captures, Capture};
use crate::{
    capture_mode, persist_rows, ring, sample_by, CaptureMode, SampleBy, GUC_CAPTURE_PLAN,
    GUC_MIN_DURATION_MS, GUC_PROFILE, GUC_SAMPLE_RATE, GUC_SLO_THRESHOLD_MS, GUC_SYNCHRONOUS,
    GUC_TRACK_COSTS, GUC_TRACK_IO, GUC_TRACK_NESTED, GUC_TRACK_SETTINGS, GUC_TRACK_TIMING,
    GUC_TRACK_VERBOSE,
};
use pgrx::pg_sys::pg_try::PgTryBuilder;
use pgrx::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::ffi::CStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Soft cap on the per-backend seen-queryId set used for `sample_by=query_id`.
/// Real workloads have a bounded number of distinct query shapes; if a backend
/// ever exceeds this the set is cleared wholesale (cheap, rare) — at worst a few
/// shapes get a second guaranteed capture.
const SEEN_QUERY_IDS_CAP: usize = 8192;

// --- Per-phase hot-path profiling (enabled by plansight.profile) ------------
static PROF_COUNT: AtomicU64 = AtomicU64::new(0);
static PROF_START_NS: AtomicU64 = AtomicU64::new(0);
static PROF_GATE_NS: AtomicU64 = AtomicU64::new(0);
static PROF_RENDER_NS: AtomicU64 = AtomicU64::new(0);
static PROF_CONSUME_NS: AtomicU64 = AtomicU64::new(0);

/// Start a phase timer iff profiling is on.
#[inline]
fn prof_start() -> Option<Instant> {
    GUC_PROFILE.get().then(Instant::now)
}
#[inline]
fn prof_add(acc: &AtomicU64, t: Option<Instant>) {
    if let Some(t) = t {
        acc.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
}

/// Per-phase hot-path timings (avg ns/capture) since the last call, then resets.
/// Enable `plansight.profile`, run a workload, then read this.
#[pg_extern]
fn plansight_capture_timings() -> TableIterator<
    'static,
    (
        name!(captures, i64),
        name!(start_instr_ns, i64),
        name!(gate_ns, i64),
        name!(render_ns, i64),
        name!(consume_ns, i64),
    ),
> {
    let n = PROF_COUNT.swap(0, Ordering::Relaxed).max(1);
    let s = PROF_START_NS.swap(0, Ordering::Relaxed);
    let g = PROF_GATE_NS.swap(0, Ordering::Relaxed);
    let r = PROF_RENDER_NS.swap(0, Ordering::Relaxed);
    let c = PROF_CONSUME_NS.swap(0, Ordering::Relaxed);
    TableIterator::once((
        n as i64,
        (s / n) as i64,
        (g / n) as i64,
        (r / n) as i64,
        (c / n) as i64,
    ))
}

static mut PREV_EXECUTOR_START: pg_sys::ExecutorStart_hook_type = None;
static mut PREV_EXECUTOR_END: pg_sys::ExecutorEnd_hook_type = None;

thread_local! {
    /// Set while persisting a capture so the UPSERT we run (which re-enters
    /// these hooks) is not captured recursively.
    static CAPTURING: Cell<bool> = const { Cell::new(false) };
    /// Per-backend xorshift state for `sample_rate` (lazily seeded).
    static RNG: Cell<u64> = const { Cell::new(0) };
    /// Per-backend reusable memory context the plan render allocates into.
    static RENDER_CTX: Cell<pg_sys::MemoryContext> = const { Cell::new(std::ptr::null_mut()) };
    /// Per-in-flight-query state, keyed by the QueryDesc address. A keyed map
    /// rather than a LIFO stack: cursor portals pair Start/End in arbitrary
    /// order (`DECLARE c1; DECLARE c2; CLOSE c1; CLOSE c2` is FIFO), and a
    /// plain stack mispairs the entries — losing captures and attributing
    /// instrumentation ownership to the wrong query.
    static SAMPLE_MAP: RefCell<Vec<(usize, SampleEntry)>> = const { RefCell::new(Vec::new()) };
    /// Length of the last rendered plan, to pre-size the next render's StringInfo
    /// in one shot (avoids repalloc/memcpy doubling mid-render for large plans).
    static LAST_PLAN_LEN: Cell<usize> = const { Cell::new(0) };
    /// queryIds this backend has already captured at least once, for the
    /// stratified `sample_by=query_id` strategy (first-seen ⇒ always capture).
    static SEEN_QUERY_IDS: RefCell<HashSet<i64>> = RefCell::new(HashSet::new());
}

/// Soft cap on in-flight sample entries. Entries are removed at ExecutorEnd
/// and cleared at transaction end; this bounds pathological accumulation from
/// portals that error before ExecutorEnd within one long transaction.
const SAMPLE_MAP_CAP: usize = 1024;

/// Reset nesting/sample bookkeeping at transaction end, so a query that errored
/// (ExecutorEnd never ran) can't leak a level or a map entry into the next
/// statement.
#[pg_guard]
unsafe extern "C-unwind" fn xact_callback(
    _event: pg_sys::XactEvent::Type,
    _arg: *mut core::ffi::c_void,
) {
    unsafe { plansight_nesting_level_reset() };
    SAMPLE_MAP.with(|s| s.borrow_mut().clear());
    // Also clear the re-entrancy flag. `ReentryGuard` normally resets it, but a
    // `longjmp` originating in a *chained* previous hook (a co-loaded C
    // extension calling PG_RE_THROW) unwinds straight past the Rust frame
    // without running destructors. A stuck `true` here silently disables
    // capture for the rest of the backend's life, with no diagnostic.
    CAPTURING.with(|c| c.set(false));
}

/// Per-in-flight-query state captured at ExecutorStart and consumed at End.
/// An entry exists only for queries whose `totaltime` instrumentation we
/// allocated (i.e. we own them).
#[derive(Clone, Copy)]
struct SampleEntry {
    /// `track_io` as read at ExecutorStart — reused at render so a mid-query GUC
    /// flip can't set `es.buffers` on an execution we didn't instrument.
    track_io: bool,
    /// `track_timing` as read at ExecutorStart — reused at render so per-node
    /// timing is only shown for an execution we actually instrumented with a
    /// timer (a mid-query GUC flip can't ask for times we never collected).
    track_timing: bool,
    /// `capture_plan` as read at ExecutorStart — when false we requested no
    /// per-node instrumentation, so we must skip the render too (stats-only).
    capture_plan: bool,
}

/// Reset the re-entrancy flag on drop, even if the persist path unwinds.
struct ReentryGuard;
impl Drop for ReentryGuard {
    fn drop(&mut self) {
        CAPTURING.with(|c| c.set(false));
    }
}

/// Push an active snapshot on creation and pop it on drop, so the active-snapshot
/// stack stays balanced even if the wrapped work unwinds.
struct ActiveSnapshotGuard;
impl ActiveSnapshotGuard {
    unsafe fn push() -> Self {
        pg_sys::PushActiveSnapshot(pg_sys::GetTransactionSnapshot());
        ActiveSnapshotGuard
    }
}
impl Drop for ActiveSnapshotGuard {
    fn drop(&mut self) {
        unsafe { pg_sys::PopActiveSnapshot() };
    }
}

/// `InstrAlloc` gained an `async_mode` parameter in PG14; PG13 takes two args.
#[cfg(feature = "pg13")]
unsafe fn instr_alloc(n: i32, opts: i32) -> *mut pg_sys::Instrumentation {
    pg_sys::InstrAlloc(n, opts)
}
#[cfg(not(feature = "pg13"))]
unsafe fn instr_alloc(n: i32, opts: i32) -> *mut pg_sys::Instrumentation {
    pg_sys::InstrAlloc(n, opts, false)
}

/// Lazily-created, long-lived (per-backend) memory context the plan render
/// allocates into. Rendering into a context we own and `MemoryContextReset`
/// afterwards lets the StringInfo buffer be reused across captures instead of
/// being freshly palloc'd (and repalloc-grown) in the per-query context each
/// time — cutting allocator churn under sustained capture throughput.
unsafe fn render_context() -> pg_sys::MemoryContext {
    let existing = RENDER_CTX.with(Cell::get);
    if !existing.is_null() {
        return existing;
    }
    let ctx = pg_sys::AllocSetContextCreateInternal(
        pg_sys::TopMemoryContext,
        c"pg_plansight render".as_ptr(),
        pg_sys::ALLOCSET_DEFAULT_MINSIZE as usize,
        pg_sys::ALLOCSET_DEFAULT_INITSIZE as usize,
        pg_sys::ALLOCSET_DEFAULT_MAXSIZE as usize,
    );
    RENDER_CTX.with(|c| c.set(ctx));
    ctx
}

/// Decide whether to capture this execution, honoring `sample_by`.
///
/// For `query_id` (stratified) sampling the *first* execution of each `queryId`
/// this backend sees is always captured — so a rarely-run query shape is never
/// starved by a very frequent one — and subsequent executions are sampled at
/// `rate`. With `random`, or when the queryId is unavailable (0), every
/// execution is an independent Bernoulli draw at `rate`.
fn want_capture(rate: f64, query_id: i64) -> bool {
    if rate >= 1.0 {
        return true;
    }
    if rate <= 0.0 {
        return false;
    }
    if sample_by() == SampleBy::QueryId && query_id != 0 {
        let first_seen = SEEN_QUERY_IDS.with(|s| {
            let mut set = s.borrow_mut();
            if set.len() >= SEEN_QUERY_IDS_CAP {
                set.clear();
            }
            set.insert(query_id)
        });
        if first_seen {
            return true;
        }
    }
    sampled(rate)
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
            // Seed from the backend pid and the clock. The cell's address is
            // NOT distinct per backend — PostgreSQL backends fork from the
            // postmaster, so every backend sees the same address and would
            // draw the identical decision sequence, systematically sampling
            // the same executions in every session. `| 1` keeps the state
            // non-zero so xorshift can't latch.
            let pid = unsafe { pg_sys::MyProcPid } as u64;
            let now_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            x = (0x9E37_79B9_7F4A_7C15 ^ (pid << 32) ^ now_ns) | 1;
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
        // ExecutorRun/ExecutorFinish are chained by the C shim; see the
        // `plansight_install_nesting_hooks` declaration for why they must not
        // be Rust frames.
        plansight_install_nesting_hooks();
        PREV_EXECUTOR_END = pg_sys::ExecutorEnd_hook;
        pg_sys::ExecutorEnd_hook = Some(executor_end);
        pg_sys::RegisterXactCallback(Some(xact_callback), std::ptr::null_mut());
    }
}

// The executor-nesting shim, implemented in C (`src/nesting.c`).
//
// ExecutorRun/ExecutorFinish are deliberately NOT hooked from Rust. Both sit on
// the executor error path, and pgrx cannot carry a PostgreSQL error across a
// Rust frame intact: `#[pg_guard]` re-raises from a `CopyErrorData` snapshot,
// which drops constraint_name, table_name, schema_name, column_name,
// datatype_name and cursorpos. Merely preloading this library used to strip
// those from *every* error in the cluster, breaking every driver that
// dispatches on constraint name. The C shim uses PG_TRY/PG_FINALLY, whose
// PG_RE_THROW resumes the original longjmp with the original ErrorData
// untouched.
unsafe extern "C" {
    // Chain the C ExecutorRun/ExecutorFinish hooks. Call once, from _PG_init.
    fn plansight_install_nesting_hooks();
    // Current executor nesting depth (0 == top level).
    fn plansight_nesting_level_get() -> core::ffi::c_int;
    // Reset the depth to 0 at transaction end.
    fn plansight_nesting_level_reset();
}

/// Current executor nesting depth, as maintained by the C shim.
#[inline]
fn nesting_level() -> i32 {
    unsafe { plansight_nesting_level_get() }
}

#[pg_guard]
unsafe extern "C-unwind" fn executor_start(query_desc: *mut pg_sys::QueryDesc, eflags: i32) {
    // Decide capture (incl. sampling) up front, so unsampled queries skip the
    // timing instrumentation entirely — that overhead is paid during execution
    // and cannot be recovered later. Top-level only by default (NESTING_LEVEL is
    // 0 outside any other executor); bare EXPLAIN (no ANALYZE) never runs, so
    // skip it. The re-entrancy guard suppresses our own UPSERT's queries.
    let top_level = nesting_level() == 0;
    let eligible = capture_mode() == CaptureMode::Hook
        && !query_desc.is_null()
        && !CAPTURING.with(Cell::get)
        && (eflags & pg_sys::EXEC_FLAG_EXPLAIN_ONLY as i32) == 0
        && (top_level || GUC_TRACK_NESTED.get());
    // queryId is computed during planning, so it's available here — needed for
    // stratified `sample_by=query_id` sampling. 0 when unavailable (PG13/off).
    // `as i64` reads it uniformly across versions (uint64 ≤ PG17, int64 on
    // PG18+); the cast is a no-op on the int64 versions, hence the allow.
    #[allow(clippy::unnecessary_cast)]
    let query_id = if eligible && !(*query_desc).plannedstmt.is_null() {
        (*(*query_desc).plannedstmt).queryId as i64
    } else {
        0
    };
    let track_io = GUC_TRACK_IO.get();
    let track_timing = GUC_TRACK_TIMING.get();
    let capture_plan = GUC_CAPTURE_PLAN.get();
    let want = eligible && want_capture(GUC_SAMPLE_RATE.get(), query_id);
    if want && capture_plan {
        // Per-node instrumentation: row counts always; timer/buffers opt-in. With
        // track_timing off we skip the per-node gettimeofday loop (the dominant
        // ANALYZE overhead) but still get actual rows. In stats-only mode
        // (capture_plan off) we request no per-node instrumentation at all — only
        // the whole-query totaltime below, for duration.
        let mut opts = pg_sys::InstrumentOption::INSTRUMENT_ROWS as i32;
        if track_timing {
            opts |= pg_sys::InstrumentOption::INSTRUMENT_TIMER as i32;
        }
        if track_io {
            opts |= pg_sys::InstrumentOption::INSTRUMENT_BUFFERS as i32;
            opts |= pg_sys::InstrumentOption::INSTRUMENT_WAL as i32;
        }
        (*query_desc).instrument_options |= opts;
    }

    match PREV_EXECUTOR_START {
        Some(prev) => prev(query_desc, eflags),
        None => pg_sys::standard_ExecutorStart(query_desc, eflags),
    }

    // standard_ExecutorStart instruments the plan nodes but does not allocate a
    // whole-query Instrumentation; do it ourselves (as auto_explain does) so
    // `totaltime` is finalized at ExecutorEnd. We own (and will capture) the
    // query only if WE allocated it — never finalize instrumentation another
    // extension (e.g. auto_explain) already allocated.
    let mut we_own = false;
    if want && !query_desc.is_null() {
        let qd = &mut *query_desc;
        if qd.totaltime.is_null() && !qd.estate.is_null() {
            let t = prof_start();
            let cxt = (*qd.estate).es_query_cxt;
            let old = pg_sys::MemoryContextSwitchTo(cxt);
            // The whole-query timer is independent of per-node instrumentation,
            // so always allocate it with a timer — the `min_duration_ms` gate
            // needs `totaltime` even when track_timing drops per-node timing.
            qd.totaltime = instr_alloc(1, pg_sys::InstrumentOption::INSTRUMENT_TIMER as i32);
            pg_sys::MemoryContextSwitchTo(old);
            prof_add(&PROF_START_NS, t);
            we_own = true;
        }
    }
    // Keying by QueryDesc address pairs Start/End correctly for interleaved
    // cursor portals (see SAMPLE_MAP). A fresh QueryDesc at a previously-seen
    // address means the old entry is stale (its query errored before
    // ExecutorEnd and the allocation was reused) — drop it unconditionally so
    // it cannot shadow the new query, then record ours only when we own the
    // instrumentation.
    SAMPLE_MAP.with(|s| {
        let mut map = s.borrow_mut();
        if let Some(stale) = map.iter().position(|(key, _)| *key == query_desc as usize) {
            map.swap_remove(stale);
        }
        if we_own {
            if map.len() >= SAMPLE_MAP_CAP {
                // Bound leak accumulation without discarding all live state:
                // evict the oldest single entry (most likely the stale one).
                map.remove(0);
            }
            map.push((
                query_desc as usize,
                SampleEntry {
                    track_io,
                    track_timing,
                    capture_plan,
                },
            ));
        }
    });
}

#[pg_guard]
unsafe extern "C-unwind" fn executor_end(query_desc: *mut pg_sys::QueryDesc) {
    let entry = SAMPLE_MAP.with(|s| {
        let mut map = s.borrow_mut();
        map.iter()
            .position(|(key, _)| *key == query_desc as usize)
            .map(|idx| map.swap_remove(idx).1)
    });
    if let Some(entry) = entry {
        maybe_capture(
            query_desc,
            entry.track_io,
            entry.track_timing,
            entry.capture_plan,
        );
    }
    match PREV_EXECUTOR_END {
        Some(prev) => prev(query_desc),
        None => pg_sys::standard_ExecutorEnd(query_desc),
    }
}

/// Capture the just-finished query. Only called when we own its instrumentation
/// (sampled + we allocated `totaltime`).
unsafe fn maybe_capture(
    query_desc: *mut pg_sys::QueryDesc,
    track_io: bool,
    track_timing: bool,
    capture_plan: bool,
) {
    if query_desc.is_null() || CAPTURING.with(Cell::get) {
        return;
    }
    // Only the parallel leader records; workers would double count.
    if pg_sys::ParallelWorkerNumber >= 0 {
        return;
    }
    // Never run capture work (snapshot push, SPI) while the transaction is
    // aborting — ExecutorEnd can fire during abort/portal cleanup.
    if pg_sys::IsAbortedTransactionBlockState() {
        return;
    }
    // The error-isolation subtransaction below cannot be started while in
    // parallel mode.
    if pg_sys::IsInParallelMode() {
        return;
    }
    let qd = &*query_desc;
    if qd.totaltime.is_null() || qd.planstate.is_null() || qd.sourceText.is_null() {
        return;
    }
    let t_gate = prof_start();
    pg_sys::InstrEndLoop(qd.totaltime);
    let duration_ms = (*qd.totaltime).total * 1000.0;
    prof_add(&PROF_GATE_NS, t_gate);
    if duration_ms < GUC_MIN_DURATION_MS.get() {
        return;
    }
    // `sourceText` is a live NUL-terminated C string; borrow its bytes with no
    // allocation.
    let sql = CStr::from_ptr(qd.sourceText).to_bytes();
    // Core queryId (0 when compute_query_id is off or on PG13). `as i64` reads
    // it uniformly across versions (uint64 ≤ PG17, int64 on PG18+); the cast is
    // a no-op on the int64 versions, hence the allow.
    #[allow(clippy::unnecessary_cast)]
    let query_id = if qd.plannedstmt.is_null() {
        0
    } else {
        (*qd.plannedstmt).queryId as i64
    };

    if GUC_PROFILE.get() {
        PROF_COUNT.fetch_add(1, Ordering::Relaxed);
    }
    // Suppress capturing any query our own render/persist might trigger
    // (covers both paths); the guard resets on every exit.
    CAPTURING.with(|c| c.set(true));
    let _guard = ReentryGuard;
    // Each capture path measures the latency we add at ExecutorEnd (render +
    // ring push / sync persist) and folds it into the shared self-overhead
    // accumulator surfaced by plansight_capture_stats(). The async path records
    // it inside `ring::push`'s locked section (one ring-lock acquisition per
    // capture instead of two); the synchronous path never touches the ring, so
    // it records the overhead itself.
    if GUC_SYNCHRONOUS.get() {
        capture_synchronous(
            query_desc,
            duration_ms,
            query_id,
            sql,
            track_io,
            track_timing,
            capture_plan,
        );
    } else {
        capture_async(
            query_desc,
            duration_ms,
            query_id,
            sql,
            track_io,
            track_timing,
            capture_plan,
        );
    }
}

/// Async (default): render and copy the bytes straight into the shared ring —
/// no heap `String`, no SPI, no recursion. The render allocates into our
/// reusable `render_context`, reset afterwards. Wrapped in `PgTryBuilder` so an
/// `ereport` inside the render can never escape into the user's finished query.
unsafe fn capture_async(
    query_desc: *mut pg_sys::QueryDesc,
    duration_ms: f64,
    query_id: i64,
    sql: &[u8],
    track_io: bool,
    track_timing: bool,
    capture_plan: bool,
) {
    // Start the self-overhead timer here (before render); `ring::push` reads it
    // under the ring lock so the recorded overhead covers render + push in a
    // single locked section.
    let t_overhead = Instant::now();
    let epoch_secs = chrono::Utc::now().timestamp_micros() as f64 / 1_000_000.0;
    // Stamp the source database so the (process-global) ring can be drained
    // per-database (M5), keeping this backend's SQL/plan text out of another
    // database's stats tables. `MyDatabaseId` is this backend's database.
    let db_oid = pg_sys::MyDatabaseId;
    // Stats-only fast path: nothing is rendered and the ring push is a plain
    // shared-memory copy, so skip the error-isolation subtransaction and its
    // per-capture overhead entirely.
    if !capture_plan {
        let t_consume = prof_start();
        ring::push(
            epoch_secs,
            duration_ms,
            query_id,
            db_oid,
            sql,
            &[],
            t_overhead,
        );
        prof_add(&PROF_CONSUME_NS, t_consume);
        return;
    }
    with_rendered_plan(
        query_desc,
        track_io,
        track_timing,
        capture_plan,
        move |plan| {
            ring::push(
                epoch_secs,
                duration_ms,
                query_id,
                db_oid,
                sql,
                plan,
                t_overhead,
            );
        },
    );
}

/// Synchronous (tests/debug): render, build an owned `Capture`, and UPSERT
/// inline. ExecutorEnd runs as the query's portal is torn down, so there may be
/// no active snapshot for our UPSERT — push one (as a bgworker txn does) via an
/// RAII guard that pops on every exit. The owned `Capture` strings are on the
/// Rust heap, so resetting the render context afterwards is safe.
unsafe fn capture_synchronous(
    query_desc: *mut pg_sys::QueryDesc,
    duration_ms: f64,
    query_id: i64,
    sql: &[u8],
    track_io: bool,
    track_timing: bool,
    capture_plan: bool,
) {
    // Synchronous mode never touches the ring, so it records its own overhead
    // (render + inline SPI UPSERT) once, at the end.
    let t_overhead = Instant::now();
    let _snapshot = ActiveSnapshotGuard::push();
    with_rendered_plan(query_desc, track_io, track_timing, capture_plan, |plan| {
        let cap = Capture {
            timestamp: chrono::Utc::now(),
            duration_ms,
            query_text: String::from_utf8_lossy(sql).into_owned(),
            plan_text: String::from_utf8_lossy(plan).into_owned(),
            query_id,
        };
        persist_capture(cap);
    });
    ring::record_overhead(t_overhead.elapsed().as_nanos() as u64);
}

/// Render the plan into the reusable scratch context and hand the bytes to `f`,
/// with sound error isolation, then restore and reset the context. Shared by
/// both capture paths.
///
/// The work runs inside an internal subtransaction (the plpgsql exception
/// pattern): catching an `elog(ERROR)` without aborting a (sub)transaction
/// leaves LWLocks, buffer pins, and catcache references dangling —
/// `LWLockReleaseAll` and resource-owner cleanup only run during abort — so a
/// bare catch could wedge the backend. On error the subtransaction rolls back
/// and the user's query still succeeds; on success it is released into the
/// parent.
unsafe fn with_rendered_plan(
    query_desc: *mut pg_sys::QueryDesc,
    track_io: bool,
    track_timing: bool,
    capture_plan: bool,
    f: impl FnOnce(&[u8]) + std::panic::UnwindSafe,
) {
    let scratch = render_context();
    let outer_context = pg_sys::CurrentMemoryContext;
    let outer_owner = pg_sys::CurrentResourceOwner;
    pg_sys::BeginInternalSubTransaction(std::ptr::null());
    pg_sys::MemoryContextSwitchTo(scratch);
    PgTryBuilder::new(move || {
        // Stats-only: no render — hand the consumer an empty plan. The core
        // aggregator builds a plan-less row from the (still present) query text.
        if !capture_plan {
            let t_consume = prof_start();
            f(&[]);
            prof_add(&PROF_CONSUME_NS, t_consume);
        } else {
            let t_render = prof_start();
            let rendered = render_plan(query_desc, track_io, track_timing);
            prof_add(&PROF_RENDER_NS, t_render);
            if let Some((ptr, len)) = rendered {
                let t_consume = prof_start();
                f(std::slice::from_raw_parts(ptr, len));
                prof_add(&PROF_CONSUME_NS, t_consume);
            }
        }
        pg_sys::ReleaseCurrentSubTransaction();
    })
    .catch_others(|_| {
        // Best-effort: capture never breaks the query. Rolling the
        // subtransaction back releases every resource the failed render
        // acquired.
        pg_sys::RollbackAndReleaseCurrentSubTransaction();
    })
    .execute();
    pg_sys::MemoryContextSwitchTo(outer_context);
    pg_sys::CurrentResourceOwner = outer_owner;
    pg_sys::MemoryContextReset(scratch);
}

fn persist_capture(cap: Capture) {
    let rows = aggregate_captures(vec![cap], GUC_SLO_THRESHOLD_MS.get());
    if rows.is_empty() {
        return;
    }
    if let Err(e) = Spi::connect_mut(|client| persist_rows(client, &rows)) {
        log!("pg_plansight: synchronous capture persist failed: {e}");
    }
}

/// Render the executed plan as `EXPLAIN (ANALYZE) FORMAT TEXT` — the same form
/// the core text parser already consumes. Returns a borrowed view `(ptr, len)`
/// into the `ExplainState`'s palloc'd StringInfo buffer; the caller must copy
/// the bytes before the surrounding memory context is reset.
unsafe fn render_plan(
    query_desc: *mut pg_sys::QueryDesc,
    track_io: bool,
    track_timing: bool,
) -> Option<(*const u8, usize)> {
    let es = pg_sys::NewExplainState();
    if es.is_null() {
        return None;
    }
    (*es).analyze = true;
    // Per-node timing only if we instrumented with a timer at ExecutorStart
    // (snapshotted, so a mid-query GUC flip can't request times we never took).
    (*es).timing = track_timing;
    (*es).verbose = GUC_TRACK_VERBOSE.get();
    (*es).costs = GUC_TRACK_COSTS.get();
    // Non-default planner GUCs behind the plan. Tunable: get_explain_guc_options
    // scans every GUC per render, so operators can drop it from the hot path.
    (*es).settings = GUC_TRACK_SETTINGS.get();
    // Buffer/WAL accounting only if it was instrumented at ExecutorStart (use the
    // value snapshotted then, not the current GUC, in case it flipped).
    if track_io {
        (*es).buffers = true;
        (*es).wal = true;
    }
    (*es).format = pg_sys::ExplainFormat::EXPLAIN_FORMAT_TEXT;

    // Pre-size the StringInfo to the last plan's length (+25%, ≥4 KiB) in one
    // shot, so a large plan doesn't repalloc-double (and memcpy) mid-render.
    let target = {
        let last = LAST_PLAN_LEN.with(Cell::get);
        (last + last / 4).max(4096)
    };
    pg_sys::enlargeStringInfo((*es).str_, target as i32);

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
    LAST_PLAN_LEN.with(|c| c.set(len));
    Some((data as *const u8, len))
}
