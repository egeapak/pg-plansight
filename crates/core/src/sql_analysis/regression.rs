//! Performance regression detection and analysis
//!
//! This module provides sophisticated detection of performance regressions
//! by analyzing query execution patterns over time and identifying anomalies.

use crate::analysis::consolidated_config::{RegressionDetectionConfig, RegressionThresholds};
use crate::sql_analysis::statistics::StatisticalCalculator;
use anyhow::Result;
use chrono::{DateTime, Timelike, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Performance regression analysis result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegressionAnalysis {
    /// Overall regression status
    pub status: RegressionStatus,
    /// Individual metric regressions
    pub metric_regressions: Vec<MetricRegression>,
    /// Time-based analysis
    pub temporal_analysis: TemporalAnalysis,
    /// Statistical analysis
    pub statistical_analysis: StatisticalAnalysis,
    /// Recommended actions
    pub recommendations: Vec<RegressionRecommendation>,
    /// Confidence level in the analysis
    pub confidence_level: ConfidenceLevel,
}

/// Overall regression status
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RegressionStatus {
    /// No regression detected
    None,
    /// Minor regression detected
    Minor,
    /// Significant regression detected
    Significant,
    /// Critical regression detected
    Critical,
    /// Insufficient data for analysis
    InsufficientData,
}

/// Individual metric regression
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricRegression {
    /// Metric being analyzed
    pub metric: PerformanceMetric,
    /// Regression severity
    pub severity: RegressionSeverity,
    /// Current value
    pub current_value: f64,
    /// Baseline value for comparison
    pub baseline_value: f64,
    /// Percentage change
    pub percentage_change: f64,
    /// Statistical significance
    pub statistical_significance: f64,
    /// Time when regression started
    pub regression_start: Option<DateTime<Utc>>,
}

/// Performance metrics that can regress
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PerformanceMetric {
    /// Average execution time
    AvgExecutionTime,
    /// Maximum execution time
    MaxExecutionTime,
    /// 95th percentile execution time
    P95ExecutionTime,
    /// 99th percentile execution time
    P99ExecutionTime,
    /// Execution frequency
    ExecutionFrequency,
    /// Memory usage
    MemoryUsage,
    /// CPU usage
    CpuUsage,
    /// IO operations
    IoOperations,
    /// Cache hit ratio
    CacheHitRatio,
}

/// Severity of regression
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RegressionSeverity {
    /// Less than 10% degradation
    Low,
    /// 10-25% degradation
    Medium,
    /// 25-50% degradation
    High,
    /// More than 50% degradation
    Critical,
}

/// Time-based analysis of performance
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemporalAnalysis {
    /// Analysis period covered
    pub analysis_period: TimePeriod,
    /// Trend direction
    pub trend: TrendDirection,
    /// Trend strength (0.0 to 1.0)
    pub trend_strength: f64,
    /// Seasonality detected
    pub seasonality: Option<SeasonalityPattern>,
    /// Change points detected
    pub change_points: Vec<ChangePoint>,
}

/// Time period for analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimePeriod {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub duration_hours: i64,
}

/// Direction of performance trend
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TrendDirection {
    Improving,
    Stable,
    Degrading,
    Volatile,
}

/// Seasonality pattern in performance
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeasonalityPattern {
    /// Type of seasonality
    pub pattern_type: SeasonalityType,
    /// Strength of seasonal effect
    pub strength: f64,
    /// Peak performance periods
    pub peak_periods: Vec<TimePeriod>,
    /// Low performance periods
    pub low_periods: Vec<TimePeriod>,
}

/// Type of seasonal pattern
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SeasonalityType {
    Hourly,
    Daily,
    Weekly,
    Monthly,
}

/// Change point in performance timeline
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangePoint {
    /// When the change occurred
    pub timestamp: DateTime<Utc>,
    /// Type of change
    pub change_type: ChangeType,
    /// Magnitude of change
    pub magnitude: f64,
    /// Confidence in detection
    pub confidence: f64,
    /// Possible causes
    pub possible_causes: Vec<String>,
}

/// Type of performance change
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ChangeType {
    /// Sudden improvement
    Improvement,
    /// Sudden degradation  
    Degradation,
    /// Change in variance
    VarianceChange,
    /// Trend change
    TrendChange,
}

/// Statistical analysis of performance data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatisticalAnalysis {
    /// Statistical tests performed
    pub tests_performed: Vec<StatisticalTest>,
    /// Distribution analysis
    pub distribution: DistributionAnalysis,
    /// Anomaly detection results
    pub anomalies: Vec<AnomalyDetection>,
    /// Correlation analysis
    pub correlations: Vec<CorrelationAnalysis>,
}

