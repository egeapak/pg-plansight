//! Allocation-churn harness for the parse path.
//!
//! The callgrind profile behind `docs/SIMD_ANALYSIS.md` attributes ~14% of
//! instructions to malloc/free/memcpy/memset — a cost the SIMD scanners do not
//! touch at all. This example measures that side directly: it wraps the global
//! allocator in a counter and reports allocation count, total bytes, and peak
//! live bytes for a full parse.
//!
//! ```bash
//! cargo run --release -p pg-plansight-core --example alloc_profile -- 2000
//! # per-site attribution:
//! valgrind --tool=dhat target/release/examples/alloc_profile 200
//! ```

use pg_plansight_core::log_parser::PostgreSQLLogParser;
use std::alloc::{GlobalAlloc, Layout, System};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

/// Counts every allocation that goes through the global allocator.
///
/// `live`/`peak` are tracked with plain relaxed atomics rather than a lock:
/// the parse path measured here is single-threaded, so the peak is exact, and
/// contention would otherwise distort the very timings this is meant to
/// explain.
struct Counting;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every method forwards to `System`, which is a correct global
// allocator, and only adds bookkeeping around it. Layouts and pointers are
// passed through unchanged, so the safety contract is inherited.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        BYTES.fetch_add(layout.size(), Relaxed);
        let live = LIVE.fetch_add(layout.size(), Relaxed) + layout.size();
        PEAK.fetch_max(live, Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        if new_size > layout.size() {
            let growth = new_size - layout.size();
            BYTES.fetch_add(growth, Relaxed);
            let live = LIVE.fetch_add(growth, Relaxed) + growth;
            PEAK.fetch_max(live, Relaxed);
        } else {
            LIVE.fetch_sub(layout.size() - new_size, Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

fn snapshot() -> (usize, usize, usize) {
    (
        ALLOCS.load(Relaxed),
        BYTES.load(Relaxed),
        PEAK.load(Relaxed),
    )
}

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
    if shape.is_multiple_of(2) {
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

fn realistic_log(num_plans: usize) -> String {
    let mut out = String::with_capacity(num_plans * 2400);
    for i in 0..num_plans {
        let shape = i % 20;
        let table = format!("tbl_{shape}");
        push_plan(&mut out, i, shape, (i as u64) * 47, &table);
    }
    out
}

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(2000);

    // Build the corpus before the first snapshot so generator allocations are
    // not charged to the parser.
    let content = realistic_log(n);
    let bytes_in = content.len();

    let (a0, b0, _) = snapshot();
    let mut parser = PostgreSQLLogParser::new();
    let plans = parser
        .parse_string_with_progress(&content, |_, _| {})
        .unwrap();
    let (a1, b1, peak1) = snapshot();

    let groups = parser.get_processed_queries(&plans);
    let (a2, b2, peak2) = snapshot();

    let plan_count = plans.len();
    println!(
        "input:            {bytes_in} bytes, {plan_count} plans, {} groups",
        groups.len()
    );
    println!(
        "parse:            {:>10} allocs  {:>12} bytes  ({:.1} allocs/plan, {:.1}x input bytes)",
        a1 - a0,
        b1 - b0,
        (a1 - a0) as f64 / plan_count as f64,
        (b1 - b0) as f64 / bytes_in as f64
    );
    println!(
        "grouping:         {:>10} allocs  {:>12} bytes  ({:.1} allocs/plan)",
        a2 - a1,
        b2 - b1,
        (a2 - a1) as f64 / plan_count as f64
    );
    println!(
        "total:            {:>10} allocs  {:>12} bytes",
        a2 - a0,
        b2 - b0
    );
    println!(
        "peak live:        after parse {:.1} MiB, after grouping {:.1} MiB",
        peak1 as f64 / (1 << 20) as f64,
        peak2 as f64 / (1 << 20) as f64
    );
}
