//! Example demonstrating the modular analysis system
//! 
//! This example shows how to:
//! - Set up analyzers with the new consolidated configuration system
//! - Run analysis on parsed plans
//! - Handle analysis results and findings

use pg_loganalyze_core::{
    ParsedPlan, PlanNode, NodeType, ScanType, JoinType, PlanCost, PlanSourceFormat,
    analysis::{
        engine::{AnalysisEngine, AnalysisEngineBuilder},
        AnalysisContext, Severity,
        consolidated_config::{AnalysisConfiguration, ConfigurationBuilder},
    }
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("PostgreSQL Plan Analysis Demo");
    println!("============================\n");
    
    // Create a sample plan with performance issues
    let plan = create_problematic_plan();
    
    // Set up analysis context
    let context = AnalysisContext::new()
        .with_work_mem_kb(4096)
        .with_parallel_workers(2)
        .with_pg_version("14.5".to_string())
        .with_query_duration(2500.0); // 2.5 seconds
    
    // Demo 1: Basic analysis with default settings
    println!("🔍 Demo 1: Basic Analysis");
    println!("--------------------------");
    run_basic_analysis(&plan, &context);
    
    // Demo 2: OLTP-optimized configuration
    println!("\n🔧 Demo 2: OLTP-Optimized Analysis");
    println!("-----------------------------------");
    run_oltp_analysis(&plan, &context);
    
    // Demo 3: Analytics warehouse configuration
    println!("\n⚡ Demo 3: Analytics Warehouse Analysis");
    println!("---------------------------------------");
    run_analytics_analysis(&plan, &context);
    
    // Demo 4: Configuration serialization
    println!("\n💾 Demo 4: Configuration Management");
    println!("-----------------------------------");
    demonstrate_configuration_management()?;
    
    Ok(())
}

fn run_basic_analysis(plan: &ParsedPlan, context: &AnalysisContext) {
    let config = AnalysisConfiguration::default();
    let engine = AnalysisEngineBuilder::new()
        .with_configuration(&config)
        .build();
    
    let result = engine.analyze(plan, context);
    
    println!("Analysis completed with {} findings", result.total_findings());
    for report in &result.reports {
        println!("  {}: {} findings", report.analyzer_name, report.findings.len());
        for finding in &report.findings {
            println!("    {:?}: {}", finding.severity, finding.title);
        }
    }
}

fn run_oltp_analysis(plan: &ParsedPlan, context: &AnalysisContext) {
    let config = ConfigurationBuilder::high_performance_oltp();
    let engine = AnalysisEngineBuilder::new()
        .with_configuration(&config)
        .build();
    
    let result = engine.analyze(plan, context);
    
    println!("OLTP Analysis completed with {} findings", result.total_findings());
    println!("Performance Assessment: {:?}", result.summary.performance_assessment);
}

fn run_analytics_analysis(plan: &ParsedPlan, context: &AnalysisContext) {
    let config = ConfigurationBuilder::analytics_warehouse();
    let engine = AnalysisEngineBuilder::new()
        .with_configuration(&config)
        .build();
    
    let result = engine.analyze(plan, context);
    
    println!("Analytics Analysis completed with {} findings", result.total_findings());
    println!("Performance Assessment: {:?}", result.summary.performance_assessment);
}

fn demonstrate_configuration_management() -> Result<(), Box<dyn std::error::Error>> {
    let config = ConfigurationBuilder::development_environment();
    
    // Serialize configuration to JSON
    let json = serde_json::to_string_pretty(&config)?;
    println!("Configuration JSON:");
    println!("{}", json);
    
    // Deserialize back
    let _restored_config: AnalysisConfiguration = serde_json::from_str(&json)?;
    println!("✅ Configuration serialization/deserialization successful");
    
    Ok(())
}

fn create_problematic_plan() -> ParsedPlan {
    // Create a plan with a large nested loop join (performance issue)
    let mut root = PlanNode::new(
        NodeType::Join(JoinType::NestedLoop { 
            join_type: pg_loganalyze_core::JoinConditionType::Inner, 
            condition: None 
        }),
        PlanCost {
            startup_cost: 0.29,
            min_total_cost: 0.29,
            max_total_cost: 25000.0, // High cost indicating performance issue
            estimated_rows: 50000,   // Large result set
            estimated_width: 120,
        },
        "Nested Loop".to_string(),
    );
    
    // Left child - Sequential scan on large table
    let left_child = PlanNode::new(
        NodeType::Scan(ScanType::SeqScan { 
            table: pg_loganalyze_core::TableReference { 
                schema: Some("public".to_string()), 
                name: "orders".to_string(), 
                alias: None 
            } 
        }),
        PlanCost {
            startup_cost: 0.0,
            min_total_cost: 0.0,
            max_total_cost: 12000.0,
            estimated_rows: 100000, // Large table
            estimated_width: 80,
        },
        "Seq Scan on orders".to_string(),
    );
    
    // Right child - Index scan (but still large)
    let right_child = PlanNode::new(
        NodeType::Scan(ScanType::IndexScan { 
            table: pg_loganalyze_core::TableReference { 
                schema: Some("public".to_string()), 
                name: "customers".to_string(), 
                alias: None 
            },
            index_name: Some("idx_customers_id".to_string()),
            scan_direction: None,
        }),
        PlanCost {
            startup_cost: 0.29,
            min_total_cost: 0.29,
            max_total_cost: 5000.0,
            estimated_rows: 10000,
            estimated_width: 40,
        },
        "Index Scan on customers".to_string(),
    );
    
    root.add_child(left_child);
    root.add_child(right_child);
    
    ParsedPlan::new(root, "SELECT * FROM orders o JOIN customers c ON o.customer_id = c.id".to_string(), PlanSourceFormat::Text)
}