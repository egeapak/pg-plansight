//! Comprehensive tests for SQL analysis features
//! 
//! This module contains extensive tests for all Phase 2 advanced analysis features
//! including complexity scoring, metadata extraction, and regression detection.

use chrono::{TimeZone, Utc};

#[cfg(test)]
mod complexity_tests {
    use crate::sql_analysis::complexity::*;

    #[test]
    fn test_simple_query_complexity_scoring() {
        let analyzer = ComplexityAnalyzer::new();
        let sql = "SELECT id, name FROM users WHERE active = true";
        let result = analyzer.analyze(sql).unwrap();

        assert_eq!(result.classification, ComplexityClass::Simple);
        assert!(result.total_score < 20.0);
        assert_eq!(result.breakdown.table_count, 1);
        assert_eq!(result.breakdown.join_info.total_joins, 0);
        assert_eq!(result.breakdown.subquery_info.total_subqueries, 0);
        assert!(result.components.join_complexity < 5.0);
    }

    #[test]
    fn test_moderate_complexity_query() {
        let analyzer = ComplexityAnalyzer::new();
        let sql = r#"
            SELECT u.name, COUNT(o.id) as order_count
            FROM users u 
            LEFT JOIN orders o ON u.id = o.user_id
            WHERE u.created_at > '2024-01-01'
              AND u.status = 'active'
            GROUP BY u.name
            HAVING COUNT(o.id) > 5
            ORDER BY order_count DESC
        "#;
        let result = analyzer.analyze(sql).unwrap();

        assert!(matches!(result.classification, ComplexityClass::Moderate | ComplexityClass::Complex));
        assert!(result.total_score >= 25.0);
        assert_eq!(result.breakdown.table_count, 2);
        assert_eq!(result.breakdown.join_info.total_joins, 1);
        assert_eq!(result.breakdown.join_info.outer_joins, 1);
        assert!(result.components.join_complexity > 0.0);
        assert!(result.components.aggregation_complexity > 0.0);
    }

    #[test]
    fn test_complex_query_with_subqueries() {
        let analyzer = ComplexityAnalyzer::new();
        let sql = r#"
            SELECT 
                u.name,
                u.email,
                (SELECT COUNT(*) FROM orders WHERE user_id = u.id) as order_count,
                (SELECT AVG(total) FROM orders WHERE user_id = u.id) as avg_order_total
            FROM users u
            WHERE EXISTS (
                SELECT 1 FROM orders o 
                WHERE o.user_id = u.id 
                  AND o.total > (SELECT AVG(total) FROM orders)
            )
            ORDER BY order_count DESC
        "#;
        let result = analyzer.analyze(sql).unwrap();

        assert!(matches!(result.classification, ComplexityClass::Complex | ComplexityClass::VeryComplex));
        assert!(result.total_score >= 40.0);
        assert_eq!(result.breakdown.subquery_info.total_subqueries, 4);
        assert_eq!(result.breakdown.subquery_info.exists_subqueries, 1);
        assert!(result.breakdown.subquery_info.max_nesting_level >= 2);
        assert!(result.components.subquery_complexity > 10.0);
        assert!(result.components.function_complexity > 0.0);
    }

    #[test]
    fn test_very_complex_query_scoring() {
        let analyzer = ComplexityAnalyzer::new();
        let sql = r#"
            WITH monthly_sales AS (
                SELECT 
                    u.id,
                    u.name,
                    DATE_TRUNC('month', o.created_at) as month,
                    SUM(o.total) as monthly_total,
                    ROW_NUMBER() OVER (PARTITION BY u.id ORDER BY SUM(o.total) DESC) as rank
                FROM users u
                JOIN orders o ON u.id = o.user_id
                JOIN order_items oi ON o.id = oi.order_id
                JOIN products p ON oi.product_id = p.id
                WHERE o.status = 'completed'
                  AND p.category IN ('electronics', 'books', 'clothing')
                  AND o.created_at >= '2024-01-01'
                GROUP BY u.id, u.name, DATE_TRUNC('month', o.created_at)
                HAVING SUM(o.total) > 1000
            ),
            top_customers AS (
                SELECT ms.*, 
                       LAG(monthly_total) OVER (PARTITION BY id ORDER BY month) as prev_month
                FROM monthly_sales ms
                WHERE rank <= 10
            )
            SELECT 
                tc.name,
                tc.monthly_total,
                tc.prev_month,
                CASE 
                    WHEN tc.prev_month IS NULL THEN 'new'
                    WHEN tc.monthly_total > tc.prev_month * 1.2 THEN 'growing'
                    WHEN tc.monthly_total < tc.prev_month * 0.8 THEN 'declining'
                    ELSE 'stable'
                END as trend
            FROM top_customers tc
            WHERE EXISTS (
                SELECT 1 FROM orders o2 
                WHERE o2.user_id = tc.id 
                  AND o2.created_at >= CURRENT_DATE - INTERVAL '30 days'
            )
            ORDER BY tc.monthly_total DESC, tc.month DESC
        "#;
        let result = analyzer.analyze(sql).unwrap();

        assert_eq!(result.classification, ComplexityClass::VeryComplex);
        assert!(result.total_score >= 60.0);
        assert!(result.breakdown.table_count >= 4);
        assert!(result.breakdown.join_info.total_joins >= 3);
        assert!(result.breakdown.function_info.window_functions >= 2);
        assert!(result.breakdown.condition_info.case_statements >= 1);
        assert!(result.components.window_complexity > 5.0);
        assert!(result.components.function_complexity > 5.0);
    }

