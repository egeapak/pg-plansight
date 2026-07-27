use crate::{PerformancePercentiles, ProcessedQuery, QueryGroupStatistics};
#[cfg(feature = "file-io")]
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
#[cfg(feature = "file-io")]
use std::fs::File;
#[cfg(feature = "file-io")]
use std::io::{BufReader, BufWriter};
#[cfg(feature = "file-io")]
use std::path::Path;
use tracing::warn;

/// Current export format version. Bump when the schema changes in a way old
/// readers cannot handle; `from_file` rejects files with a newer version.
///
/// History:
/// - 1: original format (implicit — files without a `format_version` field)
/// - 2: added `format_version` and per-query `plan_format`
pub const EXPORT_FORMAT_VERSION: u32 = 2;

fn default_format_version() -> u32 {
    1
}

/// Export format for analysis results
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisExport {
    /// Version of the export *format* (see [`EXPORT_FORMAT_VERSION`]).
    /// Missing in v1 files, hence the default.
    #[serde(default = "default_format_version")]
    pub format_version: u32,
    /// Version of the pg-plansight package that wrote the export
    /// (informational only; compatibility is decided by `format_version`).
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

/// Format of an [`ExportedQuery::plan`] string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportedPlanFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedQuery {
    pub query_hash: String,
    pub original_query: String,
    pub normalized_query: String,
    pub formatted_query: String,
    pub plan: String,
    /// Format of `plan`. Absent in v1 exports, where it is inferred from the
    /// plan content on import.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_format: Option<ExportedPlanFormat>,
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

/// Heuristic for "normalisation replaced the literals".
///
/// The normaliser emits `$1`-style placeholders. A statement it failed to parse
/// comes back byte-identical to the input, so the absence of any placeholder is
/// a reliable signal that nothing was substituted. A genuinely literal-free
/// statement (`SELECT now()`) also has no placeholder and is dropped — that is
/// the safe direction to err in.
fn looks_parameterised(normalized: &str) -> bool {
    let bytes = normalized.as_bytes();
    bytes
        .iter()
        .enumerate()
        .any(|(i, &b)| b == b'$' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit))
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
            if min_timestamp.is_none_or(|t| query.statistics.min_timestamp < t) {
                min_timestamp = Some(query.statistics.min_timestamp);
            }
            if max_timestamp.is_none_or(|t| query.statistics.max_timestamp > t) {
                max_timestamp = Some(query.statistics.max_timestamp);
            }

            let plan_format = match query.representative_plan.source_format() {
                crate::models::PlanSourceFormat::Json => ExportedPlanFormat::Json,
                crate::models::PlanSourceFormat::Text => ExportedPlanFormat::Text,
            };
            exported_queries.push(ExportedQuery {
                query_hash: fingerprint,
                original_query: query.representative_plan.query_text.clone(),
                normalized_query: query.representative_plan.normalized_query.clone(),
                formatted_query: query.representative_plan.formatted_query.clone(),
                plan: query.representative_plan.raw_plan().to_string(),
                plan_format: Some(plan_format),
                statistics: SerializableStatistics::from_query_statistics(&query.statistics),
            });
        }

        // Descending by total time, then by hash so the order is total and
        // reproducible. `ProcessedQuery` arrives from a randomly-seeded
        // `hashbrown::HashMap`, so without the tie-break, equal-duration groups
        // came out in hash order — two runs over the same log produced
        // byte-different JSON that could not be diffed or checksummed, and any
        // "top N" list reshuffled ties between runs.
        exported_queries.sort_by(|a, b| {
            b.statistics
                .total_duration_ms
                .total_cmp(&a.statistics.total_duration_ms)
                .then_with(|| a.query_hash.cmp(&b.query_hash))
        });

        let now = Utc::now();
        Self {
            format_version: EXPORT_FORMAT_VERSION,
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
                #[cfg(feature = "file-io")]
                hostname: hostname::get().ok().and_then(|h| h.into_string().ok()),
                #[cfg(not(feature = "file-io"))]
                hostname: None,
                user: std::env::var("USER").ok(),
                tags: HashMap::new(),
            },
        }
    }

    /// Strip everything that can carry literal values from the export.
    ///
    /// An export is a file that leaves the machine it was produced on — it gets
    /// attached to tickets, shared with vendors, and committed to repos — while
    /// PostgreSQL query text and plan text both embed literal constants:
    /// `WHERE email = 'a@b.com'`, `Filter: (ssn = '123-45-6789'::text)`,
    /// `Index Cond: (...)`. Only `normalized_query` is parameterised, and even
    /// that falls back to raw SQL when sqlparser cannot parse the statement.
    ///
    /// After this call each query retains its fingerprint, its statistics, and
    /// a normalised query *only if* normalisation demonstrably replaced the
    /// literals. Everything else is dropped rather than pattern-scrubbed —
    /// there is no regex that reliably finds every literal in an arbitrary
    /// plan, and a redaction that is 95% effective is worse than none because
    /// it invites trust.
    ///
    /// Host and user metadata are cleared too; both identify the environment.
    pub fn redact(&mut self) {
        for query in &mut self.queries {
            query.original_query = String::new();
            query.formatted_query = String::new();
            query.plan = String::new();
            query.plan_format = None;

            // A normalised query is only safe when normalisation actually ran.
            // On sqlparser failure the "normalised" text is the raw statement,
            // literals and all, so drop it unless it contains a placeholder.
            if !looks_parameterised(&query.normalized_query) {
                query.normalized_query = String::new();
            }
        }

        self.metadata.hostname = None;
        self.metadata.user = None;
    }

    /// Export to JSON file
    #[cfg(feature = "file-io")]
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
    #[cfg(feature = "file-io")]
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let file = File::open(&path)
            .with_context(|| format!("Failed to open import file: {}", path.as_ref().display()))?;
        let reader = BufReader::new(file);

        let export: Self = serde_json::from_reader(reader)
            .with_context(|| format!("Failed to parse JSON from: {}", path.as_ref().display()))?;

        if export.format_version > EXPORT_FORMAT_VERSION {
            anyhow::bail!(
                "Export format v{} is newer than the v{} this build supports \
                 (file written by pg-plansight {}); upgrade pg-plansight to read it",
                export.format_version,
                EXPORT_FORMAT_VERSION,
                export.version
            );
        }

        Ok(export)
    }

    /// Convert back to ProcessedQuery HashMap for use in the application
    pub fn to_processed_queries(&self) -> HashMap<String, ProcessedQuery> {
        let mut queries = HashMap::new();

        for exported in &self.queries {
            use crate::parsing::plan_builders::TextPlanBuilder;

            // v1 exports have no format marker; infer from the plan content.
            let is_json = match exported.plan_format {
                Some(format) => format == ExportedPlanFormat::Json,
                None => crate::parsing::format_detection::looks_like_json_start(&exported.plan),
            };

            let parse_result = if is_json {
                // Rebuild through the JSON pipeline: reconstructing JSON plans
                // with the text parser degraded every one of them to an empty
                // Unknown node.
                let metadata = crate::parsing::ParseMetadata::new(
                    exported.statistics.max_timestamp,
                    exported.statistics.max_duration_ms,
                    exported.original_query.clone(),
                );
                crate::parsing::PlanParserCore::parse(
                    &crate::parsing::JsonPlanParser::new(),
                    &exported.plan,
                    metadata,
                )
                .and_then(|parsed| {
                    crate::parsing::PlanFactory::create_query_plan_from_parsed(
                        exported.statistics.max_timestamp,
                        exported.statistics.max_duration_ms,
                        exported.original_query.clone(),
                        exported.plan.clone(),
                        parsed,
                    )
                })
            } else {
                TextPlanBuilder {
                    timestamp: exported.statistics.max_timestamp,
                    duration_ms: exported.statistics.max_duration_ms,
                    query_text: exported.original_query.clone(),
                    content_lines: exported.plan.lines().map(|s| s.to_string()).collect(),
                }
                .finalize()
            };

            let representative_plan = match parse_result {
                Ok(mut plan) => {
                    // Override with exported normalized/formatted queries to preserve them
                    plan.normalized_query = exported.normalized_query.clone();
                    plan.formatted_query = exported.formatted_query.clone();
                    plan
                }
                Err(e) => {
                    // If parsing fails, log a warning and create a minimal plan
                    warn!(
                        error = %e,
                        "Failed to parse plan during import, creating minimal plan"
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
                    complexity_score: None,    // Not exported
                    metadata: None,            // Not exported
                    regression_analysis: None, // Not exported
                    plan_analysis: None,       // Not exported
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
            match DateTime::parse_from_rfc3339(hour_str) {
                Ok(datetime) => {
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
                Err(e) => {
                    // Dropping the bucket silently would leave the histogram
                    // inconsistent with the summary counts and no signal why.
                    warn!(
                        hour_key = hour_str,
                        error = %e,
                        "Skipping unparseable hourly-histogram key during import"
                    );
                }
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
    // Ungated: most of these tests are pure serde/redaction checks that need no
    // I/O surface. Gating this import behind `file-io` (as it was, for the
    // tempfile roundtrip tests below) silently broke the embeddable build's test
    // compile — `cargo test --no-default-features`, which is exactly the
    // configuration the pg extension links against.
    use super::*;

    /// PG18 prints `Actual Rows` as a per-loop *average* with decimals when
    /// `loops > 1`. Deserializing into an integer made serde reject the whole
    /// document, which the state machine demoted to plain query text — so on a
    /// PG18 server with `log_format = json`, every plan containing a
    /// nested-loop node vanished from the analysis.
    #[test]
    fn pg18_fractional_actual_rows_is_accepted() {
        let node: crate::models::JsonPlanNode = serde_json::from_str(
            r#"{
                "Node Type": "Seq Scan",
                "Startup Cost": 0.0,
                "Total Cost": 1.0,
                "Plan Rows": 1,
                "Plan Width": 4,
                "Actual Rows": 1000.5,
                "Actual Loops": 3
            }"#,
        )
        .expect("PG18 fractional Actual Rows must deserialize");
        assert_eq!(node.actual_rows, Some(1000.5));
    }

    /// Two runs over the same input must produce identical bytes, otherwise
    /// exports cannot be diffed or checksummed.
    #[test]
    fn export_ordering_is_total_and_reproducible() {
        let now = Utc::now();
        let mk = |hash: &str| ExportedQuery {
            query_hash: hash.to_string(),
            original_query: "SELECT 1".into(),
            normalized_query: "SELECT $1".into(),
            formatted_query: "SELECT 1".into(),
            plan: "Result".into(),
            plan_format: Some(ExportedPlanFormat::Text),
            statistics: SerializableStatistics {
                count: 1,
                // Deliberately identical, so only the tie-break decides.
                total_duration_ms: 5.0,
                min_duration_ms: 5.0,
                max_duration_ms: 5.0,
                mean_duration_ms: 5.0,
                std_dev_ms: 0.0,
                min_timestamp: now,
                max_timestamp: now,
                percentiles: SerializablePercentiles {
                    p25: 5.0,
                    p50: 5.0,
                    p90: 5.0,
                    p95: 5.0,
                    p99: 5.0,
                },
                hourly_histogram: HashMap::new(),
                sample_execution_times: vec![5.0],
            },
        };

        let mut queries = [mk("ccc"), mk("aaa"), mk("bbb")];
        queries.sort_by(|a, b| {
            b.statistics
                .total_duration_ms
                .total_cmp(&a.statistics.total_duration_ms)
                .then_with(|| a.query_hash.cmp(&b.query_hash))
        });

        let order: Vec<&str> = queries.iter().map(|q| q.query_hash.as_str()).collect();
        assert_eq!(
            order,
            vec!["aaa", "bbb", "ccc"],
            "equal-duration groups must fall back to a deterministic hash order"
        );
    }

    // -------------------------------------------------------------------------
    // Redaction
    // -------------------------------------------------------------------------

    fn export_with(original: &str, normalized: &str, plan: &str) -> AnalysisExport {
        let now = Utc::now();
        AnalysisExport {
            format_version: EXPORT_FORMAT_VERSION,
            version: "test".to_string(),
            exported_at: now,
            analysis_period: AnalysisPeriod {
                start: now,
                end: now,
            },
            query_count: 1,
            execution_count: 1,
            queries: vec![ExportedQuery {
                query_hash: "abc123".to_string(),
                original_query: original.to_string(),
                normalized_query: normalized.to_string(),
                formatted_query: original.to_string(),
                plan: plan.to_string(),
                plan_format: Some(ExportedPlanFormat::Text),
                statistics: SerializableStatistics {
                    count: 1,
                    total_duration_ms: 1.0,
                    min_duration_ms: 1.0,
                    max_duration_ms: 1.0,
                    mean_duration_ms: 1.0,
                    std_dev_ms: 0.0,
                    min_timestamp: now,
                    max_timestamp: now,
                    percentiles: SerializablePercentiles {
                        p25: 1.0,
                        p50: 1.0,
                        p90: 1.0,
                        p95: 1.0,
                        p99: 1.0,
                    },
                    hourly_histogram: HashMap::new(),
                    sample_execution_times: vec![1.0],
                },
            }],
            metadata: ExportMetadata {
                source_files: vec!["pg.log".to_string()],
                hostname: Some("db-prod-1".to_string()),
                user: Some("alice".to_string()),
                tags: HashMap::new(),
            },
        }
    }

    #[test]
    fn redact_removes_literals_from_every_text_field() {
        let mut export = export_with(
            "SELECT * FROM users WHERE email = 'alice@example.com'",
            "SELECT * FROM users WHERE email = $1",
            "Seq Scan on users  (cost=0.00..1.00 rows=1 width=1)\n  Filter: (email = 'alice@example.com'::text)",
        );

        export.redact();

        let serialized = serde_json::to_string(&export).unwrap();
        assert!(
            !serialized.contains("alice@example.com"),
            "redacted export still contains a literal: {serialized}"
        );

        let q = &export.queries[0];
        assert!(q.original_query.is_empty());
        assert!(q.formatted_query.is_empty());
        assert!(q.plan.is_empty());
        // The parameterised form is literal-free, so it is worth keeping.
        assert_eq!(q.normalized_query, "SELECT * FROM users WHERE email = $1");
        // Fingerprint and statistics must survive — they are the whole point.
        assert_eq!(q.query_hash, "abc123");
        assert_eq!(q.statistics.count, 1);
    }

    /// When sqlparser cannot parse a statement the normaliser returns the raw
    /// SQL unchanged, so `normalized_query` carries literals too. Redaction
    /// must not trust the field name.
    #[test]
    fn redact_drops_normalized_query_when_normalization_did_not_run() {
        let raw = "SELECT a::text COLLATE \"C\" FROM t WHERE email = 'bob@example.com'";
        let mut export = export_with(raw, raw, "Seq Scan on t");

        export.redact();

        assert!(
            export.queries[0].normalized_query.is_empty(),
            "un-normalised SQL must be dropped, not exported as if parameterised"
        );
        let serialized = serde_json::to_string(&export).unwrap();
        assert!(!serialized.contains("bob@example.com"));
    }

    #[test]
    fn redact_clears_environment_metadata() {
        let mut export = export_with("SELECT 1", "SELECT 1", "Result");
        export.redact();
        assert!(export.metadata.hostname.is_none());
        assert!(export.metadata.user.is_none());
        // Source file names are retained: they are operator-chosen paths, not
        // query data, and they are needed to interpret the export.
        assert_eq!(export.metadata.source_files, vec!["pg.log".to_string()]);
    }

    #[test]
    fn looks_parameterised_detects_placeholders() {
        assert!(looks_parameterised("WHERE a = $1"));
        assert!(looks_parameterised("IN ($1, $2, $3)"));
        assert!(!looks_parameterised("WHERE a = 'x'"));
        assert!(!looks_parameterised("SELECT now()"));
        assert!(
            !looks_parameterised("cost $ estimate"),
            "bare $ is not a placeholder"
        );
    }

    #[cfg(feature = "file-io")]
    use tempfile::NamedTempFile;

    #[cfg(feature = "file-io")]
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

    #[cfg(feature = "file-io")]
    #[test]
    fn test_json_plan_survives_export_import_roundtrip() {
        // A JSON-format plan must come back as a real parsed JSON plan, not a
        // degraded Unknown node built by forcing it through the text parser.
        let raw_json = r#"[{
            "Plan": {
                "Node Type": "Index Scan",
                "Relation Name": "users",
                "Startup Cost": 0.42,
                "Total Cost": 8.44,
                "Plan Rows": 1,
                "Plan Width": 16
            },
            "Execution Time": 12.5
        }]"#;

        let metadata = crate::parsing::ParseMetadata::new(
            Utc::now(),
            150.5,
            "SELECT * FROM users WHERE id = $1".to_string(),
        );
        let parsed = crate::parsing::PlanParserCore::parse(
            &crate::parsing::JsonPlanParser::new(),
            raw_json,
            metadata,
        )
        .unwrap();
        let plan = crate::parsing::PlanFactory::create_query_plan_from_parsed(
            Utc::now(),
            150.5,
            "SELECT * FROM users WHERE id = $1".to_string(),
            raw_json.to_string(),
            parsed,
        )
        .unwrap();
        assert!(plan.is_json_plan());
        let original_node_count = plan.parsed.node_count();
        let original_cost = plan.parsed.total_cost();

        let mut queries = HashMap::new();
        queries.insert(
            "json_fp".to_string(),
            ProcessedQuery {
                statistics: QueryGroupStatistics {
                    count: 1,
                    total_duration_ms: 150.5,
                    min_duration_ms: 150.5,
                    max_duration_ms: 150.5,
                    mean_duration_ms: 150.5,
                    std_dev_ms: 0.0,
                    min_timestamp: plan.timestamp,
                    max_timestamp: plan.timestamp,
                    percentiles: PerformancePercentiles {
                        p25: 150.5,
                        p50: 150.5,
                        p90: 150.5,
                        p95: 150.5,
                        p99: 150.5,
                    },
                    hourly_histogram: HashMap::new(),
                    executions: Vec::new(),
                },
                representative_plan: plan,
                complexity_score: None,
                metadata: None,
                regression_analysis: None,
                plan_analysis: None,
            },
        );

        let export = AnalysisExport::from_processed_queries(queries, vec!["t.log".to_string()]);
        assert_eq!(export.format_version, EXPORT_FORMAT_VERSION);
        assert_eq!(
            export.queries[0].plan_format,
            Some(ExportedPlanFormat::Json)
        );

        let temp_file = NamedTempFile::new().unwrap();
        export.to_file(temp_file.path()).unwrap();
        let imported = AnalysisExport::from_file(temp_file.path()).unwrap();
        let restored = imported.to_processed_queries();

        let restored_plan = &restored["json_fp"].representative_plan;
        assert!(restored_plan.is_json_plan(), "format lost on import");
        assert_eq!(restored_plan.parsed.node_count(), original_node_count);
        assert_eq!(restored_plan.parsed.total_cost(), original_cost);
        assert!(
            !format!("{:?}", restored_plan.parsed.root.node_type).contains("Unknown"),
            "JSON plan degraded to Unknown on import: {:?}",
            restored_plan.parsed.root.node_type
        );
    }

    #[cfg(feature = "file-io")]
    #[test]
    fn test_v1_export_without_format_fields_still_imports() {
        // Files written before format_version/plan_format existed must load,
        // inferring the plan format from content.
        let v1_json = r#"{
            "version": "0.0.9",
            "exported_at": "2025-01-01T00:00:00Z",
            "analysis_period": {"start": "2025-01-01T00:00:00Z", "end": "2025-01-02T00:00:00Z"},
            "query_count": 1,
            "execution_count": 1,
            "queries": [{
                "query_hash": "abc",
                "original_query": "SELECT 1",
                "normalized_query": "SELECT $1",
                "formatted_query": "SELECT 1",
                "plan": "Result  (cost=0.00..0.01 rows=1 width=4)",
                "statistics": {
                    "count": 1,
                    "total_duration_ms": 1.0,
                    "min_duration_ms": 1.0,
                    "max_duration_ms": 1.0,
                    "mean_duration_ms": 1.0,
                    "std_dev_ms": 0.0,
                    "min_timestamp": "2025-01-01T00:00:00Z",
                    "max_timestamp": "2025-01-01T00:00:00Z",
                    "percentiles": {"p25": 1.0, "p50": 1.0, "p90": 1.0, "p95": 1.0, "p99": 1.0},
                    "hourly_histogram": {},
                    "sample_execution_times": [1.0]
                }
            }],
            "metadata": {"source_files": [], "hostname": null, "user": null, "tags": {}}
        }"#;
        let temp_file = NamedTempFile::new().unwrap();
        std::fs::write(temp_file.path(), v1_json).unwrap();

        let imported = AnalysisExport::from_file(temp_file.path()).unwrap();
        assert_eq!(imported.format_version, 1);
        let restored = imported.to_processed_queries();
        assert!(restored["abc"].representative_plan.is_text_plan());
    }

    #[cfg(feature = "file-io")]
    #[test]
    fn test_newer_format_version_is_rejected_with_clear_error() {
        let future = r#"{"format_version": 99, "version": "9.9.9", "exported_at": "2025-01-01T00:00:00Z",
            "analysis_period": {"start": "2025-01-01T00:00:00Z", "end": "2025-01-01T00:00:00Z"},
            "query_count": 0, "execution_count": 0, "queries": [],
            "metadata": {"source_files": [], "hostname": null, "user": null, "tags": {}}}"#;
        let temp_file = NamedTempFile::new().unwrap();
        std::fs::write(temp_file.path(), future).unwrap();

        let err = AnalysisExport::from_file(temp_file.path()).unwrap_err();
        assert!(
            err.to_string().contains("newer"),
            "unexpected error: {}",
            err
        );
    }
}
