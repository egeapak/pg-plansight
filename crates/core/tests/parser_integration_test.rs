use pg_loganalyze_core::{DateFilter, PostgreSQLLogParser};
use std::io::Write;
use tempfile::NamedTempFile;

/// Test parsing a complete text-format execution plan
#[test]
fn test_parse_text_plan_end_to_end() {
    let log_content = r#"2024-11-07 10:15:23.456 UTC [12345]: user@database LOG:  duration: 15.234 ms  plan:
Query Text: SELECT * FROM users WHERE id = $1;
Seq Scan on users  (cost=0.00..15.50 rows=1 width=100)
  Filter: (id = $1)
"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(log_content.as_bytes()).unwrap();
    temp_file.flush().unwrap();

    let mut parser = PostgreSQLLogParser::new();
    let plans = parser
        .parse_file_with_progress(temp_file.path(), |_, _| {})
        .expect("Failed to parse log file");

    assert_eq!(plans.len(), 1);
    let plan = &plans[0];

    assert_eq!(plan.query_text(), "SELECT * FROM users WHERE id = $1;");
    assert_eq!(plan.duration_ms(), 15.234);
    assert!(plan.is_text_plan());
}

/// Test parsing JSON-format execution plan
#[test]
fn test_parse_json_plan_end_to_end() {
    let log_content = r#"2024-11-07 10:15:23.456 UTC [12345]: user@database LOG:  duration: 25.678 ms  plan:
Query Text: SELECT * FROM orders WHERE customer_id = $1;
[
  {
    "Plan": {
      "Node Type": "Seq Scan",
      "Relation Name": "orders",
      "Alias": "orders",
      "Startup Cost": 0.00,
      "Total Cost": 25.50,
      "Plan Rows": 10,
      "Plan Width": 200
    },
    "Planning Time": 0.123,
    "Execution Time": 25.555
  }
]
"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(log_content.as_bytes()).unwrap();
    temp_file.flush().unwrap();

    let mut parser = PostgreSQLLogParser::new();
    let plans = parser
        .parse_file_with_progress(temp_file.path(), |_, _| {})
        .expect("Failed to parse log file");

    assert_eq!(plans.len(), 1);
    let plan = &plans[0];

    assert_eq!(
        plan.query_text(),
        "SELECT * FROM orders WHERE customer_id = $1;"
    );
    assert_eq!(plan.duration_ms(), 25.678);
    assert!(plan.is_json_plan());
}

/// Test parsing multiple queries from a single file
#[test]
fn test_parse_multiple_queries() {
    let log_content = r#"2024-11-07 10:15:23.456 UTC [12345]: user@database LOG:  duration: 15.234 ms  plan:
Query Text: SELECT * FROM users WHERE id = $1;
Seq Scan on users  (cost=0.00..15.50 rows=1 width=100)

2024-11-07 10:15:24.567 UTC [12346]: user@database LOG:  duration: 20.456 ms  plan:
Query Text: SELECT * FROM products WHERE category = $1;
Index Scan using idx_category on products  (cost=0.29..8.31 rows=1 width=50)
  Index Cond: (category = $1)

2024-11-07 10:15:25.678 UTC [12347]: user@database LOG:  duration: 30.789 ms  plan:
Query Text: SELECT COUNT(*) FROM orders;
Aggregate  (cost=150.00..150.01 rows=1 width=8)
  ->  Seq Scan on orders  (cost=0.00..140.00 rows=1000 width=0)
"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(log_content.as_bytes()).unwrap();
    temp_file.flush().unwrap();

    let mut parser = PostgreSQLLogParser::new();
    let plans = parser
        .parse_file_with_progress(temp_file.path(), |_, _| {})
        .expect("Failed to parse log file");

    assert_eq!(plans.len(), 3);
    assert_eq!(plans[0].duration_ms(), 15.234);
    assert_eq!(plans[1].duration_ms(), 20.456);
    assert_eq!(plans[2].duration_ms(), 30.789);
}

