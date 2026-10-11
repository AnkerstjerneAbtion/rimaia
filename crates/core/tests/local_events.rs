//! Board events and local events (task 048 Scope 2).
//!
//! A write to this machine's state announces a [`LocalChange`] on the
//! machine's own channel and nothing on the board's, because a server has no
//! machine to announce. A write to a team's settings announces that team's
//! `Settings` on the board's channel and nothing local. Both walks cover every
//! key their placement row in seam-contract D28 part 4 names, so a writer
//! moved to the wrong channel fails here rather than in a window that stops
//! refreshing.
//!
//! Over 041's `MemoryMachine`, inside `TwoTeams`: the machine store a runner
//! reads, and a board with two teams to tell apart.

use std::sync::Arc;

use chrono::Duration;
use pretty_assertions::assert_eq;
use rimaia_core::db::settings::{
    self, placement, Dismissal, Placement, RunEnvironment, ALL_KEYS, RUNNER_KEYS,
};
use rimaia_core::db::{OnArchive, ScheduleMode};
use rimaia_core::doctor::Check;
use rimaia_core::machine::local;
use rimaia_core::machine::types::WorktreeRecord;
use rimaia_core::schedule::window::{self, RunWindow};
use rimaia_core::schedule::{self, ScheduleInput};
use rimaia_core::scheduler::{capacity, pause, state, QueueState};
use rimaia_core::strategy::settings::{self as strategy, StrategyApproval};
use rimaia_core::testing::settings as test_settings;
use rimaia_core::testing::teams::TwoTeams;
use rimaia_core::worktree::cleanup::{self, AutoCleanup};
use rimaia_core::{archive, mcp, repo, ChangeEvent, LocalChange};
use serde_json::json;
use tokio::sync::broadcast::Receiver;

#[tokio::test]
async fn every_machine_state_write_announces_a_local_change_and_no_board_event() {
    let mut teams = TwoTeams::new().await;
    let machine = teams.machine.clone();
    let mut local = machine.local.subscribe();
    teams.drain_changes();
    let now = rimaia_core::Clock::now(machine.clock.as_ref());
    let runner = LocalChange::Runner;

    // D28 part 4's *Runner* row, every key through its setter.
    let mut written = Vec::new();
    settings::set_run_environment(&machine, RunEnvironment::StrictLocal)
        .await
        .expect("run environment");
    written.push(settings::RUN_ENVIRONMENT);
    mcp::set_configured_port(&machine, 4610)
        .await
        .expect("mcp port");
    written.push(mcp::MCP_PORT);
    capacity::set_max_concurrency(&machine, 3)
        .await
        .expect("max concurrency");
    written.push(capacity::MAX_CONCURRENCY);
    capacity::set_schedule_mode(&machine, ScheduleMode::Parallel)
        .await
        .expect("schedule mode");
    written.push(capacity::SCHEDULE_MODE);
    state::set_queue_state(&machine, QueueState::Running)
        .await
        .expect("queue state");
    written.push(state::QUEUE_STATE);
    window::open(&machine, &a_window())
        .await
        .expect("open a window");
    written.push(window::ACTIVE_RUN_WINDOW);
    pause::note_usage_limit(&machine, now + Duration::hours(1))
        .await
        .expect("usage-limit pause");
    written.push(pause::USAGE_LIMIT_PAUSE_UNTIL);
    cleanup::set_auto_cleanup(&machine, AutoCleanup::Off)
        .await
        .expect("auto cleanup");
    written.push(cleanup::AUTO_CLEANUP);
    settings::set_doctor_dismissals(
        &machine,
        &[Dismissal {
            check: Check::Git,
            repository: None,
            detail: "read it".to_string(),
        }],
    )
    .await
    .expect("doctor dismissals");
    written.push(settings::DOCTOR_DISMISSALS);
    settings::set_onboarding_dismissed(&machine, true)
        .await
        .expect("onboarding");
    written.push(settings::ONBOARDING_DISMISSED);
    assert_eq!(written, RUNNER_KEYS.to_vec(), "every runner key, once");
    // And the two that clear their row.
    window::close(&machine).await.expect("close the window");
    pause::clear(&machine).await.expect("lift the pause");
    for key in RUNNER_KEYS
        .iter()
        .chain(&["closing the window", "lifting the pause"])
    {
        assert_eq!(next(&mut local, key), runner, "{key}");
    }

    // Schedules: create, update, enable, fire and delete.
    let created = schedule::create(&machine, nightly())
        .await
        .expect("create a schedule");
    schedule::update(
        &machine,
        &created.id,
        ScheduleInput {
            name: "Renamed".to_string(),
            ..nightly()
        },
    )
    .await
    .expect("update it");
    schedule::set_enabled(&machine, &created.id, false)
        .await
        .expect("disable it");
    schedule::record_fire(&machine, &created.id, now)
        .await
        .expect("fire it");
    schedule::delete(&machine, &created.id)
        .await
        .expect("delete it");
    let scheduled = LocalChange::Schedules(Arc::from([created.id.clone()]));
    for write in ["create", "update", "enable", "fire", "delete"] {
        assert_eq!(next(&mut local, write), scheduled, "{write}");
    }

    // Checkouts, through each setter that writes one (task 066).
    let repository = teams.team_a.repository.id.clone();
    let ctx = teams.a.clone();
    repo::set_max_concurrency(&ctx, &machine, &repository, 2)
        .await
        .expect("a checkout's limit");
    archive::set_repository_on_archive(
        &ctx,
        &machine,
        &repository,
        OnArchive::RemoveWorktree,
        None,
    )
    .await
    .expect("a checkout's on-archive");
    repo::set_allow_unattended_runs(&ctx, &machine, &repository, true)
        .await
        .expect("a checkout's consent");
    let elsewhere = tempfile::tempdir().expect("a worktree root");
    repo::set_worktree_root(
        &ctx,
        &machine,
        &repository,
        elsewhere.path().to_string_lossy().into_owned(),
    )
    .await
    .expect("a checkout's worktree root");
    repo::set_credential_metadata(&ctx, &machine, &repository, Some("ada"), Some("CI"))
        .await
        .expect("a checkout's credential");
    repo::clear_credential_metadata(&ctx, &machine, &repository)
        .await
        .expect("a checkout's credential, cleared");
    let checkout = LocalChange::Checkouts(Arc::from([repository.clone()]));
    for write in [
        "limit",
        "on archive",
        "consent",
        "worktree root",
        "credential",
        "credential cleared",
    ] {
        assert_eq!(next(&mut local, write), checkout, "{write}");
    }
    // Worktree records.
    let task = teams.team_a.ready.clone();
    local::record_worktree(
        &machine,
        &WorktreeRecord {
            task_id: task.clone(),
            repository_id: repository.clone(),
            path: "/tmp/a-worktree".to_string(),
            fenced_at: None,
        },
    )
    .await
    .expect("record a worktree");
    local::forget_worktree(&machine, &task)
        .await
        .expect("forget it");
    let worktree = LocalChange::Worktrees(Arc::from([task]));
    for write in ["record", "forget"] {
        assert_eq!(next(&mut local, write), worktree, "{write}");
    }

    // The fixture's own record, forgotten so the checkout can go.
    let in_review = teams.team_a.in_review.clone();
    local::forget_worktree(&machine, &in_review)
        .await
        .expect("forget the fixture's worktree");
    assert_eq!(
        next(&mut local, "forget the fixture's"),
        LocalChange::Worktrees(Arc::from([in_review]))
    );
    local::remove_checkout(&machine, &repository)
        .await
        .expect("remove the checkout");
    assert_eq!(next(&mut local, "remove"), checkout);

    assert!(local.try_recv().is_err(), "each write announced once");
    assert_eq!(
        teams.drain_changes(),
        Vec::<ChangeEvent>::new(),
        "no machine write announced anything on the board's channel"
    );
}

