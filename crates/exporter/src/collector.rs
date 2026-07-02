use crate::config::Config;
use crate::metrics::MetricsBackend;
use crate::state::{FileState, StateManager};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use pg_plansight_core::{PostgreSQLLogParser, ProcessedQuery};
use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{debug, error, info, warn};

pub struct LogCollector {
    config: Config,
    state_manager: StateManager,
    metrics: Arc<dyn MetricsBackend>,
    log_parser: PostgreSQLLogParser,
    filter_patterns: Option<Vec<Regex>>,
    /// In-memory per-file bookkeeping (quiescence detection, parse-failure
    /// retry caps). Rebuilt from scratch after a daemon restart.
    file_runtime: hashbrown::HashMap<PathBuf, FileRuntime>,
}

/// Per-file runtime state that does not need to survive restarts.
#[derive(Default)]
struct FileRuntime {
    /// `(mtime, size)` observed on the previous cycle.
    last_observed: Option<(i64, u64)>,
    /// Consecutive cycles with no mtime/size change.
    unchanged_cycles: u32,
    /// Consecutive parse failures for the currently-pending range.
    parse_failures: u32,
}

/// Cycles with zero growth before a file is considered quiescent and its
/// held-back tail is flushed to EOF. Two full poll intervals of silence make
/// a mid-write stall at the exact boundary vanishingly unlikely.
const QUIESCENT_CYCLES: u32 = 2;

/// Consecutive parse failures after which the failing range is skipped, so a
/// deterministically-bad range cannot stall a file's export forever.
const MAX_PARSE_FAILURES: u32 = 3;

/// Result of parsing one byte range of a log file.
struct ParsedRange {
    /// Absolute offset up to which content was actually parsed; becomes the
    /// persisted checkpoint.
    end_offset: u64,
    plan_count: usize,
    processed_queries: hashbrown::HashMap<String, ProcessedQuery>,
}

/// Byte offset (absolute) of the START of the last complete, timestamped log
/// line in `[start, end)`, or `None` if the range contains none. Counting
/// raw bytes from `read_until` keeps offsets exact regardless of CRLF line
/// endings or invalid UTF-8.
fn find_entry_boundary(path: &Path, start: u64, end: u64) -> Result<Option<u64>> {
    use std::fs::File;
    use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};

    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    let mut reader = BufReader::with_capacity(64 * 1024, file.take(end - start));

    let mut offset = start;
    let mut last_boundary = None;
    let mut buf = Vec::with_capacity(8 * 1024);
    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 {
            break;
        }
        let complete = buf.last() == Some(&b'\n');
        if complete && is_timestamped_line(&buf) {
            last_boundary = Some(offset);
        }
        offset += n as u64;
        if !complete {
            break;
        }
    }
    Ok(last_boundary)
}

/// Cheap check for a `YYYY-MM-DD HH:MM:SS` line prefix (the shape every
/// %m/%t-prefixed PostgreSQL log line starts with). Delegates to core so the
/// boundary scan and the parser share one definition of "start of a log line".
fn is_timestamped_line(line: &[u8]) -> bool {
    pg_plansight_core::parser_utils::is_log_line_start(line)
}

/// True when the file's magic bytes say gzip or bzip2 (the compressed rotated
/// logs pg-plansight-core supports).
async fn is_compressed_file(path: &Path) -> bool {
    use tokio::io::AsyncReadExt;
    let Ok(mut file) = tokio::fs::File::open(path).await else {
        return false;
    };
    let mut magic = [0u8; 3];
    match file.read_exact(&mut magic).await {
        Ok(_) => magic[..2] == [0x1f, 0x8b] || magic == *b"BZh",
        Err(_) => false,
    }
}

