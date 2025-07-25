use anyhow::Result;
use prometheus::{
    Counter, CounterVec, Histogram, HistogramOpts, HistogramVec, IntCounterVec, IntGauge, Opts,
    Registry, GaugeVec,
};
use std::collections::HashMap;

pub struct MetricsRegistry {
    pub registry: Registry,

    // Query identification
    pub query_info: GaugeVec,

    // Query performance metrics
    pub query_duration: HistogramVec,
    pub query_executions: CounterVec,
    pub slow_queries: CounterVec,
    pub query_buffer_hits: HistogramVec,
    pub query_shared_read_blocks: HistogramVec,
    pub query_shared_written_blocks: HistogramVec,
    pub query_temp_read_blocks: HistogramVec,
    pub query_temp_written_blocks: HistogramVec,

    // Query complexity metrics
    pub query_plan_cost: HistogramVec,
    pub query_rows_examined: HistogramVec,
    pub query_plan_depth: HistogramVec,
    pub query_plan_width: HistogramVec,
    pub query_node_count: HistogramVec,

    // Database aggregate metrics
    pub database_avg_duration: HistogramVec,
    pub database_queries_per_second: HistogramVec,
    pub database_unique_queries: IntCounterVec,

    // Plan analysis metrics
    pub plan_node_types: CounterVec,
    pub scan_types: CounterVec,
    pub join_types: CounterVec,

    // Table and index usage metrics
    pub table_access_total: CounterVec,
    pub table_scan_total: CounterVec,
    pub index_usage_total: CounterVec,
    pub index_scan_total: CounterVec,
    pub table_join_frequency: CounterVec,

    // Performance histograms
    pub query_startup_cost: HistogramVec,
    pub query_total_cost: HistogramVec,
    pub query_actual_rows: HistogramVec,
    pub query_loops: HistogramVec,

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

        // Query identification - using GaugeVec as info metric
        let query_info = GaugeVec::new(
            Opts::new(
                format!("{}_query_info", namespace),
                "Query information mapping hash to normalized query text",
            ),
            &["normalized_query_hash", "database", "normalized_query", "sample_query"],
        )?;