/// Statistical test result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatisticalTest {
    /// Test name
    pub test_name: String,
    /// Test statistic value
    pub test_statistic: f64,
    /// P-value
    pub p_value: f64,
    /// Is result significant?
    pub is_significant: bool,
    /// Interpretation
    pub interpretation: String,
}

/// Distribution analysis results
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistributionAnalysis {
    /// Distribution type
    pub distribution_type: DistributionType,
    /// Mean value
    pub mean: f64,
    /// Standard deviation
    pub std_dev: f64,
    /// Skewness
    pub skewness: f64,
    /// Kurtosis
    pub kurtosis: f64,
    /// Outlier percentage
    pub outlier_percentage: f64,
}

/// Type of statistical distribution
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DistributionType {
    Normal,
    LogNormal,
    Exponential,
    Uniform,
    Bimodal,
    Unknown,
}

/// Anomaly detection result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnomalyDetection {
    /// Timestamp of anomaly
    pub timestamp: DateTime<Utc>,
    /// Anomaly score (higher = more anomalous)
    pub score: f64,
    /// Expected value
    pub expected_value: f64,
    /// Actual value
    pub actual_value: f64,
    /// Anomaly type
    pub anomaly_type: AnomalyType,
}

/// Type of anomaly
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AnomalyType {
    HighValue,
    LowValue,
    Spike,
    Drop,
    ContextualAnomaly,
}

/// Correlation analysis between metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrelationAnalysis {
    /// First metric
    pub metric1: PerformanceMetric,
    /// Second metric
    pub metric2: PerformanceMetric,
    /// Correlation coefficient
    pub correlation: f64,
    /// Correlation strength
    pub strength: CorrelationStrength,
    /// Lag in correlation (if any)
    pub lag_minutes: Option<i64>,
}

/// Strength of correlation
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CorrelationStrength {
    VeryWeak,   // |r| < 0.3
    Weak,       // 0.3 <= |r| < 0.5
    Moderate,   // 0.5 <= |r| < 0.7
    Strong,     // 0.7 <= |r| < 0.9
    VeryStrong, // |r| >= 0.9
}

/// Regression recommendation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegressionRecommendation {
    /// Type of recommendation
    pub recommendation_type: RecommendationType,
    /// Priority level
    pub priority: Priority,
    /// Description
    pub description: String,
    /// Expected impact
    pub expected_impact: ImpactLevel,
    /// Implementation effort
    pub effort_level: EffortLevel,
    /// Specific actions to take
    pub actions: Vec<String>,
}

/// Type of regression recommendation
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RecommendationType {
    /// Immediate investigation needed
    Investigation,
    /// Query optimization required
    Optimization,
    /// Infrastructure scaling needed
    Scaling,
    /// Monitoring enhancement
    Monitoring,
    /// Configuration tuning
    Configuration,
    /// Data maintenance
    Maintenance,
}

/// Priority level
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Priority {
    Low,
    Medium,
    High,
    Critical,
}

/// Impact level
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ImpactLevel {
    Low,
    Medium,
    High,
}

/// Effort level for implementation
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EffortLevel {
    Low,
    Medium,
    High,
}

/// Confidence level in analysis
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ConfidenceLevel {
    Low,      // < 70%
    Medium,   // 70-85%
    High,     // 85-95%
    VeryHigh, // > 95%
}

/// Performance data point for analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceDataPoint {
    pub timestamp: DateTime<Utc>,
    pub execution_time_ms: f64,
    pub memory_usage_mb: Option<f64>,
    pub cpu_usage_percent: Option<f64>,
    pub io_operations: Option<u64>,
    pub cache_hit_ratio: Option<f64>,
}

/// Performance regression detector
pub struct RegressionDetector {
    config: RegressionDetectionConfig,
    /// Statistical calculator with proper implementations
    stats_calc: StatisticalCalculator,
}

impl Default for RegressionDetector {
    fn default() -> Self {
        use crate::analysis::consolidated_config::WorkloadContext;
        let workload = WorkloadContext::default();
        let config = RegressionDetectionConfig::for_workload(&workload);
        Self {
            stats_calc: StatisticalCalculator::new()
                .with_significance_level(config.significance_level),
            config,
        }
    }
}

impl RegressionDetector {
    /// Create detector with specific configuration
    pub fn with_config(config: &RegressionDetectionConfig) -> Self {
        Self {
            stats_calc: StatisticalCalculator::new()
                .with_significance_level(config.significance_level),
            config: config.clone(),
        }
    }
    pub fn new() -> Self {
        Self::default()
    }

    /// Configure detector with custom thresholds
    pub fn with_thresholds(mut self, thresholds: RegressionThresholds) -> Self {
        self.config.regression_thresholds = thresholds;
        self
    }

    /// Configure detector with custom significance level
    pub fn with_significance_level(mut self, level: f64) -> Self {
        self.config.significance_level = level;
        self.stats_calc = self.stats_calc.with_significance_level(level);
        self
    }

