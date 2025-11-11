use std::fs;
use std::path::PathBuf;
use tempfile::{NamedTempFile, tempdir};
use pg_loganalyze_core::{
    AnalysisExport, DateFilter, ParseProgress, PostgreSQLLogParser, expand_files,
};

/// Helper function that mimics the non-interactive export functionality
async fn export_logs_to_file(
    log_files: Vec<PathBuf>,
    date_filter: DateFilter,
    export_path: PathBuf,
) -> Result<(), String> {
    // Expand file patterns
    let expanded_files = expand_files(&log_files);

    if expanded_files.is_empty() {
        return Err("No log files found to parse".to_string());
    }

    // Parse all log files
    let rx = PostgreSQLLogParser::parse_multiple_files_async(expanded_files.clone(), date_filter);

    // Consume progress messages until we get the final result
    let plans = loop {
        match rx.recv() {
            Ok(ParseProgress::Progress { .. }) => {}
            Ok(ParseProgress::Error { file_path, error, .. }) => {
                eprintln!("Error parsing {}: {}", file_path.display(), error);
            }
            Ok(ParseProgress::Complete { result }) => {
                break result.map_err(|e| e.to_string())?;
            }
            Err(_) => {
                return Err("Parse channel closed unexpectedly".to_string());
            }
        }
    };

    // Get processed queries with statistics
    let mut parser = PostgreSQLLogParser::new();
    let processed_queries = parser.get_processed_queries(&plans);

    // Convert hashbrown::HashMap to std::HashMap for export
    let std_queries: std::collections::HashMap<_, _> = processed_queries.into_iter().collect();

    // Create export with source file names
    let source_files: Vec<String> = expanded_files
        .iter()
        .map(|p| p.display().to_string())
        .collect();

    let export = AnalysisExport::from_processed_queries(std_queries, source_files);

    // Export to file
    export.to_file(&export_path).map_err(|e| e.to_string())?;

    Ok(())
}

#[tokio::test]
async fn test_non_interactive_export_basic() {
    // Create a temporary log file with sample data
    let temp_dir = tempdir().unwrap();
    let log_path = temp_dir.path().join("test.log");

    let log_content = r#"2025-06-12 10:15:23.456 UTC [12345]: [1-1] user=testuser,db=testdb,app=psql LOG:  duration: 10.234 ms  plan:
	Query Text: SELECT * FROM users WHERE id = $1
	Index Scan using users_pkey on users  (cost=0.42..8.44 rows=1 width=100)
	  Index Cond: (id = '123'::integer)
2025-06-12 10:15:24.789 UTC [12346]: [1-1] user=testuser,db=testdb,app=psql LOG:  duration: 15.678 ms  plan:
	Query Text: SELECT * FROM orders WHERE user_id = $1
	Index Scan using orders_user_id_idx on orders  (cost=0.42..12.55 rows=10 width=200)
	  Index Cond: (user_id = '123'::integer)
"#;

    fs::write(&log_path, log_content).unwrap();

    // Create temporary export file
    let export_file = NamedTempFile::new().unwrap();
    let export_path = export_file.path().to_path_buf();

    // Export using library API directly
    let result = export_logs_to_file(
        vec![log_path],
        DateFilter::new(None, None),
        export_path.clone(),
    ).await;

    assert!(result.is_ok(), "Export failed: {:?}", result.err());

    // Verify the export file exists and is valid JSON
    let export_content = fs::read_to_string(&export_path).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&export_content).expect("Export file is not valid JSON");

    // Verify expected fields
    assert!(parsed.get("version").is_some());
    assert!(parsed.get("query_count").is_some());
    assert!(parsed.get("execution_count").is_some());
    assert!(parsed.get("queries").is_some());
    assert!(parsed.get("metadata").is_some());

    // Verify query count
    let query_count = parsed["query_count"].as_u64().unwrap();
    assert_eq!(query_count, 2, "Expected 2 unique queries");

    let execution_count = parsed["execution_count"].as_u64().unwrap();
    assert_eq!(execution_count, 2, "Expected 2 total executions");
}

