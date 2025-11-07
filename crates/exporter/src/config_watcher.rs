use crate::config::Config;
use anyhow::{Context, Result};
use notify::{
    event::{AccessKind, AccessMode},
    Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::watch;
use tracing::{error, info};

/// Watches a configuration file and notifies subscribers when it changes
pub struct ConfigWatcher {
    config_path: PathBuf,
    config_tx: watch::Sender<Arc<Config>>,
    _watcher: RecommendedWatcher,
}

impl ConfigWatcher {
    /// Create a new config watcher and start watching the file
    pub fn new(config_path: PathBuf, initial_config: Config) -> Result<(Self, watch::Receiver<Arc<Config>>)> {
        let (config_tx, config_rx) = watch::channel(Arc::new(initial_config));

        let tx_clone = config_tx.clone();
        let path_clone = config_path.clone();

        // Create watcher
        let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            match res {
                Ok(event) => {
                    // Only reload on file close after write (most reliable indicator)
                    let should_reload = matches!(
                        event.kind,
                        EventKind::Access(AccessKind::Close(AccessMode::Write)) | EventKind::Modify(_)
                    );

                    if should_reload && event.paths.iter().any(|p| p == &path_clone) {
                        info!("Configuration file changed, reloading...");

                        match Config::load_from_file(&path_clone) {
                            Ok(new_config) => {
                                if let Err(e) = tx_clone.send(Arc::new(new_config)) {
                                    error!("Failed to send config update: {}", e);
                                } else {
                                    info!("Configuration reloaded successfully");
                                }
                            }
                            Err(e) => {
                                error!("Failed to reload configuration: {}. Keeping previous config.", e);
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Watch error: {}", e);
                }
            }
        })
        .context("Failed to create file watcher")?;

        // Watch the config file
        watcher
            .watch(&config_path, RecursiveMode::NonRecursive)
            .with_context(|| format!("Failed to watch config file: {}", config_path.display()))?;

        info!("Started watching config file: {}", config_path.display());

        Ok((
            Self {
                config_path,
                config_tx,
                _watcher: watcher,
            },
            config_rx,
        ))
    }

    /// Manually trigger a config reload (useful for testing)
    pub fn reload(&self) -> Result<()> {
        info!("Manually reloading configuration...");
        let new_config = Config::load_from_file(&self.config_path)?;
        self.config_tx
            .send(Arc::new(new_config))
            .map_err(|_| anyhow::anyhow!("Failed to send config update"))?;
        info!("Configuration reloaded successfully");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use tempfile::NamedTempFile;
    use tokio::time::{sleep, Duration};

    #[tokio::test]
    async fn test_config_hot_reload() {
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
namespace = "pg_loganalyze"

[state]
database_path = "/tmp/test_state.db"
"#;
        temp_file.write_all(config_content.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        let config_path = temp_file.path().to_path_buf();
        let initial_config = Config::load_from_file(&config_path).unwrap();

        let (_watcher, mut config_rx) = ConfigWatcher::new(config_path.clone(), initial_config).unwrap();

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
namespace = "pg_loganalyze"

[state]
database_path = "/tmp/test_state.db"
"#;
        fs::write(&config_path, updated_content).unwrap();

        // Wait for the file watcher to detect the change and reload
        sleep(Duration::from_millis(500)).await;

        // Wait for config update notification
        let timeout = tokio::time::timeout(Duration::from_secs(2), config_rx.changed()).await;
        assert!(timeout.is_ok(), "Config change notification timed out");

        // Verify config was updated
        assert_eq!(config_rx.borrow().server.bind_address, "127.0.0.1:9091");
    }
}