    /// Analyze performance data for regressions
    pub fn analyze(&self, data: &[PerformanceDataPoint]) -> Result<RegressionAnalysis> {
        if data.len() < self.config.min_data_points {
            return Ok(self.create_insufficient_data_analysis());
        }

        let temporal_analysis = self.analyze_temporal_patterns(data)?;
        let statistical_analysis = self.perform_statistical_analysis(data)?;
        let metric_regressions = self.detect_metric_regressions(data)?;
        let status = self.determine_overall_status(&metric_regressions);
        let recommendations = self.generate_recommendations(
            &metric_regressions,
            &temporal_analysis,
            &statistical_analysis,
        );
        let confidence_level = self.calculate_confidence_level(&statistical_analysis, data.len());

        Ok(RegressionAnalysis {
            status,
            metric_regressions,
            temporal_analysis,
            statistical_analysis,
            recommendations,
            confidence_level,
        })
    }

    /// Analyze temporal patterns in the data
    fn analyze_temporal_patterns(&self, data: &[PerformanceDataPoint]) -> Result<TemporalAnalysis> {
        let start = data.first().unwrap().timestamp;
        let end = data.last().unwrap().timestamp;
        let duration = end.signed_duration_since(start);

        let analysis_period = TimePeriod {
            start,
            end,
            duration_hours: duration.num_hours(),
        };

        let trend = self.calculate_trend(data);
        let trend_strength = self.calculate_trend_strength(data);
        let seasonality = self.detect_seasonality(data);
        let change_points = self.detect_change_points(data)?;

        Ok(TemporalAnalysis {
            analysis_period,
            trend,
            trend_strength,
            seasonality,
            change_points,
        })
    }

    /// Calculate overall trend direction
    fn calculate_trend(&self, data: &[PerformanceDataPoint]) -> TrendDirection {
        let mid_point = data.len() / 2;
        let first_half_avg = self.calculate_average_execution_time(&data[..mid_point]);
        let second_half_avg = self.calculate_average_execution_time(&data[mid_point..]);

        let change_ratio = (second_half_avg - first_half_avg) / first_half_avg;
        let variance = self.calculate_variance(data);
        let cv = (variance.sqrt() / self.calculate_average_execution_time(data)).abs();

        // High coefficient of variation indicates volatility
        if cv > 0.5 {
            return TrendDirection::Volatile;
        }

        match change_ratio {
            r if r > 0.05 => TrendDirection::Degrading,
            r if r < -0.05 => TrendDirection::Improving,
            _ => TrendDirection::Stable,
        }
    }

    /// Calculate trend strength (0.0 to 1.0)
    fn calculate_trend_strength(&self, data: &[PerformanceDataPoint]) -> f64 {
        // Use linear regression to calculate R-squared
        let n = data.len() as f64;
        let x_mean = (n - 1.0) / 2.0; // Time index mean
        let y_mean = self.calculate_average_execution_time(data);

        let mut ss_tot = 0.0;
        let mut ss_res = 0.0;
        let mut slope_num = 0.0;
        let mut slope_den = 0.0;

        for (i, point) in data.iter().enumerate() {
            let x = i as f64;
            let y = point.execution_time_ms;

            ss_tot += (y - y_mean).powi(2);
            slope_num += (x - x_mean) * (y - y_mean);
            slope_den += (x - x_mean).powi(2);
        }

        if slope_den == 0.0 {
            return 0.0;
        }

        let slope = slope_num / slope_den;
        let intercept = y_mean - slope * x_mean;

        for (i, point) in data.iter().enumerate() {
            let x = i as f64;
            let y = point.execution_time_ms;
            let predicted = slope * x + intercept;
            ss_res += (y - predicted).powi(2);
        }

        (if ss_tot == 0.0 {
            0.0
        } else {
            1.0 - (ss_res / ss_tot)
        })
        .clamp(0.0, 1.0)
    }

    /// Detect seasonal patterns
    fn detect_seasonality(&self, data: &[PerformanceDataPoint]) -> Option<SeasonalityPattern> {
        // Simplified seasonality detection
        // In a real implementation, you'd use FFT or autocorrelation
        if data.len() < 168 {
            // Need at least a week of hourly data
            return None;
        }

        // Group by hour of day
        let mut hourly_data: BTreeMap<u32, Vec<f64>> = BTreeMap::new();
        for point in data {
            let hour = point.timestamp.hour();
            hourly_data
                .entry(hour)
                .or_default()
                .push(point.execution_time_ms);
        }

        let hourly_averages: Vec<f64> = (0..24)
            .map(|hour| {
                hourly_data
                    .get(&hour)
                    .map(|values| values.iter().sum::<f64>() / values.len() as f64)
                    .unwrap_or(0.0)
            })
            .collect();

        let overall_avg = hourly_averages.iter().sum::<f64>() / 24.0;
        let variance = hourly_averages
            .iter()
            .map(|avg| (avg - overall_avg).powi(2))
            .sum::<f64>()
            / 24.0;

        let strength = (variance.sqrt() / overall_avg).min(1.0);

        if strength > 0.2 {
            // Significant daily pattern
            Some(SeasonalityPattern {
                pattern_type: SeasonalityType::Daily,
                strength,
                peak_periods: Vec::new(), // Would be calculated in full implementation
                low_periods: Vec::new(),
            })
        } else {
            None
        }
    }

