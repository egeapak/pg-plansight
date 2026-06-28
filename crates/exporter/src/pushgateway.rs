use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use hashbrown::HashMap;
use pg_plansight_core::ProcessedQuery;
use prometheus::{CounterVec, Encoder, HistogramVec, Registry, TextEncoder};
use std::collections::HashMap as StdHashMap;
use std::time::Duration;
use tracing::{debug, info, warn};

use crate::config::PushgatewayConfig;

pub struct PushgatewayClient {
    config: PushgatewayConfig,
    client: reqwest::Client,
}

impl PushgatewayClient {
    pub fn new(config: PushgatewayConfig) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_seconds))
            .build()
            .context("Failed to create HTTP client for pushgateway")?;

        Ok(Self { config, client })
    }

    pub async fn push_historical_data(
        &self,
        queries: &HashMap<u64, ProcessedQuery>,
        last_processed_timestamp: DateTime<Utc>,
    ) -> Result<()> {
        if !self.config.enabled || !self.config.push_historical_data {
            debug!("Pushgateway historical data push disabled");
            return Ok(());
        }

        info!(
            "Pushing historical data to pushgateway: {} unique queries",
            queries.len()
        );

        // Group queries by day for batching
        let mut daily_batches: StdHashMap<String, Vec<&ProcessedQuery>> = StdHashMap::new();
        for query in queries.values() {
            // Only push historical data (older than last processed timestamp)
            if query.statistics.max_timestamp < last_processed_timestamp {
                let day = query
                    .statistics
                    .min_timestamp
                    .format("%Y-%m-%d")
                    .to_string();
                daily_batches.entry(day).or_default().push(query);
            }
        }

        for (day, day_queries) in daily_batches {
            if let Err(e) = self.push_daily_batch(&day, &day_queries).await {
                warn!("Failed to push historical data for {}: {}", day, e);
            } else {
                info!(
                    "Successfully pushed {} queries for day {}",
                    day_queries.len(),
                    day
                );
            }
        }

        Ok(())
    }

    async fn push_daily_batch(&self, day: &str, queries: &[&ProcessedQuery]) -> Result<()> {
        let registry = Registry::new();

        // Create historical metrics with day labels
        let historical_executions = CounterVec::new(
            prometheus::Opts::new(
                "pg_plansight_historical_query_executions_total",
                "Historical query executions aggregated by day",
            ),
            &["normalized_query_hash", "database", "day"],
        )?;

        let historical_duration = HistogramVec::new(
            prometheus::HistogramOpts::new(
                "pg_plansight_historical_query_duration_seconds",
                "Historical query duration aggregated by day",
            )
            .buckets(vec![0.001, 0.01, 0.1, 1.0, 5.0, 10.0, 30.0, 60.0, 300.0]),
            &["normalized_query_hash", "database", "day"],
        )?;

        let historical_slow_queries = CounterVec::new(
            prometheus::Opts::new(
                "pg_plansight_historical_slow_queries_total",
                "Historical slow queries aggregated by day",
            ),
            &["database", "day", "threshold"],
        )?;

        registry.register(Box::new(historical_executions.clone()))?;
        registry.register(Box::new(historical_duration.clone()))?;
        registry.register(Box::new(historical_slow_queries.clone()))?;

        // Aggregate data for this day
        for query in queries {
            let query_hash = pg_plansight_core::sql_analysis::calculate_query_fingerprint(
                &query.representative_plan.normalized_query,
            )
            .unwrap_or_else(|_| "unknown".to_string());
            let database = "unknown"; // TODO: Extract from query

            // Count executions for this day
            historical_executions
                .with_label_values(&[query_hash.as_str(), database, day])
                .inc_by(query.statistics.count as f64);

            // Record durations
            for execution in &query.statistics.executions {
                historical_duration
                    .with_label_values(&[query_hash.as_str(), database, day])
                    .observe(execution.duration_ms / 1000.0);
            }

            // Count slow queries
            let slow_thresholds = vec!["1s", "5s", "10s", "30s"];
            for threshold_str in &slow_thresholds {
                let threshold_ms = self.parse_threshold_to_ms(threshold_str)?;
                let slow_count = query
                    .statistics
                    .executions
                    .iter()
                    .filter(|e| e.duration_ms >= threshold_ms)
                    .count();

                if slow_count > 0 {
                    historical_slow_queries
                        .with_label_values(&[database, day, threshold_str])
                        .inc_by(slow_count as f64);
                }
            }
        }

        // Encode metrics to Prometheus format
        let encoder = TextEncoder::new();
        let metric_families = registry.gather();
        let mut buffer = Vec::new();
        encoder.encode(&metric_families, &mut buffer)?;
        let payload = String::from_utf8(buffer)?;

        // Push to gateway using HTTP POST
        let gateway_url = format!(
            "{}/metrics/job/{}/day/{}",
            self.config.url.trim_end_matches('/'),
            urlencoding::encode(&format!("{}_historical", self.config.job_name)),
            urlencoding::encode(day)
        );

        let mut request = self
            .client
            .post(&gateway_url)
            .header("Content-Type", "text/plain; version=0.0.4")
            .body(payload);

        if let Some(ref auth) = self.config.basic_auth {
            request = request.basic_auth(&auth.username, Some(&auth.password));
        }

        let response = request
            .send()
            .await
            .with_context(|| format!("Failed to send request to pushgateway: {}", gateway_url))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            anyhow::bail!(
                "Pushgateway request failed with status {}: {}",
                status,
                body
            );
        }

        Ok(())
    }

    pub async fn cleanup_old_historical_data(&self, cutoff_date: &str) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        info!("Cleaning up historical data older than {}", cutoff_date);

        let gateway_url = format!(
            "{}/metrics/job/{}/day/{}",
            self.config.url.trim_end_matches('/'),
            urlencoding::encode(&format!("{}_historical", self.config.job_name)),
            urlencoding::encode(cutoff_date)
        );

        let mut request = self.client.delete(&gateway_url);

        if let Some(ref auth) = self.config.basic_auth {
            request = request.basic_auth(&auth.username, Some(&auth.password));
        }

        let response = request
            .send()
            .await
            .with_context(|| format!("Failed to delete from pushgateway: {}", gateway_url))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            anyhow::bail!(
                "Pushgateway delete request failed with status {}: {}",
                status,
                body
            );
        }

        Ok(())
    }

    fn parse_threshold_to_ms(&self, threshold: &str) -> Result<f64> {
        // Check for "ms" first to avoid "500ms" matching the "s" suffix
        if let Some(ms) = threshold.strip_suffix("ms") {
            Ok(ms.parse::<f64>()?)
        } else if let Some(s) = threshold.strip_suffix('s') {
            Ok(s.parse::<f64>()? * 1000.0)
        } else {
            anyhow::bail!("Invalid threshold format: {}", threshold);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PushgatewayConfig;

    #[test]
    fn test_threshold_parsing() {
        let config = PushgatewayConfig {
            enabled: true,
            url: "http://localhost:9091".to_string(),
            job_name: "test".to_string(),
            push_historical_data: true,
            historical_batch_size: 1000,
            timeout_seconds: 30,
            basic_auth: None,
        };

        let client = PushgatewayClient::new(config).unwrap();

        assert_eq!(client.parse_threshold_to_ms("1s").unwrap(), 1000.0);
        assert_eq!(client.parse_threshold_to_ms("500ms").unwrap(), 500.0);
        assert!(client.parse_threshold_to_ms("invalid").is_err());
    }
}
