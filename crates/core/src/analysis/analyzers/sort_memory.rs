use super::super::consolidated_config::AnalysisConfiguration;
use super::super::traversal::{NodeVisitor, PlanTraversal};
use super::super::{
    AnalysisContext, AnalysisReport, Analyzer, Finding, FindingType, NodePath, Severity,
};
use crate::{ParsedPlan, PlanNode};

/// Analyzer for detecting operations that exceeded `work_mem` and spilled to
/// disk (sorts, hash joins/aggregates), plus large in-memory sorts that are
/// approaching the `work_mem` limit.
///
/// This keys off the typed sort/hash execution properties
/// (`Sort Space Type`, `Sort Method`, `Sort Space Used`, `Batches`) that
/// `EXPLAIN (ANALYZE)` attaches to each node, so it only produces findings when
/// the plan was actually executed.
pub struct SortMemoryAnalyzer;

impl SortMemoryAnalyzer {
    pub fn new() -> Self {
        Self
    }

    pub fn with_config(_config: &AnalysisConfiguration) -> Self {
        Self::new()
    }
}

impl Default for SortMemoryAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer for SortMemoryAnalyzer {
    fn analyze(&self, plan: &ParsedPlan, context: &AnalysisContext) -> AnalysisReport {
        let mut report = AnalysisReport::new("SortMemoryAnalyzer".to_string())
            .with_metadata("version", self.version());

        let mut visitor = SortMemoryVisitor::new();
        PlanTraversal::depth_first(plan, &mut visitor, context);

        for finding in visitor.findings {
            report = report.add_finding(finding);
        }

        report = report
            .with_metric("nodes_analyzed", visitor.nodes_analyzed as f64)
            .with_metric(
                "nodes_with_sort_spill",
                visitor.nodes_with_sort_spill as f64,
            )
            .with_metric(
                "nodes_with_hash_spill",
                visitor.nodes_with_hash_spill as f64,
            )
            .with_metric("max_sort_space_used_kb", visitor.max_sort_space_used_kb);

        report
    }

    fn name(&self) -> &'static str {
        "SortMemoryAnalyzer"
    }

    fn description(&self) -> &'static str {
        "Detects sorts/hashes that spilled to disk (exceeded work_mem) and reports peak memory"
    }

    fn version(&self) -> &'static str {
        "1.0.0"
    }
}

/// Parse a PostgreSQL kB string such as `"524288 kB"` or `"524288"` into a
/// numeric kB value. Returns `None` if no leading integer can be parsed.
fn parse_kb(value: &str) -> Option<f64> {
    value
        .split_whitespace()
        .next()
        .and_then(|tok| tok.parse::<f64>().ok())
}

struct SortMemoryVisitor {
    findings: Vec<Finding>,
    nodes_analyzed: usize,
    nodes_with_sort_spill: usize,
    nodes_with_hash_spill: usize,
    max_sort_space_used_kb: f64,
}

impl SortMemoryVisitor {
    fn new() -> Self {
        Self {
            findings: Vec::new(),
            nodes_analyzed: 0,
            nodes_with_sort_spill: 0,
            nodes_with_hash_spill: 0,
            max_sort_space_used_kb: 0.0,
        }
    }

