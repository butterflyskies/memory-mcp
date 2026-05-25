use rusqlite::Connection;
use std::{path::Path, sync::Mutex};

use crate::error::MemoryError;

/// Append-only SQLite log of recall events for threshold calibration.
pub struct RecallLog {
    conn: Mutex<Connection>,
}

impl RecallLog {
    /// Open (or create) the recall log database at the given path.
    pub fn open(path: &Path) -> Result<Self, MemoryError> {
        let conn = Connection::open(path)
            .map_err(|e| MemoryError::Internal(format!("recall log open: {e}")))?;
        conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            CREATE TABLE IF NOT EXISTS recall_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                recall_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                memory_name TEXT NOT NULL,
                scope TEXT NOT NULL,
                rank INTEGER NOT NULL,
                distance REAL NOT NULL,
                returned_at TEXT NOT NULL DEFAULT (datetime('now')),
                was_read INTEGER NOT NULL DEFAULT 0,
                was_applied INTEGER,
                application_note TEXT,
                confidence TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_recall_events_recall_id ON recall_events(recall_id);
            CREATE INDEX IF NOT EXISTS idx_recall_events_memory ON recall_events(memory_name, scope);
        ",
        )
        .map_err(|e| MemoryError::Internal(format!("recall log init: {e}")))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Generate a unique recall_id for a batch of results.
    pub fn generate_recall_id() -> String {
        format!("r_{}", uuid::Uuid::new_v4().as_simple())
    }

    /// Log a batch of recall results.
    pub fn log_results(
        &self,
        recall_id: &str,
        session_id: &str,
        results: &[RecallResult],
    ) -> Result<(), MemoryError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| MemoryError::Internal("recall log mutex poisoned".to_string()))?;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| MemoryError::Internal(format!("recall log tx: {e}")))?;
        {
            let mut stmt = tx
                .prepare_cached(
                    "INSERT INTO recall_events \
                     (recall_id, session_id, memory_name, scope, rank, distance) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )
                .map_err(|e| MemoryError::Internal(format!("recall log prepare: {e}")))?;
            for r in results {
                stmt.execute(rusqlite::params![
                    recall_id,
                    session_id,
                    r.memory_name,
                    r.scope,
                    r.rank,
                    r.distance
                ])
                .map_err(|e| MemoryError::Internal(format!("recall log insert: {e}")))?;
            }
        }
        tx.commit()
            .map_err(|e| MemoryError::Internal(format!("recall log commit: {e}")))?;
        Ok(())
    }
}

/// A single recall result for logging purposes.
pub struct RecallResult {
    /// Name of the memory that was returned.
    pub memory_name: String,
    /// Scope of the memory (e.g. "global" or "project:foo").
    pub scope: String,
    /// Zero-based rank in the result set (lower is more relevant).
    pub rank: usize,
    /// Cosine distance from the query vector (lower is more similar).
    pub distance: f32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn open_creates_table() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("recall_log.sqlite");
        let log = RecallLog::open(&path).unwrap();
        let conn = log.conn.lock().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM recall_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn log_results_inserts_rows() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("recall_log.sqlite");
        let log = RecallLog::open(&path).unwrap();

        let recall_id = RecallLog::generate_recall_id();
        let results = vec![
            RecallResult {
                memory_name: "foo".to_string(),
                scope: "global".to_string(),
                rank: 0,
                distance: 0.1,
            },
            RecallResult {
                memory_name: "bar".to_string(),
                scope: "project:myproj".to_string(),
                rank: 1,
                distance: 0.3,
            },
        ];

        log.log_results(&recall_id, "test-session", &results)
            .unwrap();

        let conn = log.conn.lock().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM recall_events WHERE recall_id = ?1",
                [&recall_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn generate_recall_id_unique() {
        let id1 = RecallLog::generate_recall_id();
        let id2 = RecallLog::generate_recall_id();
        assert_ne!(id1, id2);
        assert!(id1.starts_with("r_"));
        assert!(id2.starts_with("r_"));
    }
}