    #[test]
    fn test_join_complexity_scoring() {
        let analyzer = ComplexityAnalyzer::new();
        let sql = r#"
            SELECT *
            FROM table1 t1
            INNER JOIN table2 t2 ON t1.id = t2.t1_id
            LEFT JOIN table3 t3 ON t1.id = t3.t1_id
            RIGHT JOIN table4 t4 ON t2.id = t4.t2_id
            CROSS JOIN table5 t5
        "#;
        let result = analyzer.analyze(sql).unwrap();

        assert_eq!(result.breakdown.join_info.total_joins, 4);
        assert_eq!(result.breakdown.join_info.inner_joins, 1);
        assert_eq!(result.breakdown.join_info.outer_joins, 2); // LEFT + RIGHT
        assert_eq!(result.breakdown.join_info.cross_joins, 1);
        assert!(result.components.join_complexity >= 15.0); // High due to cross join
    }

    #[test]
    fn test_function_complexity_analysis() {
        let analyzer = ComplexityAnalyzer::new();
        let sql = r#"
            SELECT 
                COUNT(*) as total,
                AVG(amount) as avg_amount,
                SUM(CASE WHEN amount > 100 THEN 1 ELSE 0 END) as high_amount_count,
                SUBSTRING(name, 1, 10) as short_name,
                EXTRACT(YEAR FROM created_at) as year,
                ROW_NUMBER() OVER (ORDER BY amount DESC) as rank
            FROM transactions
            GROUP BY EXTRACT(YEAR FROM created_at), SUBSTRING(name, 1, 10)
        "#;
        let result = analyzer.analyze(sql).unwrap();

        assert!(result.breakdown.function_info.total_functions >= 7);
        assert!(result.breakdown.function_info.aggregate_functions >= 3);
        assert!(result.breakdown.function_info.window_functions >= 1);
        assert!(result.breakdown.function_info.unique_functions.len() >= 6);
        assert!(result.components.function_complexity > 5.0);
        assert!(result.components.window_complexity > 0.0);
    }

    #[test]
    fn test_malformed_sql_handling() {
        let analyzer = ComplexityAnalyzer::new();
        let malformed_sql = "SELECT * FROM users WHERE incomplete AND";
        let result = analyzer.analyze(malformed_sql).unwrap();

        // Should return simple score for malformed SQL
        assert_eq!(result.classification, ComplexityClass::Simple);
        assert_eq!(result.total_score, 0.0);
    }
}

#[cfg(test)]
mod metadata_tests {
    use crate::sql_analysis::metadata::*;

    #[test]
    fn test_simple_select_metadata_extraction() {
        let extractor = MetadataExtractor::new();
        let sql = "SELECT id, name, email FROM users WHERE active = true AND created_at > '2024-01-01'";
        let result = extractor.extract(sql).unwrap();

        assert_eq!(result.operation, QueryOperation::Select);
        assert_eq!(result.table_references.len(), 1);
        assert_eq!(result.table_references[0].table, "users");
        assert_eq!(result.table_references[0].access_type, TableAccessType::Primary);

        // Should have columns: id, name, email (selected) + active, created_at (filtered)
        assert!(result.column_references.len() >= 5);
        
        let selected_columns: Vec<_> = result.column_references.iter()
            .filter(|c| matches!(c.usage, ColumnUsage::Selected))
            .map(|c| &c.column)
            .collect();
        assert!(selected_columns.contains(&&"id".to_string()));
        assert!(selected_columns.contains(&&"name".to_string()));
        assert!(selected_columns.contains(&&"email".to_string()));

        let filtered_columns: Vec<_> = result.column_references.iter()
            .filter(|c| matches!(c.usage, ColumnUsage::Filtered))
            .map(|c| &c.column)
            .collect();
        assert!(filtered_columns.contains(&&"active".to_string()));
        assert!(filtered_columns.contains(&&"created_at".to_string()));
    }

