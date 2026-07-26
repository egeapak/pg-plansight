use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Persisted `(first_seen, last_seen)` for one query hash.
pub type SeenTimestamps = (DateTime<Utc>, DateTime<Utc>);

fn parse_rfc3339(value: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)
        .with_context(|| format!("Invalid timestamp in state database: {value}"))?
        .with_timezone(&Utc))
}

/// Schema version stored in `PRAGMA user_version`.
///
/// 0 = the original schema; 1 = `processed_files` gained `dev`/`ino`.
const SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Clone)]
pub struct FileState {
    pub file_path: PathBuf,
    pub last_position: u64,
    pub last_modified_time: i64,
    pub file_size: u64,
    pub last_processed_at: DateTime<Utc>,
    /// Filesystem identity, used to detect rotation that a size comparison
    /// misses. `None` for rows written before the v1 migration, and on
    /// platforms without stable (dev, ino) — both fall back to size-only
    /// detection.
    pub dev: Option<u64>,
    pub ino: Option<u64>,
}

/// Handle to the exporter's SQLite state.
///
/// Cloneable and `Send + Sync` so a whole batch of state work can be moved into
/// `spawn_blocking` in one go. The connection is opened lazily on first use and
/// then reused: every method used to `Connection::open` the file, so a
/// collection cycle with N distinct query shapes performed N opens and N
/// individually-fsynced transactions synchronously on a tokio worker.
#[derive(Clone)]
pub struct StateManager {
    db_path: PathBuf,
    conn: Arc<Mutex<Option<Connection>>>,
}

