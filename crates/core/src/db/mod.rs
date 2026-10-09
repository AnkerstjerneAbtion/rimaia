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
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteConnection, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous,
};
use sqlx::SqlitePool;

use crate::error::{Error, Result};

pub mod models;
pub mod settings;

/// Re-exported so callers write `db::Task` rather than `db::models::Task`: the
/// module is an organizing detail, and the rows are the store's vocabulary.
pub use models::{
    new_id, BoardColumn, ExitClass, MutationSource, OnArchive, Repository, Run, RunKind, RunState,
    RunStatus, Schedule, ScheduleMode, Setting, StrategyMode, StrategySource, Task, TaskDependency,
    TaskLink,
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
/// Private: [`migrate`] is the whole public surface for the board's set, so
/// the app and the in-memory test harness cannot end up applying different
/// sets. [`migrations`] lends it out read-only, for a test that has to see
/// every file sqlx embedded.
static MIGRATOR: Migrator = sqlx::migrate!("../../src-tauri/migrations");

/// Every board migration as sqlx embedded it, in version order.
///
/// Read-only, so `no_migration_opts_out_of_its_transaction` can look at what
/// the binary actually carries rather than at the files on disk.
pub fn migrations() -> impl Iterator<Item = &'static sqlx::migrate::Migration> {
    MIGRATOR.iter()
}

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
///
/// The only way a board migration is applied to a real `rimaia.db`: it goes
/// through [`apply_migrations`], which is what makes a table rebuild safe.
pub async fn migrate(pool: &SqlitePool) -> Result<()> {
    apply_migrations(&MIGRATOR, pool).await
}

/// Applies `migrator` with foreign-key enforcement off around it, and checks
/// that what it left behind holds together (seam-contract D28 part 1).
///
/// Rebuilding a table other tables reference is only safe with enforcement
/// off: with it on, `DROP TABLE tasks` is an implicit `DELETE` that fires every
/// `ON DELETE CASCADE`. `PRAGMA foreign_keys` does nothing inside a
/// transaction, and sqlx 0.8.6 runs every SQLite migration inside one and
/// ignores `-- no-transaction` on this driver, so the pragma has to be set out
/// here, on the connection the migrator then uses. Each file still runs in
/// sqlx's own transaction, and that is what keeps a rebuild atomic.
///
/// Takes the migrator rather than naming [`MIGRATOR`] because the runner store
/// applies its own set the same way (task 040), and names no board table for
/// the same reason.
///
/// A connection that fails anywhere in here is closed rather than returned:
/// one with enforcement off must never go back to the pool, where the next
/// service would write through it unchecked. For the one-connection in-memory
/// test pool that discards the database, which is what a failed migration
/// deserves anyway.
pub async fn apply_migrations(migrator: &Migrator, pool: &SqlitePool) -> Result<()> {
    let mut conn = pool.acquire().await?;
    let applied = apply_on(migrator, &mut conn).await;
    if applied.is_err() {
        conn.close_on_drop();
    }
    applied
}

async fn apply_on(migrator: &Migrator, conn: &mut SqliteConnection) -> Result<()> {
    set_foreign_keys(conn, false).await?;

    let before = applied_migration_count(conn).await?;
    // `MigrateError` is a sibling of `sqlx::Error`, not one of its variants, so
    // the `#[from]` on `Error::Database` cannot make this hop unaided. Folded in
    // rather than given a code of its own (seam-contract D8): the only caller
    // aborts startup, so nothing branches on it.
    migrator.run(&mut *conn).await.map_err(sqlx::Error::from)?;

    // Only after a run that changed something: a board the sqlite3 CLI left
    // inconsistent is that writer's to repair, and refusing it on an ordinary
    // launch would lock the user out of the app they would repair it with.
    if applied_migration_count(conn).await? != before {
        ensure_no_dangling_references(conn).await?;
    }

    set_foreign_keys(conn, true).await
}

/// Sets enforcement and reads it back, because SQLite ignores the pragma
/// silently inside a transaction rather than refusing it.
async fn set_foreign_keys(conn: &mut SqliteConnection, enforce: bool) -> Result<()> {
    let statement = if enforce {
        "PRAGMA foreign_keys = ON"
    } else {
        "PRAGMA foreign_keys = OFF"
    };
    sqlx::query(statement).execute(&mut *conn).await?;

    let reads: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(&mut *conn)
        .await?;
    if reads != i64::from(enforce) {
        return Err(Error::internal(format!(
            "`{statement}` did not take effect: foreign_keys still reads {reads}"
        )));
    }
    Ok(())
}

/// How many migrations sqlx has recorded, or zero before its table exists.
async fn applied_migration_count(conn: &mut SqliteConnection) -> Result<i64> {
    let has_table: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_one(&mut *conn)
    .await?;
    if has_table == 0 {
        return Ok(0);
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(&mut *conn)
        .await?;
    Ok(count)
}

/// Enforcement was off while the files ran, so nothing cascaded and nothing
/// was refused: this is where a file that relied on either is caught.
async fn ensure_no_dangling_references(conn: &mut SqliteConnection) -> Result<()> {
    let dangling: Option<(String, Option<i64>, String)> =
        sqlx::query_as(r#"SELECT "table", rowid, parent FROM pragma_foreign_key_check LIMIT 1"#)
            .fetch_optional(&mut *conn)
            .await?;
    match dangling {
        None => Ok(()),
        Some((table, rowid, parent)) => {
            let rowid = rowid.map_or_else(|| "no rowid".to_string(), |rowid| rowid.to_string());
            Err(Error::internal(format!(
                "a migration left a dangling reference: {table} row {rowid} names a missing \
                 {parent} row (run PRAGMA foreign_key_check)"
            )))
        }
    }
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

    #[tokio::test]
    async fn a_migration_that_leaves_a_dangling_reference_is_refused_by_name() {
        // Enforcement is off while the files run, so nothing refuses the
        // orphan as it is written: step 4 of D28 part 1 is what catches it,
        // and the message is what a failed startup shows (D11).
        let migrations = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            migrations.path().join("20990101000000_orphan.sql"),
            "-- An orphan.\n\
             CREATE TABLE parent (id INTEGER PRIMARY KEY);\n\
             CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES parent (id));\n\
             INSERT INTO child (id, parent_id) VALUES (7, 1);\n",
        )
        .expect("write the migration");
        let migrator = Migrator::new(migrations.path()).await.expect("read it");
        let dir = tempfile::tempdir().expect("temp dir");
        let pool = connect(&dir.path().join("rimaia.db"))
            .await
            .expect("connect");

        let error = apply_migrations(&migrator, &pool)
            .await
            .expect_err("the orphan is refused");

        assert_eq!(
            error.to_string(),
            "a migration left a dangling reference: child row 7 names a missing parent row \
             (run PRAGMA foreign_key_check)"
        );
        let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&pool)
            .await
            .expect("foreign_keys");
        assert_eq!(foreign_keys, 1, "the failed connection never went back");
        pool.close().await;
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
