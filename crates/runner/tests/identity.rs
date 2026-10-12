//! Reading which runner this store is (task 049, D28 part 6).

use pretty_assertions::assert_eq;
use rimaia_runner::identity::{self, RunnerIdentity};
use rimaia_runner::RunnerStore;

async fn store() -> (tempfile::TempDir, RunnerStore) {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = RunnerStore::open(&dir.path().join("runner.db"))
        .await
        .expect("open the store");
    (dir, store)
}

#[tokio::test]
async fn a_store_nothing_has_adopted_into_has_no_identity() {
    let (_dir, store) = store().await;

    assert_eq!(identity::current(&store).await.expect("read"), None);
}

#[tokio::test]
async fn a_solo_identity_has_no_server_url() {
    let (_dir, store) = store().await;
    sqlx::query(
        "INSERT INTO runner_identity (singleton, runner_id, server_url, created_at)
         VALUES (1, 'runner-1', NULL, '2026-10-10T00:00:00Z')",
    )
    .execute(store.pool())
    .await
    .expect("record the identity");

    assert_eq!(
        identity::current(&store).await.expect("read"),
        Some(RunnerIdentity {
            runner_id: "runner-1".to_string(),
            server_url: None,
        })
    );
}

#[tokio::test]
async fn a_connected_identity_names_its_server() {
    let (_dir, store) = store().await;
    sqlx::query(
        "INSERT INTO runner_identity (singleton, runner_id, server_url, created_at)
         VALUES (1, 'runner-2', 'https://board.example', '2026-10-10T00:00:00Z')",
    )
    .execute(store.pool())
    .await
    .expect("record the identity");

    assert_eq!(
        identity::current(&store).await.expect("read"),
        Some(RunnerIdentity {
            runner_id: "runner-2".to_string(),
            server_url: Some("https://board.example".to_string()),
        })
    );
}
