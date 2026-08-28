//! The SQLite store: connection pool, migrations, models (ADR-0003).
//!
//! Three writers share this pool — the UI through Tauri commands, the MCP server
//! through other Claude Code sessions, and the run scheduler. That is why the
//! pragmas below are set at connection setup rather than hoped for, and why
//! invariants are enforced in code: the user can open the same file with any
//! SQLite tool.
//!
//! This module owns the pool and the migrator; models live beside it.

use std::path::Path;
use std::time::Duration;

use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;

use crate::error::Result;

pub mod models;
pub mod settings;

/// Re-exported so callers write `db::Task` rather than `db::models::Task`: the
/// module is an organizing detail, and the rows are the store's vocabulary.
pub use models::{
    new_id, BoardColumn, ExitClass, MutationSource, Repository, Run, RunState, RunStatus, Schedule,
    ScheduleMode, Setting, StrategyMode, StrategySource, Task, TaskDependency, TaskLink,
};

/// The one enum a settings *value* carries, re-exported alongside the row enums
/// for the same reason: it is part of the store's vocabulary, and task 008 reads
/// it beside them. The functions stay behind `settings::` — they are an accessor
/// with rules, not a row.
pub use settings::RunEnvironment;

/// How long a writer waits for the lock before giving up. Long enough to cover a
/// board reorder racing the scheduler claiming the next task.
pub(crate) const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Compiled in, so migrations need no filesystem at run time and
/// `SQLX_OFFLINE=true` in CI changes nothing here.
///
/// The path reaches out of the crate because ADR-0003 puts the migration files
/// under `src-tauri/migrations/`: they are what the application ships and what
/// `sqlx-cli --source` is pointed at, so shell tooling and packaging find them
/// without a crate-relative detour. Relative to `CARGO_MANIFEST_DIR`, so it does
/// not depend on the working directory the build runs from — and paired with
/// `build.rs`, which is what makes an added migration force a rebuild.
///
/// Private: [`migrate`] is the whole public surface, so the app and the
/// in-memory test harness cannot end up applying different sets.
static MIGRATOR: Migrator = sqlx::migrate!("../../src-tauri/migrations");

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

/// Brings the database up to the current schema.
///
/// Idempotent: sqlx records what it has applied in `_sqlx_migrations`, so a
/// second launch runs nothing. Startup calls this before the window opens and
/// aborts on failure (seam-contract D11) — there is no useful UI to draw over a
/// half-migrated database.
pub async fn migrate(pool: &SqlitePool) -> Result<()> {
    // `MigrateError` is a sibling of `sqlx::Error`, not one of its variants, so
    // the `#[from]` on `Error::Database` cannot make this hop unaided. Folded in
    // rather than given a code of its own (seam-contract D8): the only caller
    // aborts startup, so nothing branches on it.
    MIGRATOR.run(pool).await.map_err(sqlx::Error::from)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

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

    #[tokio::test]
    async fn a_second_launch_applies_no_further_migrations() {
        // Both halves of task 002's first two acceptance criteria, against a real
        // file reopened between them, because "second launch" is literally a
        // second process against the same database.
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("rimaia.db");

        let first = connect(&file).await.expect("first launch");
        migrate(&first).await.expect("a fresh database migrates");
        let after_first = applied_versions(&first).await;
        first.close().await;

        let second = connect(&file).await.expect("second launch");
        migrate(&second)
            .await
            .expect("a migrated database migrates again");
        let after_second = applied_versions(&second).await;
        second.close().await;

        assert!(!after_first.is_empty(), "no migration was applied at all");
        assert_eq!(after_first, after_second);
    }

    /// Read from sqlx's own bookkeeping rather than from the schema: what makes a
    /// second launch a no-op is that the migrator recognises what it already ran,
    /// and re-running a `CREATE TABLE` would fail long before this could tell.
    async fn applied_versions(pool: &SqlitePool) -> Vec<i64> {
        // The `!` is the same trap the schema header warns about, met from the
        // other side: sqlx declares its own `version` as `BIGINT PRIMARY KEY`
        // without a NOT NULL, and SQLite allows NULL in a non-INTEGER primary
        // key, so the macro infers `Option<i64>` for a column that never holds
        // one.
        sqlx::query_scalar!(
            r#"SELECT version AS "version!" FROM _sqlx_migrations ORDER BY version"#
        )
        .fetch_all(pool)
        .await
        .expect("the migrator's own table must be readable")
    }
}
