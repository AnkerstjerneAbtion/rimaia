//! Adopting this machine's state out of `rimaia.db` (ADR-0028 point 5,
//! seam-contract D28 "The runner set" and part 7, task 040).
//!
//! Every test runs against real files in one `TempDir`, with the board opened
//! the way the shell opens it (`common::open_board`) and a `TestClock`.

mod common;

use chrono::{DateTime, Duration, Utc};
use pretty_assertions::assert_eq;
use rimaia_core::db::settings::{placement, Placement, ALL_KEYS, RUNNER_KEYS, RUN_ENVIRONMENT};
use rimaia_core::db::{OnArchive, Schedule, ScheduleMode};
use rimaia_core::machine::{Checkout, MachineStore, WorktreeRecord};
use rimaia_core::strategy::settings::repository_default_key;
use rimaia_core::testing::db::pre_team_mode_board;
use rimaia_core::testing::{test_epoch, TestClock};
use rimaia_core::{AppPaths, Clock, ErrorCode};
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
    let paths = AppPaths::new(dir.path());
    let clock = TestClock::new(test_epoch());
    let (board, solo) = open_board(&dir.path().join("rimaia.db"), &clock).await;
    for key in RUNNER_KEYS {
        store_legacy_setting(&board.pool, key, &awkward_value(key)).await;
    }
    // Team keys, as an upgraded install still holds them in the legacy table.
    for key in ["base_instructions", "max_turns", "strategy_catalogue"] {
        store_legacy_setting(&board.pool, key, "a team's value").await;
    }
    plant_machine_state(&board.pool, &solo.team_id).await;
    let store = RunnerStore::open(&dir.path().join("runner.db"))
        .await
        .expect("open the store");
    let board_before = dump(&board.pool, &BOARD_TABLES).await;

    adopt_board(&board, &store, &solo, &paths)
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
        vec![
            ("machine_state".to_string(), clock.now()),
            ("settings".to_string(), clock.now()),
        ]
    );

    // Every per-machine column, as its board column held it. The repository
    // with no clone path has no checkout, and its task's worktree is skipped
    // with it; the one with no root gets the root `register` would derive.
    assert_eq!(
        store.list_checkouts().await.expect("read the checkouts"),
        vec![
            Checkout {
                repository_id: FULL_REPOSITORY.to_string(),
                path: "/Users/someone/code/every column".to_string(),
                worktree_root: "/Users/someone/worktrees/every column".to_string(),
                max_concurrency: 3,
                unattended_consent: true,
                on_archive: OnArchive::Script,
                on_archive_script: Some("/usr/local/bin/tidy up.sh".to_string()),
                credential_login: Some("octocat".to_string()),
                credential_label: Some("fine-grained — expires March".to_string()),
                credential_added_at: Some(at("2026-08-10T09:30:00Z")),
                created_at: at("2026-08-01T08:00:00Z"),
            },
            Checkout {
                repository_id: ROOTLESS_REPOSITORY.to_string(),
                path: "/Users/someone/code/no root".to_string(),
                worktree_root: paths
                    .worktrees_dir()
                    .join("no-root-yet")
                    .to_str()
                    .expect("a UTF-8 path")
                    .to_string(),
                max_concurrency: 1,
                unattended_consent: false,
                on_archive: OnArchive::None,
                on_archive_script: None,
                credential_login: None,
                credential_label: None,
                credential_added_at: None,
                created_at: at("2026-08-02T08:00:00Z"),
            },
        ]
    );
    assert_eq!(
        store.list_worktrees().await.expect("read the worktrees"),
        vec![
            WorktreeRecord {
                task_id: "3f2b1c00-0000-4000-8000-0000000000a1".to_string(),
                repository_id: FULL_REPOSITORY.to_string(),
                path: "/Users/someone/worktrees/every column/first".to_string(),
                fenced_at: None,
            },
            WorktreeRecord {
                task_id: "3f2b1c00-0000-4000-8000-0000000000a2".to_string(),
                repository_id: ROOTLESS_REPOSITORY.to_string(),
                path: "/elsewhere/second".to_string(),
                fenced_at: None,
            },
        ]
    );
    assert_eq!(
        store.list_schedules().await.expect("read the schedules"),
        vec![
            Schedule {
                id: "3f2b1c00-0000-4000-8000-0000000000c2".to_string(),
                name: "A one-off".to_string(),
                mode: ScheduleMode::Sequential,
                cron: None,
                start_at: Some(at("2026-08-21T18:30:00Z")),
                max_concurrency: 1,
                enabled: false,
                timezone: None,
                stop_at: None,
                last_fired_at: None,
                armed_at: None,
            },
            Schedule {
                id: "3f2b1c00-0000-4000-8000-0000000000c1".to_string(),
                name: "Nightly".to_string(),
                mode: ScheduleMode::Parallel,
                cron: Some("0 22 * * *".to_string()),
                start_at: None,
                max_concurrency: 3,
                enabled: true,
                timezone: Some("Europe/Copenhagen".to_string()),
                stop_at: Some("06:00".to_string()),
                last_fired_at: Some(at("2026-08-19T20:00:05Z")),
                armed_at: Some(at("2026-08-01T12:00:00Z")),
            },
        ]
    );
    assert_eq!(
        dump(&board.pool, &BOARD_TABLES).await,
        board_before,
        "adoption never writes the board"
    );

    // A second launch, after the board's copies moved on.
    store_legacy_setting(&board.pool, RUN_ENVIRONMENT, "strict_local").await;
    sqlx::query("UPDATE repositories SET path = '/moved', worktree_root = '/moved/worktrees'")
        .execute(&board.pool)
        .await
        .expect("move the board's clones");
    sqlx::query("UPDATE schedules SET name = 'Renamed on the board'")
        .execute(&board.pool)
        .await
        .expect("rename the board's schedules");
    clock.advance(Duration::hours(1));
    let runner_after_first = dump(store.pool(), &RUNNER_TABLES).await;
    let board_after_change = dump(&board.pool, &BOARD_TABLES).await;

    adopt_board(&board, &store, &solo, &paths)
        .await
        .expect("a second launch adopts nothing");

    assert_eq!(
        dump(store.pool(), &RUNNER_TABLES).await,
        runner_after_first,
        "nothing is copied twice, and adopted_at keeps its first instant"
    );
    assert_eq!(dump(&board.pool, &BOARD_TABLES).await, board_after_change);
}

