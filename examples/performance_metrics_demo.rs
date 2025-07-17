// Example demonstrating the new performance metrics features
use pg_loganalyze::parser_utils::QueryStatisticsCalculator;
use pg_loganalyze::models::QueryPlan;
use chrono::{Utc, TimeZone};

fn main() {
    // Example query executions with different timestamps and durations
    let executions = vec![
        QueryPlan {
            timestamp: Utc.with_ymd_and_hms(2024, 1, 1, 10, 30, 0).unwrap(),
            duration_ms: 150.0,
            query_text: "SELECT * FROM users WHERE active = true".to_string(),
            plan: "Index Scan on users".to_string(),
        },
        QueryPlan {
            timestamp: Utc.with_ymd_and_hms(2024, 1, 1, 10, 45, 0).unwrap(),
            duration_ms: 250.0,
            query_text: "SELECT * FROM users WHERE active = true".to_string(),
            plan: "Index Scan on users".to_string(),
        },
        QueryPlan {
            timestamp: Utc.with_ymd_and_hms(2024, 1, 1, 11, 15, 0).unwrap(),
            duration_ms: 300.0,
            query_text: "SELECT * FROM users WHERE active = true".to_string(),
            plan: "Index Scan on users".to_string(),
        },
        QueryPlan {
            timestamp: Utc.with_ymd_and_hms(2024, 1, 1, 11, 30, 0).unwrap(),
            duration_ms: 400.0,
            query_text: "SELECT * FROM users WHERE active = true".to_string(),
            plan: "Index Scan on users".to_string(),
        },
        QueryPlan {
            timestamp: Utc.with_ymd_and_hms(2024, 1, 1, 12, 0, 0).unwrap(),
            duration_ms: 500.0,
            query_text: "SELECT * FROM users WHERE active = true".to_string(),
            plan: "Index Scan on users".to_string(),
        },
    ];

    // Calculate performance percentiles
    let durations: Vec<f64> = executions.iter().map(|e| e.duration_ms).collect();
    let percentiles = QueryStatisticsCalculator::calculate_percentiles(&durations);

    println!("Performance Percentiles:");
    println!("  P50 (Median): {:.2}ms", percentiles.p50);
    println!("  P90:          {:.2}ms", percentiles.p90);
    println!("  P95:          {:.2}ms", percentiles.p95);
    println!("  P99:          {:.2}ms", percentiles.p99);
    
    // Generate hourly histogram
    let histogram = QueryStatisticsCalculator::generate_hourly_histogram(&executions);
    
    println!("\nHourly Performance Histogram:");
    let mut sorted_hours: Vec<_> = histogram.iter().collect();
    sorted_hours.sort_by_key(|(hour, _)| *hour);
    
    for (hour, metrics) in sorted_hours {
        println!("  {}: {} executions, avg: {:.2}ms, min: {:.2}ms, max: {:.2}ms", 
                 hour.format("%Y-%m-%d %H:00"), metrics.count, metrics.mean_duration_ms, 
                 metrics.min_duration_ms, metrics.max_duration_ms);
    }
    
    // Show additional statistics
    let (mean, std_dev) = QueryStatisticsCalculator::calculate_mean_and_std_dev(&durations);
    let (min, max) = QueryStatisticsCalculator::find_min_max(&durations);
    
    println!("\nAdditional Statistics:");
    println!("  Mean:     {:.2}ms", mean);
    println!("  Std Dev:  {:.2}ms", std_dev);
    println!("  Min:      {:.2}ms", min);
    println!("  Max:      {:.2}ms", max);
    println!("  Total:    {} executions", executions.len());
}