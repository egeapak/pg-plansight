use super::super::consolidated_config::AnalysisConfiguration;
use super::super::traversal::{NodeVisitor, PlanTraversal};
use super::super::{
    AnalysisContext, AnalysisReport, Analyzer, Finding, FindingType, NodePath, Severity,
};
use crate::{ParsedPlan, PlanNode};

/// Minimum heap fetches before an index-only scan is worth flagging.
const MIN_HEAP_FETCHES: u64 = 1_000;
/// Heap fetches must be at least this fraction of returned rows (when known).
const MIN_HEAP_FETCH_RATIO: f64 = 0.10;
/// Minimum absolute lossy bitmap blocks before flagging.
const MIN_LOSSY_BLOCKS: u64 = 64;
/// Lossy blocks must be at least this fraction of total bitmap blocks.
const MIN_LOSSY_FRACTION: f64 = 0.05;

/// Analyzer for two common, concretely-fixable index inefficiencies:
/// 1. An Index-Only Scan doing many heap fetches (stale visibility map → VACUUM).
/// 2. A bitmap heap scan that went lossy (bitmap exceeded work_mem → recheck).
pub struct IndexEfficiencyAnalyzer;

impl IndexEfficiencyAnalyzer {
    pub fn new() -> Self {
        Self
    }

    pub fn with_config(_config: &AnalysisConfiguration) -> Self {
        Self::new()
    }
}

impl Default for IndexEfficiencyAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer for IndexEfficiencyAnalyzer {
    fn analyze(&self, plan: &ParsedPlan, context: &AnalysisContext) -> AnalysisReport {
        let mut report = AnalysisReport::new("IndexEfficiencyAnalyzer".to_string())
            .with_metadata("version", self.version());

        let mut visitor = IndexEfficiencyVisitor::new();
        PlanTraversal::depth_first(plan, &mut visitor, context);

        for finding in visitor.findings {
            report = report.add_finding(finding);
        }

        report = report
            .with_metric("nodes_analyzed", visitor.nodes_analyzed as f64)
            .with_metric("total_heap_fetches", visitor.total_heap_fetches as f64)
            .with_metric("total_lossy_blocks", visitor.total_lossy_blocks as f64);

        report
    }

    fn name(&self) -> &'static str {
        "IndexEfficiencyAnalyzer"
    }

    fn description(&self) -> &'static str {
        "Detects index-only-scan heap fetches (stale VM) and lossy bitmap scans (work_mem)"
    }

    fn version(&self) -> &'static str {
        "1.0.0"
    }
}

struct IndexEfficiencyVisitor {
    findings: Vec<Finding>,
    nodes_analyzed: usize,
    total_heap_fetches: u64,
    total_lossy_blocks: u64,
}

impl IndexEfficiencyVisitor {
    fn new() -> Self {
        Self {
            findings: Vec::new(),
            nodes_analyzed: 0,
            total_heap_fetches: 0,
            total_lossy_blocks: 0,
        }
    }

    fn detect(&mut self, node: &PlanNode, path: &NodePath) {
        let props = node.properties();

        // --- Rule 1: index-only scan heap fetches -----------------------------
        if let Some(fetches) = props.heap_fetches() {
            self.total_heap_fetches += fetches;

            let actual_rows = node.actuals.as_ref().and_then(|a| a.actual_rows);
            // When we know the row count, only flag if fetches are a meaningful
            // fraction of it; otherwise rely on the absolute threshold alone.
            let ratio_ok = match actual_rows {
                Some(rows) if rows > 0 => fetches as f64 / rows as f64 > MIN_HEAP_FETCH_RATIO,
                _ => true,
            };

            if fetches >= MIN_HEAP_FETCHES && ratio_ok {
                let severity = if fetches >= 100_000 {
                    Severity::High
                } else {
                    Severity::Medium
                };

                let mut finding = Finding::new(
                    FindingType::Custom("IndexOnlyScanHeapFetches".to_string()),
                    severity,
                    "Index-only scan performed many heap fetches".to_string(),
                    format!(
                        "Operation '{}' did {} heap fetches during an index-only scan. A stale \
                         visibility map forces the scan to visit the heap, defeating the point \
                         of an index-only scan.",
                        node.description(),
                        fetches
                    ),
                    "Run VACUUM (or VACUUM ANALYZE) on the table to refresh the visibility map, \
                     and consider more aggressive autovacuum on this hot table."
                        .to_string(),
                )
                .with_node(path.clone())
                .with_evidence("heap_fetches", fetches as f64);

                if let Some(rows) = actual_rows {
                    finding = finding.with_evidence("actual_rows", rows as f64);
                    if rows > 0 {
                        finding =
                            finding.with_evidence("heap_fetch_ratio", fetches as f64 / rows as f64);
                    }
                }

                self.findings.push(finding);
            }
        }

        // --- Rule 2: lossy bitmap blocks --------------------------------------
        if let Some(lossy) = props.heap_blocks_lossy() {
            self.total_lossy_blocks += lossy;
            let exact = props.heap_blocks_exact().unwrap_or(0);
            let total = lossy + exact;
            let lossy_fraction = if total > 0 {
                lossy as f64 / total as f64
            } else {
                0.0
            };

            if lossy >= MIN_LOSSY_BLOCKS && lossy_fraction > MIN_LOSSY_FRACTION {
                let finding = Finding::new(
                    FindingType::MemorySpill,
                    Severity::Medium,
                    "Bitmap heap scan went lossy".to_string(),
                    format!(
                        "Operation '{}' produced {} lossy bitmap blocks ({:.0}% of {} total). The \
                         bitmap exceeded work_mem and became lossy, forcing per-row rechecks.",
                        node.description(),
                        lossy,
                        lossy_fraction * 100.0,
                        total
                    ),
                    "Raise work_mem so the bitmap stays exact, or add a more selective index to \
                     shrink the bitmap."
                        .to_string(),
                )
                .with_node(path.clone())
                .with_evidence("heap_blocks_lossy", lossy as f64)
                .with_evidence("heap_blocks_exact", exact as f64)
                .with_evidence("lossy_fraction", lossy_fraction);
                self.findings.push(finding);
            }
        }
    }
}