impl StateManager {
    pub fn new(db_path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: db_path.into(),
            conn: Arc::new(Mutex::new(None)),
        }
    }

    /// Run `f` against the shared connection, opening it if needed.
    pub(crate) fn with_conn<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        // A poisoned mutex means a previous caller panicked mid-statement. The
        // connection itself is still usable, and refusing every subsequent
        // write would turn one panic into a permanently dead exporter.
        let mut guard = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            *guard = Some(self.open()?);
        }
        f(guard.as_mut().expect("just opened"))
    }

    fn open(&self) -> Result<Connection> {
        let conn = Connection::open(&self.db_path).with_context(|| {
            format!("Failed to open SQLite database: {}", self.db_path.display())
        })?;
        // WAL lets the periodic readers proceed while the collector writes.
        conn.pragma_update(None, "journal_mode", "WAL")
            .context("Failed to enable WAL journal mode")?;
        // NORMAL is durable across process crashes (only a machine crash can
        // lose the last commits), which is the right trade for a checkpoint
        // that is re-derivable by re-reading the log.
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // Without a busy timeout a concurrent writer fails immediately with
        // SQLITE_BUSY, which the collector surfaces as a failed cycle.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(conn)
    }

    pub fn initialize(&self) -> Result<()> {
        // Create parent directory if it doesn't exist
        if let Some(parent) = self.db_path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create state directory: {}", parent.display())
            })?;
        }

        self.with_conn(Self::create_tables)
    }

    fn create_tables(conn: &mut Connection) -> Result<()> {
        conn.execute(
            "CREATE TABLE IF NOT EXISTS processed_files (
                file_path TEXT PRIMARY KEY,
                last_position INTEGER NOT NULL,
                last_modified_time INTEGER NOT NULL,
                file_size INTEGER NOT NULL,
                last_processed_at TEXT NOT NULL,
                dev INTEGER,
                ino INTEGER
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

        Self::migrate(conn)?;
        Ok(())
    }

    /// Bring an existing database up to [`SCHEMA_VERSION`].
    ///
    /// SQLite has no `ALTER TABLE ... ADD COLUMN IF NOT EXISTS`, so
    /// `PRAGMA user_version` is what makes this idempotent. Both added columns
    /// are nullable, so v0 rows keep their checkpoints and simply fall back to
    /// size-only rotation detection until the file is next observed. An older
    /// binary reading a migrated database still works: every SELECT names its
    /// columns explicitly.
    fn migrate(conn: &mut Connection) -> Result<()> {
        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version >= SCHEMA_VERSION {
            return Ok(());
        }

        let tx = conn.transaction()?;
        if version < 1 {
            let has_dev = tx
                .prepare("SELECT 1 FROM pragma_table_info('processed_files') WHERE name = 'dev'")?
                .exists([])?;
            if !has_dev {
                tx.execute("ALTER TABLE processed_files ADD COLUMN dev INTEGER", [])?;
                tx.execute("ALTER TABLE processed_files ADD COLUMN ino INTEGER", [])?;
            }
        }
        tx.commit()?;

        // PRAGMA user_version cannot be set inside a transaction on all builds.
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(())
    }

    pub fn get_file_state(&self, file_path: &Path) -> Result<Option<FileState>> {
        self.with_conn(|conn| {
            let result = conn
                .query_row(
                    "SELECT file_path, last_position, last_modified_time, file_size, \
                     last_processed_at, dev, ino \
                     FROM processed_files WHERE file_path = ?1",
                    params![file_path.to_string_lossy()],
                    Self::row_to_file_state,
                )
                .optional()?;
            Ok(result)
        })
    }

    /// Shared row mapper so the single-row and all-rows queries cannot drift.
    fn row_to_file_state(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileState> {
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
            dev: row.get::<_, Option<i64>>(5)?.map(|v| v as u64),
            ino: row.get::<_, Option<i64>>(6)?.map(|v| v as u64),
        })
    }

    pub fn update_file_state(&self, state: &FileState) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO processed_files \
                 (file_path, last_position, last_modified_time, file_size, last_processed_at, \
                  dev, ino) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    state.file_path.to_string_lossy(),
                    state.last_position as i64,
                    state.last_modified_time,
                    state.file_size as i64,
                    state.last_processed_at.to_rfc3339(),
                    state.dev.map(|v| v as i64),
                    state.ino.map(|v| v as i64),
                ],
            )?;
            Ok(())
        })
    }

    pub fn get_all_file_states(&self) -> Result<HashMap<PathBuf, FileState>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT file_path, last_position, last_modified_time, file_size, \
                 last_processed_at, dev, ino \
                 FROM processed_files",
            )?;

            let rows = stmt.query_map([], |row| {
                let state = Self::row_to_file_state(row)?;
                Ok((state.file_path.clone(), state))
            })?;

            let mut states = HashMap::new();
            for row in rows {
                let (path, state) = row?;
                states.insert(path, state);
            }
            Ok(states)
        })
    }

    /// Record a query hash sighting, returning the persisted
    /// `(first_seen, last_seen)` timestamps. `first_seen` is preserved across
    /// re-inserts (COALESCE), while `last_seen` advances to the current time.
    pub fn record_query_hash(&self, hash: &str, normalized_query: &str) -> Result<SeenTimestamps> {
        let batch = [(hash.to_string(), normalized_query.to_string())];
        let seen = self.record_query_hashes(&batch)?;
        seen.get(hash)
            .copied()
            .context("query hash upsert returned no row")
    }

    /// Record a whole batch of sightings in **one** transaction, returning the
    /// persisted `(first_seen, last_seen)` per hash.
    ///
    /// The collector calls this once per cycle rather than once per query. The
    /// per-query version performed an `open()` plus an individually-fsynced
    /// autocommit transaction for every distinct query shape — thousands of
    /// them, synchronously, on a tokio worker thread.
    pub fn record_query_hashes(
        &self,
        entries: &[(String, String)],
    ) -> Result<HashMap<String, SeenTimestamps>> {
        if entries.is_empty() {
            return Ok(HashMap::new());
        }

        let now = Utc::now().to_rfc3339();
        self.with_conn(|conn| {
            let tx = conn.transaction()?;
            let mut seen = HashMap::with_capacity(entries.len());
            {
                let mut stmt = tx.prepare_cached(
                    "INSERT OR REPLACE INTO query_hashes \
                     (query_hash, normalized_query, first_seen_at, last_seen_at) \
                     VALUES (?1, ?2, \
                             COALESCE((SELECT first_seen_at FROM query_hashes \
                                       WHERE query_hash = ?1), ?3), \
                             ?3) \
                     RETURNING first_seen_at, last_seen_at",
                )?;

                for (hash, normalized_query) in entries {
                    let (first_seen_str, last_seen_str): (String, String) = stmt
                        .query_row(params![hash, normalized_query, now], |row| {
                            Ok((row.get(0)?, row.get(1)?))
                        })?;
                    seen.insert(
                        hash.clone(),
                        (
                            parse_rfc3339(&first_seen_str)?,
                            parse_rfc3339(&last_seen_str)?,
                        ),
                    );
                }
            }
            tx.commit()?;
            Ok(seen)
        })
    }

    /// Delete one file's checkpoint row.
    pub fn delete_file_state(&self, file_path: &Path) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "DELETE FROM processed_files WHERE file_path = ?1",
                params![file_path.to_string_lossy()],
            )?;
            Ok(())
        })
    }

    /// Delete query-hash rows not seen since `older_than`, returning the count.
    pub fn cleanup_old_query_hashes(&self, older_than: DateTime<Utc>) -> Result<usize> {
        self.with_conn(|conn| {
            let deleted = conn.execute(
                "DELETE FROM query_hashes WHERE last_seen_at < ?1",
                params![older_than.to_rfc3339()],
            )?;
            Ok(deleted)
        })
    }

    pub fn cleanup_old_states(&self, older_than: DateTime<Utc>) -> Result<usize> {
        let cutoff = older_than.to_rfc3339();
        self.with_conn(|conn| {
            // One transaction: the two deletes are a single retention pass.
            let tx = conn.transaction()?;
            let deleted = tx.execute(
                "DELETE FROM processed_files WHERE last_processed_at < ?1",
                params![cutoff],
            )?;
            tx.execute(
                "DELETE FROM query_hashes WHERE last_seen_at < ?1",
                params![cutoff],
            )?;
            tx.commit()?;
            Ok(deleted)
        })
    }

    pub fn reset_state(&self) -> Result<()> {
        self.with_conn(|conn| {
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM processed_files", [])?;
            tx.execute("DELETE FROM query_hashes", [])?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn get_last_run_timestamp(&self) -> Result<DateTime<Utc>> {
        let timestamp: Option<i64> = self.with_conn(|conn| {
            Ok(conn.query_row(
                "SELECT MAX(last_modified_time) FROM processed_files",
                [],
                |row| row.get(0),
            )?)
        })?;

        match timestamp {
            Some(ts) => Ok(DateTime::from_timestamp(ts, 0).unwrap_or_else(Utc::now)),
            None => Ok(Utc::now() - chrono::Duration::hours(24)), // Default to 24h ago if no files processed
        }
    }
}

#[cfg(test)]
mod tests {
    // -------------------------------------------------------------------------
    // Phase 2: connection reuse, batching, rotation identity
    // -------------------------------------------------------------------------

    /// Every method used to `Connection::open` the database file, so a cycle
    /// with N distinct queries performed N opens and N individually-fsynced
    /// autocommit transactions on a tokio worker thread. WAL + a live
    /// connection is what makes reuse safe under concurrent readers.
    #[test]
    fn connection_uses_wal_and_a_busy_timeout() {
        let (manager, _dir) = setup_manager();

        let mode = manager
            .with_conn(|conn| {
                Ok(conn.query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))?)
            })
            .unwrap();
        assert_eq!(
            mode.to_lowercase(),
            "wal",
            "journal_mode must be WAL so readers do not block the writer"
        );

        let timeout: i64 = manager
            .with_conn(|conn| Ok(conn.query_row("PRAGMA busy_timeout", [], |r| r.get(0))?))
            .unwrap();
        assert!(
            timeout > 0,
            "busy_timeout must be set, otherwise a concurrent writer yields SQLITE_BUSY immediately"
        );
    }

    /// The per-query upsert must be one batched transaction, not one
    /// transaction per query.
    #[test]
    fn record_query_hashes_batches_and_preserves_first_seen() {
        let (manager, _dir) = setup_manager();

        let batch: Vec<(String, String)> = (0..500)
            .map(|i| (format!("h{i:04}"), format!("SELECT {i}")))
            .collect();

        let first = manager.record_query_hashes(&batch).unwrap();
        assert_eq!(first.len(), 500);

        std::thread::sleep(std::time::Duration::from_millis(5));
        let second = manager.record_query_hashes(&batch).unwrap();

        for (hash, (first_seen, _)) in &first {
            let (again_first, again_last) = second[hash];
            assert_eq!(
                *first_seen, again_first,
                "first_seen_at must survive a re-insert inside a batch"
            );
            assert!(
                again_last >= again_first,
                "last_seen_at must not go backwards"
            );
        }
    }

    /// A batch is a single transaction: if any row fails, none are committed.
    #[test]
    fn record_query_hashes_rolls_back_the_whole_batch_on_failure() {
        let (manager, _dir) = setup_manager();

        // Drop the table mid-flight is impractical; instead force a NOT NULL
        // violation on the second entry by handing it a hash that collides with
        // a NULL normalized_query via a direct write.
        manager
            .with_conn(|conn| {
                conn.execute("DROP TABLE query_hashes", [])?;
                conn.execute(
                    "CREATE TABLE query_hashes (
                        query_hash TEXT PRIMARY KEY,
                        normalized_query TEXT NOT NULL,
                        first_seen_at TEXT NOT NULL,
                        last_seen_at TEXT NOT NULL,
                        CHECK (normalized_query <> 'BOOM')
                    )",
                    [],
                )?;
                Ok(())
            })
            .unwrap();

        let batch = vec![
            ("ok0".to_string(), "SELECT 1".to_string()),
            ("bad".to_string(), "BOOM".to_string()),
        ];
        assert!(manager.record_query_hashes(&batch).is_err());

        let count: i64 = manager
            .with_conn(|conn| {
                Ok(conn.query_row("SELECT count(*) FROM query_hashes", [], |r| r.get(0))?)
            })
            .unwrap();
        assert_eq!(
            count, 0,
            "a failed batch must leave no rows behind; it is one transaction"
        );
    }

    /// Rotation used to be detected only by `current_size < file_size`. Under
    /// logrotate's `create` mode the replacement file can outgrow the old
    /// checkpoint within one poll interval, and the collector then resumes at
    /// the old offset — silently skipping the head of the new file.
    #[test]
    fn file_state_round_trips_device_and_inode() {
        let (manager, dir) = setup_manager();
        let path = dir.path().join("pg.log");
        std::fs::write(&path, b"x").unwrap();

        let state = FileState {
            file_path: path.clone(),
            last_position: 10,
            last_modified_time: 1,
            file_size: 10,
            last_processed_at: Utc::now(),
            dev: Some(42),
            ino: Some(4242),
        };
        manager.update_file_state(&state).unwrap();

        let loaded = manager.get_file_state(&path).unwrap().expect("a row");
        assert_eq!(loaded.dev, Some(42));
        assert_eq!(loaded.ino, Some(4242));
    }

    /// Existing deployments have a v0 database. The migration must be additive
    /// and idempotent, and must not lose checkpoints.
    #[test]
    fn migrates_a_v0_database_without_losing_checkpoints() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("v0.db");

        // Hand-build the pre-migration schema with one checkpoint row.
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute(
                "CREATE TABLE processed_files (
                    file_path TEXT PRIMARY KEY,
                    last_position INTEGER NOT NULL,
                    last_modified_time INTEGER NOT NULL,
                    file_size INTEGER NOT NULL,
                    last_processed_at TEXT NOT NULL
                )",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO processed_files VALUES ('/var/log/pg.log', 12345, 1, 999, ?1)",
                params![Utc::now().to_rfc3339()],
            )
            .unwrap();
            conn.execute("PRAGMA user_version = 0", []).unwrap();
        }

        let manager = StateManager::new(&db_path);
        manager.initialize().unwrap();

        let loaded = manager
            .get_file_state(Path::new("/var/log/pg.log"))
            .unwrap()
            .expect("the v0 checkpoint must survive migration");
        assert_eq!(loaded.last_position, 12345);
        assert_eq!(loaded.dev, None, "pre-migration rows have no identity yet");
        assert_eq!(loaded.ino, None);

        // Idempotent: a second initialize must not fail or duplicate columns.
        manager.initialize().unwrap();
        let again = manager
            .get_file_state(Path::new("/var/log/pg.log"))
            .unwrap()
            .expect("still there");
        assert_eq!(again.last_position, 12345);
    }

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
            dev: None,
            ino: None,
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
            dev: None,
            ino: None,
        };
        let recent_state = FileState {
            file_path: PathBuf::from("/recent/log.log"),
            last_position: 0,
            last_modified_time: 0,
            file_size: 0,
            last_processed_at: recent_time,
            dev: None,
            ino: None,
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
            dev: None,
            ino: None,
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
            dev: None,
            ino: None,
        })?;
        manager.update_file_state(&FileState {
            file_path: PathBuf::from("/b.log"),
            last_position: 0,
            last_modified_time: t2,
            file_size: 0,
            last_processed_at: Utc::now(),
            dev: None,
            ino: None,
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
