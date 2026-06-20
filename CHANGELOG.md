# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
