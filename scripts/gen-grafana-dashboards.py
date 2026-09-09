#!/usr/bin/env python3
"""Generate the Grafana dashboards for the pg-plansight Prometheus exporter.

Writes dashboards/*.json. Run it after changing a metric name, a label, or a
panel: the JSON is generated, not hand-maintained, so a renamed metric is a
one-line edit here rather than a hunt through thousands of lines of JSON.

    python3 scripts/gen-grafana-dashboards.py           # write dashboards/
    python3 scripts/gen-grafana-dashboards.py --check   # fail if stale (CI)

Every dashboard carries a `datasource` variable, so importing it against any
Prometheus works without editing. `$db` filters by the exporter's `database`
label and `$topk` bounds the per-query panels -- per-query series are the
high-cardinality ones, and `max_query_cardinality` in the exporter config caps
how many exist at all.
"""
import argparse, json, pathlib, sys

NS = "pg_plansight"
DS = {"type": "prometheus", "uid": "${datasource}"}

# Shared y-axis/legend scaffolding. Kept in one place so the panels stay
# visually consistent; a panel only states what makes it different.
def field_config(unit=None, decimals=None, min_=None, max_=None, custom=None, thresholds=None):
    cfg = {
        "color": {"mode": "palette-classic"},
        "custom": {
            "axisBorderShow": False, "axisCenteredZero": False, "axisLabel": "",
            "axisPlacement": "auto", "barAlignment": 0, "drawStyle": "line",
            "fillOpacity": 12, "gradientMode": "none",
            "hideFrom": {"legend": False, "tooltip": False, "viz": False},
            "insertNulls": False, "lineInterpolation": "smooth", "lineWidth": 2,
            "pointSize": 5, "scaleDistribution": {"type": "linear"},
            "showPoints": "never", "spanNulls": True,
            "stacking": {"group": "A", "mode": "none"},
            "thresholdsStyle": {"mode": "off"},
        },
        "mappings": [],
        "thresholds": thresholds or {"mode": "absolute", "steps": [{"color": "green", "value": None}]},
    }
    if custom:
        cfg["custom"].update(custom)
    if unit:
        cfg["unit"] = unit
    if decimals is not None:
        cfg["decimals"] = decimals
    if min_ is not None:
        cfg["min"] = min_
    if max_ is not None:
        cfg["max"] = max_
    return cfg


def legend(calcs=("lastNotNull", "max"), placement="right"):
    return {"calcs": list(calcs), "displayMode": "table",
            "placement": placement, "showLegend": True}


def target(expr, legend_format=None, ref="A", instant=False, fmt=None):
    t = {"datasource": DS, "editorMode": "code", "expr": expr,
         "range": not instant, "instant": instant, "refId": ref}
    if legend_format:
        t["legendFormat"] = legend_format
    if fmt:
        t["format"] = fmt
    return t


def panel(pid, title, ptype, targets, description="", w=12, h=8, x=0, y=0,
          fc=None, options=None, transformations=None, overrides=None):
    # `fc` may be either a bare defaults dict (the common case, from
    # field_config()) or a complete fieldConfig with its own "defaults"/
    # "overrides". Accepting both silently double-nested the latter under
    # fieldConfig.defaults.defaults, where Grafana ignores it -- which is how
    # the first-seen table lost its date formatting.
    if fc is not None and set(fc.keys()) <= {"defaults", "overrides"} and "defaults" in fc:
        field_cfg = {"defaults": fc["defaults"], "overrides": fc.get("overrides", [])}
    else:
        field_cfg = {"defaults": fc or field_config(), "overrides": []}
    if overrides:
        field_cfg["overrides"] = field_cfg["overrides"] + overrides
    p = {
        "id": pid, "title": title, "type": ptype, "description": description,
        "datasource": DS, "gridPos": {"h": h, "w": w, "x": x, "y": y},
        "targets": targets,
        "fieldConfig": field_cfg,
        "options": options or {"legend": legend(), "tooltip": {"mode": "multi", "sort": "desc"}},
    }
    if transformations:
        p["transformations"] = transformations
    return p