/// Test query normalization and statistics calculation
#[test]
fn test_query_normalization_and_statistics() {
    let log_content = r#"2024-11-07 10:15:23.456 UTC [12345]: user@database LOG:  duration: 15.0 ms  plan:
Query Text: SELECT * FROM users WHERE id = $1;
Seq Scan on users  (cost=0.00..15.50 rows=1 width=100)

2024-11-07 10:15:24.567 UTC [12346]: user@database LOG:  duration: 20.0 ms  plan:
Query Text: SELECT * FROM users WHERE id = $1;
Seq Scan on users  (cost=0.00..15.50 rows=1 width=100)

2024-11-07 10:15:25.678 UTC [12347]: user@database LOG:  duration: 25.0 ms  plan:
Query Text: SELECT * FROM users WHERE id = $1;
Seq Scan on users  (cost=0.00..15.50 rows=1 width=100)

2024-11-07 10:15:26.789 UTC [12348]: user@database LOG:  duration: 30.0 ms  plan:
Query Text: SELECT * FROM users WHERE id = $1;
Seq Scan on users  (cost=0.00..15.50 rows=1 width=100)
"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(log_content.as_bytes()).unwrap();
    temp_file.flush().unwrap();

    let mut parser = PostgreSQLLogParser::new();
    let plans = parser
        .parse_file_with_progress(temp_file.path(), |_, _| {})
        .unwrap();
    let processed = parser.get_processed_queries(&plans);

    assert_eq!(processed.len(), 1);
    let query = processed.values().next().unwrap();

    // Normalized query should replace $1, $2, etc. with ?
    assert_eq!(query.normalized_query(), "SELECT * FROM users WHERE id = ?;");

    // Check statistics
    assert_eq!(query.statistics.count, 4);
    assert_eq!(query.statistics.min_duration_ms, 15.0);
    assert_eq!(query.statistics.max_duration_ms, 30.0);
    assert_eq!(query.statistics.mean_duration_ms, 22.5); // (15 + 20 + 25 + 30) / 4

    // Check percentiles
    assert!(query.statistics.percentiles.p50 >= 20.0);
    assert!(query.statistics.percentiles.p95 >= 25.0);
}

/// Test compressed file support (gzip)
#[test]
fn test_parse_gzipped_log_file() {
    use flate2::Compression;
    use flate2::write::GzEncoder;

    let log_content = r#"2024-11-07 10:15:23.456 UTC [12345]: user@database LOG:  duration: 15.234 ms  plan:
Query Text: SELECT * FROM users WHERE id = $1;
Seq Scan on users  (cost=0.00..15.50 rows=1 width=100)
"#;

    // Create a gzipped file
    let mut temp_file = NamedTempFile::with_suffix(".gz").unwrap();
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(log_content.as_bytes()).unwrap();
    let compressed = encoder.finish().unwrap();
    temp_file.write_all(&compressed).unwrap();
    temp_file.flush().unwrap();

    let mut parser = PostgreSQLLogParser::new();
    let plans = parser
        .parse_file_with_progress(temp_file.path(), |_, _| {})
        .expect("Failed to parse gzipped log file");

    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].duration_ms(), 15.234);
}

/// Test date filtering with parse_multiple_files_async
#[test]
fn test_date_filtering_with_async_parser() {
    use chrono::{TimeZone, Utc};

    let log_content = r#"2024-11-07 10:15:23.456 UTC [12345]: user@database LOG:  duration: 15.0 ms  plan:
Query Text: SELECT * FROM users WHERE id = $1;
Seq Scan on users  (cost=0.00..15.50 rows=1 width=100)

2024-11-07 12:30:45.678 UTC [12346]: user@database LOG:  duration: 20.0 ms  plan:
Query Text: SELECT * FROM products WHERE id = $1;
Seq Scan on products  (cost=0.00..20.50 rows=1 width=100)

2024-11-07 15:45:12.890 UTC [12347]: user@database LOG:  duration: 25.0 ms  plan:
Query Text: SELECT * FROM orders WHERE id = $1;
Seq Scan on orders  (cost=0.00..25.50 rows=1 width=100)
"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(log_content.as_bytes()).unwrap();
    temp_file.flush().unwrap();

    // Filter to only get logs between 12:00 and 14:00
    let since = Utc.with_ymd_and_hms(2024, 11, 7, 12, 0, 0).unwrap();
    let until = Utc.with_ymd_and_hms(2024, 11, 7, 14, 0, 0).unwrap();
    let filter = DateFilter {
        since: Some(since),
        until: Some(until),
    };

    // Use async parser with date filter
    let rx = PostgreSQLLogParser::parse_multiple_files_async(
        vec![temp_file.path().to_path_buf()],
        filter,
    );

    // Collect results
    let mut plans = Vec::new();
    for msg in rx {
        if let pg_loganalyze_core::ParseProgress::Complete { result } = msg {
            plans = result.unwrap();
            break;
        }
    }

    // Should only get the middle query (12:30)
    assert_eq!(plans.len(), 1);
    assert_eq!(
        plans[0].query_text(),
        "SELECT * FROM products WHERE id = $1;"
    );
}

