use anyhow::Context as _;
use hashbrown::HashMap;
use std::collections::BTreeMap;
use std::io::BufRead;
// For Read::take on the capped line reader; needed regardless of file-io.
use std::io::Read as _;
use tracing::warn;

// File reading + decompression (gated so the core can be embedded without an
// I/O surface, e.g. inside a Postgres extension).
#[cfg(feature = "file-io")]
use bzip2::read::MultiBzDecoder;
#[cfg(feature = "file-io")]
use flate2::read::MultiGzDecoder;
#[cfg(feature = "file-io")]
use std::fs::File;
#[cfg(feature = "file-io")]
use std::io::{BufReader, Seek, SeekFrom};
#[cfg(feature = "file-io")]
use std::path::Path;

// Thread-based parallelism (gated off in single-threaded embeds such as a
// Postgres backend, where spawning threads that touch backend state is unsafe).
#[cfg(all(feature = "parallel", feature = "file-io"))]
use crate::models::{DateFilter, ParseProgress};
#[cfg(feature = "parallel")]
use rayon::prelude::*;
#[cfg(all(feature = "parallel", feature = "file-io"))]
use std::path::PathBuf;
#[cfg(all(feature = "parallel", feature = "file-io"))]
use std::sync::mpsc;
#[cfg(all(feature = "parallel", feature = "file-io"))]
use std::thread;

use crate::models::{ProcessedQuery, QueryGroupStatistics, QueryPlan};
use crate::parsing::{LogParsingState as ParsingState, PlanFormat, QueryPlanBuilder};

use crate::parser_utils::{
    QueryStatisticsCalculator, RegexPatterns, TimezoneResolver, parse_duration_from_line,
    parse_timestamp_with_tz,
};
use crate::plan_parser::PlanParser;
use crate::sql_analysis::normalize_query_enhanced;

#[cfg(feature = "file-io")]
mod magic_number {
    pub const GZIP: [u8; 2] = [0x1f, 0x8b];
    pub const BZIP2: [u8; 3] = [0x42, 0x5a, 0x68]; // "BZh"
}

/// Upper bound on bytes read from a compressed stream after decompression.
/// This is a safety ceiling against decompression bombs (a few KB expanding to
/// terabytes), set well above any realistic single PostgreSQL log file so it
/// never truncates legitimate input. Reads stop once this many decompressed
/// bytes have been consumed.
#[cfg(feature = "file-io")]
const MAX_DECOMPRESSED_BYTES: u64 = 16 * 1024 * 1024 * 1024; // 16 GiB

/// Upper bound on a single log line. auto_explain lines can be long (large
/// filters, huge IN-lists) but not gigabytes; without this cap one crafted
/// newline-free line grows the read buffer until the process OOMs.
const MAX_LINE_BYTES: u64 = 64 * 1024 * 1024; // 64 MiB

/// Upper bound on one accumulated log entry (query text + plan lines).
const MAX_ENTRY_BYTES: u64 = 256 * 1024 * 1024; // 256 MiB

/// Upper bound on the persistent fingerprint cache. Long-lived parsers (the
/// exporter daemon reuses one across poll cycles) otherwise grow an entry per
/// distinct raw query text forever.
const MAX_FINGERPRINT_CACHE_ENTRIES: usize = 100_000;

/// Bounded LRU cache mapping a query-text hash to its normalized fingerprint.
///
/// A long-lived parser would otherwise grow one entry per distinct query text
/// forever. Evicting the least-recently-used entry when full keeps the hot
/// working set warm at steady cost — unlike clearing the whole cache at the
/// threshold, which periodically dropped every entry and re-normalized the
/// entire next batch (a recurring CPU sawtooth, and permanently useless for a
/// working set just over the cap).
#[derive(Debug)]
struct FingerprintCache {
    cap: usize,
    tick: u64,
    /// hash -> (fingerprint, last-access tick).
    entries: HashMap<u64, (String, u64)>,
    /// last-access tick -> hash; the first key is the least-recently-used entry.
    order: BTreeMap<u64, u64>,
}

impl FingerprintCache {
    fn with_capacity(cap: usize) -> Self {
        Self {
            cap,
            tick: 0,
            entries: HashMap::with_capacity(cap.min(1024)),
            order: BTreeMap::new(),
        }
    }

    /// Return the fingerprint for `hash`, refreshing its recency on a hit.
    fn get(&mut self, hash: u64) -> Option<String> {
        let (fingerprint, old_tick) = {
            let entry = self.entries.get(&hash)?;
            (entry.0.clone(), entry.1)
        };
        self.tick += 1;
        let now = self.tick;
        self.order.remove(&old_tick);
        self.order.insert(now, hash);
        if let Some(entry) = self.entries.get_mut(&hash) {
            entry.1 = now;
        }
        Some(fingerprint)
    }

    /// Insert or refresh `hash`, evicting the least-recently-used entry when the
    /// cap would be exceeded (`cap == 0` disables the bound).
    fn insert(&mut self, hash: u64, fingerprint: String) {
        self.tick += 1;
        let now = self.tick;
        if let Some(entry) = self.entries.get_mut(&hash) {
            self.order.remove(&entry.1);
            entry.0 = fingerprint;
            entry.1 = now;
            self.order.insert(now, hash);
            return;
        }
        if self.cap > 0
            && self.entries.len() >= self.cap
            && let Some((&lru_tick, &lru_hash)) = self.order.iter().next()
        {
            self.order.remove(&lru_tick);
            self.entries.remove(&lru_hash);
        }
        self.entries.insert(hash, (fingerprint, now));
        self.order.insert(now, hash);
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.tick = 0;
    }

    fn len(&self) -> usize {
        self.entries.len()
    }
}

#[derive(Debug)]
pub struct PostgreSQLLogParser {
    pub regex_patterns: RegexPatterns,
    pub plan_parser: PlanParser,
    byte_buffer: Vec<u8>,
    /// Bounded LRU cache mapping query hash to fingerprint to avoid re-normalization.
    fingerprint_cache: FingerprintCache,
    /// How timezone abbreviations in log timestamps resolve to UTC offsets.
    /// Defaults to the built-in Default-tznames table; override it for servers
    /// whose `log_timezone` prints an ambiguous abbreviation (e.g. "CST").
    timezone: TimezoneResolver,
    /// Hard cap on a single log line (OOM guard against a crafted newline-free
    /// line); `u64::MAX` disables it. Default [`MAX_LINE_BYTES`].
    max_line_bytes: u64,
    /// Hard cap on one accumulated entry (query text + continuation/plan lines);
    /// `u64::MAX` disables it. Default [`MAX_ENTRY_BYTES`].
    max_entry_bytes: u64,
}

