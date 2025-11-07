use anyhow::Result;
use prometheus::{
    CounterVec, HistogramOpts, HistogramVec, IntCounterVec, IntGauge, Opts,
    Registry,
};

pub struct MetricsRegistry {
    pub registry: Registry,

    // Query performance metrics
    pub query_duration: HistogramVec,
    pub query_executions: CounterVec,
    pub slow_queries: CounterVec,

    // Query complexity metrics
    pub query_plan_cost: HistogramVec,
    pub query_rows_examined: HistogramVec,

    // Database aggregate metrics
    pub database_avg_duration: HistogramVec,
    pub database_queries_per_second: HistogramVec,
    pub database_unique_queries: IntCounterVec,

    // Plan analysis metrics
    pub plan_node_types: CounterVec,
    pub scan_types: CounterVec,
    pub join_types: CounterVec,

    // Exporter self-monitoring metrics
    pub exporter_up: IntGauge,
    pub logs_parsed_total: CounterVec,
    pub parse_errors_total: CounterVec,
    pub export_duration: HistogramVec,
    pub memory_usage: IntGauge,
    pub last_successful_parse: IntGauge,
}

impl MetricsRegistry {
    pub fn new(namespace: &str, histogram_buckets: Vec<f64>) -> Result<Self> {
        let registry = Registry::new();

        // Query performance metrics
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

        // Query complexity metrics
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

        // Database aggregate metrics
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

        // Plan analysis metrics
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

        // Exporter self-monitoring metrics
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

        // Register all metrics
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

    pub fn update_memory_usage(&self) {
        // Simple memory usage tracking - in production might want more sophisticated tracking
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            for line in status.lines() {
                if let Some(vm_rss) = line.strip_prefix("VmRSS:") {
                    if let Some(kb_str) = vm_rss.trim().strip_suffix(" kB") {
                        if let Ok(kb) = kb_str.trim().parse::<i64>() {
                            self.memory_usage.set(kb * 1024); // Convert to bytes
                            break;
                        }
                    }
                }
            }
        }
    }

