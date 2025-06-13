use anyhow::Context as _;
use chrono::{DateTime, Utc};
use regex::Regex;
use hashbrown::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;

use crate::models::{ParsingState, QueryPlan, QueryStatistics, ProcessedQuery, QueryGroupStatistics};

#[derive(Debug)]
pub struct PostgreSQLLogParser {
    pub log_line_regex: Regex,
    pub duration_regex: Regex,
    pub plan_regex: Regex,
    pub parameters_regex: Regex,
    pub placeholder_regex: Regex,
    pub query_cache: HashMap<u64, ProcessedQuery>,
}

impl PostgreSQLLogParser {
    pub fn new() -> Self {
        Self {
            log_line_regex: Regex::new(r"^(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3})(.*)")
                .unwrap(),
            duration_regex: Regex::new(r"duration: ([\d.]+) ms\s+plan:\s*$").unwrap(),
            plan_regex: Regex::new(r"\(cost=[\d.]+\.\.[\d.]+\s+rows=\d+\s+width=\d+\)").unwrap(),
            parameters_regex: Regex::new(r"parameters: (.+)$").unwrap(),
            placeholder_regex: Regex::new(r"\$\d+").unwrap(),
            query_cache: HashMap::new(),
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

        let mut reader = BufReader::new(file);
        let mut query_plans = Vec::new();
        let mut current_plan: Option<QueryPlan> = None;
        let mut plan_lines = Vec::new();
        let mut parsing_state = ParsingState::None;
        let mut line_count = 0u64;
        let mut bytes_processed = 0u64;

        // Pre-allocate with estimated capacity to reduce reallocations
        query_plans.reserve(2000);
        plan_lines.reserve(50);

        let mut line = String::with_capacity(1024);

        loop {
            line.clear();
            let bytes_read = reader.read_line(&mut line)?;

            if bytes_read == 0 {
                break; // EOF
            }

            line_count += 1;
            bytes_processed += bytes_read as u64;

            // Update progress every 5000 lines using byte counting instead of stream_position()
            if line_count % 5000 == 0 {
                let progress = (bytes_processed as f64 / total_size).min(1.0);
                progress_callback(progress);
            }

            // Remove trailing newline in place
            let line_trimmed = line.trim_end();

            if let Some(captures) = self.log_line_regex.captures(line_trimmed) {
                let timestamp_str = captures.get(1).unwrap().as_str();
                let message = captures.get(2).unwrap().as_str();

                // Check for "duration: X ms plan:" which starts auto_explain output
                if let Some(duration_match) = self.duration_regex.captures(message) {
                    // Save previous plan if exists
                    if let Some(mut plan) = current_plan.take() {
                        plan.plan = self.format_plan_lines(&plan_lines);
                        query_plans.push(plan);
                    }

                    let duration: f64 = duration_match
                        .get(1)
                        .unwrap()
                        .as_str()
                        .parse()
                        .unwrap_or(0.0);
                    let timestamp = self
                        .parse_timestamp(timestamp_str)
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
                        plan.plan = self.format_plan_lines(&plan_lines);
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
                            if self.plan_regex.is_match(trimmed) && plan_lines.is_empty() {
                                // This is the start of the execution plan - parse with indentation level
                                let indent_level = self.get_indent_level(line_trimmed);
                                let clean_content = trimmed.to_string();
                                plan_lines.push(format!("{}:{}", indent_level, clean_content));
                            } else if !plan_lines.is_empty() {
                                // We're already in the plan section - parse with indentation level
                                let indent_level = self.get_indent_level(line_trimmed);
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
            plan.plan = self.format_plan_lines(&plan_lines);
            query_plans.push(plan);
        }

        // Final progress update
        progress_callback(1.0);

        Ok(query_plans)
    }

    fn parse_timestamp(&self, timestamp_str: &str) -> anyhow::Result<DateTime<Utc>> {
        // The format is: "2025-05-28 00:00:33.355 UTC"
        // We need to handle variable length fractional seconds
        use chrono::NaiveDateTime;

        // Parse the datetime part without timezone
        let naive_dt = NaiveDateTime::parse_from_str(timestamp_str, "%Y-%m-%d %H:%M:%S%.f")?;

        // Create UTC datetime
        Ok(DateTime::from_naive_utc_and_offset(naive_dt, Utc))
    }

    pub fn get_query_statistics(&self, plans: &[QueryPlan]) -> QueryStatistics {
        let mut total_duration = 0.0;
        let mut query_count_by_text = HashMap::new();
        let mut duration_by_query = HashMap::new();

        for plan in plans {
            total_duration += plan.duration_ms;

            let query_hash = self.normalize_query(&plan.query_text);
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
        let slowest_query = plans.iter().max_by(|a, b| {
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

    fn normalize_query(&self, query: &str) -> String {
        let query = query.trim();
        self.placeholder_regex.replace_all(query, "?").to_string()
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
        let mut sorted_plans = plans.to_vec();
        sorted_plans.sort_by(|a, b| {
            b.duration_ms
                .partial_cmp(&a.duration_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        sorted_plans.into_iter().take(limit).collect()
    }

    fn get_indent_level(&self, line: &str) -> usize {
        // Count leading whitespace characters (spaces and tabs)
        line.chars().take_while(|c| c.is_whitespace()).count()
    }

    fn format_plan_lines(&self, plan_lines: &[String]) -> String {
        let mut formatted_lines = Vec::new();
        
        for line in plan_lines {
            if let Some((indent_str, content)) = line.split_once(':') {
                if let Ok(indent_level) = indent_str.parse::<usize>() {
                    // Convert indentation to consistent 2-space indentation
                    let spaces = "  ".repeat(indent_level / 2);
                    formatted_lines.push(format!("{}{}", spaces, content));
                } else {
                    // Fallback: use the line as-is if parsing fails
                    formatted_lines.push(content.to_string());
                }
            } else {
                // Fallback: use the line as-is if no indent level found
                formatted_lines.push(line.clone());
            }
        }
        
        formatted_lines.join("\n")
    }

    fn calculate_query_hash(&self, query: &str) -> u64 {
        let normalized = self.normalize_query(query);
        let mut hasher = DefaultHasher::new();
        normalized.hash(&mut hasher);
        hasher.finish()
    }

    pub fn get_processed_queries(&mut self, plans: &[QueryPlan]) -> HashMap<u64, ProcessedQuery> {
        use rayon::prelude::*;
        
        // First pass: collect all normalized queries and their hashes
        let query_hashes: Vec<(u64, String)> = plans
            .par_iter()
            .map(|plan| {
                let normalized = self.normalize_query(&plan.query_text);
                let hash = self.calculate_query_hash(&plan.query_text);
                (hash, normalized)
            })
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        // Second pass: group plans by hash and build ProcessedQuery structs
        let mut processed_queries = HashMap::new();
        
        for (hash, normalized_query) in query_hashes {
            // Find all plans matching this hash
            let matching_plans: Vec<QueryPlan> = plans
                .iter()
                .filter(|plan| self.calculate_query_hash(&plan.query_text) == hash)
                .cloned()
                .collect();

            if let Some(first_plan) = matching_plans.first() {
                // Calculate statistics
                let durations: Vec<f64> = matching_plans.iter().map(|p| p.duration_ms).collect();
                let total_duration: f64 = durations.iter().sum();
                let count = matching_plans.len();
                let mean_duration = total_duration / count as f64;
                let min_duration = durations.iter().fold(f64::INFINITY, |a, &b| a.min(b));
                let max_duration = durations.iter().fold(0.0f64, |a, &b| a.max(b));
                
                let variance = durations
                    .iter()
                    .map(|&d| (d - mean_duration).powi(2))
                    .sum::<f64>() / count as f64;
                let std_dev = variance.sqrt();

                // Find the slowest execution for the plan
                let slowest_plan = matching_plans
                    .iter()
                    .max_by(|a, b| a.duration_ms.partial_cmp(&b.duration_ms).unwrap())
                    .unwrap();

                // Format SQL
                let formatted_query = self.format_sql_query(&first_plan.query_text);

                let statistics = QueryGroupStatistics {
                    count,
                    total_duration_ms: total_duration,
                    min_duration_ms: min_duration,
                    max_duration_ms: max_duration,
                    mean_duration_ms: mean_duration,
                    std_dev_ms: std_dev,
                    executions: matching_plans.clone(),
                };

                let processed_query = ProcessedQuery {
                    original_query: first_plan.query_text.clone(),
                    plan: slowest_plan.plan.clone(),
                    normalized_query,
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

    fn format_sql_query(&self, sql: &str) -> String {
        let format_options = sqlformat::FormatOptions {
            indent: sqlformat::Indent::Spaces(4),
            uppercase: true,
            lines_between_queries: 1,
        };
        sqlformat::format(sql, &sqlformat::QueryParams::None, format_options)
    }
}
