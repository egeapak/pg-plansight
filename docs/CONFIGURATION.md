# Configuration reference (pg_plansight extension)

Every knob is a `plansight.*` GUC. Most are **`Suset`** (superuser-settable per
session — `SET plansight.x = …`) and reloadable on `SIGHUP`; a few that the
background worker reads once are **`Sighup`**. Defaults preserve current
behavior, so an upgrade changes nothing until you opt in.

For the measured cost of each knob see [`PGRX_BENCHMARKS.md`](PGRX_BENCHMARKS.md);
for the SQL tables/views/columns these feed (with example rows) see
[`VIEWS_REFERENCE.md`](VIEWS_REFERENCE.md).

## Capture mode

`plansight.capture_mode` selects the source feeding the cumulative tables
(mutually exclusive, so one execution is never double-counted):

| value | meaning |
|-------|---------|
| `off` (default) | no automatic capture; manual `plansight_ingest()` still works |
| `log` | tail an `auto_explain` text log (`plansight.log_path`); cost is auto_explain's, in a separate process |
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
| `slo_threshold_ms` | float ≥ 0 | `0` | Suset | count executions slower than this as SLO breaches (`statements.slo_breaches`); `0` disables |
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
| `profile` | bool | `off` | Suset | accumulate per-phase hot-path timings for `plansight_capture_timings()` |

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
plansight.capture_mode = 'hook'
plansight.capture_plan = off            # stats-only: ~0% OLAP, +0.9µs/point query

# Analytical workload — keep plans, shed the dominant per-node timing cost
plansight.capture_mode = 'hook'
plansight.track_timing = off            # OLAP overhead +39% -> +12%; keeps row counts

# High volume — capture a slice, but never miss a query shape
plansight.capture_mode = 'hook'
plansight.sample_rate  = 0.1
plansight.sample_by    = 'query_id'     # rare shapes still guaranteed a capture

# Lowest-overhead full plan capture — trim render-only extras
plansight.track_settings = off          # ~3.6 µs/render
plansight.track_costs    = off
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

## Diagnostics & self-monitoring

After configuring, run the **doctor** to catch inconsistent settings:

```sql
SELECT * FROM plansight_check();
```

It returns `(severity, category, message)` rows — `error` (capture won't work),
`warning` (works, but probably not as intended), `info`, or a single `ok` row
when nothing is found. Examples it catches:

- `capture_mode` set but `pg_plansight` isn't in `shared_preload_libraries`
  (worker + hooks not running) — **error**.
- `log` mode without `auto_explain` loaded, or with `log_min_duration=-1`,
  `log_analyze=off`, or `log_format != text` — **error/warning**.
- the extension installed in a different database than `plansight.database`
  (captures won't be persisted) — **warning**.
- `synchronous=on` (hot-path UPSERT) or `sample_rate=0` (captures nothing) —
  **warning**; `sample_by=query_id` with no core queryId — **info**.
- the capture ring has overflowed (lifetime) — **warning**.

To see how much latency capture actually adds, read the overhead columns of
`plansight_capture_stats()`:

```sql
SELECT overhead_calls, overhead_mean_us, overhead_min_us, overhead_max_us,
       overhead_stddev_us
FROM   plansight_capture_stats();
```

These measure the microseconds spent in the `ExecutorEnd` capture body (render +
ring push / sync persist) — *not* the per-node execution instrumentation (that's
the `track_timing` cost). `SELECT plansight_reset_stats();` zeroes them (separate
from `plansight_reset()`, which clears the stored `plansight.statements` data).
