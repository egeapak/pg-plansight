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
    /// Readiness signal, updated after each successful cycle. `None` for the
    /// one-shot batch commands, which have no HTTP surface.
    health: Option<Arc<crate::server::HealthState>>,
    /// `(label, threshold_ms)` resolved once at construction.
    ///
    /// These were re-parsed per query, per cycle, on the emit path — which is
    /// both wasted work and a failure point in code that must not be able to
    /// fail. `Config::validate` guarantees every entry parses, so building this
    /// eagerly turns a recurring hot-path error into a startup error.
    slow_thresholds: Vec<(String, f64)>,
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
    /// Consecutive cycles where a budget-clamped window yielded no complete
    /// entry. Doubles the effective budget so a single entry larger than the
    /// budget cannot stall the file forever.
    budget_stalls: u32,
}

/// Cycles with zero growth before a file is considered quiescent and its
/// held-back tail is flushed to EOF. Two full poll intervals of silence make
/// a mid-write stall at the exact boundary vanishingly unlikely.
const QUIESCENT_CYCLES: u32 = 2;

/// Consecutive parse failures after which the failing range is skipped, so a
/// deterministically-bad range cannot stall a file's export forever.
const MAX_PARSE_FAILURES: u32 = 3;

/// Filesystem identity `(dev, ino)` of an already-stat'd file.
///
/// Rotation detection previously relied on `current_size < file_size`. That
/// catches copytruncate and the common create-mode case, but under logrotate's
/// `create` mode the replacement file can outgrow the old checkpoint within one
/// poll interval — the size comparison then misses and the collector resumes at
/// the old offset, silently skipping the head of the new file.
#[cfg(unix)]
fn file_identity(metadata: &std::fs::Metadata) -> (Option<u64>, Option<u64>) {
    use std::os::unix::fs::MetadataExt;
    (Some(metadata.dev()), Some(metadata.ino()))
}

#[cfg(not(unix))]
fn file_identity(_metadata: &std::fs::Metadata) -> (Option<u64>, Option<u64>) {
    // No stable identity: fall back to size-based detection.
    (None, None)
}

/// Placeholder value for the `database` metric label.
///
/// auto_explain writes plans to the server log without naming the database
/// unless `log_line_prefix` includes `%d`, and neither the core parser nor
/// `QueryPlan` models a database today. Every metric therefore carries this
/// constant. It exists as a named constant rather than an inline literal so
/// that the day per-database attribution lands, the call sites are greppable.
pub(crate) const UNKNOWN_DATABASE: &str = "unknown";

/// Result of parsing one byte range of a log file.
struct ParsedRange {
    /// Absolute offset up to which content was actually parsed; becomes the
    /// persisted checkpoint.
    end_offset: u64,
    plan_count: usize,
    processed_queries: hashbrown::HashMap<String, ProcessedQuery>,
}

/// Read the byte range `[start, end)` of `path` into memory in one pass,
/// tolerating a file that shrank since the size was sampled (reads up to
/// `end - start` bytes).
fn read_file_range(path: &Path, start: u64, end: u64) -> Result<Vec<u8>> {
    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom};

    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    debug_assert!(end >= start, "read_file_range called with end < start");
    let len = end.saturating_sub(start);
    let mut buf = Vec::with_capacity(len as usize);
    file.take(len).read_to_end(&mut buf)?;
    Ok(buf)
}

