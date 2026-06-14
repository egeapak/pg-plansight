# Configuration reference (pg_loganalyze extension)

Every knob is a `loganalyze.*` GUC. Most are **`Suset`** (superuser-settable per
session — `SET loganalyze.x = …`) and reloadable on `SIGHUP`; a few that the
background worker reads once are **`Sighup`**. Defaults preserve current
behavior, so an upgrade changes nothing until you opt in.

For the measured cost of each knob see [`PGRX_BENCHMARKS.md`](PGRX_BENCHMARKS.md);
for the SQL views/functions they feed, see the extension
[`README`](../crates/pg_extension/README.md).

## Capture mode

`loganalyze.capture_mode` selects the source feeding the cumulative tables
(mutually exclusive, so one execution is never double-counted):

| value | meaning |
|-------|---------|
| `off` (default) | no automatic capture; manual `loganalyze_ingest()` still works |
| `log` | tail an `auto_explain` text log (`loganalyze.log_path`); cost is auto_explain's, in a separate process |
| `hook` | in-process executor hooks render the plan into a shared-memory ring, drained by the worker off the hot path |

The knobs below apply to **`hook`** mode.

## All GUCs

| GUC | Type / range | Default | Ctx | Purpose |
|-----|--------------|---------|-----|---------|
| `capture_mode` | `off`/`log`/`hook` | `off` | Suset | capture source (above) |
| `database` | string | `postgres` | Sighup | DB the worker writes to (must have the extension); read once at worker start |
| `log_path` | string | — | Sighup | auto_explain log file to tail (`log` mode) |
| `flush_interval` | int 1–3600 s | `10` | Sighup | seconds between worker drains |
| `min_duration_ms` | float ≥ 0 | `0` | Suset | skip capturing executions faster than this |
| `sample_rate` | float 0.0–1.0 | `1.0` | Suset | fraction of executions captured; **decided before instrumentation, so unsampled queries pay ~nothing** |
| `sample_by` | `random`/`query_id` | `random` | Suset | sampling strategy (see below) |
| `capture_plan` | bool | `on` | Suset | render & store the plan; **off = stats-only** (numbers, no plan) |
| `track_timing` | bool | `on` | Suset | per-node `EXPLAIN ANALYZE` timing; **off drops the per-node `gettimeofday` loop** (most OLAP overhead), keeps row counts |
| `track_io` | bool | `on` | Suset | per-node `Buffers:`/`WAL:` (feeds the buffer/WAL analyzer) |
| `track_settings` | bool | `on` | Suset | `EXPLAIN SETTINGS` (non-default planner GUCs); a ~3.6 µs/render GUC scan |
| `track_costs` | bool | `on` | Suset | estimated cost columns (render-only) |
| `track_verbose` | bool | `off` | Suset | `EXPLAIN VERBOSE` output columns / qualified names (render-only) |
| `track_nested` | bool | `off` | Suset | also capture queries nested in functions/triggers (default: top-level only, like pg_stat_statements) |
| `synchronous` | bool | `off` | Suset | UPSERT inline instead of via the ring — deterministic; **tests/debug only**, heavy on the hot path |
| `profile` | bool | `off` | Suset | accumulate per-phase hot-path timings for `loganalyze_capture_timings()` |

## Sampling: `sample_rate` × `sample_by`

`sample_rate` sets *how much* you capture; `sample_by` sets *which*:

- **`random`** (default) — each execution is an independent draw at `sample_rate`.
  Captures every query shape in proportion to how often it runs.
- **`query_id`** — *stratified*: the **first** execution of each `queryId` a
  backend sees is always captured, and the rest are sampled at `sample_rate`. A
  rarely-run query is therefore never starved by a very frequent one. Falls back
  to `random` when the queryId is unavailable (PG13, or `compute_query_id` off).

## Tuning recipes

```ini
# OLTP / high-QPS monitoring — numbers only, ~free (no render, no per-node timers)
loganalyze.capture_mode = 'hook'
loganalyze.capture_plan = off            # stats-only: ~0% OLAP, +0.9µs/point query

# Analytical workload — keep plans, shed the dominant per-node timing cost
loganalyze.capture_mode = 'hook'
loganalyze.track_timing = off            # OLAP overhead +39% -> +12%; keeps row counts

# High volume — capture a slice, but never miss a query shape
loganalyze.capture_mode = 'hook'
loganalyze.sample_rate  = 0.1
loganalyze.sample_by    = 'query_id'     # rare shapes still guaranteed a capture

# Lowest-overhead full plan capture — trim render-only extras
loganalyze.track_settings = off          # ~3.6 µs/render
loganalyze.track_costs    = off
```

## New in this release

- **`capture_plan = off` (stats-only)** — skips the EXPLAIN render *and* all
  per-node instrumentation; records `calls`/timing with an empty plan and no plan
  analysis. The cheapest capture; ≈ baseline for OLAP. Rows still group/fingerprint
  by query text, so they merge with rendered captures of the same query.
- **`track_timing` / `track_costs` / `track_verbose`** — per-field EXPLAIN
  toggles. `track_timing=off` is the big lever for analytical workloads (drops the
  per-node `gettimeofday` loop while keeping actual row counts).
- **`sample_by = query_id`** — stratified sampling that guarantees a capture of
  every distinct query shape (above).
