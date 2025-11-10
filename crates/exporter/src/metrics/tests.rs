#[cfg(test)]
#[allow(clippy::module_inception)]
mod tests {
    use crate::metrics::*;
    use std::collections::HashMap;

    #[cfg(feature = "prometheus")]
    #[test]
    fn test_prometheus_backend_creation() {
        let backend = PrometheusBackend::new("test", vec![0.1, 1.0, 10.0]).unwrap();

        // Test record duration
        let mut labels = HashMap::new();
        labels.insert("normalized_query_hash", "hash123".to_string());
        labels.insert("database", "testdb".to_string());
        labels.insert("query_timestamp", "2025-11-07".to_string());

        backend.record_query_duration(&labels, 5.5);
        backend.increment_query_executions(&labels);

        // Verify metrics exist
        let metrics = backend.registry.gather();
        assert!(
            metrics
                .iter()
                .any(|m| m.get_name() == "test_query_duration_seconds")
        );
        assert!(
            metrics
                .iter()
                .any(|m| m.get_name() == "test_query_executions_total")
        );
    }

    #[cfg(feature = "prometheus")]
    #[test]
    fn test_prometheus_backend_slow_queries() {
        let backend = PrometheusBackend::new("test", vec![1.0]).unwrap();

        let mut labels = HashMap::new();
        labels.insert("database", "prod".to_string());
        labels.insert("query_timestamp", "2025-11-07".to_string());
        labels.insert("threshold", "5s".to_string());

        backend.increment_slow_queries(&labels);
        backend.increment_slow_queries(&labels);

        let metrics = backend.registry.gather();
        let slow_metric = metrics
            .iter()
            .find(|m| m.get_name() == "test_slow_queries_total")
            .expect("slow_queries metric should exist");

        assert_eq!(
            slow_metric.get_field_type(),
            prometheus::proto::MetricType::COUNTER
        );
    }

    #[cfg(feature = "prometheus")]
    #[test]
    fn test_prometheus_backend_plan_metrics() {
        let backend = PrometheusBackend::new("test", vec![1.0]).unwrap();

        let mut labels = HashMap::new();
        labels.insert("normalized_query_hash", "hash001".to_string());
        labels.insert("database", "prod".to_string());
        labels.insert("query_timestamp", "2025-11-07".to_string());

        backend.record_query_plan_cost(&labels, 150.0);
        backend.record_query_rows_examined(&labels, 10000.0);

        let metrics = backend.registry.gather();
        assert!(
            metrics
                .iter()
                .any(|m| m.get_name() == "test_query_plan_cost")
        );
        assert!(
            metrics
                .iter()
                .any(|m| m.get_name() == "test_query_rows_examined")
        );
    }

    #[cfg(feature = "prometheus")]
    #[test]
    fn test_prometheus_backend_scan_types() {
        let backend = PrometheusBackend::new("test", vec![1.0]).unwrap();

        let mut labels = HashMap::new();
        labels.insert("scan_type", "seq_scan".to_string());
        labels.insert("database", "prod".to_string());
        labels.insert("query_timestamp", "2025-11-07".to_string());

        backend.increment_scan_type(&labels);

        labels.insert("scan_type", "index_scan".to_string());
        backend.increment_scan_type(&labels);

        let metrics = backend.registry.gather();
        assert!(
            metrics
                .iter()
                .any(|m| m.get_name() == "test_query_scan_types_total")
        );
    }

    #[cfg(feature = "prometheus")]
    #[test]
    fn test_prometheus_backend_self_monitoring() {
        let backend = PrometheusBackend::new("test", vec![0.01, 0.1]).unwrap();

        backend.set_exporter_up(1);
        backend.set_memory_usage(1024 * 1024 * 50); // 50 MB
        backend.set_last_successful_parse(1699300000);

        let mut labels = HashMap::new();
        labels.insert("file_path", "/var/log/pg.log".to_string());
        labels.insert("status", "success".to_string());
        backend.increment_logs_parsed(&labels);

        labels.insert("error_type", "parse_error".to_string());
        backend.increment_parse_errors(&labels);

        let metrics = backend.registry.gather();
        assert!(metrics.iter().any(|m| m.get_name() == "test_exporter_up"));
        assert!(
            metrics
                .iter()
                .any(|m| m.get_name() == "test_memory_usage_bytes")
        );
        assert!(
            metrics
                .iter()
                .any(|m| m.get_name() == "test_last_successful_parse_timestamp")
        );
    }

