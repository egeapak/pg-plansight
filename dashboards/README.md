# Grafana dashboards

Three dashboards for the metrics `pg-plansight-exporter` publishes. Every metric
they use is documented in [`docs/METRICS.md`](../docs/METRICS.md).

| File | Dashboard | Answers |
|---|---|---|
| `query-performance.json` | Plansight — Query performance | Which statement should I fix first, and is it getting worse? |
| `plan-shapes.json` | Plansight — Plan shapes | Is PostgreSQL choosing good plans across the fleet? |
| `exporter-health.json` | Plansight — Exporter health | Should I trust the other two dashboards? |

## Import

Grafana UI: **Dashboards → New → Import → Upload JSON file**, then pick your
Prometheus data source when prompted.

Provisioned (no clicking, survives restarts):

```yaml
# /etc/grafana/provisioning/dashboards/plansight.yml
apiVersion: 1
providers:
  - name: plansight
    type: file
    allowUiUpdates: true
    options:
      path: /var/lib/grafana/dashboards/plansight
```

```bash
sudo mkdir -p /var/lib/grafana/dashboards/plansight
sudo cp dashboards/*.json /var/lib/grafana/dashboards/plansight/
sudo systemctl restart grafana-server
```

Or via the HTTP API:

```bash
for f in dashboards/*.json; do
  jq '{dashboard: (. | del(.id)), overwrite: true}' "$f" \
    | curl -sS -X POST -H 'Content-Type: application/json' \
        -H "Authorization: Bearer $GRAFANA_TOKEN" \
        --data @- "$GRAFANA_URL/api/dashboards/db"
done
```

Each dashboard declares a `datasource` variable, so nothing needs editing to
point them at your Prometheus.

## Variables

| Variable | Default | Notes |
|---|---|---|
| `datasource` | first Prometheus | Data source selector. |
| `db` | All | Filters on the exporter's `database` label. |
| `topk` | 10 | Bounds the per-query panels. Per-query series are the high-cardinality ones. |

## They are generated, not hand-written

Do not edit the JSON by hand — regenerate it:

```bash
python3 scripts/gen-grafana-dashboards.py            # write dashboards/*.json
python3 scripts/gen-grafana-dashboards.py --check    # fail if the JSON is stale
```

A renamed metric or a new panel is a few lines in the generator instead of a
hunt through thousands of lines of JSON, and `--check` keeps the committed JSON
honest in CI.

Panels tweaked in the Grafana UI are the exception worth knowing about: export
the JSON, then port the change back into the generator, or the next run will
overwrite it.

## Caveat on the query-level panels

Nine metric families carry a `normalized_query_hash` label and are subject to
`metrics.max_query_cardinality` (default 10000). When a hash is evicted its
series vanish from `/metrics`, which in Grafana looks identical to a query that
stopped running. The "Minutes since a fingerprint was last seen" panel exists to
tell the two apart. See the cardinality section of
[`docs/METRICS.md`](../docs/METRICS.md).
