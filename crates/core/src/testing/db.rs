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

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

use crate::db::{migrate, BUSY_TIMEOUT};

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