impl PostgreSQLLogParser {
    pub fn new() -> Self {
        Self {
            regex_patterns: RegexPatterns::default(),
            plan_parser: PlanParser::new().expect("Failed to create PlanParser"),
            byte_buffer: Vec::with_capacity(8192),
            fingerprint_cache: FingerprintCache::with_capacity(MAX_FINGERPRINT_CACHE_ENTRIES),
            timezone: TimezoneResolver::default(),
            max_line_bytes: MAX_LINE_BYTES,
            max_entry_bytes: MAX_ENTRY_BYTES,
        }
    }

    /// Override the hostile-input byte caps: `max_line` bounds a single log line
    /// and `max_entry` bounds one accumulated entry (query text + continuation
    /// lines). Passing `0` for either disables that cap (`u64::MAX`) — do so only
    /// for fully trusted input, since the caps are the guard against a crafted
    /// newline-free or never-terminated entry growing memory without bound. A
    /// memory-constrained daemon may lower them; a legitimate multi-hundred-MiB
    /// IN-list may need them raised.
    pub fn with_byte_limits(mut self, max_line: u64, max_entry: u64) -> Self {
        self.max_line_bytes = if max_line == 0 { u64::MAX } else { max_line };
        self.max_entry_bytes = if max_entry == 0 { u64::MAX } else { max_entry };
        self
    }

    /// Override how timezone abbreviations in log timestamps are resolved to UTC
    /// offsets (see [`TimezoneResolver`]). Use this when the server's
    /// `log_timezone` prints an abbreviation the built-in Default-tznames table
    /// would misinterpret — e.g. `TimezoneResolver::new().with_override("CST",
    /// 8 * 3600)` to read "CST" as China Standard Time rather than US Central.
    pub fn with_timezone_override(mut self, timezone: TimezoneResolver) -> Self {
        self.timezone = timezone;
        self
    }

    /// Detect plan format based on content (streaming heuristic; a wrong
    /// Json guess is corrected by the builder's NotAPlan demotion).
    fn detect_plan_format(&self, content: &str) -> PlanFormat {
        if crate::parsing::format_detection::looks_like_json_start(content) {
            PlanFormat::Json
        } else {
            PlanFormat::Text
        }
    }

    // =========================================================================
    // State machine handlers - extracted for better readability and testability
    // =========================================================================

    /// Handle the WaitingForQuery state - looking for a "Query Text:" prefix,
    /// or the opening of a JSON entry.
    fn handle_waiting_for_query(
        mut builder: QueryPlanBuilder,
        trimmed: &str,
    ) -> (ParsingState, Option<QueryPlan>) {
        if let Some(query_text) = trimmed.strip_prefix("Query Text:") {
            builder.set_query_text(query_text.trim().to_string());
            (ParsingState::ParsingQuery(builder), None)
        } else if trimmed.starts_with('{') || trimmed.starts_with('[') {
            // auto_explain.log_format=json emits the whole entry as one JSON
            // object with the query embedded as a "Query Text" key — there is
            // no "Query Text:" text line preceding the plan.
            let typed_builder = builder.convert_to_json();
            if let QueryPlanBuilder::Json(json_builder) = typed_builder {
                Self::process_json_plan_line(json_builder, trimmed, String::new())
            } else {
                (
                    ParsingState::ParsingJsonPlan(typed_builder, String::new()),
                    None,
                )
            }
        } else {
            (ParsingState::WaitingForQuery(builder), None)
        }
    }

    /// Handle the ParsingQuery state - detecting format and starting plan parsing
    /// Returns (new_state, optional_completed_plan)
    fn handle_parsing_query(
        &self,
        builder: QueryPlanBuilder,
        trimmed: &str,
        line_trimmed: &str,
    ) -> (ParsingState, Option<QueryPlan>) {
        let format = self.detect_plan_format(trimmed);

        match format {
            PlanFormat::Text => {
                if self.regex_patterns.plan_regex.is_match(trimmed) {
                    let typed_builder = builder.convert_to_text();
                    if let QueryPlanBuilder::Text(text_builder) = typed_builder {
                        Self::process_text_plan_line(text_builder, line_trimmed)
                    } else {
                        (ParsingState::ParsingTextPlan(typed_builder), None)
                    }
                } else {
                    // Continue parsing query text - use efficient append
                    let mut updated_builder = builder;
                    updated_builder.append_query_line(line_trimmed);
                    (ParsingState::ParsingQuery(updated_builder), None)
                }
            }
            PlanFormat::Json => {
                let typed_builder = builder.convert_to_json();
                if let QueryPlanBuilder::Json(json_builder) = typed_builder {
                    Self::process_json_plan_line(json_builder, trimmed, String::new())
                } else {
                    (
                        ParsingState::ParsingJsonPlan(typed_builder, String::new()),
                        None,
                    )
                }
            }
        }
    }

    /// Handle the ParsingTextPlan state - continuing to parse text plan lines
    /// Returns (new_state, optional_completed_plan)
    fn handle_parsing_text_plan(
        builder: QueryPlanBuilder,
        line_trimmed: &str,
    ) -> (ParsingState, Option<QueryPlan>) {
        if let QueryPlanBuilder::Text(text_builder) = builder {
            Self::process_text_plan_line(text_builder, line_trimmed)
        } else {
            (ParsingState::ParsingTextPlan(builder), None)
        }
    }

    /// Handle the ParsingJsonPlan state - continuing to parse JSON plan lines
    /// Returns (new_state, optional_completed_plan)
    fn handle_parsing_json_plan(
        builder: QueryPlanBuilder,
        trimmed: &str,
        json_content: String,
    ) -> (ParsingState, Option<QueryPlan>) {
        if let QueryPlanBuilder::Json(json_builder) = builder {
            Self::process_json_plan_line(json_builder, trimmed, json_content)
        } else {
            (ParsingState::ParsingJsonPlan(builder, json_content), None)
        }
    }

