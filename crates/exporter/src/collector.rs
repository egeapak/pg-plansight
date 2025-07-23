use crate::config::Config;
use crate::metrics::MetricsRegistry;
use crate::state::{FileState, StateManager};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use pg_loganalyze_core::{PostgreSQLLogParser, ProcessedQuery, QueryPlan, calculate_query_hash};
use regex::Regex;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{debug, error, info, warn};

pub struct LogCollector {
    config: Config,
    state_manager: StateManager,
    metrics: Arc<MetricsRegistry>,
    log_parser: PostgreSQLLogParser,
    filter_patterns: Option<Vec<Regex>>,
}

impl LogCollector {
    pub fn new(
        config: Config,
        state_manager: StateManager,
        metrics: Arc<MetricsRegistry>,
    ) -> Result<Self> {
        let mut log_parser = PostgreSQLLogParser::new();

        // Compile filter patterns if provided
        let filter_patterns = if let Some(ref filters) = config.filters {
            if let Some(ref patterns) = filters.exclude_query_patterns {
                let compiled_patterns: Result<Vec<_>> = patterns
                    .iter()
                    .map(|pattern| {
                        Regex::new(pattern)
                            .with_context(|| format!("Invalid regex pattern: {}", pattern))
                    })
                    .collect();
                Some(compiled_patterns?)
            } else {
                None
            }
        } else {
            None
        };

        Ok(Self {
            config,
            state_manager,
            metrics,
            log_parser,
            filter_patterns,
        })
    }

    pub async fn collect_metrics(&mut self) -> Result<()> {
        let _timer = self
            .metrics
            .export_duration
            .with_label_values(&["collect"])
            .start_timer();

        let log_paths = self.expand_log_paths()?;
        info!("Processing {} log files", log_paths.len());

        let mut total_processed = 0;
        let mut total_errors = 0;

        for log_path in log_paths {
            match self.process_log_file(&log_path).await {
                Ok(processed) => {
                    total_processed += processed;
                    self.metrics
                        .logs_parsed_total
                        .with_label_values(&[&log_path.to_string_lossy(), "success"])
                        .inc_by(processed as f64);
                }
                Err(e) => {
                    total_errors += 1;
                    error!("Failed to process log file {}: {}", log_path.display(), e);
                    self.metrics
                        .parse_errors_total
                        .with_label_values(&[&log_path.to_string_lossy(), "file_error"])
                        .inc();
                }
            }
        }

        info!(
            "Collection complete: {} entries processed, {} files had errors",
            total_processed, total_errors
        );

        if total_errors == 0 {
            self.metrics.record_successful_parse();
        }

        self.metrics.update_memory_usage();
        Ok(())
    }

    pub async fn collect_remaining_metrics(&mut self) -> Result<()> {
        let _timer = self
            .metrics
            .export_duration
            .with_label_values(&["collect_remaining"])
            .start_timer();

        let log_paths = self.expand_log_paths()?;
        info!(
            "Processing remaining content from {} log files",
            log_paths.len()
        );

        let mut total_processed = 0;
        let mut total_errors = 0;
        let mut files_with_remaining = 0;

        for log_path in log_paths {
            match self.process_remaining_content(&log_path).await {
                Ok(processed) => {
                    if processed > 0 {
                        files_with_remaining += 1;
                        total_processed += processed;
                        self.metrics
                            .logs_parsed_total
                            .with_label_values(&[&log_path.to_string_lossy(), "success"])
                            .inc_by(processed as f64);
                        info!(
                            "Processed {} remaining lines from {}",
                            processed,
                            log_path.display()
                        );
                    } else {
                        debug!("No remaining content in {}", log_path.display());
                    }
                }
                Err(e) => {
                    total_errors += 1;
                    error!(
                        "Failed to process remaining content from {}: {}",
                        log_path.display(),
                        e
                    );
                    self.metrics
                        .parse_errors_total
                        .with_label_values(&[&log_path.to_string_lossy(), "file_error"])
                        .inc();
                }
            }
        }

        info!(
            "Remaining content processing complete: {} entries from {} files, {} files had errors",
            total_processed, files_with_remaining, total_errors
        );

        if total_errors == 0 {
            self.metrics.record_successful_parse();
        }

        self.metrics.update_memory_usage();
        Ok(())
    }

