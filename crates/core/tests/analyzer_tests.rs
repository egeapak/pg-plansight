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
mod startup_cost_tests {
    use super::*;

    #[test]
    fn test_high_startup_cost_detection_medium() {
        let config = AnalysisConfiguration::default();
        let analyzer = StartupCostAnalyzer::with_config(&config);
        let context = create_test_context();

        let node = PlanNode::new(
            NodeType::Utility(UtilityType::Sort {
                sort_keys: vec![SortKey {
                    expression: "created_at".to_string(),
                    direction: Some("DESC".to_string()),
                }],
                sort_method: Some("quicksort".to_string()),
            }),
            PlanCost {
                startup_cost: 25000.0,
                min_total_cost: 25000.0,
                max_total_cost: 30000.0,
                estimated_rows: 100000,
                estimated_width: 200,
            },
            "Sort".to_string(),
        );

        let plan = ParsedPlan::new(node);
        let report = analyzer.analyze(&plan, &context);

        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f.finding_type, FindingType::HighStartupCost))
        );
        let finding = report
            .findings
            .iter()
            .find(|f| matches!(f.finding_type, FindingType::HighStartupCost))
            .unwrap();
        assert_eq!(finding.severity, Severity::Medium);
    }

    #[test]
    fn test_high_startup_cost_detection_critical() {
        let config = AnalysisConfiguration::default();
        let analyzer = StartupCostAnalyzer::with_config(&config);
        let context = create_test_context();

        let node = PlanNode::new(
            NodeType::Utility(UtilityType::Sort {
                sort_keys: vec![SortKey {
                    expression: "column1".to_string(),
                    direction: None,
                }],
                sort_method: Some("external merge".to_string()),
            }),
            PlanCost {
                startup_cost: 150000.0,
                min_total_cost: 150000.0,
                max_total_cost: 155000.0,
                estimated_rows: 1000000,
                estimated_width: 200,
            },
            "Sort".to_string(),
        );

        let plan = ParsedPlan::new(node);
        let report = analyzer.analyze(&plan, &context);

        let finding = report
            .findings
            .iter()
            .find(|f| matches!(f.finding_type, FindingType::HighStartupCost))
            .unwrap();
        assert_eq!(finding.severity, Severity::Critical);
    }

    #[test]
    fn test_startup_cost_dominant_detection() {
        let config = AnalysisConfiguration::default();
        let analyzer = StartupCostAnalyzer::with_config(&config);
        let context = create_test_context();

        let node = PlanNode::new(
            NodeType::Utility(UtilityType::Sort {
                sort_keys: vec![SortKey {
                    expression: "id".to_string(),
                    direction: None,
                }],
                sort_method: Some("quicksort".to_string()),
            }),
            PlanCost {
                startup_cost: 9500.0,
                min_total_cost: 9500.0,
                max_total_cost: 10000.0, // 95% startup
                estimated_rows: 50000,
                estimated_width: 100,
            },
            "Sort".to_string(),
        );

        let plan = ParsedPlan::new(node);
        let report = analyzer.analyze(&plan, &context);

        assert!(report.findings.iter().any(
            |f| matches!(f.finding_type, FindingType::Custom(ref s) if s == "StartupCostDominant")
        ));
    }

    #[test]
    fn test_external_sort_detection() {
        let config = AnalysisConfiguration::default();
        let analyzer = StartupCostAnalyzer::with_config(&config);
        let context = create_test_context();

        let node = PlanNode::new(
            NodeType::Utility(UtilityType::Sort {
                sort_keys: vec![SortKey {
                    expression: "data".to_string(),
                    direction: Some("ASC".to_string()),
                }],
                sort_method: Some("external merge Disk: 2048kB".to_string()),
            }),
            PlanCost {
                startup_cost: 12000.0,
                min_total_cost: 12000.0,
                max_total_cost: 15000.0,
                estimated_rows: 500000,
                estimated_width: 150,
            },
            "Sort".to_string(),
        );

        let plan = ParsedPlan::new(node);
        let report = analyzer.analyze(&plan, &context);

        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f.finding_type, FindingType::Custom(ref s) if s == "ExternalSort"))
        );
    }

    #[test]
    fn test_expensive_materialization_detection() {
        let config = AnalysisConfiguration::default();
        let analyzer = StartupCostAnalyzer::with_config(&config);
        let context = create_test_context();

        let node = PlanNode::new(
            NodeType::Utility(UtilityType::Materialize),
            PlanCost {
                startup_cost: 8000.0,
                min_total_cost: 8000.0,
                max_total_cost: 10000.0,
                estimated_rows: 100000,
                estimated_width: 100,
            },
            "Materialize".to_string(),
        );

        let plan = ParsedPlan::new(node);
        let report = analyzer.analyze(&plan, &context);

        assert!(report.findings.iter().any(
            |f| matches!(f.finding_type, FindingType::Custom(ref s) if s == "ExpensiveMaterialization")
        ));
    }

    #[test]
    fn test_low_startup_cost_no_findings() {
        let config = AnalysisConfiguration::default();
        let analyzer = StartupCostAnalyzer::with_config(&config);
        let context = create_test_context();

        let plan = create_seq_scan_plan(10000, 1000.0);
        let report = analyzer.analyze(&plan, &context);

        assert_eq!(report.findings.len(), 0);
    }

    #[test]
    fn test_metrics_collection() {
        let config = AnalysisConfiguration::default();
        let analyzer = StartupCostAnalyzer::with_config(&config);
        let context = create_test_context();

        let node = PlanNode::new(
            NodeType::Utility(UtilityType::Sort {
                sort_keys: vec![SortKey {
                    expression: "col".to_string(),
                    direction: None,
                }],
                sort_method: Some("quicksort".to_string()),
            }),
            PlanCost {
                startup_cost: 15000.0,
                min_total_cost: 15000.0,
                max_total_cost: 20000.0,
                estimated_rows: 50000,
                estimated_width: 100,
            },
            "Sort".to_string(),
        );

        let plan = ParsedPlan::new(node);
        let report = analyzer.analyze(&plan, &context);

        assert!(report.metrics.contains_key("nodes_analyzed"));
        assert!(report.metrics.contains_key("max_startup_cost"));
        assert!(report.metrics.contains_key("nodes_with_high_startup"));
        assert_eq!(report.metrics.get("max_startup_cost"), Some(&15000.0));
    }
}

