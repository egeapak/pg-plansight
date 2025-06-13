use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::fs::File;
use std::path::Path;

#[derive(Debug, PartialEq)]
enum ParsingState {
    None,
    WaitingForQuery,
    ParsingPlan,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp: DateTime<Utc>,
    pub process_id: u32,
    pub log_level: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryPlan {
    pub timestamp: DateTime<Utc>,
    pub process_id: u32,
    pub duration_ms: f64,
    pub query_text: String,
    pub plan: String,
    pub parameters: Option<String>,
}

#[derive(Debug)]
pub struct PostgreSQLLogParser {
    pub log_line_regex: Regex,
    pub duration_regex: Regex,
    pub plan_regex: Regex,
    pub parameters_regex: Regex,
    pub placeholder_regex: Regex,
}

impl PostgreSQLLogParser {
    pub fn new() -> Self {
        Self {
            log_line_regex: Regex::new(r"^(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3} \w+) \[(\d+)\] (\w+):\s*(.*)$").unwrap(),
            duration_regex: Regex::new(r"duration: ([\d.]+) ms\s+plan:\s*$").unwrap(),
            plan_regex: Regex::new(r"\(cost=[\d.]+\.\.[\d.]+\s+rows=\d+\s+width=\d+\)").unwrap(),
            parameters_regex: Regex::new(r"parameters: (.+)$").unwrap(),
            placeholder_regex: Regex::new(r"\$\d+").unwrap(),
        }
    }

    pub fn parse_file<P: AsRef<Path>>(&self, file_path: P) -> Result<Vec<QueryPlan>, Box<dyn std::error::Error>> {
        self.parse_file_with_progress(file_path, |_| {})
    }

    pub fn parse_file_with_progress<P: AsRef<Path>, F>(&self, file_path: P, mut progress_callback: F) -> Result<Vec<QueryPlan>, Box<dyn std::error::Error>> 
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

        let mut line = String::with_capacity(512);
        
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
                let process_id: u32 = captures.get(2).unwrap().as_str().parse().unwrap_or(0);
                let _log_level = captures.get(3).unwrap().as_str().to_string();
                let message = captures.get(4).unwrap().as_str();

                // Check for "duration: X ms plan:" which starts auto_explain output
                if let Some(duration_match) = self.duration_regex.captures(message) {
                    // Save previous plan if exists
                    if let Some(mut plan) = current_plan.take() {
                        plan.plan = plan_lines.join("\n");
                        query_plans.push(plan);
                    }

                    let duration: f64 = duration_match.get(1).unwrap().as_str().parse().unwrap_or(0.0);
                    let timestamp = self.parse_timestamp(timestamp_str)?;
                    
                    current_plan = Some(QueryPlan {
                        timestamp,
                        process_id,
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
                        plan.plan = plan_lines.join("\n");
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
                                let query_text = trimmed.strip_prefix("Query Text:").unwrap_or("").trim();
                                plan.query_text = query_text.to_string();
                            }
                            parsing_state = ParsingState::ParsingPlan;
                        }
                    }
                    ParsingState::ParsingPlan => {
                        if !trimmed.is_empty() {
                            // Check if this line contains query plan (cost= pattern)
                            if self.plan_regex.is_match(trimmed) && plan_lines.is_empty() {
                                // This is the start of the execution plan
                                plan_lines.push(trimmed.to_string());
                            } else if !plan_lines.is_empty() {
                                // We're already in the plan section
                                plan_lines.push(trimmed.to_string());
                            } else {
                                // Still part of query text (multiline query)
                                if let Some(ref mut plan) = current_plan {
                                    if !plan.query_text.is_empty() {
                                        plan.query_text.push(' ');
                                    }
                                    plan.query_text.push_str(trimmed);
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
            plan.plan = plan_lines.join("\n");
            query_plans.push(plan);
        }

        // Final progress update
        progress_callback(1.0);

        Ok(query_plans)
    }

    fn parse_timestamp(&self, timestamp_str: &str) -> Result<DateTime<Utc>, Box<dyn std::error::Error>> {
        // The format is: "2025-05-28 00:00:33.355 UTC"
        // We need to handle variable length fractional seconds
        use chrono::NaiveDateTime;
        
        // Split timestamp and timezone
        let parts: Vec<&str> = timestamp_str.rsplitn(2, ' ').collect();
        if parts.len() != 2 {
            return Err("Invalid timestamp format".into());
        }
        
        let datetime_part = parts[1];
        
        // Parse the datetime part without timezone
        let naive_dt = NaiveDateTime::parse_from_str(datetime_part, "%Y-%m-%d %H:%M:%S%.f")?;
        
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
            duration_by_query.entry(query_hash)
                .and_modify(|d: &mut f64| *d += plan.duration_ms)
                .or_insert(plan.duration_ms);
        }

        let avg_duration = if plans.is_empty() { 0.0 } else { total_duration / plans.len() as f64 };
        let slowest_query = plans.iter().max_by(|a, b| a.duration_ms.partial_cmp(&b.duration_ms).unwrap_or(std::cmp::Ordering::Equal));

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

    fn get_top_queries(&self, query_counts: &HashMap<String, usize>, limit: usize) -> Vec<(String, usize)> {
        let mut sorted: Vec<_> = query_counts.iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(a.1));
        sorted.into_iter()
            .take(limit)
            .map(|(query, count)| (query.clone(), *count))
            .collect()
    }

    fn get_slowest_queries(&self, plans: &[QueryPlan], limit: usize) -> Vec<QueryPlan> {
        let mut sorted_plans = plans.to_vec();
        sorted_plans.sort_by(|a, b| b.duration_ms.partial_cmp(&a.duration_ms).unwrap_or(std::cmp::Ordering::Equal));
        sorted_plans.into_iter().take(limit).collect()
    }
}

#[derive(Debug, Clone)]
pub struct QueryStatistics {
    pub total_queries: usize,
    pub total_duration_ms: f64,
    pub average_duration_ms: f64,
    pub slowest_query_duration_ms: f64,
    pub unique_queries: usize,
    pub most_frequent_queries: Vec<(String, usize)>,
    pub slowest_queries: Vec<QueryPlan>,
}