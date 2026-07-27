use anyhow::Context as _;
use hashbrown::HashMap;
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
use crate::models::{DateFilter, GroupedPlans, ParseProgress};
#[cfg(feature = "parallel")]
use rayon::prelude::*;
#[cfg(all(feature = "parallel", feature = "file-io"))]
use std::path::PathBuf;
#[cfg(all(feature = "parallel", feature = "file-io"))]
use std::sync::mpsc;
#[cfg(all(feature = "parallel", feature = "file-io"))]
use std::thread;

use crate::grouping::{
    FingerprintCache, MAX_FINGERPRINT_CACHE_ENTRIES, QueryGrouper, finalize_group, fingerprint_for,
};
use crate::models::{ProcessedQuery, QueryPlan};
use crate::parsing::{LogParsingState as ParsingState, QueryPlanBuilder};

use crate::parser_utils::{
    RegexPatterns, TimezoneResolver, parse_duration_from_line, parse_timestamp_with_tz,
    split_log_line,
};
use crate::plan_parser::PlanParser;

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

/// Retained-plan count at which [`PostgreSQLLogParser::parse_with_progress`]
/// warns.
///
/// Only the materializing path needs this: it holds every plan of the run, at
/// roughly 4.4 KB each. The streaming path's equivalent is
/// [`GROUP_RETENTION_WARN_THRESHOLD`](crate::grouping::GROUP_RETENTION_WARN_THRESHOLD),
/// which counts retained *representatives* instead.
const PLAN_RETENTION_WARN_THRESHOLD: usize = 500_000;

/// Where the read loop delivers each completed plan.
///
/// Two implementations matter: `Vec<QueryPlan>`, which retains everything, and
/// [`QueryGrouper`], which folds each plan into its group and drops it. The
/// parser is generic over this so both share one read loop.
pub trait PlanSink {
    /// Take ownership of a completed plan.
    fn accept(&mut self, plan: QueryPlan);
    /// How many plans have been accepted so far (drives progress reporting).
    fn accepted(&self) -> usize;
    /// True when the sink will accept no more, so the read loop can stop early.
    fn is_full(&self) -> bool {
        false
    }
    /// Called when the read loop stops early with input still unread, so a
    /// bounded sink can report that plans were dropped. Stopping early is what
    /// makes the cap cheap; it also means `accept` is never offered the plan
    /// that would have exceeded the cap, so the sink cannot notice on its own.
    fn mark_truncated(&mut self) {}
}

impl PlanSink for Vec<QueryPlan> {
    fn accept(&mut self, plan: QueryPlan) {
        self.push(plan);
    }

    fn accepted(&self) -> usize {
        self.len()
    }
}

impl PlanSink for QueryGrouper {
    fn accept(&mut self, plan: QueryPlan) {
        self.fold(plan);
    }

    fn accepted(&self) -> usize {
        QueryGrouper::accepted(self)
    }

    fn is_full(&self) -> bool {
        QueryGrouper::is_full(self)
    }