/// The repository with every per-machine column set.
const FULL_REPOSITORY: &str = "3f2b1c00-0000-4000-8000-000000000011";
/// The repository with a clone path and no worktree root.
const ROOTLESS_REPOSITORY: &str = "3f2b1c00-0000-4000-8000-000000000012";
/// The repository this board has no clone path for.
const PATHLESS_REPOSITORY: &str = "3f2b1c00-0000-4000-8000-000000000013";

/// What an upgraded install's board holds of this machine: three repositories,
/// a worktree path on a task in each, and two schedules, one with every
/// nullable column set and one with every one `NULL`.
///
/// Unchecked SQL, for `common`'s reason: the runner's offline cache is
/// prepared against a database with no board tables.
async fn plant_machine_state(pool: &SqlitePool, team_id: &str) {
    sqlx::query(
        "INSERT INTO repositories
             (id, team_id, name, path, default_branch, worktree_root, allow_unattended_runs,
              created_at, max_concurrency, credential_login, credential_label,
              credential_added_at, on_archive, on_archive_script)
         VALUES
             (?2, ?1, 'Every column', '/Users/someone/code/every column', 'main',
              '/Users/someone/worktrees/every column', 1, '2026-08-01T08:00:00+00:00', 3,
              'octocat', 'fine-grained — expires March', '2026-08-10T09:30:00+00:00',
              'script', '/usr/local/bin/tidy up.sh'),
             (?3, ?1, 'No root yet', '/Users/someone/code/no root', 'main',
              NULL, 0, '2026-08-02T08:00:00+00:00', 1, NULL, NULL, NULL, 'none', NULL),
             (?4, ?1, 'Nowhere', NULL, 'main',
              NULL, 1, '2026-08-03T08:00:00+00:00', 2, NULL, NULL, NULL,
              'remove_worktree', NULL)",
    )
    .bind(team_id)
    .bind(FULL_REPOSITORY)
    .bind(ROOTLESS_REPOSITORY)
    .bind(PATHLESS_REPOSITORY)
    .execute(pool)
    .await
    .expect("plant the repositories");

    for (task_id, repository_id, worktree_path) in [
        (
            "3f2b1c00-0000-4000-8000-0000000000a1",
            FULL_REPOSITORY,
            Some("/Users/someone/worktrees/every column/first"),
        ),
        (
            "3f2b1c00-0000-4000-8000-0000000000a2",
            ROOTLESS_REPOSITORY,
            Some("/elsewhere/second"),
        ),
        (
            "3f2b1c00-0000-4000-8000-0000000000a3",
            PATHLESS_REPOSITORY,
            Some("/nowhere/third"),
        ),
        (
            "3f2b1c00-0000-4000-8000-0000000000a4",
            FULL_REPOSITORY,
            None,
        ),
    ] {
        sqlx::query(
            "INSERT INTO tasks
                 (id, team_id, repository_id, title, board_column, position, run_state,
                  worktree_path, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'A task', 'ready', 1.0, 'idle', ?4,
                     '2026-08-04T08:00:00+00:00', '2026-08-04T08:00:00+00:00')",
        )
        .bind(task_id)
        .bind(team_id)
        .bind(repository_id)
        .bind(worktree_path)
        .execute(pool)
        .await
        .expect("plant a task");
    }

    sqlx::query(
        "INSERT INTO schedules
             (id, name, mode, cron, start_at, max_concurrency, enabled, timezone, stop_at,
              last_fired_at, armed_at)
         VALUES
             ('3f2b1c00-0000-4000-8000-0000000000c1', 'Nightly', 'parallel', '0 22 * * *',
              NULL, 3, 1, 'Europe/Copenhagen', '06:00', '2026-08-19T20:00:05+00:00',
              '2026-08-01T12:00:00+00:00'),
             ('3f2b1c00-0000-4000-8000-0000000000c2', 'A one-off', 'sequential', NULL,
              '2026-08-21T18:30:00+00:00', 1, 0, NULL, NULL, NULL, NULL)",
    )
    .execute(pool)
    .await
    .expect("plant the schedules");
}

