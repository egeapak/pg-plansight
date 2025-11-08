use super::{
    AnalysisContext, AnalysisReport, Analyzer, CombinedAnalysisResult, ConfigurableAnalyzer,
};
use crate::ParsedPlan;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Configuration for the analysis engine
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Maximum time to spend on analysis (per analyzer)
    pub max_analyzer_duration: Duration,
    /// Whether to continue if an analyzer fails
    pub continue_on_error: bool,
    /// Whether to run analyzers in parallel (future enhancement)
    pub parallel_execution: bool,
    /// Custom analyzer-specific configurations
    pub analyzer_configs: HashMap<String, serde_json::Value>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            max_analyzer_duration: Duration::from_secs(30),
            continue_on_error: true,
            parallel_execution: false,
            analyzer_configs: HashMap::new(),
        }
    }
}

/// Result of running a single analyzer
#[derive(Debug, Clone)]
pub struct AnalyzerResult {
    /// Name of the analyzer
    pub analyzer_name: String,
    /// The analysis report (if successful)
    pub report: Option<AnalysisReport>,
    /// Error message (if failed)
    pub error: Option<String>,
    /// Time taken to run the analyzer
    pub duration: Duration,
    /// Whether the analyzer was skipped
    pub skipped: bool,
    /// Reason for skipping (if applicable)
    pub skip_reason: Option<String>,
}

impl AnalyzerResult {
    pub fn success(analyzer_name: String, report: AnalysisReport, duration: Duration) -> Self {
        Self {
            analyzer_name,
            report: Some(report),
            error: None,
            duration,
            skipped: false,
            skip_reason: None,
        }
    }

    pub fn error(analyzer_name: String, error: String, duration: Duration) -> Self {
        Self {
            analyzer_name,
            report: None,
            error: Some(error),
            duration,
            skipped: false,
            skip_reason: None,
        }
    }

    pub fn skipped(analyzer_name: String, reason: String) -> Self {
        Self {
            analyzer_name,
            report: None,
            error: None,
            duration: Duration::ZERO,
            skipped: true,
            skip_reason: Some(reason),
        }
    }

    pub fn is_success(&self) -> bool {
        self.report.is_some()
    }

    pub fn is_error(&self) -> bool {
        self.error.is_some()
    }
}

/// Main analysis engine that coordinates multiple analyzers
#[derive(Default)]
pub struct AnalysisEngine {
    analyzers: Vec<Box<dyn Analyzer>>,
    config: EngineConfig,
}

impl AnalysisEngine {
    /// Create a new analysis engine with default configuration
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a new analysis engine with custom configuration
    pub fn with_config(config: EngineConfig) -> Self {
        Self {
            analyzers: Vec::new(),
            config,
        }
    }

    /// Add an analyzer to the engine
    pub fn add_analyzer<A>(&mut self, analyzer: A)
    where
        A: Analyzer + 'static,
    {
        self.analyzers.push(Box::new(analyzer));
    }

    /// Add a configurable analyzer with custom configuration
    pub fn add_configurable_analyzer<A>(&mut self, mut analyzer: A, config: A::Config)
    where
        A: ConfigurableAnalyzer + 'static,
    {
        analyzer.configure(config);
        self.analyzers.push(Box::new(analyzer));
    }

    /// Get the names of all registered analyzers
    pub fn analyzer_names(&self) -> Vec<&str> {
        self.analyzers.iter().map(|a| a.name()).collect()
    }

    /// Check if an analyzer with the given name is registered
    pub fn has_analyzer(&self, name: &str) -> bool {
        self.analyzers.iter().any(|a| a.name() == name)
    }

    /// Run all analyzers on the given plan
    pub fn analyze(&self, plan: &ParsedPlan, context: &AnalysisContext) -> EngineResult {
        let start_time = Instant::now();
        let mut analyzer_results = Vec::new();

        for analyzer in &self.analyzers {
            let result = self.run_single_analyzer(analyzer.as_ref(), plan, context);
            analyzer_results.push(result);
        }

        let total_duration = start_time.elapsed();
        let successful_reports: Vec<AnalysisReport> = analyzer_results
            .iter()
            .filter_map(|r| r.report.as_ref())
            .cloned()
            .collect();

        let combined_result = CombinedAnalysisResult::new(successful_reports);

        EngineResult {
            combined_result,
            analyzer_results,
            total_duration,
            context: context.clone(),
        }
    }

    /// Run a specific analyzer by name
    pub fn analyze_with(
        &self,
        analyzer_name: &str,
        plan: &ParsedPlan,
        context: &AnalysisContext,
    ) -> Option<AnalyzerResult> {
        self.analyzers
            .iter()
            .find(|a| a.name() == analyzer_name)
            .map(|analyzer| self.run_single_analyzer(analyzer.as_ref(), plan, context))
    }

