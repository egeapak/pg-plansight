use anyhow::Context as _;
use hashbrown::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::models::{ParsingState, QueryPlan, QueryStatistics, ProcessedQuery, QueryGroupStatistics};
use crate::parser_utils::{RegexPatterns, normalize_query, calculate_query_hash, parse_timestamp, 
                        get_indent_level, format_plan_lines, format_sql_query, QueryStatisticsCalculator};

#[derive(Debug)]
pub struct PostgreSQLLogParser {
    pub regex_patterns: RegexPatterns,
    pub query_cache: HashMap<u64, ProcessedQuery>,
    line_buffer: String,
}

impl PostgreSQLLogParser {
    pub fn new() -> Self {
        Self {
            regex_patterns: RegexPatterns::new(),
            query_cache: HashMap::new(),
            line_buffer: String::with_capacity(1024),
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
        let file = File::open(&file_path)?;

        // Get total file size for progress calculation
        let total_size = file.metadata()?.len() as f64;

        // Use a larger buffer for better I/O performance
        let mut reader = BufReader::with_capacity(64 * 1024, file);
        let mut query_plans = Vec::new();
        let mut current_plan: Option<QueryPlan> = None;
        let mut plan_lines = Vec::new();
        let mut parsing_state = ParsingState::None;
        let mut line_count = 0u64;
        let mut bytes_processed = 0u64;

        // Pre-allocate with estimated capacity to reduce reallocations
        query_plans.reserve(2000);
        plan_lines.reserve(50);

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
                if let Some(duration_match) = self.regex_patterns.duration_regex.captures(message) {
                    // Save previous plan if exists
                    if let Some(mut plan) = current_plan.take() {
                        plan.plan = format_plan_lines(&plan_lines);
                        query_plans.push(plan);
                    }

                    let duration: f64 = duration_match
                        .get(1)
                        .unwrap()
                        .as_str()
                        .parse()
                        .unwrap_or(0.0);
                    let timestamp = parse_timestamp(timestamp_str)
                        .with_context(|| format!("Can't parse timestamp: '{}'", timestamp_str))?;

                    current_plan = Some(QueryPlan {
                        timestamp,
                        duration_ms: duration,
                        query_text: String::new(),
                        plan: String::new(),
                        parameters: None,
                    });
                    plan_lines.clear();
                    parsing_state = ParsingState::WaitingForQuery;
                }
                // Any other log line with timestamp ends the current parsing
                else if parsing_state != ParsingState::None {
                    if let Some(mut plan) = current_plan.take() {
                        plan.plan = format_plan_lines(&plan_lines);
                        query_plans.push(plan);
                    }
                    parsing_state = ParsingState::None;
                }
            } else {
                // Handle continuation lines (lines that don't match the log format)
                let trimmed = line_trimmed.trim();

                match parsing_state {
                    ParsingState::WaitingForQuery => {
                        if trimmed.starts_with("Query Text:") {
                            if let Some(ref mut plan) = current_plan {
                                let query_text =
                                    trimmed.strip_prefix("Query Text:").unwrap_or("").trim();
                                plan.query_text = query_text.to_string();
                            }
                            parsing_state = ParsingState::ParsingPlan;
                        }
                    }
                    ParsingState::ParsingPlan => {
                        if !trimmed.is_empty() {
                            // Check if this line contains query plan (cost= pattern)
                            if self.regex_patterns.plan_regex.is_match(trimmed) && plan_lines.is_empty() {
                                // This is the start of the execution plan - parse with indentation level
                                let indent_level = get_indent_level(line_trimmed);
                                let clean_content = trimmed.to_string();
                                plan_lines.push(format!("{}:{}", indent_level, clean_content));
                            } else if !plan_lines.is_empty() {
                                // We're already in the plan section - parse with indentation level
                                let indent_level = get_indent_level(line_trimmed);
                                let clean_content = trimmed.to_string();
                                plan_lines.push(format!("{}:{}", indent_level, clean_content));
                            } else {
                                // Still part of query text (multiline query)
                                if let Some(ref mut plan) = current_plan {
                                    if !plan.query_text.is_empty() {
                                        plan.query_text.push('\n');
                                    }
                                    plan.query_text.push_str(line_trimmed);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // Handle any remaining plan
        if let Some(mut plan) = current_plan {
            plan.plan = format_plan_lines(&plan_lines);
            query_plans.push(plan);
        }

        // Final progress update
        progress_callback(1.0);

        Ok(query_plans)
    }


    pub fn get_query_statistics(&self, plans: &[QueryPlan]) -> QueryStatistics {
        use rayon::prelude::*;
        
        // Use parallel reduce for total duration calculation
        let total_duration: f64 = plans.par_iter().map(|p| p.duration_ms).sum();
        
        // Group by normalized query sequentially (parallel reduce is complex for hashmaps)
        let mut query_count_by_text = HashMap::new();
        let mut duration_by_query = HashMap::new();

        for plan in plans {
            let query_hash = normalize_query(&plan.query_text, &self.regex_patterns.placeholder_regex);
            *query_count_by_text.entry(query_hash.clone()).or_insert(0) += 1;
            duration_by_query
                .entry(query_hash)
                .and_modify(|d: &mut f64| *d += plan.duration_ms)
                .or_insert(plan.duration_ms);
        }

        let avg_duration = if plans.is_empty() {
            0.0
        } else {
            total_duration / plans.len() as f64
        };
        let slowest_query = plans.par_iter().max_by(|a, b| {
            a.duration_ms
                .partial_cmp(&b.duration_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        QueryStatistics {
            total_queries: plans.len(),
            total_duration_ms: total_duration,
            average_duration_ms: avg_duration,
            slowest_query_duration_ms: slowest_query.map(|q| q.duration_ms).unwrap_or(0.0),
            unique_queries: query_count_by_text.len(),
            most_frequent_queries: self.get_top_queries(&query_count_by_text, 5),
            slowest_queries: self.get_slowest_queries(plans, 5),
        }
    }


    fn get_top_queries(
        &self,
        query_counts: &HashMap<String, usize>,
        limit: usize,
    ) -> Vec<(String, usize)> {
        let mut sorted: Vec<_> = query_counts.iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(a.1));
        sorted
            .into_iter()
            .take(limit)
            .map(|(query, count)| (query.clone(), *count))
            .collect()
    }

    fn get_slowest_queries(&self, plans: &[QueryPlan], limit: usize) -> Vec<QueryPlan> {
        let mut indices: Vec<_> = (0..plans.len()).collect();
        indices.sort_by(|&a, &b| {
            plans[b].duration_ms
                .partial_cmp(&plans[a].duration_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        indices.into_iter().take(limit).map(|i| plans[i].clone()).collect()
    }


    pub fn get_processed_queries(&mut self, plans: &[QueryPlan]) -> HashMap<u64, ProcessedQuery> {
        
        // Group plans by hash in a single pass
        let mut query_groups: HashMap<u64, Vec<usize>> = HashMap::new();
        let mut normalized_queries: HashMap<u64, String> = HashMap::new();
        
        for (idx, plan) in plans.iter().enumerate() {
            let hash = calculate_query_hash(&plan.query_text, &self.regex_patterns.placeholder_regex);
            query_groups.entry(hash).or_default().push(idx);
            
            // Only store normalized query once per hash
            if !normalized_queries.contains_key(&hash) {
                let normalized = normalize_query(&plan.query_text, &self.regex_patterns.placeholder_regex);
                normalized_queries.insert(hash, normalized);
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
                let (mean_duration, std_dev) = QueryStatisticsCalculator::calculate_mean_and_std_dev(&durations);
                let (min_duration, max_duration) = QueryStatisticsCalculator::find_min_max(&durations);

                // Find the slowest execution index
                let slowest_idx = indices
                    .iter()
                    .max_by(|&&a, &&b| plans[a].duration_ms.partial_cmp(&plans[b].duration_ms).unwrap())
                    .copied()
                    .unwrap_or(first_idx);

                // Format SQL
                let formatted_query = format_sql_query(&first_plan.query_text);

                // Only clone the executions we need
                let executions: Vec<QueryPlan> = indices.iter().map(|&i| plans[i].clone()).collect();

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