    fn mark_truncated(&mut self) {
        QueryGrouper::mark_truncated(self);
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

    /// Parse `reader`, delivering each completed plan to `sink`.
    ///
    /// This is the single read loop behind both the materializing path
    /// ([`parse_with_progress`](Self::parse_with_progress), whose sink is a
    /// `Vec<QueryPlan>`) and the streaming path
    /// ([`parse_into_grouper`](Self::parse_into_grouper), whose sink folds each
    /// plan into its group and drops it). Keeping one loop means the hostile-
    /// input handling — line caps, entry caps, invalid UTF-8, unparseable
    /// timestamps — cannot diverge between them.
    pub fn parse_into_sink<R: BufRead, F, S: PlanSink>(
        &mut self,
        mut reader: R,
        total_size: u64,
        mut progress_callback: F,
        sink: &mut S,
    ) -> anyhow::Result<()>
    where
        F: FnMut(f64, usize),
    {
        let total_size = total_size as f64;
        // Copy the byte caps out so the read loop can reference them while
        // `self.byte_buffer` is mutably borrowed.
        let max_line_bytes = self.max_line_bytes;
        let max_entry_bytes = self.max_entry_bytes;
        let mut parsing_state = ParsingState::None;
        let mut line_count = 0u64;
        let mut matched_log_lines = 0u64;
        let mut bytes_processed = 0u64;
        // Start from what the sink already holds: a caller may fold several
        // ranges into one grouper, and starting at 0 would report that
        // pre-existing count as this parse's first delta.
        let mut plans_processed = sink.accepted();
        let mut entry_bytes = 0u64;

        loop {
            // A bounded sink (the exporter's per-file query cap) stops the read
            // here rather than after materializing everything and slicing.
            if sink.is_full() {
                // Report that the read stopped short of the input, not that a
                // plan was definitely dropped: a plan only reaches the sink when
                // the *next* timestamped line is read, so the pending bytes may
                // hold no further plan at all. What is certain — and what the
                // caller needs to know — is that this range was not read to the
                // end while its checkpoint advances past it regardless.
                // Reaching the cap exactly at end of input reports nothing.
                if !reader
                    .fill_buf()
                    .context("Failed to check for pending input")?
                    .is_empty()
                {
                    sink.mark_truncated();
                }
                break;
            }
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

            // Validate the read buffer with simdutf8's SIMD fast path (the common
            // case: valid UTF-8, borrowed with no copy). On the rare invalid
            // input, fall back to from_utf8_lossy so bad bytes become U+FFFD —
            // replacing, not truncating: truncating to the valid prefix drops
            // query/plan content after the first bad byte and corrupts fingerprints.
            let slice: std::borrow::Cow<str> = match simdutf8::basic::from_utf8(&self.byte_buffer) {
                Ok(s) => std::borrow::Cow::Borrowed(s),
                Err(_) => String::from_utf8_lossy(&self.byte_buffer),
            };

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
                let current_len = sink.accepted();
                let delta = current_len.saturating_sub(plans_processed);
                plans_processed = current_len;
                let progress = (bytes_processed as f64 / total_size).min(1.0);
                progress_callback(progress, delta);
            }

            // Remove trailing newline in place
            let line_trimmed = slice.trim_end();

            if let Some((timestamp_str, message)) =
                split_log_line(line_trimmed, &self.regex_patterns.log_line_regex)
            {
                matched_log_lines += 1;

                // Check for "duration: X ms plan:" which starts auto_explain output
                if let Some(duration) =
                    parse_duration_from_line(message, &self.regex_patterns.duration_regex)
                {
                    // A timestamp that matches the line shape but is not a real
                    // calendar date (e.g. month 13) must not abort the whole
                    // parse; treat the line as a plan-terminating boundary. The
                    // timezone resolver honors an ambiguous log_timezone override.
                    match parse_timestamp_with_tz(timestamp_str, &self.timezone) {
                        Ok(timestamp) => {
                            let new_builder = QueryPlanBuilder::new(timestamp, duration);
                            if let Some(current_plan) =
                                parsing_state.reset_with_builder(new_builder)
                            {
                                sink.accept(current_plan);
                            }
                        }
                        Err(e) => {
                            warn!(
                                timestamp = timestamp_str,
                                error = %e,
                                "Skipping log line with invalid timestamp"
                            );
                            if let Some(plan) = parsing_state.finish() {
                                sink.accept(plan);
                            }
                        }
                    }
                }
                // Any other log line with timestamp ends the current parsing
                else if let Some(plan) = parsing_state.finish() {
                    sink.accept(plan);
                }
            } else {
                // Continuation line (does not match the log format). Delegate to
                // the shared state machine (LogParsingState::advance_continuation),
                // the single implementation used by both this streaming parser
                // and the extension's LogEntryParser. The streaming parser is
                // tolerant of a single malformed text plan: warn and continue.
                if line_trimmed.trim().is_empty() {
                    continue;
                }
                let outcome = std::mem::replace(&mut parsing_state, ParsingState::None)
                    .advance_continuation(line_trimmed, &self.regex_patterns.plan_regex);
                parsing_state = outcome.state;
                if let Some(e) = outcome.error {
                    warn!(error = %e, "Text plan parsing error");
                }
                if let Some(plan) = outcome.plan {
                    sink.accept(plan);
                }
            }
        }

        // Handle any remaining plan
        if let Some(plan) = parsing_state.finish() {
            sink.accept(plan);
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

        Ok(())
    }

    /// Parse `reader` and return every plan.
    ///
    /// Peak memory is linear in the number of plans (~4.4 KB each), because
    /// nothing is grouped until the whole reader is consumed. Prefer
    /// [`parse_into_grouper`](Self::parse_into_grouper) for anything that will
    /// end up grouped anyway; this stays for callers that genuinely need the
    /// individual plans (tests, benchmarks, format-level inspection).
    pub fn parse_with_progress<R: BufRead, F>(
        &mut self,
        reader: R,
        total_size: u64,
        progress_callback: F,
    ) -> anyhow::Result<Vec<QueryPlan>>
    where
        F: FnMut(f64, usize),
    {
        let mut plans = Vec::with_capacity(2000);
        self.parse_into_sink(reader, total_size, progress_callback, &mut plans)?;
        if plans.len() >= PLAN_RETENTION_WARN_THRESHOLD {
            warn!(
                retained_plans = plans.len(),
                "Holding a very large number of parsed plans in memory. This path retains \
                 every plan (roughly 4.4 KB each) until the caller groups them. Use \
                 parse_into_grouper to fold each plan into its group as it is parsed, or \
                 narrow the window with --since/--until."
            );
        }
        Ok(plans)
    }

    /// Parse `reader`, folding each plan into `grouper` and dropping it.
    ///
    /// Peak memory is bounded by the number of distinct query fingerprints plus
    /// 24 bytes per execution, rather than by the number of executions times the
    /// size of a plan.
    pub fn parse_into_grouper<R: BufRead, F>(
        &mut self,
        reader: R,
        total_size: u64,
        progress_callback: F,
        grouper: &mut QueryGrouper,
    ) -> anyhow::Result<()>
    where
        F: FnMut(f64, usize),
    {
        self.parse_into_sink(reader, total_size, progress_callback, grouper)
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

    /// Streaming counterpart of [`parse_file_with_progress`](Self::parse_file_with_progress).
    #[cfg(feature = "file-io")]
    pub fn parse_file_into_grouper<P: AsRef<Path>, F>(
        &mut self,
        file_path: P,
        progress_callback: F,
        grouper: &mut QueryGrouper,
    ) -> anyhow::Result<()>
    where
        F: FnMut(f64, usize),
    {
        let (reader, total_size) = Self::create_reader(&file_path)?;
        self.parse_into_grouper(reader, total_size, progress_callback, grouper)
    }

    /// Streaming counterpart of
    /// [`parse_file_range_with_progress`](Self::parse_file_range_with_progress).
    #[cfg(feature = "file-io")]
    pub fn parse_file_range_into_grouper<P: AsRef<Path>, F>(
        &mut self,
        file_path: P,
        start_offset: u64,
        end_offset: Option<u64>,
        progress_callback: F,
        grouper: &mut QueryGrouper,
    ) -> anyhow::Result<()>
    where
        F: FnMut(f64, usize),
    {
        let (reader, effective_size) =
            Self::create_reader_with_range(&file_path, start_offset, end_offset)?;
        self.parse_into_grouper(reader, effective_size, progress_callback, grouper)
    }

    /// Streaming counterpart of
    /// [`parse_string_with_progress`](Self::parse_string_with_progress).
    pub fn parse_string_into_grouper<F>(
        &mut self,
        content: &str,
        progress_callback: F,
        grouper: &mut QueryGrouper,
    ) -> anyhow::Result<()>
    where
        F: FnMut(f64, usize),
    {
        let reader = std::io::Cursor::new(content.as_bytes());
        let content_size = content.len() as u64;
        self.parse_into_grouper(reader, content_size, progress_callback, grouper)
    }

    /// Parse every file in parallel, folding results into per-file groupers and
    /// merging them in file order.
    ///
    /// `date_filter` is applied *during* the fold, so a plan outside the window
    /// is never retained. Previously each file was materialized in full and
    /// filtered afterwards, which meant `--since` narrowed the results without
    /// narrowing peak memory at all.
    #[cfg(all(feature = "parallel", feature = "file-io"))]
    pub fn parse_multiple_files_async(
        file_paths: Vec<PathBuf>,
        date_filter: DateFilter,
    ) -> mpsc::Receiver<ParseProgress> {
        let (tx, rx) = mpsc::channel();

        thread::spawn(move || {
            // `reduce` folds adjacent results as workers finish, so only the
            // in-flight set is alive at once. Collecting every file's grouper
            // first and merging afterwards would multiply the retained
            // representatives by the file count — the very thing this change
            // exists to avoid.
            //
            // This is sound because merging is associative: executions
            // concatenate in order, and the representative is a max under
            // "last wins on ties" over that concatenation, so folding adjacent
            // ranges in any grouping gives the same answer as one left-to-right
            // pass. Rayon only ever combines adjacent ranges, which is what
            // makes file order — and therefore the representative — well
            // defined. An empty grouper is the identity.
            let merged = file_paths
                .par_iter()
                .enumerate()
                .map(|(file_index, file_path)| {
                    let tx = tx.clone();

                    // Create a new parser instance for each thread
                    let mut thread_parser = PostgreSQLLogParser::new();
                    let mut grouper = QueryGrouper::new().with_filter(date_filter.clone());

                    match thread_parser.parse_file_into_grouper(
                        file_path,
                        |progress, queries_parsed| {
                            let _ = tx.send(ParseProgress::Progress {
                                file_index,
                                file_path: file_path.clone(),
                                progress,
                                queries_parsed,
                            });
                        },
                        &mut grouper,
                    ) {
                        Ok(()) => grouper,
                        Err(e) => {
                            let _ = tx.send(ParseProgress::Error {
                                file_index,
                                file_path: file_path.clone(),
                                error: format!("Parse error: {}", e),
                            });
                            // A failed file contributes nothing; an empty
                            // grouper is the reduction's identity, so the
                            // remaining files still merge in order.
                            QueryGrouper::new()
                        }
                    }
                })
                .reduce(QueryGrouper::new, |mut acc, grouper| {
                    acc.merge(grouper);
                    acc
                });

            let plan_count = merged.accepted();
            let groups = merged.finish();

            // Send final result and close channel
            let _ = tx.send(ParseProgress::Complete {
                result: Ok(GroupedPlans { plan_count, groups }),
            });
        });

        rx
    }

    /// Group a fully materialized slice of plans.
    ///
    /// Retained for callers that already hold every plan. Anything parsing from
    /// a reader should use [`parse_into_grouper`](Self::parse_into_grouper)
    /// instead, which never materializes them. The two produce identical
    /// output — `streamed_grouping_matches_batched` asserts it.
    pub fn get_processed_queries(
        &mut self,
        plans: &[QueryPlan],
    ) -> HashMap<String, ProcessedQuery> {
        // Group plans by fingerprint using enhanced normalization.
        // Pre-size from the plan count to avoid repeated rehashing on large logs.
        let mut query_groups: HashMap<String, Vec<usize>> = HashMap::with_capacity(plans.len());
        // Local cache for this batch (most useful since many plans have the same
        // query text within a batch). It is bounded by the caller-supplied slice,
        // which is already resident — unlike the streaming path, where raw query
        // texts are unbounded and only the LRU may be consulted.
        let mut local_normalization_cache: HashMap<&str, String> =
            HashMap::with_capacity(plans.len());

        for (idx, plan) in plans.iter().enumerate() {
            let query_text = plan.query_text();
            let fingerprint =
                if let Some(cached_fingerprint) = local_normalization_cache.get(query_text) {
                    cached_fingerprint.clone()
                } else {
                    let fingerprint = fingerprint_for(&mut self.fingerprint_cache, query_text);
                    local_normalization_cache.insert(query_text, fingerprint.clone());
                    fingerprint
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
            .map(|(fingerprint, indices)| Self::process_query_group(fingerprint, indices, plans))
            .collect();
        #[cfg(not(feature = "parallel"))]
        let processed_queries: HashMap<String, ProcessedQuery> = query_groups
            .into_iter()
            .map(|(fingerprint, indices)| Self::process_query_group(fingerprint, indices, plans))
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
    ) -> (String, ProcessedQuery) {
        let first_idx = indices[0];
        let count = indices.len();

        // One fused pass over the group: build lightweight execution records
        // (instead of cloning full plans) while tracking the timestamp range
        // and the slowest execution. `is_ge` makes the LAST maximum win on
        // duration ties, matching Iterator::max_by; total_cmp handles NaN
        // safely (treats NaN as greater than all other values).
        let mut executions: Vec<crate::models::ExecutionRecord> = Vec::with_capacity(count);
        let mut min_timestamp = plans[first_idx].timestamp();
        let mut max_timestamp = min_timestamp;
        let mut slowest_idx = first_idx;
        let mut slowest_duration = plans[first_idx].duration_ms();
        for &i in &indices {
            let timestamp = plans[i].timestamp();
            let duration_ms = plans[i].duration_ms();
            if timestamp < min_timestamp {
                min_timestamp = timestamp;
            }
            if timestamp > max_timestamp {
                max_timestamp = timestamp;
            }
            if duration_ms.total_cmp(&slowest_duration).is_ge() {
                slowest_idx = i;
                slowest_duration = duration_ms;
            }
            executions.push(crate::models::ExecutionRecord {
                timestamp,
                duration_ms,
            });
        }

        let processed_query = finalize_group(
            plans[slowest_idx].clone(),
            executions,
            min_timestamp,
            max_timestamp,
        );

        (fingerprint, processed_query)
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

    /// Analyze performance regression for one query group.
    ///
    /// Takes the group's [`ExecutionRecord`]s rather than the plans they came
    /// from: timestamp and duration are the only fields the regression engine
    /// reads, and requiring plans forced every execution's plan to be kept alive
    /// purely so this could be called later.
    pub fn analyze_regression(
        &self,
        executions: &[crate::models::ExecutionRecord],
    ) -> Option<crate::sql_analysis::RegressionAnalysis> {
        use crate::sql_analysis::PerformanceDataPoint;

        // The engine owns the size thresholds and dispatch logic.
        let data: Vec<PerformanceDataPoint> = executions
            .iter()
            .map(|execution| PerformanceDataPoint {
                timestamp: execution.timestamp,
                execution_time_ms: execution.duration_ms,
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
    fn test_invalid_utf8_mid_stream_replaced_not_truncated() {
        // A plan line with invalid UTF-8 bytes mid-line: the parser replaces the
        // bad bytes with U+FFFD and keeps the rest of the line — replacing, not
        // truncating. Truncating to the valid prefix would drop the query/plan
        // content after the first bad byte and corrupt fingerprints. The parse
        // must not fail, and the following valid line must still parse.
        let mut content = Vec::new();
        content.extend_from_slice(
            b"2025-06-12 00:00:16.915 UTC [3416548] LOG:  duration: 1242.373 ms  plan:\n",
        );
        content.extend_from_slice(b"\tQuery Text: SELECT * FROM users WHERE id = $1\n");
        content.extend_from_slice(b"\tLimit  (cost=0.43..599.04 rows=1000 width=56)\n");
        content.extend_from_slice(b"\t  Filter: (id = abc\xFF\xFEdef)\n");
        content.extend_from_slice(b"\t  Output: \"Id\"\n");
        content.extend_from_slice(
            b"2025-06-12 00:00:17.053 UTC [3416726] LOG:  checkpoint complete\n",
        );

        let mut parser = PostgreSQLLogParser::new();
        let plans = parser
            .parse_bytes_with_progress(&content, |_, _| {})
            .expect("invalid UTF-8 mid-stream must not fail the parse");

        assert_eq!(plans.len(), 1);
        let (plan_text, _) = plans[0].as_text_plan().expect("should be a text plan");
        // Text on BOTH sides of the invalid bytes is preserved, with U+FFFD in
        // between — nothing after the bad byte is dropped.
        assert!(plan_text.contains("Filter: (id = abc"));
        assert!(plan_text.contains("def)"));
        assert!(
            plan_text.contains('\u{FFFD}'),
            "invalid bytes must become the U+FFFD replacement char"
        );
        // The following (valid) line is still parsed normally.
        assert!(plan_text.contains("Output: \"Id\""));
    }

    #[test]
    fn test_invalid_calendar_date_does_not_abort_parse() {
        // The middle line matches the timestamp shape but is not a real date
        // (month 13); it must be skipped without losing the surrounding plans.
        let log_content = "2025-06-12 00:00:16.915 UTC [1] LOG:  duration: 100.0 ms  plan:\n\
\tQuery Text: SELECT * FROM a\n\
\tSeq Scan on a  (cost=0.00..1.00 rows=1 width=8)\n\
2025-13-01 00:00:17.000 UTC [1] LOG:  duration: 50.0 ms  plan:\n\
2025-06-12 00:00:18.915 UTC [1] LOG:  duration: 200.0 ms  plan:\n\
\tQuery Text: SELECT * FROM b\n\
\tSeq Scan on b  (cost=0.00..1.00 rows=1 width=8)\n\
2025-06-12 00:00:19.000 UTC [1] LOG:  checkpoint complete\n";

        let mut parser = PostgreSQLLogParser::new();
        let plans = parser
            .parse_string_with_progress(log_content, |_, _| {})
            .expect("invalid calendar date must not abort the parse");

        assert_eq!(plans.len(), 2, "both valid plans should be parsed");
        assert_eq!(plans[0].duration_ms(), 100.0);
        assert_eq!(plans[1].duration_ms(), 200.0);
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
