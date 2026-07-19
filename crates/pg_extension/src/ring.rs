//! Phase 2b (T4): a fixed-size shared-memory ring of captured plans.
//!
//! In async hook mode the executor hook renders the plan and pushes a compact,
//! fixed-size record here under a brief LWLock — no parse, no analysis, no SPI
//! on the query hot path. The background worker drains the ring on its timer and
//! runs the heavy `aggregate_captures` + `persist_rows` off the hot path. This
//! is the `pg_stat_statements` hot/cold split adapted for variable plan text
//! (truncated to fixed capacity).

use crate::aggregate::Capture;
use pgrx::prelude::*;
use pgrx::{PGRXSharedMemory, PgLwLock};

/// Max stored bytes of query text per record (truncated beyond this).
const SQL_CAP: usize = 1024;
/// Max stored bytes of plan text per record (truncated beyond this).
const PLAN_CAP: usize = 4096;
/// Ring slot count. Bursts beyond this within one flush interval are dropped
/// (we are a sampler, not a ledger). ~256 * 5 KiB ≈ 1.3 MiB of shared memory.
const RING_CAP: usize = 256;

#[derive(Clone, Copy)]
#[repr(C)]
struct Rec {
    epoch_secs: f64,
    duration_ms: f64,
    query_id: i64,
    sql_len: u32,
    plan_len: u32,
    sql: [u8; SQL_CAP],
    plan: [u8; PLAN_CAP],
}

const REC_ZEROED: Rec = Rec {
    epoch_secs: 0.0,
    duration_ms: 0.0,
    query_id: 0,
    sql_len: 0,
    plan_len: 0,
    sql: [0; SQL_CAP],
    plan: [0; PLAN_CAP],
};

/// Fixed-size, pointer-free ring — safe to place in shared memory.
#[repr(C)]
pub struct Ring {
    /// Pending records not yet drained.
    len: u32,
    /// Cumulative records dropped because the ring was full (never reset).
    dropped_total: u64,
    /// Cumulative records accepted into the ring (never reset).
    captured_total: u64,
    /// Unix epoch seconds of the last drain (0.0 = never). Lets operators tell
    /// "nothing matched" from "the worker isn't draining".
    last_drain_epoch: f64,
    // --- Self-overhead accounting (resettable via plansight_reset_stats). The
    // nanoseconds spent in the ExecutorEnd capture body (gate + render + ring
    // push / sync persist) — i.e. the latency we add to a captured query. Does
    // not include per-node execution instrumentation (the track_timing cost).
    /// Number of captures whose overhead was recorded.
    ovh_count: u64,
    /// Sum of recorded overheads (ns); with `ovh_count` gives the mean.
    ovh_sum_ns: u64,
    /// Sum of squared overheads (ns², wide to avoid overflow) for stddev.
    ovh_sumsq_ns: u128,
    /// Min / max recorded overhead (ns); `ovh_min_ns` is `u64::MAX` when empty.
    ovh_min_ns: u64,
    ovh_max_ns: u64,
    recs: [Rec; RING_CAP],
}

impl Default for Ring {
    fn default() -> Self {
        Ring {
            len: 0,
            dropped_total: 0,
            captured_total: 0,
            last_drain_epoch: 0.0,
            ovh_count: 0,
            ovh_sum_ns: 0,
            ovh_sumsq_ns: 0,
            ovh_min_ns: u64::MAX,
            ovh_max_ns: 0,
            recs: [REC_ZEROED; RING_CAP],
        }
    }
}

// SAFETY: `Ring` is `repr(C)`, fixed-size, and contains no pointers or heap
// allocations, so it is sound to store in the shared-memory segment.
unsafe impl PGRXSharedMemory for Ring {}

pub static RING: PgLwLock<Ring> = unsafe { PgLwLock::new(c"pg_plansight_ring") };

