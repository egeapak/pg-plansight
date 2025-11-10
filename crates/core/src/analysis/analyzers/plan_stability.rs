use super::super::consolidated_config::AnalysisConfiguration;
use super::super::{
    AnalysisContext, AnalysisReport, Analyzer, ConfigurableAnalyzer, Finding, FindingType, Severity,
};
use crate::{NodeType, ParsedPlan, PlanNode};
use std::collections::HashMap;

/// Configuration for plan stability analysis
#[derive(Debug, Clone, PartialEq)]
pub struct PlanStabilityConfig {
    /// Minimum number of plans to detect instability
    pub min_plans_for_analysis: usize,
    /// Threshold for considering cost difference significant (ratio)
    pub cost_variance_threshold: f64,
    /// Threshold for considering row estimate variance significant (ratio)
    pub row_estimate_variance_threshold: f64,
}

impl Default for PlanStabilityConfig {
    fn default() -> Self {
        Self {
            min_plans_for_analysis: 3,
            cost_variance_threshold: 2.0,         // 2x difference
            row_estimate_variance_threshold: 3.0, // 3x difference
        }
    }
}

/// Analyzer for detecting plan stability issues
///
/// This analyzer requires multiple executions of the same query to detect:
/// - Plan flipping (when the same query gets different plans)
/// - Cardinality estimate instability
/// - Parameter sniffing issues
pub struct PlanStabilityAnalyzer {
    config: PlanStabilityConfig,
    /// Historical plans keyed by normalized query
    plan_history: HashMap<String, Vec<PlanSnapshot>>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct PlanSnapshot {
    plan_signature: String,
    total_cost: f64,
    estimated_rows: u64,
    primary_node_type: String,
    join_count: usize,
    index_scan_count: usize,
    seq_scan_count: usize,
}

impl PlanSnapshot {
    fn from_plan(plan: &ParsedPlan) -> Self {
        let mut snapshot = Self {
            plan_signature: Self::compute_signature(&plan.root),
            total_cost: plan.root.cost.max_total_cost,
            estimated_rows: plan.root.cost.estimated_rows,
            primary_node_type: format!("{:?}", plan.root.node_type),
            join_count: 0,
            index_scan_count: 0,
            seq_scan_count: 0,
        };

        Self::collect_node_stats(&plan.root, &mut snapshot);
        snapshot
    }

    fn compute_signature(node: &PlanNode) -> String {
        let mut parts = vec![format!("{:?}", node.node_type)];

        for child in &node.children {
            parts.push(Self::compute_signature(child));
        }

        parts.join("|")
    }

    fn collect_node_stats(node: &PlanNode, snapshot: &mut PlanSnapshot) {
        match &node.node_type {
            NodeType::Join(_) => snapshot.join_count += 1,
            NodeType::Scan(scan_type) => {
                use crate::ScanType;
                match scan_type {
                    ScanType::IndexScan { .. } => snapshot.index_scan_count += 1,
                    ScanType::SeqScan { .. } => snapshot.seq_scan_count += 1,
                    _ => {}
                }
            }
            _ => {}
        }

        for child in &node.children {
            Self::collect_node_stats(child, snapshot);
        }
    }

    #[allow(dead_code)]
    fn is_similar_to(&self, other: &Self) -> bool {
        self.plan_signature == other.plan_signature
    }

    #[allow(dead_code)]
    fn cost_ratio(&self, other: &Self) -> f64 {
        if other.total_cost > 0.0 {
            self.total_cost / other.total_cost
        } else {
            1.0
        }
    }

    #[allow(dead_code)]
    fn row_estimate_ratio(&self, other: &Self) -> f64 {
        if other.estimated_rows > 0 {
            self.estimated_rows as f64 / other.estimated_rows as f64
        } else {
            1.0
        }
    }
}

impl PlanStabilityAnalyzer {
    pub fn new() -> Self {
        Self {
            config: PlanStabilityConfig::default(),
            plan_history: HashMap::new(),
        }
    }