#[tokio::test]
async fn test_non_interactive_export_with_date_filter() {
    use chrono::DateTime;

    let temp_dir = tempdir().unwrap();
    let log_path = temp_dir.path().join("test.log");

    let log_content = r#"2025-06-12 10:15:23.456 UTC [12345]: [1-1] user=testuser,db=testdb,app=psql LOG:  duration: 10.234 ms  plan:
	Query Text: SELECT * FROM users WHERE id = $1
	Index Scan using users_pkey on users  (cost=0.42..8.44 rows=1 width=100)
2025-06-12 12:00:00.000 UTC [12346]: [1-1] user=testuser,db=testdb,app=psql LOG:  duration: 15.678 ms  plan:
	Query Text: SELECT * FROM orders WHERE user_id = $1
	Index Scan using orders_user_id_idx on orders  (cost=0.42..12.55 rows=10 width=200)
"#;

    fs::write(&log_path, log_content).unwrap();

    let export_file = NamedTempFile::new().unwrap();
    let export_path = export_file.path().to_path_buf();

    // Create date filter (only entries after 11:00)
    let since = DateTime::parse_from_rfc3339("2025-06-12T11:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let date_filter = DateFilter::new(Some(since), None);

    let result = export_logs_to_file(
        vec![log_path],
        date_filter,
        export_path.clone(),
    ).await;

    assert!(result.is_ok());

    let export_content = fs::read_to_string(&export_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&export_content).unwrap();

    // Should only have 1 query (the one at 12:00)
    let query_count = parsed["query_count"].as_u64().unwrap();
    assert_eq!(query_count, 1, "Expected 1 query after filtering");
}

#[tokio::test]
async fn test_non_interactive_export_no_log_files() {
    let export_file = NamedTempFile::new().unwrap();
    let export_path = export_file.path().to_path_buf();

    // Try to export nonexistent files
    let result = export_logs_to_file(
        vec![PathBuf::from("/nonexistent/file.log")],
        DateFilter::new(None, None),
        export_path,
    ).await;

    // Should fail
    assert!(result.is_err());
    let err_msg = result.unwrap_err();
    assert!(
        err_msg.contains("No log files found") || err_msg.contains("Error"),
        "Expected error message about missing files, got: {}",
        err_msg
    );
}

#[tokio::test]
async fn test_non_interactive_export_multiple_files() {
    let temp_dir = tempdir().unwrap();

    // Create two log files
    let log1_path = temp_dir.path().join("test1.log");
    let log2_path = temp_dir.path().join("test2.log");

    let log1_content = r#"2025-06-12 10:15:23.456 UTC [12345]: [1-1] user=testuser,db=testdb,app=psql LOG:  duration: 10.234 ms  plan:
	Query Text: SELECT * FROM users WHERE id = $1
	Index Scan using users_pkey on users  (cost=0.42..8.44 rows=1 width=100)
"#;

    let log2_content = r#"2025-06-12 10:15:24.789 UTC [12346]: [1-1] user=testuser,db=testdb,app=psql LOG:  duration: 15.678 ms  plan:
	Query Text: SELECT * FROM orders WHERE user_id = $1
	Index Scan using orders_user_id_idx on orders  (cost=0.42..12.55 rows=10 width=200)
"#;

    fs::write(&log1_path, log1_content).unwrap();
    fs::write(&log2_path, log2_content).unwrap();

    let export_file = NamedTempFile::new().unwrap();
    let export_path = export_file.path().to_path_buf();

    // Export with both log files
    let result = export_logs_to_file(
        vec![log1_path, log2_path],
        DateFilter::new(None, None),
        export_path.clone(),
    ).await;

    assert!(result.is_ok());

    let export_content = fs::read_to_string(&export_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&export_content).unwrap();

    // Should have metadata about both source files
    let source_files = parsed["metadata"]["source_files"].as_array().unwrap();
    assert_eq!(source_files.len(), 2);
}

#[tokio::test]
async fn test_export_then_import_roundtrip() {
    let temp_dir = tempdir().unwrap();
    let log_path = temp_dir.path().join("test.log");

    let log_content = r#"2025-06-12 10:15:23.456 UTC [12345]: [1-1] user=testuser,db=testdb,app=psql LOG:  duration: 10.234 ms  plan:
	Query Text: SELECT * FROM users WHERE id = $1
	Index Scan using users_pkey on users  (cost=0.42..8.44 rows=1 width=100)
"#;

    fs::write(&log_path, log_content).unwrap();

    let export_file = NamedTempFile::new().unwrap();
    let export_path = export_file.path().to_path_buf();

    // Export using library API
    let export_result = export_logs_to_file(
        vec![log_path],
        DateFilter::new(None, None),
        export_path.clone(),
    ).await;

    assert!(export_result.is_ok());

    // Verify we can import (just check that import command accepts the file)
    // We can't fully test the TUI, but we can verify the file is valid for import
    let import_result = AnalysisExport::from_file(&export_path);
    assert!(
        import_result.is_ok(),
        "Should be able to import exported file"
    );

    let imported = import_result.unwrap();
    assert_eq!(imported.query_count, 1);
}
