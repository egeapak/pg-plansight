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

### Breaking

- **Metric label rename: `file_path` → `log_path_pattern`** on
  `pg_plansight_logs_parsed_total` and `pg_plansight_parse_errors_total`. The
  label now carries the configured glob rather than the concrete filename.
  Rotation schemes such as `log_filename = 'postgresql-%Y-%m-%d.log'` minted a
  new label value every rotation, and Prometheus client label sets are never
  evicted — so these two families grew without bound for the daemon's whole
  lifetime (the cardinality limiter only ever covered `normalized_query_hash`).
  **Dashboards and alerts selecting on `file_path` must be updated.** Per-file
  detail remains in the log output.
- **`filters.include_databases` is rejected.** It compared configured names
  against a hardcoded `"unknown"`, so any non-empty list silently dropped every
  query while the daemon logged successful collection. Remove the key from
  `config.toml`; per-database filtering returns when the core parser learns to
  read `log_line_prefix` (`%d`).
- **Extension capture defaults changed.** `plansight.min_duration_ms` now
  defaults to `1` (was `0`) and `plansight.sample_rate` to `0.8` (was `1.0`).
  The old defaults ran a full `EXPLAIN (ANALYZE, BUFFERS, WAL, SETTINGS)` on
  every statement while the capture ring could only retain a fraction of them.
  Set them explicitly to restore exhaustive capture.
- The exporter now **exits non-zero** when the metrics server or scheduler dies
  (previously exit 0). Supervisors configured with `Restart=on-failure` will
  now see these as failures, which is the intended behaviour.

### Added

- `log_parsing.max_read_bytes_per_cycle` (default 64 MiB) caps how much unread
  content one poll cycle ingests from a file. The hold-back read allocated the
  entire unread range in a single `Vec`, so a restart against a log that grew
  while the daemon was down allocated the whole backlog at once — and an
  allocation failure in Rust aborts the process, repeating on every restart.
  The remainder is deferred to the next cycle, so nothing is skipped; 0 restores
  the old unbounded behaviour.
- `pg-plansight --redact` omits query text, formatted text, plan text, and
  host/user metadata from a JSON export, keeping fingerprints and statistics.
  Use it when an export leaves the host: query and plan text both embed literal
  values.
- `pg-plansight-exporter check-config` validates a config file and exits
  non-zero with a diagnostic. Replaces the `--dry-run` / `--check-config` flags
  the docs referenced but which never existed.
- `plansight_check()` reports `auto_explain` preloaded *before* `pg_plansight`,
  which silently disables hook capture entirely.
- Documented extension rollback in `docs/INSTALLATION.md`, including the
  ordering hazard: removing the package while the library is still in
  `shared_preload_libraries` prevents PostgreSQL from starting.

### Fixed

- **A single byte could abort an entire parse run.** The timezone-offset parser
  sliced by byte index while measuring length in bytes; `regex`'s Unicode-aware
  `\d` admits multi-byte digits, so one such character panicked the rayon
  worker and failed the whole run.
- **Packaged installs could not start.** The systemd unit's `ExecStart` was
  rejected by clap, and `postinst`/RPM scriptlets generated a `config.toml`
  using sections that do not exist in the schema. The RPM's maintainer
  scriptlets were never wired into its metadata at all.
- The exporter now handles **SIGTERM**, so `systemctl stop` runs the metric
  flush instead of dropping up to a full OTel export interval.
- `RUST_LOG` unset no longer means an effective log level of ERROR.
- systemd unit: `StartLimit*` moved to `[Unit]` (systemd ignored them in
  `[Service]`, so the crash-loop brake did not exist), `AF_UNIX`/`AF_NETLINK`
  allowed so hostname resolution works, `ReadOnlyPaths` made tolerant of paths
  absent on RHEL, memory ceilings added, and the state-db environment variable
  corrected to the one the binary reads.
- Releases are now gated on a green tree and on the package-installation tests.

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