    /// Run only analyzers that match the given predicate
    pub fn analyze_filtered<F>(
        &self,
        plan: &ParsedPlan,
        context: &AnalysisContext,
        filter: F,
    ) -> EngineResult
    where
        F: Fn(&dyn Analyzer) -> bool,
    {
        let start_time = Instant::now();
        let mut analyzer_results = Vec::new();

        for analyzer in &self.analyzers {
            if filter(analyzer.as_ref()) {
                let result = self.run_single_analyzer(analyzer.as_ref(), plan, context);
                analyzer_results.push(result);
            } else {
                let result = AnalyzerResult::skipped(
                    analyzer.name().to_string(),
                    "Filtered out by predicate".to_string(),
                );
                analyzer_results.push(result);
            }
        }

        let total_duration = start_time.elapsed();
        let successful_reports: Vec<AnalysisReport> = analyzer_results
            .iter()
            .filter_map(|r| r.report.as_ref())
            .cloned()
            .collect();

        let combined_result = CombinedAnalysisResult::new(successful_reports);

        EngineResult {
            combined_result,
            analyzer_results,
            total_duration,
            context: context.clone(),
        }
    }

    /// Run a single analyzer with error handling and timeout
    fn run_single_analyzer(
        &self,
        analyzer: &dyn Analyzer,
        plan: &ParsedPlan,
        context: &AnalysisContext,
    ) -> AnalyzerResult {
        let analyzer_name = analyzer.name().to_string();

        // Check if analyzer supports this plan type
        if !analyzer.supports_plan(plan) {
            return AnalyzerResult::skipped(
                analyzer_name,
                "Plan type not supported by analyzer".to_string(),
            );
        }

        let start_time = Instant::now();

        // Run the analyzer with timeout and error handling
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            analyzer.analyze(plan, context)
        }));

        let duration = start_time.elapsed();

        // Check timeout
        if duration > self.config.max_analyzer_duration {
            return AnalyzerResult::error(
                analyzer_name,
                format!("Analyzer timed out after {:?}", duration),
                duration,
            );
        }

        match result {
            Ok(report) => AnalyzerResult::success(analyzer_name, report, duration),
            Err(_) => AnalyzerResult::error(
                analyzer_name,
                "Analyzer panicked during execution".to_string(),
                duration,
            ),
        }
    }

    /// Update the configuration of the engine
    pub fn configure(&mut self, config: EngineConfig) {
        self.config = config;
    }

    /// Get the current configuration
    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    /// Clear all analyzers
    pub fn clear_analyzers(&mut self) {
        self.analyzers.clear();
    }

    /// Remove an analyzer by name
    pub fn remove_analyzer(&mut self, name: &str) -> bool {
        let initial_len = self.analyzers.len();
        self.analyzers.retain(|a| a.name() != name);
        self.analyzers.len() < initial_len
    }

    /// Get statistics about the registered analyzers
    pub fn analyzer_stats(&self) -> AnalyzerStats {
        let mut stats = AnalyzerStats::default();

        for analyzer in &self.analyzers {
            stats.total_count += 1;
            stats
                .analyzer_versions
                .insert(analyzer.name().to_string(), analyzer.version().to_string());
        }

        stats
    }
}

/// Statistics about registered analyzers
#[derive(Debug, Default)]
pub struct AnalyzerStats {
    pub total_count: usize,
    pub analyzer_versions: HashMap<String, String>,
}

/// Complete result from the analysis engine
#[derive(Debug, Clone)]
pub struct EngineResult {
    /// Combined analysis result from successful analyzers
    pub combined_result: CombinedAnalysisResult,
    /// Individual results from all analyzers (including failures)
    pub analyzer_results: Vec<AnalyzerResult>,
    /// Total time taken for the entire analysis
    pub total_duration: Duration,
    /// The analysis context that was used
    pub context: AnalysisContext,
}

impl EngineResult {
    /// Get successful analyzer results
    pub fn successful_results(&self) -> Vec<&AnalyzerResult> {
        self.analyzer_results
            .iter()
            .filter(|r| r.is_success())
            .collect()
    }

    /// Get failed analyzer results
    pub fn failed_results(&self) -> Vec<&AnalyzerResult> {
        self.analyzer_results
            .iter()
            .filter(|r| r.is_error())
            .collect()
    }

    /// Get skipped analyzer results
    pub fn skipped_results(&self) -> Vec<&AnalyzerResult> {
        self.analyzer_results.iter().filter(|r| r.skipped).collect()
    }

