use std::fs;
use std::process::Command;
use tempfile::{NamedTempFile, tempdir};

#[test]
fn test_non_interactive_export_basic() {
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
    let export_path = export_file.path();

    // Run the command
    let output = Command::new("cargo")
        .args(&[
            "run",
            "--package",
            "pg-loganalyze",
            "--",
            log_path.to_str().unwrap(),
            "--export",
            export_path.to_str().unwrap(),
        ])
        .output()
        .expect("Failed to execute command");

    // Check exit status
    assert!(
        output.status.success(),
        "Command failed with stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify output contains expected messages
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Parsing logs in non-interactive mode"));
    assert!(stdout.contains("Found 1 log file(s) to process"));
    assert!(stdout.contains("Grouped into"));
    assert!(stdout.contains("Successfully exported analysis"));

    // Verify the export file exists and is valid JSON
    let export_content = fs::read_to_string(export_path).unwrap();
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

#[test]
fn test_non_interactive_export_with_date_filter() {
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
    let export_path = export_file.path();

    // Run with date filter (only entries after 11:00)
    let output = Command::new("cargo")
        .args(&[
            "run",
            "--package",
            "pg-loganalyze",
            "--",
            log_path.to_str().unwrap(),
            "--since",
            "2025-06-12T11:00:00",
            "--export",
            export_path.to_str().unwrap(),
        ])
        .output()
        .expect("Failed to execute command");

    assert!(output.status.success());

    let export_content = fs::read_to_string(export_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&export_content).unwrap();

    // Should only have 1 query (the one at 12:00)
    let query_count = parsed["query_count"].as_u64().unwrap();
    assert_eq!(query_count, 1, "Expected 1 query after filtering");
}

#[test]
fn test_non_interactive_export_no_log_files() {
    let export_file = NamedTempFile::new().unwrap();
    let export_path = export_file.path();

    // Run without log files
    let output = Command::new("cargo")
        .args(&[
            "run",
            "--package",
            "pg-loganalyze",
            "--",
            "/nonexistent/file.log",
            "--export",
            export_path.to_str().unwrap(),
        ])
        .output()
        .expect("Failed to execute command");

    // Should fail
    assert!(!output.status.success());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("No log files found") || stderr.contains("Error"));
}

#[test]
fn test_non_interactive_export_multiple_files() {
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
    let export_path = export_file.path();

    // Run with both log files
    let output = Command::new("cargo")
        .args(&[
            "run",
            "--package",
            "pg-loganalyze",
            "--",
            log1_path.to_str().unwrap(),
            log2_path.to_str().unwrap(),
            "--export",
            export_path.to_str().unwrap(),
        ])
        .output()
        .expect("Failed to execute command");

    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Found 2 log file(s) to process"));

    let export_content = fs::read_to_string(export_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&export_content).unwrap();

    // Should have metadata about both source files
    let source_files = parsed["metadata"]["source_files"].as_array().unwrap();
    assert_eq!(source_files.len(), 2);
}

#[test]
fn test_export_then_import_roundtrip() {
    let temp_dir = tempdir().unwrap();
    let log_path = temp_dir.path().join("test.log");

    let log_content = r#"2025-06-12 10:15:23.456 UTC [12345]: [1-1] user=testuser,db=testdb,app=psql LOG:  duration: 10.234 ms  plan:
	Query Text: SELECT * FROM users WHERE id = $1
	Index Scan using users_pkey on users  (cost=0.42..8.44 rows=1 width=100)
"#;

    fs::write(&log_path, log_content).unwrap();

    let export_file = NamedTempFile::new().unwrap();
    let export_path = export_file.path();

    // Export
    let export_output = Command::new("cargo")
        .args(&[
            "run",
            "--package",
            "pg-loganalyze",
            "--",
            log_path.to_str().unwrap(),
            "--export",
            export_path.to_str().unwrap(),
        ])
        .output()
        .expect("Failed to execute export");

    assert!(export_output.status.success());

    // Verify we can import (just check that import command accepts the file)
    // We can't fully test the TUI, but we can verify the file is valid for import
    use pg_loganalyze_core::AnalysisExport;
    let import_result = AnalysisExport::from_file(export_path);
    assert!(
        import_result.is_ok(),
        "Should be able to import exported file"
    );

    let imported = import_result.unwrap();
    assert_eq!(imported.query_count, 1);
}
