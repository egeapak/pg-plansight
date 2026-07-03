//! Memory profile of query grouping (`get_processed_queries`).
//!
//! Generates a grouped auto_explain log in memory, parses it, then measures
//! allocator activity of the grouping/statistics phase alone via a counting
//! global allocator:
//!   - peak_delta:        peak live bytes above the pre-grouping level
//!   - retained_delta:    live bytes still held after grouping returns
//!   - total_alloc_delta: cumulative bytes allocated during grouping
//!
//! Usage: mem_grouping [num_plans] [num_shapes]   (defaults: 50000 500)
//! Run with RAYON_NUM_THREADS=1 for deterministic numbers.

use pg_plansight_core::log_parser::PostgreSQLLogParser;
use std::alloc::{GlobalAlloc, Layout, System};
use std::fmt::Write as FmtWrite;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Live bytes currently allocated.
static CURRENT: AtomicUsize = AtomicUsize::new(0);
/// High-water mark of live bytes (fetch-max maintained in alloc paths).
static PEAK: AtomicUsize = AtomicUsize::new(0);
/// Cumulative bytes ever allocated.
static TOTAL: AtomicUsize = AtomicUsize::new(0);

struct CountingAllocator;

fn record_alloc(size: usize) {
    TOTAL.fetch_add(size, Ordering::Relaxed);
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

/// Format a millisecond offset from 2025-06-12 00:00:00.000 as a PostgreSQL
/// log timestamp ("YYYY-MM-DD HH:MM:SS.mmm").
fn format_ts(ms: u64) -> String {
    let day = 12 + ms / 86_400_000;
    let rem = ms % 86_400_000;
    format!(
        "2025-06-{:02} {:02}:{:02}:{:02}.{:03}",
        day,
        rem / 3_600_000,
        (rem / 60_000) % 60,
        (rem / 1000) % 60,
        rem % 1000
    )
}

/// Append one auto_explain plan block (same shape as the bench generators).
fn push_plan(out: &mut String, i: usize, shape: usize, ts_ms: u64, table: &str) {
    let pid = 3_416_000 + i % 800;
    let duration = 10.0 + ((i % 997) as f64) * 1.37;
    writeln!(
        out,
        "{} UTC [{pid}] LOG:  duration: {duration:.3} ms  plan:",
        format_ts(ts_ms)
    )
    .unwrap();

    writeln!(
        out,
        "\tQuery Text: SELECT t.\"Id\", t.\"Status\", t.\"CreatedAt\", t.\"Payload\", t.\"OwnerId\""
    )
    .unwrap();
    writeln!(out, "\tFROM \"public\".\"{table}\" AS t").unwrap();
    writeln!(
        out,
        "\tWHERE t.\"Status\" = $1 AND t.\"CreatedAt\" >= $2 AND t.\"OwnerId\" IN ($3, $4)"
    )
    .unwrap();
    if shape % 2 == 0 {
        writeln!(out, "\tORDER BY t.\"CreatedAt\" DESC").unwrap();
    }
    if shape % 4 < 3 {
        writeln!(out, "\tLIMIT $5").unwrap();
    }

    writeln!(out, "\tLimit  (cost=0.43..599.04 rows=1000 width=56)").unwrap();
    writeln!(
        out,
        "\t  Output: \"Id\", \"Status\", \"CreatedAt\", \"Payload\", \"OwnerId\""
    )
    .unwrap();
    writeln!(
        out,
        "\t  ->  Index Scan Backward using \"IX_{table}_CreatedAt\" on \"public\".\"{table}\" t  (cost=0.43..95610.13 rows=159718 width=56)"
    )
    .unwrap();
    writeln!(
        out,
        "\t        Output: \"Id\", \"Status\", \"CreatedAt\", \"Payload\", \"OwnerId\""
    )
    .unwrap();
    writeln!(out, "\t        Index Cond: (t.\"CreatedAt\" IS NOT NULL)").unwrap();
    writeln!(
        out,
        "\t        Filter: ((NOT t.\"Deleted\") AND (((t.\"Level\" > '66'::double precision) AND (t.\"Level\" <= '99'::double precision) AND (t.\"CreatedAt\" <= '2025-06-11 23:00:15.671506+00'::timestamp with time zone)) OR ((t.\"Level\" <= '66'::double precision) AND (t.\"CreatedAt\" <= '2025-06-11 23:30:15.671506+00'::timestamp with time zone))) AND (t.\"Status\" = ANY ('{{1,2,3,4}}'::integer[])))"
    )
    .unwrap();
    for j in 0..(4 + i % 9) {
        writeln!(
            out,
            "\t        ->  Bitmap Heap Scan on \"public\".\"{table}_c{j}\" c{j}  (cost=12.15..870.{j:02} rows={} width=24)",
            100 + j * 7
        )
        .unwrap();
    }

    writeln!(
        out,
        "{} UTC [{}] LOG:  checkpoint complete: wrote {} buffers (0.4%); sync files={}",
        format_ts(ts_ms + 3),
        pid + 1,
        100 + i % 500,
        i % 32
    )
    .unwrap();
}

/// Build a log whose plans fall into exactly `num_shapes` fingerprint groups,
/// with timestamps spread across ~24h.
fn make_grouped_log(num_plans: usize, num_shapes: usize) -> String {
    let step_ms = 86_400_000 / num_plans as u64;
    let mut out = String::with_capacity(num_plans * 2400);
    for i in 0..num_plans {
        let shape = i % num_shapes;
        let table = format!("tbl_{shape}");
        push_plan(&mut out, i, shape, (i as u64) * step_ms, &table);
    }
    out
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
    let mut parser = PostgreSQLLogParser::new();
    let plans = parser
        .parse_string_with_progress(&content, |_, _| {})
        .expect("parse failed");
    drop(content);
    assert_eq!(plans.len(), num_plans);

    // Snapshot allocator state so the deltas below cover grouping only.
    let before_current = CURRENT.load(Ordering::Relaxed);
    PEAK.store(before_current, Ordering::Relaxed);
    let before_total = TOTAL.load(Ordering::Relaxed);

    let out = parser.get_processed_queries(&plans);

    let peak_delta = PEAK.load(Ordering::Relaxed).saturating_sub(before_current);
    let retained_delta = CURRENT
        .load(Ordering::Relaxed)
        .saturating_sub(before_current);
    let total_alloc_delta = TOTAL.load(Ordering::Relaxed) - before_total;

    println!(
        "num_plans={num_plans} num_shapes={num_shapes} groups={}",
        out.len()
    );
    println!("peak_delta={peak_delta}");
    println!("retained_delta={retained_delta}");
    println!("total_alloc_delta={total_alloc_delta}");
}
