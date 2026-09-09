#[cfg(feature = "opentelemetry")]
use super::traits::MetricsBackend;
#[cfg(feature = "opentelemetry")]
use anyhow::Result;
#[cfg(feature = "opentelemetry")]
use opentelemetry::{
    KeyValue,
    metrics::{Counter, Gauge, Histogram, Meter, MeterProvider},
};
#[cfg(feature = "opentelemetry")]
use opentelemetry_sdk::metrics::SdkMeterProvider;
#[cfg(feature = "opentelemetry")]
use std::collections::HashMap;
#[cfg(feature = "opentelemetry")]
use std::sync::Arc;

#[cfg(feature = "opentelemetry")]
pub struct OpenTelemetryBackend {
    _meter_provider: Arc<SdkMeterProvider>,
    _meter: Meter,

    // Query performance metrics
    query_duration: Histogram<f64>,
    query_executions: Counter<u64>,
    slow_queries: Counter<u64>,

    // Query complexity metrics
    query_plan_cost: Histogram<f64>,
    query_rows_examined: Histogram<f64>,

    // Database aggregate metrics
    database_avg_duration: Histogram<f64>,
    database_qps: Histogram<f64>,
    database_unique_queries: Counter<u64>,

    // Plan analysis metrics
    plan_node_types: Counter<u64>,
    scan_types: Counter<u64>,
    join_types: Counter<u64>,

    // Exporter self-monitoring metrics. These are point-in-time values, so
    // they must be Gauge instruments: implementing set_* as
    // UpDownCounter.add() summed every snapshot into a meaningless total.
    exporter_up: Gauge<i64>,
    logs_parsed_total: Counter<u64>,
    parse_errors_total: Counter<u64>,
    export_duration: Histogram<f64>,
    memory_usage: Gauge<i64>,
    last_successful_parse: Gauge<i64>,

    // Derived per-query metrics (F7)
    query_latency_cv: Gauge<f64>,
    query_total_time_share_pct: Gauge<f64>,
    query_latency_p95_ms: Gauge<f64>,
    query_latency_p99_ms: Gauge<f64>,

    // First/last seen gauges (F9)
    query_first_seen_seconds: Gauge<f64>,
    query_last_seen_seconds: Gauge<f64>,
    query_info: Gauge<u64>,
}

