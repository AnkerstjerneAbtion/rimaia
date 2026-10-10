//! Machine state is read from and written to `runner.db` (task 041).
//!
//! Every test here asserts on `runner.db` itself, so each runs over a real
//! `RunnerStore` in a `TempDir` beside a board opened the way the shell opens
//! it, with a `TestClock`. Core's own tests assert the same behaviour against
//! its in-memory machine store; these are the ones that prove the production
//! store is the one read.

mod common;

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use pretty_assertions::assert_eq;
use rimaia_core::db::settings::{self, Dismissal, RunEnvironment, RUNNER_KEYS};
use rimaia_core::db::ScheduleMode;
use rimaia_core::doctor::Check;
use rimaia_core::machine::MachineContext;
use rimaia_core::schedule::window::{self, RunWindow};
use rimaia_core::scheduler::{capacity, pause, state, QueueState};
use rimaia_core::testing::{test_epoch, TestClock};
use rimaia_core::worktree::cleanup::{self, AutoCleanup};
use rimaia_core::{mcp, ChangeEvent, Clock, ServiceContext};
use rimaia_runner::RunnerStore;
use sqlx::SqlitePool;
use tokio::sync::broadcast::Receiver;

use common::{dump, open_board, store_legacy_setting};

/// The machine context the shell builds in `setup()`: over the runner store,
/// on the board's own sender, under the solo team.
fn machine_over(store: &RunnerStore, board: &ServiceContext, clock: &TestClock) -> MachineContext {
    MachineContext {
        store: Arc::new(store.clone()),
        clock: Arc::new(clock.clone()),
        changes: board.changes.clone(),
        event_team: board
            .scope
            .sole()
            .expect("a solo board has one team")
            .clone(),
    }
}

fn window_named(name: &str) -> RunWindow {
    RunWindow {
        schedule_id: "3f2b1c00-0000-4000-8000-0000000000d1".to_string(),
        schedule_name: name.to_string(),
        opened_at: test_epoch(),
        closes_at: None,
        mode: ScheduleMode::Parallel,
        max_concurrency: 2,
    }
}

fn dismissal(detail: &str) -> Dismissal {
    Dismissal {
        check: Check::Git,
        repository: None,
        detail: detail.to_string(),
    }
}

/// One key's value in the board's legacy table and a different one in
/// `runner_settings`.
struct Planted {
    key: &'static str,
    board: String,
    runner: String,
}

/// A window's stored spelling, written out rather than serialized here: the
/// accessor parses it, and the runner crate takes no JSON dependency for a
/// test.
fn window_json(name: &str) -> String {
    format!(
        r#"{{"scheduleId":"3f2b1c00-0000-4000-8000-0000000000d1","scheduleName":"{name}","openedAt":"2026-08-20T02:00:00Z","closesAt":null,"mode":"parallel","maxConcurrency":2}}"#
    )
}

