# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Plan analyzers** — sort-memory, filter-efficiency, index-efficiency,
  plan-shape, and estimation-health analyzers, plus a SQL anti-pattern analyzer.
- **Derived exporter metrics** — per-query coefficient of variation, total-time
  share, and p95/p99 latency.
- **First-seen / last-seen** per query group — surfaced in the TUI and exported
  as Prometheus gauges.
- **Extension cumulative stats** — coefficient of variation and SLO-breach
  counts accumulated alongside the existing timing stats.
- **PostgreSQL 19** — the extension now builds against PG19 (pgrx 0.19).

### Changed

- **MSRV is now Rust 1.96** (was 1.88).
- **Extension upgraded to pgrx 0.19.1.**
- Upgraded dependencies across the workspace: sqlparser 0.62, statrs 0.18,
  hashbrown 0.17, bzip2 0.6, prometheus 0.14, OpenTelemetry 0.32, axum 0.8,
  tower-http 0.7, tonic 0.14, rusqlite 0.40, toml 1.0, reqwest 0.12, and the
  dev/test tooling (criterion 0.8, testcontainers 0.27).
- `pg-plansight-core` is feature-gated (`parallel`, `file-io`,
  `regression-analysis`) so it can be embedded without heavy/unsafe deps; the
  extension links a minimal build. The basic regression engine is always
  available; the statistical (statrs) detector is behind `regression-analysis`.
- The shipped extension `.so` is smaller (symbol stripping).

### Removed

- The high-cardinality `query_timestamp` Prometheus label (replaced by the
  first-seen / last-seen gauges).

### Fixed

- OTLP/gRPC metric export silently dropped all metrics under the new
  OpenTelemetry SDK (periodic reader ran exports off the tokio runtime).
- Extension hardening: deterministic UPSERT lock ordering and saturating metric
  sums to avoid lock-order deadlocks and counter overflow.

## [0.1.0] - 2026-06-20

Initial public release.

### Added

- **TUI (`pg-plansight`)** — interactive terminal interface for analyzing
  PostgreSQL `auto_explain` logs: query plan browsing, grouping of similar
  queries, aggregate timing statistics, and sortable results (count, mean, min,
  max, stddev). Copy SQL or execution plans to the clipboard.
- **Log parsing engine (`pg-plansight-core`)** — parallel parsing of `text` and
  `json` `auto_explain` formats, support for gzip/bzip2 compressed logs, file
  globbing, and `--since` / `--until` time filtering.
- **Analysis** — index usage, row-estimation accuracy, scan analysis, join
  analysis, buffer usage, parallelization, startup cost, and query-pattern
  analyzers, plus regression detection across executions.
- **Export / import** — persist analysis results as JSON for archiving and
  sharing, and re-open them without re-parsing (`--export` / `--import`).
- **Prometheus exporter (`pg-plansight-exporter`)** — background daemon that
  continuously monitors log files and exports metrics to Prometheus and/or
  OpenTelemetry, with SQLite-backed state tracking and systemd integration.
- **PostgreSQL extension (`pg_plansight`)** — pgrx-based in-database capture of
  cumulative query statistics (PostgreSQL 13–18), queryable via SQL views with
  tunable capture overhead.
- **Packaging** — `.deb` and `.rpm` packages for x86_64, aarch64, armv7, and
  i686, plus extension packages per PostgreSQL major version.

[Unreleased]: https://github.com/egeapak/pg-plansight/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/egeapak/pg-plansight/releases/tag/v0.1.0