    /// Process a line for a text plan builder
    fn process_text_plan_line(
        text_builder: crate::parsing::TextPlanBuilder,
        line: &str,
    ) -> (ParsingState, Option<QueryPlan>) {
        match text_builder.add_line(line) {
            Ok((updated_builder, maybe_plan)) => {
                if let Some(plan) = maybe_plan {
                    (ParsingState::None, Some(plan))
                } else {
                    (
                        ParsingState::ParsingTextPlan(QueryPlanBuilder::Text(updated_builder)),
                        None,
                    )
                }
            }
            Err(e) => {
                warn!(error = %e, "Text plan parsing error");
                (ParsingState::None, None)
            }
        }
    }

    /// Process a line for a JSON plan builder
    fn process_json_plan_line(
        json_builder: crate::parsing::JsonPlanBuilder,
        line: &str,
        json_content: String,
    ) -> (ParsingState, Option<QueryPlan>) {
        use crate::parsing::JsonLineOutcome;

        let (updated_builder, outcome) = json_builder.add_line(line);
        match outcome {
            JsonLineOutcome::Complete(plan) => (ParsingState::None, Some(*plan)),
            JsonLineOutcome::Incomplete => (
                ParsingState::ParsingJsonPlan(
                    QueryPlanBuilder::Json(updated_builder),
                    json_content,
                ),
                None,
            ),
            JsonLineOutcome::NotAPlan => {
                // The '{'/'[' opener was part of the query text (e.g. a jsonb
                // literal), not a plan document: put the accumulated lines
                // back into the query text and resume query parsing.
                (
                    ParsingState::ParsingQuery(QueryPlanBuilder::Untyped(
                        updated_builder.into_query_builder(),
                    )),
                    None,
                )
            }
        }
    }

    #[cfg(feature = "file-io")]
    fn create_reader<P: AsRef<Path>>(file_path: P) -> anyhow::Result<(Box<dyn BufRead>, u64)> {
        let mut file = File::open(&file_path)?;
        let file_size = file.metadata()?.len();

        // Check magic bytes directly from the opened file
        let mut magic_bytes = [0u8; 3];
        let (is_gzip, is_bzip2) = match file.read_exact(&mut magic_bytes) {
            Ok(_) => {
                let is_gzip = magic_bytes[0..2] == magic_number::GZIP;
                let is_bzip2 = magic_bytes == magic_number::BZIP2;
                (is_gzip, is_bzip2)
            }
            Err(_) => (false, false),
        };
        // Reset file position to beginning
        file.seek(SeekFrom::Start(0))?;

        if is_gzip {
            // Multi* decoders handle concatenated members (cat a.gz b.gz, pigz,
            // bgzip); the single-member decoders silently stop at the first
            // member boundary. Cap decompressed output to defend against
            // decompression bombs: a tiny compressed file can otherwise expand
            // without bound.
            let decoder = MultiGzDecoder::new(file).take(MAX_DECOMPRESSED_BYTES);
            let reader = BufReader::with_capacity(64 * 1024, decoder);
            Ok((Box::new(reader), file_size))
        } else if is_bzip2 {
            let decoder = MultiBzDecoder::new(file).take(MAX_DECOMPRESSED_BYTES);
            let reader = BufReader::with_capacity(64 * 1024, decoder);
            Ok((Box::new(reader), file_size))
        } else {
            let reader = BufReader::with_capacity(64 * 1024, file);
            Ok((Box::new(reader), file_size))
        }
    }

    #[cfg(feature = "file-io")]
    fn create_reader_with_range<P: AsRef<Path>>(
        file_path: P,
        start_offset: u64,
        end_offset: Option<u64>,
    ) -> anyhow::Result<(Box<dyn BufRead>, u64)> {
        let mut file = File::open(&file_path)?;
        let file_size = file.metadata()?.len();
        let end_pos = end_offset.unwrap_or(file_size);

        if start_offset > file_size {
            anyhow::bail!(
                "Start offset {} exceeds file size {}",
                start_offset,
                file_size
            );
        }

        if end_pos > file_size {
            anyhow::bail!("End offset {} exceeds file size {}", end_pos, file_size);
        }

        if start_offset >= end_pos {
            anyhow::bail!(
                "Start offset {} must be less than end offset {}",
                start_offset,
                end_pos
            );
        }

        // Check if file is compressed (only check if starting from beginning)
        let (is_gzip, is_bzip2) = if start_offset == 0 {
            let mut magic_bytes = [0u8; 3];
            let (is_gzip, is_bzip2) = match file.read_exact(&mut magic_bytes) {
                Ok(_) => {
                    let is_gzip = magic_bytes[0..2] == magic_number::GZIP;
                    let is_bzip2 = magic_bytes == magic_number::BZIP2;
                    (is_gzip, is_bzip2)
                }
                Err(_) => (false, false),
            };
            file.seek(SeekFrom::Start(0))?;
            (is_gzip, is_bzip2)
        } else {
            (false, false) // Can't seek into compressed files
        };

        let effective_size = end_pos - start_offset;

        if is_gzip || is_bzip2 {
            // For compressed files, we can't seek to arbitrary positions
            if start_offset > 0 {
                anyhow::bail!("Cannot seek to offset {} in compressed file", start_offset);
            }

            if is_gzip {
                let decoder = MultiGzDecoder::new(file).take(MAX_DECOMPRESSED_BYTES);
                let reader = BufReader::with_capacity(64 * 1024, decoder);
                Ok((Box::new(reader), effective_size))
            } else {
                let decoder = MultiBzDecoder::new(file).take(MAX_DECOMPRESSED_BYTES);
                let reader = BufReader::with_capacity(64 * 1024, decoder);
                Ok((Box::new(reader), effective_size))
            }
        } else {
            // For uncompressed files, seek to start position and create limited reader
            file.seek(SeekFrom::Start(start_offset))?;
            let limited_reader = file.take(effective_size);
            let reader = BufReader::with_capacity(64 * 1024, limited_reader);
            Ok((Box::new(reader), effective_size))
        }
    }

