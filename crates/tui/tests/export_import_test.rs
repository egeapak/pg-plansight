use chrono::Utc;
use pg_loganalyze_core::{
    AnalysisExport, NodeType, ParsedPlan, PerformancePercentiles, PlanCost, PlanNode, PlanSource,
    ProcessedQuery, QueryGroupStatistics, QueryPlan,
};
use std::collections::HashMap;
use tempfile::NamedTempFile;

// Helper function to create a test QueryPlan
fn create_test_query_plan(
    original_query: &str,
    normalized_query: &str,
    plan_text: &str,
    duration_ms: f64,
) -> QueryPlan {
    let now = Utc::now();
    let source = PlanSource::Text {
        raw_text: plan_text.to_string(),
        plan_lines: Vec::new(),
    };

    let parsed = ParsedPlan {
        root: PlanNode::new(
            NodeType::Unknown("Result".to_string()),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 10.0,
                estimated_rows: 1,
                estimated_width: 100,
            },
            plan_text.to_string(),
        ),
        planning_time_ms: None,
        execution_time_ms: Some(duration_ms),
    };

    QueryPlan {
        timestamp: now,
        duration_ms,
        query_text: original_query.to_string(),
        normalized_query: normalized_query.to_string(),
        formatted_query: normalized_query.to_string(),
        source,
        parsed,
    }
}

// Helper function to create a test ProcessedQuery
fn create_test_processed_query(
    original_query: &str,
    normalized_query: &str,
    plan_text: &str,
    stats: QueryGroupStatistics,
) -> ProcessedQuery {
    let representative_plan = create_test_query_plan(
        original_query,
        normalized_query,
        plan_text,
        stats.max_duration_ms,
    );

    ProcessedQuery {
        representative_plan,
        statistics: stats,
        complexity_score: None,
        metadata: None,
        regression_analysis: None,
        plan_analysis: None,
        execution_indices: Vec::new(),
    }
}

#[test]
fn test_export_import_roundtrip() {
    // Create sample processed queries
    let mut queries = HashMap::new();

    let hash1 = format!("{:016x}", 12345u64);
    queries.insert(
        hash1.clone(),
        create_test_processed_query(
            "SELECT * FROM users WHERE id = $1",
            "SELECT * FROM users WHERE id = ?",
            "Seq Scan on users (cost=0.00..10.00 rows=1 width=100)",
            QueryGroupStatistics {
                count: 10,
                total_duration_ms: 100.0,
                min_duration_ms: 5.0,
                max_duration_ms: 20.0,
                mean_duration_ms: 10.0,
                std_dev_ms: 3.0,
                min_timestamp: Utc::now(),
                max_timestamp: Utc::now(),
                percentiles: PerformancePercentiles {
                    p25: 7.0,
                    p50: 10.0,
                    p90: 15.0,
                    p95: 18.0,
                    p99: 19.0,
                },
                hourly_histogram: HashMap::new(),
                executions: Vec::new(),
            },
        ),
    );

    let hash2 = format!("{:016x}", 67890u64);
    queries.insert(
        hash2.clone(),
        create_test_processed_query(
            "SELECT * FROM orders WHERE user_id = $1",
            "SELECT * FROM orders WHERE user_id = ?",
            "Index Scan using orders_user_id_idx on orders (cost=0.00..8.27 rows=1 width=50)",
            QueryGroupStatistics {
                count: 25,
                total_duration_ms: 250.0,
                min_duration_ms: 8.0,
                max_duration_ms: 15.0,
                mean_duration_ms: 10.0,
                std_dev_ms: 2.0,
                min_timestamp: Utc::now(),
                max_timestamp: Utc::now(),
                percentiles: PerformancePercentiles {
                    p25: 9.0,
                    p50: 10.0,
                    p90: 12.0,
                    p95: 13.0,
                    p99: 14.5,
                },
                hourly_histogram: HashMap::new(),
                executions: Vec::new(),
            },
        ),
    );

    // Export to file
    let export =
        AnalysisExport::from_processed_queries(queries.clone(), vec!["test.log".to_string()]);

    let temp_file = NamedTempFile::new().unwrap();
    export.to_file(temp_file.path()).unwrap();

    // Import back
    let imported = AnalysisExport::from_file(temp_file.path()).unwrap();

    // Verify export metadata
    assert_eq!(imported.query_count, 2);
    assert_eq!(imported.execution_count, 35); // 10 + 25
    assert_eq!(imported.queries.len(), 2);

    // Convert back to ProcessedQuery HashMap
    let restored_queries = imported.to_processed_queries();
    assert_eq!(restored_queries.len(), 2);
    assert!(restored_queries.contains_key(&hash1));
    assert!(restored_queries.contains_key(&hash2));

    // Verify query details
    let restored_query1 = &restored_queries[&hash1];
    assert_eq!(
        restored_query1.normalized_query(),
        "SELECT * FROM users WHERE id = ?"
    );
    assert_eq!(restored_query1.statistics.count, 10);
    assert_eq!(restored_query1.statistics.mean_duration_ms, 10.0);

    let restored_query2 = &restored_queries[&hash2];
    assert_eq!(
        restored_query2.normalized_query(),
        "SELECT * FROM orders WHERE user_id = ?"
    );
    assert_eq!(restored_query2.statistics.count, 25);
    assert_eq!(restored_query2.statistics.mean_duration_ms, 10.0);
}

