//! Benchmarks for query grouping and per-group statistics
//! (`PostgreSQLLogParser::get_processed_queries`), decoupled from log parsing:
//! plans are parsed once outside the timed body.

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use pg_plansight_core::log_parser::PostgreSQLLogParser;
use std::fmt::Write as FmtWrite;
use std::hint::black_box;
use std::time::Duration;

/// Format a millisecond offset from 2025-06-12 00:00:00.000 as a PostgreSQL
/// log timestamp ("YYYY-MM-DD HH:MM:SS.mmm"). Offsets beyond 24h roll into the
/// next day so generated timestamps stay monotonically increasing.
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

/// Append one auto_explain plan block to `out`: a timestamped duration header,
/// a `Query Text:` continuation, several tab-prefixed SQL continuation lines,
/// a tab-prefixed text plan block (10-18 lines, including a long Filter line),
/// and one unrelated timestamped log line (same shape as the log_parsing
/// bench generator).
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

/// Build a log whose plans fall into exactly `num_shapes` fingerprint groups
/// (table name = `tbl_{i % num_shapes}`), with timestamps spread across ~24h.
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

fn bench_grouping(c: &mut Criterion) {
    let mut group = c.benchmark_group("grouping");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(15));

    for (name, num_plans, num_shapes) in [
        ("10k_plans_100_shapes", 10_000usize, 100usize),
        ("10k_plans_2000_shapes", 10_000, 2000),
        ("50k_plans_20_shapes", 50_000, 20),
    ] {
        // Parse once outside the timed body; the benchmark measures grouping
        // and statistics only.
        let content = make_grouped_log(num_plans, num_shapes);
        let mut parser = PostgreSQLLogParser::new();
        let plans = parser
            .parse_string_with_progress(&content, |_, _| {})
            .unwrap();
        drop(content);
        assert_eq!(plans.len(), num_plans);
        // Warm the fingerprint cache so the timed body measures grouping and
        // aggregation rather than first-time SQL normalization.
        let warm = parser.get_processed_queries(&plans);
        assert_eq!(warm.len(), num_shapes);
        drop(warm);

        group.throughput(Throughput::Elements(num_plans as u64));
        group.bench_function(name, |b| {
            b.iter(|| black_box(parser.get_processed_queries(black_box(&plans))));
        });
    }

    group.finish();
}

criterion_group!(benches, bench_grouping);
criterion_main!(benches);