    pub fn parse_with_progress<R: BufRead, F>(
        &mut self,
        mut reader: R,
        total_size: u64,
        mut progress_callback: F,
    ) -> anyhow::Result<Vec<QueryPlan>>
    where
        F: FnMut(f64, usize),
    {
        let total_size = total_size as f64;
        // Copy the byte caps out so the read loop can reference them while
        // `self.byte_buffer` is mutably borrowed.
        let max_line_bytes = self.max_line_bytes;
        let max_entry_bytes = self.max_entry_bytes;
        let mut query_plans = Vec::with_capacity(2000);
        let mut parsing_state = ParsingState::None;
        let mut line_count = 0u64;
        let mut matched_log_lines = 0u64;
        let mut bytes_processed = 0u64;
        let mut plans_processed = 0usize;
        let mut entry_bytes = 0u64;

        loop {
            self.byte_buffer.clear();
            // Cap the single-line read: logs are untrusted input, and one
            // newline-free multi-GiB line (e.g. from a crafted .gz) would
            // otherwise grow byte_buffer until the process OOMs.
            let bytes_read = (&mut reader)
                .take(max_line_bytes)
                .read_until(b'\n', &mut self.byte_buffer)
                .context("Failed to read line from log file")?;

            if bytes_read as u64 == max_line_bytes && self.byte_buffer.last() != Some(&b'\n') {
                warn!(
                    limit = max_line_bytes,
                    "Log line exceeds the per-line limit; truncating"
                );
                // Discard the remainder of the oversized line.
                loop {
                    let buf = reader.fill_buf().context("Failed to skip oversized line")?;
                    if buf.is_empty() {
                        break;
                    }
                    match buf.iter().position(|&b| b == b'\n') {
                        Some(pos) => {
                            reader.consume(pos + 1);
                            bytes_processed += (pos + 1) as u64;
                            break;
                        }
                        None => {
                            let len = buf.len();
                            reader.consume(len);
                            bytes_processed += len as u64;
                        }
                    }
                }
            }

            // Invalid UTF-8 (e.g. LATIN1-encoded query text) is replaced with
            // U+FFFD rather than truncating the line at the first bad byte,
            // which corrupted plan content and fingerprints.
            let slice = String::from_utf8_lossy(&self.byte_buffer);

            if bytes_read == 0 {
                break; // EOF
            }

            line_count += 1;
            bytes_processed += bytes_read as u64;

            // Bound per-entry accumulation as well: continuation lines are
            // appended to the current builder until the next timestamped
            // line, which adversarial input can delay indefinitely.
            if matches!(parsing_state, ParsingState::None) {
                entry_bytes = 0;
            } else {
                entry_bytes += bytes_read as u64;
                if entry_bytes > max_entry_bytes {
                    warn!(
                        limit = max_entry_bytes,
                        "Log entry exceeds the per-entry limit; discarding it"
                    );
                    parsing_state = ParsingState::None;
                    entry_bytes = 0;
                }
            }

            // Update progress every 10000 lines for better performance
            if line_count.is_multiple_of(10000) {
                let current_len = query_plans.len();
                let delta = current_len - plans_processed;
                plans_processed = current_len;
                let progress = (bytes_processed as f64 / total_size).min(1.0);
                progress_callback(progress, delta);
            }

            // Remove trailing newline in place
            let line_trimmed = slice.trim_end();

            if let Some(captures) = self.regex_patterns.log_line_regex.captures(line_trimmed) {
                matched_log_lines += 1;
                let timestamp_str = captures.get(1).unwrap().as_str();
                let message = captures.get(2).unwrap().as_str();

                // Check for "duration: X ms plan:" which starts auto_explain output
                if let Some(duration) =
                    parse_duration_from_line(message, &self.regex_patterns.duration_regex)
                {
                    let timestamp = parse_timestamp_with_tz(timestamp_str, &self.timezone)
                        .with_context(|| format!("Can't parse timestamp: '{}'", timestamp_str))?;

                    let new_builder = QueryPlanBuilder::new(timestamp, duration);

                    if let Some(current_plan) = parsing_state.reset_with_builder(new_builder) {
                        query_plans.push(current_plan);
                    }
                }
                // Any other log line with timestamp ends the current parsing
                else if let Some(plan) = parsing_state.finish() {
                    query_plans.push(plan);
                }
            } else {
                // Handle continuation lines (lines that don't match the log format)
                let trimmed = line_trimmed.trim();

                if trimmed.is_empty() {
                    continue;
                }

                // Process state transition using extracted helper methods
                let (new_state, completed_plan) =
                    match std::mem::replace(&mut parsing_state, ParsingState::None) {
                        ParsingState::WaitingForQuery(builder) => {
                            Self::handle_waiting_for_query(builder, trimmed)
                        }
                        ParsingState::ParsingQuery(builder) => {
                            self.handle_parsing_query(builder, trimmed, line_trimmed)
                        }
                        ParsingState::ParsingTextPlan(builder) => {
                            Self::handle_parsing_text_plan(builder, line_trimmed)
                        }
                        ParsingState::ParsingJsonPlan(builder, json_content) => {
                            Self::handle_parsing_json_plan(builder, trimmed, json_content)
                        }
                        state => (state, None),
                    };

                parsing_state = new_state;
                if let Some(plan) = completed_plan {
                    query_plans.push(plan);
                }
            }
        }

        // Handle any remaining plan
        if let Some(plan) = parsing_state.finish() {
            query_plans.push(plan);
        }

        if line_count > 0 && matched_log_lines == 0 {
            warn!(
                lines = line_count,
                "No line matched the expected PostgreSQL log format; check that \
                 log_line_prefix starts with %m or %t (e.g. '%m [%p] ')"
            );
        }

        // Final progress update
        progress_callback(1.0, 0);

        Ok(query_plans)
    }

    // Convenience methods that use the generic parse_with_progress

    #[cfg(feature = "file-io")]
    pub fn parse_file_with_progress<P: AsRef<Path>, F>(
        &mut self,
        file_path: P,
        progress_callback: F,
    ) -> anyhow::Result<Vec<QueryPlan>>
    where
        F: FnMut(f64, usize),
    {
        let (reader, total_size) = Self::create_reader(&file_path)?;
        self.parse_with_progress(reader, total_size, progress_callback)
    }