    #[test]
    fn test_join_query_metadata() {
        let extractor = MetadataExtractor::new();
        let sql = r#"
            SELECT u.name, u.email, p.title, p.description
            FROM users u
            INNER JOIN profiles p ON u.id = p.user_id
            LEFT JOIN addresses a ON u.id = a.user_id
            WHERE u.active = true
              AND p.is_public = true
            ORDER BY u.created_at DESC
        "#;
        let result = extractor.extract(sql).unwrap();

        assert_eq!(result.table_references.len(), 3);
        
        let primary_tables = result.table_references.iter()
            .filter(|t| matches!(t.access_type, TableAccessType::Primary))
            .count();
        assert_eq!(primary_tables, 1);

        let joined_tables = result.table_references.iter()
            .filter(|t| matches!(t.access_type, TableAccessType::Joined))
            .count();
        assert_eq!(joined_tables, 2);

        // Check for join conditions in column usage
        let join_columns = result.column_references.iter()
            .filter(|c| matches!(c.usage, ColumnUsage::Joined))
            .count();
        assert!(join_columns >= 4); // u.id, p.user_id, u.id, a.user_id

        // Check for ordered columns
        let ordered_columns = result.column_references.iter()
            .filter(|c| matches!(c.usage, ColumnUsage::Ordered))
            .count();
        assert_eq!(ordered_columns, 1);
    }

    #[test]
    fn test_aggregate_query_metadata() {
        let extractor = MetadataExtractor::new();
        let sql = r#"
            SELECT 
                department,
                COUNT(*) as employee_count,
                AVG(salary) as avg_salary,
                SUM(salary) as total_salary,
                MAX(hire_date) as latest_hire
            FROM employees
            WHERE active = true
              AND hire_date >= '2020-01-01'
            GROUP BY department
            HAVING COUNT(*) > 5 AND AVG(salary) > 50000
            ORDER BY avg_salary DESC
        "#;
        let result = extractor.extract(sql).unwrap();

        assert!(result.function_references.len() >= 5);
        
        let aggregate_functions = result.function_references.iter()
            .filter(|f| matches!(f.category, FunctionCategory::Aggregate))
            .count();
        assert_eq!(aggregate_functions, 5); // COUNT, AVG, SUM, MAX, AVG (in HAVING)

        let grouped_columns = result.column_references.iter()
            .filter(|c| matches!(c.usage, ColumnUsage::Grouped))
            .count();
        assert_eq!(grouped_columns, 1); // department

        let aggregated_columns = result.column_references.iter()
            .filter(|c| matches!(c.usage, ColumnUsage::Aggregated))
            .count();
        assert!(aggregated_columns >= 3); // salary (multiple times), hire_date
    }

    #[test]
    fn test_complex_subquery_metadata() {
        let extractor = MetadataExtractor::new();
        let sql = r#"
            SELECT 
                u.name,
                (SELECT COUNT(*) FROM orders WHERE user_id = u.id) as order_count,
                (SELECT AVG(total) FROM orders WHERE user_id = u.id) as avg_order
            FROM users u
            WHERE EXISTS (
                SELECT 1 FROM orders o 
                WHERE o.user_id = u.id 
                  AND o.total > 100
            )
            AND u.id IN (
                SELECT DISTINCT user_id 
                FROM transactions 
                WHERE amount > 50
            )
        "#;
        let result = extractor.extract(sql).unwrap();

        // Should detect multiple table references including subqueries
        let table_count = result.table_references.len();
        assert!(table_count >= 3); // users, orders (multiple times), transactions

        // Should detect various function usages
        let function_count = result.function_references.len();
        assert!(function_count >= 3); // COUNT, AVG, DISTINCT

        // Check execution pattern
        assert!(!result.execution_pattern.likely_full_scan); // Has filtering conditions
        assert!(result.execution_pattern.estimated_selectivity < 0.5); // Multiple filters
    }

