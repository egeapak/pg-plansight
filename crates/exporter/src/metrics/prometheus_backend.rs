#[cfg(feature = "prometheus")]
use super::traits::MetricsBackend;
#[cfg(feature = "prometheus")]
use anyhow::Result;
#[cfg(feature = "prometheus")]
use prometheus::{
    CounterVec, GaugeVec, HistogramOpts, HistogramVec, IntCounterVec, IntGauge, Opts, Registry,
};
#[cfg(feature = "prometheus")]
use std::collections::{BTreeMap, HashMap, HashSet};
#[cfg(feature = "prometheus")]
use std::sync::Mutex;

/// Minimal LRU tracker that bounds how many distinct `normalized_query_hash`
/// label values the Prometheus backend keeps as live series.
///
/// Prometheus client label sets are never evicted on their own, so a
/// long-running daemon that observes many distinct query shapes would grow
/// series (and memory) without bound. `order` maps a monotonic access tick to a
/// hash so the least-recently-used hash is the first entry (O(log n) lookup),
/// and `entries` records, per hash, exactly which label combinations must be
/// removed from the metric vectors when that hash is evicted.
#[cfg(feature = "prometheus")]
#[derive(Default)]
struct QueryCardinalityLimiter {
    /// Maximum distinct hashes to retain. 0 means unlimited, but in that case
    /// the backend skips the limiter entirely, so `touch` is never called.
    max: usize,
    /// Monotonic counter used as the recency timestamp.
    tick: u64,
    /// hash -> the series recorded for that hash.
    entries: HashMap<String, QueryHashEntry>,
    /// recency tick -> hash, ordered so the first entry is the LRU hash.
    order: BTreeMap<u64, String>,
}

/// The set of series recorded for a single query hash, kept so they can be
/// removed from the metric vectors when the hash is evicted.
#[cfg(feature = "prometheus")]
#[derive(Default)]
struct QueryHashEntry {
    /// Last access tick; matches the key under which this hash lives in `order`.
    tick: u64,
    /// `database` values seen for the two-label per-query vecs
    /// (label order `[normalized_query_hash, database]`).
    databases: HashSet<String>,
    /// `(database, status)` pairs seen for `query_executions_total`
    /// (label order `[normalized_query_hash, database, status]`).
    executions: HashSet<(String, String)>,
}

/// Series that must be removed from the metric vectors after a hash is evicted.
#[cfg(feature = "prometheus")]
struct EvictedQuerySeries {
    hash: String,
    databases: Vec<String>,
    executions: Vec<(String, String)>,
}

#[cfg(feature = "prometheus")]
impl QueryCardinalityLimiter {
    fn new(max: usize) -> Self {
        Self {
            max,
            ..Default::default()
        }
    }

    /// Record an access to `hash` (recorded with `database`, plus an optional
    /// `status` for the executions counter). Returns the series that must be
    /// removed from the metric vectors if inserting this hash evicted the LRU
    /// hash.
    fn touch(
        &mut self,
        hash: &str,
        database: &str,
        status: Option<&str>,
    ) -> Option<EvictedQuerySeries> {
        self.tick += 1;
        let now = self.tick;

        if let Some(entry) = self.entries.get_mut(hash) {
            // Known hash: bump its recency and remember any newly seen labels.
            self.order.remove(&entry.tick);
            entry.tick = now;
            entry.databases.insert(database.to_string());
            if let Some(status) = status {
                entry
                    .executions
                    .insert((database.to_string(), status.to_string()));
            }
            self.order.insert(now, hash.to_string());
            return None;
        }

        // New hash: evict the LRU one first if we are already at the cap.
        let evicted = if self.entries.len() >= self.max {
            self.evict_lru()
        } else {
            None
        };

        let mut entry = QueryHashEntry {
            tick: now,
            ..Default::default()
        };
        entry.databases.insert(database.to_string());
        if let Some(status) = status {
            entry
                .executions
                .insert((database.to_string(), status.to_string()));
        }
        self.entries.insert(hash.to_string(), entry);
        self.order.insert(now, hash.to_string());
        evicted
    }