fn at(rfc3339: &str) -> DateTime<Utc> {
    rfc3339.parse().expect("a literal timestamp must parse")
}

#[tokio::test]
async fn a_fresh_install_adopts_its_identity_and_no_settings() {
    let dir = tempfile::tempdir().expect("temp dir");
    let paths = AppPaths::new(dir.path());
    let clock = TestClock::new(test_epoch());
    let (board, solo) = open_board(&dir.path().join("rimaia.db"), &clock).await;
    let store = RunnerStore::open(&dir.path().join("runner.db"))
        .await
        .expect("open the store");

    adopt_board(&board, &store, &solo, &paths)
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
        vec![
            ("machine_state".to_string(), clock.now()),
            ("settings".to_string(), clock.now()),
        ]
    );
    // Nothing on the board to map, so nothing mapped, and the step is still
    // recorded as done.
    assert_eq!(store.list_checkouts().await.expect("read"), Vec::new());
    assert_eq!(store.list_worktrees().await.expect("read"), Vec::new());
    assert_eq!(store.list_schedules().await.expect("read"), Vec::new());
}

#[tokio::test]
async fn every_settings_key_lands_in_exactly_one_store() {
    // An install from before team mode, holding every key D28 part 4 places,
    // upgraded by the launch that applies 038 and then adopted. 038's tests tie
    // its SQL lists to `placement`; this extends the guarantee to the runner
    // leg, so no key lands in two stores or in none.
    let dir = tempfile::tempdir().expect("temp dir");
    let paths = AppPaths::new(dir.path());
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
    adopt_board(&board, &store, &solo, &paths)
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
    let paths = AppPaths::new(dir.path());
    let clock = TestClock::new(test_epoch());
    let (first_board, first_solo) = open_board(&dir.path().join("rimaia.db"), &clock).await;
    store_legacy_setting(&first_board.pool, RUN_ENVIRONMENT, "strict_local").await;
    let runner_file = dir.path().join("runner.db");
    let store = RunnerStore::open(&runner_file)
        .await
        .expect("open the store");
    adopt_board(&first_board, &store, &first_solo, &paths)
        .await
        .expect("adopted against the first board");

    // The user deleted rimaia.db to start over and kept runner.db.
    let (second_board, second_solo) = open_board(&dir.path().join("rimaia-fresh.db"), &clock).await;
    store_legacy_setting(&second_board.pool, RUN_ENVIRONMENT, "inherit").await;
    let runner_before = dump(store.pool(), &RUNNER_TABLES).await;
    let board_before = dump(&second_board.pool, &BOARD_TABLES).await;
    clock.advance(Duration::hours(1));

    let error = adopt_board(&second_board, &store, &second_solo, &paths)
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
