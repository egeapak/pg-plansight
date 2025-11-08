#[cfg(feature = "prometheus")]
use super::traits::MetricsBackend;
#[cfg(feature = "prometheus")]
use anyhow::Result;
#[cfg(feature = "prometheus")]
use prometheus::{
    Counter, CounterVec, Histogram, HistogramOpts, HistogramVec, IntCounterVec, IntGauge, Opts,
    Registry,
};
#[cfg(feature = "prometheus")]
use std::collections::HashMap;

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
}

#[cfg(feature = "prometheus")]
impl PrometheusBackend {
    pub fn new(namespace: &str, histogram_buckets: Vec<f64>) -> Result<Self> {
        let registry = Registry::new();

        let query_duration = HistogramVec::new(
            HistogramOpts::new(
                format!("{}_query_duration_seconds", namespace),
                "Query execution duration in seconds",
            )
            .buckets(histogram_buckets.clone()),
            &["normalized_query_hash", "database", "query_timestamp"],
        )?;

        let query_executions = CounterVec::new(
            Opts::new(
                format!("{}_query_executions_total", namespace),
                "Total number of query executions",
            ),
            &[
                "normalized_query_hash",
                "database",
                "query_timestamp",
                "status",
            ],
        )?;

        let slow_queries = CounterVec::new(
            Opts::new(
                format!("{}_slow_queries_total", namespace),
                "Total number of slow queries by threshold",
            ),
            &["database", "query_timestamp", "threshold"],
        )?;

        let query_plan_cost = HistogramVec::new(
            HistogramOpts::new(
                format!("{}_query_plan_cost", namespace),
                "Query plan estimated cost",
            )
            .buckets(vec![0.01, 0.1, 1.0, 10.0, 100.0, 1000.0, 10000.0, 100000.0]),
            &["normalized_query_hash", "database", "query_timestamp"],
        )?;

        let query_rows_examined = HistogramVec::new(
            HistogramOpts::new(
                format!("{}_query_rows_examined", namespace),
                "Number of rows examined by query",
            )
            .buckets(vec![1.0, 10.0, 100.0, 1000.0, 10000.0, 100000.0, 1000000.0]),
            &["normalized_query_hash", "database", "query_timestamp"],
        )?;

        let database_avg_duration = HistogramVec::new(
            HistogramOpts::new(
                format!("{}_database_avg_query_duration_seconds", namespace),
                "Average query duration per database",
            )
            .buckets(histogram_buckets.clone()),
            &["database", "query_timestamp"],
        )?;

        let database_queries_per_second = HistogramVec::new(
            HistogramOpts::new(
                format!("{}_database_queries_per_second", namespace),
                "Queries per second rate per database",
            )
            .buckets(vec![0.1, 1.0, 5.0, 10.0, 50.0, 100.0, 500.0, 1000.0]),
            &["database", "query_timestamp"],
        )?;

        let database_unique_queries = IntCounterVec::new(
            Opts::new(
                format!("{}_database_unique_queries_total", namespace),
                "Total number of unique queries per database",
            ),
            &["database", "query_timestamp"],
        )?;

        let plan_node_types = CounterVec::new(
            Opts::new(
                format!("{}_query_plan_node_types_total", namespace),
                "Total count of plan node types",
            ),
            &["node_type", "database", "query_timestamp"],
        )?;

        let scan_types = CounterVec::new(
            Opts::new(
                format!("{}_query_scan_types_total", namespace),
                "Total count of scan types",
            ),
            &["scan_type", "database", "query_timestamp"],
        )?;

        let join_types = CounterVec::new(
            Opts::new(
                format!("{}_query_join_types_total", namespace),
                "Total count of join types",
            ),
            &["join_type", "database", "query_timestamp"],
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
            &["file_path", "status"],
        )?;

        let parse_errors_total = CounterVec::new(
            Opts::new(
                format!("{}_parse_errors_total", namespace),
                "Total number of parse errors",
            ),
            &["file_path", "error_type"],
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
        })
    }
}

#[cfg(feature = "prometheus")]
impl MetricsBackend for PrometheusBackend {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn record_query_duration(&self, labels: &HashMap<&str, String>, duration: f64) {
        self.query_duration
            .with_label_values(&[
                labels
                    .get("normalized_query_hash")
                    .map(|s| s.as_str())
                    .unwrap_or(""),
                labels.get("database").map(|s| s.as_str()).unwrap_or(""),
                labels
                    .get("query_timestamp")
                    .map(|s| s.as_str())
                    .unwrap_or(""),
            ])
            .observe(duration);
    }

