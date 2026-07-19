use crate::config::Config;
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::watch;
use tracing::{error, info, warn};

/// Manages configuration reloading via SIGHUP signal
///
/// This is the Unix standard way to reload configuration:
/// - Operator validates config file first
/// - Sends SIGHUP when ready: `kill -HUP <pid>` or `systemctl reload pg-plansight-exporter`
/// - Avoids issues with partial writes, editor behavior, and invalid configs
pub struct ConfigReloader {
    config_path: PathBuf,
    config_tx: watch::Sender<Arc<Config>>,
}

impl ConfigReloader {
    /// Create a new config reloader
    pub fn new(
        config_path: PathBuf,
        initial_config: Config,
    ) -> (Self, watch::Receiver<Arc<Config>>) {
        let (config_tx, config_rx) = watch::channel(Arc::new(initial_config));

        (
            Self {
                config_path,
                config_tx,
            },
            config_rx,
        )
    }

    /// Start listening for SIGHUP signals to reload configuration
    ///
    /// This runs in a background task and reloads config when SIGHUP is received.
    /// Usage:
    ///   kill -HUP <pid>
    ///   systemctl reload pg-plansight-exporter
    pub async fn run(self) -> Result<()> {
        let mut sighup =
            signal(SignalKind::hangup()).context("Failed to register SIGHUP handler")?;

        info!(
            "Config reloader started. Send SIGHUP to reload config from: {}",
            self.config_path.display()
        );

        loop {
            sighup.recv().await;
            info!("SIGHUP received, reloading configuration...");

            match self.try_reload() {
                Ok(()) => {
                    info!("Configuration reloaded successfully");
                }
                Err(e) => {
                    error!("Failed to reload configuration: {}", e);
                    error!("Keeping previous configuration");
                }
            }
        }
    }

