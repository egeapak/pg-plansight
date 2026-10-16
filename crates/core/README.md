# pg-plansight-core

Parsing and analysis library behind [pg-plansight](https://github.com/egeapak/pg-plansight),
a toolkit for PostgreSQL `auto_explain` logs.

- Streams `auto_explain` output (text and JSON plans) from files, including gzip
  and bzip2, or from memory.
- Normalizes and fingerprints queries, so executions of the same statement shape
  are grouped and aggregated (counts, durations, p95/p99 percentiles).
- Flags anti-patterns and detects performance regressions between runs.
- Exports and imports analysis results as JSON.

If you want a ready-made tool rather than a library, install the terminal UI
(`pg-plansight`) or the Prometheus / OpenTelemetry daemon
(`pg-plansight-exporter`) instead.

## Usage

```toml
[dependencies]
pg-plansight-core = "0.2"
```

```rust
use pg_plansight_core::{PostgreSQLLogParser, QueryGrouper};

let log = "\
2025-06-12 10:00:00.000 UTC [100] LOG:  duration: 12.345 ms  plan:
\tQuery Text: SELECT * FROM users WHERE id = 1
\tSeq Scan on users  (cost=0.00..1.00 rows=1 width=4) (actual time=0.010..0.020 rows=1 loops=1)
";

let mut parser = PostgreSQLLogParser::new();
let mut grouper = QueryGrouper::new();
parser.parse_string_into_grouper(log, |_progress, _plans| {}, &mut grouper)?;

assert_eq!(grouper.group_count(), 1);
# Ok::<(), anyhow::Error>(())
```

For files, `parse_file_into_grouper` streams from disk and
`parse_multiple_files_async` parses several files in parallel.

## Features

| Feature               | Default | Adds                                                        |
| --------------------- | :-----: | ----------------------------------------------------------- |
| `parallel`            |   yes   | rayon-based parallel grouping and multi-file parsing        |
| `file-io`             |   yes   | file reading, gzip/bzip2, JSON export/import, hostname      |
| `regression-analysis` |   yes   | statistical regression detection (`statrs`)                 |

Disable default features to embed the parser with no threads and no I/O surface,
as the PostgreSQL extension does.

## Performance

The hot path uses portable SIMD kernels written with
[`fearless_simd`](https://crates.io/crates/fearless_simd), with scalar fallbacks.
There is no `unsafe` in them.

## License

MIT