    #[test]
    fn test_workload_classification() {
        let extractor = MetadataExtractor::new();
        
        // OLTP query
        let oltp_sql = "SELECT * FROM users WHERE id = 123";
        let oltp_result = extractor.extract(oltp_sql).unwrap();
        assert_eq!(oltp_result.classification.workload_type, WorkloadType::OLTP);

        // OLAP query
        let olap_sql = r#"
            SELECT 
                region, 
                product_category,
                COUNT(*) as orders,
                SUM(amount) as total_revenue,
                AVG(amount) as avg_order_value
            FROM orders o
            JOIN customers c ON o.customer_id = c.id
            JOIN products p ON o.product_id = p.id
            JOIN regions r ON c.region_id = r.id
            WHERE o.order_date >= '2024-01-01'
            GROUP BY region, product_category
            ORDER BY total_revenue DESC
        "#;
        let olap_result = extractor.extract(olap_sql).unwrap();
        assert_eq!(olap_result.classification.workload_type, WorkloadType::OLAP);

        // Reporting query
        let report_sql = r#"
            SELECT 
                DATE_TRUNC('month', created_at) as month,
                COUNT(*) as user_signups,
                COUNT(CASE WHEN active = true THEN 1 END) as active_users
            FROM users
            WHERE created_at >= '2024-01-01'
            GROUP BY DATE_TRUNC('month', created_at)
            ORDER BY month
        "#;
        let report_result = extractor.extract(report_sql).unwrap();
        assert_eq!(report_result.classification.workload_type, WorkloadType::Reporting);
    }

    #[test]
    fn test_performance_hints_generation() {
        let extractor = MetadataExtractor::new();
        let sql = r#"
            SELECT * 
            FROM large_table lt1
            JOIN large_table lt2 ON lt1.unindexed_col = lt2.unindexed_col
            JOIN large_table lt3 ON lt2.another_unindexed = lt3.another_unindexed
            WHERE lt1.filter_column = 'some_value'
              AND lt2.date_column > '2024-01-01'
              AND lt3.status_column IN ('active', 'pending', 'processing')
        "#;
        let result = extractor.extract(sql).unwrap();

        assert!(!result.performance_hints.is_empty());
        
        let indexing_hints = result.performance_hints.iter()
            .filter(|h| matches!(h.category, HintCategory::Indexing))
            .count();
        assert!(indexing_hints > 0);

        let query_rewrite_hints = result.performance_hints.iter()
            .filter(|h| matches!(h.category, HintCategory::QueryRewrite))
            .count();
        assert!(query_rewrite_hints > 0); // Should suggest breaking down complex joins
    }

    #[test]
    fn test_index_hint_generation() {
        let extractor = MetadataExtractor::new();
        let sql = "SELECT * FROM users WHERE email = 'test@example.com' AND status = 'active' AND created_at > '2024-01-01'";
        let result = extractor.extract(sql).unwrap();

        assert!(!result.execution_pattern.index_hints.is_empty());
        let hint = &result.execution_pattern.index_hints[0];
        assert_eq!(hint.table, "users");
        assert!(hint.columns.len() >= 3);
        assert_eq!(hint.index_type, IndexType::BTree);
    }
}

#[cfg(test)]
mod regression_tests {
    use super::*;
    use crate::sql_analysis::regression::*;

    fn create_test_performance_data(
        base_time: f64,
        trend: f64,
        noise: f64,
        count: usize,
    ) -> Vec<PerformanceDataPoint> {
        let start_time = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        (0..count)
            .map(|i| {
                let time_factor = i as f64;
                let execution_time = base_time + trend * time_factor + noise * (i % 10) as f64;
                PerformanceDataPoint {
                    timestamp: start_time + chrono::Duration::hours(i as i64),
                    execution_time_ms: execution_time,
                    memory_usage_mb: Some(100.0 + execution_time * 0.1),
                    cpu_usage_percent: Some(20.0 + execution_time * 0.05),
                    io_operations: Some((execution_time * 10.0) as u64),
                    cache_hit_ratio: Some(0.95 - execution_time * 0.001),
                }
            })
            .collect()
    }

    #[test]
    fn test_no_regression_stable_performance() {
        let detector = RegressionDetector::new();
        let data = create_test_performance_data(100.0, 0.0, 2.0, 100);
        let result = detector.analyze(&data).unwrap();

        assert_eq!(result.status, RegressionStatus::None);
        assert!(result.metric_regressions.is_empty());
        assert_eq!(result.temporal_analysis.trend, TrendDirection::Stable);
        assert!(result.temporal_analysis.trend_strength < 0.3);
        assert!(matches!(result.confidence_level, ConfidenceLevel::Medium | ConfidenceLevel::High));
    }

