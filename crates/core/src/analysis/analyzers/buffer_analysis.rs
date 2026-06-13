//! Buffer & WAL analyzer.
//!
//! Consumes the `Buffers:` and `WAL:` lines that `EXPLAIN (ANALYZE, BUFFERS,
//! WAL)` attaches to each node (captured in-process when `loganalyze.track_io`
//! is on). The parser already stores these as node properties via its generic
//! `key: value` fallback, so this analyzer only has to interpret them.
//!
//! It surfaces three classes of hot spot:
//! - **Temp-file spills** — a node read/wrote temp blocks because its data
//!   exceeded `work_mem` (sorts, hashes, large aggregates).
//! - **Heavy disk reads** — a node read many shared blocks from disk rather than
//!   cache (poor locality / cold data).
//! - **High WAL generation** — a node emitted a large volume of WAL.

use super::super::traversal::{NodeVisitor, PlanTraversal};
use super::super::{
    AnalysisContext, AnalysisReport, Analyzer, Finding, FindingType, NodePath, Severity,
};
use crate::{ParsedPlan, PlanNode};

/// PostgreSQL reports buffer counts in 8 KiB blocks.
const BLOCK_BYTES: f64 = 8192.0;

/// Flags buffer/WAL hot spots from `EXPLAIN (BUFFERS, WAL)` output.
pub struct BufferWalAnalyzer {
    /// Minimum temp blocks (read + written) to flag a spill.
    min_temp_blocks: u64,
    /// Minimum shared-read blocks to flag heavy disk reads.
    min_read_blocks: u64,
    /// Minimum WAL bytes to flag high WAL generation.
    min_wal_bytes: u64,
}

impl BufferWalAnalyzer {
    pub fn new() -> Self {
        Self {
            min_temp_blocks: 1,       // any spill is worth noting
            min_read_blocks: 1024,    // ~8 MiB read from disk
            min_wal_bytes: 1_048_576, // 1 MiB of WAL
        }
    }
}

impl Default for BufferWalAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer for BufferWalAnalyzer {
    fn analyze(&self, plan: &ParsedPlan, context: &AnalysisContext) -> AnalysisReport {
        let mut report = AnalysisReport::new("BufferWalAnalyzer".to_string())
            .with_metadata("version", self.version());

        let mut visitor = BufferWalVisitor {
            cfg: self,
            findings: Vec::new(),
            nodes: 0,
            temp_blocks: 0,
            read_blocks: 0,
            wal_bytes: 0,
        };
        PlanTraversal::depth_first(plan, &mut visitor, context);

        let (nodes, temp_blocks, read_blocks, wal_bytes) = (
            visitor.nodes,
            visitor.temp_blocks,
            visitor.read_blocks,
            visitor.wal_bytes,
        );
        for finding in visitor.findings {
            report = report.add_finding(finding);
        }
        report
            .with_metric("nodes_analyzed", nodes as f64)
            .with_metric("temp_blocks", temp_blocks as f64)
            .with_metric("shared_read_blocks", read_blocks as f64)
            .with_metric("wal_bytes", wal_bytes as f64)
    }

    fn name(&self) -> &'static str {
        "BufferWalAnalyzer"
    }

    fn description(&self) -> &'static str {
        "Flags temp-file spills, heavy disk reads, and high WAL generation from EXPLAIN BUFFERS/WAL data"
    }
}

struct BufferWalVisitor<'a> {
    cfg: &'a BufferWalAnalyzer,
    findings: Vec<Finding>,
    nodes: u64,
    temp_blocks: u64,
    read_blocks: u64,
    wal_bytes: u64,
}