def row(pid, title, y):
    return {"id": pid, "title": title, "type": "row", "collapsed": False,
            "gridPos": {"h": 1, "w": 24, "x": 0, "y": y}, "panels": []}


def dashboard(uid, title, description, panels, tags, refresh="1m"):
    return {
        "uid": uid, "title": title, "description": description,
        "tags": tags, "timezone": "browser", "editable": True,
        "graphTooltip": 1, "schemaVersion": 41, "version": 1,
        "refresh": refresh,
        "time": {"from": "now-6h", "to": "now"},
        "templating": {"list": [
            {"name": "datasource", "label": "Prometheus", "type": "datasource",
             "query": "prometheus", "current": {}, "hide": 0, "refresh": 1,
             "regex": "", "skipUrlSync": False},
            {"name": "db", "label": "Database", "type": "query", "datasource": DS,
             "definition": f'label_values({NS}_query_executions_total, database)',
             "query": {"query": f'label_values({NS}_query_executions_total, database)',
                       "refId": "db"},
             "current": {"text": "All", "value": "$__all"},
             "includeAll": True, "multi": True, "allValue": ".*",
             "refresh": 2, "sort": 1, "hide": 0, "skipUrlSync": False, "options": []},
            {"name": "topk", "label": "Top N queries", "type": "custom",
             "query": "5,10,15,20,25", "current": {"text": "10", "value": "10"},
             "options": [{"text": n, "value": n, "selected": n == "10"}
                         for n in ("5", "10", "15", "20", "25")],
             "includeAll": False, "multi": False, "hide": 0, "skipUrlSync": False},
        ]},
        "annotations": {"list": [{
            "name": "Exporter degraded",
            "datasource": DS,
            "enable": True, "hide": False, "iconColor": "red",
            "target": {"expr": f"{NS}_exporter_up == 0", "refId": "Anno"},
            "titleFormat": "Collection cycle reported errors",
        }]},
    } | {"panels": panels}


Q = f'{{database=~"$db"}}'          # label filter used by most per-query panels