    #[test]
    fn test_minor_regression_detection() {
        let detector = RegressionDetector::new();
        let data = create_test_performance_data(100.0, 0.15, 3.0, 100); // 15% degradation over time
        let result = detector.analyze(&data).unwrap();

        assert_eq!(result.status, RegressionStatus::Minor);
        assert!(!result.metric_regressions.is_empty());
        assert_eq!(result.temporal_analysis.trend, TrendDirection::Degrading);
        assert!(result.temporal_analysis.trend_strength > 0.4);
        
        let regression = &result.metric_regressions[0];
        assert_eq!(regression.metric, PerformanceMetric::AvgExecutionTime);
        assert!(matches!(regression.severity, RegressionSeverity::Low | RegressionSeverity::Medium));
        assert!(regression.percentage_change > 10.0);
    }

    #[test]
    fn test_significant_regression_detection() {
        let detector = RegressionDetector::new();
        let data = create_test_performance_data(100.0, 0.4, 5.0, 100); // 40% degradation
        let result = detector.analyze(&data).unwrap();

        assert_eq!(result.status, RegressionStatus::Significant);
        assert!(!result.metric_regressions.is_empty());
        assert_eq!(result.temporal_analysis.trend, TrendDirection::Degrading);
        assert!(result.temporal_analysis.trend_strength > 0.6);

        let regression = &result.metric_regressions[0];
        assert!(matches!(regression.severity, RegressionSeverity::High | RegressionSeverity::Critical));
        assert!(regression.percentage_change > 30.0);
        assert!(regression.statistical_significance < 0.05);
    }

    #[test]
    fn test_critical_regression_detection() {
        let detector = RegressionDetector::new();
        let data = create_test_performance_data(100.0, 0.8, 10.0, 100); // 80% degradation
        let result = detector.analyze(&data).unwrap();

        assert_eq!(result.status, RegressionStatus::Critical);
        let regression = &result.metric_regressions[0];
        assert_eq!(regression.severity, RegressionSeverity::Critical);
        assert!(regression.percentage_change > 50.0);

        // Should have high-priority recommendations
        let critical_recommendations = result.recommendations.iter()
            .filter(|r| matches!(r.priority, Priority::Critical))
            .count();
        assert!(critical_recommendations > 0);
    }

    #[test]
    fn test_performance_improvement_detection() {
        let detector = RegressionDetector::new();
        let data = create_test_performance_data(200.0, -0.3, 5.0, 100); // Improvement over time
        let result = detector.analyze(&data).unwrap();

        assert_eq!(result.temporal_analysis.trend, TrendDirection::Improving);
        assert!(result.temporal_analysis.trend_strength > 0.4);
        assert_eq!(result.status, RegressionStatus::None); // Improvement is not a regression
    }

    #[test]
    fn test_volatile_performance_detection() {
        let detector = RegressionDetector::new();
        let data = create_test_performance_data(100.0, 0.0, 40.0, 100); // High variance
        let result = detector.analyze(&data).unwrap();

        assert_eq!(result.temporal_analysis.trend, TrendDirection::Volatile);
        assert!(!result.statistical_analysis.anomalies.is_empty());
        assert!(result.statistical_analysis.distribution.outlier_percentage > 5.0);
    }

    #[test]
    fn test_change_point_detection() {
        let detector = RegressionDetector::new();
        
        // Create data with a clear change point
        let mut data = create_test_performance_data(100.0, 0.0, 3.0, 50);
        data.extend(create_test_performance_data(150.0, 0.0, 3.0, 50));
        
        let result = detector.analyze(&data).unwrap();

        assert!(!result.temporal_analysis.change_points.is_empty());
        let change_point = &result.temporal_analysis.change_points[0];
        assert_eq!(change_point.change_type, ChangeType::Degradation);
        assert!(change_point.magnitude > 0.3);
        assert!(change_point.confidence > 0.5);
        assert!(!change_point.possible_causes.is_empty());
    }

    #[test]
    fn test_statistical_analysis() {
        let detector = RegressionDetector::new();
        let data = create_test_performance_data(100.0, 0.1, 10.0, 200);
        let result = detector.analyze(&data).unwrap();

        // Should perform statistical tests
        assert!(!result.statistical_analysis.tests_performed.is_empty());
        
        // Should analyze distribution
        let dist = &result.statistical_analysis.distribution;
        assert!(dist.mean > 0.0);
        assert!(dist.std_dev > 0.0);
        assert_ne!(dist.distribution_type, DistributionType::Unknown);

        // Should detect some anomalies with noise
        assert!(!result.statistical_analysis.anomalies.is_empty());
    }

