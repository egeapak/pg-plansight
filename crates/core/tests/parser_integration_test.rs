#![cfg(feature = "file-io")]
#[cfg(feature = "parallel")]
use pg_plansight_core::DateFilter;
use pg_plansight_core::PostgreSQLLogParser;
use std::io::Write;
use tempfile::NamedTempFile;

/// Test parsing a complete text-format execution plan
#[test]
fn test_parse_text_plan_end_to_end() {
    let log_content = r#"2025-06-25 00:03:51.601 UTC [687287] LOG:  duration: 3680.828 ms  plan:
Query Text: SELECT v."AcceptanceId", v."MeasuredDate", v."VentilatorId"
FROM "Shared"."VentilatorHourlyCaches" AS v
WHERE v."AcceptanceId" = ANY ($1) AND v."MeasuredDate" >= $2
Index Scan using "PK_VentilatorHourlyCaches" on "Shared"."VentilatorHourlyCaches" v  (cost=0.42..851.21 rows=822 width=16)
  Output: "AcceptanceId", "MeasuredDate", "VentilatorId"
  Index Cond: ((v."AcceptanceId" = ANY ('{322,319,1062,1100,1258,1259,1406,1304}'::integer[])) AND (v."MeasuredDate" >= '2025-06-15 00:03:47.919275+00'::timestamp with time zone))
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

    assert!(plan.query_text().contains("VentilatorHourlyCaches"));
    assert_eq!(plan.duration_ms(), 3680.828);
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
    let log_content = r#"2025-06-25 00:03:51.601 UTC [687287] LOG:  duration: 3680.828 ms  plan:
Query Text: SELECT v."AcceptanceId", v."MeasuredDate", v."VentilatorId"
FROM "Shared"."VentilatorHourlyCaches" AS v
WHERE v."AcceptanceId" = ANY ($1) AND v."MeasuredDate" >= $2
Index Scan using "PK_VentilatorHourlyCaches" on "Shared"."VentilatorHourlyCaches" v  (cost=0.42..851.21 rows=822 width=16)
  Output: "AcceptanceId", "MeasuredDate", "VentilatorId"
  Index Cond: ((v."AcceptanceId" = ANY ('{322,319,1062,1100,1258,1259,1406,1304}'::integer[])) AND (v."MeasuredDate" >= '2025-06-15 00:03:47.919275+00'::timestamp with time zone))

2025-06-25 00:03:56.432 UTC [687212] LOG:  duration: 9542.752 ms  plan:
Query Text: SELECT b."Id", b."AcceptanceId", b."CreatedDate", b."DeviceName", b."IsActive", b."MeasuredDate", b."SampleType", b."TestId"
FROM "Shared"."BloodGasDevices" AS b
INNER JOIN "Shared"."Acceptances" AS a ON b."AcceptanceId" = a."Id"
WHERE b."IsActive" AND b."AcceptanceId" = ANY ($1) AND b."MeasuredDate" >= $2
Nested Loop  (cost=0.71..20009.76 rows=61 width=72)
  Output: b."Id", b."AcceptanceId", b."CreatedDate", b."DeviceName", b."IsActive", b."MeasuredDate", b."SampleType", b."TestId"
  Inner Unique: true
  ->  Index Scan using "IX_BloodGasDevices_AcceptanceId" on "Shared"."BloodGasDevices" b  (cost=0.43..19969.76 rows=61 width=72)
        Output: b."Id", b."AcceptanceId", b."CreatedDate", b."DeviceName", b."IsActive", b."MeasuredDate", b."SampleType", b."TestId"
        Index Cond: (b."AcceptanceId" = ANY ('{335,360}'::integer[]))
        Filter: (b."IsActive" AND (b."MeasuredDate" >= '2025-06-15 00:03:46.887634+00'::timestamp with time zone))
  ->  Index Only Scan using "PK_Acceptances" on "Shared"."Acceptances" a  (cost=0.28..0.66 rows=1 width=4)
        Output: a."Id"
        Index Cond: (a."Id" = b."AcceptanceId")

2025-06-25 00:03:25.377 UTC [687282] LOG:  duration: 2244.493 ms  plan:
Query Text: SELECT m."Id", m."AcceptanceId", m."CreatedDate", m."DeviceName", m."IsValidated", m."MeasuredDate", m."ValidatedById", m."ValidationDate", m0."Id", m0."Comment", m0."DeviceId", m0."MeasurementTypeId", m0."Value"
FROM "Shared"."Monitors" AS m
LEFT JOIN "Shared"."MonitorMeasurements" AS m0 ON m."Id" = m0."DeviceId"
WHERE m."AcceptanceId" = $1 AND m."MeasuredDate" >= $2 AND m."MeasuredDate" <= $3
ORDER BY m."Id"
Sort  (cost=279.91..279.93 rows=7 width=110)
  Output: m."Id", m."AcceptanceId", m."CreatedDate", m."DeviceName", m."IsValidated", m."MeasuredDate", m."ValidatedById", m."ValidationDate", m0."Id", m0."Comment", m0."DeviceId", m0."MeasurementTypeId", m0."Value"
  Sort Key: m."Id"
  ->  Nested Loop Left Join  (cost=1.15..279.82 rows=7 width=110)
        Output: m."Id", m."AcceptanceId", m."CreatedDate", m."DeviceName", m."IsValidated", m."MeasuredDate", m."ValidatedById", m."ValidationDate", m0."Id", m0."Comment", m0."DeviceId", m0."MeasurementTypeId", m0."Value"
        ->  Index Scan using "IX_Monitors_AcceptanceId" on "Shared"."Monitors" m  (cost=0.57..2.79 rows=1 width=54)
              Output: m."Id", m."AcceptanceId", m."CreatedDate", m."MeasuredDate", m."DeviceName", m."IsValidated", m."ValidatedById", m."ValidationDate"
              Index Cond: (m."AcceptanceId" = 1395)
              Filter: ((m."MeasuredDate" >= '2025-06-25 00:01:20.259+00'::timestamp with time zone) AND (m."MeasuredDate" <= '2025-06-25 00:03:20.259+00'::timestamp with time zone))
        ->  Index Scan using "IX_MonitorMeasurements_DeviceId" on "Shared"."MonitorMeasurements" m0  (cost=0.57..274.10 rows=292 width=56)
              Output: m0."Id", m0."Comment", m0."DeviceId", m0."MeasurementTypeId", m0."Value"
              Index Cond: (m0."DeviceId" = m."Id")
"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(log_content.as_bytes()).unwrap();
    temp_file.flush().unwrap();

    let mut parser = PostgreSQLLogParser::new();
    let plans = parser
        .parse_file_with_progress(temp_file.path(), |_, _| {})
        .expect("Failed to parse log file");

    assert_eq!(plans.len(), 3);
    assert_eq!(plans[0].duration_ms(), 3680.828);
    assert_eq!(plans[1].duration_ms(), 9542.752);
    assert_eq!(plans[2].duration_ms(), 2244.493);
}