    fn increment_query_executions(&self, labels: &HashMap<&str, String>) {
        self.query_executions
            .with_label_values(&[
                labels
                    .get("normalized_query_hash")
                    .map(|s| s.as_str())
                    .unwrap_or(""),
                labels.get("database").map(|s| s.as_str()).unwrap_or(""),
                labels
                    .get("query_timestamp")
                    .map(|s| s.as_str())
                    .unwrap_or(""),
                labels.get("status").map(|s| s.as_str()).unwrap_or(""),
            ])
            .inc();
    }

    fn increment_slow_queries(&self, labels: &HashMap<&str, String>) {
        self.slow_queries
            .with_label_values(&[
                labels.get("database").map(|s| s.as_str()).unwrap_or(""),
                labels
                    .get("query_timestamp")
                    .map(|s| s.as_str())
                    .unwrap_or(""),
                labels.get("threshold").map(|s| s.as_str()).unwrap_or(""),
            ])
            .inc();
    }

    fn record_query_plan_cost(&self, labels: &HashMap<&str, String>, cost: f64) {
        self.query_plan_cost
            .with_label_values(&[
                labels
                    .get("normalized_query_hash")
                    .map(|s| s.as_str())
                    .unwrap_or(""),
                labels.get("database").map(|s| s.as_str()).unwrap_or(""),
                labels
                    .get("query_timestamp")
                    .map(|s| s.as_str())
                    .unwrap_or(""),
            ])
            .observe(cost);
    }

    fn record_query_rows_examined(&self, labels: &HashMap<&str, String>, rows: f64) {
        self.query_rows_examined
            .with_label_values(&[
                labels
                    .get("normalized_query_hash")
                    .map(|s| s.as_str())
                    .unwrap_or(""),
                labels.get("database").map(|s| s.as_str()).unwrap_or(""),
                labels
                    .get("query_timestamp")
                    .map(|s| s.as_str())
                    .unwrap_or(""),
            ])
            .observe(rows);
    }

    fn record_database_avg_duration(&self, labels: &HashMap<&str, String>, duration: f64) {
        self.database_avg_duration
            .with_label_values(&[
                labels.get("database").map(|s| s.as_str()).unwrap_or(""),
                labels
                    .get("query_timestamp")
                    .map(|s| s.as_str())
                    .unwrap_or(""),
            ])
            .observe(duration);
    }

    fn record_database_qps(&self, labels: &HashMap<&str, String>, qps: f64) {
        self.database_queries_per_second
            .with_label_values(&[
                labels.get("database").map(|s| s.as_str()).unwrap_or(""),
                labels
                    .get("query_timestamp")
                    .map(|s| s.as_str())
                    .unwrap_or(""),
            ])
            .observe(qps);
    }

    fn increment_database_unique_queries(&self, labels: &HashMap<&str, String>, count: u64) {
        self.database_unique_queries
            .with_label_values(&[
                labels.get("database").map(|s| s.as_str()).unwrap_or(""),
                labels
                    .get("query_timestamp")
                    .map(|s| s.as_str())
                    .unwrap_or(""),
            ])
            .inc_by(count);
    }

    fn increment_plan_node_type(&self, labels: &HashMap<&str, String>) {
        self.plan_node_types
            .with_label_values(&[
                labels.get("node_type").map(|s| s.as_str()).unwrap_or(""),
                labels.get("database").map(|s| s.as_str()).unwrap_or(""),
                labels
                    .get("query_timestamp")
                    .map(|s| s.as_str())
                    .unwrap_or(""),
            ])
            .inc();
    }

    fn increment_scan_type(&self, labels: &HashMap<&str, String>) {
        self.scan_types
            .with_label_values(&[
                labels.get("scan_type").map(|s| s.as_str()).unwrap_or(""),
                labels.get("database").map(|s| s.as_str()).unwrap_or(""),
                labels
                    .get("query_timestamp")
                    .map(|s| s.as_str())
                    .unwrap_or(""),
            ])
            .inc();
    }

    fn increment_join_type(&self, labels: &HashMap<&str, String>) {
        self.join_types
            .with_label_values(&[
                labels.get("join_type").map(|s| s.as_str()).unwrap_or(""),
                labels.get("database").map(|s| s.as_str()).unwrap_or(""),
                labels
                    .get("query_timestamp")
                    .map(|s| s.as_str())
                    .unwrap_or(""),
            ])
            .inc();
    }

    fn set_exporter_up(&self, up: i64) {
        self.exporter_up.set(up);
    }

    fn increment_logs_parsed(&self, labels: &HashMap<&str, String>) {
        self.logs_parsed_total
            .with_label_values(&[
                labels.get("file_path").map(|s| s.as_str()).unwrap_or(""),
                labels.get("status").map(|s| s.as_str()).unwrap_or(""),
            ])
            .inc();
    }

    fn increment_parse_errors(&self, labels: &HashMap<&str, String>) {
        self.parse_errors_total
            .with_label_values(&[
                labels.get("file_path").map(|s| s.as_str()).unwrap_or(""),
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

    fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}
