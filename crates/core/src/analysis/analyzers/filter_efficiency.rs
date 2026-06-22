use super::super::consolidated_config::AnalysisConfiguration;
use super::super::traversal::{NodeVisitor, PlanTraversal};
use super::super::{
    AnalysisContext, AnalysisReport, Analyzer, Finding, FindingType, NodePath, Severity,
};
use crate::{ParsedPlan, PlanNode};

/// Minimum number of rows discarded before a filter is worth flagging.
const MIN_ROWS_REMOVED: u64 = 10_000;
/// A filter is "wasteful" when it keeps less than this fraction of the rows it
/// inspected.
const MAX_WASTEFUL_SELECTIVITY: f64 = 0.10;

/// Analyzer that flags nodes which inspect many rows only to discard most of
/// them in a filter — the classic "scan reads 1M rows, keeps 2%" signal that
/// points at a missing/incomplete index or a filter that should be pushed down.
pub struct FilterEfficiencyAnalyzer;

impl FilterEfficiencyAnalyzer {
    pub fn new() -> Self {
        Self
    }

    pub fn with_config(_config: &AnalysisConfiguration) -> Self {
        Self::new()
    }
}

impl Default for FilterEfficiencyAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer for FilterEfficiencyAnalyzer {
    fn analyze(&self, plan: &ParsedPlan, context: &AnalysisContext) -> AnalysisReport {
        let mut report = AnalysisReport::new("FilterEfficiencyAnalyzer".to_string())
            .with_metadata("version", self.version());

        let mut visitor = FilterEfficiencyVisitor::new();
        PlanTraversal::depth_first(plan, &mut visitor, context);

        for finding in visitor.findings {
            report = report.add_finding(finding);
        }

        report = report
            .with_metric("nodes_analyzed", visitor.nodes_analyzed as f64)
            .with_metric("total_rows_removed", visitor.total_rows_removed as f64)
            .with_metric(
                "nodes_with_wasteful_filter",
                visitor.nodes_with_wasteful_filter as f64,
            )
            .with_metric("min_selectivity_seen", visitor.min_selectivity_seen);

        report
    }

    fn name(&self) -> &'static str {
        "FilterEfficiencyAnalyzer"
    }

    fn description(&self) -> &'static str {
        "Flags filters that discard most of the rows they inspect (missing/incomplete index)"
    }

    fn version(&self) -> &'static str {
        "1.0.0"
    }
}

/// Severity scaled by the absolute number of rows discarded.
fn severity_for_removed(removed: u64) -> Severity {
    if removed >= 1_000_000 {
        Severity::Critical
    } else if removed >= 100_000 {
        Severity::High
    } else {
        Severity::Medium
    }
}

struct FilterEfficiencyVisitor {
    findings: Vec<Finding>,
    nodes_analyzed: usize,
    total_rows_removed: u64,
    nodes_with_wasteful_filter: usize,
    min_selectivity_seen: f64,
}

impl FilterEfficiencyVisitor {
    fn new() -> Self {
        Self {
            findings: Vec::new(),
            nodes_analyzed: 0,
            total_rows_removed: 0,
            nodes_with_wasteful_filter: 0,
            min_selectivity_seen: 1.0,
        }
    }

