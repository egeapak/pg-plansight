use super::super::consolidated_config::AnalysisConfiguration;
use super::super::traversal::{NodeVisitor, PlanTraversal};
use super::super::{
    AnalysisContext, AnalysisReport, Analyzer, ConfigurableAnalyzer, Finding, FindingType,
    NodePath, Severity,
};
use crate::{ParsedPlan, PlanNode};

/// Configuration for resource utilization analysis
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceAnalysisConfig {
    /// Buffer hit ratio threshold (as decimal, e.g., 0.95 for 95%)
    pub min_buffer_hit_ratio: f64,
    /// I/O wait threshold (seconds)
    pub max_io_wait_seconds: f64,
    /// Memory utilization threshold (as ratio of work_mem)
    pub memory_pressure_threshold: f64,
    /// CPU utilization threshold (percentage)
    pub cpu_threshold_percent: f64,
}

impl Default for ResourceAnalysisConfig {
    fn default() -> Self {
        Self {
            min_buffer_hit_ratio: 0.95, // 95% buffer hits expected
            max_io_wait_seconds: 0.5,
            memory_pressure_threshold: 0.9, // 90% of work_mem
            cpu_threshold_percent: 80.0,
        }
    }
}

/// Analyzer for resource utilization patterns
pub struct ResourceAnalyzer {
    config: ResourceAnalysisConfig,
}

impl ResourceAnalyzer {
    pub fn new() -> Self {
        Self {
            config: ResourceAnalysisConfig::default(),
        }
    }

    pub fn with_config(_config: &AnalysisConfiguration) -> Self {
        Self {
            config: ResourceAnalysisConfig::default(),
        }
    }
}

impl Default for ResourceAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer for ResourceAnalyzer {
    fn analyze(&self, plan: &ParsedPlan, context: &AnalysisContext) -> AnalysisReport {
        let mut report = AnalysisReport::new("ResourceAnalyzer".to_string())
            .with_metadata("version", self.version());

        // Create a visitor to collect resource-related findings
        let mut visitor = ResourceVisitor::new(&self.config, context);
        PlanTraversal::depth_first(plan, &mut visitor, context);

        // Add all findings to the report
        for finding in visitor.findings {
            report = report.add_finding(finding);
        }

        // Add aggregate metrics
        report = report
            .with_metric("nodes_analyzed", visitor.nodes_analyzed as f64)
            .with_metric(
                "total_buffer_accesses",
                visitor.total_buffer_accesses as f64,
            )
            .with_metric("total_buffer_hits", visitor.total_buffer_hits as f64);

        if visitor.total_buffer_accesses > 0 {
            let hit_ratio = visitor.total_buffer_hits as f64 / visitor.total_buffer_accesses as f64;
            report = report.with_metric("buffer_hit_ratio", hit_ratio);
        }

        report
    }

    fn name(&self) -> &'static str {
        "ResourceAnalyzer"
    }

    fn description(&self) -> &'static str {
        "Analyzes resource utilization patterns including buffer cache, I/O, and memory usage"
    }

    fn version(&self) -> &'static str {
        "1.0.0"
    }
}

impl ConfigurableAnalyzer for ResourceAnalyzer {
    type Config = ResourceAnalysisConfig;

    fn configure(&mut self, config: Self::Config) {
        self.config = config;
    }

    fn default_config() -> Self::Config {
        ResourceAnalysisConfig::default()
    }

    fn current_config(&self) -> &Self::Config {
        &self.config
    }
}

/// Visitor implementation for collecting resource-related findings
struct ResourceVisitor<'a> {
    config: &'a ResourceAnalysisConfig,
    context: &'a AnalysisContext,
    findings: Vec<Finding>,
    // Metrics
    nodes_analyzed: usize,
    total_buffer_accesses: u64,
    total_buffer_hits: u64,
}

impl<'a> ResourceVisitor<'a> {
    fn new(config: &'a ResourceAnalysisConfig, context: &'a AnalysisContext) -> Self {
        Self {
            config,
            context,
            findings: Vec::new(),
            nodes_analyzed: 0,
            total_buffer_accesses: 0,
            total_buffer_hits: 0,
        }
    }

    fn analyze_buffer_usage(&mut self, node: &PlanNode, path: &NodePath) {
        // Check for buffer-related properties in the node
        if let Some(shared_hit_str) = node.get_property("Shared Hit Blocks") {
            if let Ok(shared_hits) = shared_hit_str.parse::<u64>() {
                self.total_buffer_hits += shared_hits;
            }
        }

        if let Some(shared_read_str) = node.get_property("Shared Read Blocks") {
            if let Ok(shared_reads) = shared_read_str.parse::<u64>() {
                self.total_buffer_accesses += shared_reads;
            }
        }

        // Calculate local buffer hit ratio if we have both hits and reads
        if let (Some(hits_str), Some(reads_str)) = (
            node.get_property("Shared Hit Blocks"),
            node.get_property("Shared Read Blocks"),
        ) {
            if let (Ok(hits), Ok(reads)) = (hits_str.parse::<u64>(), reads_str.parse::<u64>()) {
                let total = hits + reads;
                if total > 0 {
                    let hit_ratio = hits as f64 / total as f64;

                    if hit_ratio < self.config.min_buffer_hit_ratio && total > 1000 {
                        let finding = Finding::new(
                            FindingType::Custom("LowBufferHitRatio".to_string()),
                            if hit_ratio < 0.80 { Severity::High } else { Severity::Medium },
                            format!("Low buffer cache hit ratio ({:.1}%)", hit_ratio * 100.0),
                            format!(
                                "Operation {} has a buffer hit ratio of {:.1}%, which is below the recommended {}%. This suggests excessive disk I/O.",
                                node.description(), hit_ratio * 100.0, self.config.min_buffer_hit_ratio * 100.0
                            ),
                            "Consider increasing shared_buffers, optimizing queries to access less data, or adding appropriate indexes".to_string(),
                        )
                        .with_node(path.clone())
                        .with_evidence("buffer_hit_ratio", hit_ratio)
                        .with_evidence("buffer_hits", hits as f64)
                        .with_evidence("buffer_reads", reads as f64)
                        .with_evidence("total_accesses", total as f64);

                        self.findings.push(finding);
                    }
                }
            }
        }
    }

