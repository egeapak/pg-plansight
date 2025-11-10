use pg_loganalyze_core::{
    IndexReference, JoinType, NodeType, ParsedPlan, PlanCost, PlanNode, ScanType, SortKey,
    TableReference, UtilityType,
    analysis::{
        AnalysisContext, Analyzer, FindingType, Severity, analyzers::*, consolidated_config::*,
    },
};

// Helper function to create a test context
fn create_test_context() -> AnalysisContext {
    AnalysisContext::new()
        .with_work_mem_kb(4096)
        .with_parallel_workers(2)
        .with_pg_version("14.0".to_string())
}

// Helper function to create a test plan with sequential scan
fn create_seq_scan_plan(rows: u64, cost: f64) -> ParsedPlan {
    let node = PlanNode::new(
        NodeType::Scan(ScanType::SeqScan {
            table: TableReference {
                schema: Some("public".to_string()),
                name: "test_table".to_string(),
                alias: None,
            },
        }),
        PlanCost {
            startup_cost: 0.0,
            min_total_cost: 0.0,
            max_total_cost: cost,
            estimated_rows: rows,
            estimated_width: 100,
        },
        "Seq Scan on test_table".to_string(),
    );
    ParsedPlan::new(node)
}

// Helper function to create a nested loop join plan
fn create_nested_loop_plan(left_rows: u64, right_rows: u64, result_rows: u64) -> ParsedPlan {
    let left_child = PlanNode::new(
        NodeType::Scan(ScanType::SeqScan {
            table: TableReference {
                schema: Some("public".to_string()),
                name: "table1".to_string(),
                alias: None,
            },
        }),
        PlanCost {
            startup_cost: 0.0,
            min_total_cost: 0.0,
            max_total_cost: 100.0,
            estimated_rows: left_rows,
            estimated_width: 50,
        },
        "Seq Scan on table1".to_string(),
    );

    let right_child = PlanNode::new(
        NodeType::Scan(ScanType::SeqScan {
            table: TableReference {
                schema: Some("public".to_string()),
                name: "table2".to_string(),
                alias: None,
            },
        }),
        PlanCost {
            startup_cost: 0.0,
            min_total_cost: 0.0,
            max_total_cost: 100.0,
            estimated_rows: right_rows,
            estimated_width: 50,
        },
        "Seq Scan on table2".to_string(),
    );

    let mut join_node = PlanNode::new(
        NodeType::Join(JoinType::NestedLoop {
            inner_unique: false,
        }),
        PlanCost {
            startup_cost: 0.0,
            min_total_cost: 0.0,
            max_total_cost: result_rows as f64,
            estimated_rows: result_rows,
            estimated_width: 100,
        },
        "Nested Loop".to_string(),
    );

    join_node.add_child(left_child);
    join_node.add_child(right_child);

    ParsedPlan::new(join_node)
}

#[cfg(test)]
mod row_estimation_tests {
    use super::*;

    #[test]
    fn test_no_findings_for_small_row_count() {
        let config = AnalysisConfiguration::default();
        let analyzer = RowEstimationAnalyzer::with_config(&config);
        let context = create_test_context();

        let plan = create_seq_scan_plan(100, 10.0);
        let report = analyzer.analyze(&plan, &context);

        // Should not find any issues with small row count
        assert_eq!(report.findings.len(), 0);
        assert_eq!(report.analyzer_name, "RowEstimationAnalyzer");
    }

    #[test]
    fn test_excessive_row_processing_medium() {
        let config = AnalysisConfiguration::default();
        let analyzer = RowEstimationAnalyzer::with_config(&config);
        let context = create_test_context();

        let plan = create_seq_scan_plan(150_000, 15000.0);
        let report = analyzer.analyze(&plan, &context);

        // Should detect medium severity issue
        assert!(!report.findings.is_empty());
        let finding = report
            .findings
            .iter()
            .find(|f| matches!(f.finding_type, FindingType::ExcessiveRowProcessing))
            .expect("Should find excessive row processing");

        assert!(matches!(
            finding.severity,
            Severity::Medium | Severity::High
        ));
        assert!(finding.evidence.contains_key("estimated_rows"));
    }

    #[test]
    fn test_excessive_row_processing_critical() {
        let config = AnalysisConfiguration::default();
        let analyzer = RowEstimationAnalyzer::with_config(&config);
        let context = create_test_context();

        let plan = create_seq_scan_plan(15_000_000, 1500000.0);
        let report = analyzer.analyze(&plan, &context);

        // Should detect critical severity issue
        let finding = report
            .findings
            .iter()
            .find(|f| matches!(f.finding_type, FindingType::ExcessiveRowProcessing))
            .expect("Should find excessive row processing");

        assert_eq!(finding.severity, Severity::Critical);
    }

