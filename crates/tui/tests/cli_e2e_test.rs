//! End-to-end tests that drive the actual `pg-plansight` binary (via
//! CARGO_BIN_EXE), so main.rs's argument parsing, date-filter wiring, and the
//! non-interactive export path are exercised as shipped — not reimplemented in
//! test helpers that can drift from the real code.

use std::path::PathBuf;
use std::process::Command;

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_pg-plansight"))
}

fn fixture_log(dir: &std::path::Path) -> PathBuf {
    let log = "2025-01-15 10:00:00.000 UTC [1] LOG:  duration: 10.0 ms  plan:\n\tQuery Text: SELECT 1\n\tResult  (cost=0.00..0.01 rows=1 width=4)\n2025-01-15 11:00:00.000 UTC [1] LOG:  duration: 20.0 ms  plan:\n\tQuery Text: SELECT 2\n\tResult  (cost=0.00..0.02 rows=1 width=4)\n2025-01-15 11:00:01.000 UTC [1] LOG:  done\n";
    let path = dir.join("e2e.log");
    std::fs::write(&path, log).unwrap();
    path
}

#[test]
fn export_mode_writes_valid_export_json() {
    let dir = tempfile::tempdir().unwrap();
    let log_path = fixture_log(dir.path());
    let out_path = dir.path().join("analysis.json");

    let output = binary()
        .arg(&log_path)
        .arg("--export")
        .arg(&out_path)
        .output()
        .expect("binary must run");

    assert!(
        output.status.success(),
        "export run failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let exported: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out_path).unwrap())
            .expect("export must be valid JSON");
    // SELECT 1 / SELECT 2 normalize to the same shape: one group, two runs.
    assert_eq!(exported["query_count"], 1);
    assert_eq!(exported["execution_count"], 2);
}

#[test]
fn export_with_since_filter_limits_entries() {
    let dir = tempfile::tempdir().unwrap();
    let log_path = fixture_log(dir.path());
    let out_path = dir.path().join("filtered.json");

    let output = binary()
        .arg(&log_path)
        .arg("--since")
        .arg("2025-01-15T10:30:00")
        .arg("--export")
        .arg(&out_path)
        .output()
        .expect("binary must run");

    assert!(output.status.success());
    let exported: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out_path).unwrap()).unwrap();
    assert_eq!(
        exported["execution_count"], 1,
        "--since must exclude the earlier entry"
    );
}

#[test]
fn export_without_log_files_fails() {
    let output = binary()
        .arg("--export")
        .arg("/tmp/x.json")
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[test]
fn import_conflicts_with_log_files() {
    let output = binary()
        .arg("some.log")
        .arg("--import")
        .arg("analysis.json")
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "--import combined with log files must be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot be used with") || stderr.contains("conflicts"),
        "expected a clap conflict error, got: {stderr}"
    );
}

#[test]
fn import_conflicts_with_export() {
    let output = binary()
        .arg("--import")
        .arg("a.json")
        .arg("--export")
        .arg("b.json")
        .output()
        .unwrap();
    assert!(!output.status.success());
}
