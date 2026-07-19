#[cfg(feature = "file-io")]
use criterion::BenchmarkId;
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use pg_plansight_core::log_parser::PostgreSQLLogParser;
use std::fmt::Write as FmtWrite;
use std::hint::black_box;
#[cfg(feature = "file-io")]
use std::io::Write;
use std::time::Duration;
#[cfg(feature = "file-io")]
use tempfile::NamedTempFile;

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
/// and one unrelated timestamped log line. Modeled on the fixture used in the
/// log_parser unit tests (~89% continuation lines, ~2.3KB per plan).
fn push_plan(out: &mut String, i: usize, shape: usize, ts_ms: u64, table: &str) {
    let pid = 3_416_000 + i % 800;
    let duration = 10.0 + ((i % 997) as f64) * 1.37;
    writeln!(
        out,
        "{} UTC [{pid}] LOG:  duration: {duration:.3} ms  plan:",
        format_ts(ts_ms)
    )
    .unwrap();

    // Query Text: plus 3-5 tab-prefixed SQL continuation lines
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

    // Text plan block: 10-18 tab-prefixed lines, first one carries the
    // (cost=..) shape that flips the state machine into plan parsing.
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
    // One long (~300 char) Filter line, as auto_explain routinely emits.
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

    // Unrelated timestamped log line terminating the plan.
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

/// Build a realistic auto_explain log with `num_plans` plan blocks, cycling
/// through ~20 distinct query shapes (distinct table names => distinct
/// fingerprints) with monotonically advancing timestamps.
fn create_realistic_auto_explain_log(num_plans: usize) -> String {
    let mut out = String::with_capacity(num_plans * 2400);
    for i in 0..num_plans {
        let shape = i % 20;
        let table = format!("tbl_{shape}");
        push_plan(&mut out, i, shape, (i as u64) * 47, &table);
    }
    out
}

fn bench_parse_string_e2e(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse_string_e2e");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(15));

    let content = create_realistic_auto_explain_log(10_000);
    group.throughput(Throughput::Bytes(content.len() as u64));
    group.bench_function("10k_plans", |b| {
        b.iter(|| {
            let mut parser = PostgreSQLLogParser::new();
            let plans = parser
                .parse_string_with_progress(black_box(&content), |_, _| {})
                .unwrap();
            black_box(plans)
        });
    });

    group.finish();
}

fn bench_utf8_validation(c: &mut Criterion) {
    let mut group = c.benchmark_group("utf8_validation");

    // ~4000 realistic ASCII log-line byte vectors, mirroring the per-line
    // validation the parser hot loop performs on its read buffer.
    let content = create_realistic_auto_explain_log(250);
    let lines: Vec<Vec<u8>> = content
        .lines()
        .cycle()
        .take(4000)
        .map(|l| l.as_bytes().to_vec())
        .collect();
    let total_bytes: u64 = lines.iter().map(|l| l.len() as u64).sum();
    group.throughput(Throughput::Bytes(total_bytes));

    group.bench_function("std_from_utf8", |b| {
        b.iter(|| {
            for line in &lines {
                black_box(std::str::from_utf8(black_box(line)).unwrap());
            }
        });
    });

    group.bench_function("simdutf8_basic_from_utf8", |b| {
        b.iter(|| {
            for line in &lines {
                black_box(simdutf8::basic::from_utf8(black_box(line)).unwrap());
            }
        });
    });

    group.finish();
}

#[cfg(feature = "file-io")]
fn create_sample_log_data(num_queries: usize) -> String {
    let mut log_content = String::new();

    // Real auto_explain output: the entry header is a timestamped
    // "duration: ... plan:" line, and the query text + plan are TAB-INDENTED
    // CONTINUATION LINES of that entry (not separate LOG lines). Emitting the
    // format the parser actually consumes is essential — a previous version
    // of this generator produced standalone LOG lines and the benchmark
    // measured line scanning + regex rejection instead of plan assembly.
    for i in 0..num_queries {
        let query_id = i + 1;
        let duration = (i as f64 * 0.5 + 10.0) % 1000.0; // Varying durations
        let process_id = 1000 + i % 100;

        log_content.push_str(&format!(
            "2024-01-01 10:{:02}:{:02}.123 UTC [{}] LOG:  duration: {:.3} ms  plan:\n",
            i % 60,
            (i * 2) % 60,
            process_id,
            duration
        ));
        log_content.push_str(&format!(
            "\tQuery Text: SELECT * FROM users WHERE id = ${} AND status = 'active'\n",
            query_id
        ));
        log_content.push_str(&format!(
            "\tSeq Scan on users  (cost=0.00..{:.2} rows={} width={})\n",
            duration * 10.0,
            (i % 50) + 1,
            (i % 200) + 100
        ));
        log_content.push_str(&format!(
            "\t  Filter: ((id = ${}) AND (status = 'active'::text))\n",
            query_id
        ));

        // Interleave ordinary log lines (entry terminators + realistic noise).
        log_content.push_str(&format!(
            "2024-01-01 10:{:02}:{:02}.223 UTC [{}] LOG:  duration: {:.3} ms  statement: SELECT * FROM users WHERE id = ${} AND status = 'active'\n",
            i % 60, (i * 2) % 60, process_id, duration, query_id
        ));
        if i % 10 == 0 {
            log_content.push_str(&format!(
                "2024-01-01 10:{:02}:{:02}.323 UTC [{}] LOG:  checkpoints_timed: {}\n",
                i % 60,
                (i * 2) % 60,
                process_id,
                i / 10
            ));
        }
    }

    log_content
}

#[cfg(feature = "file-io")]
fn bench_original_parser(c: &mut Criterion) {
    let mut group = c.benchmark_group("original_parser");

    for size in [100, 500, 1000, 2000].iter() {
        let log_content = create_sample_log_data(*size);
        // Guard against measuring the wrong code path: the generated log must
        // actually produce plans, or the benchmark is meaningless.
        let parsed = PostgreSQLLogParser::new()
            .parse_string_with_progress(&log_content, |_, _| {})
            .expect("benchmark input must parse");
        assert_eq!(
            parsed.len(),
            *size,
            "benchmark log must yield one plan per generated entry"
        );
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(log_content.as_bytes()).unwrap();
        let temp_path = temp_file.path().to_path_buf();

        group.bench_with_input(BenchmarkId::new("queries", size), size, |b, _| {
            let mut parser = PostgreSQLLogParser::new();
            b.iter(|| {
                let result = parser.parse_file_with_progress(black_box(&temp_path), |_, _| {});
                black_box(result)
            });
        });
    }

    group.finish();
}

fn bench_string_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("string_operations");

    let sample_lines: Vec<String> = (0..1000)
        .map(|i| {
            format!(
                "2024-01-01 10:00:{:02}.123 UTC [{}] LOG:  Some log message with data {}",
                i % 60,
                1000 + i,
                i
            )
        })
        .collect();

    group.bench_function("new_string_each_iteration", |b| {
        b.iter(|| {
            for line in &sample_lines {
                let mut new_line = String::new();
                new_line.push_str(line);
                let trimmed = new_line.trim_end().to_string();
                black_box(trimmed);
            }
        });
    });

    group.bench_function("reuse_string_with_clear", |b| {
        b.iter(|| {
            let mut reused_line = String::with_capacity(512);
            for line in &sample_lines {
                reused_line.clear();
                reused_line.push_str(line);
                let trimmed = reused_line.trim_end();
                black_box(trimmed);
            }
        });
    });

    group.bench_function("direct_trim_no_allocation", |b| {
        b.iter(|| {
            for line in &sample_lines {
                let trimmed = line.trim_end();
                black_box(trimmed);
            }
        });
    });

    group.finish();
}

fn bench_regex_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("regex_operations");

    let parser = PostgreSQLLogParser::new();
    let sample_lines: Vec<String> = (0..1000)
        .map(|i| {
            format!(
                "2024-01-01 10:00:{:02}.123 UTC [{}] LOG:  duration: {:.3} ms  plan:",
                i % 60,
                1000 + i,
                i as f64 * 0.5 + 10.0
            )
        })
        .collect();

    group.bench_function("duration_regex_matching", |b| {
        b.iter(|| {
            for line in &sample_lines {
                if let Some(captures) = parser.regex_patterns.duration_regex.captures(line) {
                    let duration_str = captures.get(1).unwrap().as_str();
                    let _duration: f64 = duration_str.parse().unwrap_or(0.0);
                    black_box(_duration);
                }
            }
        });
    });

    group.finish();
}

#[cfg(feature = "file-io")]
criterion_group!(
    benches,
    bench_original_parser,
    bench_string_operations,
    bench_regex_operations,
    bench_parse_string_e2e,
    bench_utf8_validation
);
#[cfg(not(feature = "file-io"))]
criterion_group!(
    benches,
    bench_string_operations,
    bench_regex_operations,
    bench_parse_string_e2e,
    bench_utf8_validation
);
criterion_main!(benches);