# --------------------------------------------------------------------------
# Dashboard 1: query performance. The "which query should I fix" dashboard.
# --------------------------------------------------------------------------
def queries_dashboard():
    p, y = [], 0
    p.append(row(100, "Where the database time goes", y)); y += 1

    p.append(panel(
        1, "Share of total DB time by query", "bargauge",
        [target(f'topk($topk, {NS}_query_total_time_share_pct{Q})',
                "{{normalized_query_hash}}", instant=True)],
        "The ranking that decides what to optimise first. A query can be slow and "
        "irrelevant (runs twice a day) or fast and dominant (runs ten thousand times "
        "a minute) — this is total time, so it answers the second case too. "
        "Derived by the exporter from each group's total duration over the exported set.",
        w=12, h=9, x=0, y=y,
        fc=field_config(unit="percent", decimals=2, min_=0,
                        thresholds={"mode": "absolute", "steps": [
                            {"color": "green", "value": None},
                            {"color": "orange", "value": 15},
                            {"color": "red", "value": 25}]}),
        options={"displayMode": "gradient", "orientation": "horizontal",
                 "reduceOptions": {"calcs": ["lastNotNull"], "fields": "", "values": False},
                 "showUnfilled": True, "valueMode": "color", "namePlacement": "left",
                 "minVizHeight": 16, "minVizWidth": 8, "maxVizHeight": 300, "sizing": "auto",
                 "legend": {"showLegend": False, "displayMode": "list",
                            "placement": "bottom", "calcs": []}}))

    p.append(panel(
        2, "p95 latency by query", "timeseries",
        [target(f'topk($topk, {NS}_query_latency_p95_ms{Q})',
                "p95 {{normalized_query_hash}}")],
        "Tail latency per fingerprint, computed by the exporter from the group's own "
        "execution samples. Watch for step changes: a plan flip shows up here as a "
        "discontinuity, not a slope.",
        w=12, h=9, x=12, y=y,
        fc=field_config(unit="ms", custom={"fillOpacity": 8})))
    y += 9

    p.append(panel(
        3, "p99 latency by query", "timeseries",
        [target(f'topk($topk, {NS}_query_latency_p99_ms{Q})',
                "p99 {{normalized_query_hash}}")],
        "The same series at p99. Compare against p95: a widening gap means the "
        "slow path is getting slower, not that everything is.",
        w=12, h=8, x=0, y=y, fc=field_config(unit="ms")))

    p.append(panel(
        4, "Latency stability (coefficient of variation)", "timeseries",
        [target(f'topk($topk, {NS}_query_latency_cv{Q})', "CV {{normalized_query_hash}}")],
        "stddev/mean. Above ~1.0 the query is bimodal — usually a plan that is "
        "sometimes an index scan and sometimes a seq scan, or a cache that sometimes "
        "misses. A high-CV query at modest mean latency is often a better lead than a "
        "uniformly slow one, because the fast path proves a fast plan exists.",
        w=12, h=8, x=12, y=y,
        fc=field_config(decimals=2, thresholds={"mode": "absolute", "steps": [
            {"color": "green", "value": None}, {"color": "orange", "value": 0.5},
            {"color": "red", "value": 1.0}]},
            custom={"thresholdsStyle": {"mode": "dashed"}})))
    y += 8

    p.append(row(104, "What each hash actually is", y)); y += 1

    p.append(panel(
        13, "Query shape by hash", "table",
        [target(f'topk($topk, {NS}_query_total_time_share_pct{Q} '
                f'* on (normalized_query_hash, database) group_left(query_shape) '
                f'{NS}_query_info{Q})',
                instant=True, fmt="table")],
        "Every other panel here labels its series with normalized_query_hash, which "
        "tells you nothing on its own. This is the lookup table: hash to query shape, "
        "joined from the pg_plansight_query_info info metric with group_left so the "
        "text is stored once rather than on all ten per-query series. Ordered by share "
        "of DB time, so the top row is the query to read first. "
        "The shape is the normalised statement -- parameters are placeholders "
        "($1, $2), never the original values. A statement the SQL parser could not "
        "parse shows as <unparsed> rather than risking a literal in a label, and the "
        "text is truncated at metrics.max_query_shape_length characters. "
        "Empty panel: metrics.export_query_shape is false.",
        # Tall enough that the default $topk of 10 rows all fit without the
        # table needing its own scrollbar.
        w=24, h=12, x=0, y=y,
        # No unit in defaults: it would also apply to the three string columns
        # and render them as NaN. It goes on the one numeric column, below.
        fc={"defaults": {"custom": {
            "align": "auto", "cellOptions": {"type": "auto"}, "inspect": False}},
            "overrides": []},
        options={"showHeader": True, "cellHeight": "sm",
                 "footer": {"show": False, "reducer": ["sum"], "countRows": False, "fields": ""},
                 "sortBy": [{"displayName": "% of DB time", "desc": True}]},
        overrides=[
            {"matcher": {"id": "byName", "options": "Query hash"},
             "properties": [{"id": "custom.width", "value": 180}]},
            {"matcher": {"id": "byName", "options": "Database"},
             "properties": [{"id": "custom.width", "value": 140}]},
            {"matcher": {"id": "byName", "options": "% of DB time"},
             "properties": [{"id": "unit", "value": "percent"},
                            {"id": "decimals", "value": 2},
                            {"id": "custom.width", "value": 130}]},
            # The shape can be 200 characters. Let a reader open the full value
            # instead of silently clipping it at the column edge.
            {"matcher": {"id": "byName", "options": "Query shape"},
             "properties": [{"id": "custom.inspect", "value": True}]},
        ],
        transformations=[{"id": "organize", "options": {
            "excludeByName": {"Time": True, "__name__": True, "job": True,
                              "instance": True},
            "indexByName": {"normalized_query_hash": 0, "query_shape": 1,
                            "database": 2, "Value": 3},
            "renameByName": {"normalized_query_hash": "Query hash",
                             "query_shape": "Query shape",
                             "database": "Database",
                             "Value": "% of DB time"}}}]))
    y += 12

    p.append(row(101, "Throughput and failures", y)); y += 1

    p.append(panel(
        5, "Executions per second by query", "timeseries",
        [target(f'topk($topk, sum by (normalized_query_hash) '
                f'(rate({NS}_query_executions_total{{database=~"$db",status="success"}}[5m])))',
                "{{normalized_query_hash}}")],
        "Call rate per fingerprint. Pair with the time-share panel: the product of "
        "rate and mean latency is what actually consumes the database.",
        w=12, h=8, x=0, y=y,
        fc=field_config(unit="reqps", custom={"fillOpacity": 20,
                                              "stacking": {"group": "A", "mode": "normal"}})))

    p.append(panel(
        6, "Failed executions per second", "timeseries",
        [target(f'sum by (normalized_query_hash) '
                f'(rate({NS}_query_executions_total{{database=~"$db",status="error"}}[5m]))',
                "{{normalized_query_hash}}")],
        "Statements that errored. auto_explain logs the plan for statements that "
        "completed, so a query appearing here alongside a healthy latency series "
        "usually means intermittent failure (lock timeout, constraint violation) "
        "rather than a slow plan.",
        w=12, h=8, x=12, y=y,
        fc=field_config(unit="reqps", custom={"fillOpacity": 25,
                                              "stacking": {"group": "A", "mode": "normal"}})))
    y += 8

    p.append(panel(
        7, "Slow-query events per minute by threshold", "timeseries",
        [target(f'sum by (threshold) (rate({NS}_slow_queries_total{Q}[5m])) * 60',
                "slower than {{threshold}}")],
        "One counter per configured threshold (metrics.slow_query_thresholds). "
        "The shape matters more than the value: thresholds are nested, so the 1s "
        "series always sits above the 5s series. Alert on the slowest bucket you "
        "consider abnormal rather than on mean latency.",
        w=24, h=8, x=0, y=y,
        fc=field_config(unit="cpm", custom={"fillOpacity": 18})))
    y += 8

    p.append(row(102, "Latency distribution", y)); y += 1

    p.append(panel(
        8, "Query duration distribution (all queries)", "heatmap",
        [target(f'sum by (le) (increase({NS}_query_duration_seconds_bucket{Q}[$__rate_interval]))',
                "{{le}}", fmt="heatmap")],
        "The raw histogram, as a heatmap over the exporter's configured buckets "
        "(metrics.histogram_buckets). Two bright bands means two populations — the "
        "aggregate average sits between them and describes neither.",
        w=24, h=9, x=0, y=y,
        fc={"defaults": {"custom": {"hideFrom": {"legend": False, "tooltip": False, "viz": False},
                                    "scaleDistribution": {"type": "linear"}}},
            "overrides": []},
        options={"calculate": False,
                 "cellGap": 1, "cellValues": {"unit": "short"},
                 "color": {"mode": "scheme", "scheme": "Turbo", "steps": 64,
                           "reverse": False, "exponent": 0.5, "fill": "dark-orange"},
                 "exemplars": {"color": "rgba(255,0,255,0.7)"},
                 "filterValues": {"le": 1e-9},
                 "legend": {"show": True},
                 "rowsFrame": {"layout": "auto", "value": "Executions"},
                 "tooltip": {"mode": "single", "showColorScale": True, "yHistogram": True},
                 "yAxis": {"axisPlacement": "left", "reverse": False, "unit": "s"}}))
    y += 9

    p.append(panel(
        9, "Rows examined per execution (p95)", "timeseries",
        [target(f'histogram_quantile(0.95, sum by (normalized_query_hash, le) '
                f'(rate({NS}_query_rows_examined_bucket{Q}[5m])))',
                "{{normalized_query_hash}}")],
        "How much data each statement touches. A query whose latency is flat while "
        "rows examined climbs is living on borrowed time — it is scanning a growing "
        "table and has not crossed the threshold where the planner or the buffer "
        "cache gives up.",
        w=12, h=8, x=0, y=y, fc=field_config(unit="short")))

    p.append(panel(
        10, "Planner cost estimate (p95)", "timeseries",
        [target(f'histogram_quantile(0.95, sum by (normalized_query_hash, le) '
                f'(rate({NS}_query_plan_cost_bucket{Q}[5m])))',
                "{{normalized_query_hash}}")],
        "The planner's own estimate. Useful against measured latency rather than on "
        "its own: cost rising while latency holds means the estimate drifted, which "
        "is how stale statistics announce themselves before they cause a plan flip.",
        w=12, h=8, x=12, y=y, fc=field_config(unit="short")))
    y += 8

    p.append(row(103, "Fingerprint lifecycle", y)); y += 1

    p.append(panel(
        11, "Recently first-seen query shapes", "table",
        [target(f'topk(20, {NS}_query_first_seen_seconds{Q} * 1000)',
                "{{normalized_query_hash}}", instant=True, fmt="table")],
        "New fingerprints, most recent first. A deploy that changes SQL shows up as a "
        "cluster of new hashes; one appearing on its own is usually an ORM building a "
        "query it has not built before. Values are epoch milliseconds.",
        w=12, h=8, x=0, y=y,
        # No unit in defaults: it would apply to the Database and Query hash
        # string columns too and render them as NaN. It belongs on the one
        # numeric column, via the override below.
        fc={"defaults": {"custom": {
            "align": "auto", "cellOptions": {"type": "auto"}, "inspect": False}},
            "overrides": []},
        options={"showHeader": True, "cellHeight": "sm",
                 "footer": {"show": False, "reducer": ["sum"], "countRows": False, "fields": ""},
                 "sortBy": [{"displayName": "Value", "desc": True}]},
        overrides=[{"matcher": {"id": "byName", "options": "First seen"},
                    "properties": [{"id": "unit", "value": "dateTimeAsIso"},
                                   {"id": "custom.width", "value": 210}]}],
        transformations=[{"id": "organize", "options": {
            "excludeByName": {"Time": True, "__name__": True, "job": True, "instance": True},
            "renameByName": {"normalized_query_hash": "Query hash",
                             "database": "Database", "Value": "First seen"}}}]))

    p.append(panel(
        12, "Minutes since a fingerprint was last seen", "timeseries",
        [target(f'(time() - {NS}_query_last_seen_seconds{Q}) / 60',
                "{{normalized_query_hash}}")],
        "Rising steadily for one series means that query shape has stopped arriving — "
        "either it genuinely stopped, or it fell out of the exporter's cardinality cap "
        "and is no longer exported. Check max_query_cardinality before concluding the "
        "former.",
        w=12, h=8, x=12, y=y, fc=field_config(unit="m")))
    return dashboard(
        "pgplansight-queries", "Plansight — Query performance",
        "Per-query view built from the pg-plansight exporter. Ordered the way you "
        "would actually work a slow database: what consumes the most time, then how "
        "stable it is, then what its plan is doing.",
        p, ["pg-plansight", "postgresql", "query-performance"])


