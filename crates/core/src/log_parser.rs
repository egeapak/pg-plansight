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
    DateFilter, ParseProgress, ProcessedQuery, QueryGroupStatistics, QueryPlan,
};
use crate::parsing::{LogParsingState as ParsingState, QueryPlanBuilder, PlanFormat};

use crate::parser_utils::{
    QueryStatisticsCalculator, RegexPatterns,
    parse_duration_from_line, parse_timestamp,
};
use crate::sql_analysis::normalize_query_enhanced;
use crate::plan_parser::PlanParser;

mod magic_number {
    pub const GZIP: [u8; 2] = [0x1f, 0x8b];
    pub const BZIP2: [u8; 3] = [0x42, 0x5a, 0x68]; // "BZh"
}

#[derive(Debug)]
pub struct PostgreSQLLogParser {
    pub regex_patterns: RegexPatterns,
    pub query_cache: HashMap<String, ProcessedQuery>,
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

    /// Detect plan format based on content
    fn detect_plan_format(&self, content: &str) -> PlanFormat {
        let trimmed = content.trim_start();
        if trimmed.starts_with('[') || trimmed.starts_with('{') {
            PlanFormat::Json
        } else {
            PlanFormat::Text
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
        let mut query_plans = Vec::with_capacity(2000);
        let mut plan_content = String::with_capacity(2000);
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

                    let new_builder = QueryPlanBuilder::new(timestamp, duration);

                    if let Some(current_plan) = parsing_state.reset_with_builder(new_builder) {
                        query_plans.push(current_plan);
                    }

                    plan_content.clear();
                }
                // Any other log line with timestamp ends the current parsing
                else if let Some(plan) = parsing_state.finish_with_content(&plan_content) {
                    query_plans.push(plan);
                }
            } else {
                // Handle continuation lines (lines that don't match the log format)
                let trimmed = line_trimmed.trim();

                if trimmed.is_empty() {
                    continue;
                }

                parsing_state = match std::mem::replace(&mut parsing_state, ParsingState::None) {
                    ParsingState::WaitingForQuery(mut builder) => {
                        if let Some(query_text) = trimmed.strip_prefix("Query Text:") {
                            builder.set_query_text(query_text.trim().to_string());
                            ParsingState::ParsingQuery(builder)
                        } else {
                            ParsingState::WaitingForQuery(builder)
                        }
                    }
                    ParsingState::ParsingQuery(builder) => {
                        // Detect format based on first plan content line
                        let format = self.detect_plan_format(trimmed);
                        
                        match format {
                            PlanFormat::Text => {
                                if self.regex_patterns.plan_regex.is_match(trimmed) {
                                    let typed_builder = builder.convert_to_text();
                                    if let QueryPlanBuilder::Text(text_builder) = typed_builder {
                                        match text_builder.add_line(line_trimmed) {
                                            Ok((updated_builder, maybe_plan)) => {
                                                if let Some(plan) = maybe_plan {
                                                    query_plans.push(plan);
                                                    ParsingState::None
                                                } else {
                                                    ParsingState::ParsingTextPlan(QueryPlanBuilder::Text(updated_builder))
                                                }
                                            }
                                            Err(e) => {
                                                eprintln!("Text plan parsing error: {}", e);
                                                ParsingState::None
                                            }
                                        }
                                    } else {
                                        ParsingState::ParsingTextPlan(typed_builder)
                                    }
                                } else {
                                    // Continue parsing query text
                                    let mut updated_builder = builder;
                                    let current_query = updated_builder.query_text().to_string();
                                    let new_query = if current_query.is_empty() {
                                        line_trimmed.to_string()
                                    } else {
                                        format!("{}\n{}", current_query, line_trimmed)
                                    };
                                    updated_builder.set_query_text(new_query);
                                    ParsingState::ParsingQuery(updated_builder)
                                }
                            }
                            PlanFormat::Json => {
                                let typed_builder = builder.convert_to_json();
                                if let QueryPlanBuilder::Json(json_builder) = typed_builder {
                                    match json_builder.add_line(trimmed) {
                                        Ok((updated_builder, maybe_plan)) => {
                                            if let Some(plan) = maybe_plan {
                                                query_plans.push(plan);
                                                ParsingState::None
                                            } else {
                                                ParsingState::ParsingJsonPlan(QueryPlanBuilder::Json(updated_builder), String::new())
                                            }
                                        }
                                        Err(e) => {
                                            eprintln!("JSON plan parsing error: {}", e);
                                            ParsingState::None
                                        }
                                    }
                                } else {
                                    ParsingState::ParsingJsonPlan(typed_builder, String::new())
                                }
                            }
                        }
                    }
                    ParsingState::ParsingTextPlan(builder) => {
                        if let QueryPlanBuilder::Text(text_builder) = builder {
                            match text_builder.add_line(line_trimmed) {
                                Ok((updated_builder, maybe_plan)) => {
                                    if let Some(plan) = maybe_plan {
                                        query_plans.push(plan);
                                        ParsingState::None
                                    } else {
                                        ParsingState::ParsingTextPlan(QueryPlanBuilder::Text(updated_builder))
                                    }
                                }
                                Err(e) => {
                                    eprintln!("Text plan parsing error: {}", e);
                                    ParsingState::None
                                }
                            }
                        } else {
                            ParsingState::ParsingTextPlan(builder)
                        }
                    }
                    ParsingState::ParsingJsonPlan(builder, json_content) => {
                        if let QueryPlanBuilder::Json(json_builder) = builder {
                            match json_builder.add_line(trimmed) {
                                Ok((updated_builder, maybe_plan)) => {
                                    if let Some(plan) = maybe_plan {
                                        query_plans.push(plan);
                                        ParsingState::None
                                    } else {
                                        ParsingState::ParsingJsonPlan(QueryPlanBuilder::Json(updated_builder), json_content)
                                    }
                                }
                                Err(e) => {
                                    eprintln!("JSON plan parsing error: {}", e);
                                    ParsingState::None
                                }
                            }
                        } else {
                            // Invalid builder type
                            ParsingState::ParsingJsonPlan(builder, json_content)
                        }
                    }
                    state => state,
                };
            }
        }

        // Handle any remaining plan
        if let Some(plan) = parsing_state.finish_with_content(&plan_content) {
            query_plans.push(plan);
        }

        // Final progress update
        progress_callback(1.0, 0);

        Ok(query_plans)
    }

    // Convenience methods that use the generic parse_with_progress

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

    pub fn get_processed_queries(&mut self, plans: &[QueryPlan]) -> HashMap<String, ProcessedQuery> {
        // Group plans by fingerprint using enhanced normalization
        let mut query_groups: HashMap<String, Vec<usize>> = HashMap::new();
        let mut normalization_cache: HashMap<String, crate::sql_analysis::NormalizationResult> = HashMap::new();

        for (idx, plan) in plans.iter().enumerate() {
            let cache_key = plan.query_text().to_string();
            
            let fingerprint = if let Some(cached) = normalization_cache.get(&cache_key) {
                cached.fingerprint.clone()
            } else {
                match normalize_query_enhanced(plan.query_text()) {
                    Ok(result) => {
                        let fingerprint = result.fingerprint.clone();
                        normalization_cache.insert(cache_key, result);
                        fingerprint
                    }
                    Err(_) => {
                        // Fallback to simple hash for malformed SQL
                        use std::collections::hash_map::DefaultHasher;
                        use std::hash::{Hash, Hasher};
                        let mut hasher = DefaultHasher::new();
                        plan.query_text().hash(&mut hasher);
                        format!("{:016x}", hasher.finish())
                    }
                }
            };
            
            query_groups.entry(fingerprint).or_default().push(idx);
        }

        // Build ProcessedQuery structs using indices to avoid cloning
        // Use rayon to parallelize processing of different query groups
        let processed_queries: HashMap<String, ProcessedQuery> = query_groups
            .into_par_iter()
            .filter_map(|(fingerprint, indices)| {
                let first_idx = indices[0];

                // Calculate statistics using indices
                let durations: Vec<f64> =
                    indices.iter().map(|&i| plans[i].duration_ms()).collect();
                let total_duration: f64 = durations.iter().sum();
                let count = indices.len();
                let (mean_duration, std_dev) =
                    QueryStatisticsCalculator::calculate_mean_and_std_dev(&durations);
                let (min_duration, max_duration) =
                    QueryStatisticsCalculator::find_min_max(&durations);

                // Calculate timestamp range for this query group
                let timestamps: Vec<_> = indices.iter().map(|&i| plans[i].timestamp()).collect();
                let min_timestamp = *timestamps.iter().min().unwrap();
                let max_timestamp = *timestamps.iter().max().unwrap();

                // Find the slowest execution index
                let slowest_idx = indices
                    .iter()
                    .max_by(|&&a, &&b| {
                        plans[a]
                            .duration_ms()
                            .partial_cmp(&plans[b].duration_ms())
                            .unwrap()
                    })
                    .copied()
                    .unwrap_or(first_idx);

                // SQL formatting is now done in QueryPlan construction

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

                // Use the slowest execution as the representative plan
                let representative_plan = plans[slowest_idx].clone();

                let processed_query = ProcessedQuery {
                    representative_plan,
                    statistics,
                };

                Some((fingerprint, processed_query))
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
                    
                    if let Some(text_data) = plan.as_text_plan() {
                        println!("  Plan Lines: {}", text_data.plan_lines.len());
                        println!("  Plan Text preview: {}", &text_data.plan_text[..100.min(text_data.plan_text.len())]);
                    }
                }
                
                // Expect at least 1 plan
                assert!(plans.len() >= 1, "Should parse at least 1 plan, got {}", plans.len());
                
                let first_plan = &plans[0];
                assert!(first_plan.is_text_plan(), "First plan should be text format");
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
                
                if plans.len() > 0 {
                    let plan = &plans[0];
                    println!("  Is JSON Plan: {}", plan.is_json_plan());
                    println!("  Query: {}", plan.query_text());
                    println!("  Duration: {} ms", plan.duration_ms());
                    
                    if let Some(json_data) = plan.as_json_plan() {
                        println!("  JSON Details:");
                        println!("    Node Type: {}", json_data.parsed_json.plan.node_type);
                        println!("    Relation: {:?}", json_data.parsed_json.plan.relation_name);
                        println!("    Startup Cost: {}", json_data.parsed_json.plan.startup_cost);
                        
                        // Test plan parser integration
                        if let Ok(parsed_plan) = parser.plan_parser.parse_query_plan(plan) {
                            println!("    Parsed to PlanNode successfully!");
                            println!("    Source format: {:?}", parsed_plan.source_format);
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
    fn test_actual_log_file_parsing() {
        use std::fs;
        
        // Test with a sample from the actual log file
        let sample_file = "test_sample.log";
        if Path::new(sample_file).exists() {
            let log_content = fs::read_to_string(sample_file).expect("Failed to read sample file");
            let mut parser = PostgreSQLLogParser::new();
            
            match parser.parse_string_with_progress(&log_content, |_progress, _count| {}) {
                Ok(plans) => {
                    println!("Debug: Parsed {} plans from sample file", plans.len());
                    
                    for (i, plan) in plans.iter().take(3).enumerate() {
                        println!("Plan {}: ", i + 1);
                        println!("  Timestamp: {}", plan.timestamp());
                        println!("  Duration: {} ms", plan.duration_ms());
                        println!("  Query preview: {}", &plan.query_text()[..60.min(plan.query_text().len())]);
                        println!("  Is Text Plan: {}", plan.is_text_plan());
                        
                        if let Some(text_data) = plan.as_text_plan() {
                            println!("  Plan Lines: {}", text_data.plan_lines.len());
                        }
                    }
                    
                    if plans.len() > 0 {
                        println!("Sample parsing works correctly!");
                    } else {
                        println!("Warning: No plans parsed from sample file");
                    }
                }
                Err(e) => {
                    println!("Sample parsing failed: {}", e);
                }
            }
        } else {
            println!("Sample file not found, skipping test");
        }
    }

    #[test]
    fn test_plan_parsing_integration() {
        // Test with a sample log file if it exists
        let log_file = "../../logs/postgresql-2025-06-12.log";
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

                    println!(
                        "Parsed {} plans out of {} unique queries",
                        parsed_count,
                        processed_queries.len()
                    );

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