/// Test query normalization and statistics calculation
#[test]
fn test_query_normalization_and_statistics() {
    let log_content = r#"2025-06-25 00:03:51.601 UTC [687287] LOG:  duration: 15.0 ms  plan:
Query Text: SELECT v."AcceptanceId", v."MeasuredDate", v."VentilatorId"
FROM "Shared"."VentilatorHourlyCaches" AS v
WHERE v."AcceptanceId" = ANY ($1) AND v."MeasuredDate" >= $2
Index Scan using "PK_VentilatorHourlyCaches" on "Shared"."VentilatorHourlyCaches" v  (cost=0.42..851.21 rows=822 width=16)
  Output: "AcceptanceId", "MeasuredDate", "VentilatorId"

2025-06-25 00:03:52.001 UTC [687288] LOG:  duration: 20.0 ms  plan:
Query Text: SELECT v."AcceptanceId", v."MeasuredDate", v."VentilatorId"
FROM "Shared"."VentilatorHourlyCaches" AS v
WHERE v."AcceptanceId" = ANY ($1) AND v."MeasuredDate" >= $2
Index Scan using "PK_VentilatorHourlyCaches" on "Shared"."VentilatorHourlyCaches" v  (cost=0.42..851.21 rows=822 width=16)
  Output: "AcceptanceId", "MeasuredDate", "VentilatorId"

2025-06-25 00:03:52.401 UTC [687289] LOG:  duration: 25.0 ms  plan:
Query Text: SELECT v."AcceptanceId", v."MeasuredDate", v."VentilatorId"
FROM "Shared"."VentilatorHourlyCaches" AS v
WHERE v."AcceptanceId" = ANY ($1) AND v."MeasuredDate" >= $2
Index Scan using "PK_VentilatorHourlyCaches" on "Shared"."VentilatorHourlyCaches" v  (cost=0.42..851.21 rows=822 width=16)
  Output: "AcceptanceId", "MeasuredDate", "VentilatorId"

2025-06-25 00:03:52.801 UTC [687290] LOG:  duration: 30.0 ms  plan:
Query Text: SELECT v."AcceptanceId", v."MeasuredDate", v."VentilatorId"
FROM "Shared"."VentilatorHourlyCaches" AS v
WHERE v."AcceptanceId" = ANY ($1) AND v."MeasuredDate" >= $2
Index Scan using "PK_VentilatorHourlyCaches" on "Shared"."VentilatorHourlyCaches" v  (cost=0.42..851.21 rows=822 width=16)
  Output: "AcceptanceId", "MeasuredDate", "VentilatorId"
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

    // Normalized query should contain the table name
    assert!(query.normalized_query().contains("VentilatorHourlyCaches"));

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

    let log_content = r#"2025-06-25 00:03:51.601 UTC [687287] LOG:  duration: 3680.828 ms  plan:
Query Text: SELECT v."AcceptanceId", v."MeasuredDate", v."VentilatorId"
FROM "Shared"."VentilatorHourlyCaches" AS v
WHERE v."AcceptanceId" = ANY ($1) AND v."MeasuredDate" >= $2
Index Scan using "PK_VentilatorHourlyCaches" on "Shared"."VentilatorHourlyCaches" v  (cost=0.42..851.21 rows=822 width=16)
  Output: "AcceptanceId", "MeasuredDate", "VentilatorId"
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
    assert_eq!(plans[0].duration_ms(), 3680.828);
}