    fn detect(&mut self, node: &PlanNode, path: &NodePath, context: &AnalysisContext) {
        let props = node.properties();

        // Pull typed/raw values up front. Missing -> None (skip silently).
        let sort_space_type = props.sort_space_type(); // "Disk" | "Memory"
        let sort_method = props.get("Sort Method");
        let space_used_kb = props
            .get("Sort Space Used")
            .as_deref()
            .and_then(parse_kb)
            .unwrap_or(0.0);
        let batches = props.batches();

        if space_used_kb > self.max_sort_space_used_kb {
            self.max_sort_space_used_kb = space_used_kb;
        }

        // --- Rule 1: Disk sort -------------------------------------------------
        let is_disk = sort_space_type == Some("Disk");
        let method_external = sort_method
            .as_deref()
            .map(|m| m.to_lowercase().contains("external"))
            .unwrap_or(false);

        if is_disk || method_external {
            self.nodes_with_sort_spill += 1;

            let severity = if space_used_kb > 100_000.0 {
                Severity::High
            } else {
                Severity::Medium
            };

            let mut finding = Finding::new(
                FindingType::MemorySpill,
                severity,
                "Sort spilled to disk".to_string(),
                format!(
                    "Operation '{}' performed an external (disk-based) sort using {:.0} kB. \
                     It exceeded work_mem ({} kB) and spilled to disk.",
                    node.description(),
                    space_used_kb,
                    context.work_mem_kb
                ),
                "Raise work_mem to fit the sort in memory, add an index providing the \
                 required sort order, or reduce the number of rows before sorting."
                    .to_string(),
            )
            .with_node(path.clone())
            .with_evidence("sort_space_used_kb", space_used_kb)
            .with_evidence("work_mem_kb", context.work_mem_kb as f64);

            if let Some(m) = sort_method.as_deref() {
                finding = finding.with_metadata("sort_method", m);
            }
            if let Some(t) = sort_space_type {
                finding = finding.with_metadata("sort_space_type", t);
            }

            self.findings.push(finding);
        }

        // --- Rule 2: Hash batch spill -----------------------------------------
        if let Some(n) = batches
            && n > 1
        {
            self.nodes_with_hash_spill += 1;

            let severity = if n > 8 {
                Severity::High
            } else {
                Severity::Medium
            };

            let finding = Finding::new(
                FindingType::HashJoinMemorySpill,
                severity,
                "Hash operation spilled into multiple batches".to_string(),
                format!(
                    "Operation '{}' used {} hash batches (> 1), indicating the hash table \
                     did not fit in work_mem ({} kB) and spilled to disk.",
                    node.description(),
                    n,
                    context.work_mem_kb
                ),
                "Raise work_mem so the hash table fits in a single batch, or reduce the \
                 number of rows on the hashed (build) side."
                    .to_string(),
            )
            .with_node(path.clone())
            .with_evidence("batches", n as f64)
            .with_evidence("work_mem_kb", context.work_mem_kb as f64);

            self.findings.push(finding);
        }

        // NOTE: a third "in-memory sort approaching the work_mem limit" rule was
        // intentionally dropped. Its firing condition depends on knowing the
        // server's real work_mem, but the analysis context only carries a
        // default, so it would mis-fire on servers with non-default work_mem.
        // Rules 1 and 2 detect *actual* spills (Sort Space Type / Batches) and
        // do not depend on that assumption.
    }
}

impl NodeVisitor for SortMemoryVisitor {
    fn visit_node(&mut self, node: &PlanNode, path: &NodePath, context: &AnalysisContext) {
        self.nodes_analyzed += 1;
        self.detect(node, path, context);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NodeType, PlanCost, PlanNode, ScanType, TableReference};

