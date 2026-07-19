use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct FileState {
    pub file_path: PathBuf,
    pub last_position: u64,
    pub last_modified_time: i64,
    pub file_size: u64,
    pub last_processed_at: DateTime<Utc>,
}

pub struct StateManager {
    db_path: PathBuf,
}

impl StateManager {
    pub fn new(db_path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: db_path.into(),
        }
    }

    pub fn initialize(&self) -> Result<()> {
        // Create parent directory if it doesn't exist
        if let Some(parent) = self.db_path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create state directory: {}", parent.display())
            })?;
        }

        let conn = self.connect()?;
        self.create_tables(&conn)?;
        Ok(())
    }

    fn connect(&self) -> Result<Connection> {
        Connection::open(&self.db_path)
            .with_context(|| format!("Failed to open SQLite database: {}", self.db_path.display()))
    }

    fn create_tables(&self, conn: &Connection) -> Result<()> {
        conn.execute(
            "CREATE TABLE IF NOT EXISTS processed_files (
                file_path TEXT PRIMARY KEY,
                last_position INTEGER NOT NULL,
                last_modified_time INTEGER NOT NULL,
                file_size INTEGER NOT NULL,
                last_processed_at TEXT NOT NULL
            )",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS query_hashes (
                query_hash TEXT PRIMARY KEY,
                normalized_query TEXT NOT NULL,
                first_seen_at TEXT NOT NULL,
                last_seen_at TEXT NOT NULL
            )",
            [],
        )?;

        Ok(())
    }

    pub fn get_file_state(&self, file_path: &Path) -> Result<Option<FileState>> {
        let conn = self.connect()?;

        let result = conn
            .query_row(
                "SELECT file_path, last_position, last_modified_time, file_size, last_processed_at 
             FROM processed_files WHERE file_path = ?1",
                params![file_path.to_string_lossy()],
                |row| {
                    Ok(FileState {
                        file_path: PathBuf::from(row.get::<_, String>(0)?),
                        last_position: row.get::<_, i64>(1)? as u64,
                        last_modified_time: row.get(2)?,
                        file_size: row.get::<_, i64>(3)? as u64,
                        last_processed_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(4)?)
                            .map_err(|_| {
                                rusqlite::Error::InvalidColumnType(
                                    4,
                                    "timestamp".to_string(),
                                    rusqlite::types::Type::Text,
                                )
                            })?
                            .with_timezone(&Utc),
                    })
                },
            )
            .optional()?;

        Ok(result)
    }

    pub fn update_file_state(&self, state: &FileState) -> Result<()> {
        let conn = self.connect()?;

        conn.execute(
            "INSERT OR REPLACE INTO processed_files 
             (file_path, last_position, last_modified_time, file_size, last_processed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                state.file_path.to_string_lossy(),
                state.last_position as i64,
                state.last_modified_time,
                state.file_size as i64,
                state.last_processed_at.to_rfc3339(),
            ],
        )?;

        Ok(())
    }

    pub fn get_all_file_states(&self) -> Result<HashMap<PathBuf, FileState>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT file_path, last_position, last_modified_time, file_size, last_processed_at 
             FROM processed_files",
        )?;

        let rows = stmt.query_map([], |row| {
            let file_path = PathBuf::from(row.get::<_, String>(0)?);
            let state = FileState {
                file_path: file_path.clone(),
                last_position: row.get::<_, i64>(1)? as u64,
                last_modified_time: row.get(2)?,
                file_size: row.get::<_, i64>(3)? as u64,
                last_processed_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(4)?)
                    .map_err(|_| {
                        rusqlite::Error::InvalidColumnType(
                            4,
                            "timestamp".to_string(),
                            rusqlite::types::Type::Text,
                        )
                    })?
                    .with_timezone(&Utc),
            };
            Ok((file_path, state))
        })?;

        let mut states = HashMap::new();
        for row in rows {
            let (path, state) = row?;
            states.insert(path, state);
        }

        Ok(states)
    }

    /// Record a query hash sighting, returning the persisted
    /// `(first_seen, last_seen)` timestamps. `first_seen` is preserved across
    /// re-inserts (COALESCE), while `last_seen` advances to the current time.
    pub fn record_query_hash(
        &self,
        hash: &str,
        normalized_query: &str,
    ) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
        let conn = self.connect()?;
        let now = Utc::now().to_rfc3339();

        let (first_seen_str, last_seen_str): (String, String) = conn.query_row(
            "INSERT OR REPLACE INTO query_hashes
             (query_hash, normalized_query, first_seen_at, last_seen_at)
             VALUES (?1, ?2,
                     COALESCE((SELECT first_seen_at FROM query_hashes WHERE query_hash = ?1), ?3),
                     ?3)
             RETURNING first_seen_at, last_seen_at",
            params![hash, normalized_query, now],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        let first_seen = DateTime::parse_from_rfc3339(&first_seen_str)
            .with_context(|| format!("Invalid first_seen_at timestamp: {}", first_seen_str))?
            .with_timezone(&Utc);
        let last_seen = DateTime::parse_from_rfc3339(&last_seen_str)
            .with_context(|| format!("Invalid last_seen_at timestamp: {}", last_seen_str))?
            .with_timezone(&Utc);

        Ok((first_seen, last_seen))
    }

    /// Delete one file's checkpoint row.
    pub fn delete_file_state(&self, file_path: &Path) -> Result<()> {
        let conn = self.connect()?;
        conn.execute(
            "DELETE FROM processed_files WHERE file_path = ?1",
            params![file_path.to_string_lossy()],
        )?;
        Ok(())
    }

    /// Delete query-hash rows not seen since `older_than`, returning the count.
    pub fn cleanup_old_query_hashes(&self, older_than: DateTime<Utc>) -> Result<usize> {
        let conn = self.connect()?;
        let deleted = conn.execute(
            "DELETE FROM query_hashes WHERE last_seen_at < ?1",
            params![older_than.to_rfc3339()],
        )?;
        Ok(deleted)
    }

    pub fn cleanup_old_states(&self, older_than: DateTime<Utc>) -> Result<usize> {
        let conn = self.connect()?;
        let cutoff = older_than.to_rfc3339();

        let deleted = conn.execute(
            "DELETE FROM processed_files WHERE last_processed_at < ?1",
            params![cutoff],
        )?;

        // Also cleanup old query hashes
        conn.execute(
            "DELETE FROM query_hashes WHERE last_seen_at < ?1",
            params![cutoff],
        )?;

        Ok(deleted)
    }

    pub fn reset_state(&self) -> Result<()> {
        let conn = self.connect()?;
        conn.execute("DELETE FROM processed_files", [])?;
        conn.execute("DELETE FROM query_hashes", [])?;
        Ok(())
    }

    pub fn get_last_run_timestamp(&self) -> Result<DateTime<Utc>> {
        let conn = self.connect()?;

        let timestamp: Option<i64> = conn.query_row(
            "SELECT MAX(last_modified_time) FROM processed_files",
            [],
            |row| row.get(0),
        )?;

        match timestamp {
            Some(ts) => Ok(DateTime::from_timestamp(ts, 0).unwrap_or_else(Utc::now)),
            None => Ok(Utc::now() - chrono::Duration::hours(24)), // Default to 24h ago if no files processed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn setup_manager() -> (StateManager, tempfile::TempDir) {
        let temp_dir = tempdir().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let manager = StateManager::new(&db_path);
        manager.initialize().unwrap();
        (manager, temp_dir)
    }

    #[test]
    fn test_state_manager_basic_operations() -> Result<()> {
        let temp_dir = tempdir()?;
        let db_path = temp_dir.path().join("test.db");
        let manager = StateManager::new(&db_path);

        manager.initialize()?;

        let test_path = PathBuf::from("/test/log.log");
        let state = FileState {
            file_path: test_path.clone(),
            last_position: 1024,
            last_modified_time: 1234567890,
            file_size: 2048,
            last_processed_at: Utc::now(),
        };

        // Initially should be None
        assert!(manager.get_file_state(&test_path)?.is_none());

        // Update state
        manager.update_file_state(&state)?;

        // Should now return the state
        let retrieved = manager.get_file_state(&test_path)?.unwrap();
        assert_eq!(retrieved.file_path, state.file_path);
        assert_eq!(retrieved.last_position, state.last_position);
        assert_eq!(retrieved.file_size, state.file_size);

        Ok(())
    }

    // -------------------------------------------------------------------------
    // cleanup_old_states
    // -------------------------------------------------------------------------

    #[test]
    fn test_cleanup_old_states_removes_old_rows() -> Result<()> {
        let (manager, _dir) = setup_manager();

        let old_time = Utc::now() - chrono::Duration::days(10);
        let recent_time = Utc::now();

        let old_state = FileState {
            file_path: PathBuf::from("/old/log.log"),
            last_position: 0,
            last_modified_time: 0,
            file_size: 0,
            last_processed_at: old_time,
        };
        let recent_state = FileState {
            file_path: PathBuf::from("/recent/log.log"),
            last_position: 0,
            last_modified_time: 0,
            file_size: 0,
            last_processed_at: recent_time,
        };

        manager.update_file_state(&old_state)?;
        manager.update_file_state(&recent_state)?;

        // Cut off at 5 days ago — should delete the 10-day-old entry
        let cutoff = Utc::now() - chrono::Duration::days(5);
        let deleted = manager.cleanup_old_states(cutoff)?;

        assert_eq!(
            deleted, 1,
            "exactly one file state should have been deleted"
        );

        // Old entry gone
        assert!(
            manager
                .get_file_state(&PathBuf::from("/old/log.log"))?
                .is_none()
        );
        // Recent entry still present
        assert!(
            manager
                .get_file_state(&PathBuf::from("/recent/log.log"))?
                .is_some()
        );

        Ok(())
    }

    #[test]
    fn test_cleanup_old_states_returns_zero_when_nothing_to_delete() -> Result<()> {
        let (manager, _dir) = setup_manager();

        let recent_state = FileState {
            file_path: PathBuf::from("/recent/log.log"),
            last_position: 0,
            last_modified_time: 0,
            file_size: 0,
            last_processed_at: Utc::now(),
        };
        manager.update_file_state(&recent_state)?;

        // Cut off at 5 days ago — nothing is that old
        let cutoff = Utc::now() - chrono::Duration::days(5);
        let deleted = manager.cleanup_old_states(cutoff)?;

        assert_eq!(deleted, 0);

        Ok(())
    }

    // -------------------------------------------------------------------------
    // get_last_run_timestamp
    // -------------------------------------------------------------------------

    #[test]
    fn test_get_last_run_timestamp_empty_db_returns_approx_24h_ago() {
        let (manager, _dir) = setup_manager();

        let result = manager.get_last_run_timestamp().unwrap();
        let expected = Utc::now() - chrono::Duration::hours(24);
        let diff = (result - expected).num_seconds().abs();
        assert!(
            diff < 5,
            "empty DB should return ~24h ago, got diff of {}s",
            diff
        );
    }

    #[test]
    fn test_get_last_run_timestamp_returns_max_modified_time() -> Result<()> {
        let (manager, _dir) = setup_manager();

        let t1: i64 = 1_700_000_000;
        let t2: i64 = 1_700_100_000; // newer

        manager.update_file_state(&FileState {
            file_path: PathBuf::from("/a.log"),
            last_position: 0,
            last_modified_time: t1,
            file_size: 0,
            last_processed_at: Utc::now(),
        })?;
        manager.update_file_state(&FileState {
            file_path: PathBuf::from("/b.log"),
            last_position: 0,
            last_modified_time: t2,
            file_size: 0,
            last_processed_at: Utc::now(),
        })?;

        let ts = manager.get_last_run_timestamp()?;

        // The returned timestamp should correspond to t2 (the higher of the two)
        assert_eq!(ts.timestamp(), t2);

        Ok(())
    }

    // -------------------------------------------------------------------------
    // record_query_hash preserves first_seen_at
    // -------------------------------------------------------------------------

    #[test]
    fn test_record_query_hash_preserves_first_seen_at() -> Result<()> {
        let (manager, _dir) = setup_manager();

        let hash = "deadbeef01234567";
        let query = "SELECT 1";

        // First insert — returns (first_seen, last_seen).
        let (first_seen_1, last_seen_1) = manager.record_query_hash(hash, query)?;
        assert_eq!(
            first_seen_1, last_seen_1,
            "on the very first insert first_seen and last_seen should match"
        );

        // Retrieve first_seen_at directly from SQLite
        let conn = Connection::open(manager.db_path.clone())?;
        let first_seen_at_after_insert: String = conn.query_row(
            "SELECT first_seen_at FROM query_hashes WHERE query_hash = ?1",
            rusqlite::params![hash],
            |row| row.get(0),
        )?;

        // Wait a tiny bit, then insert again with same hash
        std::thread::sleep(std::time::Duration::from_millis(10));
        let (first_seen_2, last_seen_2) = manager.record_query_hash(hash, query)?;

        let first_seen_at_after_update: String = conn.query_row(
            "SELECT first_seen_at FROM query_hashes WHERE query_hash = ?1",
            rusqlite::params![hash],
            |row| row.get(0),
        )?;

        // first_seen_at must not change on subsequent inserts for the same hash
        assert_eq!(
            first_seen_at_after_insert, first_seen_at_after_update,
            "first_seen_at should be preserved on re-insert"
        );

        // The returned first_seen is stable, while last_seen advances.
        assert_eq!(
            first_seen_1, first_seen_2,
            "returned first_seen should be stable across re-inserts"
        );
        assert!(
            last_seen_2 >= last_seen_1,
            "returned last_seen should advance (or stay equal) across re-inserts"
        );
        assert!(
            last_seen_2 > first_seen_2,
            "after a delayed re-insert, last_seen should be later than first_seen"
        );

        Ok(())
    }
}