    #[test]
    fn test_correlation_analysis() {
        let detector = RegressionDetector::new();
        let start_time = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        
        // Create data with strong correlation between execution time and memory
        let data: Vec<PerformanceDataPoint> = (0..100)
            .map(|i| {
                let exec_time = 100.0 + i as f64 * 0.5;
                PerformanceDataPoint {
                    timestamp: start_time + chrono::Duration::hours(i as i64),
                    execution_time_ms: exec_time,
                    memory_usage_mb: Some(50.0 + exec_time * 0.8), // Strong correlation
                    cpu_usage_percent: Some(20.0 + exec_time * 0.1),
                    io_operations: Some((exec_time * 5.0) as u64),
                    cache_hit_ratio: Some(0.95 - exec_time * 0.001),
                }
            })
            .collect();

        let result = detector.analyze(&data).unwrap();

        assert!(!result.statistical_analysis.correlations.is_empty());
        let correlation = result.statistical_analysis.correlations.iter()
            .find(|c| c.metric1 == PerformanceMetric::AvgExecutionTime && c.metric2 == PerformanceMetric::MemoryUsage)
            .expect("Should find execution time vs memory correlation");
        
        assert!(correlation.correlation > 0.7); // Strong positive correlation
        assert!(matches!(correlation.strength, CorrelationStrength::Strong | CorrelationStrength::VeryStrong));
    }

    #[test]
    fn test_seasonality_detection() {
        let detector = RegressionDetector::new();
        let start_time = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        
        // Create data with daily pattern (higher performance during business hours)
        let data: Vec<PerformanceDataPoint> = (0..200) // 200+ hours for pattern detection
            .map(|i| {
                let hour_of_day = (i % 24) as f64;
                let business_hour_factor = if hour_of_day >= 9.0 && hour_of_day <= 17.0 {
                    1.5 // Higher load during business hours
                } else {
                    1.0
                };
                let exec_time = 100.0 * business_hour_factor + (i % 10) as f64;
                
                PerformanceDataPoint {
                    timestamp: start_time + chrono::Duration::hours(i as i64),
                    execution_time_ms: exec_time,
                    memory_usage_mb: Some(100.0 + exec_time * 0.1),
                    cpu_usage_percent: None,
                    io_operations: None,
                    cache_hit_ratio: None,
                }
            })
            .collect();

        let result = detector.analyze(&data).unwrap();

        if let Some(seasonality) = &result.temporal_analysis.seasonality {
            assert_eq!(seasonality.pattern_type, SeasonalityType::Daily);
            assert!(seasonality.strength > 0.15);
        }
    }

    #[test]
    fn test_insufficient_data_handling() {
        let detector = RegressionDetector::new();
        let data = create_test_performance_data(100.0, 0.0, 5.0, 10); // Too few points
        let result = detector.analyze(&data).unwrap();

        assert_eq!(result.status, RegressionStatus::InsufficientData);
        assert_eq!(result.confidence_level, ConfidenceLevel::Low);
        assert!(result.recommendations.iter().any(|r| 
            r.description.contains("more performance data")
        ));
    }

    #[test]
    fn test_recommendation_generation() {
        let detector = RegressionDetector::new();
        let data = create_test_performance_data(100.0, 0.6, 5.0, 100); // Strong degradation
        let result = detector.analyze(&data).unwrap();

        assert!(!result.recommendations.is_empty());
        
        // Should have investigation recommendations for regression
        let investigation_recs = result.recommendations.iter()
            .filter(|r| matches!(r.recommendation_type, RecommendationType::Investigation))
            .count();
        assert!(investigation_recs > 0);

        // Should have high or critical priority recommendations
        let high_priority_recs = result.recommendations.iter()
            .filter(|r| matches!(r.priority, Priority::High | Priority::Critical))
            .count();
        assert!(high_priority_recs > 0);

        // Recommendations should have actions
        for rec in &result.recommendations {
            assert!(!rec.actions.is_empty());
            assert!(!rec.description.is_empty());
        }
    }

    #[test]
    fn test_custom_thresholds() {
        let custom_thresholds = crate::analysis::consolidated_config::RegressionThresholds {
            minor_threshold: 0.05,      // 5% instead of 10%
            significant_threshold: 0.15, // 15% instead of 25%
            critical_threshold: 0.30,   // 30% instead of 50%
        };
        let detector = RegressionDetector::default().with_thresholds(custom_thresholds);
        
        let data = create_test_performance_data(100.0, 0.08, 2.0, 100); // 8% degradation
        let result = detector.analyze(&data).unwrap();

        // Should detect regression with custom lower threshold
        assert_ne!(result.status, RegressionStatus::None);
        assert!(!result.metric_regressions.is_empty());
    }

