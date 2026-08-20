//! The SQLite store: connection pool, migrations, models (ADR-0003).
//!
//! Three writers share this pool — the UI through Tauri commands, the MCP server
//! through other Claude Code sessions, and the run scheduler. That is why the
//! pragmas below are set at connection setup rather than hoped for, and why
//! invariants are enforced in code: the user can open the same file with any
//! SQLite tool.
//!
//! Task 002 adds migrations and models. This module currently owns the pool.

use std::path::Path;
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;

use crate::error::Result;

/// How long a writer waits for the lock before giving up. Long enough to cover a
/// board reorder racing the scheduler claiming the next task.
pub(crate) const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Opens (creating if absent) the database at `path`.
///
/// The parent directory must already exist — [`crate::AppPaths::create_all`]
/// runs first at startup.
pub async fn connect(path: &Path) -> Result<SqlitePool> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(BUSY_TIMEOUT);

    let pool = SqlitePoolOptions::new().connect_with(options).await?;
    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connect_creates_the_file_and_applies_the_pragmas() {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("rimaia.db");

        let pool = connect(&file).await.expect("connect");

        let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&pool)
            .await
            .expect("journal_mode");
        let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&pool)
            .await
            .expect("foreign_keys");

        assert_eq!(journal.to_lowercase(), "wal");
        assert_eq!(foreign_keys, 1);
        assert!(file.exists());

        pool.close().await;
    }
}