    pub fn with_config(_config: &AnalysisConfiguration) -> Self {
        Self {
            config: PlanStabilityConfig::default(),
            plan_history: HashMap::new(),
        }
    }

    /// Record a plan execution for stability analysis
    pub fn record_plan(&mut self, query_fingerprint: String, plan: &ParsedPlan) {
        let snapshot = PlanSnapshot::from_plan(plan);
        let history = self.plan_history.entry(query_fingerprint).or_default();

        history.push(snapshot);

        // Keep only recent history (last 100 plans per query)
        if history.len() > 100 {
            history.remove(0);
        }
    }

    fn detect_plan_flipping(&self, query_fingerprint: &str) -> Option<Finding> {
        let history = self.plan_history.get(query_fingerprint)?;

        if history.len() < self.config.min_plans_for_analysis {
            return None;
        }

        // Check if we have multiple distinct plan signatures
        let mut signatures = HashMap::new();
        for snapshot in history {
            *signatures.entry(&snapshot.plan_signature).or_insert(0) += 1;
        }

        if signatures.len() > 1 {
            let total = history.len();
            let mut signature_details: Vec<_> = signatures
                .iter()
                .map(|(sig, count)| (sig, *count, (*count as f64 / total as f64) * 100.0))
                .collect();
            signature_details.sort_by(|a, b| b.1.cmp(&a.1));

            let primary_plan_pct = signature_details[0].2;
            let severity = if primary_plan_pct < 60.0 {
                Severity::Critical // Very unstable
            } else if primary_plan_pct < 80.0 {
                Severity::High
            } else {
                Severity::Medium
            };

            Some(Finding::new(
                FindingType::Custom("PlanFlipping".to_string()),
                severity,
                format!("Plan instability detected ({} different plans)", signatures.len()),
                format!(
                    "Query has used {} different execution plans across {} executions. Primary plan used {:.1}% of the time.",
                    signatures.len(), total, primary_plan_pct
                ),
                "Investigate parameter values, statistics freshness, or consider plan guides. This may indicate parameter sniffing issues.".to_string(),
            )
            .with_evidence("distinct_plans", signatures.len() as f64)
            .with_evidence("total_executions", total as f64)
            .with_evidence("primary_plan_percentage", primary_plan_pct))
        } else {
            None
        }
    }

    fn detect_cost_instability(&self, query_fingerprint: &str) -> Option<Finding> {
        let history = self.plan_history.get(query_fingerprint)?;

        if history.len() < self.config.min_plans_for_analysis {
            return None;
        }

        // Calculate cost variance
        let costs: Vec<f64> = history.iter().map(|s| s.total_cost).collect();
        let min_cost = costs.iter().copied().fold(f64::INFINITY, f64::min);
        let max_cost = costs.iter().copied().fold(f64::NEG_INFINITY, f64::max);

        if min_cost > 0.0 {
            let cost_ratio = max_cost / min_cost;

            if cost_ratio > self.config.cost_variance_threshold {
                Some(Finding::new(
                    FindingType::Custom("CostInstability".to_string()),
                    if cost_ratio > 10.0 { Severity::High } else { Severity::Medium },
                    format!("Highly variable cost estimates ({:.1}x range)", cost_ratio),
                    format!(
                        "Query shows cost estimates ranging from {:.0} to {:.0} ({:.1}x variance). This suggests unstable statistics or parameter sensitivity.",
                        min_cost, max_cost, cost_ratio
                    ),
                    "Run ANALYZE on involved tables and consider using prepared statements with stable parameters".to_string(),
                )
                .with_evidence("min_cost", min_cost)
                .with_evidence("max_cost", max_cost)
                .with_evidence("cost_variance_ratio", cost_ratio)
                .with_evidence("sample_size", history.len() as f64))
            } else {
                None
            }
        } else {
            None
        }
    }

