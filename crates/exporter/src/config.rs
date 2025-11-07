use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub server: ServerConfig,
    pub log_parsing: LogParsingConfig,
    pub metrics: MetricsConfig,
    pub state: StateConfig,
    pub filters: Option<FiltersConfig>,
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
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MetricsConfig {
    #[serde(default = "default_namespace")]
    pub namespace: String,
    #[serde(default = "default_backend")]
    pub backend: String,
    #[serde(default)]
    pub opentelemetry: Option<OpenTelemetryConfig>,
    #[serde(default = "default_histogram_buckets")]
    pub histogram_buckets: Vec<f64>,
    #[serde(default = "default_slow_query_thresholds")]
    pub slow_query_thresholds: Vec<String>,
    #[serde(default = "default_retain_days")]
    pub retain_days: u32,
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
            },
            metrics: MetricsConfig {
                namespace: default_namespace(),
                backend: default_backend(),
                opentelemetry: None,
                histogram_buckets: default_histogram_buckets(),
                slow_query_thresholds: default_slow_query_thresholds(),
                retain_days: default_retain_days(),
            },
            state: StateConfig {
                database_path: default_database_path(),
            },
            filters: None,
        }
    }
}

fn default_bind_address() -> String {
    "0.0.0.0:9090".to_string()
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

fn default_namespace() -> String {
    "pg_loganalyze".to_string()
}

fn default_backend() -> String {
    "prometheus".to_string()
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

fn default_database_path() -> String {
    "/var/lib/pg-loganalyze-exporter/state.db".to_string()
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
