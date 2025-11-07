use crate::{ParsedPlan, PlanNode};
use super::super::{
    Analyzer, ConfigurableAnalyzer, AnalysisContext, AnalysisReport, Finding,
    FindingType, Severity, NodePath
};
use super::super::consolidated_config::AnalysisConfiguration;
use chrono::{DateTime, Utc};
use std::collections::HashMap;

/// Configuration for temporal analysis
#[derive(Debug, Clone, PartialEq)]
pub struct TemporalAnalysisConfig {
    /// Minimum number of data points required for analysis
    pub min_data_points: usize,
    /// Threshold for detecting performance degradation (as ratio)
    pub degradation_threshold: f64,
    /// Threshold for detecting spikes (as ratio from baseline)
    pub spike_threshold: f64,
    /// Window size for moving averages
    pub moving_average_window: usize,
}

impl Default for TemporalAnalysisConfig {
    fn default() -> Self {
        Self {
            min_data_points: 10,
            degradation_threshold: 1.5, // 50% slower than baseline
            spike_threshold: 3.0,       // 3x slower than average
            moving_average_window: 5,
        }
    }
}

/// Analyzer for detecting temporal performance patterns
pub struct TemporalAnalyzer {
    config: TemporalAnalysisConfig,
    // Historical data for comparison
    history: Vec<PerformancePoint>,
}

#[derive(Debug, Clone)]
struct PerformancePoint {
    timestamp: DateTime<Utc>,
    duration_ms: f64,
    cost: f64,
}

impl TemporalAnalyzer {
    pub fn new() -> Self {
        Self {
            config: TemporalAnalysisConfig::default(),
            history: Vec::new(),
        }
    }

    pub fn with_config(config: &AnalysisConfiguration) -> Self {
        Self {
            config: TemporalAnalysisConfig::default(),
            history: Vec::new(),
        }
    }

    /// Add a performance point to the history
    pub fn add_performance_point(&mut self, timestamp: DateTime<Utc>, duration_ms: f64, cost: f64) {
        self.history.push(PerformancePoint {
            timestamp,
            duration_ms,
            cost,
        });

        // Keep only recent history (e.g., last 1000 points)
        if self.history.len() > 1000 {
            self.history.remove(0);
        }
    }

    fn detect_performance_spike(&self, current_duration: f64) -> Option<Finding> {
        if self.history.len() < self.config.min_data_points {
            return None;
        }

        let avg_duration: f64 = self.history.iter()
            .map(|p| p.duration_ms)
            .sum::<f64>() / self.history.len() as f64;

        if current_duration > avg_duration * self.config.spike_threshold {
            let spike_ratio = current_duration / avg_duration;

            Some(Finding::new(
                FindingType::Custom("PerformanceSpike".to_string()),
                if spike_ratio > 5.0 { Severity::Critical }
                else if spike_ratio > 3.0 { Severity::High }
                else { Severity::Medium },
                "Performance spike detected".to_string(),
                format!(
                    "Current execution time ({:.2}ms) is {:.2}x higher than the average ({:.2}ms) over the last {} executions",
                    current_duration, spike_ratio, avg_duration, self.history.len()
                ),
                "Investigate recent changes, check for resource contention, or verify statistics are up to date".to_string(),
            )
            .with_evidence("current_duration_ms", current_duration)
            .with_evidence("average_duration_ms", avg_duration)
            .with_evidence("spike_ratio", spike_ratio)
            .with_evidence("historical_sample_size", self.history.len() as f64))
        } else {
            None
        }
    }