#[cfg(feature = "opentelemetry")]
impl OpenTelemetryBackend {
    pub fn new(meter_provider: SdkMeterProvider, namespace: &str) -> Result<Self> {
        let meter_provider = Arc::new(meter_provider);
        // Convert namespace to 'static lifetime (acceptable for initialization-time setup)
        let static_namespace: &'static str = Box::leak(namespace.to_string().into_boxed_str());
        let meter = meter_provider.meter(static_namespace);

        // Query performance metrics
        let query_duration = meter
            .f64_histogram(format!("{}.query.duration_seconds", namespace))
            .with_description("Query execution duration in seconds")
            .build();

        let query_executions = meter
            .u64_counter(format!("{}.query.executions_total", namespace))
            .with_description("Total number of query executions")
            .build();

        let slow_queries = meter
            .u64_counter(format!("{}.query.slow_queries_total", namespace))
            .with_description("Total number of slow queries by threshold")
            .build();

        // Query complexity metrics
        let query_plan_cost = meter
            .f64_histogram(format!("{}.query.plan_cost", namespace))
            .with_description("Query plan estimated cost")
            .build();

        let query_rows_examined = meter
            .f64_histogram(format!("{}.query.rows_examined", namespace))
            .with_description("Number of rows examined by query")
            .build();

        // Database aggregate metrics
        let database_avg_duration = meter
            .f64_histogram(format!("{}.database.avg_query_duration_seconds", namespace))
            .with_description("Average query duration per database")
            .build();

        let database_qps = meter
            .f64_histogram(format!("{}.database.queries_per_second", namespace))
            .with_description("Queries per second rate per database")
            .build();

        let database_unique_queries = meter
            .u64_counter(format!("{}.database.unique_queries_total", namespace))
            .with_description("Total number of unique queries per database")
            .build();

        // Plan analysis metrics
        let plan_node_types = meter
            .u64_counter(format!("{}.plan.node_types_total", namespace))
            .with_description("Total count of plan node types")
            .build();

        let scan_types = meter
            .u64_counter(format!("{}.plan.scan_types_total", namespace))
            .with_description("Total count of scan types")
            .build();

        let join_types = meter
            .u64_counter(format!("{}.plan.join_types_total", namespace))
            .with_description("Total count of join types")
            .build();

        // Exporter self-monitoring metrics
        let exporter_up = meter
            .i64_gauge(format!("{}.exporter.up", namespace))
            .with_description("Whether the exporter is running successfully")
            .build();

        let logs_parsed_total = meter
            .u64_counter(format!("{}.exporter.logs_parsed_total", namespace))
            .with_description("Total number of log entries parsed")
            .build();

        let parse_errors_total = meter
            .u64_counter(format!("{}.exporter.parse_errors_total", namespace))
            .with_description("Total number of parse errors")
            .build();

        let export_duration = meter
            .f64_histogram(format!("{}.exporter.export_duration_seconds", namespace))
            .with_description("Time spent exporting metrics")
            .build();

        let memory_usage = meter
            .i64_gauge(format!("{}.exporter.memory_usage_bytes", namespace))
            .with_description("Current memory usage in bytes")
            .build();

        let last_successful_parse = meter
            .i64_gauge(format!(
                "{}.exporter.last_successful_parse_timestamp",
                namespace
            ))
            .with_description("Timestamp of last successful parse operation")
            .build();

        // Derived per-query metrics (F7)
        let query_latency_cv = meter
            .f64_gauge(format!("{}.query.latency_cv", namespace))
            .with_description(
                "Coefficient of variation of query latency (stddev/mean); flags unstable/bimodal queries",
            )
            .build();

        let query_total_time_share_pct = meter
            .f64_gauge(format!("{}.query.total_time_share_pct", namespace))
            .with_description(
                "Percent of total DB time across the exported set attributable to this query",
            )
            .build();

        let query_latency_p95_ms = meter
            .f64_gauge(format!("{}.query.latency_p95_ms", namespace))
            .with_description("95th percentile query latency in milliseconds")
            .build();

        let query_latency_p99_ms = meter
            .f64_gauge(format!("{}.query.latency_p99_ms", namespace))
            .with_description("99th percentile query latency in milliseconds")
            .build();

        // First/last seen gauges (F9)
        let query_first_seen_seconds = meter
            .f64_gauge(format!("{}.query.first_seen_seconds", namespace))
            .with_description("Unix epoch seconds when this query fingerprint was first seen")
            .build();

        let query_last_seen_seconds = meter
            .f64_gauge(format!("{}.query.last_seen_seconds", namespace))
            .with_description("Unix epoch seconds when this query fingerprint was last seen")
            .build();

        // Hash -> query shape mapping, published as an info metric whose value
        // is always 1 and whose meaning lives in the attributes.
        let query_info = meter
            .u64_gauge(format!("{}.query.info", namespace))
            .with_description(
                "Always 1; carries the query_shape attribute for a normalized_query_hash",
            )
            .build();

        // Set initial value for exporter_up
        exporter_up.record(1, &[]);

        Ok(Self {
            _meter_provider: meter_provider,
            _meter: meter,
            query_duration,
            query_executions,
            slow_queries,
            query_plan_cost,
            query_rows_examined,
            database_avg_duration,
            database_qps,
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
            query_info,
        })
    }

    fn labels_to_attributes(&self, labels: &HashMap<&str, String>) -> Vec<KeyValue> {
        labels
            .iter()
            .map(|(k, v)| KeyValue::new(k.to_string(), v.clone()))
            .collect()
    }
}

#[cfg(feature = "opentelemetry")]
impl MetricsBackend for OpenTelemetryBackend {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn record_query_duration(&self, labels: &HashMap<&str, String>, duration: f64) {
        let attrs = self.labels_to_attributes(labels);
        self.query_duration.record(duration, &attrs);
    }

    fn increment_query_executions(&self, labels: &HashMap<&str, String>) {
        let attrs = self.labels_to_attributes(labels);
        self.query_executions.add(1, &attrs);
    }

    fn increment_slow_queries(&self, labels: &HashMap<&str, String>) {
        let attrs = self.labels_to_attributes(labels);
        self.slow_queries.add(1, &attrs);
    }

    fn increment_slow_queries_by(&self, labels: &HashMap<&str, String>, count: u64) {
        let attrs = self.labels_to_attributes(labels);
        self.slow_queries.add(count, &attrs);
    }

    fn record_query_plan_cost(&self, labels: &HashMap<&str, String>, cost: f64) {
        let attrs = self.labels_to_attributes(labels);
        self.query_plan_cost.record(cost, &attrs);
    }

