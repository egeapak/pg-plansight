use anyhow::Result;
use std::any::Any;
use std::collections::HashMap;

/// Unified metrics backend trait supporting both Prometheus and OpenTelemetry
pub trait MetricsBackend: Send + Sync {
    fn as_any(&self) -> &dyn Any;
    // Query performance metrics
    fn record_query_duration(&self, labels: &HashMap<&str, String>, duration: f64);
    fn increment_query_executions(&self, labels: &HashMap<&str, String>);
    fn increment_slow_queries(&self, labels: &HashMap<&str, String>);

    // Query complexity metrics
    fn record_query_plan_cost(&self, labels: &HashMap<&str, String>, cost: f64);
    fn record_query_rows_examined(&self, labels: &HashMap<&str, String>, rows: f64);

    // Database aggregate metrics
    fn record_database_avg_duration(&self, labels: &HashMap<&str, String>, duration: f64);
    fn record_database_qps(&self, labels: &HashMap<&str, String>, qps: f64);
    fn increment_database_unique_queries(&self, labels: &HashMap<&str, String>, count: u64);

    // Plan analysis metrics
    fn increment_plan_node_type(&self, labels: &HashMap<&str, String>);
    fn increment_scan_type(&self, labels: &HashMap<&str, String>);
    fn increment_join_type(&self, labels: &HashMap<&str, String>);

    // Exporter self-monitoring metrics
    fn set_exporter_up(&self, up: i64);
    fn increment_logs_parsed(&self, labels: &HashMap<&str, String>);
    fn increment_parse_errors(&self, labels: &HashMap<&str, String>);
    fn record_export_duration(&self, labels: &HashMap<&str, String>, duration: f64);
    fn set_memory_usage(&self, bytes: i64);
    fn set_last_successful_parse(&self, timestamp: i64);

    // Derived per-query metrics (F7)
    fn set_query_latency_cv(&self, labels: &HashMap<&str, String>, cv: f64);
    fn set_query_total_time_share_pct(&self, labels: &HashMap<&str, String>, pct: f64);
    fn set_query_latency_p95_ms(&self, labels: &HashMap<&str, String>, p95_ms: f64);
    fn set_query_latency_p99_ms(&self, labels: &HashMap<&str, String>, p99_ms: f64);

    // Lifecycle methods
    fn shutdown(&self) -> Result<()>;
}
