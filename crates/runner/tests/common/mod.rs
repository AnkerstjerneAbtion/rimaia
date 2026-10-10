//! A launch's two files, built the way the shell builds them.
//!
//! One helper for every adoption test, so none of them can drift from the
//! shell's setup hook or from each other: `db::connect` on a real file,
//! `db::migrate`, `identity::ensure_solo`, then the context with the solo
//! scope, acting for the solo user, on a `TestClock`.
//!
//! Board tables are read here through unchecked `sqlx::query*` strings, never
//! the checked macros: `crates/runner/.sqlx/` is prepared against a database
//! that holds only the runner migrations, so a checked macro over a board
//! table would fail that prepare (seam-contract D33 point 3).

#![allow(dead_code)] // Each test binary uses its own subset.

use std::path::Path;
use std::sync::Arc;

use rimaia_core::db::{self, MutationSource};
use rimaia_core::identity::{self, SoloIdentity};
use rimaia_core::testing::TestClock;
use rimaia_core::{ServiceContext, TeamScope};
use sqlx::SqlitePool;

/// The board tables adoption must never write.
pub const BOARD_TABLES: [&str; 8] = [
    "settings",
    "team_settings",
    "user_settings",
    "solo_identity",
    "runners",
    "repositories",
    "tasks",
    "schedules",
];

/// The runner set's tables.
pub const RUNNER_TABLES: [&str; 7] = [
    "runner_identity",
    "runner_settings",
    "adoptions",
    "checkouts",
    "worktrees",
    "held_leases",
    "schedules",
];

/// The board at `file`, opened as a launch opens it.
pub async fn open_board(file: &Path, clock: &TestClock) -> (ServiceContext, SoloIdentity) {
    let pool = db::connect(file).await.expect("open the board");
    db::migrate(&pool).await.expect("migrate the board");
    let solo = identity::ensure_solo(&pool, clock)
        .await
        .expect("establish the solo identity");
    let context = ServiceContext::new(
        pool,
        Arc::new(clock.clone()),
        MutationSource::Ui,
        TeamScope::one(solo.team_id.clone()),
        solo.user_id.clone(),
    );
    (context, solo)
}

/// Every row of every one of `tables`, each spelled as the literal SQLite
/// would write for each column, so a value that changed type is a change too.
pub async fn dump(pool: &SqlitePool, tables: &[&str]) -> Vec<(String, Vec<String>)> {
    let mut dumped = Vec::new();
    for table in tables {
        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info(?1) ORDER BY cid")
                .bind(table)
                .fetch_all(pool)
                .await
                .expect("read the columns");
        assert!(!columns.is_empty(), "{table} exists");
        let select = columns
            .iter()
            .map(|column| format!("'{column}=' || quote(\"{column}\")"))
            .collect::<Vec<_>>()
            .join(" || ' ' || ");
        let mut rows: Vec<String> =
            sqlx::query_scalar(&format!("SELECT {select} FROM \"{table}\""))
                .fetch_all(pool)
                .await
                .unwrap_or_else(|error| panic!("read {table}: {error}"));
        rows.sort();
        dumped.push(((*table).to_string(), rows));
    }
    dumped
}

/// Writes `key` into the board's legacy `settings` table, as every runner key
/// was written before task 041 moved its readers, and as an upgraded install
/// still holds it.
pub async fn store_legacy_setting(pool: &SqlitePool, key: &str, value: &str) {
    sqlx::query("INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)")
        .bind(key)
        .bind(value)
        .execute(pool)
        .await
        .expect("store a legacy setting");
}