    async fn process_log_file(&mut self, log_path: &Path) -> Result<usize> {
        let metadata = std::fs::metadata(log_path)?;
        let current_mtime = metadata
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs() as i64;
        let current_size = metadata.len();

        // Get previous state for this file
        let mut file_state = self
            .state_manager
            .get_file_state(log_path)?
            .unwrap_or_else(|| FileState {
                file_path: log_path.to_path_buf(),
                last_position: 0,
                last_modified_time: 0,
                file_size: 0,
                last_processed_at: Utc::now(),
            });

        // Check if file needs processing
        if current_mtime <= file_state.last_modified_time && current_size <= file_state.file_size {
            debug!("File {} unchanged, skipping", log_path.display());
            return Ok(0);
        }

        // Handle file truncation (log rotation)
        if current_size < file_state.file_size {
            info!(
                "File {} appears to have been truncated/rotated, processing from beginning",
                log_path.display()
            );
            file_state.last_position = 0;
        }

        let new_content_size = current_size - file_state.file_size;
        let lines_processed = if new_content_size > 0 {
            // Use file range parsing to process only the new content
            let end_pos = Some(current_size);
            match self.log_parser.parse_file_range_with_progress(
                log_path,
                file_state.last_position,
                end_pos,
                |_, _| {},
            ) {
                Ok(query_plans) => {
                    if !query_plans.is_empty() {
                        self.process_query_plans(&query_plans).await?;
                        info!(
                            "Processed {} query plans from {} bytes of new content in {}",
                            query_plans.len(),
                            new_content_size,
                            log_path.display()
                        );
                        query_plans.len() // Return number of query plans processed
                    } else {
                        0
                    }
                }
                Err(e) => {
                    warn!(
                        "Failed to process new content from {}: {}",
                        log_path.display(),
                        e
                    );
                    self.metrics
                        .parse_errors_total
                        .with_label_values(&[&log_path.to_string_lossy(), "parse_error"])
                        .inc();
                    0
                }
            }
        } else {
            0 // No new content to process
        };

        // Update file state
        file_state.last_position = current_size; // Now points to end of file
        file_state.last_modified_time = current_mtime;
        file_state.file_size = current_size;
        file_state.last_processed_at = Utc::now();

        self.state_manager.update_file_state(&file_state)?;

        Ok(lines_processed)
    }

