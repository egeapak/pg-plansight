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
    sql_len: u32,
    plan_len: u32,
    sql: [u8; SQL_CAP],
    plan: [u8; PLAN_CAP],
}

const REC_ZEROED: Rec = Rec {
    epoch_secs: 0.0,
    duration_ms: 0.0,
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
    recs: [Rec; RING_CAP],
}

impl Default for Ring {
    fn default() -> Self {
        Ring {
            len: 0,
            dropped_total: 0,
            captured_total: 0,
            recs: [REC_ZEROED; RING_CAP],
        }
    }
}

// SAFETY: `Ring` is `repr(C)`, fixed-size, and contains no pointers or heap
// allocations, so it is sound to store in the shared-memory segment.
unsafe impl PGRXSharedMemory for Ring {}

pub static RING: PgLwLock<Ring> = unsafe { PgLwLock::new(c"pg_loganalyze_ring") };

/// Register the ring in shared memory. Call from `_PG_init` during
/// `shared_preload_libraries` processing (the macro installs the shmem hooks).
pub fn init_shmem() {
    pgrx::pg_shmem_init!(RING);
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
pub fn push(epoch_secs: f64, duration_ms: f64, sql: &[u8], plan: &[u8]) {
    let mut ring = RING.exclusive();
    let idx = ring.len as usize;
    if idx >= RING_CAP {
        ring.dropped_total += 1;
        return;
    }
    let slot = &mut ring.recs[idx];
    slot.epoch_secs = epoch_secs;
    slot.duration_ms = duration_ms;
    slot.sql_len = copy_truncated(&mut slot.sql, sql);
    slot.plan_len = copy_truncated(&mut slot.plan, plan);
    ring.len += 1;
    ring.captured_total += 1;
}

/// Capacity of the ring (slots).
pub const fn capacity() -> usize {
    RING_CAP
}

/// Snapshot of the ring counters: (pending, captured_total, dropped_total).
pub fn stats() -> (u32, u64, u64) {
    let ring = RING.share();
    (ring.len, ring.captured_total, ring.dropped_total)
}

/// Drain all pending records (cold path, in the worker). Returns the captures
/// and the cumulative dropped count (the worker computes the per-cycle delta).
pub fn drain() -> (Vec<Capture>, u64) {
    let mut ring = RING.exclusive();
    let n = ring.len as usize;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let r = &ring.recs[i];
        out.push(Capture {
            timestamp: epoch_to_utc(r.epoch_secs),
            duration_ms: r.duration_ms,
            query_text: String::from_utf8_lossy(&r.sql[..r.sql_len as usize]).into_owned(),
            plan_text: String::from_utf8_lossy(&r.plan[..r.plan_len as usize]).into_owned(),
        });
    }
    ring.len = 0;
    (out, ring.dropped_total)
}

fn epoch_to_utc(secs: f64) -> chrono::DateTime<chrono::Utc> {
    let micros = (secs * 1_000_000.0) as i64;
    chrono::DateTime::from_timestamp_micros(micros).unwrap_or_else(chrono::Utc::now)
}