    /// Detect change points in performance
    fn detect_change_points(&self, data: &[PerformanceDataPoint]) -> Result<Vec<ChangePoint>> {
        let mut change_points = Vec::new();
        let window_size = (data.len() / 10).clamp(5, 50); // Adaptive window size

        for i in window_size..(data.len() - window_size) {
            let before = &data[(i - window_size)..i];
            let after = &data[i..(i + window_size)];

            let before_avg = self.calculate_average_execution_time(before);
            let after_avg = self.calculate_average_execution_time(after);

            let change_magnitude = (after_avg - before_avg) / before_avg;

            if change_magnitude.abs() > 0.2 {
                // 20% change threshold
                let change_type = if change_magnitude > 0.0 {
                    ChangeType::Degradation
                } else {
                    ChangeType::Improvement
                };

                change_points.push(ChangePoint {
                    timestamp: data[i].timestamp,
                    change_type,
                    magnitude: change_magnitude.abs(),
                    confidence: 0.8, // Simplified confidence calculation
                    possible_causes: self.infer_possible_causes(change_magnitude),
                });
            }
        }

        Ok(change_points)
    }

    /// Infer possible causes of performance changes
    fn infer_possible_causes(&self, change_magnitude: f64) -> Vec<String> {
        let mut causes = Vec::new();

        if change_magnitude > 0.5 {
            causes.push("Major system change or deployment".to_string());
            causes.push("Infrastructure issue or resource contention".to_string());
        } else if change_magnitude > 0.2 {
            causes.push("Query plan change".to_string());
            causes.push("Index modification or corruption".to_string());
            causes.push("Data growth or distribution change".to_string());
        } else {
            causes.push("Minor configuration change".to_string());
            causes.push("Normal system variation".to_string());
        }

        causes
    }

    /// Perform statistical analysis
    fn perform_statistical_analysis(
        &self,
        data: &[PerformanceDataPoint],
    ) -> Result<StatisticalAnalysis> {
        let distribution = self.analyze_distribution(data);
        let anomalies = self.detect_anomalies(data);
        let tests_performed = self.perform_statistical_tests(data);
        let correlations = self.analyze_correlations(data);

        Ok(StatisticalAnalysis {
            tests_performed,
            distribution,
            anomalies,
            correlations,
        })
    }

    /// Analyze data distribution using corrected statistical calculations
    fn analyze_distribution(&self, data: &[PerformanceDataPoint]) -> DistributionAnalysis {
        let values: Vec<f64> = data.iter().map(|p| p.execution_time_ms).collect();

        if values.is_empty() {
            return DistributionAnalysis {
                distribution_type: DistributionType::Unknown,
                mean: 0.0,
                std_dev: 0.0,
                skewness: 0.0,
                kurtosis: 0.0,
                outlier_percentage: 0.0,
            };
        }

        let mean = values.iter().sum::<f64>() / values.len() as f64;
        // Use proper sample standard deviation (N-1)
        let std_dev = self.stats_calc.sample_std_dev(&values);

        // Calculate proper skewness and kurtosis with bias correction
        let skewness = self.stats_calc.skewness(&values);
        let kurtosis = self.stats_calc.excess_kurtosis(&values);

        // Detect outliers using proper IQR method with interpolation
        let outlier_indices = self
            .stats_calc
            .detect_iqr_outliers(&values)
            .unwrap_or_else(|_| Vec::new());
        let outlier_percentage = outlier_indices.len() as f64 / values.len() as f64 * 100.0;

        // Classify distribution type
        let distribution_type = self.classify_distribution(skewness, kurtosis);

        DistributionAnalysis {
            distribution_type,
            mean,
            std_dev,
            skewness,
            kurtosis,
            outlier_percentage,
        }
    }

    /// Classify distribution type
    fn classify_distribution(&self, skewness: f64, kurtosis: f64) -> DistributionType {
        if skewness.abs() < 0.5 && kurtosis.abs() < 0.5 {
            DistributionType::Normal
        } else if skewness > 1.0 {
            DistributionType::LogNormal
        } else if kurtosis < -1.0 {
            DistributionType::Uniform
        } else {
            DistributionType::Unknown
        }
    }

