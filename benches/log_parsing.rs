use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use pg_auto_explain_analyze_rs::log_parser::PostgreSQLLogParser;
use std::io::Write;
use tempfile::NamedTempFile;

fn create_sample_log_data(num_queries: usize) -> String {
    let mut log_content = String::new();

    for i in 0..num_queries {
        let query_id = i + 1;
        let duration = (i as f64 * 0.5 + 10.0) % 1000.0; // Varying durations
        let process_id = 1000 + i % 100;

        // Add log entry with duration and plan start
        log_content.push_str(&format!(
            "2024-01-01 10:{:02}:{:02}.123 UTC [{}] LOG:  duration: {:.3} ms  plan:\n",
            i % 60,
            (i * 2) % 60,
            process_id,
            duration
        ));

        // Add Query Text
        log_content.push_str(&format!(
            "2024-01-01 10:{:02}:{:02}.124 UTC [{}] LOG:  Query Text: SELECT * FROM users WHERE id = ${} AND status = 'active'\n",
            i % 60, (i * 2) % 60, process_id, query_id
        ));

        // Add execution plan (simplified)
        log_content.push_str(&format!(
            "2024-01-01 10:{:02}:{:02}.125 UTC [{}] LOG:  Seq Scan on users  (cost=0.00..{:.2} rows={} width={})\n",
            i % 60, (i * 2) % 60, process_id, duration * 10.0, (i % 50) + 1, (i % 200) + 100
        ));
        log_content.push_str(&format!(
            "2024-01-01 10:{:02}:{:02}.126 UTC [{}] LOG:    Filter: ((id = ${}) AND (status = 'active'::text))\n",
            i % 60, (i * 2) % 60, process_id, query_id
        ));

        // Add parameters if present
        if i % 3 == 0 {
            log_content.push_str(&format!(
                "2024-01-01 10:{:02}:{:02}.127 UTC [{}] LOG:  parameters: ${} = '{}'\n",
                i % 60,
                (i * 2) % 60,
                process_id,
                query_id,
                query_id * 100
            ));
        }

        // Add statement end
        log_content.push_str(&format!(
            "2024-01-01 10:{:02}:{:02}.128 UTC [{}] LOG:  duration: {:.3} ms  statement: SELECT * FROM users WHERE id = ${} AND status = 'active'\n",
            i % 60, (i * 2) % 60, process_id, duration, query_id
        ));

        // Add some noise lines
        if i % 10 == 0 {
            log_content.push_str(&format!(
                "2024-01-01 10:{:02}:{:02}.129 UTC [{}] LOG:  checkpoints_timed: {}\n",
                i % 60,
                (i * 2) % 60,
                process_id,
                i / 10
            ));
        }
    }

    log_content
}

fn bench_original_parser(c: &mut Criterion) {
    let mut group = c.benchmark_group("original_parser");

    for size in [100, 500, 1000, 2000].iter() {
        let log_content = create_sample_log_data(*size);
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(log_content.as_bytes()).unwrap();
        let temp_path = temp_file.path().to_path_buf();

        group.bench_with_input(BenchmarkId::new("queries", size), size, |b, _| {
            let mut parser = PostgreSQLLogParser::new();
            b.iter(|| {
                let result = parser.parse_file_with_progress(black_box(&temp_path), |_| {});
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

criterion_group!(
    benches,
    bench_original_parser,
    bench_string_operations,
    bench_regex_operations
);
criterion_main!(benches);
