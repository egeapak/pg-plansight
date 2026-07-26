use pg_plansight_exporter::{Config, StateManager};
use std::fs;
use std::io::Write;
use tempfile::{NamedTempFile, TempDir};

/// Test state manager initialization
#[test]
fn test_state_manager_initialization() {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("test_state.db");

    let state_manager = StateManager::new(db_path.to_str().unwrap());
    let result = state_manager.initialize();

    assert!(result.is_ok());
    assert!(db_path.exists());
}

/// Test file state tracking
#[test]
fn test_file_state_tracking() {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("test_state.db");
    let log_file = temp_dir.path().join("test.log");

    fs::write(&log_file, "test content").unwrap();

    let state_manager = StateManager::new(db_path.to_str().unwrap());
    state_manager.initialize().unwrap();

    // Get initial state (should be None)
    let initial_state = state_manager.get_file_state(&log_file).unwrap();
    assert!(initial_state.is_none());

    // Update state
    let file_state = pg_plansight_exporter::FileState {
        file_path: log_file.clone(),
        last_position: 100,
        last_modified_time: 12345,
        file_size: 200,
        last_processed_at: chrono::Utc::now(),
        dev: None,
        ino: None,
    };

    state_manager.update_file_state(&file_state).unwrap();

    // Retrieve state
    let retrieved_state = state_manager.get_file_state(&log_file).unwrap();
    assert!(retrieved_state.is_some());

    let state = retrieved_state.unwrap();
    assert_eq!(state.last_position, 100);
    assert_eq!(state.file_size, 200);
}

/// Test query hash recording and retrieval
#[test]
fn test_query_hash_recording() {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("test_state.db");

    let state_manager = StateManager::new(db_path.to_str().unwrap());
    state_manager.initialize().unwrap();

    // Record query hash
    let query_hash = "abc123def456";
    let normalized_query = "SELECT * FROM users WHERE id = ?";

    state_manager
        .record_query_hash(query_hash, normalized_query)
        .unwrap();

    // Record same hash again (should be idempotent)
    state_manager
        .record_query_hash(query_hash, normalized_query)
        .unwrap();

    // Record different hash
    let query_hash2 = "def789ghi012";
    let normalized_query2 = "SELECT * FROM products WHERE category = ?";

    state_manager
        .record_query_hash(query_hash2, normalized_query2)
        .unwrap();
}

/// Test state reset functionality
#[test]
fn test_state_reset() {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("test_state.db");
    let log_file = temp_dir.path().join("test.log");

    fs::write(&log_file, "test content").unwrap();

    let state_manager = StateManager::new(db_path.to_str().unwrap());
    state_manager.initialize().unwrap();

    // Add some state
    let file_state = pg_plansight_exporter::FileState {
        file_path: log_file.clone(),
        last_position: 100,
        last_modified_time: 12345,
        file_size: 200,
        last_processed_at: chrono::Utc::now(),
        dev: None,
        ino: None,
    };

    state_manager.update_file_state(&file_state).unwrap();

    // Verify state exists
    let state_before = state_manager.get_file_state(&log_file).unwrap();
    assert!(state_before.is_some());

    // Reset state
    state_manager.reset_state().unwrap();

    // Verify state is cleared
    let state_after = state_manager.get_file_state(&log_file).unwrap();
    assert!(state_after.is_none());
}

/// Test getting all file states
#[test]
fn test_get_all_file_states() {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("test_state.db");

    let state_manager = StateManager::new(db_path.to_str().unwrap());
    state_manager.initialize().unwrap();

    // Add multiple file states
    for i in 0..3 {
        let log_file = temp_dir.path().join(format!("test{}.log", i));
        fs::write(&log_file, "test content").unwrap();

        let file_state = pg_plansight_exporter::FileState {
            file_path: log_file,
            last_position: i * 100,
            last_modified_time: 12345 + (i as i64),
            file_size: i * 200,
            last_processed_at: chrono::Utc::now(),
            dev: None,
            ino: None,
        };

        state_manager.update_file_state(&file_state).unwrap();
    }

    // Get all states
    let all_states = state_manager.get_all_file_states().unwrap();
    assert_eq!(all_states.len(), 3);
}

