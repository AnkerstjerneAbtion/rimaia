//! A migrated, private SQLite database per test.
//!
//! Service tests run against the real schema — the same migrations the shipped
//! app applies (ADR-0003) — but in memory, so a test never touches a file and
//! two tests can never see each other's rows.
//!
//! This builds its own connect options rather than calling [`crate::db::connect`]
//! because the production settings do not all apply in memory: there is no WAL
//! journal and no file to create. The pragmas that *are* behaviour, foreign keys
//! and the busy timeout, are shared with production rather than restated.

use std::path::{Path, PathBuf};

use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{SqliteConnection, SqlitePool};
use tempfile::TempDir;

use crate::clock::Clock;
use crate::db::{migrate, BUSY_TIMEOUT};
use crate::events::RunnerId;

/// A fresh database with every migration applied.
///
/// Capped at one connection deliberately: each new connection to `:memory:`
/// gets its own empty database, so a second one would silently see no schema.
pub async fn test_pool() -> SqlitePool {
    let options = SqliteConnectOptions::new()
        .filename(":memory:")
        .foreign_keys(true)
        .busy_timeout(BUSY_TIMEOUT);

    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        // The database lives inside the connection; reaping it would erase the
        // schema mid-test.
        .idle_timeout(None)
        .max_lifetime(None)
        .connect_with(options)
        .await
        .expect("an in-memory SQLite database must always open");

    // Through `db::migrate`, not a second `sqlx::migrate!`, so a test can never
    // pass against a schema the running app does not have (ADR-0003).
    migrate(&pool)
        .await
        .expect("migrations must apply cleanly to an empty database");

    pool
}

/// A second `runners` row for `user_id`, for a test that needs two machines
/// over one board (seam-contract D31 point 13's contract harness).
///
/// A fixture, not a pairing service: pairing is tasks 047 and 052's, and
/// outside this module nothing but `identity::ensure_solo` creates a runner.
///
/// Its own statement rather than `identity`'s, which is private to that module
/// so that no production function outside it takes a bare connection (task
/// 039's `no_service_takes_a_pool_without_a_scope`). Same columns, same
/// provider.
pub async fn insert_runner(
    conn: &mut SqliteConnection,
    clock: &dyn Clock,
    user_id: &str,
    label: &str,
) -> RunnerId {
    let runner_id = crate::db::new_id();
    sqlx::query(
        "INSERT INTO runners (id, user_id, label, provider, paired_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )
    .bind(&runner_id)
    .bind(user_id)
    .bind(label)
    .bind(crate::runner::provider::ProviderId::ClaudeCode.as_str())
    .bind(clock.now())
    .execute(&mut *conn)
    .await
    .expect("a runner for an existing user must insert");
    runner_id
}

/// The version of task 038's team-mode rebuild: every board migration older
/// than this is the schema an install had before team mode.
pub const TEAM_MODE_REBUILD_VERSION: &str = "20261003120000";

/// A directory holding copies of the board migrations whose file names `keep`
/// accepts, for a test that applies part of the board set through sqlx's
/// public `Migrator::new`.
///
/// The returned directory only has to outlive `Migrator::new`, which reads
/// every file into memory.
pub fn board_migrations(keep: impl Fn(&str) -> bool) -> TempDir {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/migrations");
    let dir = tempfile::Builder::new()
        .prefix("rimaia-migrations-")
        .tempdir()
        .expect("a scratch directory");
    for entry in std::fs::read_dir(&source).expect("read the board migrations") {
        let path: PathBuf = entry.expect("a directory entry").path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("a UTF-8 file name")
            .to_string();
        if name.ends_with(".sql") && keep(&name) {
            std::fs::copy(&path, dir.path().join(&name)).expect("copy a migration");
        }
    }
    dir
}

/// A file board at `file`, at the schema every install had before task 038's
/// rebuild: every board migration older than [`TEAM_MODE_REBUILD_VERSION`],
/// applied through sqlx's public API and never through hand-written DDL that
/// could drift from what an install actually holds.
///
/// One builder for every test that starts from that schema (038's rebuild
/// test, 040's settings-placement test), so no two of them can build two
/// different old boards. Opened through [`crate::db::connect`], so a later
/// `db::migrate` on the same pool is exactly the launch that upgrades it.
pub async fn pre_team_mode_board(file: &Path) -> SqlitePool {
    let older = board_migrations(|name| name < TEAM_MODE_REBUILD_VERSION);
    let migrator = Migrator::new(older.path())
        .await
        .expect("read the pre-rebuild migrations");
    let pool = crate::db::connect(file).await.expect("open the board");
    migrator
        .run(&pool)
        .await
        .expect("the pre-rebuild migrations apply");
    pool
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_pool_comes_back_migrated() {
        let pool = test_pool().await;

        let applied: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
        )
        .fetch_one(&pool)
        .await
        .expect("query the schema");

        assert_eq!(applied, 1, "the migrator must have run");
    }

    #[tokio::test]
    async fn foreign_keys_are_enforced_as_they_are_in_production() {
        let pool = test_pool().await;

        let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&pool)
            .await
            .expect("foreign_keys");

        assert_eq!(foreign_keys, 1);
    }

    #[tokio::test]
    async fn two_pools_do_not_share_a_database() {
        // The shared-cache form of in-memory SQLite would make every test in the
        // process write to one database. This asserts we did not reach for it.
        let first = test_pool().await;
        let second = test_pool().await;

        sqlx::query("CREATE TABLE only_in_the_first (id INTEGER PRIMARY KEY)")
            .execute(&first)
            .await
            .expect("create a table");

        let leaked: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM sqlite_master WHERE name = 'only_in_the_first'",
        )
        .fetch_one(&second)
        .await
        .expect("query the schema");

        assert_eq!(leaked, 0);
    }
}