/// Test date filtering with parse_multiple_files_async
#[cfg(feature = "parallel")]
#[test]
fn test_date_filtering_with_async_parser() {
    use chrono::{TimeZone, Utc};

    let log_content = r#"2025-06-25 00:01:00.000 UTC [687287] LOG:  duration: 15.0 ms  plan:
Query Text: SELECT v."AcceptanceId", v."MeasuredDate", v."VentilatorId"
FROM "Shared"."VentilatorHourlyCaches" AS v
WHERE v."AcceptanceId" = ANY ($1) AND v."MeasuredDate" >= $2
Index Scan using "PK_VentilatorHourlyCaches" on "Shared"."VentilatorHourlyCaches" v  (cost=0.42..851.21 rows=822 width=16)

2025-06-25 00:02:30.000 UTC [687288] LOG:  duration: 20.0 ms  plan:
Query Text: SELECT b."Id", b."AcceptanceId"
FROM "Shared"."BloodGasDevices" AS b
WHERE b."IsActive" AND b."AcceptanceId" = ANY ($1)
Index Scan using "IX_BloodGasDevices_AcceptanceId" on "Shared"."BloodGasDevices" b  (cost=0.43..851.21 rows=61 width=72)

2025-06-25 00:04:00.000 UTC [687289] LOG:  duration: 25.0 ms  plan:
Query Text: SELECT m."Id", m."AcceptanceId"
FROM "Shared"."Monitors" AS m
WHERE m."AcceptanceId" = $1
Index Scan using "IX_Monitors_AcceptanceId" on "Shared"."Monitors" m  (cost=0.57..2.79 rows=1 width=54)
"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(log_content.as_bytes()).unwrap();
    temp_file.flush().unwrap();

    // Filter to only get logs between 00:02:00 and 00:03:00
    let since = Utc.with_ymd_and_hms(2025, 6, 25, 0, 2, 0).unwrap();
    let until = Utc.with_ymd_and_hms(2025, 6, 25, 0, 3, 0).unwrap();
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
        if let pg_plansight_core::ParseProgress::Complete { result } = msg {
            plans = result.unwrap();
            break;
        }
    }

    // Should only get the middle query (00:02:30)
    assert_eq!(plans.len(), 1);
    assert!(plans[0].query_text().contains("BloodGasDevices"));
}

