# pg_loganalyze (PostgreSQL extension)

A PostgreSQL extension that captures cumulative `auto_explain` query statistics
and exposes them via SQL, analogous to `pg_stat_statements`. It reuses the
`pg-loganalyze-core` parser/normalizer as an embedded, single-threaded, no-IO
library.

> **Status: Phase 1 (manual ingest).** The SQL surface and cumulative-stats
> storage are complete and tested. Automatic in-process capture (an
> `ExecutorEnd` hook plus a background-worker flush) is planned for Phase 2 —
> see `docs/PGRX_EXTENSION_DESIGN.md`.

## What it does

- `loganalyze_ingest(text) -> bigint` — parse a chunk of `auto_explain` log
  output, group executions by normalized-query fingerprint, and fold the
  per-group aggregates into the cumulative `loganalyze.statements` table.
  Returns the number of distinct query groups written.
- `loganalyze_reset()` — discard all accumulated statistics.
- `loganalyze.statements` — raw cumulative counters (mergeable aggregates:
  `calls`, `total_time_ms`, `sum_sq_time_ms`, `min/max_time_ms`,
  `first/last_seen`).
- `loganalyze.statements_summary` — derives `mean_time_ms` and population
  `stddev_time_ms` from the stored aggregates.
- `loganalyze.top_by_total_time` — the summary ordered by cumulative time.

## Why it is a separate workspace

The crate requires the `pgrx` toolchain and a `pgNN` feature to compile, so it
is `exclude`d from the parent Cargo workspace. Plain `cargo build/clippy
--workspace` (and the main CI gate) never touch it; build it with `cargo pgrx`.

## Building & testing

Prerequisites: `cargo install cargo-pgrx --locked` and `cargo pgrx init`
(or point pgrx at an existing install, e.g.
`cargo pgrx init --pg16 $(which pg_config)` plus the matching
`postgresql-server-dev-NN` headers).

```bash
cd crates/pg_extension

# Compile against a specific major:
cargo build --no-default-features --features pg16

# Run the in-database test suite (must NOT run as root — Postgres refuses
# initdb as root):
cargo pgrx test pg16

# Install into a local cluster and try it:
cargo pgrx install --no-default-features --features pg16 -c $(which pg_config)
```

```sql
CREATE EXTENSION pg_loganalyze;
SELECT loganalyze_ingest($$<paste auto_explain log lines>$$);
SELECT * FROM loganalyze.statements_summary ORDER BY total_time_ms DESC;
```