    /// Remove and return the least-recently-used hash's tracked series.
    fn evict_lru(&mut self) -> Option<EvictedQuerySeries> {
        let (&lru_tick, _) = self.order.first_key_value()?;
        let hash = self.order.remove(&lru_tick)?;
        let entry = self.entries.remove(&hash)?;
        Some(EvictedQuerySeries {
            hash,
            databases: entry.databases.into_iter().collect(),
            executions: entry.executions.into_iter().collect(),
        })
    }
}

#[cfg(feature = "prometheus")]
pub struct PrometheusBackend {
    pub registry: Registry,
    query_duration: HistogramVec,
    query_executions: CounterVec,
    slow_queries: CounterVec,
    query_plan_cost: HistogramVec,
    query_rows_examined: HistogramVec,
    database_avg_duration: HistogramVec,
    database_queries_per_second: HistogramVec,
    database_unique_queries: IntCounterVec,
    plan_node_types: CounterVec,
    scan_types: CounterVec,
    join_types: CounterVec,
    exporter_up: IntGauge,
    logs_parsed_total: CounterVec,
    parse_errors_total: CounterVec,
    export_duration: HistogramVec,
    memory_usage: IntGauge,
    last_successful_parse: IntGauge,
    // Derived per-query metrics (F7)
    query_latency_cv: GaugeVec,
    query_total_time_share_pct: GaugeVec,
    query_latency_p95_ms: GaugeVec,
    query_latency_p99_ms: GaugeVec,
    // First/last seen gauges (F9)
    query_first_seen_seconds: GaugeVec,
    query_last_seen_seconds: GaugeVec,
    // Bounded cardinality for per-query series. `max_query_cardinality` == 0
    // disables eviction entirely (unbounded, the historical behavior); the
    // limiter is only consulted when the cap is non-zero.
    max_query_cardinality: usize,
    query_cardinality: Mutex<QueryCardinalityLimiter>,
}

