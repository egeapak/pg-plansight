use crate::{PerformancePercentiles, ProcessedQuery, QueryGroupStatistics};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;

/// Export format for analysis results
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisExport {
    /// Version of the export format for future compatibility
    pub version: String,
    /// When this export was created
    pub exported_at: DateTime<Utc>,
    /// Date range of the analyzed queries
    pub analysis_period: AnalysisPeriod,
    /// Number of unique query patterns
    pub query_count: usize,
    /// Total number of query executions
    pub execution_count: usize,
    /// Processed queries with statistics
    pub queries: Vec<ExportedQuery>,
    /// Optional metadata
    pub metadata: ExportMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisPeriod {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportMetadata {
    /// Source log files
    pub source_files: Vec<String>,
    /// Hostname where analysis was performed
    pub hostname: Option<String>,
    /// User who performed the analysis
    pub user: Option<String>,
    /// Custom tags
    pub tags: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedQuery {
    pub query_hash: String,
    pub original_query: String,
    pub normalized_query: String,
    pub formatted_query: String,
    pub plan: String,
    pub statistics: SerializableStatistics,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializableStatistics {
    pub count: usize,
    pub total_duration_ms: f64,
    pub min_duration_ms: f64,
    pub max_duration_ms: f64,
    pub mean_duration_ms: f64,
    pub std_dev_ms: f64,
    pub min_timestamp: DateTime<Utc>,
    pub max_timestamp: DateTime<Utc>,
    pub percentiles: SerializablePercentiles,
    pub hourly_histogram: HashMap<String, SerializableHourlyMetrics>, // ISO8601 hour string as key
    pub sample_execution_times: Vec<f64>, // Sample of execution times for visualization
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializablePercentiles {
    pub p25: f64,
    pub p50: f64,
    pub p90: f64,
    pub p95: f64,
    pub p99: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializableHourlyMetrics {
    pub count: usize,
    pub total_duration_ms: f64,
    pub min_duration_ms: f64,
    pub max_duration_ms: f64,
    pub mean_duration_ms: f64,
}

impl AnalysisExport {
    /// Create a new export from processed queries
    pub fn from_processed_queries(
        queries: HashMap<String, ProcessedQuery>,
        source_files: Vec<String>,
    ) -> Self {
        let mut exported_queries = Vec::new();
        let mut min_timestamp = None;
        let mut max_timestamp = None;
        let mut total_executions = 0;

        for (fingerprint, query) in queries {
            total_executions += query.statistics.count;

            // Track date range
            if min_timestamp.is_none() || query.statistics.min_timestamp < min_timestamp.unwrap() {
                min_timestamp = Some(query.statistics.min_timestamp);
            }
            if max_timestamp.is_none() || query.statistics.max_timestamp > max_timestamp.unwrap() {
                max_timestamp = Some(query.statistics.max_timestamp);
            }

            exported_queries.push(ExportedQuery {
                query_hash: fingerprint,
                original_query: query.representative_plan.query_text.clone(),
                normalized_query: query.representative_plan.normalized_query.clone(),
                formatted_query: query.representative_plan.formatted_query.clone(),
                plan: query.representative_plan.raw_plan().to_string(),
                statistics: SerializableStatistics::from_query_statistics(&query.statistics),
            });
        }

        // Sort by total duration (descending) for better readability
        exported_queries.sort_by(|a, b| {
            b.statistics
                .total_duration_ms
                .partial_cmp(&a.statistics.total_duration_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let now = Utc::now();
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            exported_at: now,
            analysis_period: AnalysisPeriod {
                start: min_timestamp.unwrap_or(now),
                end: max_timestamp.unwrap_or(now),
            },
            query_count: exported_queries.len(),
            execution_count: total_executions,
            queries: exported_queries,
            metadata: ExportMetadata {
                source_files,
                hostname: hostname::get().ok().and_then(|h| h.into_string().ok()),
                user: std::env::var("USER").ok(),
                tags: HashMap::new(),
            },
        }
    }

    /// Export to JSON file
    pub fn to_file<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let file = File::create(&path).with_context(|| {
            format!("Failed to create export file: {}", path.as_ref().display())
        })?;
        let writer = BufWriter::new(file);

        serde_json::to_writer_pretty(writer, self)
            .with_context(|| format!("Failed to write JSON to: {}", path.as_ref().display()))?;

        Ok(())
    }

    /// Import from JSON file
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let file = File::open(&path)
            .with_context(|| format!("Failed to open import file: {}", path.as_ref().display()))?;
        let reader = BufReader::new(file);

        let export: Self = serde_json::from_reader(reader)
            .with_context(|| format!("Failed to parse JSON from: {}", path.as_ref().display()))?;

        Ok(export)
    }

    /// Convert back to ProcessedQuery HashMap for use in the application
    pub fn to_processed_queries(&self) -> HashMap<String, ProcessedQuery> {
        let mut queries = HashMap::new();

        for exported in &self.queries {
            use crate::parsing::plan_builders::TextPlanBuilder;

            // Create a TextPlanBuilder manually since it doesn't have a `new` method
            let mut builder = TextPlanBuilder {
                timestamp: exported.statistics.max_timestamp,
                duration_ms: exported.statistics.max_duration_ms,
                query_text: exported.original_query.clone(),
                content_lines: exported.plan.lines().map(|s| s.to_string()).collect(),
            };

            // Finalize the builder to create a QueryPlan with full parsing
            let representative_plan = match builder.finalize() {
                Ok(mut plan) => {
                    // Override with exported normalized/formatted queries to preserve them
                    plan.normalized_query = exported.normalized_query.clone();
                    plan.formatted_query = exported.formatted_query.clone();
                    plan
                }
                Err(e) => {
                    // If parsing fails, log a warning and create a minimal plan
                    eprintln!(
                        "Warning: Failed to parse plan during import: {}. Creating minimal plan.",
                        e
                    );

                    use crate::{
                        NodeType, ParsedPlan, PlanNode, PlanProperties, PlanSource, QueryPlan,
                    };

                    let source = PlanSource::Text {
                        raw_text: exported.plan.clone(),
                        plan_lines: Vec::new(),
                    };

                    let parsed = ParsedPlan {
                        root: PlanNode {
                            node_type: NodeType::Unknown(
                                "Failed to parse during import".to_string(),
                            ),
                            original_text: "Parse failed during import".to_string(),
                            properties: PlanProperties::default(),
                            actuals: None,
                            cost: crate::PlanCost {
                                startup_cost: 0.0,
                                min_total_cost: 0.0,
                                max_total_cost: 0.0,
                                estimated_rows: 0,
                                estimated_width: 0,
                            },
                            children: Vec::new(),
                        },
                        planning_time_ms: None,
                        execution_time_ms: None,
                    };

                    QueryPlan {
                        timestamp: exported.statistics.max_timestamp,
                        duration_ms: exported.statistics.max_duration_ms,
                        query_text: exported.original_query.clone(),
                        normalized_query: exported.normalized_query.clone(),
                        formatted_query: exported.formatted_query.clone(),
                        source,
                        parsed,
                    }
                }
            };

            queries.insert(
                exported.query_hash.clone(),
                ProcessedQuery {
                    representative_plan,
                    statistics: exported.statistics.to_query_statistics(),
                    complexity_score: None,        // Not exported
                    metadata: None,                // Not exported
                    regression_analysis: None,     // Not exported
                    plan_analysis: None,           // Not exported
                    execution_indices: Vec::new(), // Not exported
                },
            );
        }

        queries
    }
}

impl SerializableStatistics {
    fn from_query_statistics(stats: &QueryGroupStatistics) -> Self {
        // Convert hourly histogram to serializable format
        let mut hourly_histogram = HashMap::new();
        for (datetime, metrics) in &stats.hourly_histogram {
            let hour_key = datetime.format("%Y-%m-%dT%H:00:00Z").to_string();
            hourly_histogram.insert(
                hour_key,
                SerializableHourlyMetrics {
                    count: metrics.count,
                    total_duration_ms: metrics.total_duration_ms,
                    min_duration_ms: metrics.min_duration_ms,
                    max_duration_ms: metrics.max_duration_ms,
                    mean_duration_ms: metrics.mean_duration_ms,
                },
            );
        }

        // Sample up to 100 execution times for visualization
        let sample_execution_times: Vec<f64> = stats
            .executions
            .iter()
            .take(100)
            .map(|e| e.duration_ms)
            .collect();

        Self {
            count: stats.count,
            total_duration_ms: stats.total_duration_ms,
            min_duration_ms: stats.min_duration_ms,
            max_duration_ms: stats.max_duration_ms,
            mean_duration_ms: stats.mean_duration_ms,
            std_dev_ms: stats.std_dev_ms,
            min_timestamp: stats.min_timestamp,
            max_timestamp: stats.max_timestamp,
            percentiles: SerializablePercentiles {
                p25: stats.percentiles.p25,
                p50: stats.percentiles.p50,
                p90: stats.percentiles.p90,
                p95: stats.percentiles.p95,
                p99: stats.percentiles.p99,
            },
            hourly_histogram,
            sample_execution_times,
        }
    }

    fn to_query_statistics(&self) -> QueryGroupStatistics {
        // Convert hourly histogram back
        let mut hourly_histogram = HashMap::new();
        for (hour_str, metrics) in &self.hourly_histogram {
            if let Ok(datetime) = DateTime::parse_from_rfc3339(hour_str) {
                hourly_histogram.insert(
                    datetime.with_timezone(&Utc),
                    crate::HourlyMetrics {
                        count: metrics.count,
                        total_duration_ms: metrics.total_duration_ms,
                        min_duration_ms: metrics.min_duration_ms,
                        max_duration_ms: metrics.max_duration_ms,
                        mean_duration_ms: metrics.mean_duration_ms,
                    },
                );
            }
        }

        QueryGroupStatistics {
            count: self.count,
            total_duration_ms: self.total_duration_ms,
            min_duration_ms: self.min_duration_ms,
            max_duration_ms: self.max_duration_ms,
            mean_duration_ms: self.mean_duration_ms,
            std_dev_ms: self.std_dev_ms,
            min_timestamp: self.min_timestamp,
            max_timestamp: self.max_timestamp,
            percentiles: PerformancePercentiles {
                p25: self.percentiles.p25,
                p50: self.percentiles.p50,
                p90: self.percentiles.p90,
                p95: self.percentiles.p95,
                p99: self.percentiles.p99,
            },
            hourly_histogram,
            executions: Vec::new(), // We don't serialize full execution history
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_export_import_roundtrip() {
        use crate::{NodeType, ParsedPlan, PlanNode, PlanProperties, PlanSource, QueryPlan};

        let mut queries = HashMap::new();

        // Create a sample query with proper structure
        let fingerprint = "test_fingerprint_12345".to_string();
        let timestamp = Utc::now();

        // Create a minimal QueryPlan for testing
        let source = PlanSource::Text {
            raw_text: "Seq Scan on users".to_string(),
            plan_lines: Vec::new(),
        };

        let parsed = ParsedPlan {
            root: PlanNode {
                node_type: NodeType::Scan(crate::plan_parser::ScanType::SeqScan {
                    table: crate::plan_parser::TableReference {
                        schema: None,
                        name: "users".to_string(),
                        alias: None,
                    },
                }),
                original_text: "Seq Scan on users".to_string(),
                properties: PlanProperties::default(),
                actuals: None,
                cost: crate::PlanCost {
                    startup_cost: 0.0,
                    min_total_cost: 0.0,
                    max_total_cost: 100.0,
                    estimated_rows: 10,
                    estimated_width: 50,
                },
                children: Vec::new(),
            },
            planning_time_ms: None,
            execution_time_ms: Some(10.0),
        };

        let representative_plan = QueryPlan {
            timestamp,
            duration_ms: 20.0,
            query_text: "SELECT * FROM users WHERE id = $1".to_string(),
            normalized_query: "SELECT * FROM users WHERE id = ?".to_string(),
            formatted_query: "SELECT * FROM users WHERE id = ?".to_string(),
            source,
            parsed,
        };

        queries.insert(
            fingerprint.clone(),
            ProcessedQuery {
                representative_plan,
                statistics: QueryGroupStatistics {
                    count: 10,
                    total_duration_ms: 100.0,
                    min_duration_ms: 5.0,
                    max_duration_ms: 20.0,
                    mean_duration_ms: 10.0,
                    std_dev_ms: 3.0,
                    min_timestamp: timestamp,
                    max_timestamp: timestamp,
                    percentiles: PerformancePercentiles {
                        p25: 7.0,
                        p50: 10.0,
                        p90: 15.0,
                        p95: 18.0,
                        p99: 19.0,
                    },
                    hourly_histogram: HashMap::new(),
                    executions: Vec::new(),
                },
                complexity_score: None,
                metadata: None,
                regression_analysis: None,
                plan_analysis: None,
                execution_indices: Vec::new(),
            },
        );

        // Export
        let export =
            AnalysisExport::from_processed_queries(queries.clone(), vec!["test.log".to_string()]);

        // Write to temp file
        let temp_file = NamedTempFile::new().unwrap();
        export.to_file(temp_file.path()).unwrap();

        // Import back
        let imported = AnalysisExport::from_file(temp_file.path()).unwrap();

        // Verify
        assert_eq!(imported.query_count, 1);
        assert_eq!(imported.execution_count, 10);
        assert_eq!(imported.queries.len(), 1);
        assert_eq!(
            imported.queries[0].normalized_query,
            "SELECT * FROM users WHERE id = ?"
        );

        // Convert back to ProcessedQuery
        let restored_queries = imported.to_processed_queries();
        assert_eq!(restored_queries.len(), 1);
        assert!(restored_queries.contains_key(&fingerprint));
    }
}
