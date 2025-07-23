//! Example demonstrating the modular analysis system
//! 
//! This example shows how to:
//! - Set up analyzers with custom configurations
//! - Run analysis on parsed plans
//! - Handle analysis results and findings

use pg_loganalyze_core::{
    ParsedPlan, PlanNode, NodeType, ScanType, JoinType, PlanCost, PlanSourceFormat, PlanActuals,
    analysis::{
        engine::{AnalysisEngine, AnalysisEngineBuilder},
        AnalysisContext, Severity,
        analyzers::RowEstimationAnalyzer,
        config::{AnalysisConfig, ConfigUtils, RowEstimationConfig},
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
    
    // Demo 2: Custom configuration
    println!("\n🔧 Demo 2: Custom Configuration");
    println!("--------------------------------");
    run_custom_configuration_analysis(&plan, &context);
    
    // Demo 3: Performance-focused analysis
    println!("\n⚡ Demo 3: Performance-Focused Analysis");
    println!("---------------------------------------");
    run_performance_focused_analysis(&plan, &context);
    
    // Demo 4: Configuration serialization
    println!("\n💾 Demo 4: Configuration Management");
    println!("-----------------------------------");
    demonstrate_configuration_management()?;
    
    Ok(())
}

/// Create a sample plan with various performance issues for demonstration
fn create_problematic_plan() -> ParsedPlan {
    // Create a plan that will trigger multiple warnings:
    // 1. Excessive row processing (2M rows)
    // 2. Row estimation error (estimated 1K, actual 100K)
    // 3. Potential cartesian product
    
    // Left side of join - small table scan with accurate estimation
    let left_child = {
        let cost = PlanCost {
            startup_cost: 0.0,
            min_total_cost: 0.0,
            max_total_cost: 50.0,
            estimated_rows: 1_000,
            estimated_width: 32,
        };
        
        PlanNode::new(
            NodeType::Scan(ScanType::IndexScan),
            cost,
            "Index Scan using idx_users_active on users".to_string(),
        )
    };
    
    // Right side of join - large table with severe estimation error
    let right_child = {
        let cost = PlanCost {
            startup_cost: 0.0,
            min_total_cost: 0.0,
            max_total_cost: 1_000.0,
            estimated_rows: 2_000, // Severely underestimated
            estimated_width: 64,
        };
        
        let mut node = PlanNode::new(
            NodeType::Scan(ScanType::SeqScan),
            cost,
            "Seq Scan on orders".to_string(),
        );
        
        // Add actual execution data showing the estimation error
        let actuals = PlanActuals {
            actual_time_ms: Some(2000.0),
            actual_rows: Some(2_000_000), // 1000x underestimate!
            actual_loops: Some(1),
        };
        node.set_actuals(actuals);
        
        node
    };
    
    // Join node - will create near-cartesian product
    let mut root = {
        let cost = PlanCost {
            startup_cost: 100.0,
            min_total_cost: 100.0,
            max_total_cost: 500_000.0,
            estimated_rows: 1_800_000, // 90% of cartesian product (1K * 2K = 2M)
            estimated_width: 96,
        };
        
        let mut node = PlanNode::new(
            NodeType::Join(JoinType::NestedLoop),
            cost,
            "Nested Loop".to_string(),
        );
        
        // Add actual data showing it was indeed expensive
        let actuals = PlanActuals {
            actual_time_ms: Some(2400.0),
            actual_rows: Some(1_900_000),
            actual_loops: Some(1),
        };
        node.set_actuals(actuals);
        
        node
    };
    
    // Assemble the plan
    root.add_child(left_child);
    root.add_child(right_child);
    
    ParsedPlan::new(root, "Problematic plan for demo".to_string(), PlanSourceFormat::Text)
}

fn run_basic_analysis(plan: &ParsedPlan, context: &AnalysisContext) {
    // Create engine with single analyzer
    let mut engine = AnalysisEngine::new();
    engine.add_analyzer(RowEstimationAnalyzer::new());
    
    // Run analysis
    let result = engine.analyze(plan, context);
    
    // Display results
    println!("Analysis completed in {:?}", result.total_duration);
    println!("Performance Assessment: {:?}", result.performance_assessment());
    println!("Total Findings: {}", result.combined_result.summary.total_findings);
    
    // Show findings by severity
    for severity in [Severity::Critical, Severity::High, Severity::Medium, Severity::Low] {
        let findings = result.combined_result.findings_by_severity(&severity);
        if !findings.is_empty() {
            println!("  {:?}: {} findings", severity, findings.len());
            for finding in findings.iter().take(2) { // Show first 2 of each severity
                println!("    • {}", finding.title);
            }
        }
    }
    
    // Show execution summary
    let summary = result.execution_summary();
    println!("Execution Summary:");
    println!("  Successful analyzers: {}/{}", summary.successful_count, summary.total_analyzers);
    println!("  Failed analyzers: {}", summary.failed_count);
}

fn run_custom_configuration_analysis(plan: &ParsedPlan, context: &AnalysisContext) {
    // Create custom configuration with stricter thresholds
    let mut config = RowEstimationConfig::default();
    config.row_thresholds.critical_row_count = 500_000; // Lower threshold
    config.row_thresholds.high_row_count = 50_000;
    config.estimation_error_thresholds.critical_error_ratio = 5.0; // More sensitive
    
    // Create analyzer with custom config
    let analyzer = RowEstimationAnalyzer::with_config(config);
    
    let mut engine = AnalysisEngine::new();
    engine.add_analyzer(analyzer);
    
    let result = engine.analyze(plan, context);
    
    println!("Custom Configuration Results:");
    println!("Total Findings: {}", result.combined_result.summary.total_findings);
    
    // Show all findings with details
    for finding in &result.combined_result.reports[0].findings {
        println!("  [{:?}] {}", finding.severity, finding.title);
        if let Some(row_count) = finding.evidence.get("row_count") {
            println!("    Row count: {}", row_count);
        }
        if let Some(error_ratio) = finding.evidence.get("error_ratio") {
            println!("    Estimation error: {:.1}x", error_ratio);
        }
    }
}

fn run_performance_focused_analysis(plan: &ParsedPlan, context: &AnalysisContext) {
    // Use performance-focused configuration
    let perf_config = ConfigUtils::performance_focused();
    
    // Create analyzer with performance config
    let analyzer = RowEstimationAnalyzer::with_config(perf_config.row_estimation);
    
    let mut engine = AnalysisEngine::new();
    engine.add_analyzer(analyzer);
    
    let result = engine.analyze(plan, context);
    
    println!("Performance-Focused Analysis:");
    println!("Found {} high-impact issues", result.combined_result.summary.total_findings);
    
    // Focus on critical and high severity issues only
    let critical_findings = result.combined_result.findings_by_severity(&Severity::Critical);
    let high_findings = result.combined_result.findings_by_severity(&Severity::High);
    
    println!("Critical Issues ({}):", critical_findings.len());
    for finding in critical_findings {
        println!("  🚨 {}", finding.title);
        println!("     💡 {}", finding.suggestion);
    }
    
    println!("High Priority Issues ({}):", high_findings.len());
    for finding in high_findings {
        println!("  ⚠️  {}", finding.title);
        println!("     💡 {}", finding.suggestion);
    }
}

fn demonstrate_configuration_management() -> Result<(), Box<dyn std::error::Error>> {
    // Create a custom configuration
    let mut config = AnalysisConfig::default();
    config.global.min_severity = Severity::High;
    config.row_estimation.row_thresholds.critical_row_count = 750_000;
    
    // Serialize to TOML
    let toml_config = ConfigUtils::to_toml(&config)?;
    println!("Configuration as TOML:");
    println!("{}", toml_config.lines().take(10).collect::<Vec<_>>().join("\n"));
    println!("... (truncated)\n");
    
    // Serialize to JSON  
    let json_config = ConfigUtils::to_json(&config)?;
    println!("Configuration as JSON:");
    println!("{}", json_config.lines().take(8).collect::<Vec<_>>().join("\n"));
    println!("... (truncated)\n");
    
    // Round-trip test
    let parsed_config = ConfigUtils::from_toml(&toml_config)?;
    println!("Round-trip test: {}", if config == parsed_config { "✅ Passed" } else { "❌ Failed" });
    
    // Show specialized configurations
    let high_sev_config = ConfigUtils::high_severity_only();
    let dev_config = ConfigUtils::development_mode();
    
    println!("Specialized configurations:");
    println!("  High severity only - min severity: {:?}", high_sev_config.global.min_severity);
    println!("  Development mode - min severity: {:?}", dev_config.global.min_severity);
    
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_demo_plan_creation() {
        let plan = create_problematic_plan();
        assert_eq!(plan.root.children.len(), 2);
        assert!(plan.root.is_join());
        
        // Verify we have actual data for testing estimation errors
        assert!(plan.root.actuals.is_some());
        assert!(plan.root.children[1].actuals.is_some());
    }
    
    #[test]
    fn test_analysis_finds_issues() {
        let plan = create_problematic_plan();
        let context = AnalysisContext::new();
        
        let mut engine = AnalysisEngine::new();
        engine.add_analyzer(RowEstimationAnalyzer::new());
        
        let result = engine.analyze(&plan, &context);
        
        // Should find multiple issues in our problematic plan
        assert!(result.combined_result.summary.total_findings > 0);
        assert!(result.has_critical_issues());
    }
}