    fn detect_gradual_degradation(&self) -> Option<Finding> {
        if self.history.len() < self.config.min_data_points * 2 {
            return None;
        }

        // Split history into two halves and compare
        let mid_point = self.history.len() / 2;
        let first_half = &self.history[..mid_point];
        let second_half = &self.history[mid_point..];

        let avg_first: f64 = first_half.iter()
            .map(|p| p.duration_ms)
            .sum::<f64>() / first_half.len() as f64;

        let avg_second: f64 = second_half.iter()
            .map(|p| p.duration_ms)
            .sum::<f64>() / second_half.len() as f64;

        if avg_second > avg_first * self.config.degradation_threshold {
            let degradation_ratio = avg_second / avg_first;

            Some(Finding::new(
                FindingType::Custom("GradualDegradation".to_string()),
                if degradation_ratio > 2.0 { Severity::High } else { Severity::Medium },
                "Gradual performance degradation detected".to_string(),
                format!(
                    "Query performance has degraded by {:.1}% over time. Recent average ({:.2}ms) is {:.2}x slower than earlier average ({:.2}ms)",
                    (degradation_ratio - 1.0) * 100.0, avg_second, degradation_ratio, avg_first
                ),
                "Consider VACUUM ANALYZE, reindexing, or investigating data growth patterns".to_string(),
            )
            .with_evidence("early_average_ms", avg_first)
            .with_evidence("recent_average_ms", avg_second)
            .with_evidence("degradation_ratio", degradation_ratio)
            .with_evidence("sample_size", self.history.len() as f64))
        } else {
            None
        }
    }

    fn calculate_variability(&self) -> Option<Finding> {
        if self.history.len() < self.config.min_data_points {
            return None;
        }

        let avg: f64 = self.history.iter()
            .map(|p| p.duration_ms)
            .sum::<f64>() / self.history.len() as f64;

        let variance: f64 = self.history.iter()
            .map(|p| {
                let diff = p.duration_ms - avg;
                diff * diff
            })
            .sum::<f64>() / self.history.len() as f64;

        let std_dev = variance.sqrt();
        let coefficient_of_variation = if avg > 0.0 { std_dev / avg } else { 0.0 };

        // High variability suggests unstable performance
        if coefficient_of_variation > 1.0 {
            Some(Finding::new(
                FindingType::Custom("HighPerformanceVariability".to_string()),
                if coefficient_of_variation > 2.0 { Severity::High } else { Severity::Medium },
                "High performance variability detected".to_string(),
                format!(
                    "Query shows highly variable performance (coefficient of variation: {:.2}). Standard deviation ({:.2}ms) is {:.1}% of average ({:.2}ms)",
                    coefficient_of_variation, std_dev, coefficient_of_variation * 100.0, avg
                ),
                "Investigate causes of variability: concurrent workload, cache effects, or plan instability".to_string(),
            )
            .with_evidence("average_duration_ms", avg)
            .with_evidence("std_deviation_ms", std_dev)
            .with_evidence("coefficient_of_variation", coefficient_of_variation))
        } else {
            None
        }
    }
}

impl Default for TemporalAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer for TemporalAnalyzer {
    fn analyze(&self, plan: &ParsedPlan, context: &AnalysisContext) -> AnalysisReport {
        let mut report = AnalysisReport::new("TemporalAnalyzer".to_string())
            .with_metadata("version", self.version());

        // If we have query duration, check for spikes
        if let Some(duration) = context.query_duration_ms {
            if let Some(finding) = self.detect_performance_spike(duration) {
                report = report.add_finding(finding);
            }
        }

        // Check for gradual degradation
        if let Some(finding) = self.detect_gradual_degradation() {
            report = report.add_finding(finding);
        }

        // Check for high variability
        if let Some(finding) = self.calculate_variability() {
            report = report.add_finding(finding);
        }

        // Add metrics
        report = report
            .with_metric("historical_data_points", self.history.len() as f64);

        if !self.history.is_empty() {
            let avg: f64 = self.history.iter()
                .map(|p| p.duration_ms)
                .sum::<f64>() / self.history.len() as f64;

            report = report
                .with_metric("average_duration_ms", avg)
                .with_metric("min_duration_ms", self.history.iter().map(|p| p.duration_ms).fold(f64::INFINITY, f64::min))
                .with_metric("max_duration_ms", self.history.iter().map(|p| p.duration_ms).fold(f64::NEG_INFINITY, f64::max));
        }

        report
    }