    /// Manually trigger a config reload (useful for testing and manual operations)
    pub fn try_reload(&self) -> Result<()> {
        // Validate config file exists
        if !self.config_path.exists() {
            anyhow::bail!(
                "Configuration file not found: {}",
                self.config_path.display()
            );
        }

        // Try to load and parse the config
        let new_config = Config::load_from_file(&self.config_path).with_context(|| {
            format!(
                "Failed to parse config file: {}",
                self.config_path.display()
            )
        })?;

        // Validate the config (e.g., check poll_interval is valid)
        new_config
            .poll_interval_duration()
            .context("Invalid poll_interval in config")?;

        // Validate the filter regexes now: the scheduler applies the new
        // config asynchronously, and a bad pattern there would reject the
        // whole update after "reloaded successfully" was already logged.
        if let Some(ref filters) = new_config.filters
            && let Some(ref patterns) = filters.exclude_query_patterns
        {
            for pattern in patterns {
                regex::Regex::new(pattern).with_context(|| {
                    format!("Invalid exclude_query_patterns regex: {}", pattern)
                })?;
            }
        }

        // SIGHUP only hot-applies the collector config and poll interval.
        // Changes to sections wired up once at startup silently keep their
        // old values, so call each one out explicitly.
        {
            let current = self.config_tx.borrow();
            let mut needs_restart = Vec::new();
            if current.server.bind_address != new_config.server.bind_address
                || current.server.metrics_path != new_config.server.metrics_path
            {
                needs_restart.push("server.bind_address/metrics_path");
            }
            if current.metrics.namespace != new_config.metrics.namespace {
                needs_restart.push("metrics.namespace");
            }
            if current.metrics.backends != new_config.metrics.backends {
                needs_restart.push("metrics.backends");
            }
            if current.metrics.histogram_buckets != new_config.metrics.histogram_buckets {
                needs_restart.push("metrics.histogram_buckets");
            }
            if current.state.database_path != new_config.state.database_path {
                needs_restart.push("state.database_path");
            }
            for section in needs_restart {
                warn!(
                    section,
                    "Changed setting is not hot-reloadable; restart the daemon to apply it"
                );
            }
        }

        // If we get here, config is valid - send it
        self.config_tx
            .send(Arc::new(new_config))
            .map_err(|_| anyhow::anyhow!("Failed to send config update (channel closed)"))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_manual_reload() {
        // Create a temporary config file
        let mut temp_file = NamedTempFile::new().unwrap();
        let config_content = r#"
[server]
bind_address = "0.0.0.0:9090"
metrics_path = "/metrics"

[log_parsing]
log_paths = ["/var/log/postgresql/*.log"]
poll_interval = "30s"
batch_size = 1000

[metrics]
namespace = "pg_plansight"

[state]
database_path = "/tmp/test_state.db"
"#;
        temp_file.write_all(config_content.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        let config_path = temp_file.path().to_path_buf();
        let initial_config = Config::load_from_file(&config_path).unwrap();

        let (reloader, config_rx) = ConfigReloader::new(config_path.clone(), initial_config);

        // Verify initial config
        assert_eq!(config_rx.borrow().server.bind_address, "0.0.0.0:9090");

        // Update the config file
        let updated_content = r#"
[server]
bind_address = "127.0.0.1:9091"
metrics_path = "/metrics"

[log_parsing]
log_paths = ["/var/log/postgresql/*.log"]
poll_interval = "30s"
batch_size = 1000

[metrics]
namespace = "pg_plansight"

[state]
database_path = "/tmp/test_state.db"
"#;
        std::fs::write(&config_path, updated_content).unwrap();

        // Manually trigger reload
        reloader.try_reload().unwrap();

        // Verify config was updated
        assert_eq!(config_rx.borrow().server.bind_address, "127.0.0.1:9091");
    }

    #[test]
    fn test_try_reload_when_config_file_deleted_returns_err() {
        // Create and then immediately delete the config file.
        let mut temp_file = NamedTempFile::new().unwrap();
        let config_content = r#"
[server]
bind_address = "0.0.0.0:9090"
metrics_path = "/metrics"

[log_parsing]
log_paths = ["/var/log/postgresql/*.log"]
poll_interval = "30s"
batch_size = 1000

[metrics]
namespace = "pg_plansight"

[state]
database_path = "/tmp/test_state.db"
"#;
        temp_file.write_all(config_content.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        let config_path = temp_file.path().to_path_buf();
        let initial_config = Config::load_from_file(&config_path).unwrap();
        let (reloader, _config_rx) = ConfigReloader::new(config_path.clone(), initial_config);

        // Delete the file
        drop(temp_file); // NamedTempFile is deleted when dropped

        let result = reloader.try_reload();
        assert!(
            result.is_err(),
            "try_reload on deleted file should return Err"
        );
    }

    #[test]
    fn test_invalid_config_rejected() {
        // Create a temporary config file
        let mut temp_file = NamedTempFile::new().unwrap();
        let config_content = r#"
[server]
bind_address = "0.0.0.0:9090"
metrics_path = "/metrics"

[log_parsing]
log_paths = ["/var/log/postgresql/*.log"]
poll_interval = "30s"
batch_size = 1000

[metrics]
namespace = "pg_plansight"

[state]
database_path = "/tmp/test_state.db"
"#;
        temp_file.write_all(config_content.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        let config_path = temp_file.path().to_path_buf();
        let initial_config = Config::load_from_file(&config_path).unwrap();

        let (reloader, config_rx) = ConfigReloader::new(config_path.clone(), initial_config);

        // Write invalid config (bad poll_interval)
        let invalid_content = r#"
[server]
bind_address = "0.0.0.0:9090"
metrics_path = "/metrics"

[log_parsing]
log_paths = ["/var/log/postgresql/*.log"]
poll_interval = "invalid"
batch_size = 1000

[metrics]
namespace = "pg_plansight"

[state]
database_path = "/tmp/test_state.db"
"#;
        std::fs::write(&config_path, invalid_content).unwrap();

        // Try to reload - should fail
        let result = reloader.try_reload();
        assert!(result.is_err());

        // Original config should still be in use
        assert_eq!(config_rx.borrow().server.bind_address, "0.0.0.0:9090");
    }
}
