use std::io::{BufRead, BufReader};
use std::fs::File;
use std::path::Path;
use crate::log_parser::QueryPlan;
use regex::Regex;
use chrono::{DateTime, Utc};

#[derive(Debug, PartialEq)]
enum ParsingStateEnum {
    None,
    WaitingForQuery,
    ParsingPlan,
}

pub struct ParsedResult {
    pub query_plans: Vec<QueryPlan>,
    pub total_lines: u64,
    pub total_bytes: u64,
}

pub struct ParsingHelper {
    pub log_line_regex: Regex,
    pub duration_regex: Regex,
    pub plan_regex: Regex,
    pub parameters_regex: Regex,
}

impl ParsingHelper {
    pub fn new() -> Self {
        Self {
            log_line_regex: Regex::new(r"^(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3} \w+) \[(\d+)\] (\w+):\s*(.*)$").unwrap(),
            duration_regex: Regex::new(r"duration: ([\d.]+) ms\s+plan:\s*$").unwrap(),
            plan_regex: Regex::new(r"duration: [\d.]+ ms\s+statement:").unwrap(),
            parameters_regex: Regex::new(r"parameters: (.+)$").unwrap(),
        }
    }

    pub fn parse_file_optimized<P: AsRef<Path>, F>(&self, file_path: P, mut progress_callback: F) -> Result<ParsedResult, Box<dyn std::error::Error>> 
    where
        F: FnMut(f64),
    {
        let file = File::open(&file_path)?;
        let total_size = file.metadata()?.len() as f64;
        
        let mut reader = BufReader::new(file);
        let mut query_plans = Vec::new();
        let mut current_plan: Option<QueryPlan> = None;
        let mut plan_lines = Vec::new();
        let mut parsing_state = ParsingStateEnum::None;
        let mut line_count = 0u64;
        let mut bytes_processed = 0u64;

        query_plans.reserve(2000);
        plan_lines.reserve(50);

        let mut line = String::with_capacity(512);
        
        loop {
            line.clear();
            let bytes_read = reader.read_line(&mut line)?;
            
            if bytes_read == 0 {
                break;
            }
            
            line_count += 1;
            bytes_processed += bytes_read as u64;
            
            if line_count % 5000 == 0 {
                let progress = (bytes_processed as f64 / total_size).min(1.0);
                progress_callback(progress);
            }
            
            let line_trimmed = line.trim_end();
            
            if let Some(captures) = self.log_line_regex.captures(line_trimmed) {
                let timestamp_str = captures.get(1).unwrap().as_str();
                let process_id: u32 = captures.get(2).unwrap().as_str().parse().unwrap_or(0);
                let message = captures.get(4).unwrap().as_str();

                if let Some(duration_match) = self.duration_regex.captures(message) {
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
                    parsing_state = ParsingStateEnum::WaitingForQuery;
                }
                else if self.plan_regex.is_match(message) {
                    if let Some(mut plan) = current_plan.take() {
                        plan.plan = plan_lines.join("\n");
                        query_plans.push(plan);
                    }
                    parsing_state = ParsingStateEnum::None;
                }
                else {
                    self.process_message_by_state(&mut parsing_state, &mut current_plan, &mut plan_lines, message);
                }
            } else {
                self.process_continuation_line(&parsing_state, &mut current_plan, &mut plan_lines, line_trimmed);
            }
        }

        if let Some(mut plan) = current_plan {
            plan.plan = plan_lines.join("\n");
            query_plans.push(plan);
        }

        progress_callback(1.0);

        Ok(ParsedResult {
            query_plans,
            total_lines: line_count,
            total_bytes: bytes_processed,
        })
    }

    fn process_message_by_state(&self, parsing_state: &mut ParsingStateEnum, current_plan: &mut Option<QueryPlan>, plan_lines: &mut Vec<String>, message: &str) {
        match parsing_state {
            ParsingStateEnum::WaitingForQuery => {
                if message.starts_with("Query Text:") {
                    if let Some(plan) = current_plan {
                        plan.query_text = message.strip_prefix("Query Text: ").unwrap_or("").trim().to_string();
                    }
                    *parsing_state = ParsingStateEnum::ParsingPlan;
                }
            }
            ParsingStateEnum::ParsingPlan => {
                if let Some(params_match) = self.parameters_regex.captures(message) {
                    if let Some(plan) = current_plan {
                        plan.parameters = Some(params_match.get(1).unwrap().as_str().to_string());
                    }
                } else if !message.trim().is_empty() && 
                         !message.starts_with("DETAIL:") && 
                         !message.starts_with("STATEMENT:") &&
                         !message.starts_with("ERROR:") {
                    plan_lines.push(message.to_string());
                }
            }
            _ => {}
        }
    }

    fn process_continuation_line(&self, parsing_state: &ParsingStateEnum, current_plan: &mut Option<QueryPlan>, plan_lines: &mut Vec<String>, line_trimmed: &str) {
        match parsing_state {
            ParsingStateEnum::WaitingForQuery => {
                if line_trimmed.starts_with("Query Text:") {
                    if let Some(plan) = current_plan {
                        plan.query_text = line_trimmed.strip_prefix("Query Text:").unwrap_or("").trim().to_string();
                    }
                }
            }
            ParsingStateEnum::ParsingPlan => {
                if !line_trimmed.is_empty() {
                    plan_lines.push(line_trimmed.to_string());
                }
            }
            _ => {}
        }
    }

    fn parse_timestamp(&self, timestamp_str: &str) -> Result<DateTime<Utc>, Box<dyn std::error::Error>> {
        use chrono::NaiveDateTime;
        
        let parts: Vec<&str> = timestamp_str.rsplitn(2, ' ').collect();
        if parts.len() != 2 {
            return Err("Invalid timestamp format".into());
        }
        
        let datetime_part = parts[1];
        let naive_dt = NaiveDateTime::parse_from_str(datetime_part, "%Y-%m-%d %H:%M:%S%.f")?;
        Ok(DateTime::from_naive_utc_and_offset(naive_dt, Utc))
    }
}