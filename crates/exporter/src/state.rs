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

    pub fn record_query_hash(&self, hash: &str, normalized_query: &str) -> Result<()> {
        let conn = self.connect()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            "INSERT OR REPLACE INTO query_hashes 
             (query_hash, normalized_query, first_seen_at, last_seen_at)
             VALUES (?1, ?2, 
                     COALESCE((SELECT first_seen_at FROM query_hashes WHERE query_hash = ?1), ?3),
                     ?3)",
            params![hash, normalized_query, now],
        )?;

        Ok(())
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

        let timestamp: Option<i64> = conn
            .query_row(
                "SELECT MAX(last_modified_time) FROM processed_files",
                [],
                |row| row.get(0),
            )
            .optional()?;

        match timestamp {
            Some(ts) => Ok(DateTime::from_timestamp(ts, 0).unwrap_or_else(|| Utc::now())),
            None => Ok(Utc::now() - chrono::Duration::hours(24)), // Default to 24h ago if no files processed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

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
}
