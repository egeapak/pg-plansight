use super::traits::MetricsBackend;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;

/// Composite backend that forwards metrics to multiple backends simultaneously
pub struct CompositeBackend {
    backends: Vec<Arc<dyn MetricsBackend>>,
}

impl CompositeBackend {
    pub fn new(backends: Vec<Arc<dyn MetricsBackend>>) -> Self {
        Self { backends }
    }

    pub fn backends(&self) -> &[Arc<dyn MetricsBackend>] {
        &self.backends
    }
}

impl MetricsBackend for CompositeBackend {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn record_query_duration(&self, labels: &HashMap<&str, String>, duration: f64) {
        for backend in &self.backends {
            backend.record_query_duration(labels, duration);
        }
    }

    fn increment_query_executions(&self, labels: &HashMap<&str, String>) {
        for backend in &self.backends {
            backend.increment_query_executions(labels);
        }
    }

    fn increment_slow_queries(&self, labels: &HashMap<&str, String>) {
        for backend in &self.backends {
            backend.increment_slow_queries(labels);
        }
    }

    fn increment_slow_queries_by(&self, labels: &HashMap<&str, String>, count: u64) {
        for backend in &self.backends {
            backend.increment_slow_queries_by(labels, count);
        }
    }

    fn record_query_plan_cost(&self, labels: &HashMap<&str, String>, cost: f64) {
        for backend in &self.backends {
            backend.record_query_plan_cost(labels, cost);
        }
    }

    fn record_query_rows_examined(&self, labels: &HashMap<&str, String>, rows: f64) {
        for backend in &self.backends {
            backend.record_query_rows_examined(labels, rows);
        }
    }

    fn record_database_avg_duration(&self, labels: &HashMap<&str, String>, duration: f64) {
        for backend in &self.backends {
            backend.record_database_avg_duration(labels, duration);
        }
    }

    fn record_database_qps(&self, labels: &HashMap<&str, String>, qps: f64) {
        for backend in &self.backends {
            backend.record_database_qps(labels, qps);
        }
    }

    fn increment_database_unique_queries(&self, labels: &HashMap<&str, String>, count: u64) {
        for backend in &self.backends {
            backend.increment_database_unique_queries(labels, count);
        }
    }

    fn increment_plan_node_type(&self, labels: &HashMap<&str, String>) {
        for backend in &self.backends {
            backend.increment_plan_node_type(labels);
        }
    }

    fn increment_scan_type(&self, labels: &HashMap<&str, String>) {
        for backend in &self.backends {
            backend.increment_scan_type(labels);
        }
    }

    fn increment_join_type(&self, labels: &HashMap<&str, String>) {
        for backend in &self.backends {
            backend.increment_join_type(labels);
        }
    }

    fn set_exporter_up(&self, up: i64) {
        for backend in &self.backends {
            backend.set_exporter_up(up);
        }
    }

    fn increment_logs_parsed(&self, labels: &HashMap<&str, String>) {
        for backend in &self.backends {
            backend.increment_logs_parsed(labels);
        }
    }

    fn increment_logs_parsed_by(&self, labels: &HashMap<&str, String>, count: u64) {
        for backend in &self.backends {
            backend.increment_logs_parsed_by(labels, count);
        }
    }

    fn increment_parse_errors(&self, labels: &HashMap<&str, String>) {
        for backend in &self.backends {
            backend.increment_parse_errors(labels);
        }
    }

    fn record_export_duration(&self, labels: &HashMap<&str, String>, duration: f64) {
        for backend in &self.backends {
            backend.record_export_duration(labels, duration);
        }
    }

    fn set_memory_usage(&self, bytes: i64) {
        for backend in &self.backends {
            backend.set_memory_usage(bytes);
        }
    }

    fn set_last_successful_parse(&self, timestamp: i64) {
        for backend in &self.backends {
            backend.set_last_successful_parse(timestamp);
        }
    }

    fn set_query_latency_cv(&self, labels: &HashMap<&str, String>, cv: f64) {
        for backend in &self.backends {
            backend.set_query_latency_cv(labels, cv);
        }
    }

    fn set_query_total_time_share_pct(&self, labels: &HashMap<&str, String>, pct: f64) {
        for backend in &self.backends {
            backend.set_query_total_time_share_pct(labels, pct);
        }
    }

    fn set_query_latency_p95_ms(&self, labels: &HashMap<&str, String>, p95_ms: f64) {
        for backend in &self.backends {
            backend.set_query_latency_p95_ms(labels, p95_ms);
        }
    }

    fn set_query_latency_p99_ms(&self, labels: &HashMap<&str, String>, p99_ms: f64) {
        for backend in &self.backends {
            backend.set_query_latency_p99_ms(labels, p99_ms);
        }
    }

    fn set_query_first_seen_seconds(&self, labels: &HashMap<&str, String>, secs: f64) {
        for backend in &self.backends {
            backend.set_query_first_seen_seconds(labels, secs);
        }
    }

    fn set_query_last_seen_seconds(&self, labels: &HashMap<&str, String>, secs: f64) {
        for backend in &self.backends {
            backend.set_query_last_seen_seconds(labels, secs);
        }
    }

    fn shutdown(&self) -> Result<()> {
        for backend in &self.backends {
            backend.shutdown()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "prometheus")]
    #[test]
    fn test_composite_backend_with_prometheus() {
        use crate::metrics::PrometheusBackend;

        let backend1 = Arc::new(PrometheusBackend::new("test1", vec![1.0]).unwrap());
        let backend2 = Arc::new(PrometheusBackend::new("test2", vec![1.0]).unwrap());

        let composite = CompositeBackend::new(vec![backend1.clone(), backend2.clone()]);

        let mut labels = HashMap::new();
        labels.insert("normalized_query_hash", "hash".to_string());
        labels.insert("database", "db".to_string());

        composite.record_query_duration(&labels, 5.5);

        // Verify both backends received the metric
        let metrics1 = backend1.registry.gather();
        let metrics2 = backend2.registry.gather();

        assert!(
            metrics1
                .iter()
                .any(|m| m.name() == "test1_query_duration_seconds")
        );
        assert!(
            metrics2
                .iter()
                .any(|m| m.name() == "test2_query_duration_seconds")
        );
    }

    #[test]
    fn test_composite_backend_empty() {
        let composite = CompositeBackend::new(vec![]);

        let mut labels = HashMap::new();
        labels.insert("test", "value".to_string());

        // Should not panic with empty backends
        composite.record_query_duration(&labels, 1.0);
        composite.set_exporter_up(1);
    }
}