    /// Detect anomalies using modified Z-score (more robust than simple Z-score)
    fn detect_anomalies(&self, data: &[PerformanceDataPoint]) -> Vec<AnomalyDetection> {
        let values: Vec<f64> = data.iter().map(|p| p.execution_time_ms).collect();

        // Use modified Z-score for more robust anomaly detection
        let anomaly_info = self
            .stats_calc
            .detect_anomalies_modified_zscore(&values, 3.5);

        anomaly_info
            .into_iter()
            .map(|info| {
                let point = &data[info.index];
                let anomaly_type = if info.modified_zscore > 4.0 {
                    if info.value > self.stats_calc.quantile(&values, 0.5).unwrap_or(0.0) {
                        AnomalyType::Spike
                    } else {
                        AnomalyType::Drop
                    }
                } else if info.value > self.stats_calc.quantile(&values, 0.5).unwrap_or(0.0) {
                    AnomalyType::HighValue
                } else {
                    AnomalyType::LowValue
                };

                AnomalyDetection {
                    timestamp: point.timestamp,
                    score: info.modified_zscore.abs(),
                    expected_value: self.stats_calc.quantile(&values, 0.5).unwrap_or(0.0), // Use median as expected
                    actual_value: info.value,
                    anomaly_type,
                }
            })
            .collect()
    }

    /// Perform statistical tests using proper implementations
    fn perform_statistical_tests(&self, data: &[PerformanceDataPoint]) -> Vec<StatisticalTest> {
        let mut tests = Vec::new();
        let values: Vec<f64> = data.iter().map(|p| p.execution_time_ms).collect();

        // Proper normality test using Jarque-Bera
        if let Ok(normality_result) = self.stats_calc.normality_test(&values) {
            tests.push(StatisticalTest {
                test_name: normality_result.test_name,
                test_statistic: normality_result.test_statistic,
                p_value: normality_result.p_value,
                is_significant: !normality_result.is_normal,
                interpretation: if normality_result.is_normal {
                    "Data appears to follow a normal distribution".to_string()
                } else {
                    format!("Data significantly deviates from normal distribution (skewness: {:.3}, kurtosis: {:.3})", 
                           normality_result.skewness, normality_result.kurtosis)
                },
            });
        }

        tests
    }

    /// Analyze correlations between metrics
    fn analyze_correlations(&self, data: &[PerformanceDataPoint]) -> Vec<CorrelationAnalysis> {
        let mut correlations = Vec::new();

        // Correlation between execution time and memory usage
        if data.iter().any(|p| p.memory_usage_mb.is_some()) {
            let exec_times: Vec<f64> = data.iter().map(|p| p.execution_time_ms).collect();
            let memory_usage: Vec<f64> = data.iter().filter_map(|p| p.memory_usage_mb).collect();

            if memory_usage.len() == exec_times.len()
                && memory_usage.len() >= 3
                && let Ok(correlation) = self.stats_calc.correlation(&exec_times, &memory_usage)
            {
                let strength = self.classify_correlation_strength(correlation);

                correlations.push(CorrelationAnalysis {
                    metric1: PerformanceMetric::AvgExecutionTime,
                    metric2: PerformanceMetric::MemoryUsage,
                    correlation,
                    strength,
                    lag_minutes: None,
                });
            }
        }

        // Correlation between execution time and CPU usage
        if data.iter().any(|p| p.cpu_usage_percent.is_some()) {
            let exec_times: Vec<f64> = data.iter().map(|p| p.execution_time_ms).collect();
            let cpu_usage: Vec<f64> = data.iter().filter_map(|p| p.cpu_usage_percent).collect();

            if cpu_usage.len() == exec_times.len()
                && cpu_usage.len() >= 3
                && let Ok(correlation) = self.stats_calc.correlation(&exec_times, &cpu_usage)
            {
                let strength = self.classify_correlation_strength(correlation);

                correlations.push(CorrelationAnalysis {
                    metric1: PerformanceMetric::AvgExecutionTime,
                    metric2: PerformanceMetric::CpuUsage,
                    correlation,
                    strength,
                    lag_minutes: None,
                });
            }
        }

        correlations
    }

    /// Classify correlation strength
    fn classify_correlation_strength(&self, correlation: f64) -> CorrelationStrength {
        let abs_corr = correlation.abs();
        match abs_corr {
            r if r >= 0.9 => CorrelationStrength::VeryStrong,
            r if r >= 0.7 => CorrelationStrength::Strong,
            r if r >= 0.5 => CorrelationStrength::Moderate,
            r if r >= 0.3 => CorrelationStrength::Weak,
            _ => CorrelationStrength::VeryWeak,
        }
    }

