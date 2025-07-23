use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, Timelike, Utc};
use rayon::iter::{IntoParallelRefIterator, ParallelIterator as _};
use regex::Regex;
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;
use xxhash_rust::xxh64::Xxh64;

use crate::PlanLine;
use crate::models::{HourlyMetrics, PerformancePercentiles, QueryPlan};

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
    let mut hasher = Xxh64::new(0);
    hasher.update(normalized_query.as_bytes());
    hasher.digest()
}

pub fn parse_timestamp(timestamp_str: &str) -> anyhow::Result<DateTime<Utc>> {
    let naive_dt = NaiveDateTime::parse_from_str(timestamp_str, "%Y-%m-%d %H:%M:%S%.f")?;
    Ok(DateTime::from_naive_utc_and_offset(naive_dt, Utc))
}

pub fn parse_relative_date(date_str: &str) -> anyhow::Result<DateTime<Utc>> {
    let now = Utc::now();

    // Try parsing as absolute timestamp first
    if let Ok(dt) = parse_absolute_timestamp(date_str) {
        return Ok(dt);
    }

    // Parse relative time format (e.g., "2h", "3d", "1w")
    let regex = Regex::new(r"^(\d+)([smhdw])$")?;
    if let Some(caps) = regex.captures(date_str) {
        let amount: i64 = caps.get(1).unwrap().as_str().parse()?;
        let unit = caps.get(2).unwrap().as_str();

        let duration = match unit {
            "s" => Duration::seconds(amount),
            "m" => Duration::minutes(amount),
            "h" => Duration::hours(amount),
            "d" => Duration::days(amount),
            "w" => Duration::weeks(amount),
            _ => return Err(anyhow::anyhow!("Invalid time unit: {}", unit)),
        };

        return Ok(now - duration);
    }

    Err(anyhow::anyhow!("Invalid date format: {}", date_str))
}

fn parse_absolute_timestamp(date_str: &str) -> anyhow::Result<DateTime<Utc>> {
    // Try various timestamp formats
    let formats = [
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M:%S%.f",
    ];

    for format in &formats {
        if let Ok(naive_dt) = NaiveDateTime::parse_from_str(date_str, format) {
            return Ok(DateTime::from_naive_utc_and_offset(naive_dt, Utc));
        }
    }

    // Try date-only format
    if let Ok(naive_date) = NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
        let naive_dt = naive_date.and_hms_opt(0, 0, 0).unwrap();
        return Ok(DateTime::from_naive_utc_and_offset(naive_dt, Utc));
    }

    Err(anyhow::anyhow!(
        "Could not parse absolute timestamp: {}",
        date_str
    ))
}

pub fn get_indent_level(line: &str) -> usize {
    line.chars().take_while(|c| c.is_whitespace()).count()
}

pub fn format_plan_lines(plan_lines: &[PlanLine]) -> String {
    let mut plan = String::new();

    plan_lines.iter().for_each(|pl| {
        writeln!(
            &mut plan,
            "{:indent$}{content}",
            "",
            content = pl.query,
            indent = pl.indentation / 2
        )
        .unwrap();
    });

    if !plan.is_empty() {
        plan.pop();
    }

    plan
}