impl NodeVisitor for IndexEfficiencyVisitor {
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
            "Index Only Scan using ix on t".to_string(),
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
        IndexEfficiencyAnalyzer::new().analyze(&plan, &AnalysisContext::new())
    }

    #[test]
    fn test_index_only_scan_heap_fetches_flagged() {
        let mut node = with_actual_rows(scan_node(), 210_000);
        node.properties_mut().set("Heap Fetches", "200000");

        let report = analyze(node);
        let f: Vec<_> = report
            .findings
            .iter()
            .filter(
                |f| matches!(&f.finding_type, FindingType::Custom(s) if s == "IndexOnlyScanHeapFetches"),
            )
            .collect();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].severity, Severity::High);
        assert_eq!(f[0].evidence.get("heap_fetches"), Some(&200000.0));
    }

    #[test]
    fn test_index_only_scan_few_fetches_no_finding() {
        let mut node = with_actual_rows(scan_node(), 210_000);
        node.properties_mut().set("Heap Fetches", "5");

        let report = analyze(node);
        assert!(report.findings.is_empty());
    }

    #[test]
    fn test_heap_fetches_low_ratio_suppressed() {
        // 2000 fetches but 10M rows -> ratio 0.0002, below 0.10 -> not flagged.
        let mut node = with_actual_rows(scan_node(), 10_000_000);
        node.properties_mut().set("Heap Fetches", "2000");

        let report = analyze(node);
        assert!(report.findings.is_empty());
    }

    #[test]
    fn test_heap_fetches_no_actuals_uses_absolute() {
        let mut node = scan_node();
        node.properties_mut().set("Heap Fetches", "5000");

        let report = analyze(node);
        assert!(report.findings.iter().any(
            |f| matches!(&f.finding_type, FindingType::Custom(s) if s == "IndexOnlyScanHeapFetches"),
        ));
    }

    #[test]
    fn test_lossy_bitmap_flagged() {
        let mut node = scan_node();
        node.properties_mut().set("Heap Blocks: lossy", "5000");
        node.properties_mut().set("Heap Blocks: exact", "1000");

        let report = analyze(node);
        let f: Vec<_> = report
            .findings
            .iter()
            .filter(|f| matches!(f.finding_type, FindingType::MemorySpill))
            .collect();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].evidence.get("heap_blocks_lossy"), Some(&5000.0));
    }

    #[test]
    fn test_mostly_exact_bitmap_no_finding() {
        let mut node = scan_node();
        node.properties_mut().set("Heap Blocks: lossy", "2");
        node.properties_mut().set("Heap Blocks: exact", "100000");

        let report = analyze(node);
        assert!(report.findings.is_empty());
    }

    #[test]
    fn test_lossy_below_absolute_guard_no_finding() {
        // High fraction but only 10 lossy blocks (< 64 absolute guard).
        let mut node = scan_node();
        node.properties_mut().set("Heap Blocks: lossy", "10");
        node.properties_mut().set("Heap Blocks: exact", "5");

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
    fn test_metrics_emitted() {
        let mut node = scan_node();
        node.properties_mut().set("Heap Fetches", "1500");
        node.properties_mut().set("Heap Blocks: lossy", "100");
        node.properties_mut().set("Heap Blocks: exact", "100");

        let report = analyze(node);
        assert_eq!(report.metrics.get("total_heap_fetches"), Some(&1500.0));
        assert_eq!(report.metrics.get("total_lossy_blocks"), Some(&100.0));
    }
}