    fn name(&self) -> &'static str {
        "TemporalAnalyzer"
    }

    fn description(&self) -> &'static str {
        "Analyzes temporal patterns in query performance to detect spikes, degradation, and variability"
    }

    fn version(&self) -> &'static str {
        "1.0.0"
    }
}

impl ConfigurableAnalyzer for TemporalAnalyzer {
    type Config = TemporalAnalysisConfig;

    fn configure(&mut self, config: Self::Config) {
        self.config = config;
    }

    fn default_config() -> Self::Config {
        TemporalAnalysisConfig::default()
    }

    fn current_config(&self) -> &Self::Config {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PlanNode, NodeType, ScanType, PlanCost, TableReference};

    fn create_test_plan() -> ParsedPlan {
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
                max_total_cost: 100.0,
                estimated_rows: 1000,
                estimated_width: 50,
            },
            "Seq Scan on test_table".to_string(),
        );
        ParsedPlan::new(node)
    }

    #[test]
    fn test_insufficient_data() {
        let analyzer = TemporalAnalyzer::new();
        let plan = create_test_plan();
        let context = AnalysisContext::new().with_query_duration(100.0);

        let report = analyzer.analyze(&plan, &context);

        // Should not produce findings with insufficient historical data
        assert_eq!(report.findings.len(), 0);
    }

    #[test]
    fn test_performance_spike_detection() {
        let mut analyzer = TemporalAnalyzer::new();
        let now = Utc::now();

        // Add historical data with consistent performance
        for i in 0..20 {
            analyzer.add_performance_point(
                now - chrono::Duration::minutes(20 - i),
                100.0, // Consistent 100ms
                1000.0,
            );
        }

        let plan = create_test_plan();
        let context = AnalysisContext::new().with_query_duration(400.0); // 4x spike

        let report = analyzer.analyze(&plan, &context);

        // Should detect performance spike
        assert!(!report.findings.is_empty());
        assert!(report.findings.iter().any(|f|
            matches!(f.finding_type, FindingType::Custom(ref s) if s == "PerformanceSpike")
        ));
    }

    #[test]
    fn test_gradual_degradation() {
        let mut analyzer = TemporalAnalyzer::new();
        let now = Utc::now();

        // First half: fast performance
        for i in 0..15 {
            analyzer.add_performance_point(
                now - chrono::Duration::minutes(30 - i),
                50.0,
                500.0,
            );
        }

        // Second half: degraded performance
        for i in 0..15 {
            analyzer.add_performance_point(
                now - chrono::Duration::minutes(15 - i),
                100.0, // 2x slower
                500.0,
            );
        }

        let plan = create_test_plan();
        let context = AnalysisContext::new();

        let report = analyzer.analyze(&plan, &context);

        // Should detect gradual degradation
        assert!(report.findings.iter().any(|f|
            matches!(f.finding_type, FindingType::Custom(ref s) if s == "GradualDegradation")
        ));
    }

    #[test]
    fn test_high_variability() {
        let mut analyzer = TemporalAnalyzer::new();
        let now = Utc::now();

        // Add data with high variability: mostly fast, but with some extreme spikes
        for i in 0..20 {
            // Most queries are fast (50ms), but some have extreme spikes (5000ms)
            let duration = if i % 4 == 0 { 5000.0 } else { 50.0 };
            analyzer.add_performance_point(
                now - chrono::Duration::minutes(20 - i),
                duration,
                1000.0,
            );
        }

        let plan = create_test_plan();
        let context = AnalysisContext::new();

        let report = analyzer.analyze(&plan, &context);

        // Should detect high variability (CoV > 1.0)
        assert!(report.findings.iter().any(|f|
            matches!(f.finding_type, FindingType::Custom(ref s) if s == "HighPerformanceVariability")
        ));
    }
}
