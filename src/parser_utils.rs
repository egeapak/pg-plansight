use chrono::{DateTime, NaiveDateTime, Utc};
use rayon::iter::{IntoParallelRefIterator, ParallelIterator as _};
use regex::Regex;
use std::borrow::Cow;
use std::collections::hash_map::DefaultHasher;
use std::fmt::Write as _;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use crate::PlanLine;

#[derive(Debug)]
pub struct RegexPatterns {
    pub log_line_regex: Regex,
    pub duration_regex: Regex,
    pub plan_regex: Regex,
    pub placeholder_regex: Regex,
}

impl RegexPatterns {
    pub fn new() -> Self {
        Self {
            log_line_regex: Regex::new(r"^(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3})(.*)")
                .unwrap(),
            duration_regex: Regex::new(r"duration: ([\d.]+) ms\s+plan:\s*$").unwrap(),
            plan_regex: Regex::new(r"\(cost=[\d.]+\.\.[\d.]+\s+rows=\d+\s+width=\d+\)").unwrap(),
            placeholder_regex: Regex::new(r"\$\d+").unwrap(),
        }
    }
}

impl Default for RegexPatterns {
    fn default() -> Self {
        Self::new()
    }
}

pub fn normalize_query<'q>(query: &'q str, placeholder_regex: &Regex) -> Cow<'q, str> {
    let query = query.trim();
    placeholder_regex.replace_all(query, "?")
}

pub fn calculate_query_hash(normalized_query: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    normalized_query.hash(&mut hasher);
    hasher.finish()
}

pub fn parse_timestamp(timestamp_str: &str) -> anyhow::Result<DateTime<Utc>> {
    let naive_dt = NaiveDateTime::parse_from_str(timestamp_str, "%Y-%m-%d %H:%M:%S%.f")?;
    Ok(DateTime::from_naive_utc_and_offset(naive_dt, Utc))
}

pub fn get_indent_level(line: &str) -> usize {
    line.chars().take_while(|c| c.is_whitespace()).count()
}

pub fn format_plan_lines(plan_lines: &[PlanLine]) -> String {
    let mut plan = String::new();

    plan_lines.iter().for_each(|pl| {
        write!(
            &mut plan,
            "{:indent$}{content}\n",
            "",
            content = pl.query,
            indent = pl.indentation / 2
        );
    });

    if !plan.is_empty() {
        plan.pop();
    }

    plan
}

pub fn format_sql_query(sql: &str) -> String {
    let format_options = sqlformat::FormatOptions {
        indent: sqlformat::Indent::Spaces(4),
        uppercase: true,
        lines_between_queries: 1,
    };
    sqlformat::format(sql, &sqlformat::QueryParams::None, format_options)
}

pub struct QueryStatisticsCalculator;

impl QueryStatisticsCalculator {
    pub fn calculate_mean_and_std_dev(durations: &[f64]) -> (f64, f64) {
        if durations.is_empty() {
            return (0.0, 0.0);
        }

        let mean = durations.par_iter().sum::<f64>() / durations.len() as f64;
        let variance = durations
            .par_iter()
            .map(|&d| (d - mean).powi(2))
            .sum::<f64>()
            / durations.len() as f64;
        let std_dev = variance.sqrt();

        (mean, std_dev)
    }

    pub fn find_min_max(durations: &[f64]) -> (f64, f64) {
        if durations.is_empty() {
            return (0.0, 0.0);
        }

        let min = durations.par_iter().min_by(|a, b| a.total_cmp(b)).unwrap();
        let max = durations.par_iter().max_by(|a, b| a.total_cmp(b)).unwrap();

        (*min, *max)
    }
}

pub fn parse_duration_from_line(line: &str, duration_regex: &Regex) -> Option<f64> {
    duration_regex
        .captures(line)
        .and_then(|caps| caps.get(1))
        .and_then(|m| m.as_str().parse().ok())
}

fn expand_path(folder_path: &PathBuf) -> Vec<PathBuf> {
    if !folder_path.exists() {
        return vec![];
    }
    if folder_path.is_dir() {
        fs::read_dir(folder_path)
            .unwrap()
            .flatten()
            .flat_map(|entry| expand_path(&entry.path()))
            .collect()
    } else {
        vec![folder_path.clone()]
    }
}

pub fn expand_files(file_paths: &[PathBuf]) -> Vec<PathBuf> {
    file_paths
        .iter()
        .filter_map(|p| std::fs::canonicalize(p).as_ref().map(expand_path).ok())
        .flatten()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_query() {
        let patterns = RegexPatterns::new();
        let query = "SELECT * FROM users WHERE id = $1 AND status = $2";
        let normalized = normalize_query(query, &patterns.placeholder_regex);
        assert_eq!(
            normalized,
            "SELECT * FROM users WHERE id = ? AND status = ?"
        );
    }

    #[test]
    fn test_get_indent_level() {
        assert_eq!(get_indent_level("    test"), 4);
        assert_eq!(get_indent_level("\t\ttest"), 2);
        assert_eq!(get_indent_level("test"), 0);
    }

    #[test]
    fn test_parse_timestamp() {
        let timestamp_str = "2024-01-01 10:30:45.123";
        let result = parse_timestamp(timestamp_str);
        assert!(result.is_ok());
    }

    #[test]
    fn test_statistics_calculation() {
        let durations = vec![100.0, 200.0, 300.0, 400.0, 500.0];
        let (mean, std_dev) = QueryStatisticsCalculator::calculate_mean_and_std_dev(&durations);
        assert_eq!(mean, 300.0);
        assert!((std_dev - 141.42).abs() < 0.1);

        let (min, max) = QueryStatisticsCalculator::find_min_max(&durations);
        assert_eq!(min, 100.0);
        assert_eq!(max, 500.0);
    }
}
