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
    #[serde(default = "default_histogram_buckets")]
    pub histogram_buckets: Vec<f64>,
    #[serde(default = "default_slow_query_thresholds")]
    pub slow_query_thresholds: Vec<String>,
    #[serde(default = "default_retain_days")]
    pub retain_days: u32,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use tempfile::NamedTempFile;

    #[test]
    fn test_parse_duration_seconds() {
        assert_eq!(
            parse_duration("30s").unwrap(),
            std::time::Duration::from_secs(30)
        );
        assert_eq!(
            parse_duration("1.5s").unwrap(),
            std::time::Duration::from_secs_f64(1.5)
        );
    }

    #[test]
    fn test_parse_duration_minutes() {
        assert_eq!(
            parse_duration("5m").unwrap(),
            std::time::Duration::from_secs(300)
        );
        assert_eq!(
            parse_duration("2.5m").unwrap(),
            std::time::Duration::from_secs_f64(150.0)
        );
    }

    #[test]
    fn test_parse_duration_hours() {
        assert_eq!(
            parse_duration("2h").unwrap(),
            std::time::Duration::from_secs(7200)
        );
        assert_eq!(
            parse_duration("0.5h").unwrap(),
            std::time::Duration::from_secs_f64(1800.0)
        );
    }

    #[test]
    fn test_parse_duration_invalid() {
        assert!(parse_duration("30").is_err());
        assert!(parse_duration("30x").is_err());
        assert!(parse_duration("invalid").is_err());
        assert!(parse_duration("").is_err());
    }

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.server.bind_address, "0.0.0.0:9090");
        assert_eq!(config.server.metrics_path, "/metrics");
        assert_eq!(config.log_parsing.poll_interval, "30s");
        assert_eq!(config.log_parsing.batch_size, 1000);
        assert_eq!(config.metrics.namespace, "pg_loganalyze");
        assert_eq!(config.metrics.retain_days, 7);
    }

    #[test]
    fn test_poll_interval_duration() {
        let mut config = Config::default();
        config.log_parsing.poll_interval = "45s".to_string();
        assert_eq!(
            config.poll_interval_duration().unwrap(),
            std::time::Duration::from_secs(45)
        );

        config.log_parsing.poll_interval = "2m".to_string();
        assert_eq!(
            config.poll_interval_duration().unwrap(),
            std::time::Duration::from_secs(120)
        );
    }

    #[test]
    fn test_load_minimal_config() {
        let toml_content = r#"
[server]
bind_address = "127.0.0.1:8080"

[log_parsing]
log_paths = ["/var/log/test.log"]

[metrics]
namespace = "test"

[state]
database_path = "/tmp/test.db"
"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(toml_content.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        let config = Config::load_from_file(&temp_file.path().to_path_buf()).unwrap();
        assert_eq!(config.server.bind_address, "127.0.0.1:8080");
        assert_eq!(config.log_parsing.log_paths, vec!["/var/log/test.log"]);
        assert_eq!(config.metrics.namespace, "test");
        assert_eq!(config.state.database_path, "/tmp/test.db");
    }

    #[test]
    fn test_load_config_with_filters() {
        let toml_content = r#"
[server]
bind_address = "0.0.0.0:9090"

[log_parsing]
log_paths = ["/var/log/*.log"]

[metrics]
namespace = "pg"

[state]
database_path = "/var/lib/state.db"

[filters]
include_databases = ["prod", "staging"]
exclude_query_patterns = ["^BEGIN$", "^COMMIT$"]
min_duration_ms = 100.0
"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(toml_content.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        let config = Config::load_from_file(&temp_file.path().to_path_buf()).unwrap();
        assert!(config.filters.is_some());
        let filters = config.filters.unwrap();
        assert_eq!(
            filters.include_databases,
            Some(vec!["prod".to_string(), "staging".to_string()])
        );
        assert_eq!(
            filters.exclude_query_patterns,
            Some(vec!["^BEGIN$".to_string(), "^COMMIT$".to_string()])
        );
        assert_eq!(filters.min_duration_ms, Some(100.0));
    }

    #[test]
    fn test_load_config_with_custom_buckets() {
        let toml_content = r#"
[server]
bind_address = "0.0.0.0:9090"

[log_parsing]
log_paths = ["/var/log/test.log"]

[metrics]
namespace = "pg"
histogram_buckets = [0.1, 1.0, 10.0, 100.0]
slow_query_thresholds = ["500ms", "2s", "10s"]

[state]
database_path = "/tmp/state.db"
"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(toml_content.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        let config = Config::load_from_file(&temp_file.path().to_path_buf()).unwrap();
        assert_eq!(
            config.metrics.histogram_buckets,
            vec![0.1, 1.0, 10.0, 100.0]
        );
        assert_eq!(
            config.metrics.slow_query_thresholds,
            vec!["500ms".to_string(), "2s".to_string(), "10s".to_string()]
        );
    }

    #[test]
    fn test_load_config_invalid_toml() {
        let toml_content = r#"
[server
bind_address = "invalid toml"
"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(toml_content.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        let result = Config::load_from_file(&temp_file.path().to_path_buf());
        assert!(result.is_err());
    }

    #[test]
    fn test_load_config_nonexistent_file() {
        let result = Config::load_from_file(&PathBuf::from("/nonexistent/config.toml"));
        assert!(result.is_err());
    }

    #[test]
    fn test_config_defaults_applied() {
        let toml_content = r#"
[server]

[log_parsing]
log_paths = ["/var/log/test.log"]

[metrics]

[state]
"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(toml_content.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        let config = Config::load_from_file(&temp_file.path().to_path_buf()).unwrap();

        // Check that defaults are applied
        assert_eq!(config.server.bind_address, "0.0.0.0:9090");
        assert_eq!(config.server.metrics_path, "/metrics");
        assert_eq!(config.log_parsing.poll_interval, "30s");
        assert_eq!(config.log_parsing.batch_size, 1000);
        assert_eq!(config.metrics.namespace, "pg_loganalyze");
        assert_eq!(config.metrics.retain_days, 7);
        assert_eq!(
            config.state.database_path,
            "/var/lib/pg-loganalyze-exporter/state.db"
        );
    }
}
