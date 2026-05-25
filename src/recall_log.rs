use rusqlite::Connection;
use std::{path::Path, path::PathBuf, time::Duration};

use crate::error::MemoryError;

/// Append-only SQLite log of recall events for threshold calibration.
///
/// Each method opens a fresh SQLite connection, performs its work, and drops
/// the connection. SQLite WAL mode handles concurrent access at the database
/// level, so no in-process locking is required.
pub struct RecallLog {
    /// Filesystem path to the SQLite database file.
    path: PathBuf,
    /// How long to wait for a locked database before returning a busy error.
    busy_timeout: Duration,
}

/// Statistics for a distance range bucket.
#[derive(Debug, Clone)]
pub struct DistanceBucket {
    /// Lower bound (inclusive) of the distance range.
    pub range_start: f32,
    /// Upper bound (exclusive) of the distance range.
    pub range_end: f32,
    /// Total number of recall events in this bucket.
    pub total: u64,
    /// Number of events marked as applied (memory materially influenced the session).
    pub applied: u64,
    /// Number of events marked as not_applied (memory was not relevant).
    pub not_applied: u64,
    /// Number of events marked as maybe (partially relevant or uncertain).
    pub maybe: u64,
    /// Number of events with no verdict recorded.
    pub unknown: u64,
}

impl RecallLog {
    /// Open a fresh SQLite connection to the recall log database.
    ///
    /// The connection is configured with the instance's busy timeout.
    /// Callers are responsible for dropping the returned connection promptly.
    fn conn(&self) -> Result<Connection, MemoryError> {
        let conn = Connection::open(&self.path)
            .map_err(|e| MemoryError::Internal(format!("recall log open: {e}")))?;
        conn.busy_timeout(self.busy_timeout)
            .map_err(|e| MemoryError::Internal(format!("recall log busy_timeout: {e}")))?;
        Ok(conn)
    }