    #[cfg(feature = "opentelemetry")]
    #[tokio::test]
    async fn test_opentelemetry_backend_creation() {
        use opentelemetry_sdk::metrics::SdkMeterProvider;

        let meter_provider = SdkMeterProvider::builder().build();
        let backend = OpenTelemetryBackend::new(meter_provider, "test").unwrap();

        let mut labels = HashMap::new();
        labels.insert("normalized_query_hash", "hash123".to_string());
        labels.insert("database", "testdb".to_string());
        labels.insert("query_timestamp", "2025-11-07".to_string());

        // Should not panic
        backend.record_query_duration(&labels, 5.5);
        backend.increment_query_executions(&labels);
        backend.increment_slow_queries(&labels);
    }

    #[cfg(feature = "opentelemetry")]
    #[tokio::test]
    async fn test_opentelemetry_backend_plan_metrics() {
        use opentelemetry_sdk::metrics::SdkMeterProvider;

        let meter_provider = SdkMeterProvider::builder().build();
        let backend = OpenTelemetryBackend::new(meter_provider, "test").unwrap();

        let mut labels = HashMap::new();
        labels.insert("normalized_query_hash", "hash001".to_string());
        labels.insert("database", "prod".to_string());
        labels.insert("query_timestamp", "2025-11-07".to_string());

        // Should not panic
        backend.record_query_plan_cost(&labels, 150.0);
        backend.record_query_rows_examined(&labels, 10000.0);
    }

    #[cfg(feature = "opentelemetry")]
    #[tokio::test]
    async fn test_opentelemetry_backend_scan_and_join_types() {
        use opentelemetry_sdk::metrics::SdkMeterProvider;

        let meter_provider = SdkMeterProvider::builder().build();
        let backend = OpenTelemetryBackend::new(meter_provider, "test").unwrap();

        let mut labels = HashMap::new();
        labels.insert("scan_type", "seq_scan".to_string());
        labels.insert("database", "prod".to_string());
        labels.insert("query_timestamp", "2025-11-07".to_string());

        backend.increment_scan_type(&labels);

        labels.insert("join_type", "hash_join".to_string());
        backend.increment_join_type(&labels);

        // Should not panic
    }

    #[cfg(feature = "opentelemetry")]
    #[tokio::test]
    async fn test_opentelemetry_backend_self_monitoring() {
        use opentelemetry_sdk::metrics::SdkMeterProvider;

        let meter_provider = SdkMeterProvider::builder().build();
        let backend = OpenTelemetryBackend::new(meter_provider, "test").unwrap();

        backend.set_exporter_up(1);
        backend.set_memory_usage(1024 * 1024 * 50);
        backend.set_last_successful_parse(1699300000);

        let mut labels = HashMap::new();
        labels.insert("file_path", "/var/log/pg.log".to_string());
        labels.insert("status", "success".to_string());
        backend.increment_logs_parsed(&labels);

        labels.insert("operation", "collect".to_string());
        backend.record_export_duration(&labels, 1.5);

        // Should not panic
    }

    #[test]
    fn test_helper_functions() {
        // Test update_memory_usage (won't actually update on non-Linux or if /proc doesn't exist)
        #[cfg(feature = "prometheus")]
        {
            let backend = PrometheusBackend::new("test", vec![1.0]).unwrap();
            update_memory_usage(&backend);
            // Just verify it doesn't panic
        }

        // Test record_successful_parse
        #[cfg(feature = "prometheus")]
        {
            let backend = PrometheusBackend::new("test", vec![1.0]).unwrap();
            record_successful_parse(&backend);

            let metrics = backend.registry.gather();
            let timestamp_metric = metrics
                .iter()
                .find(|m| m.get_name() == "test_last_successful_parse_timestamp");
            assert!(timestamp_metric.is_some());
        }
    }

    #[cfg(feature = "prometheus")]
    #[test]
    fn test_metrics_backend_factory_prometheus() {
        let backend = create_metrics_backend(MetricsBackendType::Prometheus {
            namespace: "test".to_string(),
            histogram_buckets: vec![0.1, 1.0, 10.0],
        })
        .unwrap();

        let mut labels = HashMap::new();
        labels.insert("normalized_query_hash", "hash".to_string());
        labels.insert("database", "db".to_string());
        labels.insert("query_timestamp", "ts".to_string());

        backend.record_query_duration(&labels, 2.5);
        // Should not panic
    }

    #[test]
    fn test_trait_object_downcast() {
        #[cfg(feature = "prometheus")]
        {
            let backend = create_metrics_backend(MetricsBackendType::Prometheus {
                namespace: "test".to_string(),
                histogram_buckets: vec![1.0],
            })
            .unwrap();

            // Test as_any downcast
            let prometheus_backend = backend.as_any().downcast_ref::<PrometheusBackend>();
            assert!(prometheus_backend.is_some());
        }
    }
}
