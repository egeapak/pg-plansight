use anyhow::Context as _;
use bzip2::read::BzDecoder;
use flate2::read::GzDecoder;
use hashbrown::HashMap;
use rayon::prelude::*;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use crate::models::{ParseProgress, ParsingState, ProcessedQuery, QueryGroupStatistics, QueryPlan};

use crate::PlanLine;
use crate::parser_utils::{
    QueryStatisticsCalculator, RegexPatterns, calculate_query_hash, format_sql_query,
    normalize_query, parse_duration_from_line, parse_timestamp,
};

mod magic_number {
    pub const GZIP: [u8; 2] = [0x1f, 0x8b];
    pub const BZIP2: [u8; 3] = [0x42, 0x5a, 0x68]; // "BZh"
}

#[derive(Debug)]
pub struct PostgreSQLLogParser {
    pub regex_patterns: RegexPatterns,
    pub query_cache: HashMap<u64, ProcessedQuery>,
    line_buffer: String,
}

impl PostgreSQLLogParser {
    pub fn new() -> Self {
        Self {
            regex_patterns: RegexPatterns::default(),
            query_cache: HashMap::with_capacity(100),
            line_buffer: String::with_capacity(1024),
        }
    }

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
            let decoder = GzDecoder::new(file);
            let reader = BufReader::with_capacity(64 * 1024, decoder);
            Ok((Box::new(reader), file_size))
        } else if is_bzip2 {
            let decoder = BzDecoder::new(file);
            let reader = BufReader::with_capacity(64 * 1024, decoder);
            Ok((Box::new(reader), file_size))
        } else {
            let reader = BufReader::with_capacity(64 * 1024, file);
            Ok((Box::new(reader), file_size))
        }
    }

    pub fn parse_file_with_progress<P: AsRef<Path>, F>(
        &mut self,
        file_path: P,
        mut progress_callback: F,
    ) -> anyhow::Result<Vec<QueryPlan>>
    where
        F: FnMut(f64),
    {
        let (mut reader, total_size) = Self::create_reader(&file_path)?;
        let total_size = total_size as f64;
        let mut query_plans = Vec::with_capacity(2000);
        let mut plan_lines = Vec::with_capacity(50);
        let mut parsing_state = ParsingState::None;
        let mut line_count = 0u64;
        let mut bytes_processed = 0u64;

        loop {
            self.line_buffer.clear();
            let bytes_read = reader.read_line(&mut self.line_buffer)?;

            if bytes_read == 0 {
                break; // EOF
            }

            line_count += 1;
            bytes_processed += bytes_read as u64;

            // Update progress every 10000 lines for better performance
            if line_count % 10000 == 0 {
                let progress = (bytes_processed as f64 / total_size).min(1.0);
                progress_callback(progress);
            }

            // Remove trailing newline in place
            let line_trimmed = self.line_buffer.trim_end();

            if let Some(captures) = self.regex_patterns.log_line_regex.captures(line_trimmed) {
                let timestamp_str = captures.get(1).unwrap().as_str();
                let message = captures.get(2).unwrap().as_str();

                // Check for "duration: X ms plan:" which starts auto_explain output
                if let Some(duration) =
                    parse_duration_from_line(message, &self.regex_patterns.duration_regex)
                {
                    let timestamp = parse_timestamp(timestamp_str)
                        .with_context(|| format!("Can't parse timestamp: '{}'", timestamp_str))?;

                    let new_plan = QueryPlan {
                        timestamp,
                        duration_ms: duration,
                        query_text: String::new(),
                        plan: String::new(),
                    };

                    if let Some(current_plan) = parsing_state.reset(new_plan) {
                        query_plans.push(current_plan.finalize(&plan_lines));
                    }

                    plan_lines.clear();
                }
                // Any other log line with timestamp ends the current parsing
                else if let Some(plan) = parsing_state.finish() {
                    query_plans.push(plan.finalize(&plan_lines));
                }
            } else {
                // Handle continuation lines (lines that don't match the log format)
                let trimmed = line_trimmed.trim();

                if trimmed.is_empty() {
                    continue;
                }

                parsing_state = match parsing_state {
                    ParsingState::WaitingForQuery(mut plan) => {
                        if let Some(query_text) = trimmed.strip_prefix("Query Text:") {
                            plan.query_text = query_text.to_string();
                            ParsingState::ParsingQuery(plan)
                        } else {
                            ParsingState::WaitingForQuery(plan)
                        }
                    }
                    ParsingState::ParsingQuery(mut plan) => {
                        if self.regex_patterns.plan_regex.is_match(trimmed) {
                            let plan_line = PlanLine::new(line_trimmed);
                            plan_lines.push(plan_line);
                            ParsingState::ParsingPlan(plan)
                        } else {
                            if !plan.query_text.is_empty() {
                                plan.query_text.push('\n');
                            }
                            plan.query_text.push_str(line_trimmed);
                            ParsingState::ParsingQuery(plan)
                        }
                    }
                    ParsingState::ParsingPlan(plan) => {
                        let plan_line = PlanLine::new(line_trimmed);
                        plan_lines.push(plan_line);
                        ParsingState::ParsingPlan(plan)
                    }
                    state => state,
                };
            }
        }

        // Handle any remaining plan
        if let Some(plan) = parsing_state.finish() {
            query_plans.push(plan.finalize(&plan_lines));
        }

        // Final progress update
        progress_callback(1.0);

        Ok(query_plans)
    }

    pub fn parse_multiple_files_async(file_paths: Vec<PathBuf>) -> mpsc::Receiver<ParseProgress> {
        let (tx, rx) = mpsc::channel();

        thread::spawn(move || {
            let results: Vec<_> = file_paths
                .par_iter()
                .enumerate()
                .map(|(file_index, file_path)| {
                    let tx = tx.clone();

                    // Create a new parser instance for each thread
                    let mut thread_parser = PostgreSQLLogParser::new();

                    match thread_parser.parse_file_with_progress(file_path, |progress| {
                        let _ = tx.send(ParseProgress::Progress {
                            file_index,
                            file_path: file_path.clone(),
                            progress,
                        });
                    }) {
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

            // Collect successful results
            let mut all_query_plans = Vec::new();
            for result in results {
                match result {
                    Ok((_, mut plans)) => {
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

    pub fn get_processed_queries(&mut self, plans: &[QueryPlan]) -> HashMap<u64, ProcessedQuery> {
        // Group plans by hash in a single pass
        let mut query_groups: HashMap<u64, Vec<usize>> = HashMap::new();
        let mut normalized_queries: HashMap<u64, String> = HashMap::new();

        for (idx, plan) in plans.iter().enumerate() {
            let normalized =
                normalize_query(&plan.query_text, &self.regex_patterns.placeholder_regex);
            let hash = calculate_query_hash(&normalized);
            query_groups.entry(hash).or_default().push(idx);

            // Only store normalized query once per hash
            if !normalized_queries.contains_key(&hash) {
                normalized_queries.insert(hash, normalized.to_string());
            }
        }

        // Build ProcessedQuery structs using indices to avoid cloning
        let mut processed_queries = HashMap::new();

        for (hash, indices) in query_groups {
            if let Some(normalized_query) = normalized_queries.get(&hash) {
                let first_idx = indices[0];
                let first_plan = &plans[first_idx];

                // Calculate statistics using indices
                let durations: Vec<f64> = indices.iter().map(|&i| plans[i].duration_ms).collect();
                let total_duration: f64 = durations.iter().sum();
                let count = indices.len();
                let (mean_duration, std_dev) =
                    QueryStatisticsCalculator::calculate_mean_and_std_dev(&durations);
                let (min_duration, max_duration) =
                    QueryStatisticsCalculator::find_min_max(&durations);

                // Find the slowest execution index
                let slowest_idx = indices
                    .iter()
                    .max_by(|&&a, &&b| {
                        plans[a]
                            .duration_ms
                            .partial_cmp(&plans[b].duration_ms)
                            .unwrap()
                    })
                    .copied()
                    .unwrap_or(first_idx);

                // Format SQL
                let formatted_query = format_sql_query(&first_plan.query_text);

                // Only clone the executions we need
                let executions: Vec<QueryPlan> =
                    indices.iter().map(|&i| plans[i].clone()).collect();

                let statistics = QueryGroupStatistics {
                    count,
                    total_duration_ms: total_duration,
                    min_duration_ms: min_duration,
                    max_duration_ms: max_duration,
                    mean_duration_ms: mean_duration,
                    std_dev_ms: std_dev,
                    executions,
                };

                let processed_query = ProcessedQuery {
                    original_query: first_plan.query_text.clone(),
                    plan: plans[slowest_idx].plan.clone(),
                    normalized_query: normalized_query.clone(),
                    formatted_query,
                    statistics,
                };

                processed_queries.insert(hash, processed_query);
            }
        }

        // Cache the results
        self.query_cache = processed_queries.clone();
        processed_queries
    }
}

impl Default for PostgreSQLLogParser {
    fn default() -> Self {
        Self::new()
    }
}