/// Local offset of the START of the last complete, timestamped log line in
/// `bytes`, or `None` if there is none. Counting raw bytes keeps offsets exact
/// regardless of CRLF line endings or invalid UTF-8. This is the in-memory
/// equivalent of the old separate boundary-scan pass over the file.
fn last_entry_boundary(bytes: &[u8]) -> Option<usize> {
    let mut offset = 0usize;
    let mut last = None;
    for line in bytes.split_inclusive(|&b| b == b'\n') {
        let complete = line.last() == Some(&b'\n');
        if complete && is_timestamped_line(line) {
            last = Some(offset);
        }
        offset += line.len();
    }
    last
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

        let slow_thresholds = config
            .metrics
            .slow_query_thresholds
            .iter()
            .map(|label| {
                crate::config::parse_threshold_to_ms(label)
                    .map(|ms| (label.clone(), ms))
                    .with_context(|| format!("invalid slow_query_thresholds entry {label:?}"))
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(Self {
            config,
            state_manager,
            metrics,
            log_parser,
            filter_patterns,
            file_runtime: hashbrown::HashMap::new(),
            health: None,
            slow_thresholds,
        })
    }

    /// Attach the readiness signal updated after each clean cycle.
    pub fn with_health(mut self, health: Arc<crate::server::HealthState>) -> Self {
        self.health = Some(health);
        self
    }

    pub async fn collect_metrics(&mut self) -> Result<()> {
        let start = std::time::Instant::now();

        let log_paths = self.expand_log_paths()?;
        info!("Processing {} log files", log_paths.len());

        // Drop per-file runtime state for files that no longer match the glob.
        // This map was insert-only, so under daily rotation it accumulated one
        // entry per filename ever seen for the daemon's whole lifetime.
        {
            let live: std::collections::HashSet<&Path> =
                log_paths.iter().map(|(path, _)| path.as_path()).collect();
            self.file_runtime
                .retain(|path, _| live.contains(path.as_path()));
        }

        let mut total_processed = 0;
        let mut total_errors = 0;

        for (log_path, pattern) in log_paths {
            match self.process_log_file(&log_path, &pattern).await {
                Ok(processed) => {
                    total_processed += processed;
                    if processed > 0 {
                        let mut labels_map = std::collections::HashMap::new();
                        labels_map.insert("log_path_pattern", pattern.to_string());
                        labels_map.insert("status", "success".to_string());
                        self.metrics
                            .increment_logs_parsed_by(&labels_map, processed as u64);
                    }
                }
                Err(e) => {
                    total_errors += 1;
                    error!("Failed to process log file {}: {}", log_path.display(), e);
                    let mut labels_map = std::collections::HashMap::new();
                    labels_map.insert("log_path_pattern", pattern.to_string());
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
            if let Some(health) = &self.health {
                health.mark_success();
            }
        }

        // `exporter_up` was set to 1 once at construction and never touched
        // again, so it could not express degradation and any alert on it was
        // decorative. It now means "the most recent cycle completed with no
        // per-file errors". Note that liveness is properly expressed by
        // Prometheus's own synthetic `up{job=...}`; alert on staleness of
        // `last_successful_parse_timestamp` for "is it keeping up".
        self.metrics
            .set_exporter_up(if total_errors == 0 { 1 } else { 0 });

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

        for (log_path, pattern) in log_paths {
            match self.process_remaining_content(&log_path, &pattern).await {
                Ok(processed) => {
                    if processed > 0 {
                        files_with_remaining += 1;
                        total_processed += processed;
                        let mut labels_map = std::collections::HashMap::new();
                        labels_map.insert("log_path_pattern", pattern.to_string());
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
                    labels_map.insert("log_path_pattern", pattern.to_string());
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
            if let Some(health) = &self.health {
                health.mark_success();
            }
        }

        // `exporter_up` was set to 1 once at construction and never touched
        // again, so it could not express degradation and any alert on it was
        // decorative. It now means "the most recent cycle completed with no
        // per-file errors". Note that liveness is properly expressed by
        // Prometheus's own synthetic `up{job=...}`; alert on staleness of
        // `last_successful_parse_timestamp` for "is it keeping up".
        self.metrics
            .set_exporter_up(if total_errors == 0 { 1 } else { 0 });

        crate::metrics::update_memory_usage(self.metrics.as_ref());

        let duration = start.elapsed().as_secs_f64();
        let mut labels_map = std::collections::HashMap::new();
        labels_map.insert("operation", "collect_remaining".to_string());
        self.metrics.record_export_duration(&labels_map, duration);

        Ok(())
    }

    async fn process_log_file(&mut self, log_path: &Path, pattern: &str) -> Result<usize> {
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
                dev: None,
                ino: None,
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

        // Handle rotation. `file_size` stores the size observed last cycle (not
        // the parsed boundary), keeping the full detection window for
        // copytruncate-style rotation; `(dev, ino)` additionally catches a
        // replacement file that outgrew the old checkpoint within one interval,
        // which the size comparison alone would miss.
        let (current_dev, current_ino) = file_identity(&metadata);
        let identity_changed = match (file_state.dev, file_state.ino, current_dev, current_ino) {
            (Some(old_dev), Some(old_ino), Some(new_dev), Some(new_ino)) => {
                old_dev != new_dev || old_ino != new_ino
            }
            // Pre-migration row, or a platform without stable identity: the
            // size comparison is all we have.
            _ => false,
        };

        if identity_changed || current_size < file_state.file_size {
            info!(
                file = %log_path.display(),
                identity_changed,
                "File was rotated or truncated; processing from the beginning"
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
        // Clamp how much of the backlog one cycle ingests. The remainder is
        // picked up next cycle; `last_entry_boundary` guarantees the cut lands
        // on a complete-line start, so an arbitrary byte budget is safe.
        let budget = self.effective_read_budget(log_path);
        let read_end = if is_compressed || budget == 0 {
            current_size
        } else {
            current_size.min(file_state.last_position.saturating_add(budget))
        };
        let clamped = read_end < current_size;

        // A clamped window must never be treated as EOF: cutting mid-backlog is
        // not the same as "the writer stopped here".
        let hold_back = (!quiescent || clamped) && !is_compressed;
        let parsed = match self
            .parse_range_blocking(log_path, file_state.last_position, read_end, hold_back)
            .await
        {
            Ok(parsed) => {
                let made_progress = parsed
                    .as_ref()
                    .is_some_and(|p| p.end_offset > file_state.last_position);
                if let Some(runtime) = self.file_runtime.get_mut(log_path) {
                    runtime.parse_failures = 0;
                    if clamped && !made_progress {
                        // The clamped window held no complete entry; widen it so
                        // an entry larger than the budget is eventually read.
                        runtime.budget_stalls = runtime.budget_stalls.saturating_add(1);
                    } else {
                        runtime.budget_stalls = 0;
                    }
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
                labels_map.insert("log_path_pattern", pattern.to_string());
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
        file_state.dev = current_dev;
        file_state.ino = current_ino;

        self.state_manager.update_file_state(&file_state)?;

        Ok(plan_count)
    }

    async fn process_remaining_content(&mut self, log_path: &Path, pattern: &str) -> Result<usize> {
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
                labels_map.insert("log_path_pattern", pattern.to_string());
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
        let (dev, ino) = file_identity(&metadata);
        let new_state = FileState {
            file_path: log_path.to_path_buf(),
            last_position: parsed.end_offset,
            last_modified_time: current_mtime,
            file_size: parsed.end_offset,
            last_processed_at: Utc::now(),
            dev,
            ino,
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
                // The hold-back path (uncompressed incremental read) reads the
                // new range from disk ONCE, finds the last complete-entry
                // boundary in that in-memory buffer, and parses the bytes up to
                // it — no second pass over the file. The flush path
                // (hold_back=false, also the compressed-file path) stays on the
                // file API because it must decompress and read to EOF.
                let (query_plans, parse_end) = if hold_back_last_entry {
                    let bytes = read_file_range(&path, start, end)?;
                    let Some(parse_len) = last_entry_boundary(&bytes).filter(|&b| b > 0) else {
                        return Ok(None);
                    };
                    let plans = parser.parse_with_progress(
                        std::io::Cursor::new(&bytes[..parse_len]),
                        parse_len as u64,
                        |_, _| {},
                    )?;
                    (plans, start + parse_len as u64)
                } else {
                    let plans = parser.parse_file_range_with_progress(
                        &path,
                        start,
                        Some(end),
                        |_, _| {},
                    )?;
                    (plans, end)
                };

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
            if !self.should_include_query(query) {
                continue;
            }
            grand_total_ms += query.statistics.total_duration_ms;
            included.push(query);
        }

        if included.is_empty() {
            return Ok(());
        }

        // Everything fallible happens BEFORE the first metric is touched.
        //
        // Emission and the file checkpoint must be all-or-nothing. Previously
        // the per-query state write sat inside the emit loop, so a failure on
        // query k left queries 0..k already counted and returned before
        // `update_file_state` — and because the retry cap only covers parse
        // failures, the same range was re-parsed and re-emitted into monotonic
        // counters on every poll thereafter, indefinitely.
        let hashes: Vec<String> = included
            .iter()
            .map(|q| {
                format!(
                    "{:016x}",
                    xxhash_rust::xxh3::xxh3_64(q.normalized_query().as_bytes())
                )
            })
            .collect();

        let entries: Vec<(String, String)> = hashes
            .iter()
            .zip(included.iter())
            .map(|(hash, q)| (hash.clone(), q.normalized_query().to_string()))
            .collect();

        // One batched transaction, off the async runtime: this is thousands of
        // upserts on a busy cycle and SQLite is blocking.
        let state_manager = self.state_manager.clone();
        let seen = tokio::task::spawn_blocking(move || state_manager.record_query_hashes(&entries))
            .await
            .context("query-hash batch task panicked")??;

        // From here on nothing can fail, so no partial emission is possible.
        for (hash, query) in hashes.iter().zip(included.iter()) {
            let (first_seen, last_seen) = seen.get(hash).copied().unwrap_or_else(|| {
                let now = Utc::now();
                (now, now)
            });

            self.update_query_metrics(
                hash,
                UNKNOWN_DATABASE,
                query,
                grand_total_ms,
                first_seen,
                last_seen,
            );
        }

        Ok(())
    }

    /// Record one query's series. Infallible and synchronous by design: it
    /// only touches in-memory counters, and the emit pass must not be able to
    /// fail partway through (see `emit_query_metrics`).
    fn update_query_metrics(
        &self,
        query_hash: &str,
        database: &str,
        query: &ProcessedQuery,
        grand_total_ms: f64,
        first_seen: DateTime<Utc>,
        last_seen: DateTime<Utc>,
    ) {
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
        for (threshold_str, threshold_ms) in &self.slow_thresholds {
            let threshold_ms = *threshold_ms;
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
            self.update_plan_metrics(database, parsed_plan);
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
    }

    /// Count plan node types. Infallible and synchronous, as above.
    fn update_plan_metrics(&self, database: &str, parsed_plan: &pg_plansight_core::ParsedPlan) {
        // Recursively walk the plan tree and count node types
        self.count_node_metrics(&parsed_plan.root, database);
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

    /// Infallible: called during the pre-emit filter pass, which must not be
    /// able to fail once emission has started.
    fn should_include_query(&self, query: &ProcessedQuery) -> bool {
        if let Some(ref filters) = self.config.filters {
            // Check minimum duration
            if let Some(min_duration_ms) = filters.min_duration_ms
                && query.statistics.min_duration_ms < min_duration_ms
            {
                return false;
            }

            // NOTE: `filters.include_databases` is intentionally not applied.
            // It used to compare the configured names against a hardcoded
            // "unknown", so any non-empty list dropped every query while the
            // daemon reported successful collection. `Config::validate` now
            // rejects the key outright rather than honoring it incorrectly.

            // Check query pattern exclusions
            if let Some(ref patterns) = self.filter_patterns {
                for pattern in patterns {
                    if pattern.is_match(query.normalized_query()) {
                        return false;
                    }
                }
            }
        }

        true
    }

    /// Expand the configured globs to `(path, originating pattern)`.
    ///
    /// The pattern travels with the path so metrics can be labelled by it: the
    /// concrete filename is an unbounded label value (a rotation scheme like
    /// `postgresql-%Y-%m-%d.log` mints a new one every day, and Prometheus
    /// client label sets are never evicted).
    fn expand_log_paths(&self) -> Result<Vec<(PathBuf, Arc<str>)>> {
        let mut paths = Vec::new();

        for pattern in &self.config.log_parsing.log_paths {
            let label: Arc<str> = Arc::from(pattern.as_str());
            match glob::glob(pattern) {
                Ok(entries) => {
                    for entry in entries {
                        match entry {
                            Ok(path) => {
                                if path.is_file() {
                                    paths.push((path, label.clone()));
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

    // NOTE: there is deliberately no `extract_database_name` here any more.
    //
    // It used to ignore its argument and return the literal "unknown", which
    // made the `database` label on every metric a constant, and made
    // `filters.include_databases` compare user-supplied names against
    // "unknown" — silently dropping 100% of queries while the daemon logged
    // "Collection complete". Attributing a query to a database requires
    // `log_line_prefix` parsing in pg-plansight-core (which has no `database`
    // field on `QueryPlan` today); until that exists, the honest thing is a
    // single named constant. See `UNKNOWN_DATABASE`.

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
    pub(crate) fn compile_filter_patterns_pub(config: &Config) -> Result<Option<Vec<Regex>>> {
        Self::compile_filter_patterns(config)
    }

    #[cfg(test)]
    pub(crate) fn expand_log_paths_pub(&self) -> Result<Vec<PathBuf>> {
        Ok(self
            .expand_log_paths()?
            .into_iter()
            .map(|(path, _)| path)
            .collect())
    }

    #[cfg(test)]
    pub(crate) fn file_runtime_len(&self) -> usize {
        self.file_runtime.len()
    }

    /// Delete state rows (processed files, query hashes) not seen within
    /// `metrics.retain_days`. 0 disables retention cleanup.
    ///
    /// File checkpoints are only dropped for files that no longer exist on
    /// disk: deleting the row of a merely-idle file that still matches the
    /// glob would re-parse it from byte 0 next cycle and double-count its
    /// entire history into monotonic counters.
    /// Per-cycle read budget for `log_path`, doubled once per consecutive
    /// stalled cycle so a single entry larger than the configured budget is
    /// eventually read rather than stalling the file forever. 0 = unlimited.
    fn effective_read_budget(&self, log_path: &Path) -> u64 {
        let base = self.config.log_parsing.max_read_bytes_per_cycle;
        if base == 0 {
            return 0;
        }
        let stalls = self
            .file_runtime
            .get(log_path)
            .map(|r| r.budget_stalls)
            .unwrap_or(0);
        base.saturating_mul(1u64 << stalls.min(20))
    }

    /// Runs the retention pass off the async runtime.
    ///
    /// The pass is blocking SQLite plus one `exists()` syscall per tracked
    /// file; on the scheduler task that stalls a tokio worker for as long as it
    /// takes.
    pub async fn cleanup_old_state(&self) -> Result<usize> {
        let retain_days = self.config.metrics.retain_days;
        if retain_days == 0 {
            return Ok(0);
        }
        let cutoff = Utc::now() - chrono::Duration::days(i64::from(retain_days));
        let state_manager = self.state_manager.clone();

        tokio::task::spawn_blocking(move || Self::cleanup_blocking(&state_manager, cutoff))
            .await
            .context("retention cleanup task panicked")?
    }

    fn cleanup_blocking(state_manager: &StateManager, cutoff: DateTime<Utc>) -> Result<usize> {
        let mut removed = 0;
        for (path, state) in state_manager.get_all_file_states()? {
            if state.last_processed_at < cutoff && !path.exists() {
                state_manager.delete_file_state(&path)?;
                removed += 1;
            }
        }
        removed += state_manager.cleanup_old_query_hashes(cutoff)?;
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
                max_read_bytes_per_cycle: 0,
            },
            metrics: MetricsConfig {
                namespace: "test".to_string(),
                backends: vec!["prometheus".to_string()],
                opentelemetry: None,
                histogram_buckets: vec![1.0],
                slow_query_thresholds: vec![],
                retain_days: 7,
                max_query_cardinality: 0,
            },
            state: StateConfig {
                database_path: "/tmp/test_collector.db".to_string(),
            },
            filters: None,
            pushgateway: None,
        }
    }

    /// Counts emitted executions so a test can assert that a failed cycle
    /// emitted *nothing*.
    #[derive(Default)]
    struct CountingMetrics {
        executions: std::sync::atomic::AtomicU64,
        /// Distinct label values seen on logs_parsed / parse_errors. These are
        /// unbounded Prometheus label values if they carry a filename.
        log_labels: std::sync::Mutex<std::collections::HashSet<String>>,
    }

    impl CountingMetrics {
        fn note_log_labels(&self, labels: &HashMap<&str, String>) {
            if let Some(value) = labels
                .get("log_path_pattern")
                .or_else(|| labels.get("file_path"))
            {
                self.log_labels
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(value.clone());
            }
        }

        fn distinct_log_labels(&self) -> usize {
            self.log_labels
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .len()
        }
    }

    impl MetricsBackend for CountingMetrics {
        fn increment_query_executions(&self, _labels: &HashMap<&str, String>) {
            self.executions
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        fn increment_logs_parsed(&self, labels: &HashMap<&str, String>) {
            self.note_log_labels(labels);
        }
        fn increment_parse_errors(&self, labels: &HashMap<&str, String>) {
            self.note_log_labels(labels);
        }
        fn record_query_duration(&self, _labels: &HashMap<&str, String>, _duration_secs: f64) {}
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

    /// Collector wired to a counting backend, plus the state db path so a test
    /// can make writes fail.
    fn make_counting_collector(
        config: Config,
        db_path: &std::path::Path,
    ) -> (LogCollector, Arc<CountingMetrics>) {
        let state_manager = crate::state::StateManager::new(db_path);
        state_manager.initialize().unwrap();
        let backend = Arc::new(CountingMetrics::default());
        let metrics: Arc<dyn MetricsBackend> = backend.clone();
        (
            LogCollector::new(config, state_manager, metrics).unwrap(),
            backend,
        )
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
        let result = crate::config::parse_threshold_to_ms("500ms").unwrap();
        assert_eq!(result, 500.0);
    }

    #[test]
    fn test_parse_threshold_1s() {
        let result = crate::config::parse_threshold_to_ms("1s").unwrap();
        assert_eq!(result, 1000.0);
    }

    #[test]
    fn test_parse_threshold_5s() {
        let result = crate::config::parse_threshold_to_ms("5s").unwrap();
        assert_eq!(result, 5000.0);
    }

    #[test]
    fn test_parse_threshold_2_5s() {
        let result = crate::config::parse_threshold_to_ms("2.5s").unwrap();
        assert_eq!(result, 2500.0);
    }

    #[test]
    fn test_parse_threshold_invalid_returns_err() {
        let result = crate::config::parse_threshold_to_ms("invalid");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_threshold_no_unit_returns_err() {
        let result = crate::config::parse_threshold_to_ms("500");
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

    // -------------------------------------------------------------------------
    // Phase 2: emission is atomic with respect to the checkpoint
    // -------------------------------------------------------------------------

    /// Break the state database so every subsequent write fails.
    ///
    /// Dropping the table is used rather than `chmod`: the connection is
    /// cached, so a permission change on the file does not affect the open
    /// handle, and root ignores the read-only bit entirely.
    fn break_state_writes(collector: &LogCollector) {
        collector
            .state_manager
            .with_conn(|conn| {
                conn.execute("DROP TABLE query_hashes", [])?;
                Ok(())
            })
            .unwrap();
    }

    fn repair_state_writes(collector: &LogCollector) {
        collector
            .state_manager
            .with_conn(|conn| {
                conn.execute(
                    "CREATE TABLE query_hashes (
                        query_hash TEXT PRIMARY KEY,
                        normalized_query TEXT NOT NULL,
                        first_seen_at TEXT NOT NULL,
                        last_seen_at TEXT NOT NULL
                    )",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
    }

    /// A failing state write must emit nothing at all.
    ///
    /// The per-query upsert used to sit inside the emit loop, so a failure on
    /// query k left queries 0..k already counted and then returned before the
    /// checkpoint advanced — and since the retry cap only covers parse
    /// failures, the same range was re-parsed and re-emitted on every poll
    /// thereafter, indefinitely.
    #[tokio::test]
    async fn state_write_failure_emits_no_metrics() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("pg.log");
        std::fs::write(&log, format!("{ENTRY_A}{BARRIER}")).unwrap();
        let db = dir.path().join("state.db");

        let (mut collector, metrics) = make_counting_collector(make_minimal_config(), &db);
        break_state_writes(&collector);

        let result = collector.process_log_file(&log, "*.log").await;
        assert!(result.is_err(), "a failed state write must fail the cycle");
        assert_eq!(
            metrics
                .executions
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "no metric may be emitted when the state write fails"
        );

        // The checkpoint must not have advanced either.
        let checkpoint = collector.state_manager.get_file_state(&log).unwrap();
        assert!(
            checkpoint.is_none_or(|c| c.last_position == 0),
            "the checkpoint must not advance past content that was never emitted"
        );
    }

    /// Repeated failures must not multiply counters, and recovery must emit
    /// exactly once.
    #[tokio::test]
    async fn repeated_state_failure_does_not_multiply_counters() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("pg.log");
        std::fs::write(&log, format!("{ENTRY_A}{BARRIER}")).unwrap();
        let db = dir.path().join("state.db");

        let (mut collector, metrics) = make_counting_collector(make_minimal_config(), &db);
        break_state_writes(&collector);

        for _ in 0..5 {
            assert!(collector.process_log_file(&log, "*.log").await.is_err());
        }
        assert_eq!(
            metrics
                .executions
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "five failed cycles must not have emitted anything"
        );

        repair_state_writes(&collector);
        collector.process_log_file(&log, "*.log").await.unwrap();
        assert_eq!(
            metrics
                .executions
                .load(std::sync::atomic::Ordering::Relaxed),
            1,
            "after recovery the entry must be counted exactly once, not six times"
        );
    }

    /// logrotate `create` mode: a new inode at the same path, larger than the
    /// old checkpoint, so the size comparison alone cannot see the rotation.
    #[cfg(unix)]
    #[tokio::test]
    async fn rotation_detected_by_inode_when_size_does_not_shrink() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("pg.log");
        std::fs::write(&path, format!("{ENTRY_A}{BARRIER}")).unwrap();

        let mut collector = make_collector(make_minimal_config());
        assert_eq!(collector.process_log_file(&path, "*.log").await.unwrap(), 1);

        let checkpoint = collector
            .state_manager
            .get_file_state(&path)
            .unwrap()
            .expect("a checkpoint");
        assert!(checkpoint.last_position > 0);
        assert!(checkpoint.ino.is_some(), "identity must be persisted");

        // Rename away and create a NEW inode at the same path, deliberately
        // larger than the old file so `current_size < file_size` is false.
        std::fs::rename(&path, dir.path().join("pg.log.1")).unwrap();
        let entry_b = "2025-01-15 11:00:00.000 UTC [1] LOG:  duration: 20.0 ms  plan:\n\tQuery Text: SELECT 2\n\tResult  (cost=0.00..0.02 rows=1 width=4)\n";
        std::fs::write(&path, format!("{ENTRY_A}{BARRIER}{entry_b}{BARRIER}")).unwrap();

        let new_ino = std::fs::metadata(&path)
            .map(|m| {
                use std::os::unix::fs::MetadataExt;
                m.ino()
            })
            .unwrap();
        assert_ne!(new_ino, checkpoint.ino.unwrap(), "test needs a new inode");

        // Before the fix: resumed at the old offset and saw only the tail.
        assert_eq!(
            collector.process_log_file(&path, "*.log").await.unwrap(),
            2,
            "an inode change must be treated as rotation and re-read from 0"
        );
    }

    // -------------------------------------------------------------------------
    // Phase 3: bounded label cardinality and runtime-map pruning
    // -------------------------------------------------------------------------

    /// `log_filename = 'postgresql-%Y-%m-%d.log'` produces a new filename per
    /// rotation. With the concrete path as a label value, every rotation added
    /// a permanent series to `logs_parsed_total` and `parse_errors_total` —
    /// the LRU only ever bounded `normalized_query_hash`.
    #[tokio::test]
    async fn log_metrics_label_by_pattern_not_by_filename() {
        let dir = tempdir().unwrap();
        let mut config = make_minimal_config();
        config.log_parsing.log_paths = vec![format!("{}/*.log", dir.path().display())];

        let db = dir.path().join("state.db");
        let (mut collector, metrics) = make_counting_collector(config, &db);

        for day in 1..=20 {
            let path = dir.path().join(format!("postgresql-2026-01-{day:02}.log"));
            std::fs::write(&path, format!("{ENTRY_A}{BARRIER}")).unwrap();
            collector.collect_metrics().await.unwrap();
            std::fs::remove_file(&path).unwrap();
        }

        assert_eq!(
            metrics.distinct_log_labels(),
            1,
            "log metrics grew to {} distinct label values across 20 rotations; \
             expected one per configured pattern",
            metrics.distinct_log_labels()
        );
    }

    /// `file_runtime` was insert-only: one entry per filename ever seen, held
    /// for the daemon's lifetime.
    #[tokio::test]
    async fn file_runtime_is_pruned_when_files_disappear() {
        let dir = tempdir().unwrap();
        let mut config = make_minimal_config();
        config.log_parsing.log_paths = vec![format!("{}/*.log", dir.path().display())];

        let db = dir.path().join("state.db");
        let (mut collector, _metrics) = make_counting_collector(config, &db);

        for day in 1..=5 {
            let path = dir.path().join(format!("pg-{day}.log"));
            std::fs::write(&path, format!("{ENTRY_A}{BARRIER}")).unwrap();
            collector.collect_metrics().await.unwrap();
            std::fs::remove_file(&path).unwrap();
        }

        // One more cycle: the glob now matches nothing.
        collector.collect_metrics().await.unwrap();

        assert_eq!(
            collector.file_runtime_len(),
            0,
            "runtime state for files that no longer match the glob must be pruned"
        );
    }

    // -------------------------------------------------------------------------
    // Phase 4: per-cycle read budget
    // -------------------------------------------------------------------------

    /// Build a log of `n` distinct entries followed by a barrier.
    fn multi_entry_log(n: usize) -> String {
        let mut out = String::new();
        for i in 0..n {
            out.push_str(&format!(
                "2025-01-15 10:{:02}:{:02}.000 UTC [1] LOG:  duration: {}.0 ms  plan:\n\tQuery Text: SELECT {}\n\tResult  (cost=0.00..0.01 rows=1 width=4)\n",
                i / 60, i % 60, 10 + i, i
            ));
        }
        out.push_str(BARRIER);
        out
    }

    /// The hold-back path allocated the entire unread range in one `Vec`. On a
    /// restart against a log that grew while the daemon was down, that is the
    /// whole backlog in a single allocation — and an allocation failure in Rust
    /// aborts the process, which then repeats on every restart.
    #[tokio::test]
    async fn catch_up_is_chunked_across_cycles_without_loss_or_duplication() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("pg.log");
        let content = multi_entry_log(40);
        std::fs::write(&log, &content).unwrap();

        let mut config = make_minimal_config();
        // Roughly three entries' worth.
        config.log_parsing.max_read_bytes_per_cycle = (content.len() / 13) as u64;

        let db = dir.path().join("state.db");
        let (mut collector, _metrics) = make_counting_collector(config, &db);

        let first = collector.process_log_file(&log, "*.log").await.unwrap();
        assert!(first > 0, "the first cycle must make progress");
        assert!(
            first < 40,
            "the first cycle read the whole file ({first} entries); the budget was not applied"
        );

        let mut total = first;
        for _ in 0..60 {
            let n = collector.process_log_file(&log, "*.log").await.unwrap();
            total += n;
            if n == 0 {
                break;
            }
        }

        assert_eq!(
            total, 40,
            "every entry must be ingested exactly once across the chunked cycles"
        );
    }

    /// A single entry larger than the budget must not stall the file forever:
    /// a clamped window containing no complete entry yields no boundary, so the
    /// budget has to grow until one fits.
    #[tokio::test]
    async fn entry_larger_than_the_budget_is_not_stalled() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("pg.log");
        let padding = "x".repeat(4096);
        let big = format!(
            "2025-01-15 10:00:00.000 UTC [1] LOG:  duration: 10.0 ms  plan:\n\tQuery Text: SELECT '{padding}'\n\tResult  (cost=0.00..0.01 rows=1 width=4)\n"
        );
        std::fs::write(&log, format!("{big}{BARRIER}")).unwrap();

        let mut config = make_minimal_config();
        config.log_parsing.max_read_bytes_per_cycle = 128;

        let db = dir.path().join("state.db");
        let (mut collector, _metrics) = make_counting_collector(config, &db);

        let mut total = 0;
        for _ in 0..40 {
            total += collector.process_log_file(&log, "*.log").await.unwrap();
            if total > 0 {
                break;
            }
        }

        assert_eq!(
            total, 1,
            "an entry larger than the budget must eventually be read, exactly once"
        );
    }

    /// 0 keeps the historical unbounded behaviour.
    #[tokio::test]
    async fn budget_of_zero_reads_everything_in_one_cycle() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("pg.log");
        std::fs::write(&log, multi_entry_log(20)).unwrap();

        let mut config = make_minimal_config();
        config.log_parsing.max_read_bytes_per_cycle = 0;

        let db = dir.path().join("state.db");
        let (mut collector, _metrics) = make_counting_collector(config, &db);

        assert_eq!(collector.process_log_file(&log, "*.log").await.unwrap(), 20);
    }

    const ENTRY_A: &str = "2025-01-15 10:00:00.000 UTC [1] LOG:  duration: 10.0 ms  plan:\n\tQuery Text: SELECT 1\n\tResult  (cost=0.00..0.01 rows=1 width=4)\n";
    const BARRIER: &str = "2025-01-15 10:00:01.000 UTC [1] LOG:  checkpoint complete\n";

    #[test]
    fn test_last_entry_boundary_returns_last_timestamped_line_start() {
        let content = format!("{ENTRY_A}{BARRIER}");
        let boundary = last_entry_boundary(content.as_bytes()).unwrap();
        assert_eq!(boundary, ENTRY_A.len(), "boundary must be BARRIER's start");
    }

    #[test]
    fn test_last_entry_boundary_ignores_incomplete_final_line() {
        // Barrier line has no trailing newline: still being written.
        let content = format!("{ENTRY_A}2025-01-15 10:00:01.000 UTC [1] LOG:  partial");
        let boundary = last_entry_boundary(content.as_bytes()).unwrap();
        // Only ENTRY_A's own first line qualifies.
        assert_eq!(boundary, 0);
    }

    #[test]
    fn test_last_entry_boundary_none_without_timestamped_lines() {
        assert!(last_entry_boundary(b"\tcontinuation only\n\tmore\n").is_none());
    }

    #[test]
    fn test_read_file_range_reads_exact_window() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("r.log");
        std::fs::write(&path, b"0123456789").unwrap();
        assert_eq!(read_file_range(&path, 2, 6).unwrap(), b"2345");
        // Tolerates a shrunk window (reads what is available).
        assert_eq!(read_file_range(&path, 8, 100).unwrap(), b"89");
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
        let processed = collector.process_log_file(&path, "*.log").await.unwrap();
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

        let processed = collector.process_log_file(&path, "*.log").await.unwrap();
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
        assert_eq!(collector.process_log_file(&path, "*.log").await.unwrap(), 1);
        // Cycle 2: unchanged once — still held back.
        assert_eq!(collector.process_log_file(&path, "*.log").await.unwrap(), 0);
        // Cycle 3: unchanged twice — quiescent, tail flushes to EOF.
        assert_eq!(
            collector.process_log_file(&path, "*.log").await.unwrap(),
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
        assert_eq!(collector.process_log_file(&path, "*.log").await.unwrap(), 0);
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
            collector.process_log_file(&path, "*.log").await.unwrap(),
            1,
            "gzip log must parse on first sight"
        );
        // Second cycle: already ingested, no duplicates.
        assert_eq!(collector.process_log_file(&path, "*.log").await.unwrap(), 0);
    }

    #[tokio::test]
    async fn test_remaining_content_flushes_to_eof() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("flush.log");
        // File ends with a complete entry but no trailing line after it: the
        // shutdown flush must still parse it.
        std::fs::write(&path, ENTRY_A).unwrap();

        let mut collector = make_collector(make_minimal_config());
        let processed = collector
            .process_remaining_content(&path, "*.log")
            .await
            .unwrap();
        assert_eq!(processed, 1);

        let state = collector
            .state_manager
            .get_file_state(&path)
            .unwrap()
            .unwrap();
        assert_eq!(state.last_position, ENTRY_A.len() as u64);

        // Re-running finds nothing new.
        let processed = collector
            .process_remaining_content(&path, "*.log")
            .await
            .unwrap();
        assert_eq!(processed, 0);
    }
}
