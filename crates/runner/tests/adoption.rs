//! Adopting this machine's state out of `rimaia.db` (ADR-0028 point 5,
//! seam-contract D28 "The runner set" and part 7, task 040).
//!
//! Every test runs against real files in one `TempDir`, with the board opened
//! the way the shell opens it (`common::open_board`) and a `TestClock`.

mod common;

use chrono::{DateTime, Duration, Utc};
use pretty_assertions::assert_eq;
use rimaia_core::db::settings::{placement, Placement, ALL_KEYS, RUNNER_KEYS, RUN_ENVIRONMENT};
use rimaia_core::strategy::settings::repository_default_key;
use rimaia_core::testing::db::pre_team_mode_board;
use rimaia_core::testing::{test_epoch, TestClock};
use rimaia_core::{Clock, ErrorCode};
use rimaia_runner::adopt::adopt_board;
use rimaia_runner::RunnerStore;
use sqlx::SqlitePool;

use common::{dump, open_board, store_legacy_setting, BOARD_TABLES, RUNNER_TABLES};

/// Values a careless copy would mangle: JSON, a trailing newline, quotes and
/// non-ASCII text, so "byte for byte" means something.
fn awkward_value(key: &str) -> String {
    format!("{{\"key\":\"{key}\",\"note\":\"it's — ünïcode\"}}\n")
}

#[tokio::test]
async fn the_runner_adopts_the_board_once() {
    let dir = tempfile::tempdir().expect("temp dir");
    let clock = TestClock::new(test_epoch());
    let (board, solo) = open_board(&dir.path().join("rimaia.db"), &clock).await;
    for key in RUNNER_KEYS {
        store_legacy_setting(&board.pool, key, &awkward_value(key)).await;
    }
    // Team keys, as an upgraded install still holds them in the legacy table.
    for key in ["base_instructions", "max_turns", "strategy_catalogue"] {
        store_legacy_setting(&board.pool, key, "a team's value").await;
    }
    let store = RunnerStore::open(&dir.path().join("runner.db"))
        .await
        .expect("open the store");
    let board_before = dump(&board.pool, &BOARD_TABLES).await;

    adopt_board(&board, &store, &solo)
        .await
        .expect("the first launch adopts");

    let mut expected_settings: Vec<(String, String)> = RUNNER_KEYS
        .iter()
        .map(|key| ((*key).to_string(), awkward_value(key)))
        .collect();
    expected_settings.sort();
    assert_eq!(runner_settings(store.pool()).await, expected_settings);
    assert_eq!(
        runner_identity(store.pool()).await,
        vec![(1, solo.runner_id.clone(), None, clock.now())]
    );
    assert_eq!(
        adoptions(store.pool()).await,
        vec![("settings".to_string(), clock.now())]
    );
    assert_eq!(
        dump(&board.pool, &BOARD_TABLES).await,
        board_before,
        "adoption never writes the board"
    );

    // A second launch, after the board's copy of a runner key moved on.
    store_legacy_setting(&board.pool, RUN_ENVIRONMENT, "strict_local").await;
    clock.advance(Duration::hours(1));
    let runner_after_first = dump(store.pool(), &RUNNER_TABLES).await;
    let board_after_change = dump(&board.pool, &BOARD_TABLES).await;

    adopt_board(&board, &store, &solo)
        .await
        .expect("a second launch adopts nothing");

    assert_eq!(
        dump(store.pool(), &RUNNER_TABLES).await,
        runner_after_first,
        "nothing is copied twice, and adopted_at keeps its first instant"
    );
    assert_eq!(dump(&board.pool, &BOARD_TABLES).await, board_after_change);
}

#[tokio::test]
async fn a_fresh_install_adopts_its_identity_and_no_settings() {
    let dir = tempfile::tempdir().expect("temp dir");
    let clock = TestClock::new(test_epoch());
    let (board, solo) = open_board(&dir.path().join("rimaia.db"), &clock).await;
    let store = RunnerStore::open(&dir.path().join("runner.db"))
        .await
        .expect("open the store");

    adopt_board(&board, &store, &solo)
        .await
        .expect("a fresh install adopts");

    // Absent on the board, so absent here: never an empty string standing in
    // for "the default".
    assert_eq!(runner_settings(store.pool()).await, Vec::new());
    assert_eq!(
        runner_identity(store.pool()).await,
        vec![(1, solo.runner_id.clone(), None, clock.now())]
    );
    assert_eq!(
        adoptions(store.pool()).await,
        vec![("settings".to_string(), clock.now())]
    );
}