#[cfg(feature = "prometheus")]
impl PrometheusBackend {
    pub fn new(
        namespace: &str,
        histogram_buckets: Vec<f64>,
        max_query_cardinality: usize,
    ) -> Result<Self> {
        let registry = Registry::new();

        let query_duration = HistogramVec::new(
            HistogramOpts::new(
                format!("{}_query_duration_seconds", namespace),
                "Query execution duration in seconds",
            )
            .buckets(histogram_buckets.clone()),
            &["normalized_query_hash", "database"],
        )?;

        let query_executions = CounterVec::new(
            Opts::new(
                format!("{}_query_executions_total", namespace),
                "Total number of query executions",
            ),
            &["normalized_query_hash", "database", "status"],
        )?;

        let slow_queries = CounterVec::new(
            Opts::new(
                format!("{}_slow_queries_total", namespace),
                "Total number of slow queries by threshold",
            ),
            &["database", "threshold"],
        )?;

        let query_plan_cost = HistogramVec::new(
            HistogramOpts::new(
                format!("{}_query_plan_cost", namespace),
                "Query plan estimated cost",
            )
            .buckets(vec![0.01, 0.1, 1.0, 10.0, 100.0, 1000.0, 10000.0, 100000.0]),
            &["normalized_query_hash", "database"],
        )?;

        let query_rows_examined = HistogramVec::new(
            HistogramOpts::new(
                format!("{}_query_rows_examined", namespace),
                "Number of rows examined by query",
            )
            .buckets(vec![1.0, 10.0, 100.0, 1000.0, 10000.0, 100000.0, 1000000.0]),
            &["normalized_query_hash", "database"],
        )?;

        let database_avg_duration = HistogramVec::new(
            HistogramOpts::new(
                format!("{}_database_avg_query_duration_seconds", namespace),
                "Average query duration per database",
            )
            .buckets(histogram_buckets.clone()),
            &["database"],
        )?;

        let database_queries_per_second = HistogramVec::new(
            HistogramOpts::new(
                format!("{}_database_queries_per_second", namespace),
                "Queries per second rate per database",
            )
            .buckets(vec![0.1, 1.0, 5.0, 10.0, 50.0, 100.0, 500.0, 1000.0]),
            &["database"],
        )?;

        let database_unique_queries = IntCounterVec::new(
            Opts::new(
                format!("{}_database_unique_queries_total", namespace),
                "Total number of unique queries per database",
            ),
            &["database"],
        )?;

        let plan_node_types = CounterVec::new(
            Opts::new(
                format!("{}_query_plan_node_types_total", namespace),
                "Total count of plan node types",
            ),
            &["node_type", "database"],
        )?;

        let scan_types = CounterVec::new(
            Opts::new(
                format!("{}_query_scan_types_total", namespace),
                "Total count of scan types",
            ),
            &["scan_type", "database"],
        )?;

        let join_types = CounterVec::new(
            Opts::new(
                format!("{}_query_join_types_total", namespace),
                "Total count of join types",
            ),
            &["join_type", "database"],
        )?;

        let exporter_up = IntGauge::new(
            format!("{}_exporter_up", namespace),
            "Whether the exporter is running successfully",
        )?;
        exporter_up.set(1);

        let logs_parsed_total = CounterVec::new(
            Opts::new(
                format!("{}_logs_parsed_total", namespace),
                "Total number of log entries parsed",
            ),
            &["log_path_pattern", "status"],
        )?;

        let parse_errors_total = CounterVec::new(
            Opts::new(
                format!("{}_parse_errors_total", namespace),
                "Total number of parse errors",
            ),
            &["log_path_pattern", "error_type"],
        )?;

        let export_duration = HistogramVec::new(
            HistogramOpts::new(
                format!("{}_export_duration_seconds", namespace),
                "Time spent exporting metrics",
            )
            .buckets(vec![0.001, 0.01, 0.1, 1.0, 5.0, 10.0]),
            &["operation"],
        )?;

        let memory_usage = IntGauge::new(
            format!("{}_memory_usage_bytes", namespace),
            "Current memory usage in bytes",
        )?;

        let last_successful_parse = IntGauge::new(
            format!("{}_last_successful_parse_timestamp", namespace),
            "Timestamp of last successful parse operation",
        )?;

        // Derived per-query metrics (F7)
        let query_latency_cv = GaugeVec::new(
            Opts::new(
                format!("{}_query_latency_cv", namespace),
                "Coefficient of variation of query latency (stddev/mean); flags unstable/bimodal queries",
            ),
            &["normalized_query_hash", "database"],
        )?;

        let query_total_time_share_pct = GaugeVec::new(
            Opts::new(
                format!("{}_query_total_time_share_pct", namespace),
                "Percent of total DB time across the exported set attributable to this query",
            ),
            &["normalized_query_hash", "database"],
        )?;

        let query_latency_p95_ms = GaugeVec::new(
            Opts::new(
                format!("{}_query_latency_p95_ms", namespace),
                "95th percentile query latency in milliseconds",
            ),
            &["normalized_query_hash", "database"],
        )?;

        let query_latency_p99_ms = GaugeVec::new(
            Opts::new(
                format!("{}_query_latency_p99_ms", namespace),
                "99th percentile query latency in milliseconds",
            ),
            &["normalized_query_hash", "database"],
        )?;

        // First/last seen gauges (F9)
        let query_first_seen_seconds = GaugeVec::new(
            Opts::new(
                format!("{}_query_first_seen_seconds", namespace),
                "Unix epoch seconds when this query fingerprint was first seen",
            ),
            &["normalized_query_hash", "database"],
        )?;

        let query_last_seen_seconds = GaugeVec::new(
            Opts::new(
                format!("{}_query_last_seen_seconds", namespace),
                "Unix epoch seconds when this query fingerprint was last seen",
            ),
            &["normalized_query_hash", "database"],
        )?;

        registry.register(Box::new(query_duration.clone()))?;
        registry.register(Box::new(query_executions.clone()))?;
        registry.register(Box::new(slow_queries.clone()))?;
        registry.register(Box::new(query_plan_cost.clone()))?;
        registry.register(Box::new(query_rows_examined.clone()))?;
        registry.register(Box::new(database_avg_duration.clone()))?;
        registry.register(Box::new(database_queries_per_second.clone()))?;
        registry.register(Box::new(database_unique_queries.clone()))?;
        registry.register(Box::new(plan_node_types.clone()))?;
        registry.register(Box::new(scan_types.clone()))?;
        registry.register(Box::new(join_types.clone()))?;
        registry.register(Box::new(exporter_up.clone()))?;
        registry.register(Box::new(logs_parsed_total.clone()))?;
        registry.register(Box::new(parse_errors_total.clone()))?;
        registry.register(Box::new(export_duration.clone()))?;
        registry.register(Box::new(memory_usage.clone()))?;
        registry.register(Box::new(last_successful_parse.clone()))?;
        registry.register(Box::new(query_latency_cv.clone()))?;
        registry.register(Box::new(query_total_time_share_pct.clone()))?;
        registry.register(Box::new(query_latency_p95_ms.clone()))?;
        registry.register(Box::new(query_latency_p99_ms.clone()))?;
        registry.register(Box::new(query_first_seen_seconds.clone()))?;
        registry.register(Box::new(query_last_seen_seconds.clone()))?;

        Ok(Self {
            registry,
            query_duration,
            query_executions,
            slow_queries,
            query_plan_cost,
            query_rows_examined,
            database_avg_duration,
            database_queries_per_second,
            database_unique_queries,
            plan_node_types,
            scan_types,
            join_types,
            exporter_up,
            logs_parsed_total,
            parse_errors_total,
            export_duration,
            memory_usage,
            last_successful_parse,
            query_latency_cv,
            query_total_time_share_pct,
            query_latency_p95_ms,
            query_latency_p99_ms,
            query_first_seen_seconds,
            query_last_seen_seconds,
            max_query_cardinality,
            query_cardinality: Mutex::new(QueryCardinalityLimiter::new(max_query_cardinality)),
        })
    }