/// Test error handling for malformed log entries
#[test]
fn test_malformed_log_handling() {
    let log_content = r#"This is not a valid log entry
Some random text
2025-06-25 00:03:51.601 UTC [687287] LOG:  duration: 3680.828 ms  plan:
Query Text: SELECT v."AcceptanceId", v."MeasuredDate", v."VentilatorId"
FROM "Shared"."VentilatorHourlyCaches" AS v
WHERE v."AcceptanceId" = ANY ($1) AND v."MeasuredDate" >= $2
Index Scan using "PK_VentilatorHourlyCaches" on "Shared"."VentilatorHourlyCaches" v  (cost=0.42..851.21 rows=822 width=16)
  Output: "AcceptanceId", "MeasuredDate", "VentilatorId"
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
    let log_content = r#"2025-06-25 00:03:51.601 UTC [687287] LOG:  duration: 3680.828 ms  plan:
Query Text: SELECT v."AcceptanceId", v."MeasuredDate", v."VentilatorId"
FROM "Shared"."VentilatorHourlyCaches" AS v
WHERE v."AcceptanceId" = ANY ($1) AND v."MeasuredDate" >= $2
Index Scan using "PK_VentilatorHourlyCaches" on "Shared"."VentilatorHourlyCaches" v  (cost=0.42..851.21 rows=822 width=16)
  Output: "AcceptanceId", "MeasuredDate", "VentilatorId"
"#;

    let mut parser = PostgreSQLLogParser::new();
    let plans = parser
        .parse_string_with_progress(log_content, |_, _| {})
        .unwrap();

    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].duration_ms(), 3680.828);
}

/// Test large duration values
#[test]
fn test_large_duration_values() {
    let log_content = r#"2025-06-25 00:03:56.432 UTC [687212] LOG:  duration: 9542.752 ms  plan:
Query Text: SELECT b."Id", b."AcceptanceId", b."CreatedDate", b."DeviceName", b."IsActive", b."MeasuredDate", b."SampleType", b."TestId"
FROM "Shared"."BloodGasDevices" AS b
INNER JOIN "Shared"."Acceptances" AS a ON b."AcceptanceId" = a."Id"
WHERE b."IsActive" AND b."AcceptanceId" = ANY ($1) AND b."MeasuredDate" >= $2
Nested Loop  (cost=0.71..20009.76 rows=61 width=72)
  Output: b."Id", b."AcceptanceId", b."CreatedDate", b."DeviceName", b."IsActive", b."MeasuredDate", b."SampleType", b."TestId"
  Inner Unique: true
  ->  Index Scan using "IX_BloodGasDevices_AcceptanceId" on "Shared"."BloodGasDevices" b  (cost=0.43..19969.76 rows=61 width=72)
        Index Cond: (b."AcceptanceId" = ANY ('{335,360}'::integer[]))
  ->  Index Only Scan using "PK_Acceptances" on "Shared"."Acceptances" a  (cost=0.28..0.66 rows=1 width=4)
        Index Cond: (a."Id" = b."AcceptanceId")
"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(log_content.as_bytes()).unwrap();
    temp_file.flush().unwrap();

    let mut parser = PostgreSQLLogParser::new();
    let plans = parser
        .parse_file_with_progress(temp_file.path(), |_, _| {})
        .unwrap();

    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].duration_ms(), 9542.752);
}

/// Test incremental file range parsing
#[test]
fn test_file_range_parsing() {
    let log_content = r#"2025-06-25 00:03:51.601 UTC [687287] LOG:  duration: 3680.828 ms  plan:
Query Text: SELECT v."AcceptanceId", v."MeasuredDate", v."VentilatorId"
FROM "Shared"."VentilatorHourlyCaches" AS v
WHERE v."AcceptanceId" = ANY ($1) AND v."MeasuredDate" >= $2
Index Scan using "PK_VentilatorHourlyCaches" on "Shared"."VentilatorHourlyCaches" v  (cost=0.42..851.21 rows=822 width=16)
  Output: "AcceptanceId", "MeasuredDate", "VentilatorId"

2025-06-25 00:03:56.432 UTC [687212] LOG:  duration: 9542.752 ms  plan:
Query Text: SELECT b."Id", b."AcceptanceId"
FROM "Shared"."BloodGasDevices" AS b
WHERE b."IsActive" AND b."AcceptanceId" = ANY ($1)
Nested Loop  (cost=0.71..20009.76 rows=61 width=72)
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