    async fn process_remaining_content(&mut self, log_path: &Path) -> Result<usize> {
        let metadata = std::fs::metadata(log_path)?;
        let current_size = metadata.len();

        // Get previous state for this file
        let file_state = self.state_manager.get_file_state(log_path)?;

        let (start_position, should_process) = match file_state {
            Some(state) => {
                // Check if there's remaining content
                if current_size <= state.file_size {
                    debug!(
                        "No remaining content in {} (current: {}, last: {})",
                        log_path.display(),
                        current_size,
                        state.file_size
                    );
                    return Ok(0);
                }
                info!(
                    "Found remaining content in {}: {} bytes (from position {} to {})",
                    log_path.display(),
                    current_size - state.file_size,
                    state.file_size,
                    current_size
                );
                (state.last_position, true)
            }
            None => {
                info!(
                    "No previous state for {}, processing entire file",
                    log_path.display()
                );
                (0, true)
            }
        };

        if !should_process {
            return Ok(0);
        }

        let mut file = File::open(log_path)?;
        file.seek(SeekFrom::Start(start_position))?;

        let reader = BufReader::new(file);
        let mut lines_processed = 0;
        let mut current_position = start_position;
        let mut log_lines = Vec::new();

        // Collect all remaining lines
        for line_result in reader.lines() {
            match line_result {
                Ok(line) => {
                    current_position += line.len() as u64 + 1; // +1 for newline
                    lines_processed += 1;
                    log_lines.push(line);
                }
                Err(e) => {
                    warn!("Error reading line from {}: {}", log_path.display(), e);
                    self.metrics
                        .parse_errors_total
                        .with_label_values(&[&log_path.to_string_lossy(), "line_read_error"])
                        .inc();
                }
            }
        }

        // Process using file range parsing if we have content to process
        if lines_processed > 0 {
            let end_pos = Some(current_position);
            match self.log_parser.parse_file_range_with_progress(
                log_path,
                start_position,
                end_pos,
                |_, _| {},
            ) {
                Ok(query_plans) => {
                    if !query_plans.is_empty() {
                        self.process_query_plans(&query_plans).await?;
                        info!(
                            "Successfully processed {} query plans from remaining content",
                            query_plans.len()
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        "Failed to process remaining content from {}: {}",
                        log_path.display(),
                        e
                    );
                    self.metrics
                        .parse_errors_total
                        .with_label_values(&[&log_path.to_string_lossy(), "parse_error"])
                        .inc();
                }
            }
        }

        // Update file state to mark this content as processed
        let current_mtime = metadata
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs() as i64;
        let new_state = FileState {
            file_path: log_path.to_path_buf(),
            last_position: current_position,
            last_modified_time: current_mtime,
            file_size: current_size,
            last_processed_at: Utc::now(),
        };

        self.state_manager.update_file_state(&new_state)?;

        Ok(lines_processed)
    }

    async fn process_query_plans(&mut self, query_plans: &[QueryPlan]) -> Result<()> {
        let processed_queries = self.log_parser.get_processed_queries(query_plans);

        for (_query_hash, query) in processed_queries.iter() {
            if !self.should_include_query(query)? {
                continue;
            }

            let query_hash = calculate_query_hash(&query.normalized_query);
            let stable_hash = format!("{:016x}", query_hash);
            let query_timestamp = self.format_timestamp_for_labels(query.statistics.min_timestamp);
            let database = self.extract_database_name(&query.original_query);

            // Record the query hash for future reference
            self.state_manager
                .record_query_hash(&stable_hash, &query.normalized_query)?;

            // Update metrics
            self.update_query_metrics(&stable_hash, &query_timestamp, &database, query)
                .await?;
        }

        Ok(())
    }

    async fn update_query_metrics(
        &self,
        query_hash: &str,
        query_timestamp: &str,
        database: &str,
        query: &ProcessedQuery,
    ) -> Result<()> {
        let labels = &[query_hash, database, query_timestamp];

        // Query performance metrics
        for execution in &query.statistics.executions {
            let duration_secs = execution.duration_ms / 1000.0;
            self.metrics
                .query_duration
                .with_label_values(labels)
                .observe(duration_secs);

            self.metrics
                .query_executions
                .with_label_values(&[query_hash, database, query_timestamp, "success"])
                .inc();
        }

        // Slow query tracking
        for threshold_str in &self.config.metrics.slow_query_thresholds {
            let threshold_ms = self.parse_threshold_to_ms(threshold_str)?;
            let slow_count = query
                .statistics
                .executions
                .iter()
                .filter(|e| e.duration_ms >= threshold_ms)
                .count();

            if slow_count > 0 {
                self.metrics
                    .slow_queries
                    .with_label_values(&[database, query_timestamp, threshold_str])
                    .inc_by(slow_count as f64);
            }
        }

        // Plan analysis metrics if available
        if let Some(ref parsed_plan) = query.parsed_plan {
            // Extract plan cost if available
            if let Some(cost) = self.extract_plan_cost(&query.plan) {
                self.metrics
                    .query_plan_cost
                    .with_label_values(labels)
                    .observe(cost);
            }

            // Count plan node types
            self.update_plan_metrics(database, query_timestamp, &query.plan)
                .await?;
        }

        Ok(())
    }

    async fn update_plan_metrics(&self, database: &str, timestamp: &str, plan: &str) -> Result<()> {
        // Simple plan analysis - in a real implementation you'd want more sophisticated parsing
        let plan_lower = plan.to_lowercase();

        // Count scan types
        if plan_lower.contains("seq scan") {
            self.metrics
                .scan_types
                .with_label_values(&["seq_scan", database, timestamp])
                .inc();
        }
        if plan_lower.contains("index scan") {
            self.metrics
                .scan_types
                .with_label_values(&["index_scan", database, timestamp])
                .inc();
        }
        if plan_lower.contains("bitmap heap scan") {
            self.metrics
                .scan_types
                .with_label_values(&["bitmap_heap_scan", database, timestamp])
                .inc();
        }

        // Count join types
        if plan_lower.contains("hash join") {
            self.metrics
                .join_types
                .with_label_values(&["hash_join", database, timestamp])
                .inc();
        }
        if plan_lower.contains("nested loop") {
            self.metrics
                .join_types
                .with_label_values(&["nested_loop", database, timestamp])
                .inc();
        }
        if plan_lower.contains("merge join") {
            self.metrics
                .join_types
                .with_label_values(&["merge_join", database, timestamp])
                .inc();
        }

        Ok(())
    }

    fn should_include_query(&self, query: &ProcessedQuery) -> Result<bool> {
        if let Some(ref filters) = self.config.filters {
            // Check minimum duration
            if let Some(min_duration_ms) = filters.min_duration_ms {
                if query.statistics.min_duration_ms < min_duration_ms {
                    return Ok(false);
                }
            }

            // Check database inclusion
            if let Some(ref include_dbs) = filters.include_databases {
                let db_name = self.extract_database_name(&query.original_query);
                if !include_dbs.contains(&db_name) {
                    return Ok(false);
                }
            }

            // Check query pattern exclusions
            if let Some(ref patterns) = self.filter_patterns {
                for pattern in patterns {
                    if pattern.is_match(&query.normalized_query) {
                        return Ok(false);
                    }
                }
            }
        }

        Ok(true)
    }

    fn expand_log_paths(&self) -> Result<Vec<PathBuf>> {
        let mut paths = Vec::new();

        for pattern in &self.config.log_parsing.log_paths {
            match glob::glob(pattern) {
                Ok(entries) => {
                    for entry in entries {
                        match entry {
                            Ok(path) => {
                                if path.is_file() {
                                    paths.push(path);
                                }
                            }
                            Err(e) => {
                                warn!("Error processing glob entry: {}", e);
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!("Invalid glob pattern '{}': {}", pattern, e);
                }
            }
        }

        Ok(paths)
    }

    fn format_timestamp_for_labels(&self, timestamp: DateTime<Utc>) -> String {
        timestamp.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
    }

    fn extract_database_name(&self, _query: &str) -> String {
        // In a real implementation, you'd extract this from the log context
        // For now, return a default
        "unknown".to_string()
    }

    fn extract_plan_cost(&self, plan: &str) -> Option<f64> {
        // Simple regex to extract cost from plan text
        let cost_regex = Regex::new(r"cost=[\d.]+\.\.(\d+\.?\d*)").ok()?;
        if let Some(captures) = cost_regex.captures(plan) {
            captures.get(1)?.as_str().parse().ok()
        } else {
            None
        }
    }

    fn parse_threshold_to_ms(&self, threshold: &str) -> Result<f64> {
        if let Some(s) = threshold.strip_suffix('s') {
            Ok(s.parse::<f64>()? * 1000.0)
        } else if let Some(ms) = threshold.strip_suffix("ms") {
            Ok(ms.parse::<f64>()?)
        } else {
            anyhow::bail!("Invalid threshold format: {}", threshold);
        }
    }
}