    #[test]
    fn test_cartesian_product_detection() {
        let config = AnalysisConfiguration::default();
        let analyzer = RowEstimationAnalyzer::with_config(&config);
        let context = create_test_context();

        // Create a cartesian product: 1000 * 500 = 500,000
        let plan = create_nested_loop_plan(1000, 500, 450_000);
        let report = analyzer.analyze(&plan, &context);

        // Should detect cartesian product
        let finding = report
            .findings
            .iter()
            .find(|f| matches!(f.finding_type, FindingType::CartesianProduct))
            .expect("Should detect cartesian product");

        assert_eq!(finding.severity, Severity::Critical);
        assert!(finding.evidence.contains_key("cartesian_ratio"));
    }

    #[test]
    fn test_metrics_collection() {
        let config = AnalysisConfiguration::default();
        let analyzer = RowEstimationAnalyzer::with_config(&config);
        let context = create_test_context();

        let plan = create_seq_scan_plan(1_000_000, 100000.0);
        let report = analyzer.analyze(&plan, &context);

        // Verify metrics are collected
        assert!(report.metrics.contains_key("nodes_analyzed"));
        assert!(report.metrics.contains_key("max_processed_rows"));
        assert_eq!(report.metrics.get("nodes_analyzed"), Some(&1.0));
    }
}

#[cfg(test)]
mod scan_analysis_tests {
    use super::*;

    #[test]
    fn test_large_sequential_scan_detection() {
        let config = AnalysisConfiguration::default();
        let analyzer = ScanAnalyzer::with_config(&config);
        let context = create_test_context();

        let plan = create_seq_scan_plan(500_000, 50000.0);
        let report = analyzer.analyze(&plan, &context);

        // Should detect large sequential scan
        let finding = report
            .findings
            .iter()
            .find(|f| matches!(f.finding_type, FindingType::LargeSequentialScan))
            .expect("Should find large sequential scan");

        assert!(matches!(
            finding.severity,
            Severity::Medium | Severity::High | Severity::Critical
        ));
    }

    #[test]
    fn test_small_sequential_scan_ok() {
        let config = AnalysisConfiguration::default();
        let analyzer = ScanAnalyzer::with_config(&config);
        let context = create_test_context();

        let plan = create_seq_scan_plan(500, 50.0);
        let report = analyzer.analyze(&plan, &context);

        // Small scans should not trigger findings
        assert_eq!(report.findings.len(), 0);
    }

    #[test]
    fn test_index_scan_analysis() {
        let config = AnalysisConfiguration::default();
        let analyzer = ScanAnalyzer::with_config(&config);
        let context = create_test_context();

        let node = PlanNode::new(
            NodeType::Scan(ScanType::IndexScan {
                table: TableReference {
                    schema: Some("public".to_string()),
                    name: "indexed_table".to_string(),
                    alias: None,
                },
                index: Some(IndexReference {
                    name: "idx_test".to_string(),
                }),
                backward: false,
                only: false,
            }),
            PlanCost {
                startup_cost: 500.0, // High startup cost
                min_total_cost: 500.0,
                max_total_cost: 600.0,
                estimated_rows: 100,
                estimated_width: 50,
            },
            "Index Scan using idx_test".to_string(),
        );

        let plan = ParsedPlan::new(node);
        let report = analyzer.analyze(&plan, &context);

        // Analyzer should run without errors
        assert_eq!(report.analyzer_name, "ScanAnalyzer");
    }
}

#[cfg(test)]
mod join_analysis_tests {
    use super::*;

    #[test]
    fn test_large_nested_loop_detection() {
        let config = AnalysisConfiguration::default();
        let analyzer = JoinAnalyzer::with_config(&config);
        let context = create_test_context();

        // Large nested loop join
        let plan = create_nested_loop_plan(50_000, 1000, 100_000);
        let report = analyzer.analyze(&plan, &context);

        // Should detect large nested loop
        let finding = report
            .findings
            .iter()
            .find(|f| matches!(f.finding_type, FindingType::LargeNestedLoop))
            .expect("Should find large nested loop");

        assert!(matches!(
            finding.severity,
            Severity::High | Severity::Critical
        ));
    }