    /// Open (or create) the recall log database at the given path.
    ///
    /// A temporary connection is used to run the schema migration, then
    /// dropped. Subsequent operations open their own short-lived connections.
    /// `busy_timeout` controls how long each connection will wait when the
    /// database is locked before returning an error.
    pub fn open(path: &Path, busy_timeout: Duration) -> Result<Self, MemoryError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| MemoryError::Internal(format!("recall log dir: {e}")))?;
        }
        let conn = Connection::open(path)
            .map_err(|e| MemoryError::Internal(format!("recall log open: {e}")))?;
        conn.busy_timeout(busy_timeout)
            .map_err(|e| MemoryError::Internal(format!("recall log busy_timeout: {e}")))?;
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
                was_applied TEXT,
                application_note TEXT,
                confidence TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_recall_events_recall_id ON recall_events(recall_id);
            CREATE INDEX IF NOT EXISTS idx_recall_events_memory ON recall_events(memory_name, scope);
        ",
        )
        .map_err(|e| MemoryError::Internal(format!("recall log init: {e}")))?;
        drop(conn);
        Ok(Self {
            path: path.to_path_buf(),
            busy_timeout,
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
        let conn = self.conn()?;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| MemoryError::Internal(format!("recall log tx: {e}")))?;
        {
            let mut stmt = tx
                .prepare(
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

    /// Mark a recall event with the agent's verdict.
    ///
    /// Updates the `was_applied`, `application_note`, and `confidence` columns
    /// for rows matching the given `recall_id`, `memory_name`, and `session_id`
    /// where `was_applied IS NULL` (first call wins).
    /// The `verdict` string must be one of `"applied"`, `"maybe"`, or `"not_applied"`.
    /// Returns the number of rows updated.
    pub fn mark_applied(
        &self,
        recall_id: &str,
        memory_name: &str,
        session_id: &str,
        verdict: &str,
        application_note: Option<&str>,
        confidence: &str,
    ) -> Result<u64, MemoryError> {
        let conn = self.conn()?;
        let rows = conn
            .execute(
                "UPDATE recall_events \
                 SET was_applied = ?1, application_note = ?2, confidence = ?3 \
                 WHERE recall_id = ?4 AND memory_name = ?5 AND session_id = ?6 AND was_applied IS NULL",
                rusqlite::params![
                    verdict,
                    application_note,
                    confidence,
                    recall_id,
                    memory_name,
                    session_id
                ],
            )
            .map_err(|e| MemoryError::Internal(format!("recall log mark_applied: {e}")))?;
        Ok(rows as u64)
    }

    /// Mark all unread recall events for a given session and memory as read.
    ///
    /// Sets `was_read = 1` for every matching row where it was still `0`.
    /// Returns the number of rows updated.
    pub fn mark_read(&self, session_id: &str, memory_name: &str) -> Result<u64, MemoryError> {
        let conn = self.conn()?;
        let rows = conn
            .execute(
                "UPDATE recall_events \
                 SET was_read = 1 \
                 WHERE session_id = ?1 AND memory_name = ?2 AND was_read = 0",
                rusqlite::params![session_id, memory_name],
            )
            .map_err(|e| MemoryError::Internal(format!("recall log mark_read: {e}")))?;
        Ok(rows as u64)
    }

    /// Return recall precision statistics bucketed by distance range.
    ///
    /// Buckets cover [0.00, 0.05), [0.05, 0.10), … up to [0.95, 1.00].
    /// Each bucket reports total, applied, not_applied, and unknown counts.
    /// Distances outside [0, 1) are clamped into the nearest boundary bucket
    /// by the SQL query.
    pub fn recall_stats(&self) -> Result<Vec<DistanceBucket>, MemoryError> {
        let conn = self.conn()?;

        let mut buckets: Vec<DistanceBucket> = (0..20)
            .map(|i| {
                let start = i as f32 * 0.05;
                DistanceBucket {
                    range_start: start,
                    range_end: start + 0.05,
                    total: 0,
                    applied: 0,
                    not_applied: 0,
                    maybe: 0,
                    unknown: 0,
                }
            })
            .collect();

        let mut stmt = conn
            .prepare(
                "SELECT \
                 MIN(CAST(distance * 20 AS INTEGER), 19) AS bucket, \
                 COUNT(*) AS total, \
                 SUM(CASE WHEN was_applied = 'applied' THEN 1 ELSE 0 END) AS applied, \
                 SUM(CASE WHEN was_applied = 'not_applied' THEN 1 ELSE 0 END) AS not_applied, \
                 SUM(CASE WHEN was_applied = 'maybe' THEN 1 ELSE 0 END) AS maybe, \
                 SUM(CASE WHEN was_applied IS NULL THEN 1 ELSE 0 END) AS unknown \
             FROM recall_events \
             WHERE distance >= 0.0 \
             GROUP BY bucket",
            )
            .map_err(|e| MemoryError::Internal(format!("recall_stats prepare: {e}")))?;

        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)? as usize,
                    row.get::<_, u64>(1)?,
                    row.get::<_, u64>(2)?,
                    row.get::<_, u64>(3)?,
                    row.get::<_, u64>(4)?,
                    row.get::<_, u64>(5)?,
                ))
            })
            .map_err(|e| MemoryError::Internal(format!("recall_stats query: {e}")))?;

        for row in rows {
            let (idx, total, applied, not_applied, maybe, unknown) =
                row.map_err(|e| MemoryError::Internal(format!("recall_stats row: {e}")))?;
            if idx < 20 {
                let b = &mut buckets[idx];
                b.total = total;
                b.applied = applied;
                b.not_applied = not_applied;
                b.maybe = maybe;
                b.unknown = unknown;
            }
        }

        Ok(buckets)
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
    pub distance: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn open_creates_table() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("recall_log.sqlite");
        let log = RecallLog::open(&path, Duration::from_secs(5)).unwrap();
        let conn = log.conn().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM recall_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn log_results_inserts_rows() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("recall_log.sqlite");
        let log = RecallLog::open(&path, Duration::from_secs(5)).unwrap();

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

        let conn = log.conn().unwrap();
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

    #[test]
    fn mark_applied_updates_row() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("recall_log.sqlite");
        let log = RecallLog::open(&path, Duration::from_secs(5)).unwrap();

        let recall_id = RecallLog::generate_recall_id();
        let results = vec![
            RecallResult {
                memory_name: "alpha".to_string(),
                scope: "global".to_string(),
                rank: 0,
                distance: 0.2,
            },
            RecallResult {
                memory_name: "beta".to_string(),
                scope: "global".to_string(),
                rank: 1,
                distance: 0.4,
            },
        ];
        log.log_results(&recall_id, "sess-1", &results).unwrap();

        // Mark only "alpha" as applied.
        let rows_affected = log
            .mark_applied(
                &recall_id,
                "alpha",
                "sess-1",
                "applied",
                Some("used for greeting"),
                "high",
            )
            .unwrap();
        assert_eq!(rows_affected, 1);

        // Verify via direct DB access.
        let conn = log.conn().unwrap();
        let (was_applied, note, confidence): (Option<String>, Option<String>, Option<String>) =
            conn.query_row(
                "SELECT was_applied, application_note, confidence \
                 FROM recall_events WHERE recall_id = ?1 AND memory_name = 'alpha'",
                [&recall_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(was_applied.as_deref(), Some("applied"));
        assert_eq!(note.as_deref(), Some("used for greeting"));
        assert_eq!(confidence.as_deref(), Some("high"));

        // "beta" should be untouched.
        let beta_applied: Option<String> = conn
            .query_row(
                "SELECT was_applied FROM recall_events WHERE recall_id = ?1 AND memory_name = 'beta'",
                [&recall_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(beta_applied, None);
    }

    #[test]
    fn mark_read_correlates_session() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("recall_log.sqlite");
        let log = RecallLog::open(&path, Duration::from_secs(5)).unwrap();

        let recall_id = RecallLog::generate_recall_id();
        let results = vec![RecallResult {
            memory_name: "gamma".to_string(),
            scope: "global".to_string(),
            rank: 0,
            distance: 0.1,
        }];
        log.log_results(&recall_id, "sess-read", &results).unwrap();

        // Initially was_read = 0.
        {
            let conn = log.conn().unwrap();
            let was_read: i64 = conn
                .query_row(
                    "SELECT was_read FROM recall_events WHERE recall_id = ?1",
                    [&recall_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(was_read, 0);
        }

        // Mark as read.
        let rows = log.mark_read("sess-read", "gamma").unwrap();
        assert_eq!(rows, 1);

        // Verify was_read = 1.
        let conn = log.conn().unwrap();
        let was_read: i64 = conn
            .query_row(
                "SELECT was_read FROM recall_events WHERE recall_id = ?1",
                [&recall_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(was_read, 1);

        // A second call on the same row returns 0 affected (already 1).
        drop(conn);
        let rows2 = log.mark_read("sess-read", "gamma").unwrap();
        assert_eq!(rows2, 0);
    }

    #[test]
    fn recall_stats_buckets() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("recall_log.sqlite");
        let log = RecallLog::open(&path, Duration::from_secs(5)).unwrap();

        let recall_id = RecallLog::generate_recall_id();
        // distance 0.1 → bucket index 2  (range [0.10, 0.15))
        // distance 0.1 → bucket index 2
        // distance 0.35 → bucket index 7  (range [0.35, 0.40))
        let results = vec![
            RecallResult {
                memory_name: "r1".to_string(),
                scope: "global".to_string(),
                rank: 0,
                distance: 0.1,
            },
            RecallResult {
                memory_name: "r2".to_string(),
                scope: "global".to_string(),
                rank: 1,
                distance: 0.1,
            },
            RecallResult {
                memory_name: "r3".to_string(),
                scope: "global".to_string(),
                rank: 2,
                distance: 0.35,
            },
        ];
        log.log_results(&recall_id, "sess-stats", &results).unwrap();

        // Mark r1 applied, r2 not-applied, r3 unknown (leave as-is).
        log.mark_applied(&recall_id, "r1", "sess-stats", "applied", None, "medium")
            .unwrap();
        log.mark_applied(&recall_id, "r2", "sess-stats", "not_applied", None, "low")
            .unwrap();

        let stats = log.recall_stats().unwrap();

        // Bucket for 0.10–0.15 is index 2.
        let bucket_low = &stats[2];
        assert_eq!(bucket_low.total, 2);
        assert_eq!(bucket_low.applied, 1);
        assert_eq!(bucket_low.not_applied, 1);
        assert_eq!(bucket_low.maybe, 0);
        assert_eq!(bucket_low.unknown, 0);

        // Bucket for 0.35–0.40 is index 7.
        let bucket_mid = &stats[7];
        assert_eq!(bucket_mid.total, 1);
        assert_eq!(bucket_mid.applied, 0);
        assert_eq!(bucket_mid.not_applied, 0);
        assert_eq!(bucket_mid.maybe, 0);
        assert_eq!(bucket_mid.unknown, 1);
    }

    #[test]
    fn mark_applied_maybe_verdict() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("recall_log.sqlite");
        let log = RecallLog::open(&path, Duration::from_secs(5)).unwrap();

        let recall_id = RecallLog::generate_recall_id();
        let results = vec![
            RecallResult {
                memory_name: "m1".to_string(),
                scope: "global".to_string(),
                rank: 0,
                distance: 0.2,
            },
            RecallResult {
                memory_name: "m2".to_string(),
                scope: "global".to_string(),
                rank: 1,
                distance: 0.2,
            },
        ];
        log.log_results(&recall_id, "sess-maybe", &results).unwrap();

        log.mark_applied(
            &recall_id,
            "m1",
            "sess-maybe",
            "maybe",
            Some("uncertain"),
            "low",
        )
        .unwrap();

        let stats = log.recall_stats().unwrap();
        // distance 0.2 → bucket index 4 (range [0.20, 0.25))
        let bucket = &stats[4];
        assert_eq!(bucket.total, 2);
        assert_eq!(bucket.maybe, 1);
        assert_eq!(bucket.applied, 0);
        assert_eq!(bucket.not_applied, 0);
        assert_eq!(bucket.unknown, 1);

        // Verify DB value directly.
        let conn = log.conn().unwrap();
        let was_applied: Option<String> = conn
            .query_row(
                "SELECT was_applied FROM recall_events WHERE recall_id = ?1 AND memory_name = 'm1'",
                [&recall_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(was_applied.as_deref(), Some("maybe"));
    }

    #[test]
    fn mark_applied_idempotent_first_call_wins() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("recall_log.sqlite");
        let log = RecallLog::open(&path, Duration::from_secs(5)).unwrap();

        let recall_id = RecallLog::generate_recall_id();
        let results = vec![RecallResult {
            memory_name: "idem".to_string(),
            scope: "global".to_string(),
            rank: 0,
            distance: 0.2,
        }];
        log.log_results(&recall_id, "sess", &results).unwrap();

        let rows = log
            .mark_applied(&recall_id, "idem", "sess", "applied", Some("first"), "high")
            .unwrap();
        assert_eq!(rows, 1);

        let rows2 = log
            .mark_applied(
                &recall_id,
                "idem",
                "sess",
                "not_applied",
                Some("second"),
                "low",
            )
            .unwrap();
        assert_eq!(rows2, 0, "second call must be a no-op (IS NULL guard)");

        let conn = log.conn().unwrap();
        let verdict: String = conn
            .query_row(
                "SELECT was_applied FROM recall_events WHERE recall_id = ?1 AND memory_name = 'idem'",
                [&recall_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(verdict, "applied", "first verdict must be preserved");
    }

    #[test]
    fn recall_stats_non_boundary_distances() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("recall_log.sqlite");
        let log = RecallLog::open(&path, Duration::from_secs(5)).unwrap();

        let recall_id = RecallLog::generate_recall_id();
        let results = vec![
            RecallResult {
                memory_name: "a".to_string(),
                scope: "global".to_string(),
                rank: 0,
                distance: 0.02,
            },
            RecallResult {
                memory_name: "b".to_string(),
                scope: "global".to_string(),
                rank: 1,
                distance: 0.04,
            },
            RecallResult {
                memory_name: "c".to_string(),
                scope: "global".to_string(),
                rank: 2,
                distance: 0.07,
            },
        ];
        log.log_results(&recall_id, "sess", &results).unwrap();

        let stats = log.recall_stats().unwrap();
        assert_eq!(
            stats[0].total, 2,
            "0.02 and 0.04 both in bucket 0 [0.00, 0.05)"
        );
        assert_eq!(stats[1].total, 1, "0.07 in bucket 1 [0.05, 0.10)");
    }

    #[test]
    fn recall_stats_empty_db() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("recall_log.sqlite");
        let log = RecallLog::open(&path, Duration::from_secs(5)).unwrap();

        let stats = log.recall_stats().unwrap();
        assert_eq!(stats.len(), 20);
        assert!(stats.iter().all(|b| b.total == 0));
    }

    #[test]
    fn mark_applied_no_match_returns_zero() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("recall_log.sqlite");
        let log = RecallLog::open(&path, Duration::from_secs(5)).unwrap();

        let rows = log
            .mark_applied("no-id", "no-mem", "no-sess", "applied", None, "high")
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[test]
    fn mark_read_no_match_returns_zero() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("recall_log.sqlite");
        let log = RecallLog::open(&path, Duration::from_secs(5)).unwrap();

        let rows = log.mark_read("no-sess", "no-mem").unwrap();
        assert_eq!(rows, 0);
    }

    #[test]
    fn verdict_as_str_matches_sql() {
        use crate::types::Verdict;
        assert_eq!(Verdict::Applied.as_str(), "applied");
        assert_eq!(Verdict::Maybe.as_str(), "maybe");
        assert_eq!(Verdict::NotApplied.as_str(), "not_applied");
    }
}