    /// Record that a per-query series carrying `normalized_query_hash` = `hash`
    /// was just touched, evicting the least-recently-used hash's series when the
    /// configured cap would otherwise be exceeded. A cap of 0 disables this
    /// entirely and preserves the historical unbounded behavior.
    fn note_query(&self, hash: &str, database: &str, status: Option<&str>) {
        if self.max_query_cardinality == 0 {
            return;
        }
        let evicted = {
            let mut limiter = self
                .query_cardinality
                .lock()
                .expect("query cardinality limiter mutex poisoned");
            limiter.touch(hash, database, status)
        };
        if let Some(evicted) = evicted {
            self.remove_query_series(&evicted);
        }
    }

    /// Remove every series belonging to an evicted query hash from the
    /// per-query metric vectors so it disappears from `/metrics`. Errors are
    /// ignored: not every vec necessarily has a series for a given label tuple.
    fn remove_query_series(&self, evicted: &EvictedQuerySeries) {
        for database in &evicted.databases {
            // Two-label vecs, label order [normalized_query_hash, database].
            let labels = [evicted.hash.as_str(), database.as_str()];
            let _ = self.query_duration.remove_label_values(&labels);
            let _ = self.query_plan_cost.remove_label_values(&labels);
            let _ = self.query_rows_examined.remove_label_values(&labels);
            let _ = self.query_latency_cv.remove_label_values(&labels);
            let _ = self.query_total_time_share_pct.remove_label_values(&labels);
            let _ = self.query_latency_p95_ms.remove_label_values(&labels);
            let _ = self.query_latency_p99_ms.remove_label_values(&labels);
            let _ = self.query_first_seen_seconds.remove_label_values(&labels);
            let _ = self.query_last_seen_seconds.remove_label_values(&labels);
        }
        for (database, status) in &evicted.executions {
            // query_executions_total, label order
            // [normalized_query_hash, database, status].
            let labels = [evicted.hash.as_str(), database.as_str(), status.as_str()];
            let _ = self.query_executions.remove_label_values(&labels);
        }
    }
}

#[cfg(feature = "prometheus")]
impl MetricsBackend for PrometheusBackend {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn record_query_duration(&self, labels: &HashMap<&str, String>, duration: f64) {
        let hash = labels
            .get("normalized_query_hash")
            .map(|s| s.as_str())
            .unwrap_or("");
        let database = labels.get("database").map(|s| s.as_str()).unwrap_or("");
        self.note_query(hash, database, None);
        self.query_duration
            .with_label_values(&[hash, database])
            .observe(duration);
    }

    fn increment_query_executions(&self, labels: &HashMap<&str, String>) {
        let hash = labels
            .get("normalized_query_hash")
            .map(|s| s.as_str())
            .unwrap_or("");
        let database = labels.get("database").map(|s| s.as_str()).unwrap_or("");
        let status = labels.get("status").map(|s| s.as_str()).unwrap_or("");
        self.note_query(hash, database, Some(status));
        self.query_executions
            .with_label_values(&[hash, database, status])
            .inc();
    }

