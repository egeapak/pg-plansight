# Plansight

[![CI](https://github.com/egeapak/pg-plansight/actions/workflows/ci.yml/badge.svg)](https://github.com/egeapak/pg-plansight/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![CodSpeed](https://img.shields.io/endpoint?url=https://codspeed.io/badge.json)](https://app.codspeed.io/egeapak/pg-plansight?utm_source=badge)

**Plansight** turns PostgreSQL `auto_explain` output into something you can
actually reason about. It parses query plans out of your logs (or captures them
live inside the server), groups similar queries, computes aggregate timing
statistics, and surfaces concrete optimization findings — index suggestions,
bad row estimates, expensive scans, costly joins, and more.

It ships as three complementary tools that share one analysis engine:

| Component | What it is | When to use it |
|-----------|------------|----------------|
| **`pg-plansight`** | Interactive terminal UI (TUI) | Ad-hoc, exploratory analysis of one or more log files |
| **`pg-plansight-exporter`** | Background daemon | Continuous monitoring; exports metrics to Prometheus / OpenTelemetry |
| **`pg_plansight`** | PostgreSQL extension (pgrx) | In-database capture, no log files required (like `pg_stat_statements`) |

---

## Quick start

### Analyze a log file in the TUI

```bash
# From a release package (see Installation below)
pg-plansight /var/log/postgresql/postgresql.log

# Multiple files and globs work too; compressed logs (.gz, .bz2) are supported
pg-plansight '/var/log/postgresql/postgresql-*.log.gz'

# Only look at recent activity
pg-plansight --since 2h /var/log/postgresql/postgresql.log
```

You'll need PostgreSQL configured to emit `auto_explain` output, for example:

```ini
# postgresql.conf
shared_preload_libraries = 'auto_explain'
auto_explain.log_min_duration = 0      # log every statement (tune for prod)
auto_explain.log_analyze = on
auto_explain.log_buffers = on
auto_explain.log_format = text         # 'text' or 'json' are both supported
```

### Build and run from source

```bash
# Build the whole workspace
cargo build --release

# Run the TUI against a log file
cargo run --release -- /path/to/postgresql.log
```

The default binary is the TUI (`crates/tui`). See
[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) for the full development setup.

---

## TUI controls

| Key | Action |
|-----|--------|
| `↑` / `↓` | Move between queries |
| `Tab` | Switch focus between panes |
| `Enter` | Open the detailed view for the selected query |
| `PgUp` / `PgDn`, `←` / `→` | Scroll |
| `c` / `m` / `n` / `x` / `s` | Sort by **c**ount / **m**ean / mi**n** / ma**x** / **s**tddev |
| `Ctrl+S` / `Ctrl+E` | Copy the **S**QL / the **E**xecution plan to the clipboard |
| `Esc` | Back |
| `q` | Quit |

---

## Export / import

Plansight can parse logs without opening the TUI and persist the analysis as
JSON for archiving, diffing, or sharing:

```bash
# Parse and export (non-interactive)
pg-plansight --export analysis.json /var/log/postgresql/postgresql.log

# Re-open a previous analysis instantly, no re-parsing
pg-plansight --import analysis.json
```

See [docs/EXPORT_IMPORT.md](docs/EXPORT_IMPORT.md) for the format and details.

---

## Continuous monitoring (exporter)

`pg-plansight-exporter` runs as a systemd service, tails your logs, and exposes
Prometheus / OpenTelemetry metrics for query performance, execution patterns,
and plan analysis:

```bash
sudo systemctl enable --now pg-plansight-exporter.service
# Configuration: /etc/pg-plansight-exporter/config.toml
```

See the [exporter README](crates/exporter/README.md) and
[docs/CONFIGURATION.md](docs/CONFIGURATION.md).

---

## In-database capture (PostgreSQL extension)

`pg_plansight` is a [`pgrx`](https://github.com/pgcentralfoundation/pgrx)
extension that captures cumulative query statistics *inside* the server (no log
files), supporting **PostgreSQL 13–18**:

```sql
CREATE EXTENSION pg_plansight;
SELECT * FROM plansight.statements_summary ORDER BY total_time_ms DESC;
```

See the [extension README](crates/pg_extension/README.md),
[docs/PGRX_EXTENSION_DESIGN.md](docs/PGRX_EXTENSION_DESIGN.md), and
[docs/VIEWS_REFERENCE.md](docs/VIEWS_REFERENCE.md).

---

## Installation

Pre-built `.deb` and `.rpm` packages are published for x86_64, aarch64, armv7,
and i686 on each [release](https://github.com/egeapak/pg-plansight/releases).
Full instructions — including supported distributions and the PostgreSQL
extension — are in [docs/INSTALLATION.md](docs/INSTALLATION.md).

Binaries need glibc 2.28 or newer (Debian 10+, Ubuntu 20.04+, RHEL 8+). Asset
names carry a package revision after the version, so 0.2.0 ships as
`pg-plansight_0.2.0-1_amd64.deb`:

```bash
# Debian / Ubuntu
sudo dpkg -i pg-plansight_<version>-1_amd64.deb

# RHEL / Fedora
sudo dnf install pg-plansight-<version>-1.x86_64.rpm
```

Upgrading from 0.1.0 needs a few manual steps — see
[docs/INSTALLATION.md](docs/INSTALLATION.md#upgrading-from-010).

---

## Project layout

```
crates/
  core/          # Parsing + analysis engine (shared library)
  tui/           # Interactive terminal UI  ->  pg-plansight binary
  exporter/      # Prometheus/OTel daemon   ->  pg-plansight-exporter binary
  pg_extension/  # pgrx PostgreSQL extension (own workspace; PG 13–18)
  bench-harness/ # Cross-version benchmarking harness (Docker)
  core/examples/ # Runnable library examples (cargo run -p pg-plansight-core --example ...)
docs/            # Installation, configuration, design, and feature docs
```

---

## Documentation

| Topic | Document |
|-------|----------|
| Installation | [docs/INSTALLATION.md](docs/INSTALLATION.md) |
| Configuration | [docs/CONFIGURATION.md](docs/CONFIGURATION.md) |
| Development & building from source | [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) |
| Export / import format | [docs/EXPORT_IMPORT.md](docs/EXPORT_IMPORT.md) |
| Packaging (deb/rpm, extension) | [docs/PACKAGING.md](docs/PACKAGING.md) |
| PostgreSQL extension design | [docs/PGRX_EXTENSION_DESIGN.md](docs/PGRX_EXTENSION_DESIGN.md) |
| SQL views reference | [docs/VIEWS_REFERENCE.md](docs/VIEWS_REFERENCE.md) |
| Understanding plan costs | [POSTGRESQL_COST_EXPLANATION.md](POSTGRESQL_COST_EXPLANATION.md) |

---

## Building & testing

```bash
cargo build --workspace
cargo test  --workspace
cargo fmt --all
cargo clippy --workspace --all-features --all-targets -- -D warnings
```

Integration tests that spin up real PostgreSQL containers are gated behind
`#[ignore]` (they require Docker); run them with `cargo test -- --ignored`.

### Native-CPU builds (opt-in)

Default builds target generic x86-64 for portability. On the machine that will
run the binary, enable the host's ISA extensions (AVX2/AVX-512/BMI2) with the
provided alias or the equivalent manual form:

```bash
cargo build-native                                    # alias for -C target-cpu=native
RUSTFLAGS="-C target-cpu=native" cargo build --release
```

For binaries that are distributable across modern x86-64 machines, use
`-C target-cpu=x86-64-v3` instead. Note that changing `RUSTFLAGS` invalidates
the build cache (full rebuild), and an ambient `RUSTFLAGS` overrides the alias.

Measure before adopting: on the parse benchmark in `crates/core/benches` run on
an AVX-512 Xeon, both `native` and `x86-64-v3` builds were slightly *slower*
than the default generic build — the hot regex/memchr paths already select
AVX2 code at runtime, so wider vector codegen bought nothing. Use
`cargo bench-native` to check whether your host behaves differently.

---

## Contributing

Issues and pull requests are welcome. Please run `cargo fmt` and the `clippy`
command above before submitting; CI enforces both. See
[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) to get started.

## License

Licensed under the [MIT License](LICENSE). © 2025 Ege Apak.

## Memory sizing

Plansight streams log input and folds each plan into its query group as it is
parsed, keeping only the group's representative. Peak memory is therefore driven
by the number of *distinct query shapes* in the log rather than by the number of
executions times the size of a plan (~4.4 KB each).

Measured with `cargo run --release --example mem_pipeline` on a 34 MiB synthetic
log of 50,000 plans:

| distinct shapes | peak memory | vs. log bytes |
|---|---|---|
| 20 | 2.7 MiB | 0.08x |
| 500 | 7.6 MiB | 0.22x |
| 50,000 (every query unique) | 484 MiB | 14x |

The last row is the shape to watch. One representative plan is retained per
distinct fingerprint, so a log in which nothing groups cannot be compressed —
memory grows with the log. That normally means normalization is not collapsing
what it should: statements `sqlparser` cannot parse fall back to grouping by
exact text. A warning is logged once a run retains 50,000 distinct fingerprints
(roughly 370 MB of representatives).

Executions themselves still cost memory, at 24 bytes each — but the vector
holding them grows by doubling and the statistics pass transiently copies the
durations, so budget about 1.7x that: ~40 bytes per execution, or ~4 GB at 100
million executions.

`--since`/`--until` bound memory as well as results, because the window is
applied while folding rather than after every plan is already resident:

```bash
pg-plansight --since 2h /var/log/postgresql/postgresql.log
```

The exporter reads incrementally and additionally caps each cycle with
`log_parsing.max_read_bytes_per_cycle` (64 MiB by default).