    #[cfg(feature = "file-io")]
    pub fn parse_file_range_with_progress<P: AsRef<Path>, F>(
        &mut self,
        file_path: P,
        start_offset: u64,
        end_offset: Option<u64>,
        progress_callback: F,
    ) -> anyhow::Result<Vec<QueryPlan>>
    where
        F: FnMut(f64, usize),
    {
        let (reader, effective_size) =
            Self::create_reader_with_range(&file_path, start_offset, end_offset)?;
        self.parse_with_progress(reader, effective_size, progress_callback)
    }

    pub fn parse_string_with_progress<F>(
        &mut self,
        content: &str,
        progress_callback: F,
    ) -> anyhow::Result<Vec<QueryPlan>>
    where
        F: FnMut(f64, usize),
    {
        let reader = std::io::Cursor::new(content.as_bytes());
        let content_size = content.len() as u64;
        self.parse_with_progress(reader, content_size, progress_callback)
    }

    pub fn parse_bytes_with_progress<F>(
        &mut self,
        content: &[u8],
        progress_callback: F,
    ) -> anyhow::Result<Vec<QueryPlan>>
    where
        F: FnMut(f64, usize),
    {
        let reader = std::io::Cursor::new(content);
        let content_size = content.len() as u64;
        self.parse_with_progress(reader, content_size, progress_callback)
    }

    #[cfg(all(feature = "parallel", feature = "file-io"))]
    pub fn parse_multiple_files_async(
        file_paths: Vec<PathBuf>,
        date_filter: DateFilter,
    ) -> mpsc::Receiver<ParseProgress> {
        let (tx, rx) = mpsc::channel();

        thread::spawn(move || {
            let results: Vec<_> = file_paths
                .par_iter()
                .enumerate()
                .map(|(file_index, file_path)| {
                    let tx = tx.clone();

                    // Create a new parser instance for each thread
                    let mut thread_parser = PostgreSQLLogParser::new();

                    match thread_parser.parse_file_with_progress(
                        file_path,
                        |progress, queries_parsed| {
                            let _ = tx.send(ParseProgress::Progress {
                                file_index,
                                file_path: file_path.clone(),
                                progress,
                                queries_parsed,
                            });
                        },
                    ) {
                        Ok(plans) => Ok((file_index, plans)),
                        Err(e) => {
                            let _ = tx.send(ParseProgress::Error {
                                file_index,
                                file_path: file_path.clone(),
                                error: format!("Parse error: {}", e),
                            });
                            Err((file_index, e))
                        }
                    }
                })
                .collect();

            // Collect successful results and apply date filtering
            let mut all_query_plans = Vec::new();
            for result in results {
                match result {
                    Ok((_, mut plans)) => {
                        // Apply date filtering
                        plans.retain(|plan| date_filter.matches(plan.timestamp()));
                        all_query_plans.append(&mut plans);
                    }
                    Err((_, _)) => {
                        // Error already reported through progress
                        continue;
                    }
                }
            }

            // Send final result and close channel
            let _ = tx.send(ParseProgress::Complete {
                result: Ok(all_query_plans),
            });
        });

        rx
    }

    /// Calculate a fast hash for a query string using xxHash
    fn calculate_query_hash(query: &str) -> u64 {
        // Use xxHash for fast, high-quality hashing
        xxhash_rust::xxh3::xxh3_64(query.as_bytes())
    }

    pub fn get_processed_queries(
        &mut self,
        plans: &[QueryPlan],
    ) -> HashMap<String, ProcessedQuery> {
        // The fingerprint cache is a bounded LRU; it self-evicts, so there is no
        // clear-at-threshold sawtooth here.
        // Group plans by fingerprint using enhanced normalization.
        // Pre-size from the plan count to avoid repeated rehashing on large logs.
        let mut query_groups: HashMap<String, Vec<usize>> = HashMap::with_capacity(plans.len());
        // Local cache for this batch (most useful since many plans have same query within a batch)
        let mut local_normalization_cache: HashMap<&str, String> =
            HashMap::with_capacity(plans.len());

        for (idx, plan) in plans.iter().enumerate() {
            let query_text = plan.query_text();

            // Check local cache first (for queries within this batch)
            let fingerprint =
                if let Some(cached_fingerprint) = local_normalization_cache.get(query_text) {
                    cached_fingerprint.clone()
                } else {
                    // Check persistent cache using fast hash
                    let query_hash = Self::calculate_query_hash(query_text);
                    if let Some(cached_fingerprint) = self.fingerprint_cache.get(query_hash) {
                        // Store in local cache for subsequent lookups in this batch
                        local_normalization_cache.insert(query_text, cached_fingerprint.clone());
                        cached_fingerprint
                    } else {
                        // Only normalize if not in either cache
                        match normalize_query_enhanced(query_text) {
                            Ok(result) => {
                                let fingerprint = result.fingerprint.clone();
                                // Update both caches
                                self.fingerprint_cache
                                    .insert(query_hash, fingerprint.clone());
                                local_normalization_cache.insert(query_text, fingerprint.clone());
                                fingerprint
                            }
                            Err(_) => {
                                // Fallback to simple hash for malformed SQL
                                let fallback_fingerprint = format!("{:016x}", query_hash);
                                self.fingerprint_cache
                                    .insert(query_hash, fallback_fingerprint.clone());
                                local_normalization_cache
                                    .insert(query_text, fallback_fingerprint.clone());
                                fallback_fingerprint
                            }
                        }
                    }
                };

            query_groups.entry(fingerprint).or_default().push(idx);
        }

        // Build ProcessedQuery structs using indices to avoid cloning. With the
        // `parallel` feature this fans the per-group work out across rayon; in
        // the embeddable (single-threaded) build it runs serially. The per-group
        // work is identical, so it lives in `process_query_group`.
        #[cfg(feature = "parallel")]
        let processed_queries: HashMap<String, ProcessedQuery> = query_groups
            .into_par_iter()
            .filter_map(|(fingerprint, indices)| {
                Self::process_query_group(fingerprint, indices, plans)
            })
            .collect();
        #[cfg(not(feature = "parallel"))]
        let processed_queries: HashMap<String, ProcessedQuery> = query_groups
            .into_iter()
            .filter_map(|(fingerprint, indices)| {
                Self::process_query_group(fingerprint, indices, plans)
            })
            .collect();

        processed_queries
    }