#[tokio::test]
async fn every_settings_key_lands_in_exactly_one_store() {
    // An install from before team mode, holding every key D28 part 4 places,
    // upgraded by the launch that applies 038 and then adopted. 038's tests tie
    // its SQL lists to `placement`; this extends the guarantee to the runner
    // leg, so no key lands in two stores or in none.
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("rimaia.db");
    let per_repository = repository_default_key("3f2b1c00-0000-4000-8000-000000000001");
    let mut original: Vec<(String, String)> = ALL_KEYS
        .iter()
        .copied()
        .chain([per_repository.as_str()])
        .map(|key| (key.to_string(), awkward_value(key)))
        .collect();
    original.sort();
    let old_board = pre_team_mode_board(&file).await;
    for (key, value) in &original {
        store_legacy_setting(&old_board, key, value).await;
    }
    old_board.close().await;

    let clock = TestClock::new(test_epoch());
    let (board, solo) = open_board(&file, &clock).await;
    let store = RunnerStore::open(&dir.path().join("runner.db"))
        .await
        .expect("open the store");
    adopt_board(&board, &store, &solo)
        .await
        .expect("the upgraded install adopts");

    let team: Vec<(String, String)> =
        sqlx::query_as("SELECT key, value FROM team_settings WHERE team_id = ?1")
            .bind(&solo.team_id)
            .fetch_all(&board.pool)
            .await
            .expect("read team_settings");
    let user: Vec<(String, String)> =
        sqlx::query_as("SELECT key, value FROM user_settings WHERE user_id = ?1")
            .bind(&solo.user_id)
            .fetch_all(&board.pool)
            .await
            .expect("read user_settings");
    let runner = runner_settings(store.pool()).await;
    // Placement has no order, so each store is named by its spelling.
    let store_of = |placed: Placement| format!("{placed:?}");
    let mut landed: Vec<(String, String, String)> = team
        .into_iter()
        .map(|(key, value)| (key, value, store_of(Placement::Team)))
        .chain(
            user.into_iter()
                .map(|(key, value)| (key, value, store_of(Placement::User))),
        )
        .chain(
            runner
                .into_iter()
                .map(|(key, value)| (key, value, store_of(Placement::Runner))),
        )
        .collect();
    landed.sort();

    // Exactly once each, with its original value, in the store its placement
    // names.
    let expected: Vec<(String, String, String)> = original
        .iter()
        .map(|(key, value)| (key.clone(), value.clone(), store_of(placement(key))))
        .collect();
    assert_eq!(landed, expected);
    assert!(
        original
            .iter()
            .any(|(key, _)| placement(key) == Placement::Runner),
        "the runner leg is exercised"
    );
}

#[tokio::test]
async fn a_runner_store_from_another_board_is_refused() {
    let dir = tempfile::tempdir().expect("temp dir");
    let clock = TestClock::new(test_epoch());
    let (first_board, first_solo) = open_board(&dir.path().join("rimaia.db"), &clock).await;
    store_legacy_setting(&first_board.pool, RUN_ENVIRONMENT, "strict_local").await;
    let runner_file = dir.path().join("runner.db");
    let store = RunnerStore::open(&runner_file)
        .await
        .expect("open the store");
    adopt_board(&first_board, &store, &first_solo)
        .await
        .expect("adopted against the first board");

    // The user deleted rimaia.db to start over and kept runner.db.
    let (second_board, second_solo) = open_board(&dir.path().join("rimaia-fresh.db"), &clock).await;
    store_legacy_setting(&second_board.pool, RUN_ENVIRONMENT, "inherit").await;
    let runner_before = dump(store.pool(), &RUNNER_TABLES).await;
    let board_before = dump(&second_board.pool, &BOARD_TABLES).await;
    clock.advance(Duration::hours(1));

    let error = adopt_board(&second_board, &store, &second_solo)
        .await
        .expect_err("a store from another board is refused");

    assert_eq!(error.code(), ErrorCode::Invalid);
    let path = runner_file.display();
    assert_eq!(
        error.to_string(),
        format!(
            "{path} belongs to runner {}, but this board's runner is {}. Move {path} aside to \
             start this machine over, or restore the rimaia.db it was adopted against.",
            first_solo.runner_id, second_solo.runner_id
        )
    );
    assert_eq!(dump(store.pool(), &RUNNER_TABLES).await, runner_before);
    assert_eq!(dump(&second_board.pool, &BOARD_TABLES).await, board_before);
}

async fn runner_settings(pool: &SqlitePool) -> Vec<(String, String)> {
    sqlx::query!("SELECT key, value FROM runner_settings ORDER BY key")
        .fetch_all(pool)
        .await
        .expect("read runner_settings")
        .into_iter()
        .map(|row| (row.key, row.value))
        .collect()
}

async fn runner_identity(pool: &SqlitePool) -> Vec<(i64, String, Option<String>, DateTime<Utc>)> {
    sqlx::query!(
        r#"SELECT singleton, runner_id, server_url, created_at AS "created_at: DateTime<Utc>"
             FROM runner_identity"#
    )
    .fetch_all(pool)
    .await
    .expect("read runner_identity")
    .into_iter()
    .map(|row| (row.singleton, row.runner_id, row.server_url, row.created_at))
    .collect()
}

async fn adoptions(pool: &SqlitePool) -> Vec<(String, DateTime<Utc>)> {
    sqlx::query!(
        r#"SELECT step, adopted_at AS "adopted_at: DateTime<Utc>" FROM adoptions ORDER BY step"#
    )
    .fetch_all(pool)
    .await
    .expect("read adoptions")
    .into_iter()
    .map(|row| (row.step, row.adopted_at))
    .collect()
}