/// Test error handling for malformed log entries
#[test]
fn test_malformed_log_handling() {
    let log_content = r#"This is not a valid log entry
Some random text
2024-11-07 10:15:23.456 UTC [12345]: user@database LOG:  duration: 15.234 ms  plan:
Query Text: SELECT * FROM users WHERE id = $1;
Seq Scan on users  (cost=0.00..15.50 rows=1 width=100)
More invalid text
Another invalid line
"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(log_content.as_bytes()).unwrap();
    temp_file.flush().unwrap();

    let mut parser = PostgreSQLLogParser::new();
    let result = parser.parse_file_with_progress(temp_file.path(), |_, _| {});

    // Should successfully parse valid entry despite malformed lines
    assert!(result.is_ok());
    let plans = result.unwrap();
    assert_eq!(plans.len(), 1);
}

/// Test query hash consistency
#[test]
fn test_query_hash_consistency() {
    use xxhash_rust::xxh3::xxh3_64;

    let query1 = "SELECT * FROM users WHERE id = ?;";
    let query2 = "SELECT * FROM users WHERE id = ?;";
    let query3 = "SELECT * FROM products WHERE id = ?;";

    let hash1 = xxh3_64(query1.as_bytes());
    let hash2 = xxh3_64(query2.as_bytes());
    let hash3 = xxh3_64(query3.as_bytes());

    // Same queries should have same hash
    assert_eq!(hash1, hash2);

    // Different queries should have different hash
    assert_ne!(hash1, hash3);
}

/// Test empty file handling
#[test]
fn test_empty_file_parsing() {
    let temp_file = NamedTempFile::new().unwrap();
    let mut parser = PostgreSQLLogParser::new();
    let plans = parser
        .parse_file_with_progress(temp_file.path(), |_, _| {})
        .unwrap();

    assert_eq!(plans.len(), 0);
}

/// Test parsing from string
#[test]
fn test_parse_from_string() {
    let log_content = r#"2024-11-07 10:15:23.456 UTC [12345]: user@database LOG:  duration: 15.234 ms  plan:
Query Text: SELECT * FROM users WHERE id = $1;
Seq Scan on users  (cost=0.00..15.50 rows=1 width=100)
"#;

    let mut parser = PostgreSQLLogParser::new();
    let plans = parser
        .parse_string_with_progress(log_content, |_, _| {})
        .unwrap();

    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].duration_ms(), 15.234);
}

/// Test large duration values
#[test]
fn test_large_duration_values() {
    let log_content = r#"2024-11-07 10:15:23.456 UTC [12345]: user@database LOG:  duration: 123456.789 ms  plan:
Query Text: SELECT * FROM users WHERE id = $1;
Seq Scan on users  (cost=0.00..15.50 rows=1 width=100)
"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(log_content.as_bytes()).unwrap();
    temp_file.flush().unwrap();

    let mut parser = PostgreSQLLogParser::new();
    let plans = parser
        .parse_file_with_progress(temp_file.path(), |_, _| {})
        .unwrap();

    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].duration_ms(), 123456.789);
}

/// Test incremental file range parsing
#[test]
fn test_file_range_parsing() {
    let log_content = r#"2024-11-07 10:15:23.456 UTC [12345]: user@database LOG:  duration: 15.0 ms  plan:
Query Text: SELECT * FROM users WHERE id = $1;
Seq Scan on users  (cost=0.00..15.50 rows=1 width=100)

2024-11-07 10:15:24.567 UTC [12346]: user@database LOG:  duration: 20.0 ms  plan:
Query Text: SELECT * FROM products WHERE id = $1;
Seq Scan on products  (cost=0.00..20.50 rows=1 width=100)
"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(log_content.as_bytes()).unwrap();
    temp_file.flush().unwrap();

    let mut parser = PostgreSQLLogParser::new();
    let file_size = std::fs::metadata(temp_file.path()).unwrap().len();

    // Parse entire file using range
    let plans = parser
        .parse_file_range_with_progress(temp_file.path(), 0, Some(file_size), |_, _| {})
        .unwrap();

    assert_eq!(plans.len(), 2);
}