    /// Aggregate a single fingerprint group into a [`ProcessedQuery`]. Pure and
    /// side-effect free so it can be driven by either a parallel or sequential
    /// iterator.
    fn process_query_group(
        fingerprint: String,
        indices: Vec<usize>,
        plans: &[QueryPlan],
    ) -> Option<(String, ProcessedQuery)> {
        let first_idx = indices[0];

        // Calculate statistics using indices
        let durations: Vec<f64> = indices.iter().map(|&i| plans[i].duration_ms()).collect();
        let total_duration: f64 = durations.iter().sum();
        let count = indices.len();
        let (mean_duration, std_dev) =
            QueryStatisticsCalculator::calculate_mean_and_std_dev(&durations);
        let (min_duration, max_duration) = QueryStatisticsCalculator::find_min_max(&durations);

        // Calculate timestamp range for this query group
        let timestamps: Vec<_> = indices.iter().map(|&i| plans[i].timestamp()).collect();
        // Safe: timestamps is non-empty because indices is non-empty (we have first_idx)
        let min_timestamp = *timestamps
            .iter()
            .min()
            .expect("timestamps vec is non-empty since indices is non-empty");
        let max_timestamp = *timestamps
            .iter()
            .max()
            .expect("timestamps vec is non-empty since indices is non-empty");

        // Find the slowest execution index
        // Use total_cmp for f64 to handle NaN safely (treats NaN as greater than all other values)
        let slowest_idx = indices
            .iter()
            .max_by(|&&a, &&b| plans[a].duration_ms().total_cmp(&plans[b].duration_ms()))
            .copied()
            .unwrap_or(first_idx);

        // SQL formatting is now done in QueryPlan construction

        // Create lightweight execution records instead of cloning full plans
        let executions: Vec<crate::models::ExecutionRecord> = indices
            .iter()
            .map(|&i| crate::models::ExecutionRecord {
                timestamp: plans[i].timestamp(),
                duration_ms: plans[i].duration_ms(),
            })
            .collect();

        // Calculate percentiles
        let percentiles = QueryStatisticsCalculator::calculate_percentiles(&durations);

        // Generate hourly histogram using the execution records
        let hourly_histogram = QueryStatisticsCalculator::generate_hourly_histogram(&executions);

        let statistics = QueryGroupStatistics {
            count,
            total_duration_ms: total_duration,
            min_duration_ms: min_duration,
            max_duration_ms: max_duration,
            mean_duration_ms: mean_duration,
            std_dev_ms: std_dev,
            min_timestamp,
            max_timestamp,
            percentiles,
            hourly_histogram,
            executions,
        };

        // Use the slowest execution as the representative plan
        let representative_plan = plans[slowest_idx].clone();

        // Skip Phase 2 analysis for now - make it lazy-loaded
        let processed_query = ProcessedQuery {
            representative_plan,
            statistics,
            complexity_score: None,
            metadata: None,
            regression_analysis: None,
            plan_analysis: None,
            execution_indices: indices,
        };

        Some((fingerprint, processed_query))
    }

    /// Clear the fingerprint cache to free memory
    pub fn clear_fingerprint_cache(&mut self) {
        self.fingerprint_cache.clear();
    }

    /// Get the size of the fingerprint cache
    pub fn fingerprint_cache_size(&self) -> usize {
        self.fingerprint_cache.len()
    }

    /// Analyze query complexity using AST-based scoring
    pub fn analyze_complexity(
        &self,
        plan: &QueryPlan,
    ) -> Option<crate::sql_analysis::ComplexityScore> {
        use crate::sql_analysis::ComplexityAnalyzer;

        let analyzer = ComplexityAnalyzer::new();
        analyzer.analyze(&plan.query_text).ok()
    }

    /// Extract comprehensive query metadata
    pub fn extract_metadata(&self, plan: &QueryPlan) -> Option<crate::sql_analysis::QueryMetadata> {
        use crate::sql_analysis::MetadataExtractor;

        let extractor = MetadataExtractor::new();
        extractor.extract(&plan.query_text).ok()
    }

    /// Analyze performance regression for this query group
    pub fn analyze_regression(
        &self,
        plans: &[&QueryPlan],
    ) -> Option<crate::sql_analysis::RegressionAnalysis> {
        use crate::sql_analysis::PerformanceDataPoint;

        // Convert QueryPlans to PerformanceDataPoints; the engine owns the size
        // thresholds and dispatch logic.
        let data: Vec<PerformanceDataPoint> = plans
            .iter()
            .map(|plan| PerformanceDataPoint {
                timestamp: plan.timestamp,
                execution_time_ms: plan.duration_ms,
                memory_usage_mb: None, // Would need to extract from plan if available
                cpu_usage_percent: None,
                io_operations: None,
                cache_hit_ratio: None,
            })
            .collect();

        crate::sql_analysis::default_regression_engine().analyze(&data)
    }
}

