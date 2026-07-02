use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub server: ServerConfig,
    pub log_parsing: LogParsingConfig,
    pub metrics: MetricsConfig,
    pub state: StateConfig,
    pub filters: Option<FiltersConfig>,
    pub pushgateway: Option<PushgatewayConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerConfig {
    #[serde(default = "default_bind_address")]
    pub bind_address: String,
    #[serde(default = "default_metrics_path")]
    pub metrics_path: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LogParsingConfig {
    pub log_paths: Vec<String>,
    #[serde(default = "default_poll_interval")]
    pub poll_interval: String,
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
    /// Maximum file size to process in MB (0 = unlimited)
    #[serde(default = "default_max_file_size_mb")]
    pub max_file_size_mb: u64,
    /// Maximum queries to collect per file (0 = unlimited)
    #[serde(default = "default_max_queries_per_file")]
    pub max_queries_per_file: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MetricsConfig {
    #[serde(default = "default_namespace")]
    pub namespace: String,
    #[serde(default = "default_backends")]
    pub backends: Vec<String>,
    #[serde(default)]
    pub opentelemetry: Option<OpenTelemetryConfig>,
    #[serde(default = "default_histogram_buckets")]
    pub histogram_buckets: Vec<f64>,
    #[serde(default = "default_slow_query_thresholds")]
    pub slow_query_thresholds: Vec<String>,
    #[serde(default = "default_retain_days")]
    pub retain_days: u32,
    /// Maximum number of distinct `normalized_query_hash` label values the
    /// Prometheus backend keeps as live series (0 = unlimited). Prometheus
    /// client label sets are never evicted on their own, so a long-running
    /// daemon that observes many distinct query shapes grows series (and
    /// memory) without bound. When this cap is exceeded the least-recently-used
    /// query hash's series are evicted from the registry.
    #[serde(default = "default_max_query_cardinality")]
    pub max_query_cardinality: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OpenTelemetryConfig {
    #[serde(default = "default_otlp_endpoint")]
    pub endpoint: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StateConfig {
    #[serde(default = "default_database_path")]
    pub database_path: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FiltersConfig {
    pub include_databases: Option<Vec<String>>,
    pub exclude_query_patterns: Option<Vec<String>>,
    pub min_duration_ms: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PushgatewayConfig {
    pub enabled: bool,
    pub url: String,
    pub job_name: String,
    #[serde(default = "default_push_historical_data")]
    pub push_historical_data: bool,
    #[serde(default = "default_historical_batch_size")]
    pub historical_batch_size: usize,
    #[serde(default = "default_push_timeout_seconds")]
    pub timeout_seconds: u64,
    pub basic_auth: Option<BasicAuthConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BasicAuthConfig {
    pub username: String,
    pub password: String,
}

impl Config {
    pub fn load_from_file(path: &PathBuf) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    pub fn poll_interval_duration(&self) -> anyhow::Result<std::time::Duration> {
        parse_duration(&self.log_parsing.poll_interval)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig {
                bind_address: default_bind_address(),
                metrics_path: default_metrics_path(),
            },
            log_parsing: LogParsingConfig {
                log_paths: vec!["/var/log/postgresql/*.log".to_string()],
                poll_interval: default_poll_interval(),
                batch_size: default_batch_size(),
                max_file_size_mb: default_max_file_size_mb(),
                max_queries_per_file: default_max_queries_per_file(),
            },
            metrics: MetricsConfig {
                namespace: default_namespace(),
                backends: default_backends(),
                opentelemetry: None,
                histogram_buckets: default_histogram_buckets(),
                slow_query_thresholds: default_slow_query_thresholds(),
                retain_days: default_retain_days(),
                max_query_cardinality: default_max_query_cardinality(),
            },
            state: StateConfig {
                database_path: default_database_path(),
            },
            filters: None,
            pushgateway: None,
        }
    }
}

fn default_bind_address() -> String {
    // Bind to loopback by default: the endpoint is unauthenticated, so exposing
    // it on all interfaces out of the box is unsafe. Operators who need remote
    // scraping can set an explicit address (and front it with TLS/auth).
    "127.0.0.1:9090".to_string()
}

fn default_metrics_path() -> String {
    "/metrics".to_string()
}

fn default_poll_interval() -> String {
    "30s".to_string()
}

fn default_batch_size() -> usize {
    1000
}

fn default_max_file_size_mb() -> u64 {
    // 0 = unlimited. Kept unlimited by default: the collector reads logs
    // *incrementally* (only new bytes per poll), and an oversized file is
    // skipped wholesale rather than truncated — so a non-zero default would
    // silently and permanently halt ingestion of a busy log once it grows past
    // the limit. Real DoS protection (decompression-bomb cap, recursion cap)
    // lives in the core parser. Set a non-zero value to opt into skipping.
    0
}

fn default_max_queries_per_file() -> usize {
    // 0 = unlimited. Opt-in cap on queries held in memory per file.
    0
}

fn default_namespace() -> String {
    "pg_plansight".to_string()
}

fn default_backends() -> Vec<String> {
    vec!["prometheus".to_string()]
}

fn default_otlp_endpoint() -> String {
    "http://localhost:4317".to_string()
}

fn default_histogram_buckets() -> Vec<f64> {
    vec![0.001, 0.01, 0.1, 1.0, 5.0, 10.0, 30.0, 60.0, 300.0]
}

fn default_slow_query_thresholds() -> Vec<String> {
    vec![
        "1s".to_string(),
        "5s".to_string(),
        "10s".to_string(),
        "30s".to_string(),
    ]
}

fn default_retain_days() -> u32 {
    7
}

fn default_max_query_cardinality() -> usize {
    // Cap the number of distinct query fingerprints tracked by the Prometheus
    // backend. Prometheus client label sets are never evicted, so an unbounded
    // daemon leaks one series set per unique query shape until restart. 10k is
    // generous for real workloads while bounding worst-case memory; set 0 to
    // disable eviction and keep the historical unbounded behavior.
    10_000
}

fn default_database_path() -> String {
    "/var/lib/pg-plansight-exporter/state.db".to_string()
}

fn default_push_historical_data() -> bool {
    false
}

fn default_historical_batch_size() -> usize {
    1000
}

fn default_push_timeout_seconds() -> u64 {
    30
}

#[cfg(test)]
pub(crate) fn parse_duration_pub(s: &str) -> anyhow::Result<std::time::Duration> {
    parse_duration(s)
}

fn parse_duration(duration_str: &str) -> anyhow::Result<std::time::Duration> {
    let duration_str = duration_str.trim();

    if let Some(num_str) = duration_str.strip_suffix('s') {
        let seconds: f64 = num_str.parse()?;
        Ok(std::time::Duration::from_secs_f64(seconds))
    } else if let Some(num_str) = duration_str.strip_suffix('m') {
        let minutes: f64 = num_str.parse()?;
        Ok(std::time::Duration::from_secs_f64(minutes * 60.0))
    } else if let Some(num_str) = duration_str.strip_suffix('h') {
        let hours: f64 = num_str.parse()?;
        Ok(std::time::Duration::from_secs_f64(hours * 3600.0))
    } else {
        anyhow::bail!(
            "Invalid duration format: {}. Use format like '30s', '5m', '2h'",
            duration_str
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    // -------------------------------------------------------------------------
    // parse_duration edge cases
    // -------------------------------------------------------------------------

    #[test]
    fn test_parse_duration_seconds() {
        let d = parse_duration_pub("30s").unwrap();
        assert_eq!(d.as_secs(), 30);
    }

    #[test]
    fn test_parse_duration_minutes() {
        let d = parse_duration_pub("5m").unwrap();
        assert_eq!(d.as_secs(), 300);
    }

    #[test]
    fn test_parse_duration_hours() {
        let d = parse_duration_pub("2h").unwrap();
        assert_eq!(d.as_secs(), 7200);
    }

    #[test]
    fn test_parse_duration_zero_seconds() {
        let d = parse_duration_pub("0s").unwrap();
        assert_eq!(d.as_secs(), 0);
    }

    #[test]
    fn test_parse_duration_missing_unit_returns_err() {
        // A bare number with no unit suffix should fail.
        let result = parse_duration_pub("30");
        assert!(result.is_err(), "bare number without unit should fail");
    }

    #[test]
    fn test_parse_duration_unknown_suffix_returns_err() {
        // "ms" is not a supported unit in parse_duration (only s/m/h).
        // Note: parse_threshold_to_ms in collector.rs handles "ms" separately.
        let result = parse_duration_pub("30ms");
        // "30ms" strips 's' giving num_str "30m" which parses as f64 "30m" — that
        // actually fails to parse as f64, so it returns Err.
        assert!(
            result.is_err(),
            "\"30ms\" should not parse as a valid duration"
        );
    }

    #[test]
    fn test_parse_duration_empty_string_returns_err() {
        let result = parse_duration_pub("");
        assert!(result.is_err());
    }

    // -------------------------------------------------------------------------
    // Config::load_from_file — partial / truncated TOML
    // -------------------------------------------------------------------------

    #[test]
    fn test_load_from_file_truncated_toml_returns_err() {
        let mut temp_file = NamedTempFile::with_suffix(".toml").unwrap();
        // Write a truncated / syntactically broken TOML
        temp_file
            .write_all(b"[server\nbind_address = \"0.0.0")
            .unwrap();
        temp_file.flush().unwrap();

        let result = Config::load_from_file(&temp_file.path().to_path_buf());
        assert!(result.is_err(), "truncated TOML should return an error");
    }

    #[test]
    fn test_load_from_file_missing_required_field_returns_err() {
        let mut temp_file = NamedTempFile::with_suffix(".toml").unwrap();
        // `log_paths` under `[log_parsing]` is required (no default)
        let content = r#"
[server]
bind_address = "0.0.0.0:9090"
metrics_path = "/metrics"

[log_parsing]
# log_paths intentionally omitted

[metrics]
namespace = "test"

[state]
database_path = "/tmp/state.db"
"#;
        temp_file.write_all(content.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        let result = Config::load_from_file(&temp_file.path().to_path_buf());
        assert!(
            result.is_err(),
            "missing log_paths should fail to deserialise"
        );
    }

    // -------------------------------------------------------------------------
    // Config::poll_interval_duration round-trips
    // -------------------------------------------------------------------------

    #[test]
    fn test_poll_interval_duration_fractional_seconds() {
        let config = Config {
            log_parsing: LogParsingConfig {
                log_paths: vec![],
                poll_interval: "1.5s".to_string(),
                batch_size: 1000,
                max_file_size_mb: 0,
                max_queries_per_file: 0,
            },
            ..Config::default()
        };
        let d = config.poll_interval_duration().unwrap();
        assert_eq!(d.as_millis(), 1500);
    }
}