    fn detect(&mut self, node: &PlanNode, path: &NodePath) {
        let props = node.properties();
        let kept = node.actuals.as_ref().and_then(|a| a.actual_rows);

        // --- Rule 1: low-selectivity (or large-absolute) WHERE filter --------
        if let Some(removed) = props.rows_removed_by_filter() {
            self.total_rows_removed = self.total_rows_removed.saturating_add(removed);

            if removed >= MIN_ROWS_REMOVED {
                // Selectivity is only meaningful when we know how many rows passed.
                let (selectivity, selectivity_known) = match kept {
                    Some(k) if k.saturating_add(removed) > 0 => {
                        (k as f64 / k.saturating_add(removed) as f64, true)
                    }
                    _ => (0.0, false),
                };

                if selectivity_known && selectivity < self.min_selectivity_seen {
                    self.min_selectivity_seen = selectivity;
                }

                // Flag when selectivity is poor, or when we can't compute it but
                // the absolute number of discarded rows is large.
                if !selectivity_known || selectivity < MAX_WASTEFUL_SELECTIVITY {
                    self.nodes_with_wasteful_filter += 1;

                    let detail = if selectivity_known {
                        format!(
                            "Operation '{}' discarded {} rows in a filter, keeping only {} \
                             ({:.1}% selectivity).",
                            node.description(),
                            removed,
                            kept.unwrap_or(0),
                            selectivity * 100.0
                        )
                    } else {
                        format!(
                            "Operation '{}' discarded {} rows in a filter (selectivity unknown \
                             — plan was not run with ANALYZE row counts).",
                            node.description(),
                            removed
                        )
                    };

                    let mut finding = Finding::new(
                        FindingType::ExcessiveRowProcessing,
                        severity_for_removed(removed),
                        "Filter discards most inspected rows".to_string(),
                        detail,
                        "Add or extend an index covering the filter predicate so these rows are \
                         never read, or push the predicate earlier in the plan."
                            .to_string(),
                    )
                    .with_node(path.clone())
                    .with_evidence("rows_removed", removed as f64);

                    if let Some(k) = kept {
                        finding = finding.with_evidence("rows_kept", k as f64);
                    }
                    if selectivity_known {
                        finding = finding.with_evidence("selectivity", selectivity);
                    }
                    if let Some(f) = props.filter() {
                        finding = finding.with_metadata("filter", f);
                    }

                    self.findings.push(finding);
                }
            }
        }

        // --- Rule 2: weak index (high recheck) --------------------------------
        if let Some(recheck) = props.rows_removed_by_index_recheck()
            && recheck >= MIN_ROWS_REMOVED
        {
            self.total_rows_removed = self.total_rows_removed.saturating_add(recheck);
            let finding = Finding::new(
                FindingType::PoorIndexSelectivity,
                Severity::Medium,
                "Imprecise index condition rechecked many rows".to_string(),
                format!(
                    "Operation '{}' removed {} rows during index recheck, meaning the index \
                     condition is not selective enough and many candidate rows had to be \
                     re-tested.",
                    node.description(),
                    recheck
                ),
                "Consider a more selective or composite index so fewer rows require rechecking."
                    .to_string(),
            )
            .with_node(path.clone())
            .with_evidence("rows_removed_by_index_recheck", recheck as f64);
            self.findings.push(finding);
        }

        // --- Rule 3: expensive join filter ------------------------------------
        if let Some(jf) = props.rows_removed_by_join_filter()
            && jf >= MIN_ROWS_REMOVED
        {
            self.total_rows_removed = self.total_rows_removed.saturating_add(jf);
            let finding = Finding::new(
                FindingType::IneffectiveJoinAlgorithm,
                Severity::Medium,
                "Join filter discards many joined rows".to_string(),
                format!(
                    "Operation '{}' produced a large intermediate result and then discarded {} \
                     rows with a join filter, indicating the join condition is not fully \
                     indexed or is over-producing rows.",
                    node.description(),
                    jf
                ),
                "Ensure the join keys are indexed and the most selective join condition drives \
                 the join, so fewer rows are produced before filtering."
                    .to_string(),
            )
            .with_node(path.clone())
            .with_evidence("rows_removed_by_join_filter", jf as f64);
            self.findings.push(finding);
        }
    }
}

