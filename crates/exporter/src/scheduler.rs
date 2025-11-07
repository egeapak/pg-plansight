use crate::collector::LogCollector;
use crate::config::Config;
use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::{self, MissedTickBehavior};
use tracing::{error, info};

pub struct Scheduler {
    collector: LogCollector,
    poll_interval: Duration,
    config_rx: Option<watch::Receiver<Arc<Config>>>,
}

impl Scheduler {
    pub fn new(collector: LogCollector, poll_interval: Duration) -> Self {
        Self {
            collector,
            poll_interval,
            config_rx: None,
        }
    }

    /// Create a scheduler with hot reload support
    pub fn with_hot_reload(
        collector: LogCollector,
        poll_interval: Duration,
        config_rx: watch::Receiver<Arc<Config>>,
    ) -> Self {
        Self {
            collector,
            poll_interval,
            config_rx: Some(config_rx),
        }
    }

    pub async fn start(&mut self) -> Result<()> {
        info!(
            "Starting log collection scheduler with interval: {:?}",
            self.poll_interval
        );

        let mut interval = time::interval(self.poll_interval);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        // Do initial collection
        if let Err(e) = self.collector.collect_metrics().await {
            error!("Initial metrics collection failed: {}", e);
        }

        loop {
            tokio::select! {
                // Check for config updates
                _ = async {
                    if let Some(ref mut rx) = self.config_rx {
                        rx.changed().await
                    } else {
                        std::future::pending().await
                    }
                } => {
                    if let Some(ref rx) = self.config_rx {
                        let new_config = rx.borrow().clone();
                        info!("Configuration updated, applying changes...");

                        // Update collector config
                        if let Err(e) = self.collector.update_config((*new_config).clone()) {
                            error!("Failed to update collector config: {}", e);
                        } else {
                            // Update poll interval
                            if let Ok(new_interval) = new_config.poll_interval_duration() {
                                if new_interval != self.poll_interval {
                                    info!("Poll interval changed from {:?} to {:?}", self.poll_interval, new_interval);
                                    self.poll_interval = new_interval;
                                    interval = time::interval(self.poll_interval);
                                    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
                                }
                            }
                        }
                    }
                }

                // Scheduled collection
                _ = interval.tick() => {
                    if let Err(e) = self.collector.collect_metrics().await {
                        error!("Scheduled metrics collection failed: {}", e);
                    }
                }
            }
        }
    }
}