impl NodeVisitor for BufferWalVisitor<'_> {
    fn visit_node(&mut self, node: &PlanNode, path: &NodePath, _context: &AnalysisContext) {
        self.nodes += 1;
        let label = node_label(node);

        if let Some(raw) = node.properties.get("Buffers") {
            let b = parse_buffers(&raw);
            let temp = b.temp_read + b.temp_written;
            self.temp_blocks += temp;
            self.read_blocks += b.shared_read;

            if temp >= self.cfg.min_temp_blocks {
                let mb = temp as f64 * BLOCK_BYTES / 1_000_000.0;
                let severity = if mb >= 100.0 {
                    Severity::Critical
                } else if mb >= 10.0 {
                    Severity::High
                } else if mb >= 1.0 {
                    Severity::Medium
                } else {
                    Severity::Low
                };
                self.findings.push(
                    Finding::new(
                        FindingType::MemorySpill,
                        severity,
                        format!("{label} spilled ~{mb:.1} MB to temp files"),
                        format!(
                            "This node read/wrote {temp} temp blocks (~{mb:.1} MB) because its \
                             working set exceeded work_mem and spilled to disk."
                        ),
                        "Raise work_mem for this query or session, or reduce the rows that must be \
                         sorted/hashed/grouped (tighter filters or a supporting index)."
                            .to_string(),
                    )
                    .with_node(path.clone())
                    .with_evidence("temp_blocks", temp as f64)
                    .with_evidence("temp_mb", mb)
                    .with_metadata("node", &label),
                );
            }

            if b.shared_read >= self.cfg.min_read_blocks {
                let mb = b.shared_read as f64 * BLOCK_BYTES / 1_000_000.0;
                let total = b.shared_hit + b.shared_read;
                let hit_ratio = if total > 0 {
                    b.shared_hit as f64 / total as f64
                } else {
                    1.0
                };
                let severity = if mb >= 512.0 {
                    Severity::High
                } else if mb >= 64.0 {
                    Severity::Medium
                } else {
                    Severity::Low
                };
                self.findings.push(
                    Finding::new(
                        FindingType::Custom("HighBufferReads".to_string()),
                        severity,
                        format!(
                            "{label} read ~{mb:.1} MB from disk ({:.0}% cache hit)",
                            hit_ratio * 100.0
                        ),
                        format!(
                            "This node read {} shared blocks (~{mb:.1} MB) from disk rather than \
                             cache (buffer hit ratio {:.0}%).",
                            b.shared_read,
                            hit_ratio * 100.0
                        ),
                        "Hot data may not fit in shared_buffers/OS cache; consider more memory, or \
                         touch fewer blocks via a more selective index or filter."
                            .to_string(),
                    )
                    .with_node(path.clone())
                    .with_evidence("shared_read_blocks", b.shared_read as f64)
                    .with_evidence("read_mb", mb)
                    .with_evidence("cache_hit_ratio", hit_ratio)
                    .with_metadata("node", &label),
                );
            }
        }

        if let Some(raw) = node.properties.get("WAL") {
            let bytes = parse_wal_bytes(&raw);
            self.wal_bytes += bytes;
            if bytes >= self.cfg.min_wal_bytes {
                let mb = bytes as f64 / 1_000_000.0;
                let severity = if mb >= 64.0 {
                    Severity::High
                } else if mb >= 8.0 {
                    Severity::Medium
                } else {
                    Severity::Low
                };
                self.findings.push(
                    Finding::new(
                        FindingType::Custom("HighWalVolume".to_string()),
                        severity,
                        format!("{label} generated ~{mb:.1} MB of WAL"),
                        format!("This node emitted {bytes} bytes (~{mb:.1} MB) of WAL."),
                        "Write-heavy step: batch the writes, reduce touched rows, or expect the \
                         replication/IO cost this implies."
                            .to_string(),
                    )
                    .with_node(path.clone())
                    .with_evidence("wal_bytes", bytes as f64)
                    .with_metadata("node", &label),
                );
            }
        }
    }
}

/// Short human label for a node, taken from its plan line (before the cost).
fn node_label(node: &PlanNode) -> String {
    let head = node
        .original_text
        .split("  (cost")
        .next()
        .unwrap_or("")
        .trim();
    if head.is_empty() {
        "node".to_string()
    } else {
        head.to_string()
    }
}

#[derive(Default)]
struct Buffers {
    shared_hit: u64,
    shared_read: u64,
    temp_read: u64,
    temp_written: u64,
}

/// Parse a `Buffers:` value such as
/// `shared hit=3 read=350 dirtied=2, temp read=483 written=525`.
fn parse_buffers(s: &str) -> Buffers {
    let mut b = Buffers::default();
    let mut group = "";
    for tok in s.split([' ', ',']).filter(|t| !t.is_empty()) {
        match tok {
            "shared" | "local" | "temp" => group = tok,
            _ => {
                if let Some((key, val)) = tok.split_once('=')
                    && let Ok(n) = val.parse::<u64>()
                {
                    match (group, key) {
                        ("shared", "hit") => b.shared_hit = n,
                        ("shared", "read") => b.shared_read = n,
                        ("temp", "read") => b.temp_read = n,
                        ("temp", "written") => b.temp_written = n,
                        _ => {}
                    }
                }
            }
        }
    }
    b
}

/// Parse the `bytes=N` field from a `WAL:` value such as
/// `records=525 fpi=3 bytes=98765`.
fn parse_wal_bytes(s: &str) -> u64 {
    s.split_whitespace()
        .find_map(|tok| {
            tok.strip_prefix("bytes=")
                .and_then(|v| v.parse::<u64>().ok())
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_shared_and_temp_buffers() {
        let b = parse_buffers("shared hit=3 read=350 dirtied=2, temp read=483 written=525");
        assert_eq!(b.shared_hit, 3);
        assert_eq!(b.shared_read, 350);
        assert_eq!(b.temp_read, 483);
        assert_eq!(b.temp_written, 525);
    }

    #[test]
    fn parses_wal_bytes() {
        assert_eq!(parse_wal_bytes("records=525 fpi=3 bytes=98765"), 98765);
        assert_eq!(parse_wal_bytes("records=0"), 0);
    }

    #[test]
    fn flags_temp_spill() {
        let cost = crate::plan_parser::PlanCost {
            startup_cost: 1.0,
            min_total_cost: 2.0,
            max_total_cost: 2.0,
            estimated_rows: 1,
            estimated_width: 1,
        };
        let mut node = PlanNode::new(
            crate::NodeType::Unknown("Sort".to_string()),
            cost,
            "Sort  (cost=1.0..2.0 rows=1 width=1)".to_string(),
        );
        // ~8 MB of temp spill.
        node.set_property(
            "Buffers".to_string(),
            "shared hit=1, temp read=512 written=512".to_string(),
        );
        let plan = ParsedPlan {
            root: node,
            planning_time_ms: None,
            execution_time_ms: None,
        };
        let report = BufferWalAnalyzer::new().analyze(&plan, &AnalysisContext::new());
        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f.finding_type, FindingType::MemorySpill)),
            "expected a MemorySpill finding from temp buffers"
        );
    }
}