    fn record_query_rows_examined(&self, labels: &HashMap<&str, String>, rows: f64) {
        let attrs = self.labels_to_attributes(labels);
        self.query_rows_examined.record(rows, &attrs);
    }

    fn record_database_avg_duration(&self, labels: &HashMap<&str, String>, duration: f64) {
        let attrs = self.labels_to_attributes(labels);
        self.database_avg_duration.record(duration, &attrs);
    }

    fn record_database_qps(&self, labels: &HashMap<&str, String>, qps: f64) {
        let attrs = self.labels_to_attributes(labels);
        self.database_qps.record(qps, &attrs);
    }

    fn increment_database_unique_queries(&self, labels: &HashMap<&str, String>, count: u64) {
        let attrs = self.labels_to_attributes(labels);
        self.database_unique_queries.add(count, &attrs);
    }

    fn increment_plan_node_type(&self, labels: &HashMap<&str, String>) {
        let attrs = self.labels_to_attributes(labels);
        self.plan_node_types.add(1, &attrs);
    }

    fn increment_scan_type(&self, labels: &HashMap<&str, String>) {
        let attrs = self.labels_to_attributes(labels);
        self.scan_types.add(1, &attrs);
    }

    fn increment_join_type(&self, labels: &HashMap<&str, String>) {
        let attrs = self.labels_to_attributes(labels);
        self.join_types.add(1, &attrs);
    }

    fn set_exporter_up(&self, up: i64) {
        self.exporter_up.record(up, &[]);
    }

    fn increment_logs_parsed(&self, labels: &HashMap<&str, String>) {
        let attrs = self.labels_to_attributes(labels);
        self.logs_parsed_total.add(1, &attrs);
    }

    fn increment_logs_parsed_by(&self, labels: &HashMap<&str, String>, count: u64) {
        let attrs = self.labels_to_attributes(labels);
        self.logs_parsed_total.add(count, &attrs);
    }

    fn increment_parse_errors(&self, labels: &HashMap<&str, String>) {
        let attrs = self.labels_to_attributes(labels);
        self.parse_errors_total.add(1, &attrs);
    }

    fn record_export_duration(&self, labels: &HashMap<&str, String>, duration: f64) {
        let attrs = self.labels_to_attributes(labels);
        self.export_duration.record(duration, &attrs);
    }

    fn set_memory_usage(&self, bytes: i64) {
        self.memory_usage.record(bytes, &[]);
    }

    fn set_last_successful_parse(&self, timestamp: i64) {
        self.last_successful_parse.record(timestamp, &[]);
    }

    fn set_query_latency_cv(&self, labels: &HashMap<&str, String>, cv: f64) {
        let attrs = self.labels_to_attributes(labels);
        self.query_latency_cv.record(cv, &attrs);
    }

    fn set_query_total_time_share_pct(&self, labels: &HashMap<&str, String>, pct: f64) {
        let attrs = self.labels_to_attributes(labels);
        self.query_total_time_share_pct.record(pct, &attrs);
    }

    fn set_query_latency_p95_ms(&self, labels: &HashMap<&str, String>, p95_ms: f64) {
        let attrs = self.labels_to_attributes(labels);
        self.query_latency_p95_ms.record(p95_ms, &attrs);
    }

    fn set_query_latency_p99_ms(&self, labels: &HashMap<&str, String>, p99_ms: f64) {
        let attrs = self.labels_to_attributes(labels);
        self.query_latency_p99_ms.record(p99_ms, &attrs);
    }

    fn set_query_first_seen_seconds(&self, labels: &HashMap<&str, String>, secs: f64) {
        let attrs = self.labels_to_attributes(labels);
        self.query_first_seen_seconds.record(secs, &attrs);
    }

    fn set_query_last_seen_seconds(&self, labels: &HashMap<&str, String>, secs: f64) {
        let attrs = self.labels_to_attributes(labels);
        self.query_last_seen_seconds.record(secs, &attrs);
    }

    fn set_query_info(&self, labels: &HashMap<&str, String>) {
        let attrs = self.labels_to_attributes(labels);
        self.query_info.record(1, &attrs);
    }

    fn shutdown(&self) -> Result<()> {
        if let Err(e) = self._meter_provider.shutdown() {
            tracing::error!("Failed to shutdown meter provider: {:?}", e);
        }
        Ok(())
    }
}