    #[test]
    fn test_confidence_level_calculation() {
        let detector = RegressionDetector::new();
        
        // High confidence: large dataset, stable distribution
        let large_stable_data = create_test_performance_data(100.0, 0.1, 2.0, 1000);
        let high_conf_result = detector.analyze(&large_stable_data).unwrap();
        assert!(matches!(high_conf_result.confidence_level, ConfidenceLevel::High | ConfidenceLevel::VeryHigh));

        // Low confidence: small dataset, high variance
        let small_noisy_data = create_test_performance_data(100.0, 0.1, 50.0, 50);
        let low_conf_result = detector.analyze(&small_noisy_data).unwrap();
        assert!(matches!(low_conf_result.confidence_level, ConfidenceLevel::Low | ConfidenceLevel::Medium));
    }
}

#[cfg(test)]
mod integration_tests {
    use crate::sql_analysis::{
        normalize_query_enhanced, calculate_query_fingerprint,
        ComplexityAnalyzer, MetadataExtractor
    };
    use crate::sql_analysis::complexity::ComplexityClass;
    use crate::sql_analysis::metadata::{TableAccessType, FunctionCategory, QueryOperation};

    #[test]
    fn test_end_to_end_analysis_pipeline() {
        // Test complete analysis pipeline: normalization -> complexity -> metadata -> regression
        
        let sql = r#"
            SELECT 
                u.name,
                u.email,
                COUNT(o.id) as order_count,
                AVG(o.total) as avg_order_value,
                SUM(o.total) as total_spent
            FROM users u
            LEFT JOIN orders o ON u.id = o.user_id
            WHERE u.created_at >= '2024-01-01'
              AND u.status = 'active'
            GROUP BY u.id, u.name, u.email
            HAVING COUNT(o.id) > 5
            ORDER BY total_spent DESC
            LIMIT 100
        "#;

        // 1. Normalization
        let norm_result = normalize_query_enhanced(sql).unwrap();
        assert!(norm_result.successful);
        assert!(norm_result.parameter_count > 0);
        assert!(!norm_result.fingerprint.is_empty());

        // 2. Complexity Analysis
        let complexity_analyzer = ComplexityAnalyzer::new();
        let complexity_result = complexity_analyzer.analyze(sql).unwrap();
        assert!(matches!(complexity_result.classification, ComplexityClass::Moderate | ComplexityClass::Complex));
        assert!(complexity_result.total_score > 20.0);

        // 3. Metadata Extraction
        let metadata_extractor = MetadataExtractor::new();
        let metadata_result = metadata_extractor.extract(sql).unwrap();
        assert_eq!(metadata_result.operation, QueryOperation::Select);
        assert_eq!(metadata_result.table_references.len(), 2);
        assert!(!metadata_result.function_references.is_empty());
        assert!(!metadata_result.performance_hints.is_empty());

        // 4. Verify consistency between analyses
        assert_eq!(norm_result.fingerprint, calculate_query_fingerprint(sql).unwrap());
        
        // Complexity should correlate with metadata complexity indicators
        let has_joins = !metadata_result.table_references.iter()
            .any(|t| matches!(t.access_type, TableAccessType::Joined));
        let has_aggregates = !metadata_result.function_references.iter()
            .any(|f| matches!(f.category, FunctionCategory::Aggregate));
        
        if has_joins || has_aggregates {
            assert!(complexity_result.total_score > 15.0);
        }
    }