    fn detect_cardinality_instability(&self, query_fingerprint: &str) -> Option<Finding> {
        let history = self.plan_history.get(query_fingerprint)?;

        if history.len() < self.config.min_plans_for_analysis {
            return None;
        }

        // Calculate row estimate variance
        let estimates: Vec<u64> = history.iter().map(|s| s.estimated_rows).collect();
        let min_estimate = *estimates.iter().min()?;
        let max_estimate = *estimates.iter().max()?;

        if min_estimate > 0 {
            let estimate_ratio = max_estimate as f64 / min_estimate as f64;

            if estimate_ratio > self.config.row_estimate_variance_threshold {
                Some(Finding::new(
                    FindingType::Custom("CardinalityInstability".to_string()),
                    if estimate_ratio > 100.0 { Severity::Critical }
                    else if estimate_ratio > 10.0 { Severity::High }
                    else { Severity::Medium },
                    format!("Unstable cardinality estimates ({:.1}x range)", estimate_ratio),
                    format!(
                        "Query shows row estimates ranging from {} to {} ({:.1}x variance). This indicates poor statistics or parameter sniffing.",
                        min_estimate, max_estimate, estimate_ratio
                    ),
                    "This is often caused by: 1) Stale statistics 2) Highly skewed data 3) Parameter-dependent selectivity. Run ANALYZE and consider histogram adjustments.".to_string(),
                )
                .with_evidence("min_estimated_rows", min_estimate as f64)
                .with_evidence("max_estimated_rows", max_estimate as f64)
                .with_evidence("estimate_variance_ratio", estimate_ratio)
                .with_evidence("sample_size", history.len() as f64))
            } else {
                None
            }
        } else {
            None
        }
    }
}

impl Default for PlanStabilityAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer for PlanStabilityAnalyzer {
    fn analyze(&self, _plan: &ParsedPlan, _context: &AnalysisContext) -> AnalysisReport {
        let mut report = AnalysisReport::new("PlanStabilityAnalyzer".to_string())
            .with_metadata("version", self.version());

        // Analyze all tracked queries for stability issues
        for query_fingerprint in self.plan_history.keys() {
            if let Some(finding) = self.detect_plan_flipping(query_fingerprint) {
                report = report.add_finding(finding);
            }

            if let Some(finding) = self.detect_cost_instability(query_fingerprint) {
                report = report.add_finding(finding);
            }

            if let Some(finding) = self.detect_cardinality_instability(query_fingerprint) {
                report = report.add_finding(finding);
            }
        }

        report = report
            .with_metric("tracked_queries", self.plan_history.len() as f64)
            .with_metric(
                "total_plan_executions",
                self.plan_history.values().map(|v| v.len()).sum::<usize>() as f64,
            );

        report
    }

    fn name(&self) -> &'static str {
        "PlanStabilityAnalyzer"
    }

    fn description(&self) -> &'static str {
        "Detects plan instability, flipping, and cardinality estimate variance across executions"
    }

    fn version(&self) -> &'static str {
        "1.0.0"
    }
}

impl ConfigurableAnalyzer for PlanStabilityAnalyzer {
    type Config = PlanStabilityConfig;

    fn configure(&mut self, config: Self::Config) {
        self.config = config;
    }

    fn default_config() -> Self::Config {
        PlanStabilityConfig::default()
    }

