//! `runner.db` opens like the board and migrates like the board (task 040).

mod common;

use pretty_assertions::assert_eq;
use rimaia_runner::RunnerStore;

#[tokio::test]
async fn the_runner_store_opens_with_the_boards_pragmas() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("runner.db");

    let store = RunnerStore::open(&file).await.expect("open the store");

    let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(store.pool())
        .await
        .expect("journal_mode");
    let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(store.pool())
        .await
        .expect("foreign_keys");
    assert_eq!(journal.to_lowercase(), "wal");
    assert_eq!(foreign_keys, 1);
    assert!(file.exists());
    assert_eq!(store.path(), file);
    store.pool().close().await;
}

#[tokio::test]
async fn a_second_launch_applies_no_further_runner_migrations() {
    // A real file reopened between the two, because "second launch" is a
    // second process against the same database.
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("runner.db");

    let first = RunnerStore::open(&file).await.expect("first launch");
    let after_first = common::dump(first.pool(), &["_sqlx_migrations"]).await;
    first.pool().close().await;
    let second = RunnerStore::open(&file).await.expect("second launch");
    let after_second = common::dump(second.pool(), &["_sqlx_migrations"]).await;
    let versions = applied_versions(&second).await;
    second.pool().close().await;

    // Every column of sqlx's bookkeeping, `installed_on` and the checksum
    // included, so a file re-applied or re-recorded would show.
    assert_eq!(after_first, after_second);
    assert_eq!(
        versions,
        vec![20261003130000],
        "the runner set and no board version"
    );
}

async fn applied_versions(store: &RunnerStore) -> Vec<i64> {
    // sqlx declares `version` as `BIGINT PRIMARY KEY` with no NOT NULL, so the
    // macro would infer an `Option` for a column that never holds NULL.
    sqlx::query_scalar!(r#"SELECT version AS "version!" FROM _sqlx_migrations ORDER BY version"#)
        .fetch_all(store.pool())
        .await
        .expect("read the bookkeeping")
}