    /// Get a summary of the analysis execution
    pub fn execution_summary(&self) -> ExecutionSummary {
        let successful_count = self.successful_results().len();
        let failed_count = self.failed_results().len();
        let skipped_count = self.skipped_results().len();

        let total_analyzer_time: Duration = self.analyzer_results.iter().map(|r| r.duration).sum();

        ExecutionSummary {
            total_analyzers: self.analyzer_results.len(),
            successful_count,
            failed_count,
            skipped_count,
            total_duration: self.total_duration,
            total_analyzer_time,
            efficiency_ratio: if self.total_duration.as_millis() > 0 {
                total_analyzer_time.as_millis() as f64 / self.total_duration.as_millis() as f64
            } else {
                0.0
            },
        }
    }

    /// Check if any critical issues were found
    pub fn has_critical_issues(&self) -> bool {
        use super::Severity;
        self.combined_result
            .summary
            .severity_counts
            .get(&Severity::Critical)
            .unwrap_or(&0)
            > &0
    }

    /// Get the worst case performance assessment
    pub fn performance_assessment(&self) -> &super::PerformanceAssessment {
        &self.combined_result.summary.performance_assessment
    }
}

/// Summary of analysis execution performance
#[derive(Debug, Clone)]
pub struct ExecutionSummary {
    pub total_analyzers: usize,
    pub successful_count: usize,
    pub failed_count: usize,
    pub skipped_count: usize,
    pub total_duration: Duration,
    pub total_analyzer_time: Duration,
    pub efficiency_ratio: f64, // analyzer_time / total_time (measures parallelization efficiency)
}

/// Builder for constructing analysis engines with common analyzer sets
pub struct AnalysisEngineBuilder {
    engine: AnalysisEngine,
}

impl AnalysisEngineBuilder {
    pub fn new() -> Self {
        Self {
            engine: AnalysisEngine::new(),
        }
    }

    pub fn with_config(config: EngineConfig) -> Self {
        Self {
            engine: AnalysisEngine::with_config(config),
        }
    }

    pub fn add_analyzer<A>(mut self, analyzer: A) -> Self
    where
        A: Analyzer + 'static,
    {
        self.engine.add_analyzer(analyzer);
        self
    }

    pub fn add_configurable_analyzer<A>(mut self, analyzer: A, config: A::Config) -> Self
    where
        A: ConfigurableAnalyzer + 'static,
    {
        self.engine.add_configurable_analyzer(analyzer, config);
        self
    }

    /// Add all default analyzers (to be implemented when analyzers are created)
    pub fn with_default_analyzers(self) -> Self {
        // This will be implemented when we create the actual analyzer implementations
        self
    }

    /// Add performance-focused analyzers only
    pub fn with_performance_analyzers(self) -> Self {
        // This will be implemented when we create the actual analyzer implementations
        self
    }

    /// Add index-focused analyzers only
    pub fn with_index_analyzers(self) -> Self {
        // This will be implemented when we create the actual analyzer implementations
        self
    }

    pub fn build(self) -> AnalysisEngine {
        self.engine
    }
}