#[test]
fn test_export_preserves_statistics() {
    let mut queries = HashMap::new();

    let hash = format!("{:016x}", 11111u64);
    let now = Utc::now();

    queries.insert(
        hash.clone(),
        create_test_processed_query(
            "SELECT COUNT(*) FROM products",
            "SELECT COUNT(*) FROM products",
            "Aggregate (cost=100.00..100.01 rows=1 width=8)",
            QueryGroupStatistics {
                count: 100,
                total_duration_ms: 1500.0,
                min_duration_ms: 10.0,
                max_duration_ms: 30.0,
                mean_duration_ms: 15.0,
                std_dev_ms: 4.5,
                min_timestamp: now,
                max_timestamp: now,
                percentiles: PerformancePercentiles {
                    p25: 12.0,
                    p50: 15.0,
                    p90: 20.0,
                    p95: 25.0,
                    p99: 28.0,
                },
                hourly_histogram: HashMap::new(),
                executions: Vec::new(),
            },
        ),
    );

    // Export and import
    let export = AnalysisExport::from_processed_queries(queries, vec!["test.log".to_string()]);
    let temp_file = NamedTempFile::new().unwrap();
    export.to_file(temp_file.path()).unwrap();
    let imported = AnalysisExport::from_file(temp_file.path()).unwrap();
    let restored = imported.to_processed_queries();

    // Verify all statistics are preserved
    let stats = &restored[&hash].statistics;
    assert_eq!(stats.count, 100);
    assert_eq!(stats.total_duration_ms, 1500.0);
    assert_eq!(stats.min_duration_ms, 10.0);
    assert_eq!(stats.max_duration_ms, 30.0);
    assert_eq!(stats.mean_duration_ms, 15.0);
    assert_eq!(stats.std_dev_ms, 4.5);

    // Verify percentiles
    assert_eq!(stats.percentiles.p25, 12.0);
    assert_eq!(stats.percentiles.p50, 15.0);
    assert_eq!(stats.percentiles.p90, 20.0);
    assert_eq!(stats.percentiles.p95, 25.0);
    assert_eq!(stats.percentiles.p99, 28.0);
}

#[test]
fn test_export_sorts_by_total_duration() {
    let mut queries = HashMap::new();

    // Add queries with different total durations
    queries.insert(
        format!("{:016x}", 1u64),
        create_test_processed_query(
            "SELECT 1",
            "SELECT ?",
            "Result",
            QueryGroupStatistics {
                count: 1,
                total_duration_ms: 50.0, // Lower
                min_duration_ms: 50.0,
                max_duration_ms: 50.0,
                mean_duration_ms: 50.0,
                std_dev_ms: 0.0,
                min_timestamp: Utc::now(),
                max_timestamp: Utc::now(),
                percentiles: PerformancePercentiles {
                    p25: 50.0,
                    p50: 50.0,
                    p90: 50.0,
                    p95: 50.0,
                    p99: 50.0,
                },
                hourly_histogram: HashMap::new(),
                executions: Vec::new(),
            },
        ),
    );

    queries.insert(
        format!("{:016x}", 2u64),
        create_test_processed_query(
            "SELECT 2",
            "SELECT ?",
            "Result",
            QueryGroupStatistics {
                count: 1,
                total_duration_ms: 200.0, // Higher
                min_duration_ms: 200.0,
                max_duration_ms: 200.0,
                mean_duration_ms: 200.0,
                std_dev_ms: 0.0,
                min_timestamp: Utc::now(),
                max_timestamp: Utc::now(),
                percentiles: PerformancePercentiles {
                    p25: 200.0,
                    p50: 200.0,
                    p90: 200.0,
                    p95: 200.0,
                    p99: 200.0,
                },
                hourly_histogram: HashMap::new(),
                executions: Vec::new(),
            },
        ),
    );

    let export = AnalysisExport::from_processed_queries(queries, vec!["test.log".to_string()]);

    // Verify queries are sorted by total duration (descending)
    assert!(
        export.queries[0].statistics.total_duration_ms
            > export.queries[1].statistics.total_duration_ms
    );
    assert_eq!(export.queries[0].statistics.total_duration_ms, 200.0);
    assert_eq!(export.queries[1].statistics.total_duration_ms, 50.0);
}

#[test]
fn test_import_nonexistent_file() {
    let result = AnalysisExport::from_file("/nonexistent/file.json");
    assert!(result.is_err());
}

#[test]
fn test_export_metadata() {
    let mut queries = HashMap::new();
    queries.insert(
        format!("{:016x}", 1u64),
        create_test_processed_query(
            "SELECT 1",
            "SELECT ?",
            "Result",
            QueryGroupStatistics {
                count: 1,
                total_duration_ms: 10.0,
                min_duration_ms: 10.0,
                max_duration_ms: 10.0,
                mean_duration_ms: 10.0,
                std_dev_ms: 0.0,
                min_timestamp: Utc::now(),
                max_timestamp: Utc::now(),
                percentiles: PerformancePercentiles {
                    p25: 10.0,
                    p50: 10.0,
                    p90: 10.0,
                    p95: 10.0,
                    p99: 10.0,
                },
                hourly_histogram: HashMap::new(),
                executions: Vec::new(),
            },
        ),
    );

    let export = AnalysisExport::from_processed_queries(
        queries,
        vec!["log1.log".to_string(), "log2.log".to_string()],
    );

    // Verify metadata
    assert_eq!(export.metadata.source_files.len(), 2);
    assert_eq!(export.metadata.source_files[0], "log1.log");
    assert_eq!(export.metadata.source_files[1], "log2.log");
    assert!(export.metadata.hostname.is_some());
}
