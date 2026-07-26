//! End-to-end memory profile of parse + group, for both paths.
//!
//! `mem_grouping` measures the grouping phase in isolation, starting from plans
//! that are already resident. That misses the thing that actually decides peak
//! memory on a large log: whether every plan has to be resident at all.
//!
//! This measures the whole pipeline through a counting global allocator:
//!   - batched:   parse to `Vec<QueryPlan>`, then `get_processed_queries`
//!   - streamed:  `parse_into_grouper`, folding each plan and dropping it
//!
//! For each it reports peak live bytes over the run and the bytes still held
//! when the grouped map is returned.
//!
//! Usage: mem_pipeline [num_plans] [num_shapes]   (defaults: 50000 500)
//! Run with RAYON_NUM_THREADS=1 for deterministic numbers.

use pg_plansight_core::{PostgreSQLLogParser, QueryGrouper};
use std::alloc::{GlobalAlloc, Layout, System};
use std::fmt::Write as FmtWrite;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Live bytes currently allocated.
static CURRENT: AtomicUsize = AtomicUsize::new(0);
/// High-water mark of live bytes (fetch-max maintained in alloc paths).
static PEAK: AtomicUsize = AtomicUsize::new(0);

struct CountingAllocator;

fn record_alloc(size: usize) {
    let live = CURRENT.fetch_add(size, Ordering::Relaxed) + size;
    PEAK.fetch_max(live, Ordering::Relaxed);
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            record_alloc(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        CURRENT.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            CURRENT.fetch_sub(layout.size(), Ordering::Relaxed);
            record_alloc(new_size);
        }
        new_ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            record_alloc(layout.size());
        }
        ptr
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn push_plan(out: &mut String, i: usize, shape: usize, off_ms: u64, table: &str) {
    let secs = off_ms / 1000;
    let ms = off_ms % 1000;
    let (h, m, s) = (secs / 3600 % 24, secs / 60 % 60, secs % 60);
    writeln!(
        out,
        "2025-06-15 {h:02}:{m:02}:{s:02}.{ms:03} UTC [{i}] LOG:  duration: {}.{} ms  plan:",
        10 + (i % 90),
        i % 1000
    )
    .unwrap();
    writeln!(
        out,
        "\tQuery Text: SELECT a.id, a.name, a.created_at FROM {table} a JOIN other_{shape} b ON b.a_id = a.id WHERE a.status = 'active' AND a.created_at > '2025-01-01' ORDER BY a.created_at DESC LIMIT {}",
        i % 50
    )
    .unwrap();
    writeln!(out, "\tLimit  (cost=0.43..599.04 rows=1000 width=56)").unwrap();
    writeln!(
        out,
        "\t  ->  Nested Loop  (cost=0.43..599.04 rows=1000 width=56)"
    )
    .unwrap();
    writeln!(
        out,
        "\t        ->  Index Scan using idx_{shape} on {table} a  (cost=0.43..95610.13 rows=159718 width=56)"
    )
    .unwrap();
    writeln!(out, "\t              Index Cond: (a.status = 'active')").unwrap();
    writeln!(
        out,
        "\t              Filter: (a.created_at > '2025-01-01 00:00:00+00'::timestamp with time zone)"
    )
    .unwrap();
    writeln!(
        out,
        "\t        ->  Seq Scan on other_{shape} b  (cost=0.00..35.50 rows=10 width=100)"
    )
    .unwrap();
    writeln!(out, "\t              Filter: (b.a_id = a.id)").unwrap();
}

fn make_grouped_log(num_plans: usize, num_shapes: usize) -> String {
    let mut out = String::new();
    for i in 0..num_plans {
        let shape = i % num_shapes;
        let table = format!("tbl_{shape}");
        push_plan(&mut out, i, shape, (i as u64) * 37, &table);
    }
    out
}

struct Measurement {
    peak: usize,
    retained: usize,
    groups: usize,
    elapsed: std::time::Duration,
}

/// Run `f` with the allocator counters rebased, reporting its peak and the bytes
/// its result still holds.
fn measure<T>(f: impl FnOnce() -> (T, usize)) -> Measurement {
    let base = CURRENT.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let started = std::time::Instant::now();
    let (result, groups) = f();
    let elapsed = started.elapsed();
    let peak = PEAK.load(Ordering::Relaxed).saturating_sub(base);
    let retained = CURRENT.load(Ordering::Relaxed).saturating_sub(base);
    drop(result);
    Measurement {
        peak,
        retained,
        groups,
        elapsed,
    }
}

fn mib(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let num_plans: usize = args
        .next()
        .map(|a| a.parse().expect("num_plans must be a number"))
        .unwrap_or(50_000);
    let num_shapes: usize = args
        .next()
        .map(|a| a.parse().expect("num_shapes must be a number"))
        .unwrap_or(500);

    let content = make_grouped_log(num_plans, num_shapes);
    let log_bytes = content.len();

    // Batched: every plan resident, then grouped.
    let batched = measure(|| {
        let mut parser = PostgreSQLLogParser::new();
        let plans = parser
            .parse_string_with_progress(&content, |_, _| {})
            .expect("parse failed");
        assert_eq!(plans.len(), num_plans);
        let grouped = parser.get_processed_queries(&plans);
        let groups = grouped.len();
        ((plans, grouped), groups)
    });

    // Streamed: each plan folded into its group and dropped.
    let streamed = measure(|| {
        let mut parser = PostgreSQLLogParser::new();
        let mut grouper = QueryGrouper::new();
        parser
            .parse_string_into_grouper(&content, |_, _| {}, &mut grouper)
            .expect("parse failed");
        assert_eq!(grouper.accepted(), num_plans);
        let grouped = grouper.finish();
        let groups = grouped.len();
        (grouped, groups)
    });

    assert_eq!(
        batched.groups, streamed.groups,
        "both paths must produce the same groups"
    );

    println!(
        "num_plans={num_plans} num_shapes={num_shapes} groups={} log_bytes={log_bytes} ({:.1} MiB)",
        streamed.groups,
        mib(log_bytes)
    );
    println!(
        "batched:   peak={:>12} ({:>8.1} MiB)  retained={:>12} ({:>8.1} MiB)  {:?}",
        batched.peak,
        mib(batched.peak),
        batched.retained,
        mib(batched.retained),
        batched.elapsed
    );
    println!(
        "streamed:  peak={:>12} ({:>8.1} MiB)  retained={:>12} ({:>8.1} MiB)  {:?}",
        streamed.peak,
        mib(streamed.peak),
        streamed.retained,
        mib(streamed.retained),
        streamed.elapsed
    );
    println!(
        "reduction: peak={:.1}x  retained={:.1}x",
        batched.peak as f64 / streamed.peak.max(1) as f64,
        batched.retained as f64 / streamed.retained.max(1) as f64,
    );
    println!(
        "peak vs log bytes: batched={:.2}x  streamed={:.2}x",
        batched.peak as f64 / log_bytes as f64,
        streamed.peak as f64 / log_bytes as f64,
    );
}