    fn increment_slow_queries(&self, labels: &HashMap<&str, String>) {
        self.slow_queries
            .with_label_values(&[
                labels.get("database").map(|s| s.as_str()).unwrap_or(""),
                labels.get("threshold").map(|s| s.as_str()).unwrap_or(""),
            ])
            .inc();
    }

    fn increment_slow_queries_by(&self, labels: &HashMap<&str, String>, count: u64) {
        self.slow_queries
            .with_label_values(&[
                labels.get("database").map(|s| s.as_str()).unwrap_or(""),
                labels.get("threshold").map(|s| s.as_str()).unwrap_or(""),
            ])
            .inc_by(count as f64);
    }

    fn record_query_plan_cost(&self, labels: &HashMap<&str, String>, cost: f64) {
        let hash = labels
            .get("normalized_query_hash")
            .map(|s| s.as_str())
            .unwrap_or("");
        let database = labels.get("database").map(|s| s.as_str()).unwrap_or("");
        self.note_query(hash, database, None);
        self.query_plan_cost
            .with_label_values(&[hash, database])
            .observe(cost);
    }

    fn record_query_rows_examined(&self, labels: &HashMap<&str, String>, rows: f64) {
        let hash = labels
            .get("normalized_query_hash")
            .map(|s| s.as_str())
            .unwrap_or("");
        let database = labels.get("database").map(|s| s.as_str()).unwrap_or("");
        self.note_query(hash, database, None);
        self.query_rows_examined
            .with_label_values(&[hash, database])
            .observe(rows);
    }

    fn record_database_avg_duration(&self, labels: &HashMap<&str, String>, duration: f64) {
        self.database_avg_duration
            .with_label_values(&[labels.get("database").map(|s| s.as_str()).unwrap_or("")])
            .observe(duration);
    }

    fn record_database_qps(&self, labels: &HashMap<&str, String>, qps: f64) {
        self.database_queries_per_second
            .with_label_values(&[labels.get("database").map(|s| s.as_str()).unwrap_or("")])
            .observe(qps);
    }

    fn increment_database_unique_queries(&self, labels: &HashMap<&str, String>, count: u64) {
        self.database_unique_queries
            .with_label_values(&[labels.get("database").map(|s| s.as_str()).unwrap_or("")])
            .inc_by(count);
    }

    fn increment_plan_node_type(&self, labels: &HashMap<&str, String>) {
        self.plan_node_types
            .with_label_values(&[
                labels.get("node_type").map(|s| s.as_str()).unwrap_or(""),
                labels.get("database").map(|s| s.as_str()).unwrap_or(""),
            ])
            .inc();
    }

    fn increment_scan_type(&self, labels: &HashMap<&str, String>) {
        self.scan_types
            .with_label_values(&[
                labels.get("scan_type").map(|s| s.as_str()).unwrap_or(""),
                labels.get("database").map(|s| s.as_str()).unwrap_or(""),
            ])
            .inc();
    }

    fn increment_join_type(&self, labels: &HashMap<&str, String>) {
        self.join_types
            .with_label_values(&[
                labels.get("join_type").map(|s| s.as_str()).unwrap_or(""),
                labels.get("database").map(|s| s.as_str()).unwrap_or(""),
            ])
            .inc();
    }

    fn set_exporter_up(&self, up: i64) {
        self.exporter_up.set(up);
    }

    fn increment_logs_parsed(&self, labels: &HashMap<&str, String>) {
        self.logs_parsed_total
            .with_label_values(&[
                labels
                    .get("log_path_pattern")
                    .map(|s| s.as_str())
                    .unwrap_or(""),
                labels.get("status").map(|s| s.as_str()).unwrap_or(""),
            ])
            .inc();
    }

    fn increment_logs_parsed_by(&self, labels: &HashMap<&str, String>, count: u64) {
        self.logs_parsed_total
            .with_label_values(&[
                labels
                    .get("log_path_pattern")
                    .map(|s| s.as_str())
                    .unwrap_or(""),
                labels.get("status").map(|s| s.as_str()).unwrap_or(""),
            ])
            .inc_by(count as f64);
    }