#[cfg(test)]
mod index_usage_tests {
    use super::*;

    #[test]
    fn test_missing_index_small_result_set() {
        let config = AnalysisConfiguration::default();
        let analyzer = IndexUsageAnalyzer::with_config(&config);
        let context = create_test_context();

        let mut node = PlanNode::new(
            NodeType::Scan(ScanType::SeqScan {
                table: TableReference {
                    schema: Some("public".to_string()),
                    name: "users".to_string(),
                    alias: None,
                },
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 1000.0,
                estimated_rows: 5000, // Moderate selectivity
                estimated_width: 100,
            },
            "Seq Scan on users".to_string(),
        );

        node.set_property("Filter".to_string(), "(status = 'active')".to_string());

        let plan = ParsedPlan::new(node);
        let report = analyzer.analyze(&plan, &context);

        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f.finding_type, FindingType::MissingIndex))
        );
        let finding = report
            .findings
            .iter()
            .find(|f| matches!(f.finding_type, FindingType::MissingIndex))
            .unwrap();
        assert_eq!(finding.severity, Severity::Low);
    }

    #[test]
    fn test_missing_index_large_result_set() {
        let config = AnalysisConfiguration::default();
        let analyzer = IndexUsageAnalyzer::with_config(&config);
        let context = create_test_context();

        let mut node = PlanNode::new(
            NodeType::Scan(ScanType::SeqScan {
                table: TableReference {
                    schema: Some("public".to_string()),
                    name: "orders".to_string(),
                    alias: None,
                },
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 10000.0,
                estimated_rows: 200000, // High selectivity
                estimated_width: 150,
            },
            "Seq Scan on orders".to_string(),
        );

        node.set_property(
            "Filter".to_string(),
            "(customer_id = 12345)".to_string(),
        );

        let plan = ParsedPlan::new(node);
        let report = analyzer.analyze(&plan, &context);

        let finding = report
            .findings
            .iter()
            .find(|f| matches!(f.finding_type, FindingType::MissingIndex))
            .unwrap();
        assert_eq!(finding.severity, Severity::High);
    }

    #[test]
    fn test_function_in_filter_lower() {
        let config = AnalysisConfiguration::default();
        let analyzer = IndexUsageAnalyzer::with_config(&config);
        let context = create_test_context();

        let mut node = PlanNode::new(
            NodeType::Scan(ScanType::SeqScan {
                table: TableReference {
                    schema: Some("public".to_string()),
                    name: "accounts".to_string(),
                    alias: None,
                },
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 5000.0,
                estimated_rows: 10000,
                estimated_width: 100,
            },
            "Seq Scan on accounts".to_string(),
        );

        node.set_property(
            "Filter".to_string(),
            "LOWER(email) = 'test@example.com'".to_string(),
        );

        let plan = ParsedPlan::new(node);
        let report = analyzer.analyze(&plan, &context);

        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f.finding_type, FindingType::Custom(ref s) if s == "FunctionInFilter"))
        );
    }

    #[test]
    fn test_function_in_filter_date() {
        let config = AnalysisConfiguration::default();
        let analyzer = IndexUsageAnalyzer::with_config(&config);
        let context = create_test_context();

        let mut node = PlanNode::new(
            NodeType::Scan(ScanType::SeqScan {
                table: TableReference {
                    schema: Some("public".to_string()),
                    name: "events".to_string(),
                    alias: None,
                },
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 8000.0,
                estimated_rows: 50000,
                estimated_width: 120,
            },
            "Seq Scan on events".to_string(),
        );

        node.set_property(
            "Filter".to_string(),
            "DATE(created_at) = '2024-01-01'".to_string(),
        );

        let plan = ParsedPlan::new(node);
        let report = analyzer.analyze(&plan, &context);

        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f.finding_type, FindingType::Custom(ref s) if s == "FunctionInFilter"))
        );
    }

    #[test]
    fn test_leading_wildcard_like() {
        let config = AnalysisConfiguration::default();
        let analyzer = IndexUsageAnalyzer::with_config(&config);
        let context = create_test_context();

        let mut node = PlanNode::new(
            NodeType::Scan(ScanType::SeqScan {
                table: TableReference {
                    schema: Some("public".to_string()),
                    name: "products".to_string(),
                    alias: None,
                },
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 3000.0,
                estimated_rows: 15000,
                estimated_width: 80,
            },
            "Seq Scan on products".to_string(),
        );

        node.set_property("Filter".to_string(), "name LIKE '%widget'".to_string());

        let plan = ParsedPlan::new(node);
        let report = analyzer.analyze(&plan, &context);

        assert!(report.findings.iter().any(
            |f| matches!(f.finding_type, FindingType::Custom(ref s) if s == "LeadingWildcardLike")
        ));
    }

    #[test]
    fn test_large_index_scan_detection() {
        let config = AnalysisConfiguration::default();
        let analyzer = IndexUsageAnalyzer::with_config(&config);
        let context = create_test_context();

        let node = PlanNode::new(
            NodeType::Scan(ScanType::IndexScan {
                table: TableReference {
                    schema: Some("public".to_string()),
                    name: "large_table".to_string(),
                    alias: None,
                },
                index: Some(IndexReference {
                    name: "idx_created_at".to_string(),
                }),
                backward: false,
                only: false,
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 25000.0,
                estimated_rows: 80000, // Large index scan
                estimated_width: 150,
            },
            "Index Scan using idx_created_at".to_string(),
        );

        let plan = ParsedPlan::new(node);
        let report = analyzer.analyze(&plan, &context);

        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f.finding_type, FindingType::Custom(ref s) if s == "LargeIndexScan"))
        );
    }

    #[test]
    fn test_large_bitmap_scan_detection() {
        let config = AnalysisConfiguration::default();
        let analyzer = IndexUsageAnalyzer::with_config(&config);
        let context = create_test_context();

        let node = PlanNode::new(
            NodeType::Scan(ScanType::BitmapHeapScan {
                table: TableReference {
                    schema: Some("public".to_string()),
                    name: "big_table".to_string(),
                    alias: None,
                },
                recheck_condition: None,
            }),
            PlanCost {
                startup_cost: 1000.0,
                min_total_cost: 1000.0,
                max_total_cost: 50000.0,
                estimated_rows: 150000, // Very large bitmap scan
                estimated_width: 200,
            },
            "Bitmap Heap Scan on big_table".to_string(),
        );

        let plan = ParsedPlan::new(node);
        let report = analyzer.analyze(&plan, &context);

        assert!(report.findings.iter().any(
            |f| matches!(f.finding_type, FindingType::Custom(ref s) if s == "LargeBitmapScan")
        ));
    }

    #[test]
    fn test_efficient_index_scan_no_findings() {
        let config = AnalysisConfiguration::default();
        let analyzer = IndexUsageAnalyzer::with_config(&config);
        let context = create_test_context();

        let node = PlanNode::new(
            NodeType::Scan(ScanType::IndexScan {
                table: TableReference {
                    schema: Some("public".to_string()),
                    name: "users".to_string(),
                    alias: None,
                },
                index: Some(IndexReference {
                    name: "idx_user_id".to_string(),
                }),
                backward: false,
                only: false,
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 50.0,
                estimated_rows: 100, // Small, efficient
                estimated_width: 50,
            },
            "Index Scan using idx_user_id".to_string(),
        );

        let plan = ParsedPlan::new(node);
        let report = analyzer.analyze(&plan, &context);

        assert_eq!(report.findings.len(), 0);
    }

    #[test]
    fn test_seq_scan_or_filter_no_index_suggestion() {
        let config = AnalysisConfiguration::default();
        let analyzer = IndexUsageAnalyzer::with_config(&config);
        let context = create_test_context();

        let mut node = PlanNode::new(
            NodeType::Scan(ScanType::SeqScan {
                table: TableReference {
                    schema: Some("public".to_string()),
                    name: "items".to_string(),
                    alias: None,
                },
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 5000.0,
                estimated_rows: 25000,
                estimated_width: 100,
            },
            "Seq Scan on items".to_string(),
        );

        // OR conditions don't benefit from single-column indexes
        node.set_property(
            "Filter".to_string(),
            "(status = 'active' OR status = 'pending')".to_string(),
        );

        let plan = ParsedPlan::new(node);
        let report = analyzer.analyze(&plan, &context);

        // Should not suggest index for OR conditions
        assert!(!report
            .findings
            .iter()
            .any(|f| matches!(f.finding_type, FindingType::MissingIndex)));
    }

    #[test]
    fn test_metrics_collection() {
        let config = AnalysisConfiguration::default();
        let analyzer = IndexUsageAnalyzer::with_config(&config);
        let context = create_test_context();

        let plan = create_seq_scan_plan(10000, 1000.0);
        let report = analyzer.analyze(&plan, &context);

        assert!(report.metrics.contains_key("nodes_analyzed"));
        assert!(report.metrics.contains_key("seq_scans"));
        assert!(report.metrics.contains_key("index_scans"));
        assert_eq!(report.metrics.get("seq_scans"), Some(&1.0));
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