    fn current_config(&self) -> &Self::Config {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{JoinType, NodeType, PlanCost, PlanNode, ScanType, TableReference};

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

    fn create_hash_join_plan(rows: u64, cost: f64) -> ParsedPlan {
        let left = PlanNode::new(
            NodeType::Scan(ScanType::SeqScan {
                table: TableReference {
                    schema: None,
                    name: "t1".to_string(),
                    alias: None,
                },
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: cost / 2.0,
                estimated_rows: rows,
                estimated_width: 50,
            },
            "Scan t1".to_string(),
        );

        let right = PlanNode::new(
            NodeType::Scan(ScanType::SeqScan {
                table: TableReference {
                    schema: None,
                    name: "t2".to_string(),
                    alias: None,
                },
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: cost / 2.0,
                estimated_rows: rows,
                estimated_width: 50,
            },
            "Scan t2".to_string(),
        );

        let mut join = PlanNode::new(
            NodeType::Join(JoinType::HashJoin {
                hash_condition: Some("t1.id = t2.id".to_string()),
                hash_buckets: None,
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: cost,
                estimated_rows: rows,
                estimated_width: 100,
            },
            "Hash Join".to_string(),
        );

        join.add_child(left);
        join.add_child(right);
        ParsedPlan::new(join)
    }

    #[test]
    fn test_stable_plans_no_findings() {
        let mut analyzer = PlanStabilityAnalyzer::new();
        let context = AnalysisContext::new();

        // Record same plan multiple times
        for _ in 0..5 {
            let plan = create_seq_scan_plan(1000, 100.0);
            analyzer.record_plan("SELECT * FROM test_table".to_string(), &plan);
        }

        let plan = create_seq_scan_plan(1000, 100.0);
        let report = analyzer.analyze(&plan, &context);

        // Should have no findings for stable plans
        assert_eq!(report.findings.len(), 0);
        assert_eq!(report.metrics.get("tracked_queries"), Some(&1.0));
    }

    #[test]
    fn test_plan_flipping_detection() {
        let mut analyzer = PlanStabilityAnalyzer::new();
        let context = AnalysisContext::new();

        let query = "SELECT * FROM t1 JOIN t2 ON t1.id = t2.id".to_string();

        // Record different plans for the same query
        for _ in 0..2 {
            let plan = create_seq_scan_plan(1000, 100.0);
            analyzer.record_plan(query.clone(), &plan);
        }

        for _ in 0..2 {
            let plan = create_hash_join_plan(1000, 100.0);
            analyzer.record_plan(query.clone(), &plan);
        }

        let plan = create_seq_scan_plan(1000, 100.0);
        let report = analyzer.analyze(&plan, &context);

        // Should detect plan flipping
        assert!(
            report.findings.iter().any(
                |f| matches!(f.finding_type, FindingType::Custom(ref s) if s == "PlanFlipping")
            )
        );
    }

    #[test]
    fn test_cost_instability_detection() {
        let mut analyzer = PlanStabilityAnalyzer::new();
        let context = AnalysisContext::new();

        let query = "SELECT * FROM test_table".to_string();

        // Record plans with wildly different costs
        analyzer.record_plan(query.clone(), &create_seq_scan_plan(1000, 100.0));
        analyzer.record_plan(query.clone(), &create_seq_scan_plan(1000, 500.0));
        analyzer.record_plan(query.clone(), &create_seq_scan_plan(1000, 1000.0));

        let plan = create_seq_scan_plan(1000, 100.0);
        let report = analyzer.analyze(&plan, &context);

        // Should detect cost instability
        assert!(report.findings.iter().any(
            |f| matches!(f.finding_type, FindingType::Custom(ref s) if s == "CostInstability")
        ));
    }

    #[test]
    fn test_cardinality_instability_detection() {
        let mut analyzer = PlanStabilityAnalyzer::new();
        let context = AnalysisContext::new();

        let query = "SELECT * FROM test_table WHERE param = $1".to_string();

        // Record plans with vastly different row estimates (parameter sniffing)
        analyzer.record_plan(query.clone(), &create_seq_scan_plan(10, 100.0));
        analyzer.record_plan(query.clone(), &create_seq_scan_plan(100, 500.0));
        analyzer.record_plan(query.clone(), &create_seq_scan_plan(10000, 5000.0));

        let plan = create_seq_scan_plan(10, 100.0);
        let report = analyzer.analyze(&plan, &context);

        // Should detect cardinality instability
        assert!(report.findings.iter().any(|f|
            matches!(f.finding_type, FindingType::Custom(ref s) if s == "CardinalityInstability")
        ));
    }
}