impl LogCollector {
    pub fn new(
        config: Config,
        state_manager: StateManager,
        metrics: Arc<dyn MetricsBackend>,
    ) -> Result<Self> {
        let log_parser = PostgreSQLLogParser::new();

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
            file_runtime: hashbrown::HashMap::new(),
        })
    }

    pub async fn collect_metrics(&mut self) -> Result<()> {
        let start = std::time::Instant::now();

        let log_paths = self.expand_log_paths()?;
        info!("Processing {} log files", log_paths.len());

        let mut total_processed = 0;
        let mut total_errors = 0;

        for log_path in log_paths {
            match self.process_log_file(&log_path).await {
                Ok(processed) => {
                    total_processed += processed;
                    if processed > 0 {
                        let mut labels_map = std::collections::HashMap::new();
                        labels_map.insert("file_path", log_path.to_string_lossy().to_string());
                        labels_map.insert("status", "success".to_string());
                        self.metrics
                            .increment_logs_parsed_by(&labels_map, processed as u64);
                    }
                }
                Err(e) => {
                    total_errors += 1;
                    error!("Failed to process log file {}: {}", log_path.display(), e);
                    let mut labels_map = std::collections::HashMap::new();
                    labels_map.insert("file_path", log_path.to_string_lossy().to_string());
                    labels_map.insert("error_type", "file_error".to_string());
                    self.metrics.increment_parse_errors(&labels_map);
                }
            }
        }

        info!(
            "Collection complete: {} entries processed, {} files had errors",
            total_processed, total_errors
        );

        if total_errors == 0 {
            crate::metrics::record_successful_parse(self.metrics.as_ref());
        }

        crate::metrics::update_memory_usage(self.metrics.as_ref());

        let duration = start.elapsed().as_secs_f64();
        let mut labels_map = std::collections::HashMap::new();
        labels_map.insert("operation", "collect".to_string());
        self.metrics.record_export_duration(&labels_map, duration);

        Ok(())
    }

    pub async fn collect_remaining_metrics(&mut self) -> Result<()> {
        let start = std::time::Instant::now();

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
                        let mut labels_map = std::collections::HashMap::new();
                        labels_map.insert("file_path", log_path.to_string_lossy().to_string());
                        labels_map.insert("status", "success".to_string());
                        self.metrics
                            .increment_logs_parsed_by(&labels_map, processed as u64);
                        info!(
                            "Processed {} remaining entries from {}",
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
                    let mut labels_map = std::collections::HashMap::new();
                    labels_map.insert("file_path", log_path.to_string_lossy().to_string());
                    labels_map.insert("error_type", "file_error".to_string());
                    self.metrics.increment_parse_errors(&labels_map);
                }
            }
        }

        info!(
            "Remaining content processing complete: {} entries from {} files, {} files had errors",
            total_processed, files_with_remaining, total_errors
        );

        if total_errors == 0 {
            crate::metrics::record_successful_parse(self.metrics.as_ref());
        }

        crate::metrics::update_memory_usage(self.metrics.as_ref());

        let duration = start.elapsed().as_secs_f64();
        let mut labels_map = std::collections::HashMap::new();
        labels_map.insert("operation", "collect_remaining".to_string());
        self.metrics.record_export_duration(&labels_map, duration);

        Ok(())
    }

    async fn process_log_file(&mut self, log_path: &Path) -> Result<usize> {
        // Use async file metadata to avoid blocking the runtime
        let metadata = tokio::fs::metadata(log_path).await?;
        let current_mtime = metadata
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs() as i64;
        let current_size = metadata.len();

        // Check file size limit (0 = unlimited)
        let max_size_bytes = self.config.log_parsing.max_file_size_mb * 1024 * 1024;
        if max_size_bytes > 0 && current_size > max_size_bytes {
            warn!(
                file = %log_path.display(),
                size_mb = current_size / (1024 * 1024),
                max_size_mb = self.config.log_parsing.max_file_size_mb,
                "File exceeds maximum size limit, skipping"
            );
            return Ok(0);
        }

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

        // Track quiescence in memory: two cycles with no growth mean the
        // writer has moved on (rotation, idle database), so the held-back
        // tail can safely be flushed to EOF — otherwise the final entry of a
        // rotated file would never be exported.
        let quiescent = {
            let runtime = self.file_runtime.entry(log_path.to_path_buf()).or_default();
            if runtime.last_observed == Some((current_mtime, current_size)) {
                runtime.unchanged_cycles = runtime.unchanged_cycles.saturating_add(1);
            } else {
                runtime.unchanged_cycles = 0;
                runtime.last_observed = Some((current_mtime, current_size));
            }
            runtime.unchanged_cycles >= QUIESCENT_CYCLES
        };

        // Handle file truncation (log rotation). file_size stores the size
        // observed last cycle (not the parsed boundary), keeping the full
        // detection window for copytruncate-style rotation.
        if current_size < file_state.file_size {
            info!(
                "File {} appears to have been truncated/rotated, processing from beginning",
                log_path.display()
            );
            file_state.last_position = 0;
            file_state.file_size = 0;
        }

        if current_size <= file_state.last_position {
            debug!("File {} fully processed, skipping", log_path.display());
            return Ok(0);
        }

        // Compressed logs (rotated *.gz/*.bz2) cannot be read incrementally
        // and contain no plain-text line boundaries to scan; they are
        // complete by definition, so parse the whole stream once.
        let is_compressed = is_compressed_file(log_path).await;
        if is_compressed && file_state.last_position > 0 {
            return Ok(0); // already ingested in full
        }

        // Parse only up to the start of the last timestamped line: PostgreSQL
        // may be mid-write of a multi-line auto_explain entry at the snapshot
        // boundary, and an entry is only known complete once the NEXT
        // timestamped line exists. Everything at/after the boundary is
        // re-examined next cycle, and quiescent files flush to EOF (above).
        let hold_back = !quiescent && !is_compressed;
        let parsed = match self
            .parse_range_blocking(log_path, file_state.last_position, current_size, hold_back)
            .await
        {
            Ok(parsed) => {
                if let Some(runtime) = self.file_runtime.get_mut(log_path) {
                    runtime.parse_failures = 0;
                }
                parsed
            }
            Err(e) => {
                // Do NOT advance the checkpoint on a transient error — that
                // would permanently skip the unread range. But cap retries: a
                // deterministically-failing range must not stall the file's
                // export forever.
                let failures = {
                    let runtime = self.file_runtime.entry(log_path.to_path_buf()).or_default();
                    runtime.parse_failures = runtime.parse_failures.saturating_add(1);
                    runtime.parse_failures
                };
                let mut labels_map = std::collections::HashMap::new();
                labels_map.insert("file_path", log_path.to_string_lossy().to_string());
                labels_map.insert("error_type", "parse_error".to_string());
                self.metrics.increment_parse_errors(&labels_map);

                if failures >= MAX_PARSE_FAILURES {
                    error!(
                        "Giving up on byte range {}..{} of {} after {} consecutive parse \
                         failures; skipping it: {}",
                        file_state.last_position,
                        current_size,
                        log_path.display(),
                        failures,
                        e
                    );
                    file_state.last_position = current_size;
                    file_state.file_size = current_size;
                    file_state.last_modified_time = current_mtime;
                    file_state.last_processed_at = Utc::now();
                    self.state_manager.update_file_state(&file_state)?;
                    if let Some(runtime) = self.file_runtime.get_mut(log_path) {
                        runtime.parse_failures = 0;
                    }
                } else {
                    warn!(
                        "Failed to process new content from {} (attempt {}/{}, will retry): {}",
                        log_path.display(),
                        failures,
                        MAX_PARSE_FAILURES,
                        e
                    );
                }
                return Ok(0);
            }
        };

        let Some(parsed) = parsed else {
            // No complete entry boundary in the new range yet.
            debug!(
                "No complete log entry boundary in {} yet, waiting for more content",
                log_path.display()
            );
            return Ok(0);
        };

        let plan_count = parsed.plan_count;
        if !parsed.processed_queries.is_empty() {
            self.emit_query_metrics(parsed.processed_queries).await?;
            info!(
                "Processed {} query plans from {} bytes of new content in {}",
                plan_count,
                parsed.end_offset - file_state.last_position,
                log_path.display()
            );
        }

        // last_position advances only to the boundary actually parsed (so
        // nothing is skipped); file_size records the observed size for the
        // truncation check above. Compressed files checkpoint at their full
        // (compressed) size since they are ingested in one shot.
        file_state.last_position = if is_compressed {
            current_size
        } else {
            parsed.end_offset
        };
        file_state.file_size = current_size;
        file_state.last_modified_time = current_mtime;
        file_state.last_processed_at = Utc::now();

        self.state_manager.update_file_state(&file_state)?;

        Ok(plan_count)
    }

    async fn process_remaining_content(&mut self, log_path: &Path) -> Result<usize> {
        // Use async file metadata to avoid blocking the runtime
        let metadata = tokio::fs::metadata(log_path).await?;
        let current_size = metadata.len();

        // Get previous state for this file
        let file_state = self.state_manager.get_file_state(log_path)?;

        let start_position = match file_state {
            Some(state) => {
                // Check if there's remaining content
                if current_size <= state.last_position {
                    debug!(
                        "No remaining content in {} (current: {}, last: {})",
                        log_path.display(),
                        current_size,
                        state.last_position
                    );
                    return Ok(0);
                }
                info!(
                    "Found remaining content in {}: {} bytes (from position {} to {})",
                    log_path.display(),
                    current_size - state.last_position,
                    state.last_position,
                    current_size
                );
                state.last_position
            }
            None => {
                info!(
                    "No previous state for {}, processing entire file",
                    log_path.display()
                );
                0
            }
        };

        // Final flush: parse the whole remaining range to EOF (no boundary
        // hold-back — nothing more will be written).
        let parsed = match self
            .parse_range_blocking(log_path, start_position, current_size, false)
            .await
        {
            Ok(Some(parsed)) => parsed,
            Ok(None) => return Ok(0),
            Err(e) => {
                warn!(
                    "Failed to process remaining content from {}: {}",
                    log_path.display(),
                    e
                );
                let mut labels_map = std::collections::HashMap::new();
                labels_map.insert("file_path", log_path.to_string_lossy().to_string());
                labels_map.insert("error_type", "parse_error".to_string());
                self.metrics.increment_parse_errors(&labels_map);
                return Ok(0);
            }
        };

        let plan_count = parsed.plan_count;
        if !parsed.processed_queries.is_empty() {
            self.emit_query_metrics(parsed.processed_queries).await?;
            info!(
                "Successfully processed {} query plans from remaining content",
                plan_count
            );
        }

        // Update file state to mark this content as processed. Offsets are
        // byte-accurate (metadata length), immune to CRLF/UTF-8 line-length
        // drift.
        let current_mtime = metadata
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs() as i64;
        let new_state = FileState {
            file_path: log_path.to_path_buf(),
            last_position: parsed.end_offset,
            last_modified_time: current_mtime,
            file_size: parsed.end_offset,
            last_processed_at: Utc::now(),
        };

        self.state_manager.update_file_state(&new_state)?;

        Ok(plan_count)
    }

    /// Parse `[start, end)` of `log_path` on the blocking thread pool (log
    /// parsing is CPU-bound and `get_processed_queries` fans out over rayon —
    /// neither belongs on a tokio worker thread).
    ///
    /// With `hold_back_last_entry`, parsing stops at the start of the last
    /// complete timestamped line so a mid-write entry is never half-parsed;
    /// returns `None` when the range contains no safe boundary yet.
    async fn parse_range_blocking(
        &mut self,
        log_path: &Path,
        start: u64,
        end: u64,
        hold_back_last_entry: bool,
    ) -> Result<Option<ParsedRange>> {
        let max_queries = self.config.log_parsing.max_queries_per_file;
        let path = log_path.to_path_buf();
        // Move the parser into the blocking task and put it back afterwards
        // (its fingerprint cache persists across cycles).
        let mut parser = std::mem::take(&mut self.log_parser);

        let (parser_back, result) = tokio::task::spawn_blocking(move || {
            let result = (|| -> Result<Option<ParsedRange>> {
                let parse_end = if hold_back_last_entry {
                    match find_entry_boundary(&path, start, end)? {
                        Some(boundary) if boundary > start => boundary,
                        _ => return Ok(None),
                    }
                } else {
                    end
                };

                let query_plans = parser.parse_file_range_with_progress(
                    &path,
                    start,
                    Some(parse_end),
                    |_, _| {},
                )?;

                let plans_to_process = if max_queries > 0 && query_plans.len() > max_queries {
                    warn!(
                        file = %path.display(),
                        query_count = query_plans.len(),
                        max_queries = max_queries,
                        "Query count exceeds limit, truncating"
                    );
                    &query_plans[..max_queries]
                } else {
                    &query_plans[..]
                };

                let plan_count = plans_to_process.len();
                let processed_queries = if plan_count > 0 {
                    parser.get_processed_queries(plans_to_process)
                } else {
                    Default::default()
                };

                Ok(Some(ParsedRange {
                    end_offset: parse_end,
                    plan_count,
                    processed_queries,
                }))
            })();
            (parser, result)
        })
        .await
        .context("Log parsing task panicked")?;

        self.log_parser = parser_back;
        result
    }

    async fn emit_query_metrics(
        &mut self,
        processed_queries: hashbrown::HashMap<String, ProcessedQuery>,
    ) -> Result<()> {
        // Pre-pass: filter the queries and accumulate the grand total of
        // total_duration_ms across the included set. The "exported set" boundary
        // for share-of-total is this emit_query_metrics batch.
        let mut included: Vec<&ProcessedQuery> = Vec::new();
        let mut grand_total_ms = 0.0_f64;
        for (_query_hash, query) in processed_queries.iter() {
            if !self.should_include_query(query)? {
                continue;
            }
            grand_total_ms += query.statistics.total_duration_ms;
            included.push(query);
        }

        // Emit pass: emit per-query series for each included query.
        for query in included {
            // Calculate hash using same approach as log parser
            let query_hash = xxhash_rust::xxh3::xxh3_64(query.normalized_query().as_bytes());
            let stable_hash = format!("{:016x}", query_hash);
            let database = self.extract_database_name(query.original_query());

            // Record the query hash for future reference, capturing the persisted
            // first/last-seen timestamps so we can export them as gauges.
            let (first_seen, last_seen) = self
                .state_manager
                .record_query_hash(&stable_hash, query.normalized_query())?;

            // Update metrics
            self.update_query_metrics(
                &stable_hash,
                &database,
                query,
                grand_total_ms,
                first_seen,
                last_seen,
            )
            .await?;
        }

        Ok(())
    }

    async fn update_query_metrics(
        &self,
        query_hash: &str,
        database: &str,
        query: &ProcessedQuery,
        grand_total_ms: f64,
        first_seen: DateTime<Utc>,
        last_seen: DateTime<Utc>,
    ) -> Result<()> {
        // First/last seen gauges (F9), keyed by {hash, database}.
        {
            let mut labels_map = std::collections::HashMap::new();
            labels_map.insert("normalized_query_hash", query_hash.to_string());
            labels_map.insert("database", database.to_string());
            self.metrics
                .set_query_first_seen_seconds(&labels_map, first_seen.timestamp() as f64);
            self.metrics
                .set_query_last_seen_seconds(&labels_map, last_seen.timestamp() as f64);
        }

        // Query performance metrics. Build the label maps once; the loop only
        // records values.
        {
            let mut labels_map = std::collections::HashMap::new();
            labels_map.insert("normalized_query_hash", query_hash.to_string());
            labels_map.insert("database", database.to_string());
            let mut exec_labels = labels_map.clone();
            exec_labels.insert("status", "success".to_string());
            for execution in &query.statistics.executions {
                let duration_secs = execution.duration_ms / 1000.0;
                self.metrics
                    .record_query_duration(&labels_map, duration_secs);
                self.metrics.increment_query_executions(&exec_labels);
            }
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
                let mut labels_map = std::collections::HashMap::new();
                labels_map.insert("database", database.to_string());
                labels_map.insert("threshold", threshold_str.to_string());
                self.metrics
                    .increment_slow_queries_by(&labels_map, slow_count as u64);
            }
        }

        // Plan analysis metrics using parsed plan
        {
            let parsed_plan = query.parsed_plan();

            // Extract plan cost from parsed plan
            let cost = parsed_plan.root.cost.max_total_cost;
            let mut labels_map = std::collections::HashMap::new();
            labels_map.insert("normalized_query_hash", query_hash.to_string());
            labels_map.insert("database", database.to_string());
            self.metrics.record_query_plan_cost(&labels_map, cost);

            // Count plan node types using proper parsing
            self.update_plan_metrics(database, parsed_plan).await?;
        }

        // Derived per-query metrics (F7). share-of-total is scoped to the
        // current emit_query_metrics batch (the "exported set" boundary).
        {
            let stats = &query.statistics;
            let mut labels_map = std::collections::HashMap::new();
            labels_map.insert("normalized_query_hash", query_hash.to_string());
            labels_map.insert("database", database.to_string());

            let cv = crate::metrics::derived::coefficient_of_variation(
                stats.mean_duration_ms,
                stats.std_dev_ms,
            );
            let share =
                crate::metrics::derived::time_share_pct(stats.total_duration_ms, grand_total_ms);
            self.metrics.set_query_latency_cv(&labels_map, cv);
            self.metrics
                .set_query_total_time_share_pct(&labels_map, share);
            self.metrics
                .set_query_latency_p95_ms(&labels_map, stats.percentiles.p95);
            self.metrics
                .set_query_latency_p99_ms(&labels_map, stats.percentiles.p99);
            // rows_per_call: deferred — no aggregate rows source available.
        }

        Ok(())
    }

    async fn update_plan_metrics(
        &self,
        database: &str,
        parsed_plan: &pg_plansight_core::ParsedPlan,
    ) -> Result<()> {
        // Recursively walk the plan tree and count node types
        self.count_node_metrics(&parsed_plan.root, database);

        Ok(())
    }

    fn count_node_metrics(&self, node: &pg_plansight_core::PlanNode, database: &str) {
        use pg_plansight_core::{JoinType, NodeType, ScanType};

        // Count scan types
        if let NodeType::Scan(scan_type) = &node.node_type {
            let scan_label = match scan_type {
                ScanType::SeqScan { .. } => "seq_scan",
                ScanType::IndexScan { .. } => "index_scan",
                ScanType::BitmapHeapScan { .. } => "bitmap_heap_scan",
                ScanType::BitmapIndexScan { .. } => "bitmap_index_scan",
                ScanType::ParallelBitmapHeapScan { .. } => "parallel_bitmap_heap_scan",
            };

            let mut labels_map = std::collections::HashMap::new();
            labels_map.insert("scan_type", scan_label.to_string());
            labels_map.insert("database", database.to_string());
            self.metrics.increment_scan_type(&labels_map);
        }

        // Count join types
        if let NodeType::Join(join_type) = &node.node_type {
            let join_label = match join_type {
                JoinType::NestedLoop { .. } | JoinType::NestedLoopLeftJoin { .. } => "nested_loop",
                JoinType::HashJoin { .. } => "hash_join",
                JoinType::MergeJoin { .. } => "merge_join",
            };

            let mut labels_map = std::collections::HashMap::new();
            labels_map.insert("join_type", join_label.to_string());
            labels_map.insert("database", database.to_string());
            self.metrics.increment_join_type(&labels_map);
        }

        // Recursively process children
        for child in &node.children {
            self.count_node_metrics(child, database);
        }
    }

    fn should_include_query(&self, query: &ProcessedQuery) -> Result<bool> {
        if let Some(ref filters) = self.config.filters {
            // Check minimum duration
            if let Some(min_duration_ms) = filters.min_duration_ms
                && query.statistics.min_duration_ms < min_duration_ms
            {
                return Ok(false);
            }

            // Check database inclusion
            if let Some(ref include_dbs) = filters.include_databases {
                let db_name = self.extract_database_name(query.original_query());
                if !include_dbs.contains(&db_name) {
                    return Ok(false);
                }
            }

            // Check query pattern exclusions
            if let Some(ref patterns) = self.filter_patterns {
                for pattern in patterns {
                    if pattern.is_match(query.normalized_query()) {
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

    fn extract_database_name(&self, _query: &str) -> String {
        // In a real implementation, you'd extract this from the log context
        // For now, return a default
        "unknown".to_string()
    }

    fn parse_threshold_to_ms(&self, threshold: &str) -> Result<f64> {
        if let Some(ms) = threshold.strip_suffix("ms") {
            Ok(ms.parse::<f64>()?)
        } else if let Some(s) = threshold.strip_suffix('s') {
            Ok(s.parse::<f64>()? * 1000.0)
        } else {
            anyhow::bail!("Invalid threshold format: {}", threshold);
        }
    }

    fn compile_filter_patterns(config: &Config) -> Result<Option<Vec<Regex>>> {
        if let Some(ref filters) = config.filters
            && let Some(ref patterns) = filters.exclude_query_patterns
        {
            let compiled_patterns: Result<Vec<_>> = patterns
                .iter()
                .map(|pattern| {
                    Regex::new(pattern)
                        .with_context(|| format!("Invalid regex pattern: {}", pattern))
                })
                .collect();
            return Ok(Some(compiled_patterns?));
        }
        Ok(None)
    }

    #[cfg(test)]
    pub(crate) fn parse_threshold_to_ms_pub(&self, threshold: &str) -> Result<f64> {
        self.parse_threshold_to_ms(threshold)
    }

    #[cfg(test)]
    pub(crate) fn compile_filter_patterns_pub(config: &Config) -> Result<Option<Vec<Regex>>> {
        Self::compile_filter_patterns(config)
    }

    #[cfg(test)]
    pub(crate) fn expand_log_paths_pub(&self) -> Result<Vec<PathBuf>> {
        self.expand_log_paths()
    }

    /// Delete state rows (processed files, query hashes) not seen within
    /// `metrics.retain_days`. 0 disables retention cleanup.
    ///
    /// File checkpoints are only dropped for files that no longer exist on
    /// disk: deleting the row of a merely-idle file that still matches the
    /// glob would re-parse it from byte 0 next cycle and double-count its
    /// entire history into monotonic counters.
    pub fn cleanup_old_state(&self) -> Result<usize> {
        let retain_days = self.config.metrics.retain_days;
        if retain_days == 0 {
            return Ok(0);
        }
        let cutoff = Utc::now() - chrono::Duration::days(i64::from(retain_days));
        let mut removed = 0;
        for (path, state) in self.state_manager.get_all_file_states()? {
            if state.last_processed_at < cutoff && !path.exists() {
                self.state_manager.delete_file_state(&path)?;
                removed += 1;
            }
        }
        removed += self.state_manager.cleanup_old_query_hashes(cutoff)?;
        Ok(removed)
    }

    pub fn update_config(&mut self, new_config: Config) -> Result<()> {
        info!("Updating collector configuration");

        // Recompile filter patterns if they changed
        let new_filter_patterns = Self::compile_filter_patterns(&new_config)?;

        // Check what changed for logging
        if self.config.log_parsing.log_paths != new_config.log_parsing.log_paths {
            info!(
                "Log paths updated: {:?} -> {:?}",
                self.config.log_parsing.log_paths, new_config.log_parsing.log_paths
            );
        }

        if self.config.log_parsing.batch_size != new_config.log_parsing.batch_size {
            info!(
                "Batch size updated: {} -> {}",
                self.config.log_parsing.batch_size, new_config.log_parsing.batch_size
            );
        }

        if self.config.metrics.slow_query_thresholds != new_config.metrics.slow_query_thresholds {
            info!(
                "Slow query thresholds updated: {:?} -> {:?}",
                self.config.metrics.slow_query_thresholds, new_config.metrics.slow_query_thresholds
            );
        }

        // Apply new configuration
        self.config = new_config;
        self.filter_patterns = new_filter_patterns;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        Config, FiltersConfig, LogParsingConfig, MetricsConfig, ServerConfig, StateConfig,
    };
    use crate::metrics::MetricsBackend;
    use std::collections::HashMap;
    use tempfile::tempdir;

    /// A no-op metrics backend for testing that doesn't require Prometheus.
    struct NoopMetrics;

    impl MetricsBackend for NoopMetrics {
        fn record_query_duration(&self, _labels: &HashMap<&str, String>, _duration_secs: f64) {}
        fn increment_query_executions(&self, _labels: &HashMap<&str, String>) {}
        fn increment_slow_queries(&self, _labels: &HashMap<&str, String>) {}
        fn record_query_plan_cost(&self, _labels: &HashMap<&str, String>, _cost: f64) {}
        fn record_query_rows_examined(&self, _labels: &HashMap<&str, String>, _rows: f64) {}
        fn record_database_avg_duration(&self, _labels: &HashMap<&str, String>, _duration: f64) {}
        fn record_database_qps(&self, _labels: &HashMap<&str, String>, _qps: f64) {}
        fn increment_database_unique_queries(&self, _labels: &HashMap<&str, String>, _count: u64) {}
        fn increment_plan_node_type(&self, _labels: &HashMap<&str, String>) {}
        fn increment_scan_type(&self, _labels: &HashMap<&str, String>) {}
        fn increment_join_type(&self, _labels: &HashMap<&str, String>) {}
        fn set_exporter_up(&self, _value: i64) {}
        fn set_memory_usage(&self, _bytes: i64) {}
        fn set_last_successful_parse(&self, _timestamp: i64) {}
        fn increment_logs_parsed(&self, _labels: &HashMap<&str, String>) {}
        fn increment_parse_errors(&self, _labels: &HashMap<&str, String>) {}
        fn record_export_duration(&self, _labels: &HashMap<&str, String>, _duration_secs: f64) {}
        fn set_query_latency_cv(&self, _labels: &HashMap<&str, String>, _cv: f64) {}
        fn set_query_total_time_share_pct(&self, _labels: &HashMap<&str, String>, _pct: f64) {}
        fn set_query_latency_p95_ms(&self, _labels: &HashMap<&str, String>, _p95_ms: f64) {}
        fn set_query_latency_p99_ms(&self, _labels: &HashMap<&str, String>, _p99_ms: f64) {}
        fn set_query_first_seen_seconds(&self, _labels: &HashMap<&str, String>, _secs: f64) {}
        fn set_query_last_seen_seconds(&self, _labels: &HashMap<&str, String>, _secs: f64) {}
        fn shutdown(&self) -> anyhow::Result<()> {
            Ok(())
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    fn make_minimal_config() -> Config {
        Config {
            server: ServerConfig {
                bind_address: "0.0.0.0:9090".to_string(),
                metrics_path: "/metrics".to_string(),
            },
            log_parsing: LogParsingConfig {
                log_paths: vec![],
                poll_interval: "30s".to_string(),
                batch_size: 1000,
                max_file_size_mb: 0,
                max_queries_per_file: 0,
            },
            metrics: MetricsConfig {
                namespace: "test".to_string(),
                backends: vec!["prometheus".to_string()],
                opentelemetry: None,
                histogram_buckets: vec![1.0],
                slow_query_thresholds: vec![],
                retain_days: 7,
            },
            state: StateConfig {
                database_path: "/tmp/test_collector.db".to_string(),
            },
            filters: None,
            pushgateway: None,
        }
    }

    fn make_collector(config: Config) -> LogCollector {
        let temp_dir = tempdir().unwrap();
        let db_path = temp_dir.path().join("collector_test.db");
        // Keep temp_dir alive by leaking it for the duration of the test.
        // This is acceptable in test-only code.
        std::mem::forget(temp_dir);
        let state_manager = crate::state::StateManager::new(&db_path);
        state_manager.initialize().unwrap();
        let metrics: Arc<dyn MetricsBackend> = Arc::new(NoopMetrics);
        LogCollector::new(config, state_manager, metrics).unwrap()
    }

    // -------------------------------------------------------------------------
    // parse_threshold_to_ms
    // -------------------------------------------------------------------------

    #[test]
    fn test_parse_threshold_500ms() {
        let collector = make_collector(make_minimal_config());
        let result = collector.parse_threshold_to_ms_pub("500ms").unwrap();
        assert_eq!(result, 500.0);
    }

    #[test]
    fn test_parse_threshold_1s() {
        let collector = make_collector(make_minimal_config());
        let result = collector.parse_threshold_to_ms_pub("1s").unwrap();
        assert_eq!(result, 1000.0);
    }

    #[test]
    fn test_parse_threshold_5s() {
        let collector = make_collector(make_minimal_config());
        let result = collector.parse_threshold_to_ms_pub("5s").unwrap();
        assert_eq!(result, 5000.0);
    }

    #[test]
    fn test_parse_threshold_2_5s() {
        let collector = make_collector(make_minimal_config());
        let result = collector.parse_threshold_to_ms_pub("2.5s").unwrap();
        assert_eq!(result, 2500.0);
    }

    #[test]
    fn test_parse_threshold_invalid_returns_err() {
        let collector = make_collector(make_minimal_config());
        let result = collector.parse_threshold_to_ms_pub("invalid");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_threshold_no_unit_returns_err() {
        let collector = make_collector(make_minimal_config());
        let result = collector.parse_threshold_to_ms_pub("500");
        assert!(result.is_err());
    }

    // -------------------------------------------------------------------------
    // compile_filter_patterns
    // -------------------------------------------------------------------------

    #[test]
    fn test_compile_filter_patterns_valid_regex() {
        let mut config = make_minimal_config();
        config.filters = Some(FiltersConfig {
            include_databases: None,
            exclude_query_patterns: Some(vec!["^BEGIN$".to_string(), "^COMMIT$".to_string()]),
            min_duration_ms: None,
        });
        let result = LogCollector::compile_filter_patterns_pub(&config).unwrap();
        assert!(result.is_some());
        let patterns = result.unwrap();
        assert_eq!(patterns.len(), 2);
    }

    #[test]
    fn test_compile_filter_patterns_invalid_regex_returns_err() {
        let mut config = make_minimal_config();
        config.filters = Some(FiltersConfig {
            include_databases: None,
            exclude_query_patterns: Some(vec!["[invalid regex".to_string()]),
            min_duration_ms: None,
        });
        let result = LogCollector::compile_filter_patterns_pub(&config);
        assert!(result.is_err());
    }

    #[test]
    fn test_compile_filter_patterns_no_filters_returns_none() {
        let config = make_minimal_config();
        let result = LogCollector::compile_filter_patterns_pub(&config).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_compile_filter_patterns_no_exclude_patterns_returns_none() {
        let mut config = make_minimal_config();
        config.filters = Some(FiltersConfig {
            include_databases: Some(vec!["mydb".to_string()]),
            exclude_query_patterns: None,
            min_duration_ms: None,
        });
        let result = LogCollector::compile_filter_patterns_pub(&config).unwrap();
        assert!(result.is_none());
    }

    // -------------------------------------------------------------------------
    // expand_log_paths (glob)
    // -------------------------------------------------------------------------

    #[test]
    fn test_expand_log_paths_matches_files_in_temp_dir() {
        let temp_dir = tempdir().unwrap();
        let log1 = temp_dir.path().join("pg-2024-01-01.log");
        let log2 = temp_dir.path().join("pg-2024-01-02.log");
        let other = temp_dir.path().join("README.txt");
        std::fs::write(&log1, "log1").unwrap();
        std::fs::write(&log2, "log2").unwrap();
        std::fs::write(&other, "readme").unwrap();

        let glob_pattern = format!("{}/*.log", temp_dir.path().display());
        let mut config = make_minimal_config();
        config.log_parsing.log_paths = vec![glob_pattern];

        let collector = make_collector(config);
        let paths = collector.expand_log_paths_pub().unwrap();

        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&log1));
        assert!(paths.contains(&log2));
        assert!(!paths.contains(&other));
    }

    #[test]
    fn test_expand_log_paths_no_pattern_returns_empty() {
        let config = make_minimal_config(); // log_paths is empty
        let collector = make_collector(config);
        let paths = collector.expand_log_paths_pub().unwrap();
        assert!(paths.is_empty());
    }

    #[test]
    fn test_expand_log_paths_nonexistent_glob_returns_empty() {
        let mut config = make_minimal_config();
        config.log_parsing.log_paths = vec!["/nonexistent_dir_xyz/*.log".to_string()];
        let collector = make_collector(config);
        let paths = collector.expand_log_paths_pub().unwrap();
        assert!(paths.is_empty());
    }

    // -------------------------------------------------------------------------
    // Entry-boundary detection & checkpointing
    // -------------------------------------------------------------------------

    const ENTRY_A: &str = "2025-01-15 10:00:00.000 UTC [1] LOG:  duration: 10.0 ms  plan:\n\tQuery Text: SELECT 1\n\tResult  (cost=0.00..0.01 rows=1 width=4)\n";
    const BARRIER: &str = "2025-01-15 10:00:01.000 UTC [1] LOG:  checkpoint complete\n";

    #[test]
    fn test_find_entry_boundary_returns_last_timestamped_line_start() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("t.log");
        let content = format!("{ENTRY_A}{BARRIER}");
        std::fs::write(&path, &content).unwrap();

        let boundary = find_entry_boundary(&path, 0, content.len() as u64)
            .unwrap()
            .unwrap();
        assert_eq!(
            boundary,
            ENTRY_A.len() as u64,
            "boundary must be BARRIER's start"
        );
    }

    #[test]
    fn test_find_entry_boundary_ignores_incomplete_final_line() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("t.log");
        // Barrier line has no trailing newline: still being written.
        let content = format!("{ENTRY_A}2025-01-15 10:00:01.000 UTC [1] LOG:  partial");
        std::fs::write(&path, &content).unwrap();

        let boundary = find_entry_boundary(&path, 0, content.len() as u64)
            .unwrap()
            .unwrap();
        // Only ENTRY_A's own first line qualifies.
        assert_eq!(boundary, 0);
    }

    #[test]
    fn test_find_entry_boundary_none_without_timestamped_lines() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("t.log");
        std::fs::write(&path, "\tcontinuation only\n\tmore\n").unwrap();
        assert!(find_entry_boundary(&path, 0, 24).unwrap().is_none());
    }

    #[tokio::test]
    async fn test_mid_write_entry_is_held_back_until_complete() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("poll.log");

        // Cycle 1: entry A complete, entry B mid-write (its continuation is
        // still being written; no line after it yet).
        let entry_b_start = "2025-01-15 10:00:02.000 UTC [1] LOG:  duration: 20.0 ms  plan:\n\tQuery Text: SELECT 2\n";
        std::fs::write(&path, format!("{ENTRY_A}{BARRIER}{entry_b_start}")).unwrap();

        let mut collector = make_collector(make_minimal_config());
        let processed = collector.process_log_file(&path).await.unwrap();
        assert_eq!(processed, 1, "only the complete entry A must be parsed");

        let state = collector
            .state_manager
            .get_file_state(&path)
            .unwrap()
            .unwrap();
        assert_eq!(
            state.last_position,
            (ENTRY_A.len() + BARRIER.len()) as u64,
            "checkpoint must stop at the start of the mid-write entry"
        );

        // Cycle 2: B's plan line lands plus a following barrier line.
        {
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            write!(
                f,
                "\tResult  (cost=0.00..0.02 rows=1 width=4)\n2025-01-15 10:00:03.000 UTC [1] LOG:  done\n"
            )
            .unwrap();
        }

        let processed = collector.process_log_file(&path).await.unwrap();
        assert_eq!(
            processed, 1,
            "entry B must be parsed exactly once, when complete"
        );
    }

    #[tokio::test]
    async fn test_quiescent_file_tail_is_flushed() {
        // A rotated/idle file stops growing with its final entry held back;
        // after QUIESCENT_CYCLES unchanged observations the tail must flush,
        // or the last entry of every rotated file would never be exported.
        let dir = tempdir().unwrap();
        let path = dir.path().join("rotated.log");
        let entry_b = "2025-01-15 10:00:02.000 UTC [1] LOG:  duration: 20.0 ms  plan:\n\tQuery Text: SELECT 2\n\tResult  (cost=0.00..0.02 rows=1 width=4)\n";
        std::fs::write(&path, format!("{ENTRY_A}{BARRIER}{entry_b}")).unwrap();

        let mut collector = make_collector(make_minimal_config());
        // Cycle 1: entry A parses; B is held back (no line after it).
        assert_eq!(collector.process_log_file(&path).await.unwrap(), 1);
        // Cycle 2: unchanged once — still held back.
        assert_eq!(collector.process_log_file(&path).await.unwrap(), 0);
        // Cycle 3: unchanged twice — quiescent, tail flushes to EOF.
        assert_eq!(
            collector.process_log_file(&path).await.unwrap(),
            1,
            "held-back tail of a quiescent file must be flushed"
        );

        let state = collector
            .state_manager
            .get_file_state(&path)
            .unwrap()
            .unwrap();
        assert_eq!(
            state.last_position,
            (ENTRY_A.len() + BARRIER.len() + entry_b.len()) as u64
        );

        // Cycle 4: nothing left.
        assert_eq!(collector.process_log_file(&path).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn test_compressed_file_is_ingested_once() {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use std::io::Write as _;

        // Compressed rotated logs have no plain-text boundaries to scan; they
        // must be parsed in one shot, not skipped forever.
        let dir = tempdir().unwrap();
        let path = dir.path().join("old.log.gz");
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(format!("{ENTRY_A}{BARRIER}").as_bytes())
            .unwrap();
        std::fs::write(&path, enc.finish().unwrap()).unwrap();

        let mut collector = make_collector(make_minimal_config());
        assert_eq!(
            collector.process_log_file(&path).await.unwrap(),
            1,
            "gzip log must parse on first sight"
        );
        // Second cycle: already ingested, no duplicates.
        assert_eq!(collector.process_log_file(&path).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn test_remaining_content_flushes_to_eof() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("flush.log");
        // File ends with a complete entry but no trailing line after it: the
        // shutdown flush must still parse it.
        std::fs::write(&path, ENTRY_A).unwrap();

        let mut collector = make_collector(make_minimal_config());
        let processed = collector.process_remaining_content(&path).await.unwrap();
        assert_eq!(processed, 1);

        let state = collector
            .state_manager
            .get_file_state(&path)
            .unwrap()
            .unwrap();
        assert_eq!(state.last_position, ENTRY_A.len() as u64);

        // Re-running finds nothing new.
        let processed = collector.process_remaining_content(&path).await.unwrap();
        assert_eq!(processed, 0);
    }
}