# --------------------------------------------------------------------------
# Dashboard 2: plan shapes. "Is PostgreSQL choosing good plans?"
# --------------------------------------------------------------------------
def plans_dashboard():
    p, y = [], 0
    p.append(row(200, "Access paths", y)); y += 1

    p.append(panel(
        1, "Scan type mix", "timeseries",
        [target(f'sum by (scan_type) (rate({NS}_query_scan_types_total{Q}[5m])) * 60',
                "{{scan_type}}")],
        "Plan nodes by access path, per minute. The ratio is the signal: a rising "
        "Seq Scan share against flat Index Scan usually means a new query without an "
        "index, or a predicate the planner can no longer use. This is the panel that "
        "catches a missing index before anyone files a ticket.",
        w=16, h=9, x=0, y=y,
        fc=field_config(unit="cpm", custom={"fillOpacity": 35,
                                            "stacking": {"group": "A", "mode": "normal"},
                                            "lineWidth": 1})))

    p.append(panel(
        2, "Sequential scan share", "stat",
        [target(f'100 * sum(rate({NS}_query_scan_types_total{{database=~"$db",scan_type="Seq Scan"}}[30m])) '
                f'/ sum(rate({NS}_query_scan_types_total{Q}[30m]))', "Seq Scan share")],
        "Sequential scans as a percentage of all scan nodes over the last 30 minutes. "
        "There is no universally correct value — small tables are legitimately "
        "seq-scanned — so treat a sustained change as the finding, not the level.",
        w=8, h=9, x=16, y=y,
        fc=field_config(unit="percent", decimals=1, min_=0, max_=100,
                        thresholds={"mode": "absolute", "steps": [
                            {"color": "green", "value": None},
                            {"color": "orange", "value": 15},
                            {"color": "red", "value": 30}]}),
        options={"colorMode": "background", "graphMode": "area",
                 "justifyMode": "auto", "orientation": "auto",
                 "reduceOptions": {"calcs": ["lastNotNull"], "fields": "", "values": False},
                 "textMode": "auto", "wideLayout": True,
                 "percentChangeColorMode": "standard", "showPercentChange": True}))
    y += 9

    p.append(panel(
        3, "Join strategy mix", "timeseries",
        [target(f'sum by (join_type) (rate({NS}_query_join_types_total{Q}[5m])) * 60',
                "{{join_type}}")],
        "Nested Loop, Hash Join and Merge Join per minute. A Nested Loop is cheap on "
        "few rows and ruinous on many, so a jump in Nested Loop rate alongside rising "
        "rows-examined is the classic row-estimate-gone-wrong signature.",
        w=12, h=8, x=0, y=y,
        fc=field_config(unit="cpm", custom={"fillOpacity": 35,
                                            "stacking": {"group": "A", "mode": "normal"},
                                            "lineWidth": 1})))

    p.append(panel(
        4, "Plan node types", "piechart",
        [target(f'sum by (node_type) (increase({NS}_query_plan_node_types_total{Q}[$__range]))',
                "{{node_type}}", instant=True)],
        "Every node type the analyser saw over the dashboard window. Mostly a shape "
        "check: Sort and Aggregate appearing where you expect none means work is "
        "happening in the database that could happen in an index.",
        w=12, h=8, x=12, y=y,
        fc={"defaults": {"color": {"mode": "palette-classic"}, "mappings": [],
                         "custom": {"hideFrom": {"legend": False, "tooltip": False, "viz": False}}},
            "overrides": []},
        options={"displayLabels": ["percent"], "legend": legend(("value",), "right"),
                 "pieType": "donut", "tooltip": {"mode": "single", "sort": "none"},
                 "reduceOptions": {"calcs": ["lastNotNull"], "fields": "", "values": False}}))
    y += 8

    p.append(row(201, "Per-database totals", y)); y += 1

    p.append(panel(
        5, "Average query duration per database", "timeseries",
        [target(f'histogram_quantile(0.5, sum by (database, le) '
                f'(rate({NS}_database_avg_query_duration_seconds_bucket{Q}[10m])))',
                "{{database}} median")],
        "Per-database central tendency. Deliberately blunt — it is the number to put "
        "on a wall, not the one to debug with. Use the per-query dashboard for that.",
        w=12, h=8, x=0, y=y, fc=field_config(unit="s")))

    p.append(panel(
        7, "Query rate per database", "timeseries",
        [target(f'histogram_quantile(0.9, sum by (database, le) '
                f'(rate({NS}_database_queries_per_second_bucket{Q}[10m])))',
                "{{database}} p90")],
        "Observed statements per second, per database, as the exporter measured it. "
        "Read it against the per-query execution panel: if total rate is flat while "
        "one fingerprint's rate climbs, traffic shifted rather than grew.",
        w=24, h=7, x=0, y=y + 8, fc=field_config(unit="reqps")))

    p.append(panel(
        6, "Distinct query shapes per database", "timeseries",
        [target(f'{NS}_database_unique_queries_total{Q}', "{{database}}")],
        "Distinct fingerprints seen. Flat is healthy. A steady climb means "
        "normalization is not collapsing what it should — typically statements "
        "sqlparser cannot parse, which fall back to grouping by exact text and mint a "
        "fingerprint per literal. That grows exporter memory and Prometheus series "
        "together, and is what max_query_cardinality exists to contain.",
        w=12, h=8, x=12, y=y,
        fc=field_config(unit="short", custom={"fillOpacity": 10})))
    return dashboard(
        "pgplansight-plans", "Plansight — Plan shapes",
        "What PostgreSQL's planner is actually choosing, aggregated across queries. "
        "Use it to notice a plan regression across the fleet; use the query dashboard "
        "to find which statement caused it.",
        p, ["pg-plansight", "postgresql", "query-plans"])