    fn increment_parse_errors(&self, labels: &HashMap<&str, String>) {
        self.parse_errors_total
            .with_label_values(&[
                labels
                    .get("log_path_pattern")
                    .map(|s| s.as_str())
                    .unwrap_or(""),
                labels.get("error_type").map(|s| s.as_str()).unwrap_or(""),
            ])
            .inc();
    }

    fn record_export_duration(&self, labels: &HashMap<&str, String>, duration: f64) {
        self.export_duration
            .with_label_values(&[labels.get("operation").map(|s| s.as_str()).unwrap_or("")])
            .observe(duration);
    }

    fn set_memory_usage(&self, bytes: i64) {
        self.memory_usage.set(bytes);
    }

    fn set_last_successful_parse(&self, timestamp: i64) {
        self.last_successful_parse.set(timestamp);
    }

    fn set_query_latency_cv(&self, labels: &HashMap<&str, String>, cv: f64) {
        let hash = labels
            .get("normalized_query_hash")
            .map(|s| s.as_str())
            .unwrap_or("");
        let database = labels.get("database").map(|s| s.as_str()).unwrap_or("");
        self.note_query(hash, database, None);
        self.query_latency_cv
            .with_label_values(&[hash, database])
            .set(cv);
    }

    fn set_query_total_time_share_pct(&self, labels: &HashMap<&str, String>, pct: f64) {
        let hash = labels
            .get("normalized_query_hash")
            .map(|s| s.as_str())
            .unwrap_or("");
        let database = labels.get("database").map(|s| s.as_str()).unwrap_or("");
        self.note_query(hash, database, None);
        self.query_total_time_share_pct
            .with_label_values(&[hash, database])
            .set(pct);
    }

    fn set_query_latency_p95_ms(&self, labels: &HashMap<&str, String>, p95_ms: f64) {
        let hash = labels
            .get("normalized_query_hash")
            .map(|s| s.as_str())
            .unwrap_or("");
        let database = labels.get("database").map(|s| s.as_str()).unwrap_or("");
        self.note_query(hash, database, None);
        self.query_latency_p95_ms
            .with_label_values(&[hash, database])
            .set(p95_ms);
    }

    fn set_query_latency_p99_ms(&self, labels: &HashMap<&str, String>, p99_ms: f64) {
        let hash = labels
            .get("normalized_query_hash")
            .map(|s| s.as_str())
            .unwrap_or("");
        let database = labels.get("database").map(|s| s.as_str()).unwrap_or("");
        self.note_query(hash, database, None);
        self.query_latency_p99_ms
            .with_label_values(&[hash, database])
            .set(p99_ms);
    }

    fn set_query_first_seen_seconds(&self, labels: &HashMap<&str, String>, secs: f64) {
        let hash = labels
            .get("normalized_query_hash")
            .map(|s| s.as_str())
            .unwrap_or("");
        let database = labels.get("database").map(|s| s.as_str()).unwrap_or("");
        self.note_query(hash, database, None);
        self.query_first_seen_seconds
            .with_label_values(&[hash, database])
            .set(secs);
    }

    fn set_query_last_seen_seconds(&self, labels: &HashMap<&str, String>, secs: f64) {
        let hash = labels
            .get("normalized_query_hash")
            .map(|s| s.as_str())
            .unwrap_or("");
        let database = labels.get("database").map(|s| s.as_str()).unwrap_or("");
        self.note_query(hash, database, None);
        self.query_last_seen_seconds
            .with_label_values(&[hash, database])
            .set(secs);
    }

    fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}

#[cfg(all(test, feature = "prometheus"))]
mod cardinality_tests {
    use super::{MetricsBackend, PrometheusBackend};
    use std::collections::HashMap;

    /// Collect all `normalized_query_hash` label values currently present in the
    /// gathered metric family named `metric_name`.
    fn query_hashes_in_family(backend: &PrometheusBackend, metric_name: &str) -> Vec<String> {
        let mut hashes = Vec::new();
        for family in backend.registry.gather() {
            if family.name() != metric_name {
                continue;
            }
            for metric in family.get_metric() {
                for pair in metric.get_label() {
                    if pair.name() == "normalized_query_hash" {
                        hashes.push(pair.value().to_string());
                    }
                }
            }
        }
        hashes
    }

