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

- **All-literal `IN (...)` lists collapse to a single placeholder**, so
  `IN (1,2,3)` and `IN (1,2,3,4)` are one query group. Every distinct list
  length was previously its own fingerprint, which fragments ORM batch loads
  into hundreds of groups — each also an extra Prometheus series and another
  row competing in the top-N view. `pg_stat_statements` collapses these the
  same way. Query hashes for such queries change, so historical series will not
  line up across the upgrade.
- **Metric label rename: `file_path` → `log_path_pattern`** on
  `pg_plansight_logs_parsed_total` and `pg_plansight_parse_errors_total`. The
  label now carries the configured glob rather than the concrete filename.
  Rotation schemes such as `log_filename = 'postgresql-%Y-%m-%d.log'` minted a
  new label value every rotation, and Prometheus client label sets are never
  evicted — so these two families grew without bound for the daemon's whole
  lifetime (the cardinality limiter only ever covered `normalized_query_hash`).
  **Dashboards and alerts selecting on `file_path` must be updated.** Per-file
  detail remains in the log output.
- **Unknown config keys are now rejected.** A typo such as `poll_intervall` was
  silently ignored and the default used — and a SIGHUP reload still logged
  "Configuration reloaded successfully". This will refuse to start a config that
  previously "worked"; run `check-config` before restarting.
- **`[pushgateway]` is removed.** It was never wired up: `PushgatewayClient` was
  only ever constructed by its own unit test, so the section did nothing, while
  a *partial* section was a hard startup failure on a no-op feature. The section
  is tolerated with a warning for one release, then dropped.
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

- **`GET /ready`** reports whether a collection cycle has succeeded within three
  poll intervals (503 otherwise), and the HTTP listener now starts regardless of
  which metrics backend is configured. Previously it only started when
  `"prometheus"` was in `metrics.backends`, so an OpenTelemetry-only deployment
  had no HTTP surface at all and nothing for a probe to hit. `/health` remains
  an unconditional liveness check.
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

- **Disk spills reported "using 0 kB".** PostgreSQL packs several fields onto
  one text plan line (`Sort Method: external merge  Disk: 524288kB`,
  `Heap Blocks: exact=100 lossy=900`, `Buckets: 1024  Batches: 4`), and the
  generic first-colon split swallowed the tail into the first key. `Sort Space
  Used`, `Sort Space Type`, `Batches` and `Heap Blocks: lossy` therefore never
  existed on text plans — the format auto_explain emits by default — so the
  spill size was always 0, its size-based severity escalation could never fire,
  and the hash-batch and lossy-bitmap rules were dead. PostgreSQL's JSON
  spellings (`Exact Heap Blocks`, `Hash Batches`, …) are now aliased onto the
  same keys, so both formats agree.
- **PG18 JSON plans were silently dropped.** PostgreSQL 18 prints `Actual Rows`
  as a per-loop *average* with decimals when `loops > 1`. Deserializing into an
  integer made serde reject the whole document, which the state machine then
  demoted to plain query text — so with `auto_explain.log_format = json` on
  PG18, every plan containing a nested-loop node vanished from the analysis
  with only a warning.
- **Group standard deviation used the population divisor** (N) while the
  function it documents itself as matching uses the sample divisor (N-1).
  This is the value that reaches `std_dev_ms` and the export; small groups —
  which is what slow queries usually form — were understated by up to ~30%.
- **A zero baseline produced an infinite "Critical" regression.** The basic
  regression path divided by the first-half average without a guard, so a
  first half of all-zero durations reported `+inf%` at maximum confidence.
  Not hypothetical: `auto_explain.log_min_duration = 0` makes
  `duration: 0.000 ms` entries routine.
- **Heap-fetch ratio was inflated by the loop count.** `Heap Fetches` is
  cumulative across loops while `rows=N` is the per-loop average, so the ratio
  on the inner side of a nested loop was overstated by a factor of `loops`,
  producing false "visibility map" findings.
- **Seasonality was detected in logs that do not span full days.** Unobserved
  hours were treated as `0.0` and the mean divided by a fixed 24, understating
  the mean and inflating the variance — so business-hours-only traffic reported
  a "significant daily pattern" that was an artefact of the missing buckets.
- **Exports were not reproducible.** Groups with equal total duration came out
  in the iteration order of a randomly-seeded hash map, so two runs over the
  same log produced byte-different JSON that could not be diffed or checksummed.
  Ties now break on the query hash.
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
- `pg_plansight_exporter_up` now tracks the most recent cycle's outcome instead
  of being set to 1 once at construction and never updated — any alert on it was
  previously decorative. Note that liveness is properly expressed by Prometheus's
  own synthetic `up{job=...}`; for "is it keeping up", alert on staleness of
  `pg_plansight_last_successful_parse_timestamp`.
- `process` and `process-rest` now reject an empty `metrics.backends` like
  `daemon` always did. They previously built an empty composite backend that
  discarded every metric while still advancing checkpoints to EOF, so a backfill
  silently consumed the backlog into nothing.
- systemd unit: `StartLimit*` moved to `[Unit]` (systemd ignored them in
  `[Service]`, so the crash-loop brake did not exist), `AF_UNIX`/`AF_NETLINK`
  allowed so hostname resolution works, `ReadOnlyPaths` made tolerant of paths
  absent on RHEL, memory ceilings added, and the state-db environment variable
  corrected to the one the binary reads.
- Releases are now gated on a green tree and on the package-installation tests,
  and carry a `SHA256SUMS` file.
- **RUSTSEC-2026-0204** (invalid pointer dereference in `crossbeam-epoch`,
  reached via `rayon-core` — the parser's hot path) and the `quick-xml` DoS
  advisories RUSTSEC-2026-0194/0195 are resolved by dependency updates. A new
  `cargo-deny` CI job now gates advisories, licenses and sources over both
  workspaces.
- `pg-plansight` and `pg-plansight-exporter` declared `pg-plansight-core` by
  path with no version, so neither crate was actually publishable despite
  carrying publishable metadata.

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