    #[test]
    fn test_hash_join_analysis() {
        let config = AnalysisConfiguration::default();
        let analyzer = JoinAnalyzer::with_config(&config);
        let context = create_test_context();

        let left_child = PlanNode::new(
            NodeType::Scan(ScanType::SeqScan {
                table: TableReference {
                    schema: Some("public".to_string()),
                    name: "table1".to_string(),
                    alias: None,
                },
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 1000.0,
                estimated_rows: 10000,
                estimated_width: 100,
            },
            "Seq Scan on table1".to_string(),
        );

        let right_child = PlanNode::new(
            NodeType::Scan(ScanType::SeqScan {
                table: TableReference {
                    schema: Some("public".to_string()),
                    name: "table2".to_string(),
                    alias: None,
                },
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 2000.0,
                estimated_rows: 20000,
                estimated_width: 100,
            },
            "Seq Scan on table2".to_string(),
        );

        let mut join_node = PlanNode::new(
            NodeType::Join(JoinType::HashJoin {
                hash_condition: Some("table1.id = table2.id".to_string()),
                hash_buckets: None,
            }),
            PlanCost {
                startup_cost: 100.0,
                min_total_cost: 100.0,
                max_total_cost: 3500.0,
                estimated_rows: 15000,
                estimated_width: 200,
            },
            "Hash Join".to_string(),
        );

        join_node.add_child(left_child);
        join_node.add_child(right_child);

        let plan = ParsedPlan::new(join_node);
        let report = analyzer.analyze(&plan, &context);

        // Should analyze without errors
        assert_eq!(report.analyzer_name, "JoinAnalyzer");
    }
}

#[cfg(test)]
mod parallelization_tests {
    use super::*;

    #[test]
    fn test_parallelization_analyzer_runs() {
        let config = AnalysisConfiguration::default();
        let analyzer = ParallelizationAnalyzer::with_config(&config);
        let context = create_test_context();

        // Create a simple sequential scan
        let plan = create_seq_scan_plan(100_000, 10000.0);
        let report = analyzer.analyze(&plan, &context);

        // Should run without errors
        assert_eq!(report.analyzer_name, "ParallelizationAnalyzer");
    }

    #[test]
    fn test_missed_parallelization_opportunity() {
        let config = AnalysisConfiguration::default();
        let analyzer = ParallelizationAnalyzer::with_config(&config);
        let context = create_test_context();

        // Large sequential scan that could benefit from parallelization
        let plan = create_seq_scan_plan(5_000_000, 500000.0);
        let report = analyzer.analyze(&plan, &context);

        // Should detect missed parallelization opportunity
        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f.finding_type, FindingType::MissedParallelization))
        );
    }
}

#[cfg(test)]
mod configuration_tests {
    use super::*;

    #[test]
    fn test_oltp_configuration() {
        let config = ConfigurationBuilder::high_performance_oltp();

        assert_eq!(config.workload.workload_type, WorkloadType::OLTP);
        assert_eq!(config.workload.database_size, DatabaseSize::Large);
        assert_eq!(
            config.workload.performance_target,
            PerformanceTarget::Latency
        );
        assert_eq!(config.global.min_severity, Severity::Low);
    }

    #[test]
    fn test_analytics_configuration() {
        let config = ConfigurationBuilder::analytics_warehouse();

        assert_eq!(config.workload.workload_type, WorkloadType::OLAP);
        assert_eq!(config.workload.database_size, DatabaseSize::VeryLarge);
        assert_eq!(
            config.workload.performance_target,
            PerformanceTarget::Throughput
        );
        assert_eq!(config.global.min_severity, Severity::Medium);
    }

    #[test]
    fn test_threshold_scaling() {
        let oltp_workload = WorkloadContext {
            workload_type: WorkloadType::OLTP,
            database_size: DatabaseSize::Small,
            performance_target: PerformanceTarget::Latency,
            postgres_version: "15.0".to_string(),
        };

        let olap_workload = WorkloadContext {
            workload_type: WorkloadType::OLAP,
            database_size: DatabaseSize::VeryLarge,
            performance_target: PerformanceTarget::Throughput,
            postgres_version: "15.0".to_string(),
        };

        let oltp_thresholds = SmartThresholds::for_workload(&oltp_workload);
        let olap_thresholds = SmartThresholds::for_workload(&olap_workload);

        // OLTP should have stricter (lower) thresholds
        assert!(oltp_thresholds.row_counts.high < olap_thresholds.row_counts.high);
        assert!(oltp_thresholds.costs.high < olap_thresholds.costs.high);
        assert!(oltp_thresholds.durations.high < olap_thresholds.durations.high);
    }

    #[test]
    fn test_configuration_serialization() {
        let config = ConfigurationBuilder::development_environment();

        // Serialize to JSON
        let json = serde_json::to_string(&config).expect("Should serialize");

        // Deserialize back
        let restored: AnalysisConfiguration =
            serde_json::from_str(&json).expect("Should deserialize");

        assert_eq!(
            config.workload.workload_type,
            restored.workload.workload_type
        );
        assert_eq!(config.global.min_severity, restored.global.min_severity);
    }
}