    fn labels_for(hash: &str) -> HashMap<&'static str, String> {
        let mut labels = HashMap::new();
        labels.insert("normalized_query_hash", hash.to_string());
        labels.insert("database", "testdb".to_string());
        labels
    }

    #[test]
    fn evicts_least_recently_used_query_hash_when_cap_exceeded() {
        // Cap of 2 distinct query hashes.
        let backend = PrometheusBackend::new("test", vec![1.0], 2).unwrap();

        // Record three distinct hashes in order: a, b, c. Once "c" arrives the
        // cap is exceeded and the LRU hash ("a") must be evicted.
        backend.record_query_duration(&labels_for("a"), 1.0);
        backend.record_query_duration(&labels_for("b"), 1.0);
        backend.record_query_duration(&labels_for("c"), 1.0);

        let mut hashes = query_hashes_in_family(&backend, "test_query_duration_seconds");
        hashes.sort();
        assert_eq!(
            hashes,
            vec!["b".to_string(), "c".to_string()],
            "only the two most-recent hashes should remain; 'a' should be evicted"
        );
    }

    #[test]
    fn eviction_removes_series_across_all_per_query_vecs() {
        let backend = PrometheusBackend::new("test", vec![1.0], 1).unwrap();

        // Populate every per-query metric for hash "a".
        backend.record_query_duration(&labels_for("a"), 1.0);
        let mut exec_labels = labels_for("a");
        exec_labels.insert("status", "success".to_string());
        backend.increment_query_executions(&exec_labels);
        backend.set_query_latency_p95_ms(&labels_for("a"), 1.0);

        // A second hash evicts "a" (cap is 1).
        backend.record_query_duration(&labels_for("b"), 1.0);

        for family in [
            "test_query_duration_seconds",
            "test_query_executions_total",
            "test_query_latency_p95_ms",
        ] {
            let hashes = query_hashes_in_family(&backend, family);
            assert!(
                !hashes.contains(&"a".to_string()),
                "evicted hash 'a' should be gone from {family}, got {hashes:?}"
            );
        }
    }

    #[test]
    fn unlimited_cap_keeps_all_query_hashes() {
        // A cap of 0 disables eviction and preserves the unbounded behavior.
        let backend = PrometheusBackend::new("test", vec![1.0], 0).unwrap();
        for hash in ["a", "b", "c", "d"] {
            backend.record_query_duration(&labels_for(hash), 1.0);
        }
        let hashes = query_hashes_in_family(&backend, "test_query_duration_seconds");
        assert_eq!(hashes.len(), 4, "no hashes should be evicted when cap is 0");
    }

    #[test]
    fn touching_a_hash_refreshes_its_recency() {
        // The core LRU semantic: re-recording an existing hash must move it to
        // most-recently-used so a genuinely idle hash is evicted instead. With
        // cap 2: record a, b; touch a again; record c. LRU is now "b", so "b"
        // (not "a") must be evicted even though "a" was inserted first.
        let backend = PrometheusBackend::new("test", vec![1.0], 2).unwrap();
        backend.record_query_duration(&labels_for("a"), 1.0);
        backend.record_query_duration(&labels_for("b"), 1.0);
        backend.record_query_duration(&labels_for("a"), 1.0); // refresh "a"
        backend.record_query_duration(&labels_for("c"), 1.0); // evicts LRU = "b"

        let mut hashes = query_hashes_in_family(&backend, "test_query_duration_seconds");
        hashes.sort();
        assert_eq!(
            hashes,
            vec!["a".to_string(), "c".to_string()],
            "recently-touched 'a' must survive; idle 'b' must be evicted"
        );
    }

    #[test]
    fn evicted_hash_can_be_readmitted() {
        // Eviction is not permanent blacklisting: a churned-out hash that is
        // seen again is re-admitted as a fresh series (its counter restarts —
        // the documented trade-off of bounded cardinality). Cap 1: a, then b
        // evicts a, then a again evicts b and reappears.
        let backend = PrometheusBackend::new("test", vec![1.0], 1).unwrap();
        backend.record_query_duration(&labels_for("a"), 1.0);
        backend.record_query_duration(&labels_for("b"), 1.0);
        backend.record_query_duration(&labels_for("a"), 1.0);

        let hashes = query_hashes_in_family(&backend, "test_query_duration_seconds");
        assert_eq!(
            hashes,
            vec!["a".to_string()],
            "re-seen 'a' should be re-admitted and 'b' evicted"
        );
    }
}