    fn analyze_io_patterns(&mut self, node: &PlanNode, path: &NodePath) {
        // Look for I/O Wait time in node properties
        if let Some(io_wait_str) = node.get_property("I/O Wait Time") {
            if let Ok(io_wait_ms) = io_wait_str.parse::<f64>() {
                let io_wait_seconds = io_wait_ms / 1000.0;

                if io_wait_seconds > self.config.max_io_wait_seconds {
                    let finding = Finding::new(
                        FindingType::Custom("ExcessiveIOWait".to_string()),
                        if io_wait_seconds > 5.0 { Severity::Critical }
                        else if io_wait_seconds > 2.0 { Severity::High }
                        else { Severity::Medium },
                        "Excessive I/O wait time detected".to_string(),
                        format!(
                            "Operation {} spent {:.2} seconds waiting for I/O, indicating storage bottleneck",
                            node.description(), io_wait_seconds
                        ),
                        "Investigate storage performance, consider faster disks, or optimize query to reduce data access".to_string(),
                    )
                    .with_node(path.clone())
                    .with_evidence("io_wait_seconds", io_wait_seconds)
                    .with_metadata("operation_type", &node.description());

                    self.findings.push(finding);
                }
            }
        }
    }

    fn analyze_memory_pressure(&mut self, node: &PlanNode, path: &NodePath) {
        // Check if node uses significant memory relative to work_mem
        if let Some(peak_memory_str) = node.get_property("Peak Memory Usage") {
            if let Ok(peak_memory_kb) = peak_memory_str.parse::<u64>() {
                let work_mem_kb = self.context.work_mem_kb as f64;
                let memory_ratio = peak_memory_kb as f64 / work_mem_kb;

                if memory_ratio > self.config.memory_pressure_threshold {
                    let finding = Finding::new(
                        FindingType::Custom("MemoryPressure".to_string()),
                        if memory_ratio > 2.0 { Severity::Critical }
                        else if memory_ratio > 1.5 { Severity::High }
                        else { Severity::Medium },
                        "High memory pressure detected".to_string(),
                        format!(
                            "Operation {} using {} KB of memory ({:.1}% of work_mem = {} KB)",
                            node.description(), peak_memory_kb, memory_ratio * 100.0, work_mem_kb
                        ),
                        "Consider increasing work_mem or optimizing the query to reduce memory requirements".to_string(),
                    )
                    .with_node(path.clone())
                    .with_evidence("peak_memory_kb", peak_memory_kb as f64)
                    .with_evidence("work_mem_kb", work_mem_kb)
                    .with_evidence("memory_ratio", memory_ratio);

                    self.findings.push(finding);
                }
            }
        }
    }
}

impl<'a> NodeVisitor for ResourceVisitor<'a> {
    fn visit_node(&mut self, node: &PlanNode, path: &NodePath, _context: &AnalysisContext) {
        self.nodes_analyzed += 1;

        self.analyze_buffer_usage(node, path);
        self.analyze_io_patterns(node, path);
        self.analyze_memory_pressure(node, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NodeType, PlanCost, PlanNode, ScanType, TableReference};

    #[test]
    fn test_resource_analyzer_basic() {
        let config = AnalysisConfiguration::default();
        let analyzer = ResourceAnalyzer::with_config(&config);
        let context = AnalysisContext::new();

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
                max_total_cost: 1000.0,
                estimated_rows: 10000,
                estimated_width: 100,
            },
            "Seq Scan on test_table".to_string(),
        );

        let plan = ParsedPlan::new(node);
        let report = analyzer.analyze(&plan, &context);

        assert_eq!(report.analyzer_name, "ResourceAnalyzer");
        assert!(report.metrics.contains_key("nodes_analyzed"));
    }

    #[test]
    fn test_analyzer_metrics() {
        let analyzer = ResourceAnalyzer::new();
        let context = AnalysisContext::new();

        let node = PlanNode::new(
            NodeType::Scan(ScanType::SeqScan {
                table: TableReference {
                    schema: None,
                    name: "test".to_string(),
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
            "Seq Scan".to_string(),
        );

        let plan = ParsedPlan::new(node);
        let report = analyzer.analyze(&plan, &context);

        // Should collect metrics
        assert_eq!(report.metrics.get("nodes_analyzed"), Some(&1.0));
    }
}