impl Default for AnalysisEngineBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::super::{AnalysisReport, Finding, FindingType, Severity};
    use super::*;
    use crate::{NodeType, ParsedPlan, PlanCost, PlanNode, ScanType, TableReference};

    // Mock analyzer for testing
    struct MockAnalyzer {
        name: &'static str,
        should_fail: bool,
        should_panic: bool,
        delay_ms: u64,
    }

    impl MockAnalyzer {
        fn new(name: &'static str) -> Self {
            Self {
                name,
                should_fail: false,
                should_panic: false,
                delay_ms: 0,
            }
        }

        fn with_failure(mut self) -> Self {
            self.should_fail = true;
            self
        }

        fn with_panic(mut self) -> Self {
            self.should_panic = true;
            self
        }

        fn with_delay(mut self, delay_ms: u64) -> Self {
            self.delay_ms = delay_ms;
            self
        }
    }

    impl Analyzer for MockAnalyzer {
        fn analyze(&self, _plan: &ParsedPlan, _context: &AnalysisContext) -> AnalysisReport {
            if self.delay_ms > 0 {
                std::thread::sleep(Duration::from_millis(self.delay_ms));
            }

            if self.should_panic {
                panic!("Mock analyzer panic");
            }

            if self.should_fail {
                // We can't return an error from this trait, so we'll create an empty report
                // In a real scenario, analyzers would handle their own errors
                return AnalysisReport::new(self.name.to_string());
            }

            AnalysisReport::new(self.name.to_string()).add_finding(Finding::new(
                FindingType::ExcessiveRowProcessing,
                Severity::Medium,
                "Mock finding".to_string(),
                "This is a test finding".to_string(),
                "Do something".to_string(),
            ))
        }

        fn name(&self) -> &'static str {
            self.name
        }

        fn description(&self) -> &'static str {
            "Mock analyzer for testing"
        }
    }

    fn create_test_plan() -> ParsedPlan {
        let cost = PlanCost {
            startup_cost: 0.0,
            min_total_cost: 0.0,
            max_total_cost: 100.0,
            estimated_rows: 1000,
            estimated_width: 50,
        };

        let root = PlanNode::new(
            NodeType::Scan(ScanType::SeqScan {
                table: TableReference {
                    schema: None,
                    name: "test_table".to_string(),
                    alias: None,
                },
            }),
            cost,
            "Test plan".to_string(),
        );

        ParsedPlan::new(root)
    }

    #[test]
    fn test_engine_creation() {
        let mut engine = AnalysisEngine::new();
        engine.add_analyzer(MockAnalyzer::new("test1"));
        engine.add_analyzer(MockAnalyzer::new("test2"));

        assert_eq!(engine.analyzer_names(), vec!["test1", "test2"]);
        assert!(engine.has_analyzer("test1"));
        assert!(!engine.has_analyzer("nonexistent"));
    }

    #[test]
    fn test_successful_analysis() {
        let mut engine = AnalysisEngine::new();
        engine.add_analyzer(MockAnalyzer::new("test1"));
        engine.add_analyzer(MockAnalyzer::new("test2"));

        let plan = create_test_plan();
        let context = AnalysisContext::new();
        let result = engine.analyze(&plan, &context);

        assert_eq!(result.successful_results().len(), 2);
        assert_eq!(result.failed_results().len(), 0);
        assert_eq!(result.combined_result.reports.len(), 2);
    }

    #[test]
    fn test_failed_analysis() {
        let mut engine = AnalysisEngine::new();
        engine.add_analyzer(MockAnalyzer::new("good"));
        engine.add_analyzer(MockAnalyzer::new("bad").with_failure());

        let plan = create_test_plan();
        let context = AnalysisContext::new();
        let result = engine.analyze(&plan, &context);

        // Both should be "successful" since we can't fail from the trait
        // But the failed one will have no findings
        assert_eq!(result.successful_results().len(), 2);
        assert_eq!(result.combined_result.reports[0].findings.len(), 1);
        assert_eq!(result.combined_result.reports[1].findings.len(), 0);
    }

    #[test]
    fn test_analyzer_removal() {
        let mut engine = AnalysisEngine::new();
        engine.add_analyzer(MockAnalyzer::new("test1"));
        engine.add_analyzer(MockAnalyzer::new("test2"));

        assert_eq!(engine.analyzer_names().len(), 2);

        let removed = engine.remove_analyzer("test1");
        assert!(removed);
        assert_eq!(engine.analyzer_names(), vec!["test2"]);

        let not_removed = engine.remove_analyzer("nonexistent");
        assert!(!not_removed);
    }

    #[test]
    fn test_specific_analyzer_execution() {
        let mut engine = AnalysisEngine::new();
        engine.add_analyzer(MockAnalyzer::new("target"));
        engine.add_analyzer(MockAnalyzer::new("other"));

        let plan = create_test_plan();
        let context = AnalysisContext::new();

        let result = engine.analyze_with("target", &plan, &context);
        assert!(result.is_some());
        assert_eq!(result.unwrap().analyzer_name, "target");

        let no_result = engine.analyze_with("nonexistent", &plan, &context);
        assert!(no_result.is_none());
    }

    #[test]
    fn test_filtered_analysis() {
        let mut engine = AnalysisEngine::new();
        engine.add_analyzer(MockAnalyzer::new("include_me"));
        engine.add_analyzer(MockAnalyzer::new("exclude_me"));

        let plan = create_test_plan();
        let context = AnalysisContext::new();

        let result = engine.analyze_filtered(&plan, &context, |analyzer| {
            analyzer.name().contains("include")
        });

        assert_eq!(result.successful_results().len(), 1);
        assert_eq!(result.skipped_results().len(), 1);
        assert_eq!(result.successful_results()[0].analyzer_name, "include_me");
    }

    #[test]
    fn test_engine_builder() {
        let engine = AnalysisEngineBuilder::new()
            .add_analyzer(MockAnalyzer::new("test1"))
            .add_analyzer(MockAnalyzer::new("test2"))
            .build();

        assert_eq!(engine.analyzer_names().len(), 2);
    }

    #[test]
    fn test_analyzer_stats() {
        let mut engine = AnalysisEngine::new();
        engine.add_analyzer(MockAnalyzer::new("test1"));
        engine.add_analyzer(MockAnalyzer::new("test2"));

        let stats = engine.analyzer_stats();
        assert_eq!(stats.total_count, 2);
        assert!(stats.analyzer_versions.contains_key("test1"));
        assert!(stats.analyzer_versions.contains_key("test2"));
    }
}