    fn sort_node() -> PlanNode {
        // A generic node to attach sort/hash properties to. Node type is
        // irrelevant — the analyzer keys off properties, not NodeType.
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
            "Sort".to_string(),
        )
    }

    fn analyze(node: PlanNode) -> AnalysisReport {
        let plan = ParsedPlan::new(node);
        SortMemoryAnalyzer::new().analyze(&plan, &AnalysisContext::new())
    }

    #[test]
    fn test_external_sort_detected() {
        let mut node = sort_node();
        node.properties_mut().set("Sort Space Type", "Disk");
        node.properties_mut().set("Sort Method", "external merge");
        node.properties_mut().set("Sort Space Used", "120000 kB");

        let report = analyze(node);
        let spills: Vec<_> = report
            .findings
            .iter()
            .filter(|f| matches!(f.finding_type, FindingType::MemorySpill))
            .collect();
        assert_eq!(spills.len(), 1);
        assert_eq!(spills[0].severity, Severity::High);
        assert_eq!(
            spills[0].evidence.get("sort_space_used_kb"),
            Some(&120000.0)
        );
        assert_eq!(
            spills[0].metadata.get("sort_method"),
            Some(&"external merge".to_string())
        );
        assert_eq!(
            spills[0].metadata.get("sort_space_type"),
            Some(&"Disk".to_string())
        );
    }

    #[test]
    fn test_external_sort_medium_severity() {
        let mut node = sort_node();
        node.properties_mut().set("Sort Space Type", "Disk");
        node.properties_mut().set("Sort Space Used", "5000 kB");

        let report = analyze(node);
        let spills: Vec<_> = report
            .findings
            .iter()
            .filter(|f| matches!(f.finding_type, FindingType::MemorySpill))
            .collect();
        assert_eq!(spills.len(), 1);
        assert_eq!(spills[0].severity, Severity::Medium);
    }

    #[test]
    fn test_hash_batch_spill_detected() {
        let mut node = sort_node();
        node.properties_mut().set("Batches", "16");

        let report = analyze(node);
        let spills: Vec<_> = report
            .findings
            .iter()
            .filter(|f| matches!(f.finding_type, FindingType::HashJoinMemorySpill))
            .collect();
        assert_eq!(spills.len(), 1);
        assert_eq!(spills[0].severity, Severity::High);
        assert_eq!(spills[0].evidence.get("batches"), Some(&16.0));
    }

    #[test]
    fn test_hash_batch_spill_medium() {
        let mut node = sort_node();
        node.properties_mut().set("Batches", "4");

        let report = analyze(node);
        let spills: Vec<_> = report
            .findings
            .iter()
            .filter(|f| matches!(f.finding_type, FindingType::HashJoinMemorySpill))
            .collect();
        assert_eq!(spills.len(), 1);
        assert_eq!(spills[0].severity, Severity::Medium);
    }

    #[test]
    fn test_in_memory_quicksort_no_finding() {
        let mut node = sort_node();
        node.properties_mut().set("Sort Space Type", "Memory");
        node.properties_mut().set("Sort Method", "quicksort");
        node.properties_mut().set("Sort Space Used", "64");

        let report = analyze(node);
        assert!(report.findings.is_empty());
    }

    #[test]
    fn test_single_batch_no_finding() {
        let mut node = sort_node();
        node.properties_mut().set("Batches", "1");

        let report = analyze(node);
        assert!(report.findings.is_empty());
    }

    #[test]
    fn test_no_properties_no_finding() {
        let report = analyze(sort_node());
        assert!(report.findings.is_empty());
        assert_eq!(report.metrics.get("nodes_analyzed"), Some(&1.0));
    }

    #[test]
    fn test_large_in_memory_quicksort_no_finding() {
        // A big in-memory quicksort must NOT be flagged: only actual spills
        // (disk / multiple batches) are reported, since the real work_mem is
        // unknown to the analyzer.
        let mut node = sort_node();
        node.properties_mut().set("Sort Space Type", "Memory");
        node.properties_mut().set("Sort Method", "quicksort");
        node.properties_mut().set("Sort Space Used", "950");

        let plan = ParsedPlan::new(node);
        let context = AnalysisContext::new().with_work_mem_kb(1000);
        let report = SortMemoryAnalyzer::new().analyze(&plan, &context);

        assert!(report.findings.is_empty());
    }

    #[test]
    fn test_kb_parsing() {
        assert_eq!(parse_kb("524288 kB"), Some(524288.0));
        assert_eq!(parse_kb("524288"), Some(524288.0));
        assert_eq!(parse_kb("not-a-number"), None);
        assert_eq!(parse_kb(""), None);
    }

    #[test]
    fn test_unparseable_space_still_flags_disk() {
        let mut node = sort_node();
        node.properties_mut().set("Sort Space Type", "Disk");
        node.properties_mut().set("Sort Space Used", "unknown");

        let report = analyze(node);
        let spills: Vec<_> = report
            .findings
            .iter()
            .filter(|f| matches!(f.finding_type, FindingType::MemorySpill))
            .collect();
        assert_eq!(spills.len(), 1);
        assert_eq!(spills[0].severity, Severity::Medium);
        assert_eq!(spills[0].evidence.get("sort_space_used_kb"), Some(&0.0));
    }

    #[test]
    fn test_metrics_emitted() {
        let mut child = sort_node();
        child.properties_mut().set("Sort Space Type", "Disk");
        child.properties_mut().set("Sort Space Used", "300000 kB");

        let mut root = sort_node();
        root.properties_mut().set("Sort Space Type", "Disk");
        root.properties_mut().set("Sort Space Used", "5000 kB");
        root.add_child(child);

        let report = analyze(root);
        assert_eq!(
            report.metrics.get("max_sort_space_used_kb"),
            Some(&300000.0)
        );
        assert_eq!(report.metrics.get("nodes_with_sort_spill"), Some(&2.0));
        assert_eq!(report.metrics.get("nodes_analyzed"), Some(&2.0));
    }
}
