use crate::collector::LogCollector;
use anyhow::Result;
use std::time::Duration;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{error, info};

pub struct Scheduler {
    collector: LogCollector,
    poll_interval: Duration,
}

impl Scheduler {
    pub fn new(collector: LogCollector, poll_interval: Duration) -> Self {
        Self {
            collector,
            poll_interval,
        }
    }

    pub async fn start(&mut self) -> Result<()> {
        info!(
            "Starting log collection scheduler with interval: {:?}",
            self.poll_interval
        );

        let mut interval = interval(self.poll_interval);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        // Do initial collection
        if let Err(e) = self.collector.collect_metrics().await {
            error!("Initial metrics collection failed: {}", e);
        }

        loop {
            interval.tick().await;

            if let Err(e) = self.collector.collect_metrics().await {
                error!("Scheduled metrics collection failed: {}", e);
            }
        }
    }
}