    /// Detect regressions in individual metrics
    fn detect_metric_regressions(
        &self,
        data: &[PerformanceDataPoint],
    ) -> Result<Vec<MetricRegression>> {
        let mut regressions = Vec::new();

        // Split data into baseline and current periods
        let split_point = data.len() * 3 / 4; // Use last 25% as current period
        let baseline_data = &data[..split_point];
        let current_data = &data[split_point..];

        // Analyze execution time regression
        let baseline_avg = self.calculate_average_execution_time(baseline_data);
        let current_avg = self.calculate_average_execution_time(current_data);
        let percentage_change = (current_avg - baseline_avg) / baseline_avg;

        if percentage_change.abs() > self.config.regression_thresholds.minor_threshold {
            let severity = self.classify_regression_severity(percentage_change.abs());
            let significance = self.calculate_statistical_significance(baseline_data, current_data);

            regressions.push(MetricRegression {
                metric: PerformanceMetric::AvgExecutionTime,
                severity,
                current_value: current_avg,
                baseline_value: baseline_avg,
                percentage_change: percentage_change * 100.0,
                statistical_significance: significance,
                regression_start: Some(current_data.first().unwrap().timestamp),
            });
        }

        Ok(regressions)
    }

    /// Classify regression severity
    fn classify_regression_severity(&self, change_percentage: f64) -> RegressionSeverity {
        if change_percentage >= self.config.regression_thresholds.critical_threshold {
            RegressionSeverity::Critical
        } else if change_percentage >= self.config.regression_thresholds.significant_threshold {
            RegressionSeverity::High
        } else if change_percentage >= self.config.regression_thresholds.minor_threshold {
            RegressionSeverity::Medium
        } else {
            RegressionSeverity::Low
        }
    }

    /// Calculate statistical significance using proper Welch's t-test
    fn calculate_statistical_significance(
        &self,
        baseline: &[PerformanceDataPoint],
        current: &[PerformanceDataPoint],
    ) -> f64 {
        let baseline_values: Vec<f64> = baseline.iter().map(|p| p.execution_time_ms).collect();
        let current_values: Vec<f64> = current.iter().map(|p| p.execution_time_ms).collect();

        // Use proper Welch's t-test
        match self
            .stats_calc
            .welch_t_test(&baseline_values, &current_values)
        {
            Ok(result) => result.p_value,
            Err(_) => 1.0, // No significant difference if test fails
        }
    }

    /// Calculate average execution time
    fn calculate_average_execution_time(&self, data: &[PerformanceDataPoint]) -> f64 {
        if data.is_empty() {
            return 0.0;
        }
        data.iter().map(|p| p.execution_time_ms).sum::<f64>() / data.len() as f64
    }

    /// Calculate variance of execution times using proper sample variance
    fn calculate_variance(&self, data: &[PerformanceDataPoint]) -> f64 {
        if data.len() < 2 {
            return 0.0;
        }

        let values: Vec<f64> = data.iter().map(|p| p.execution_time_ms).collect();
        self.stats_calc.sample_variance(&values)
    }

    /// Determine overall regression status
    fn determine_overall_status(&self, regressions: &[MetricRegression]) -> RegressionStatus {
        if regressions.is_empty() {
            return RegressionStatus::None;
        }

        let max_severity = regressions
            .iter()
            .map(|r| &r.severity)
            .max_by_key(|s| match s {
                RegressionSeverity::Low => 1,
                RegressionSeverity::Medium => 2,
                RegressionSeverity::High => 3,
                RegressionSeverity::Critical => 4,
            });

        match max_severity {
            Some(RegressionSeverity::Critical) => RegressionStatus::Critical,
            Some(RegressionSeverity::High) => RegressionStatus::Significant,
            Some(RegressionSeverity::Medium) => RegressionStatus::Significant,
            Some(RegressionSeverity::Low) => RegressionStatus::Minor,
            None => RegressionStatus::None,
        }
    }