    #[test]
    fn test_analysis_with_various_sql_types() {
        let test_cases = vec![
            ("Simple SELECT", "SELECT id, name FROM users WHERE active = true"),
            ("Complex JOIN", r#"
                SELECT u.name, p.title, c.name as company
                FROM users u
                JOIN profiles p ON u.id = p.user_id
                LEFT JOIN companies c ON p.company_id = c.id
                WHERE u.created_at > '2024-01-01'
            "#),
            ("Aggregate Query", r#"
                SELECT department, COUNT(*), AVG(salary)
                FROM employees
                GROUP BY department
                HAVING COUNT(*) > 10
            "#),
            ("Subquery", r#"
                SELECT * FROM users
                WHERE id IN (SELECT user_id FROM orders WHERE total > 100)
            "#),
            ("Window Function", r#"
                SELECT name, salary,
                       ROW_NUMBER() OVER (ORDER BY salary DESC) as rank
                FROM employees
            "#),
        ];

        for (test_name, sql) in test_cases {
            println!("Testing: {}", test_name);
            
            // All analyses should succeed
            let norm_result = normalize_query_enhanced(sql);
            assert!(norm_result.is_ok(), "Normalization failed for {}: {:?}", test_name, norm_result.err());
            
            let complexity_result = ComplexityAnalyzer::new().analyze(sql);
            assert!(complexity_result.is_ok(), "Complexity analysis failed for {}: {:?}", test_name, complexity_result.err());
            
            let metadata_result = MetadataExtractor::new().extract(sql);
            assert!(metadata_result.is_ok(), "Metadata extraction failed for {}: {:?}", test_name, metadata_result.err());
            
            // Verify basic properties
            let norm = norm_result.unwrap();
            let complexity = complexity_result.unwrap();
            let metadata = metadata_result.unwrap();
            
            assert!(norm.successful);
            assert!(!norm.fingerprint.is_empty());
            assert!(complexity.total_score >= 0.0);
            assert_eq!(metadata.operation, QueryOperation::Select);
            assert!(!metadata.table_references.is_empty());
        }
    }

    #[test]
    fn test_performance_with_large_dataset() {
        use std::time::Instant;
        
        // Test performance with a complex query
        let complex_sql = r#"
            WITH RECURSIVE category_tree AS (
                SELECT id, name, parent_id, 0 as level
                FROM categories 
                WHERE parent_id IS NULL
                
                UNION ALL
                
                SELECT c.id, c.name, c.parent_id, ct.level + 1
                FROM categories c
                JOIN category_tree ct ON c.parent_id = ct.id
            ),
            monthly_sales AS (
                SELECT 
                    ct.name as category,
                    DATE_TRUNC('month', o.created_at) as month,
                    COUNT(DISTINCT o.id) as order_count,
                    COUNT(DISTINCT o.customer_id) as unique_customers,
                    SUM(oi.quantity * oi.price) as revenue,
                    AVG(oi.quantity * oi.price) as avg_order_value,
                    ROW_NUMBER() OVER (PARTITION BY ct.name ORDER BY SUM(oi.quantity * oi.price) DESC) as revenue_rank
                FROM category_tree ct
                JOIN products p ON p.category_id = ct.id
                JOIN order_items oi ON oi.product_id = p.id
                JOIN orders o ON o.id = oi.order_id
                WHERE o.status = 'completed'
                  AND o.created_at >= '2024-01-01'
                  AND o.created_at < '2025-01-01'
                GROUP BY ct.name, DATE_TRUNC('month', o.created_at)
            )
            SELECT 
                ms.category,
                ms.month,
                ms.order_count,
                ms.unique_customers,
                ms.revenue,
                ms.avg_order_value,
                ms.revenue_rank,
                LAG(ms.revenue) OVER (PARTITION BY ms.category ORDER BY ms.month) as prev_month_revenue,
                CASE 
                    WHEN LAG(ms.revenue) OVER (PARTITION BY ms.category ORDER BY ms.month) IS NULL THEN NULL
                    ELSE (ms.revenue - LAG(ms.revenue) OVER (PARTITION BY ms.category ORDER BY ms.month)) / 
                         LAG(ms.revenue) OVER (PARTITION BY ms.category ORDER BY ms.month) * 100
                END as revenue_growth_pct
            FROM monthly_sales ms
            WHERE ms.revenue_rank <= 10
            ORDER BY ms.category, ms.month
        "#;

        // Measure performance
        let start = Instant::now();
        
        let norm_result = normalize_query_enhanced(complex_sql).unwrap();
        let norm_time = start.elapsed();
        
        let complexity_start = Instant::now();
        let complexity_result = ComplexityAnalyzer::new().analyze(complex_sql).unwrap();
        let complexity_time = complexity_start.elapsed();
        
        let metadata_start = Instant::now();
        let metadata_result = MetadataExtractor::new().extract(complex_sql).unwrap();
        let metadata_time = metadata_start.elapsed();
        
        let total_time = start.elapsed();
        
        // Performance assertions (should complete in reasonable time)
        assert!(norm_time.as_millis() < 500, "Normalization took too long: {:?}", norm_time);
        assert!(complexity_time.as_millis() < 1000, "Complexity analysis took too long: {:?}", complexity_time);
        assert!(metadata_time.as_millis() < 1000, "Metadata extraction took too long: {:?}", metadata_time);
        assert!(total_time.as_millis() < 2000, "Total analysis took too long: {:?}", total_time);
        
        // Quality assertions
        assert!(norm_result.successful);
        assert_eq!(complexity_result.classification, ComplexityClass::VeryComplex);
        assert!(complexity_result.total_score > 70.0);
        assert!(metadata_result.table_references.len() >= 4);
        assert!(metadata_result.function_references.len() >= 8);
        
        println!("Performance test completed in {:?}", total_time);
        println!("- Normalization: {:?}", norm_time);
        println!("- Complexity: {:?}", complexity_time);
        println!("- Metadata: {:?}", metadata_time);
    }
}