/// Test configuration loading from TOML
#[test]
fn test_config_loading_from_file() {
    let config_content = r#"
[server]
bind_address = "127.0.0.1:9091"
metrics_path = "/test-metrics"

[log_parsing]
log_paths = ["/var/log/test/*.log"]
poll_interval = "15s"
batch_size = 500

[metrics]
namespace = "test_namespace"
histogram_buckets = [0.001, 0.01, 0.1, 1.0]
slow_query_thresholds = ["500ms", "1s", "5s"]
retain_days = 14

[state]
database_path = "/tmp/test_state.db"

[filters]
exclude_query_patterns = ["^BEGIN$", "^COMMIT$"]
min_duration_ms = 50.0
"#;

    let mut temp_file = NamedTempFile::with_suffix(".toml").unwrap();
    temp_file.write_all(config_content.as_bytes()).unwrap();
    temp_file.flush().unwrap();

    let config = Config::load_from_file(&temp_file.path().to_path_buf()).unwrap();

    assert_eq!(config.server.bind_address, "127.0.0.1:9091");
    assert_eq!(config.server.metrics_path, "/test-metrics");
    assert_eq!(config.log_parsing.poll_interval, "15s");
    assert_eq!(config.log_parsing.batch_size, 500);
    assert_eq!(config.metrics.namespace, "test_namespace");
    assert_eq!(config.metrics.retain_days, 14);
    assert_eq!(config.state.database_path, "/tmp/test_state.db");

    let filters = config.filters.unwrap();
    assert!(
        filters.include_databases.is_none(),
        "include_databases is unsupported and must not appear in a valid config"
    );
    assert_eq!(filters.min_duration_ms.unwrap(), 50.0);
}

/// Test default configuration
#[test]
fn test_default_config() {
    let config = Config::default();

    // Default binds to loopback for safety (unauthenticated endpoint).
    assert_eq!(config.server.bind_address, "127.0.0.1:9090");
    assert_eq!(config.server.metrics_path, "/metrics");
    assert_eq!(config.log_parsing.poll_interval, "30s");
    assert_eq!(config.log_parsing.batch_size, 1000);
    // Size/query caps are opt-in (0 = unlimited) so incremental ingestion is
    // never silently halted on a large live log.
    assert_eq!(config.log_parsing.max_file_size_mb, 0);
    assert_eq!(config.log_parsing.max_queries_per_file, 0);
    assert_eq!(config.metrics.namespace, "pg_plansight");
    assert_eq!(config.metrics.retain_days, 7);
}

/// Test poll interval parsing
#[test]
fn test_poll_interval_parsing() {
    let config = Config::default();

    // Test seconds
    let config_with_seconds = Config {
        log_parsing: pg_plansight_exporter::LogParsingConfig {
            log_paths: vec![],
            poll_interval: "45s".to_string(),
            batch_size: 1000,
            max_file_size_mb: 0,
            max_queries_per_file: 0,
        },
        ..config.clone()
    };
    let duration = config_with_seconds.poll_interval_duration().unwrap();
    assert_eq!(duration.as_secs(), 45);

    // Test minutes
    let config_with_minutes = Config {
        log_parsing: pg_plansight_exporter::LogParsingConfig {
            log_paths: vec![],
            poll_interval: "2m".to_string(),
            batch_size: 1000,
            max_file_size_mb: 0,
            max_queries_per_file: 0,
        },
        ..config.clone()
    };
    let duration = config_with_minutes.poll_interval_duration().unwrap();
    assert_eq!(duration.as_secs(), 120);

    // Test hours
    let config_with_hours = Config {
        log_parsing: pg_plansight_exporter::LogParsingConfig {
            log_paths: vec![],
            poll_interval: "1h".to_string(),
            batch_size: 1000,
            max_file_size_mb: 0,
            max_queries_per_file: 0,
        },
        ..config
    };
    let duration = config_with_hours.poll_interval_duration().unwrap();
    assert_eq!(duration.as_secs(), 3600);
}

/// Test state persistence across restarts
#[test]
fn test_state_persistence_across_restarts() {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("test_state.db");
    let log_file = temp_dir.path().join("test.log");

    fs::write(&log_file, "test content").unwrap();

    // First session: create state manager and save state
    {
        let state_manager = StateManager::new(db_path.to_str().unwrap());
        state_manager.initialize().unwrap();

        let file_state = pg_plansight_exporter::FileState {
            file_path: log_file.clone(),
            last_position: 100,
            last_modified_time: 12345,
            file_size: 200,
            last_processed_at: chrono::Utc::now(),
            dev: None,
            ino: None,
        };

        state_manager.update_file_state(&file_state).unwrap();
    }

    // Second session: create new state manager with same DB
    {
        let state_manager = StateManager::new(db_path.to_str().unwrap());
        state_manager.initialize().unwrap();

        // Retrieve previously saved state
        let retrieved_state = state_manager.get_file_state(&log_file).unwrap();
        assert!(retrieved_state.is_some());

        let state = retrieved_state.unwrap();
        assert_eq!(state.last_position, 100);
        assert_eq!(state.file_size, 200);
    }
}

/// Test configuration validation
#[test]
fn test_invalid_config_handling() {
    let invalid_config = r#"
[server]
bind_address = "invalid address"

[log_parsing]
poll_interval = "invalid"
"#;

    let mut temp_file = NamedTempFile::with_suffix(".toml").unwrap();
    temp_file.write_all(invalid_config.as_bytes()).unwrap();
    temp_file.flush().unwrap();

    // Should still load the config (validation happens at runtime)
    let config_result = Config::load_from_file(&temp_file.path().to_path_buf());

    // Config loading might succeed but poll_interval_duration should fail
    if let Ok(config) = config_result {
        let duration_result = config.poll_interval_duration();
        assert!(duration_result.is_err());
    }
}