    /// Generate recommendations based on analysis
    fn generate_recommendations(
        &self,
        regressions: &[MetricRegression],
        temporal: &TemporalAnalysis,
        statistical: &StatisticalAnalysis,
    ) -> Vec<RegressionRecommendation> {
        let mut recommendations = Vec::new();

        // Regression-based recommendations
        for regression in regressions {
            match regression.severity {
                RegressionSeverity::Critical => {
                    recommendations.push(RegressionRecommendation {
                        recommendation_type: RecommendationType::Investigation,
                        priority: Priority::Critical,
                        description: format!(
                            "Critical performance regression detected in {:?}",
                            regression.metric
                        ),
                        expected_impact: ImpactLevel::High,
                        effort_level: EffortLevel::High,
                        actions: vec![
                            "Immediately investigate query execution plans".to_string(),
                            "Check for recent system changes or deployments".to_string(),
                            "Review database statistics and indexes".to_string(),
                        ],
                    });
                }
                RegressionSeverity::High => {
                    recommendations.push(RegressionRecommendation {
                        recommendation_type: RecommendationType::Optimization,
                        priority: Priority::High,
                        description: "Significant performance degradation requires optimization"
                            .to_string(),
                        expected_impact: ImpactLevel::High,
                        effort_level: EffortLevel::Medium,
                        actions: vec![
                            "Analyze query execution plans for changes".to_string(),
                            "Consider query rewriting or index optimization".to_string(),
                        ],
                    });
                }
                _ => {
                    recommendations.push(RegressionRecommendation {
                        recommendation_type: RecommendationType::Monitoring,
                        priority: Priority::Medium,
                        description: "Minor performance degradation detected".to_string(),
                        expected_impact: ImpactLevel::Medium,
                        effort_level: EffortLevel::Low,
                        actions: vec![
                            "Continue monitoring performance trends".to_string(),
                            "Schedule regular performance review".to_string(),
                        ],
                    });
                }
            }
        }

        // Temporal pattern recommendations
        if matches!(temporal.trend, TrendDirection::Degrading) && temporal.trend_strength > 0.7 {
            recommendations.push(RegressionRecommendation {
                recommendation_type: RecommendationType::Investigation,
                priority: Priority::High,
                description: "Strong degrading trend detected over time".to_string(),
                expected_impact: ImpactLevel::High,
                effort_level: EffortLevel::Medium,
                actions: vec![
                    "Investigate underlying cause of performance trend".to_string(),
                    "Consider proactive optimization measures".to_string(),
                ],
            });
        }

        // Anomaly-based recommendations
        if statistical.anomalies.len() > 5 {
            // High number of anomalies
            recommendations.push(RegressionRecommendation {
                recommendation_type: RecommendationType::Investigation,
                priority: Priority::Medium,
                description: "High number of performance anomalies detected".to_string(),
                expected_impact: ImpactLevel::Medium,
                effort_level: EffortLevel::Medium,
                actions: vec![
                    "Investigate causes of performance variability".to_string(),
                    "Consider system stability improvements".to_string(),
                ],
            });
        }

        recommendations
    }

    /// Calculate confidence level in the analysis
    fn calculate_confidence_level(
        &self,
        statistical: &StatisticalAnalysis,
        data_size: usize,
    ) -> ConfidenceLevel {
        let mut confidence_score = 0.0;

        // Data size factor
        confidence_score += match data_size {
            n if n >= 1000 => 0.4,
            n if n >= 500 => 0.3,
            n if n >= 100 => 0.2,
            _ => 0.1,
        };

        // Statistical significance factor
        for test in &statistical.tests_performed {
            if test.is_significant {
                confidence_score += 0.2;
            }
        }

        // Distribution normality factor
        if matches!(
            statistical.distribution.distribution_type,
            DistributionType::Normal
        ) {
            confidence_score += 0.2;
        }

        // Low anomaly factor
        if (statistical.anomalies.len() as f64) / (data_size as f64) < 0.05 {
            confidence_score += 0.2;
        }

        match confidence_score {
            s if s >= 0.95 => ConfidenceLevel::VeryHigh,
            s if s >= 0.85 => ConfidenceLevel::High,
            s if s >= 0.70 => ConfidenceLevel::Medium,
            _ => ConfidenceLevel::Low,
        }
    }