# --------------------------------------------------------------------------
# Dashboard 3: exporter health. "Do I trust the two dashboards above?"
# --------------------------------------------------------------------------
def health_dashboard():
    p, y = [], 0
    p.append(row(300, "Is the pipeline keeping up?", y)); y += 1

    p.append(panel(
        1, "Collection cycle outcome", "stat",
        [target(f'{NS}_exporter_up', "exporter_up", instant=True)],
        "1 when the last collection cycle finished with no per-file errors. This is "
        "NOT liveness — if you can scrape it, the process is alive, so use "
        "Prometheus's own up{job=...} for that. This gauge answers a different "
        "question: did the most recent cycle actually work.",
        w=6, h=6, x=0, y=y,
        fc=field_config(decimals=0, thresholds={"mode": "absolute", "steps": [
            {"color": "red", "value": None}, {"color": "green", "value": 1}]}) |
           {"mappings": [{"type": "value", "options": {
               "0": {"text": "Degraded", "color": "red", "index": 0},
               "1": {"text": "Healthy", "color": "green", "index": 1}}}]},
        options={"colorMode": "background", "graphMode": "none", "justifyMode": "center",
                 "orientation": "auto", "textMode": "auto", "wideLayout": True,
                 "reduceOptions": {"calcs": ["lastNotNull"], "fields": "", "values": False}}))

    p.append(panel(
        2, "Parse staleness", "stat",
        [target(f'time() - {NS}_last_successful_parse_timestamp', "staleness", instant=True)],
        "Seconds since the last successful parse. This is the metric to alert on for "
        "'the exporter is up but no longer keeping up' — it rises whether the daemon "
        "is stuck, the log stopped being written, or permissions changed. Alert above "
        "a few poll intervals.",
        w=6, h=6, x=6, y=y,
        fc=field_config(unit="s", decimals=0, thresholds={"mode": "absolute", "steps": [
            {"color": "green", "value": None}, {"color": "orange", "value": 180},
            {"color": "red", "value": 600}]}),
        options={"colorMode": "background", "graphMode": "area", "justifyMode": "auto",
                 "orientation": "auto", "textMode": "auto", "wideLayout": True,
                 "reduceOptions": {"calcs": ["lastNotNull"], "fields": "", "values": False}}))

    p.append(panel(
        3, "Log entries parsed per second", "timeseries",
        [target(f'sum by (status) (rate({NS}_logs_parsed_total[5m]))', "{{status}}")],
        "Ingestion throughput. The label is the configured glob, not the concrete "
        "filename — deliberately, because a rotating log_filename would otherwise "
        "mint a new series on every rotation and Prometheus never evicts label values.",
        w=12, h=6, x=12, y=y,
        fc=field_config(unit="reqps", custom={"fillOpacity": 20})))
    y += 6

    p.append(panel(
        4, "Parse errors per second by type", "timeseries",
        [target(f'sum by (error_type) (rate({NS}_parse_errors_total[5m]))', "{{error_type}}")],
        "Entries the parser rejected. A steady trickle is normal on a busy log "
        "(truncated final entry, an unsupported statement). A step change means the "
        "log format moved under you — a PostgreSQL upgrade, or auto_explain.log_format "
        "being switched between text and json.",
        w=12, h=8, x=0, y=y,
        fc=field_config(unit="reqps", custom={"fillOpacity": 30,
                                              "stacking": {"group": "A", "mode": "normal"}})))

    p.append(panel(
        5, "Collection cycle duration", "timeseries",
        [target(f'histogram_quantile(0.95, sum by (operation, le) '
                f'(rate({NS}_export_duration_seconds_bucket[10m])))', "p95 {{operation}}")],
        "How long each phase of a cycle takes. If parse duration approaches the poll "
        "interval the exporter is about to fall behind; max_read_bytes_per_cycle "
        "bounds how much one cycle will attempt, deferring the rest rather than "
        "dropping it.",
        w=12, h=8, x=12, y=y, fc=field_config(unit="s")))
    y += 8

    p.append(panel(
        6, "Exporter resident memory", "timeseries",
        [target(f'{NS}_memory_usage_bytes', "resident")],
        "Peak memory is driven by the number of distinct query shapes retained, not "
        "by log volume — one representative plan is kept per fingerprint. A steady "
        "climb here tracks the distinct-shapes panel on the plan dashboard.",
        w=24, h=7, x=0, y=y,
        fc=field_config(unit="bytes", custom={"fillOpacity": 15})))
    return dashboard(
        "pgplansight-health", "Plansight — Exporter health",
        "Whether to trust the other two dashboards. Every panel here is about the "
        "collection pipeline, not about PostgreSQL.",
        p, ["pg-plansight", "exporter", "meta"])