/// Whether `init_shmem` ran (i.e. the library was loaded via
/// `shared_preload_libraries`). Touching `RING` without it panics inside
/// pgrx's lock ("PgLwLock was not initialized"), so SQL functions must check
/// this first and raise a proper PostgreSQL error instead.
static SHMEM_INITIALIZED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// True when the shared-memory ring is available (library preloaded).
pub fn is_available() -> bool {
    SHMEM_INITIALIZED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Register the ring in shared memory. Call from `_PG_init` during
/// `shared_preload_libraries` processing (the macro installs the shmem hooks).
pub fn init_shmem() {
    pgrx::pg_shmem_init!(RING);
    SHMEM_INITIALIZED.store(true, std::sync::atomic::Ordering::Relaxed);
}

fn copy_truncated(dst: &mut [u8], src: &[u8]) -> u32 {
    let n = src.len().min(dst.len());
    dst[..n].copy_from_slice(&src[..n]);
    n as u32
}

/// Push a capture into the ring (hot path) directly from borrowed bytes — no
/// intermediate heap `String`. The query text and rendered plan are copied
/// straight from their source buffers into the fixed shared slot, capped at
/// `SQL_CAP`/`PLAN_CAP`. Drops and counts if full. Builds the record in place,
/// so there is no large stack temporary.
pub fn push(epoch_secs: f64, duration_ms: f64, query_id: i64, sql: &[u8], plan: &[u8]) {
    RING.exclusive()
        .push_rec(epoch_secs, duration_ms, query_id, sql, plan);
}

impl Ring {
    /// Append one record, or drop (and count) if full. Pure logic, no locking —
    /// the public `push` holds the LWLock around this. Unit-testable.
    fn push_rec(
        &mut self,
        epoch_secs: f64,
        duration_ms: f64,
        query_id: i64,
        sql: &[u8],
        plan: &[u8],
    ) {
        let idx = self.len as usize;
        if idx >= RING_CAP {
            self.dropped_total += 1;
            return;
        }
        let slot = &mut self.recs[idx];
        slot.epoch_secs = epoch_secs;
        slot.duration_ms = duration_ms;
        slot.query_id = query_id;
        slot.sql_len = copy_truncated(&mut slot.sql, sql);
        slot.plan_len = copy_truncated(&mut slot.plan, plan);
        self.len += 1;
        self.captured_total += 1;
    }

    /// Take the populated records out and reset `len`; returns them plus the
    /// cumulative dropped count. Pure logic, no locking. Unit-testable.
    fn take_recs(&mut self) -> (Vec<Rec>, u64) {
        let n = self.len as usize;
        let recs = self.recs[..n].to_vec();
        self.len = 0;
        self.last_drain_epoch = chrono::Utc::now().timestamp_micros() as f64 / 1_000_000.0;
        (recs, self.dropped_total)
    }

    /// Fold one capture's overhead (ns) into the accumulator. Saturating so a
    /// pathological outlier can never wrap the sums. Pure logic, no locking.
    fn record_overhead(&mut self, ns: u64) {
        self.ovh_count += 1;
        self.ovh_sum_ns = self.ovh_sum_ns.saturating_add(ns);
        self.ovh_sumsq_ns = self
            .ovh_sumsq_ns
            .saturating_add((ns as u128) * (ns as u128));
        if ns < self.ovh_min_ns {
            self.ovh_min_ns = ns;
        }
        if ns > self.ovh_max_ns {
            self.ovh_max_ns = ns;
        }
    }

    /// Clear the overhead accumulator. Pure logic, no locking.
    fn reset_overhead(&mut self) {
        self.ovh_count = 0;
        self.ovh_sum_ns = 0;
        self.ovh_sumsq_ns = 0;
        self.ovh_min_ns = u64::MAX;
        self.ovh_max_ns = 0;
    }

    /// `(count, sum_ns, sumsq_ns, min_ns, max_ns)` — `min_ns` is 0 when empty.
    fn overhead_snapshot(&self) -> (u64, u64, u128, u64, u64) {
        let min = if self.ovh_count == 0 {
            0
        } else {
            self.ovh_min_ns
        };
        (
            self.ovh_count,
            self.ovh_sum_ns,
            self.ovh_sumsq_ns,
            min,
            self.ovh_max_ns,
        )
    }
}

/// Record one capture's ExecutorEnd overhead (ns) — hot path, brief lock.
pub fn record_overhead(ns: u64) {
    RING.exclusive().record_overhead(ns);
}

/// Reset just the self-overhead accumulator (leaves the cumulative ring counters).
pub fn reset_overhead() {
    RING.exclusive().reset_overhead();
}

/// Snapshot of the overhead accumulator: `(count, sum_ns, sumsq_ns, min_ns, max_ns)`.
pub fn overhead_stats() -> (u64, u64, u128, u64, u64) {
    RING.share().overhead_snapshot()
}

/// Capacity of the ring (slots).
pub const fn capacity() -> usize {
    RING_CAP
}

/// Snapshot of the ring counters:
/// (pending, captured_total, dropped_total, last_drain_epoch).
pub fn stats() -> (u32, u64, u64, f64) {
    let ring = RING.share();
    (
        ring.len,
        ring.captured_total,
        ring.dropped_total,
        ring.last_drain_epoch,
    )
}

/// Drain all pending records (cold path, in the worker). Returns the captures
/// and the cumulative dropped count (the worker computes the per-cycle delta).
pub fn drain() -> (Vec<Capture>, u64) {
    // Hold the exclusive lock only long enough to memcpy the populated records
    // out; build the owned `Capture`s (heap allocation + UTF-8 decode) after
    // releasing it, so a drain never blocks the hot-path `push`.
    let (recs, dropped) = RING.exclusive().take_recs();
    let out = recs
        .iter()
        .map(|r| Capture {
            timestamp: epoch_to_utc(r.epoch_secs),
            duration_ms: r.duration_ms,
            query_text: String::from_utf8_lossy(&r.sql[..r.sql_len as usize]).into_owned(),
            plan_text: String::from_utf8_lossy(&r.plan[..r.plan_len as usize]).into_owned(),
            query_id: r.query_id,
        })
        .collect();
    (out, dropped)
}

fn epoch_to_utc(secs: f64) -> chrono::DateTime<chrono::Utc> {
    let micros = (secs * 1_000_000.0) as i64;
    chrono::DateTime::from_timestamp_micros(micros).unwrap_or_else(chrono::Utc::now)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_ring() -> Box<Ring> {
        // `Ring` is ~1.3 MiB (a big fixed Rec array), so `Box::new(Ring::default())`
        // builds it as a stack temporary that can overflow a test thread's stack.
        // Allocate zeroed on the heap instead — an all-zero Ring matches Default
        // except `ovh_min_ns`, which we set explicitly.
        let mut r = unsafe { Box::<Ring>::new_zeroed().assume_init() };
        r.ovh_min_ns = u64::MAX;
        r
    }

    #[test]
    fn push_then_take_round_trips() {
        let mut r = empty_ring();
        r.push_rec(1.5, 2.0, 42, b"select 1", b"Seq Scan");
        r.push_rec(3.0, 4.0, 0, b"select 2", b"Index Scan");
        let (recs, dropped) = r.take_recs();
        assert_eq!(dropped, 0);
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].query_id, 42);
        assert_eq!(&recs[0].sql[..recs[0].sql_len as usize], b"select 1");
        assert_eq!(&recs[1].plan[..recs[1].plan_len as usize], b"Index Scan");
        // Draining resets length.
        assert_eq!(r.take_recs().0.len(), 0);
    }

    #[test]
    fn overflow_drops_and_counts() {
        let mut r = empty_ring();
        for _ in 0..(RING_CAP + 10) {
            r.push_rec(0.0, 0.0, 0, b"q", b"p");
        }
        let (recs, dropped) = r.take_recs();
        assert_eq!(recs.len(), RING_CAP, "ring holds at most RING_CAP records");
        assert_eq!(dropped, 10, "excess pushes are counted as drops");
    }

    #[test]
    fn overhead_accumulates_and_resets() {
        let mut r = empty_ring();
        assert_eq!(r.overhead_snapshot(), (0, 0, 0, 0, 0));
        r.record_overhead(100);
        r.record_overhead(300);
        let (count, sum, sumsq, min, max) = r.overhead_snapshot();
        assert_eq!((count, sum, min, max), (2, 400, 100, 300));
        assert_eq!(sumsq, 100u128 * 100 + 300 * 300); // for stddev
        r.reset_overhead();
        assert_eq!(r.overhead_snapshot(), (0, 0, 0, 0, 0), "reset clears it");
    }

    #[test]
    fn truncates_oversized_text_to_caps() {
        let mut r = empty_ring();
        let big_sql = vec![b'x'; SQL_CAP + 500];
        let big_plan = vec![b'y'; PLAN_CAP + 500];
        r.push_rec(0.0, 0.0, 0, &big_sql, &big_plan);
        let (recs, _) = r.take_recs();
        assert_eq!(recs[0].sql_len as usize, SQL_CAP);
        assert_eq!(recs[0].plan_len as usize, PLAN_CAP);
    }
}
