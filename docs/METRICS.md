# Exported metrics

Every metric the `pg-plansight-exporter` daemon publishes, as of 0.2.0. The
names below assume the default `metrics.namespace = "pg_plansight"`; change that
in `config.toml` and every name changes with it.

Scrape it at `http://<host>:<port><metrics_path>` — `127.0.0.1:9090/metrics` by
default. The same families are exported over OTLP when
`metrics.backends` includes `"opentelemetry"`.

Ready-made Grafana dashboards for these metrics live in
[`dashboards/`](../dashboards/README.md).

## A note on cardinality before you build anything

Ten families carry a `normalized_query_hash` label — one series per distinct
query *shape*, not per execution. That is the useful axis and also the dangerous
one:

- `metrics.max_query_cardinality` (default `10000`) caps how many hashes stay
  live. The exporter evicts the least-recently-used hash and removes its series
  from `/metrics`, because Prometheus client libraries never evict label values
  on their own.
- A hash disappearing from `/metrics` is therefore ambiguous: the query may have
  stopped running, or it may have been evicted. `pg_plansight_query_last_seen_seconds`
  distinguishes the two while the series is still live.
- If distinct shapes grow without bound, normalization is not collapsing what it
  should — statements `sqlparser` cannot parse fall back to grouping by exact
  text. Watch `pg_plansight_database_unique_queries_total`.

The `log_path_pattern` label is deliberately the configured glob, not the
concrete filename: a rotating `log_filename` would otherwise mint a new label
value on every rotation.

## Per-query metrics

| Metric | Type | Labels | What it is for |
|---|---|---|---|
| `pg_plansight_query_duration_seconds` | histogram | `normalized_query_hash`, `database` | Raw execution-duration distribution. Buckets come from `metrics.histogram_buckets`. Use for heatmaps and `histogram_quantile`. |
| `pg_plansight_query_executions_total` | counter | `normalized_query_hash`, `database`, `status` | Call volume. `status` is `success` or `error`. |
| `pg_plansight_query_latency_p95_ms` | gauge | `normalized_query_hash`, `database` | p95 computed by the exporter from the group's own samples — not a histogram approximation. |
| `pg_plansight_query_latency_p99_ms` | gauge | `normalized_query_hash`, `database` | As above, p99. |
| `pg_plansight_query_latency_cv` | gauge | `normalized_query_hash`, `database` | Coefficient of variation (stddev/mean). Above ~1.0 the query is bimodal — usually a plan that is sometimes an index scan and sometimes a seq scan. |
| `pg_plansight_query_total_time_share_pct` | gauge | `normalized_query_hash`, `database` | Percent of total DB time across the exported set. **The ranking that decides what to optimise first.** |
| `pg_plansight_query_plan_cost` | histogram | `normalized_query_hash`, `database` | The planner's estimated cost. Meaningful against measured latency, not alone. |
| `pg_plansight_query_rows_examined` | histogram | `normalized_query_hash`, `database` | Rows the plan touched. Rising while latency is flat is a query living on borrowed time. |
| `pg_plansight_query_first_seen_seconds` | gauge | `normalized_query_hash`, `database` | Unix epoch seconds the fingerprint was first seen. A cluster of new hashes is usually a deploy. |
| `pg_plansight_query_last_seen_seconds` | gauge | `normalized_query_hash`, `database` | Unix epoch seconds last seen. `time() - this` is staleness. |
| `pg_plansight_query_info` | gauge | `normalized_query_hash`, `database`, `query_shape` | Always `1`. Carries the readable query shape for a hash. See below. |

## Reading a hash: `pg_plansight_query_info`

A `normalized_query_hash` on its own tells you nothing. `pg_plansight_query_info`
is the lookup table from hash to query shape, published as the standard
Prometheus *info metric*: the value is always `1` and the meaning lives in the
labels, so the text is stored once instead of on all ten per-query series.

Join it onto any per-query metric with `group_left`:

```promql
topk(10,
  pg_plansight_query_total_time_share_pct
    * on (normalized_query_hash, database) group_left(query_shape)
      pg_plansight_query_info
)
```

Join on **both** `normalized_query_hash` and `database`. The same query shape
can run in two databases, and on `normalized_query_hash` alone the right side
then holds two series for one hash, which fails with `multiple matches for
labels: many-to-one matching must be unique on the right side`.

The result keeps the original value and gains a `query_shape` label, so a table
panel can show the statement instead of the hash. Two rules govern what the
label contains:

- **Only normalised text is published.** `query_shape` holds the query with its
  parameters replaced by placeholders (`WHERE id = $1`). When `sqlparser` cannot
  parse a statement, the "normalised" text is still the raw statement with every
  literal in it, so the shape becomes `<unparsed>` instead. A metrics store never
  forgets, and an email address or a token in a label value cannot be taken
  back. Set `metrics.export_query_shape = false` to publish no query text at
  all.