impl Default for PostgreSQLLogParser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_cache_evicts_least_recently_used() {
        let mut cache = FingerprintCache::with_capacity(2);
        cache.insert(1, "a".to_string());
        cache.insert(2, "b".to_string());
        // Touch key 1 so key 2 becomes the least-recently-used.
        assert_eq!(cache.get(1).as_deref(), Some("a"));
        // Inserting a third key evicts the LRU (key 2), not key 1.
        cache.insert(3, "c".to_string());
        assert_eq!(cache.get(2), None, "LRU entry must be evicted");
        assert_eq!(cache.get(1).as_deref(), Some("a"), "touched entry survives");
        assert_eq!(cache.get(3).as_deref(), Some("c"));
        assert_eq!(cache.len(), 2, "cache stays bounded at its capacity");
    }

    #[test]
    fn fingerprint_cache_reinsert_refreshes_without_growing() {
        let mut cache = FingerprintCache::with_capacity(2);
        cache.insert(1, "a".to_string());
        cache.insert(1, "a2".to_string()); // same key updates in place
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.get(1).as_deref(), Some("a2"));
    }

    #[test]
    fn fingerprint_cache_zero_cap_is_unbounded() {
        let mut cache = FingerprintCache::with_capacity(0);
        for i in 0..10 {
            cache.insert(i, format!("f{i}"));
        }
        assert_eq!(cache.len(), 10, "cap 0 disables eviction");
    }

    #[test]
    fn with_byte_limits_truncates_long_line() {
        // A single line far longer than a tiny per-line cap must be truncated
        // (no hang, no OOM); a well-formed entry after it still parses.
        let long = "x".repeat(5000);
        let log = format!(
            "2025-06-15 10:00:00.000 UTC [1] LOG:  {long}\n\
             2025-06-15 10:00:01.000 UTC [1] LOG:  duration: 5.0 ms  plan:\n\
             \tQuery Text: SELECT 1\n\
             \tResult  (cost=0.00..0.01 rows=1 width=4)\n\
             2025-06-15 10:00:02.000 UTC [1] LOG:  done\n"
        );
        let mut parser = PostgreSQLLogParser::new().with_byte_limits(64, 0);
        let plans = parser.parse_string_with_progress(&log, |_, _| {}).unwrap();
        assert_eq!(
            plans.len(),
            1,
            "the well-formed entry parses after a truncated long line"
        );
    }

    #[test]
    fn with_byte_limits_discards_oversized_entry() {
        // A plan entry whose continuation lines exceed a tiny per-entry cap is
        // discarded rather than accumulated without bound.
        let filler = "\tsome continuation line of plan text\n".repeat(50);
        let log = format!(
            "2025-06-15 10:00:00.000 UTC [1] LOG:  duration: 5.0 ms  plan:\n\
             \tQuery Text: SELECT 1\n{filler}\
             2025-06-15 10:00:02.000 UTC [1] LOG:  done\n"
        );
        let mut parser = PostgreSQLLogParser::new().with_byte_limits(0, 100);
        let plans = parser.parse_string_with_progress(&log, |_, _| {}).unwrap();
        assert_eq!(plans.len(), 0, "the oversized entry is discarded");
    }

    #[test]
    fn test_text_parsing_debug() {
        let log_content = r#"2025-06-12 00:00:16.915 UTC [3416548] LOG:  duration: 1242.373 ms  plan:
	Query Text: SELECT v."Id", v."EndDate", v."IsDismissed", v."Level", v."MachineModelName", v."PatientId", v."StartDate", v."VitalAlarmSourceTypeId", v."VitalAlarmTypeId"
	FROM "Shared"."VitalAlarms" AS v
	WHERE v."EndDate" IS NOT NULL AND NOT (v."IsDismissed") AND ((v."Level" > 66.0 AND v."Level" <= 99.0 AND v."EndDate" <= $1) OR (v."Level" <= 66.0 AND v."EndDate" <= $2))
	ORDER BY v."EndDate" DESC
	LIMIT $3
	Limit  (cost=0.43..599.04 rows=1000 width=56)
	  Output: "Id", "EndDate", "IsDismissed", "Level", "MachineModelName", "PatientId", "StartDate", "VitalAlarmSourceTypeId", "VitalAlarmTypeId"
	  ->  Index Scan Backward using "IX_VitalAlarms_EndDate" on "Shared"."VitalAlarms" v  (cost=0.43..95610.13 rows=159718 width=56)
	        Output: "Id", "EndDate", "IsDismissed", "Level", "MachineModelName", "PatientId", "StartDate", "VitalAlarmSourceTypeId", "VitalAlarmTypeId"
	        Index Cond: (v."EndDate" IS NOT NULL)
	        Filter: ((NOT v."IsDismissed") AND (((v."Level" > '66'::double precision) AND (v."Level" <= '99'::double precision) AND (v."EndDate" <= '2025-06-11 23:00:15.671506+00'::timestamp with time zone)) OR ((v."Level" <= '66'::double precision) AND (v."EndDate" <= '2025-06-11 23:30:15.671506+00'::timestamp with time zone))))
2025-06-12 00:00:17.053 UTC [3416726] LOG:  job 1002 (Compression Policy [1002]) exiting with success: execution time 3321.83 ms"#;

        let mut parser = PostgreSQLLogParser::new();

        match parser.parse_string_with_progress(log_content, |_progress, _count| {}) {
            Ok(plans) => {
                println!("Debug: Parsed {} plans", plans.len());

                for (i, plan) in plans.iter().enumerate() {
                    println!("Plan {}: ", i + 1);
                    println!("  Timestamp: {}", plan.timestamp());
                    println!("  Duration: {} ms", plan.duration_ms());
                    println!("  Query: {}", plan.query_text());
                    println!("  Is Text Plan: {}", plan.is_text_plan());
                    println!("  Is JSON Plan: {}", plan.is_json_plan());

                    if let Some((plan_text, plan_lines)) = plan.as_text_plan() {
                        println!("  Plan Lines: {}", plan_lines.len());
                        println!(
                            "  Plan Text preview: {}",
                            &plan_text[..100.min(plan_text.len())]
                        );
                    }
                }

                // Expect at least 1 plan
                assert!(
                    !plans.is_empty(),
                    "Should parse at least 1 plan, got {}",
                    plans.len()
                );

                let first_plan = &plans[0];
                assert!(
                    first_plan.is_text_plan(),
                    "First plan should be text format"
                );
                assert_eq!(first_plan.duration_ms(), 1242.373);
                assert!(first_plan.query_text().contains("SELECT v.\"Id\""));
            }
            Err(e) => {
                panic!("Parsing failed: {}", e);
            }
        }
    }

    #[test]
    fn test_json_plan_parsing_integration() {
        // Test JSON plan parsing with log format
        let json_log_content = r#"2025-01-15 10:30:00.123 UTC [12345] LOG:  duration: 150.5 ms  plan:
	Query Text: SELECT * FROM users WHERE id = $1
	[
	  {
	    "Plan": {
	      "Node Type": "Index Scan",
	      "Relation Name": "users",
	      "Schema": "public",
	      "Alias": "u",
	      "Startup Cost": 0.42,
	      "Total Cost": 8.44,
	      "Plan Rows": 1,
	      "Plan Width": 16,
	      "Index Name": "users_pkey",
	      "Index Cond": "(id = $1)",
	      "Output": ["id", "name", "email"]
	    }
	  }
	]
2025-01-15 10:30:01.123 UTC [12346] LOG:  some other log message"#;

        let mut parser = PostgreSQLLogParser::new();

        match parser.parse_string_with_progress(json_log_content, |_progress, _count| {}) {
            Ok(plans) => {
                println!("JSON Debug: Parsed {} plans", plans.len());

                if !plans.is_empty() {
                    let plan = &plans[0];
                    println!("  Is JSON Plan: {}", plan.is_json_plan());
                    println!("  Query: {}", plan.query_text());
                    println!("  Duration: {} ms", plan.duration_ms());

                    if let Some((_raw_json, parsed_json)) = plan.as_json_plan() {
                        println!("  JSON Details:");
                        println!("    Node Type: {}", parsed_json.plan.node_type);
                        println!("    Relation: {:?}", parsed_json.plan.relation_name);
                        println!("    Startup Cost: {}", parsed_json.plan.startup_cost);

                        // Test plan parser integration
                        if let Ok(parsed_plan) = parser.plan_parser.parse_query_plan(plan) {
                            println!("    Parsed to PlanNode successfully!");
                            println!("    Root node: {}", parsed_plan.root.description());
                        }
                    }

                    assert!(plan.is_json_plan(), "Should be JSON plan");
                    assert_eq!(plan.duration_ms(), 150.5);
                    assert!(plan.query_text().contains("SELECT * FROM users"));
                } else {
                    println!("Warning: No plans parsed from JSON content");
                }
            }
            Err(e) => {
                println!("JSON parsing failed: {}", e);
            }
        }
    }

    #[test]
    fn test_real_auto_explain_json_object_format() {
        // auto_explain.log_format=json emits a top-level OBJECT with the
        // query embedded as a "Query Text" key (no "Query Text:" text line).
        let log = "2025-01-15 10:30:00.123 UTC [12345] LOG:  duration: 150.5 ms  plan:\n\t{\n\t  \"Query Text\": \"SELECT * FROM users WHERE id = 1\",\n\t  \"Plan\": {\n\t    \"Node Type\": \"Index Scan\",\n\t    \"Relation Name\": \"users\",\n\t    \"Startup Cost\": 0.42,\n\t    \"Total Cost\": 8.44,\n\t    \"Plan Rows\": 1,\n\t    \"Plan Width\": 16\n\t  }\n\t}\n2025-01-15 10:30:01.123 UTC [12346] LOG:  some other log message\n";
        let mut parser = PostgreSQLLogParser::new();
        let plans = parser.parse_string_with_progress(log, |_, _| {}).unwrap();

        assert_eq!(plans.len(), 1, "real auto_explain JSON entry must parse");
        assert!(plans[0].is_json_plan());
        assert_eq!(plans[0].duration_ms(), 150.5);
        assert_eq!(plans[0].query_text(), "SELECT * FROM users WHERE id = 1");
    }

    #[test]
    fn test_json_object_entry_terminated_by_eof() {
        // Same as above but the log ends right after the entry (no trailing
        // log line): the finalize path must extract the query text too.
        let log = "2025-01-15 10:30:00.123 UTC [12345] LOG:  duration: 99.0 ms  plan:\n\t{\n\t  \"Query Text\": \"SELECT 1\",\n\t  \"Plan\": {\n\t    \"Node Type\": \"Result\",\n\t    \"Startup Cost\": 0.0,\n\t    \"Total Cost\": 0.01,\n\t    \"Plan Rows\": 1,\n\t    \"Plan Width\": 4\n\t  }\n\t}\n";
        let mut parser = PostgreSQLLogParser::new();
        let plans = parser.parse_string_with_progress(log, |_, _| {}).unwrap();

        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].query_text(), "SELECT 1");
    }

    #[test]
    fn test_json_literal_in_query_text_does_not_drop_entry() {
        // A multi-line query containing a JSON literal at start-of-line used
        // to flip the parser into JSON mode and silently drop the entry.
        let log = "2025-01-15 10:30:00.123 UTC [12345] LOG:  duration: 150.5 ms  plan:\n\tQuery Text: SELECT * FROM events WHERE payload @> '\n\t{\"status\": \"active\"}'\n\tSeq Scan on events  (cost=0.00..35.50 rows=10 width=100)\n\t  Filter: (payload @> '{\"status\": \"active\"}'::jsonb)\n2025-01-15 10:30:01.123 UTC [12346] LOG:  some other log message\n";
        let mut parser = PostgreSQLLogParser::new();
        let plans = parser.parse_string_with_progress(log, |_, _| {}).unwrap();

        assert_eq!(plans.len(), 1, "entry with JSON literal in query dropped");
        assert!(plans[0].is_text_plan());
        assert!(
            plans[0].query_text().contains("{\"status\": \"active\"}"),
            "JSON literal must remain part of the query text: {:?}",
            plans[0].query_text()
        );
        assert!(plans[0].plan_text().contains("Seq Scan on events"));
    }

    #[cfg(feature = "file-io")]
    #[test]
    fn test_multi_member_gzip_parses_all_members() {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use std::io::Write as _;

        let entry = |ts: &str| {
            format!(
                "2025-01-15 {ts} UTC [1] LOG:  duration: 10.0 ms  plan:\n\tQuery Text: SELECT 1\n\tResult  (cost=0.00..0.01 rows=1 width=4)\n2025-01-15 {ts} UTC [1] LOG:  filler\n"
            )
        };

        // Two independently-gzipped members concatenated, as produced by
        // `cat a.gz b.gz`, pigz, or bgzip.
        let mut bytes = Vec::new();
        for member in [entry("10:00:00.000"), entry("11:00:00.000")] {
            let mut enc = GzEncoder::new(Vec::new(), Compression::default());
            enc.write_all(member.as_bytes()).unwrap();
            bytes.extend(enc.finish().unwrap());
        }

        let dir = std::env::temp_dir().join(format!("plansight-gz-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("multi.log.gz");
        std::fs::write(&path, &bytes).unwrap();

        let mut parser = PostgreSQLLogParser::new();
        let plans = parser.parse_file_with_progress(&path, |_, _| {}).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(
            plans.len(),
            2,
            "both gzip members must be decoded (single-member decoder stops at the first)"
        );
    }
}
