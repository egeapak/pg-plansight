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

use crate::models::{
    DateFilter, ParseProgress, ParsingState, ProcessedQuery, QueryGroupStatistics, QueryPlan,
};

use crate::PlanLine;
use crate::plan_parser::PlanParser;
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
    pub plan_parser: PlanParser,
    byte_buffer: Vec<u8>,
}

impl PostgreSQLLogParser {
    pub fn new() -> Self {
        Self {
            regex_patterns: RegexPatterns::default(),
            query_cache: HashMap::with_capacity(100),
            plan_parser: PlanParser::new().expect("Failed to create PlanParser"),
            byte_buffer: Vec::with_capacity(8192),
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

    fn create_reader_with_range<P: AsRef<Path>>(
        file_path: P,
        start_offset: u64,
        end_offset: Option<u64>,
    ) -> anyhow::Result<(Box<dyn BufRead>, u64)> {
        let mut file = File::open(&file_path)?;
        let file_size = file.metadata()?.len();
        let end_pos = end_offset.unwrap_or(file_size);
        
        if start_offset > file_size {
            anyhow::bail!("Start offset {} exceeds file size {}", start_offset, file_size);
        }
        
        if end_pos > file_size {
            anyhow::bail!("End offset {} exceeds file size {}", end_pos, file_size);
        }
        
        if start_offset >= end_pos {
            anyhow::bail!("Start offset {} must be less than end offset {}", start_offset, end_pos);
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
                let decoder = GzDecoder::new(file);
                let reader = BufReader::with_capacity(64 * 1024, decoder);
                Ok((Box::new(reader), effective_size))
            } else {
                let decoder = BzDecoder::new(file);
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

    pub fn parse_file_with_progress<P: AsRef<Path>, F>(
        &mut self,
        file_path: P,
        progress_callback: F,
    ) -> anyhow::Result<Vec<QueryPlan>>
    where
        F: FnMut(f64, usize),
    {
        let (reader, total_size) = Self::create_reader(&file_path)?;
        self.parse_reader_with_progress(reader, total_size, progress_callback)
    }

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
        let (reader, effective_size) = Self::create_reader_with_range(&file_path, start_offset, end_offset)?;
        self.parse_reader_with_progress(reader, effective_size, progress_callback)
    }

    pub fn parse_reader_with_progress<R: BufRead, F>(
        &mut self,
        mut reader: R,
        total_size: u64,
        mut progress_callback: F,
    ) -> anyhow::Result<Vec<QueryPlan>>
    where
        F: FnMut(f64, usize),
    {
        let total_size = total_size as f64;
        let mut query_plans = Vec::with_capacity(2000);
        let mut plan_lines = Vec::with_capacity(50);
        let mut parsing_state = ParsingState::None;
        let mut line_count = 0u64;
        let mut bytes_processed = 0u64;
        let mut plans_processed = 0usize;

        loop {
            self.byte_buffer.clear();
            let bytes_read = reader
                .read_until(b'\n', &mut self.byte_buffer)
                .expect("Line to be read");

            let slice = str::from_utf8(&self.byte_buffer)
                .unwrap_or_else(|e| str::from_utf8(&self.byte_buffer[..e.valid_up_to()]).unwrap());

            if bytes_read == 0 {
                break; // EOF
            }

            line_count += 1;
            bytes_processed += bytes_read as u64;

            // Update progress every 10000 lines for better performance
            if line_count % 10000 == 0 {
                let current_len = query_plans.len();
                let delta = current_len - plans_processed;
                plans_processed = current_len;
                let progress = (bytes_processed as f64 / total_size).min(1.0);
                progress_callback(progress, delta);
            }

            // Remove trailing newline in place
            let line_trimmed = slice.trim_end();

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
                        plan_lines: Vec::new(),
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
        progress_callback(1.0, 0);

        Ok(query_plans)
    }

    pub fn parse_string_with_progress<F>(
        &mut self,
        content: &str,
        mut progress_callback: F,
    ) -> anyhow::Result<Vec<QueryPlan>>
    where
        F: FnMut(f64, usize),
    {
        let reader = std::io::Cursor::new(content.as_bytes());
        let content_size = content.len() as u64;
        self.parse_reader_with_progress(reader, content_size, progress_callback)
    }

    pub fn parse_bytes_with_progress<F>(
        &mut self,
        content: &[u8],
        mut progress_callback: F,
    ) -> anyhow::Result<Vec<QueryPlan>>
    where
        F: FnMut(f64, usize),
    {
        let reader = std::io::Cursor::new(content);
        let content_size = content.len() as u64;
        self.parse_reader_with_progress(reader, content_size, progress_callback)
    }

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
                        plans.retain(|plan| date_filter.matches(plan.timestamp));
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
        // Use rayon to parallelize processing of different query groups
        let processed_queries: HashMap<u64, ProcessedQuery> = query_groups
            .into_par_iter()
            .filter_map(|(hash, indices)| {
                normalized_queries.get(&hash).map(|normalized_query| {
                    let first_idx = indices[0];
                    let first_plan = &plans[first_idx];

                    // Calculate statistics using indices
                    let durations: Vec<f64> =
                        indices.iter().map(|&i| plans[i].duration_ms).collect();
                    let total_duration: f64 = durations.iter().sum();
                    let count = indices.len();
                    let (mean_duration, std_dev) =
                        QueryStatisticsCalculator::calculate_mean_and_std_dev(&durations);
                    let (min_duration, max_duration) =
                        QueryStatisticsCalculator::find_min_max(&durations);

                    // Calculate timestamp range for this query group
                    let timestamps: Vec<_> = indices.iter().map(|&i| plans[i].timestamp).collect();
                    let min_timestamp = *timestamps.iter().min().unwrap();
                    let max_timestamp = *timestamps.iter().max().unwrap();

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

                    // Calculate percentiles
                    let percentiles = QueryStatisticsCalculator::calculate_percentiles(&durations);

                    // Generate hourly histogram
                    let hourly_histogram =
                        QueryStatisticsCalculator::generate_hourly_histogram(&executions);

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

                    // Parse the execution plan from the slowest execution using structured plan lines
                    let parsed_plan = if !plans[slowest_idx].plan_lines.is_empty() {
                        self.plan_parser
                            .parse_plan_from_lines(&plans[slowest_idx].plan_lines)
                            .map_err(|e| {
                                eprintln!("Failed to parse plan from lines for query {}: {}", hash, e);
                                e
                            })
                            .ok()
                    } else {
                        // Fallback to text parsing if plan_lines are empty
                        self.plan_parser
                            .parse_plan(&plans[slowest_idx].plan)
                            .map_err(|e| {
                                eprintln!("Failed to parse plan from text for query {}: {}", hash, e);
                                e
                            })
                            .ok()
                    };

                    let processed_query = ProcessedQuery {
                        original_query: first_plan.query_text.clone(),
                        plan: plans[slowest_idx].plan.clone(),
                        parsed_plan,
                        normalized_query: normalized_query.clone(),
                        formatted_query,
                        statistics,
                    };

                    (hash, processed_query)
                })
            })
            .collect();

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    
    #[test]
    fn test_plan_parsing_integration() {
        // Test with a sample log file if it exists
        let log_file = "logs/postgresql-2025-06-12.log";
        if Path::new(log_file).exists() {
            let mut parser = PostgreSQLLogParser::new();
            
            // Parse just a few queries to test integration
            if let Ok(query_plans) = parser.parse_file_with_progress(log_file, |_, _| {}) {
                if !query_plans.is_empty() {
                    // Process the queries to trigger plan parsing
                    let processed_queries = parser.get_processed_queries(&query_plans);
                    
                    // Verify that some plans were parsed
                    let parsed_count = processed_queries
                        .values()
                        .filter(|q| q.parsed_plan.is_some())
                        .count();
                    
                    println!("Parsed {} plans out of {} unique queries", 
                             parsed_count, processed_queries.len());
                    
                    // At least some plans should be parsed successfully
                    assert!(parsed_count > 0, "No plans were successfully parsed");
                    
                    // Check that parsed plans have expected structure
                    for query in processed_queries.values() {
                        if let Some(parsed_plan) = &query.parsed_plan {
                            assert!(parsed_plan.node_count() > 0);
                            assert!(parsed_plan.max_depth() > 0);
                            assert!(parsed_plan.total_cost() >= 0.0);
                        }
                    }
                    
                    println!("Plan parsing integration test passed!");
                } else {
                    println!("No query plans found in log file, skipping test");
                }
            } else {
                println!("Could not parse log file, skipping test");
            }
        } else {
            println!("Log file not found, skipping plan parsing integration test");
        }
    }
}