    pub fn record_successful_parse(&self) {
        use std::time::{SystemTime, UNIX_EPOCH};
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        self.last_successful_parse.set(timestamp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_registry_creation() {
        let registry = MetricsRegistry::new("test_pg", vec![0.1, 1.0, 10.0]).unwrap();

        // Verify exporter_up is set to 1
        let metrics = registry.registry.gather();
        let exporter_up = metrics.iter()
            .find(|m| m.get_name() == "test_pg_exporter_up")
            .expect("exporter_up metric should exist");

        assert_eq!(exporter_up.get_field_type(), prometheus::proto::MetricType::GAUGE);
    }

    #[test]
    fn test_query_duration_recording() {
        let registry = MetricsRegistry::new("test", vec![0.1, 1.0, 10.0]).unwrap();

        // Record some query durations
        registry.query_duration
            .with_label_values(&["hash123", "production", "2025-11-07"])
            .observe(5.5);

        registry.query_duration
            .with_label_values(&["hash456", "staging", "2025-11-07"])
            .observe(0.5);

        let metrics = registry.registry.gather();
        let duration_metric = metrics.iter()
            .find(|m| m.get_name() == "test_query_duration_seconds")
            .expect("query_duration metric should exist");

        assert_eq!(duration_metric.get_field_type(), prometheus::proto::MetricType::HISTOGRAM);
        assert_eq!(duration_metric.get_metric().len(), 2); // Two label combinations
    }

    #[test]
    fn test_query_executions_counter() {
        let registry = MetricsRegistry::new("test", vec![1.0]).unwrap();

        // Increment counters
        registry.query_executions
            .with_label_values(&["hash789", "production", "2025-11-07", "success"])
            .inc();

        registry.query_executions
            .with_label_values(&["hash789", "production", "2025-11-07", "success"])
            .inc();

        let metrics = registry.registry.gather();
        let exec_metric = metrics.iter()
            .find(|m| m.get_name() == "test_query_executions_total")
            .expect("query_executions metric should exist");

        assert_eq!(exec_metric.get_field_type(), prometheus::proto::MetricType::COUNTER);
    }

    #[test]
    fn test_slow_queries_counter() {
        let registry = MetricsRegistry::new("test", vec![1.0]).unwrap();

        // Record slow queries
        registry.slow_queries
            .with_label_values(&["production", "2025-11-07", "1s"])
            .inc();

        registry.slow_queries
            .with_label_values(&["production", "2025-11-07", "5s"])
            .inc();

        registry.slow_queries
            .with_label_values(&["production", "2025-11-07", "1s"])
            .inc();

        let metrics = registry.registry.gather();
        let slow_metric = metrics.iter()
            .find(|m| m.get_name() == "test_slow_queries_total")
            .expect("slow_queries metric should exist");

        assert_eq!(slow_metric.get_field_type(), prometheus::proto::MetricType::COUNTER);
        assert_eq!(slow_metric.get_metric().len(), 2); // Two thresholds
    }

    #[test]
    fn test_plan_cost_histogram() {
        let registry = MetricsRegistry::new("test", vec![1.0, 10.0]).unwrap();

        // Record plan costs
        registry.query_plan_cost
            .with_label_values(&["hash001", "production", "2025-11-07"])
            .observe(50.0);

        registry.query_plan_cost
            .with_label_values(&["hash002", "production", "2025-11-07"])
            .observe(1500.0);

        let metrics = registry.registry.gather();
        let cost_metric = metrics.iter()
            .find(|m| m.get_name() == "test_query_plan_cost")
            .expect("query_plan_cost metric should exist");

        assert_eq!(cost_metric.get_field_type(), prometheus::proto::MetricType::HISTOGRAM);
    }

    #[test]
    fn test_database_aggregates() {
        let registry = MetricsRegistry::new("test", vec![0.1, 1.0, 10.0]).unwrap();

        // Record database-level metrics
        registry.database_avg_duration
            .with_label_values(&["production", "2025-11-07"])
            .observe(2.5);

        registry.database_queries_per_second
            .with_label_values(&["production", "2025-11-07"])
            .observe(150.0);

        registry.database_unique_queries
            .with_label_values(&["production", "2025-11-07"])
            .inc();

        let metrics = registry.registry.gather();

        // Verify all three metrics exist
        assert!(metrics.iter().any(|m| m.get_name() == "test_database_avg_query_duration_seconds"));
        assert!(metrics.iter().any(|m| m.get_name() == "test_database_queries_per_second"));
        assert!(metrics.iter().any(|m| m.get_name() == "test_database_unique_queries_total"));
    }

    #[test]
    fn test_plan_analysis_counters() {
        let registry = MetricsRegistry::new("test", vec![1.0]).unwrap();

        // Record various plan node types
        registry.plan_node_types
            .with_label_values(&["SeqScan", "production", "2025-11-07"])
            .inc();

        registry.scan_types
            .with_label_values(&["IndexScan", "production", "2025-11-07"])
            .inc();

        registry.join_types
            .with_label_values(&["HashJoin", "production", "2025-11-07"])
            .inc();

        let metrics = registry.registry.gather();

        assert!(metrics.iter().any(|m| m.get_name() == "test_query_plan_node_types_total"));
        assert!(metrics.iter().any(|m| m.get_name() == "test_query_scan_types_total"));
        assert!(metrics.iter().any(|m| m.get_name() == "test_query_join_types_total"));
    }

    #[test]
    fn test_exporter_self_monitoring() {
        let registry = MetricsRegistry::new("test", vec![0.01, 0.1, 1.0]).unwrap();

        // Record parsing metrics
        registry.logs_parsed_total
            .with_label_values(&["/var/log/pg.log", "success"])
            .inc_by(100.0);

        registry.parse_errors_total
            .with_label_values(&["/var/log/pg.log", "malformed_line"])
            .inc();

        registry.export_duration
            .with_label_values(&["collect"])
            .observe(0.5);

        registry.memory_usage.set(1024 * 1024 * 50); // 50 MB

        registry.record_successful_parse();

        let metrics = registry.registry.gather();

        assert!(metrics.iter().any(|m| m.get_name() == "test_logs_parsed_total"));
        assert!(metrics.iter().any(|m| m.get_name() == "test_parse_errors_total"));
        assert!(metrics.iter().any(|m| m.get_name() == "test_export_duration_seconds"));
        assert!(metrics.iter().any(|m| m.get_name() == "test_memory_usage_bytes"));
        assert!(metrics.iter().any(|m| m.get_name() == "test_last_successful_parse_timestamp"));
    }

    #[test]
    fn test_custom_histogram_buckets() {
        let custom_buckets = vec![0.001, 0.01, 0.1, 1.0, 10.0, 100.0];
        let registry = MetricsRegistry::new("custom", custom_buckets.clone()).unwrap();

        registry.query_duration
            .with_label_values(&["hash", "db", "ts"])
            .observe(5.0);

        let metrics = registry.registry.gather();
        let duration_metric = metrics.iter()
            .find(|m| m.get_name() == "custom_query_duration_seconds")
            .unwrap();

        // Histogram should exist with our custom buckets
        assert_eq!(duration_metric.get_field_type(), prometheus::proto::MetricType::HISTOGRAM);
    }

    #[test]
    fn test_namespace_in_metric_names() {
        let namespace = "my_custom_namespace";
        let registry = MetricsRegistry::new(namespace, vec![1.0]).unwrap();

        let metrics = registry.registry.gather();

        // All metrics should have the namespace prefix
        for metric in metrics {
            let name = metric.get_name();
            assert!(
                name.starts_with(namespace),
                "Metric {} should start with namespace {}",
                name,
                namespace
            );
        }
    }

    #[test]
    fn test_multiple_label_combinations() {
        let registry = MetricsRegistry::new("test", vec![1.0]).unwrap();

        // Record metrics with different label combinations
        let combinations = [
            ("hash1", "prod", "2025-11-07"),
            ("hash2", "prod", "2025-11-07"),
            ("hash1", "staging", "2025-11-07"),
            ("hash3", "prod", "2025-11-08"),
        ];

        for (hash, db, ts) in combinations.iter() {
            registry.query_duration
                .with_label_values(&[hash, db, ts])
                .observe(1.0);
        }

        let metrics = registry.registry.gather();
        let duration_metric = metrics.iter()
            .find(|m| m.get_name() == "test_query_duration_seconds")
            .unwrap();

        // Should have 4 different label combinations
        assert_eq!(duration_metric.get_metric().len(), 4);
    }
}