DASHBOARDS = {
    "query-performance.json": queries_dashboard,
    "plan-shapes.json": plans_dashboard,
    "exporter-health.json": health_dashboard,
}


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", default=None, help="output directory (default: <repo>/dashboards)")
    ap.add_argument("--check", action="store_true",
                    help="exit non-zero if the committed JSON differs from what this would write")
    args = ap.parse_args()

    out = pathlib.Path(args.out) if args.out else pathlib.Path(__file__).resolve().parent.parent / "dashboards"
    out.mkdir(parents=True, exist_ok=True)

    stale = []
    for name, build in DASHBOARDS.items():
        rendered = json.dumps(build(), indent=2, sort_keys=False) + "\n"
        path = out / name
        if args.check:
            if not path.exists() or path.read_text() != rendered:
                stale.append(name)
        else:
            path.write_text(rendered)
            print(f"wrote {path.relative_to(pathlib.Path.cwd()) if str(path).startswith(str(pathlib.Path.cwd())) else path}")

    if args.check:
        if stale:
            print("Stale dashboard JSON: " + ", ".join(stale), file=sys.stderr)
            print("Run: python3 scripts/gen-grafana-dashboards.py", file=sys.stderr)
            return 1
        print(f"OK: all {len(DASHBOARDS)} dashboards match the generator.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