impl NodeVisitor for FilterEfficiencyVisitor {
    fn visit_node(&mut self, node: &PlanNode, path: &NodePath, _context: &AnalysisContext) {
        self.nodes_analyzed += 1;
        self.detect(node, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NodeType, PlanActuals, PlanCost, PlanNode, ScanType, TableReference};

    fn scan_node() -> PlanNode {
        PlanNode::new(
            NodeType::Scan(ScanType::SeqScan {
                table: TableReference {
                    schema: None,
                    name: "t".to_string(),
                    alias: None,
                },
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 0.0,
                max_total_cost: 100.0,
                estimated_rows: 1000,
                estimated_width: 50,
            },
            "Seq Scan on t".to_string(),
        )
    }

    fn with_actual_rows(mut node: PlanNode, rows: u64) -> PlanNode {
        node.set_actuals(PlanActuals {
            actual_time_ms: Some(1.0),
            actual_rows: Some(rows),
            actual_loops: Some(1),
        });
        node
    }

    fn analyze(node: PlanNode) -> AnalysisReport {
        let plan = ParsedPlan::new(node);
        FilterEfficiencyAnalyzer::new().analyze(&plan, &AnalysisContext::new())
    }

    #[test]
    fn test_low_selectivity_seq_scan_flagged() {
        let mut node = with_actual_rows(scan_node(), 2_000);
        node.properties_mut()
            .set("Rows Removed by Filter", "998000");
        node.properties_mut().set("Filter", "(status = 'x')");

        let report = analyze(node);
        let f: Vec<_> = report
            .findings
            .iter()
            .filter(|f| matches!(f.finding_type, FindingType::ExcessiveRowProcessing))
            .collect();
        assert_eq!(f.len(), 1);
        // 998000 removed -> High (>= 100k, < 1M)
        assert_eq!(f[0].severity, Severity::High);
        assert!(f[0].evidence.get("selectivity").unwrap() < &0.10);
        assert_eq!(
            f[0].metadata.get("filter"),
            Some(&"(status = 'x')".to_string())
        );
    }

    #[test]
    fn test_critical_severity_boundary() {
        let mut node = with_actual_rows(scan_node(), 10_000);
        node.properties_mut()
            .set("Rows Removed by Filter", "2000000");

        let report = analyze(node);
        let f: Vec<_> = report
            .findings
            .iter()
            .filter(|f| matches!(f.finding_type, FindingType::ExcessiveRowProcessing))
            .collect();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].severity, Severity::Critical);
    }

    #[test]
    fn test_unknown_selectivity_large_absolute_flagged() {
        // No actuals -> selectivity unknown, but absolute removed is large.
        let mut node = scan_node();
        node.properties_mut()
            .set("Rows Removed by Filter", "500000");

        let report = analyze(node);
        let f: Vec<_> = report
            .findings
            .iter()
            .filter(|f| matches!(f.finding_type, FindingType::ExcessiveRowProcessing))
            .collect();
        assert_eq!(f.len(), 1);
        assert!(!f[0].evidence.contains_key("selectivity"));
    }

    #[test]
    fn test_index_recheck_flagged() {
        let mut node = scan_node();
        node.properties_mut()
            .set("Rows Removed by Index Recheck", "50000");

        let report = analyze(node);
        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f.finding_type, FindingType::PoorIndexSelectivity))
        );
    }

    #[test]
    fn test_join_filter_flagged() {
        let mut node = scan_node();
        node.properties_mut()
            .set("Rows Removed by Join Filter", "80000");

        let report = analyze(node);
        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f.finding_type, FindingType::IneffectiveJoinAlgorithm))
        );
    }

    #[test]
    fn test_selective_filter_no_finding() {
        // Keeps 90% -> selectivity 0.9, not wasteful.
        let mut node = with_actual_rows(scan_node(), 9_000_000);
        node.properties_mut()
            .set("Rows Removed by Filter", "1000000");

        let report = analyze(node);
        assert!(report.findings.is_empty());
    }

    #[test]
    fn test_small_absolute_removed_no_finding() {
        let mut node = with_actual_rows(scan_node(), 100);
        node.properties_mut().set("Rows Removed by Filter", "500");

        let report = analyze(node);
        assert!(report.findings.is_empty());
    }

    #[test]
    fn test_no_properties_no_finding() {
        let report = analyze(scan_node());
        assert!(report.findings.is_empty());
        assert_eq!(report.metrics.get("nodes_analyzed"), Some(&1.0));
    }

    #[test]
    fn test_zero_kept_and_removed_no_panic() {
        // actual_rows = 0 and removed below threshold -> no finding, no div panic.
        let mut node = with_actual_rows(scan_node(), 0);
        node.properties_mut().set("Rows Removed by Filter", "0");

        let report = analyze(node);
        assert!(report.findings.is_empty());
    }

    #[test]
    fn test_recheck_below_threshold_no_finding() {
        let mut node = scan_node();
        node.properties_mut()
            .set("Rows Removed by Index Recheck", "100");

        let report = analyze(node);
        assert!(report.findings.is_empty());
    }
}