fn dismissals_json(detail: &str) -> String {
    format!(r#"[{{"check":"git","repository":null,"detail":"{detail}"}}]"#)
}

fn planted(now: DateTime<Utc>) -> Vec<Planted> {
    vec![
        Planted {
            key: settings::RUN_ENVIRONMENT,
            board: "inherit".to_string(),
            runner: "strict_local".to_string(),
        },
        Planted {
            key: mcp::MCP_PORT,
            board: "4600".to_string(),
            runner: "4601".to_string(),
        },
        Planted {
            key: capacity::MAX_CONCURRENCY,
            board: "3".to_string(),
            runner: "5".to_string(),
        },
        Planted {
            key: capacity::SCHEDULE_MODE,
            board: "sequential".to_string(),
            runner: "parallel".to_string(),
        },
        Planted {
            key: state::QUEUE_STATE,
            board: "paused".to_string(),
            runner: "running".to_string(),
        },
        Planted {
            key: window::ACTIVE_RUN_WINDOW,
            board: window_json("the board's"),
            runner: window_json("the runner's"),
        },
        Planted {
            key: pause::USAGE_LIMIT_PAUSE_UNTIL,
            board: (now + Duration::hours(1)).to_rfc3339(),
            runner: (now + Duration::hours(2)).to_rfc3339(),
        },
        Planted {
            key: cleanup::AUTO_CLEANUP,
            board: "off".to_string(),
            runner: "on_done_acknowledged".to_string(),
        },
        Planted {
            key: settings::DOCTOR_DISMISSALS,
            board: dismissals_json("the board's"),
            runner: dismissals_json("the runner's"),
        },
        Planted {
            key: settings::ONBOARDING_DISMISSED,
            board: "false".to_string(),
            runner: "true".to_string(),
        },
    ]
}

#[tokio::test]
async fn runner_state_is_read_from_the_runner_store() {
    let dir = tempfile::tempdir().expect("temp dir");
    let clock = TestClock::new(test_epoch());
    let (board, solo) = open_board(&dir.path().join("rimaia.db"), &clock).await;
    let store = RunnerStore::open(&dir.path().join("runner.db"))
        .await
        .expect("open the store");
    let machine = machine_over(&store, &board, &clock);
    let now = clock.now();
    let table = planted(now);
    assert_eq!(
        table.iter().map(|row| row.key).collect::<Vec<_>>(),
        RUNNER_KEYS.to_vec(),
        "every runner key, once"
    );
    for row in &table {
        store_legacy_setting(&board.pool, row.key, &row.board).await;
        sqlx::query!(
            "INSERT INTO runner_settings (key, value) VALUES (?1, ?2)",
            row.key,
            row.runner,
        )
        .execute(store.pool())
        .await
        .expect("plant the runner's row");
    }

    // Every typed accessor answers with the runner's value.
    assert_eq!(
        settings::run_environment(&machine).await.expect("read"),
        RunEnvironment::StrictLocal
    );
    assert_eq!(mcp::configured_port(&machine).await.expect("read"), 4601);
    assert_eq!(capacity::max_concurrency(&machine).await.expect("read"), 5);
    assert_eq!(
        capacity::schedule_mode(&machine).await.expect("read"),
        ScheduleMode::Parallel
    );
    assert_eq!(
        state::queue_state(&machine).await.expect("read"),
        QueueState::Running
    );
    assert_eq!(
        window::active(&machine).await.expect("read"),
        Some(window_named("the runner's"))
    );
    assert_eq!(
        pause::active_until(&machine, now).await.expect("read"),
        Some(now + Duration::hours(2))
    );
    assert_eq!(
        cleanup::auto_cleanup(&machine).await.expect("read"),
        AutoCleanup::OnDoneAcknowledged
    );
    assert_eq!(
        settings::doctor_dismissals(&machine).await.expect("read"),
        vec![dismissal("the runner's")]
    );
    assert!(settings::onboarding_dismissed(&machine)
        .await
        .expect("read"));

    // Every setter writes `runner_settings`, leaves the board's row byte for
    // byte, and announces exactly what it announced before task 041: one
    // `Settings`, for the solo team.
    let board_before = dump(&board.pool, &["settings"]).await;
    let mut changes = board.subscribe();
    let settings_event = ChangeEvent::settings(solo.team_id.clone());

    settings::set_run_environment(&machine, RunEnvironment::Inherit)
        .await
        .expect("write");
    expect_written(
        &store,
        &mut changes,
        &settings_event,
        settings::RUN_ENVIRONMENT,
        "inherit",
    )
    .await;

    mcp::set_configured_port(&machine, 4602)
        .await
        .expect("write");
    expect_written(&store, &mut changes, &settings_event, mcp::MCP_PORT, "4602").await;

    capacity::set_max_concurrency(&machine, 6)
        .await
        .expect("write");
    expect_written(
        &store,
        &mut changes,
        &settings_event,
        capacity::MAX_CONCURRENCY,
        "6",
    )
    .await;

    capacity::set_schedule_mode(&machine, ScheduleMode::Sequential)
        .await
        .expect("write");
    expect_written(
        &store,
        &mut changes,
        &settings_event,
        capacity::SCHEDULE_MODE,
        "sequential",
    )
    .await;

    state::set_queue_state(&machine, QueueState::Paused)
        .await
        .expect("write");
    expect_written(
        &store,
        &mut changes,
        &settings_event,
        state::QUEUE_STATE,
        "paused",
    )
    .await;

    let reopened = window_named("set by the setter");
    window::open(&machine, &reopened).await.expect("write");
    expect_changed(
        &store,
        &mut changes,
        &settings_event,
        window::ACTIVE_RUN_WINDOW,
    )
    .await;
    assert_eq!(
        window::active(&machine).await.expect("read"),
        Some(reopened)
    );

    let later = now + Duration::hours(3);
    pause::note_usage_limit(&machine, later)
        .await
        .expect("write");
    expect_written(
        &store,
        &mut changes,
        &settings_event,
        pause::USAGE_LIMIT_PAUSE_UNTIL,
        &later.to_rfc3339(),
    )
    .await;

    cleanup::set_auto_cleanup(&machine, AutoCleanup::Off)
        .await
        .expect("write");
    expect_written(
        &store,
        &mut changes,
        &settings_event,
        cleanup::AUTO_CLEANUP,
        "off",
    )
    .await;

    let dismissals = vec![dismissal("set by the setter")];
    settings::set_doctor_dismissals(&machine, &dismissals)
        .await
        .expect("write");
    expect_changed(
        &store,
        &mut changes,
        &settings_event,
        settings::DOCTOR_DISMISSALS,
    )
    .await;
    assert_eq!(
        settings::doctor_dismissals(&machine).await.expect("read"),
        dismissals
    );

    settings::set_onboarding_dismissed(&machine, false)
        .await
        .expect("write");
    expect_written(
        &store,
        &mut changes,
        &settings_event,
        settings::ONBOARDING_DISMISSED,
        "false",
    )
    .await;

    // Closing the window and lifting the pause remove their rows.
    window::close(&machine).await.expect("close");
    expect_cleared(
        &store,
        &mut changes,
        &settings_event,
        window::ACTIVE_RUN_WINDOW,
    )
    .await;
    pause::clear(&machine).await.expect("lift");
    expect_cleared(
        &store,
        &mut changes,
        &settings_event,
        pause::USAGE_LIMIT_PAUSE_UNTIL,
    )
    .await;

    assert_eq!(
        dump(&board.pool, &["settings"]).await,
        board_before,
        "no setter touched the board's copy"
    );
}

/// `key` now holds `value` in `runner_settings`, and the write announced
/// `event` and nothing else.
async fn expect_written(
    store: &RunnerStore,
    changes: &mut Receiver<ChangeEvent>,
    event: &ChangeEvent,
    key: &str,
    value: &str,
) {
    assert_eq!(
        runner_setting(store.pool(), key).await,
        Some(value.to_string()),
        "{key}"
    );
    expect_one(changes, event, key);
}

/// `key` holds something other than what was planted in `runner_settings`,
/// and the write announced `event` and nothing else. For a JSON value, whose
/// exact spelling is its accessor's to read back.
async fn expect_changed(
    store: &RunnerStore,
    changes: &mut Receiver<ChangeEvent>,
    event: &ChangeEvent,
    key: &str,
) {
    let planted = planted(test_epoch())
        .into_iter()
        .find(|row| row.key == key)
        .expect("a planted key")
        .runner;
    let stored = runner_setting(store.pool(), key).await;
    assert!(stored.is_some(), "{key} is stored");
    assert_ne!(stored, Some(planted), "{key} was rewritten");
    expect_one(changes, event, key);
}

async fn expect_cleared(
    store: &RunnerStore,
    changes: &mut Receiver<ChangeEvent>,
    event: &ChangeEvent,
    key: &str,
) {
    assert_eq!(runner_setting(store.pool(), key).await, None, "{key}");
    expect_one(changes, event, key);
}

fn expect_one(changes: &mut Receiver<ChangeEvent>, event: &ChangeEvent, key: &str) {
    assert_eq!(
        changes.try_recv().ok().as_ref(),
        Some(event),
        "{key}'s setter announces it"
    );
    assert!(
        changes.try_recv().is_err(),
        "{key}'s setter announces it once"
    );
}

async fn runner_setting(pool: &SqlitePool, key: &str) -> Option<String> {
    sqlx::query_scalar!("SELECT value FROM runner_settings WHERE key = ?1", key)
        .fetch_optional(pool)
        .await
        .expect("read runner_settings")
}

/// Unix only, for `passing_queue_environment`'s reason: a fire runs the
/// preflight doctor, which spawns a `#!/bin/sh` stand-in for `claude`.
#[cfg(unix)]
#[tokio::test]
async fn a_schedule_opens_its_window_from_the_runner_store() {
    use rimaia_core::board::{BoardPort, InProcessBoard};
    use rimaia_core::schedule::{self, ScheduleInput};
    use rimaia_core::scheduler::{self, InFlight};
    use rimaia_core::testing::doctor::passing_queue_environment;

    let dir = tempfile::tempdir().expect("temp dir");
    let clock = TestClock::new(test_epoch());
    let (board, solo) = open_board(&dir.path().join("rimaia.db"), &clock).await;
    let store = RunnerStore::open(&dir.path().join("runner.db"))
        .await
        .expect("open the store");
    let machine = machine_over(&store, &board, &clock);
    let (_environment, paths, runner) = passing_queue_environment();
    let board_port: Arc<dyn BoardPort> = Arc::new(InProcessBoard::new(
        board.clone(),
        paths.clone(),
        runner.provider.clone(),
        solo.runner_id.clone(),
    ));

    let fires_at = clock.now() + Duration::minutes(10);
    let created = schedule::create(
        &machine,
        ScheduleInput {
            name: "Tonight".to_string(),
            mode: ScheduleMode::Parallel,
            max_concurrency: 3,
            timezone: "UTC".to_string(),
            cron: None,
            start_at: Some(fires_at),
            stop_at: None,
            enabled: true,
        },
    )
    .await
    .expect("create a schedule");
    let board_before = dump(&board.pool, &["settings", "schedules"]).await;

    let mut changes = board.subscribe();
    let (queue, loop_task) = scheduler::build(
        board_port,
        machine.clone(),
        board.clone(),
        paths,
        runner,
        InFlight::new(),
    );
    let looping = tokio::spawn(loop_task.run());

    clock.set(fires_at + Duration::seconds(5));
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while window::active(&machine).await.expect("read").is_none() {
            changes.recv().await.ok();
        }
    })
    .await
    .expect("the schedule fires and opens its window");

    let fired = clock.now();
    let last_fired_at = sqlx::query!(
        r#"SELECT last_fired_at AS "last_fired_at: DateTime<Utc>" FROM schedules WHERE id = ?1"#,
        created.id,
    )
    .fetch_one(store.pool())
    .await
    .expect("read the schedule's row in runner.db")
    .last_fired_at;
    assert_eq!(
        last_fired_at,
        Some(fired),
        "the fire is recorded in runner.db"
    );

    assert!(
        runner_setting(store.pool(), window::ACTIVE_RUN_WINDOW)
            .await
            .is_some(),
        "the window is written to runner.db"
    );
    assert_eq!(
        window::active(&machine)
            .await
            .expect("read the window back"),
        Some(RunWindow {
            schedule_id: created.id.clone(),
            schedule_name: "Tonight".to_string(),
            opened_at: fired,
            closes_at: None,
            mode: ScheduleMode::Parallel,
            max_concurrency: 3,
        })
    );
    assert_eq!(
        runner_setting(store.pool(), state::QUEUE_STATE).await,
        Some("running".to_string())
    );
    assert_eq!(
        dump(&board.pool, &["settings", "schedules"]).await,
        board_before,
        "the board's copies are left alone"
    );

    queue.shutdown();
    looping.await.expect("the loop ends");
}
