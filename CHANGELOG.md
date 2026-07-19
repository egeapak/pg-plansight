# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

From v0.1.0 onward, the entry for each release is generated with
[git-cliff](https://git-cliff.org/) from Conventional-Commit squash-merge
titles — run `just changelog X.Y.Z` (see `cliff.toml`). The 0.1.0 entry below is
curated by hand (the pre-0.1.0 history predates the Conventional-Commit
convention).

## [Unreleased]

_No changes yet._

## [0.1.0] - 2026-07-19

Initial public release.

### Added

- **TUI (`pg-plansight`)** — interactive terminal interface for analyzing
  PostgreSQL `auto_explain` logs: query plan browsing, grouping of similar
  queries, aggregate timing statistics, and sortable results (count, mean, min,
  max, stddev). Copy SQL or execution plans to the clipboard.
- **Log parsing engine (`pg-plansight-core`)** — parallel parsing of `text` and
  `json` `auto_explain` formats, support for gzip/bzip2 compressed logs, file
  globbing, and `--since` / `--until` time filtering. The core is feature-gated
  (`parallel`, `file-io`, `regression-analysis`) so it can be embedded without
  heavy or unsafe dependencies — the extension links a minimal build.
- **Analysis** — index usage, row-estimation accuracy, scan analysis, join
  analysis, buffer usage, parallelization, startup cost, and query-pattern
  analyzers, plus regression detection across executions. Additional analyzers:
  sort-memory, filter-efficiency, index-efficiency, plan-shape,
  estimation-health, and a SQL anti-pattern analyzer. The basic regression
  engine is always available; the statistical (statrs) detector is behind the
  `regression-analysis` feature.
- **Export / import** — persist analysis results as JSON for archiving and
  sharing, and re-open them without re-parsing (`--export` / `--import`).
- **Prometheus exporter (`pg-plansight-exporter`)** — background daemon that
  continuously monitors log files and exports metrics to Prometheus and/or
  OpenTelemetry, with SQLite-backed state tracking and systemd integration.
  Derived metrics include per-query coefficient of variation, total-time share,
  and p95/p99 latency, plus first-seen / last-seen gauges per query group.
- **PostgreSQL extension (`pg_plansight`)** — pgrx-based in-database capture of
  cumulative query statistics (PostgreSQL 13–18), queryable via SQL views with
  tunable capture overhead. Cumulative stats include coefficient of variation
  and SLO-breach counts alongside the timing aggregates.
- **Packaging** — `.deb` and `.rpm` packages for the CLI (x86_64, aarch64,
  armv7, i686) and per-PostgreSQL-major extension packages (13–18, x86_64 and
  arm64), built and smoke-tested on tagged releases.

### Notes

- Minimum Supported Rust Version (MSRV): Rust 1.96.
- The extension is built with pgrx 0.19.1.

[Unreleased]: https://github.com/egeapak/pg-plansight/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/egeapak/pg-plansight/releases/tag/v0.1.0