#[tokio::test]
async fn every_team_setting_write_announces_its_teams_settings() {
    let mut teams = TwoTeams::new().await;
    let mut local = teams.machine.local.subscribe();
    teams.drain_changes();

    let team_keys: Vec<&str> = ALL_KEYS
        .iter()
        .copied()
        .filter(|key| placement(key) == Placement::Team)
        .collect();
    // Each team, so "its team" is the one written, never the other.
    let sides = [
        (
            teams.a.clone(),
            teams.team_a.team_id.clone(),
            teams.team_a.repository.id.clone(),
        ),
        (
            teams.b.clone(),
            teams.team_b.team_id.clone(),
            teams.team_b.repository.id.clone(),
        ),
    ];
    for (ctx, team_id, repository_id) in &sides {
        // D28 part 4's *Team* row, every key through the one writer.
        for key in &team_keys {
            test_settings::set_team(ctx, team_id, key, "{}")
                .await
                .unwrap_or_else(|error| panic!("{key}: {error}"));
        }
        // And the accessors a door reaches, each over its own key.
        settings::set_base_instructions(ctx, "Run the linters.")
            .await
            .expect("base instructions");
        rimaia_core::strategy::catalogue::set_catalogue(ctx, r#"{"models":[]}"#)
            .await
            .expect("the strategy catalogue");
        let defaults = serde_json::from_value(json!({ "mode": "default" })).expect("defaults");
        strategy::set_global_default(ctx, &defaults)
            .await
            .expect("the global strategy default");
        strategy::set_repository_default(ctx, repository_id, &defaults)
            .await
            .expect("a repository's strategy default");
        strategy::set_approval(ctx, StrategyApproval::Automatic)
            .await
            .expect("strategy approval");

        let writes = team_keys.len() + 5;
        assert_eq!(
            teams.drain_changes(),
            vec![ChangeEvent::settings(team_id.clone()); writes],
            "every write announces its own team's settings, once"
        );
    }
    assert!(
        local.try_recv().is_err(),
        "no team write announced anything local"
    );
}

fn next(local: &mut Receiver<LocalChange>, write: &str) -> LocalChange {
    local
        .try_recv()
        .unwrap_or_else(|error| panic!("{write} announced nothing local: {error:?}"))
}

fn a_window() -> RunWindow {
    RunWindow {
        schedule_id: "3f2b1c00-0000-4000-8000-0000000000d1".to_string(),
        schedule_name: "Tonight".to_string(),
        opened_at: rimaia_core::testing::test_epoch(),
        closes_at: None,
        mode: ScheduleMode::Parallel,
        max_concurrency: 2,
    }
}

fn nightly() -> ScheduleInput {
    ScheduleInput {
        name: "Nightly".to_string(),
        mode: ScheduleMode::Sequential,
        max_concurrency: 1,
        timezone: "UTC".to_string(),
        cron: Some("0 22 * * *".to_string()),
        start_at: None,
        stop_at: None,
        enabled: true,
    }
}