pub fn format_sql_query(sql: &str) -> String {
    let dialect = PostgreSqlDialect {};

    match Parser::parse_sql(&dialect, sql) {
        Ok(statements) => {
            let mut formatted = String::new();
            for (i, statement) in statements.iter().enumerate() {
                if i > 0 {
                    formatted.push('\n');
                }
                // Use built-in pretty-printing with {:#} format specifier
                formatted.push_str(&format!("{:#}", statement));
            }
            formatted
        }
        Err(_) => sql.to_string(), // Return original if parsing fails
    }
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

    pub fn calculate_percentiles(durations: &[f64]) -> PerformancePercentiles {
        if durations.is_empty() {
            return PerformancePercentiles {
                p25: 0.0,
                p50: 0.0,
                p90: 0.0,
                p95: 0.0,
                p99: 0.0,
            };
        }

        let mut sorted_durations = durations.to_vec();
        sorted_durations.sort_by(|a, b| a.total_cmp(b));

        PerformancePercentiles {
            p25: Self::percentile(&sorted_durations, 25.0),
            p50: Self::percentile(&sorted_durations, 50.0),
            p90: Self::percentile(&sorted_durations, 90.0),
            p95: Self::percentile(&sorted_durations, 95.0),
            p99: Self::percentile(&sorted_durations, 99.0),
        }
    }

    fn percentile(sorted_data: &[f64], percentile: f64) -> f64 {
        if sorted_data.is_empty() {
            return 0.0;
        }

        let index = (percentile / 100.0) * (sorted_data.len() - 1) as f64;
        let lower_index = index.floor() as usize;
        let upper_index = index.ceil() as usize;

        if lower_index == upper_index {
            sorted_data[lower_index]
        } else {
            let weight = index - lower_index as f64;
            sorted_data[lower_index] * (1.0 - weight) + sorted_data[upper_index] * weight
        }
    }

    pub fn generate_hourly_histogram(
        executions: &[QueryPlan],
    ) -> HashMap<DateTime<Utc>, HourlyMetrics> {
        let mut histogram = HashMap::new();

        for execution in executions {
            // Truncate to hour precision (set minutes, seconds, nanoseconds to 0)
            let hour_key = execution
                .timestamp
                .with_minute(0)
                .unwrap()
                .with_second(0)
                .unwrap()
                .with_nanosecond(0)
                .unwrap();

            let entry = histogram.entry(hour_key).or_insert(HourlyMetrics {
                count: 0,
                total_duration_ms: 0.0,
                min_duration_ms: execution.duration_ms,
                max_duration_ms: execution.duration_ms,
                mean_duration_ms: 0.0,
            });

            entry.count += 1;
            entry.total_duration_ms += execution.duration_ms;
            entry.min_duration_ms = entry.min_duration_ms.min(execution.duration_ms);
            entry.max_duration_ms = entry.max_duration_ms.max(execution.duration_ms);
        }

        // Calculate mean for each hour
        for metrics in histogram.values_mut() {
            metrics.mean_duration_ms = metrics.total_duration_ms / metrics.count as f64;
        }

        histogram
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

    #[test]
    fn test_parse_relative_date() {
        // Test relative dates
        assert!(parse_relative_date("2h").is_ok());
        assert!(parse_relative_date("3d").is_ok());
        assert!(parse_relative_date("1w").is_ok());
        assert!(parse_relative_date("30m").is_ok());
        assert!(parse_relative_date("45s").is_ok());

        // Test invalid formats
        assert!(parse_relative_date("2x").is_err());
        assert!(parse_relative_date("invalid").is_err());
        assert!(parse_relative_date("").is_err());

        // Test absolute dates
        assert!(parse_relative_date("2024-01-01T10:30:00").is_ok());
        assert!(parse_relative_date("2024-01-01 10:30:00").is_ok());
        assert!(parse_relative_date("2024-01-01").is_ok());
    }

    #[test]
    fn test_calculate_percentiles() {
        let durations = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];
        let percentiles = QueryStatisticsCalculator::calculate_percentiles(&durations);

        assert_eq!(percentiles.p25, 3.25); // 25th percentile
        assert_eq!(percentiles.p50, 5.5); // Median
        assert!((percentiles.p90 - 9.1).abs() < 0.001);
        assert!((percentiles.p95 - 9.55).abs() < 0.001);
        assert!((percentiles.p99 - 9.91).abs() < 0.001);

        // Test empty case
        let empty_percentiles = QueryStatisticsCalculator::calculate_percentiles(&[]);
        assert_eq!(empty_percentiles.p25, 0.0);
        assert_eq!(empty_percentiles.p50, 0.0);
        assert_eq!(empty_percentiles.p90, 0.0);
        assert_eq!(empty_percentiles.p95, 0.0);
        assert_eq!(empty_percentiles.p99, 0.0);
    }

    #[test]
    fn test_percentile_calculation() {
        // Test with single value
        let single = vec![42.0];
        let percentiles = QueryStatisticsCalculator::calculate_percentiles(&single);
        assert_eq!(percentiles.p25, 42.0);
        assert_eq!(percentiles.p50, 42.0);
        assert_eq!(percentiles.p90, 42.0);
        assert_eq!(percentiles.p95, 42.0);
        assert_eq!(percentiles.p99, 42.0);

        // Test with two values
        let two = vec![10.0, 20.0];
        let percentiles = QueryStatisticsCalculator::calculate_percentiles(&two);
        assert_eq!(percentiles.p50, 15.0); // Average of 10 and 20
    }

    #[test]
    fn test_generate_hourly_histogram() {
        use chrono::TimeZone;

        let executions = vec![
            QueryPlan {
                timestamp: Utc.with_ymd_and_hms(2024, 1, 1, 10, 30, 0).unwrap(),
                duration_ms: 100.0,
                query_text: "SELECT 1".to_string(),
                plan: "Plan 1".to_string(),
                plan_lines: vec![],
            },
            QueryPlan {
                timestamp: Utc.with_ymd_and_hms(2024, 1, 1, 10, 45, 0).unwrap(),
                duration_ms: 200.0,
                query_text: "SELECT 2".to_string(),
                plan: "Plan 2".to_string(),
                plan_lines: vec![],
            },
            QueryPlan {
                timestamp: Utc.with_ymd_and_hms(2024, 1, 1, 11, 15, 0).unwrap(),
                duration_ms: 300.0,
                query_text: "SELECT 3".to_string(),
                plan: "Plan 3".to_string(),
                plan_lines: vec![],
            },
        ];

        let histogram = QueryStatisticsCalculator::generate_hourly_histogram(&executions);

        // Check 10th hour
        let hour_10_key = Utc.with_ymd_and_hms(2024, 1, 1, 10, 0, 0).unwrap();
        let hour_10 = histogram.get(&hour_10_key).unwrap();
        assert_eq!(hour_10.count, 2);
        assert_eq!(hour_10.total_duration_ms, 300.0);
        assert_eq!(hour_10.min_duration_ms, 100.0);
        assert_eq!(hour_10.max_duration_ms, 200.0);
        assert_eq!(hour_10.mean_duration_ms, 150.0);

        // Check 11th hour
        let hour_11_key = Utc.with_ymd_and_hms(2024, 1, 1, 11, 0, 0).unwrap();
        let hour_11 = histogram.get(&hour_11_key).unwrap();
        assert_eq!(hour_11.count, 1);
        assert_eq!(hour_11.total_duration_ms, 300.0);
        assert_eq!(hour_11.min_duration_ms, 300.0);
        assert_eq!(hour_11.max_duration_ms, 300.0);
        assert_eq!(hour_11.mean_duration_ms, 300.0);

        // Should have exactly 2 hours
        assert_eq!(histogram.len(), 2);

        // Verify keys are properly truncated to hour precision
        for (hour_key, _) in histogram.iter() {
            assert_eq!(hour_key.minute(), 0);
            assert_eq!(hour_key.second(), 0);
            assert_eq!(hour_key.nanosecond(), 0);
        }
    }
}