        // Query performance metrics
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
            &[
                "normalized_query_hash",
                "database",
                "status",
            ],
        )?;

        let slow_queries = CounterVec::new(
            Opts::new(
                format!("{}_slow_queries_total", namespace),
                "Total number of slow queries by threshold",
            ),
            &["database", "threshold"],
        )?;

        let query_buffer_hits = HistogramVec::new(
            HistogramOpts::new(
                format!("{}_query_buffer_hits", namespace),
                "Buffer hits per query execution",
            )
            .buckets(vec![0.0, 10.0, 100.0, 1000.0, 10000.0, 100000.0, 1000000.0]),
            &["normalized_query_hash", "database"],
        )?;

        let query_shared_read_blocks = HistogramVec::new(
            HistogramOpts::new(
                format!("{}_query_shared_read_blocks", namespace),
                "Shared blocks read per query",
            )
            .buckets(vec![0.0, 1.0, 10.0, 100.0, 1000.0, 10000.0, 100000.0]),
            &["normalized_query_hash", "database"],
        )?;

        let query_shared_written_blocks = HistogramVec::new(
            HistogramOpts::new(
                format!("{}_query_shared_written_blocks", namespace),
                "Shared blocks written per query",
            )
            .buckets(vec![0.0, 1.0, 10.0, 100.0, 1000.0, 10000.0, 100000.0]),
            &["normalized_query_hash", "database"],
        )?;

        let query_temp_read_blocks = HistogramVec::new(
            HistogramOpts::new(
                format!("{}_query_temp_read_blocks", namespace),
                "Temporary blocks read per query",
            )
            .buckets(vec![0.0, 1.0, 10.0, 100.0, 1000.0, 10000.0, 100000.0]),
            &["normalized_query_hash", "database"],
        )?;

        let query_temp_written_blocks = HistogramVec::new(
            HistogramOpts::new(
                format!("{}_query_temp_written_blocks", namespace),
                "Temporary blocks written per query",
            )
            .buckets(vec![0.0, 1.0, 10.0, 100.0, 1000.0, 10000.0, 100000.0]),
            &["normalized_query_hash", "database"],
        )?;

        // Query complexity metrics
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

        let query_plan_depth = HistogramVec::new(
            HistogramOpts::new(
                format!("{}_query_plan_depth", namespace),
                "Query plan tree depth",
            )
            .buckets(vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 8.0, 10.0, 15.0, 20.0]),
            &["normalized_query_hash", "database"],
        )?;

        let query_plan_width = HistogramVec::new(
            HistogramOpts::new(
                format!("{}_query_plan_width", namespace),
                "Query plan row width estimate",
            )
            .buckets(vec![8.0, 16.0, 32.0, 64.0, 128.0, 256.0, 512.0, 1024.0, 2048.0]),
            &["normalized_query_hash", "database"],
        )?;

        let query_node_count = HistogramVec::new(
            HistogramOpts::new(
                format!("{}_query_plan_node_count", namespace),
                "Number of nodes in query plan",
            )
            .buckets(vec![1.0, 2.0, 3.0, 5.0, 8.0, 12.0, 20.0, 30.0, 50.0, 100.0]),
            &["normalized_query_hash", "database"],
        )?;

        // Database aggregate metrics
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

        // Plan analysis metrics
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

        // Table and index usage metrics
        let table_access_total = CounterVec::new(
            Opts::new(
                format!("{}_table_access_total", namespace),
                "Total number of table accesses",
            ),
            &["schema_name", "table_name", "database"],
        )?;

        let table_scan_total = CounterVec::new(
            Opts::new(
                format!("{}_table_scan_total", namespace),
                "Total number of table scans by type",
            ),
            &["schema_name", "table_name", "scan_type", "database"],
        )?;

        let index_usage_total = CounterVec::new(
            Opts::new(
                format!("{}_index_usage_total", namespace),
                "Total number of index usages",
            ),
            &["schema_name", "table_name", "index_name", "database"],
        )?;

        let index_scan_total = CounterVec::new(
            Opts::new(
                format!("{}_index_scan_total", namespace),
                "Total number of index scans by type",
            ),
            &["schema_name", "table_name", "index_name", "scan_type", "database"],
        )?;

        let table_join_frequency = CounterVec::new(
            Opts::new(
                format!("{}_table_join_frequency_total", namespace),
                "Frequency of table joins",
            ),
            &["left_table", "right_table", "join_type", "database"],
        )?;

        // Performance histograms
        let query_startup_cost = HistogramVec::new(
            HistogramOpts::new(
                format!("{}_query_startup_cost", namespace),
                "Query plan startup cost",
            )
            .buckets(vec![0.0, 0.01, 0.1, 1.0, 10.0, 100.0, 1000.0, 10000.0]),
            &["normalized_query_hash", "database"],
        )?;

        let query_total_cost = HistogramVec::new(
            HistogramOpts::new(
                format!("{}_query_total_cost", namespace),
                "Query plan total cost",
            )
            .buckets(vec![0.01, 0.1, 1.0, 10.0, 100.0, 1000.0, 10000.0, 100000.0]),
            &["normalized_query_hash", "database"],
        )?;

        let query_actual_rows = HistogramVec::new(
            HistogramOpts::new(
                format!("{}_query_actual_rows", namespace),
                "Actual rows returned by query operations",
            )
            .buckets(vec![0.0, 1.0, 10.0, 100.0, 1000.0, 10000.0, 100000.0, 1000000.0]),
            &["normalized_query_hash", "database"],
        )?;

        let query_loops = HistogramVec::new(
            HistogramOpts::new(
                format!("{}_query_loops", namespace),
                "Number of loops in query execution",
            )
            .buckets(vec![1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 500.0, 1000.0]),
            &["normalized_query_hash", "database"],
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
        registry.register(Box::new(query_info.clone()))?;
        registry.register(Box::new(query_duration.clone()))?;
        registry.register(Box::new(query_executions.clone()))?;
        registry.register(Box::new(slow_queries.clone()))?;
        registry.register(Box::new(query_buffer_hits.clone()))?;
        registry.register(Box::new(query_shared_read_blocks.clone()))?;
        registry.register(Box::new(query_shared_written_blocks.clone()))?;
        registry.register(Box::new(query_temp_read_blocks.clone()))?;
        registry.register(Box::new(query_temp_written_blocks.clone()))?;
        registry.register(Box::new(query_plan_cost.clone()))?;
        registry.register(Box::new(query_rows_examined.clone()))?;
        registry.register(Box::new(query_plan_depth.clone()))?;
        registry.register(Box::new(query_plan_width.clone()))?;
        registry.register(Box::new(query_node_count.clone()))?;
        registry.register(Box::new(database_avg_duration.clone()))?;
        registry.register(Box::new(database_queries_per_second.clone()))?;
        registry.register(Box::new(database_unique_queries.clone()))?;
        registry.register(Box::new(plan_node_types.clone()))?;
        registry.register(Box::new(scan_types.clone()))?;
        registry.register(Box::new(join_types.clone()))?;
        registry.register(Box::new(table_access_total.clone()))?;
        registry.register(Box::new(table_scan_total.clone()))?;
        registry.register(Box::new(index_usage_total.clone()))?;
        registry.register(Box::new(index_scan_total.clone()))?;
        registry.register(Box::new(table_join_frequency.clone()))?;
        registry.register(Box::new(query_startup_cost.clone()))?;
        registry.register(Box::new(query_total_cost.clone()))?;
        registry.register(Box::new(query_actual_rows.clone()))?;
        registry.register(Box::new(query_loops.clone()))?;
        registry.register(Box::new(exporter_up.clone()))?;
        registry.register(Box::new(logs_parsed_total.clone()))?;
        registry.register(Box::new(parse_errors_total.clone()))?;
        registry.register(Box::new(export_duration.clone()))?;
        registry.register(Box::new(memory_usage.clone()))?;
        registry.register(Box::new(last_successful_parse.clone()))?;

        Ok(Self {
            registry,
            query_info,
            query_duration,
            query_executions,
            slow_queries,
            query_buffer_hits,
            query_shared_read_blocks,
            query_shared_written_blocks,
            query_temp_read_blocks,
            query_temp_written_blocks,
            query_plan_cost,
            query_rows_examined,
            query_plan_depth,
            query_plan_width,
            query_node_count,
            database_avg_duration,
            database_queries_per_second,
            database_unique_queries,
            plan_node_types,
            scan_types,
            join_types,
            table_access_total,
            table_scan_total,
            index_usage_total,
            index_scan_total,
            table_join_frequency,
            query_startup_cost,
            query_total_cost,
            query_actual_rows,
            query_loops,
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