- **The text is bounded.** Whitespace is collapsed to single spaces, and the
  value is truncated to `metrics.max_query_shape_length` characters (default
  `200`) with a trailing `…`. A label value rides on every scrape. The hash
  stays the join key for the full text, which the TUI and the JSON export hold.

The series is evicted with the rest of a hash's series when
`metrics.max_query_cardinality` is exceeded.

## Plan-shape metrics

Aggregated across queries, so these are low-cardinality and safe to alert on.

| Metric | Type | Labels | What it is for |
|---|---|---|---|
| `pg_plansight_query_scan_types_total` | counter | `scan_type`, `database` | `Seq Scan`, `Index Scan`, `Index Only Scan`, `Bitmap Heap Scan`, … A rising Seq Scan share against flat Index Scan is how a missing index announces itself. |
| `pg_plansight_query_join_types_total` | counter | `join_type`, `database` | `Nested Loop`, `Hash Join`, `Merge Join`. A jump in Nested Loop alongside rising rows-examined is the classic bad-row-estimate signature. |
| `pg_plansight_query_plan_node_types_total` | counter | `node_type`, `database` | Every plan node type seen. Mostly a shape check. |
| `pg_plansight_slow_queries_total` | counter | `database`, `threshold` | One counter per entry in `metrics.slow_query_thresholds` (default `1s`, `5s`, `10s`, `30s`). Thresholds are nested, so the `1s` series always sits above `5s`. |

## Per-database metrics

| Metric | Type | Labels | What it is for |
|---|---|---|---|
| `pg_plansight_database_avg_query_duration_seconds` | histogram | `database` | Per-database central tendency. Deliberately blunt — a wall number, not a debugging one. |
| `pg_plansight_database_queries_per_second` | histogram | `database` | Observed query rate per database. |
| `pg_plansight_database_unique_queries_total` | counter | `database` | Distinct fingerprints seen. Flat is healthy; a steady climb means normalization is failing to collapse shapes. |

## Exporter health

These describe the collection pipeline, not PostgreSQL. They answer "should I
trust the panels above".

| Metric | Type | Labels | What it is for |
|---|---|---|---|
| `pg_plansight_exporter_up` | gauge | — | `1` when the last collection cycle finished with no per-file errors. **Not liveness** — if you can scrape it the process is alive, so use Prometheus's own `up{job=...}` for that. |
| `pg_plansight_last_successful_parse_timestamp` | gauge | — | Unix seconds of the last successful parse. `time() - this` is the staleness to alert on: it rises whether the daemon is stuck, the log stopped being written, or permissions changed. |
| `pg_plansight_logs_parsed_total` | counter | `log_path_pattern`, `status` | Ingestion throughput. |
| `pg_plansight_parse_errors_total` | counter | `log_path_pattern`, `error_type` | Rejected entries. A trickle is normal; a step change means the log format moved — a PostgreSQL upgrade, or `auto_explain.log_format` switched between `text` and `json`. |
| `pg_plansight_export_duration_seconds` | histogram | `operation` | Time per phase of a cycle. If parse duration approaches the poll interval the exporter is about to fall behind. |
| `pg_plansight_memory_usage_bytes` | gauge | — | Resident memory. Driven by distinct query shapes retained, not by log volume. |

## Suggested alerts

Derived from the semantics above rather than from round numbers — tune the
thresholds to your workload.

```promql
# The exporter is up but no longer keeping up. The single most useful alert here.
time() - pg_plansight_last_successful_parse_timestamp > 600

# Collection is completing with per-file errors.
pg_plansight_exporter_up == 0

# The log format changed under us, or the log is being truncated mid-entry.
sum(rate(pg_plansight_parse_errors_total[15m])) > 1

# A query shape became bimodal: a fast plan exists and is not always chosen.
pg_plansight_query_latency_cv > 1.5

# Sequential scans took over. Compare against your own baseline, not this number.
100 * sum(rate(pg_plansight_query_scan_types_total{scan_type="Seq Scan"}[30m]))
    / sum(rate(pg_plansight_query_scan_types_total[30m])) > 30

# Fingerprint growth: normalization is not collapsing shapes, and both exporter
# memory and Prometheus series are growing with it.
delta(pg_plansight_database_unique_queries_total[6h]) > 500
```

## The extension exports nothing to Prometheus

`pg_plansight`, the pgrx extension, is a separate surface: it accumulates the
same statistics *inside* PostgreSQL and exposes them as SQL views
(`plansight.statements_summary` and friends — see
[VIEWS_REFERENCE.md](VIEWS_REFERENCE.md)). Nothing above comes from it. To get
extension data into Prometheus, scrape those views with a generic SQL exporter,
or run the daemon alongside.