    /// Create analysis result for insufficient data
    fn create_insufficient_data_analysis(&self) -> RegressionAnalysis {
        RegressionAnalysis {
            status: RegressionStatus::InsufficientData,
            metric_regressions: Vec::new(),
            temporal_analysis: TemporalAnalysis {
                analysis_period: TimePeriod {
                    start: Utc::now(),
                    end: Utc::now(),
                    duration_hours: 0,
                },
                trend: TrendDirection::Stable,
                trend_strength: 0.0,
                seasonality: None,
                change_points: Vec::new(),
            },
            statistical_analysis: StatisticalAnalysis {
                tests_performed: Vec::new(),
                distribution: DistributionAnalysis {
                    distribution_type: DistributionType::Unknown,
                    mean: 0.0,
                    std_dev: 0.0,
                    skewness: 0.0,
                    kurtosis: 0.0,
                    outlier_percentage: 0.0,
                },
                anomalies: Vec::new(),
                correlations: Vec::new(),
            },
            recommendations: vec![RegressionRecommendation {
                recommendation_type: RecommendationType::Monitoring,
                priority: Priority::Medium,
                description: "Collect more performance data for analysis".to_string(),
                expected_impact: ImpactLevel::Medium,
                effort_level: EffortLevel::Low,
                actions: vec![
                    "Enable more detailed performance logging".to_string(),
                    "Wait for more data points to accumulate".to_string(),
                ],
            }],
            confidence_level: ConfidenceLevel::Low,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn create_test_data(
        base_time: f64,
        trend: f64,
        noise: f64,
        count: usize,
    ) -> Vec<PerformanceDataPoint> {
        let mut data = Vec::new();
        let start_time = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();

        for i in 0..count {
            let time_factor = i as f64;
            let execution_time = base_time + trend * time_factor + noise * (i % 10) as f64;

            data.push(PerformanceDataPoint {
                timestamp: start_time + chrono::Duration::hours(i as i64),
                execution_time_ms: execution_time,
                memory_usage_mb: Some(100.0 + execution_time * 0.1),
                cpu_usage_percent: Some(20.0 + execution_time * 0.05),
                io_operations: Some((execution_time * 10.0) as u64),
                cache_hit_ratio: Some(0.95 - execution_time * 0.001),
            });
        }

        data
    }

    #[test]
    fn test_no_regression_detection() {
        let detector = RegressionDetector::new();
        let data = create_test_data(100.0, 0.0, 5.0, 100); // Stable performance

        let result = detector.analyze(&data).unwrap();

        assert_eq!(result.status, RegressionStatus::None);
        assert!(result.metric_regressions.is_empty());
        assert_eq!(result.temporal_analysis.trend, TrendDirection::Stable);
    }

    #[test]
    fn test_minor_regression_detection() {
        let detector = RegressionDetector::new();
        let data = create_test_data(100.0, 0.2, 5.0, 100); // Slight degradation

        let result = detector.analyze(&data).unwrap();

        assert_ne!(result.status, RegressionStatus::None);
        assert!(!result.metric_regressions.is_empty());
        assert_eq!(result.temporal_analysis.trend, TrendDirection::Degrading);
    }

    #[test]
    fn test_significant_regression_detection() {
        let detector = RegressionDetector::new();
        let data = create_test_data(100.0, 1.0, 5.0, 100); // Clear degradation

        let result = detector.analyze(&data).unwrap();

        assert!(matches!(
            result.status,
            RegressionStatus::Significant | RegressionStatus::Critical
        ));
        assert!(!result.metric_regressions.is_empty());
        assert_eq!(result.temporal_analysis.trend, TrendDirection::Degrading);
        assert!(result.temporal_analysis.trend_strength > 0.5);
    }

    #[test]
    fn test_improvement_detection() {
        let detector = RegressionDetector::new();
        let data = create_test_data(200.0, -0.5, 5.0, 100); // Performance improvement

        let result = detector.analyze(&data).unwrap();

        assert_eq!(result.temporal_analysis.trend, TrendDirection::Improving);
        assert!(result.temporal_analysis.trend_strength > 0.3);
    }

    #[test]
    fn test_volatile_performance_detection() {
        let detector = RegressionDetector::new();
        let data = create_test_data(100.0, 0.0, 100.0, 100); // Very high variance for clear volatility

        let result = detector.analyze(&data).unwrap();

        assert_eq!(result.temporal_analysis.trend, TrendDirection::Volatile);
        // Anomaly detection may or may not find anomalies in cyclical patterns
        // The important thing is detecting the volatile trend
    }

    #[test]
    fn test_change_point_detection() {
        let detector = RegressionDetector::new();
        let mut data = create_test_data(100.0, 0.0, 5.0, 50);
        data.extend(create_test_data(150.0, 0.0, 5.0, 50)); // Step change

        let result = detector.analyze(&data).unwrap();

        assert!(!result.temporal_analysis.change_points.is_empty());
        let change_point = &result.temporal_analysis.change_points[0];
        assert_eq!(change_point.change_type, ChangeType::Degradation);
        assert!(change_point.magnitude > 0.2);
    }

    #[test]
    fn test_insufficient_data() {
        let detector = RegressionDetector::new();
        let data = create_test_data(100.0, 0.0, 5.0, 10); // Too few data points

        let result = detector.analyze(&data).unwrap();

        assert_eq!(result.status, RegressionStatus::InsufficientData);
        assert_eq!(result.confidence_level, ConfidenceLevel::Low);
    }

    #[test]
    fn test_recommendation_generation() {
        let detector = RegressionDetector::new();
        let data = create_test_data(100.0, 2.0, 5.0, 100); // Strong degradation

        let result = detector.analyze(&data).unwrap();

        assert!(!result.recommendations.is_empty());

        let has_high_priority = result
            .recommendations
            .iter()
            .any(|r| matches!(r.priority, Priority::High | Priority::Critical));
        assert!(has_high_priority);
    }

    #[test]
    fn test_correlation_analysis() {
        let detector = RegressionDetector::new();
        let data = create_test_data(100.0, 0.5, 5.0, 100);

        let result = detector.analyze(&data).unwrap();

        // Should detect correlation between execution time and memory usage
        let has_correlation = result
            .statistical_analysis
            .correlations
            .iter()
            .any(|c| !matches!(c.strength, CorrelationStrength::VeryWeak));
        assert!(has_correlation);
    }
}
