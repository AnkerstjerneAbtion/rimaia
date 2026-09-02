//! The sequential run queue, end to end: five tasks worked top-down, a failure
//! that does not stop the night, a board reordered mid-queue, Pause and Stop,
//! and what a crash leaves behind (task 009; ADR-0007, ADR-0010, ADR-0011,
//! ADR-0012; seam-contract D9).
//!
//! # Nothing here starts a real `claude`, and nothing here sleeps
//!
//! The CLI is a shell script in a `tempfile::TempDir` that replays a recorded
//! stream from `tests/fixtures/cli/` — `tests/runner_process.rs` established
//! that pattern and its header gives the two reasons in full: a real child
//! costs the operator money, and `cargo test` is routinely run from inside a
//! Claude Code session, so it would inherit exactly the `CLAUDE_*` variables
//! task 008 exists to strip. The stand-in here differs from that one in a
//! single way: it **dispatches on the task**, because a queue is by definition
//! more than one run and the interesting scenarios are the ones where the
//! second task behaves differently from the first. The worktree a run is given
//! is `<worktree-root>/<task-id>` (ADR-0005), so its own working directory is
//! the only thing the script needs to tell them apart.
//!
//! No test waits on a timer. A test that needs a run to be *in flight*
//! subscribes to the tail channel (seam-contract D14) and acts on the first
//! snapshot; a test that needs the queue to have got somewhere waits on
//! `ChangeEvent` publications (ADR-0018) and re-reads the board. The `timeout`
//! wrappers are failure bounds, not waits: on a passing run they cost nothing,
//! and on a broken one they turn a hung CI job into a named assertion.
//!
//! # Every whole-loop run here is `RunTrigger::Queued`
//!
//! Which is what the queue uses anyway (ADR-0012: bypass is the unattended
//! path) and also what every recording in the corpus was captured under, so the
//! runner's `init` verification is live rather than worked around. See
//! `tests/runner_process.rs`'s header.

use std::path::{Path, PathBuf};
use std::time::Duration;

use pretty_assertions::assert_eq;
use rimaia_core::db::{BoardColumn, ExitClass, RunState, RunStatus, Task};
use rimaia_core::repo::{self, NewRepository};
use rimaia_core::runner::events::RunTail;
use rimaia_core::runner::outcome::{start_run, NewRun};
use rimaia_core::runner::{run_task, CancelSignal, RunRequest, RunTrigger, RunnerConfig};
use rimaia_core::scheduler::{self, ClaimOutcome, QueueHandle, QueueState, SkipReason};
use rimaia_core::startup;
use rimaia_core::tasks::{self, NewTask, TaskFilter, TaskSummary};
use rimaia_core::testing::fixtures::{fixture_lines, fixture_path};
use rimaia_core::testing::{TempRepo, TestContext};
use rimaia_core::{AppPaths, ChangeEvent, ServiceContext};
use tempfile::TempDir;
use tokio::sync::broadcast::Receiver;

/// A ceiling on any one test. Generous because a five-task queue creates five
/// real git worktrees and spawns five real processes; short enough that a
/// scheduler that deadlocks fails rather than hangs.
const TEST_TIMEOUT: Duration = Duration::from_secs(90);

/// How much of a recording the gated and hanging stand-ins replay before they
/// stop. Enough to reach an `assistant` event, which is what publishes the tail
/// snapshot a test waits on (`success.jsonl` lines 1-5 are init, rate limit,
/// two assistant messages and a tool result).
const HEAD_LINES: usize = 5;

// ---------------------------------------------------------------------------
// Working the board
// ---------------------------------------------------------------------------

#[tokio::test]
async fn five_ready_tasks_run_in_board_order_without_intervention() {
    // Task 009's first acceptance criterion, and the whole MVP loop: a queue is
    // started once and five cards end where the morning review starts.
    let fixture = Fixture::new().await;
    let mut expected = Vec::new();
    for title in ["Alpha", "Bravo", "Charlie", "Delta", "Echo"] {
        expected.push(fixture.add_task(title).await);
    }

    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue();
    queue.start().await.expect("start the queue");

    wait_until(
        &fixture,
        &mut changes,
        "every task to reach in_review",
        |board| {
            board
                .iter()
                .all(|task| task.task.column == BoardColumn::InReview)
        },
    )
    .await;

    assert_eq!(
        fixture.cli.started(),
        expected,
        "the queue must work the column top-down"
    );
    fixture.cli.assert_never_two_at_once();
    for task_id in &expected {
        let task = fixture.task(task_id).await;
        assert_eq!(task.column, BoardColumn::InReview);
        assert_eq!(task.run_state, RunState::Idle);
    }

    queue.shutdown();
}

#[tokio::test]
async fn a_to_b_to_c_run_in_dependency_order_in_one_queue_pass() {
    // Task 011's first acceptance criterion, and the reason ADR-0008 does not
    // wait for a human: "A → B → C in `ready` run in dependency order in a
    // single unattended queue run, with no human interaction."
    //
    // The cards are laid out in the *opposite* order on the board — C at the
    // top, A at the bottom — so board order and dependency order disagree.
    // Without ADR-0008's predicate the queue would start C first, which is the
    // failure this test exists to catch; with it, C and B are skipped on the
    // first pass and become claimable only as their dependencies file
    // themselves for review.
    let fixture = Fixture::new().await;
    let c = fixture.add_task("Charlie").await;
    let b = fixture.add_task("Bravo").await;
    let a = fixture.add_task("Alpha").await;
    tasks::set_task_dependencies(fixture.ctx(), &b, std::slice::from_ref(&a))
        .await
        .expect("B depends on A");
    tasks::set_task_dependencies(fixture.ctx(), &c, std::slice::from_ref(&b))
        .await
        .expect("C depends on B");

    // Before anything runs: two of the three cards are held, each naming its
    // own blocker, and only A is claimable.
    let plan = scheduler::selection::plan(fixture.ctx())
        .await
        .expect("the queue's own plan");
    assert_eq!(
        plan.iter()
            .map(|entry| (entry.task_id.clone(), entry.skip))
            .collect::<Vec<_>>(),
        vec![
            (c.clone(), Some(SkipReason::DependencyNotSatisfied)),
            (b.clone(), Some(SkipReason::DependencyNotSatisfied)),
            (a.clone(), None),
        ],
    );

    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue();
    queue.start().await.expect("start the queue");

    wait_until(
        &fixture,
        &mut changes,
        "every task to reach in_review",
        |board| {
            board
                .iter()
                .all(|task| task.task.column == BoardColumn::InReview)
        },
    )
    .await;

    assert_eq!(
        fixture.cli.started(),
        vec![a.clone(), b.clone(), c.clone()],
        "dependency order, not board order",
    );
    fixture.cli.assert_never_two_at_once();

    // And each attempt recorded what it was branched from: A off the default
    // branch, B off A's branch, C off B's (ADR-0008's amendment).
    assert_eq!(
        recorded_base_ref(&fixture, &a).await.as_deref(),
        Some("main")
    );
    assert_eq!(
        recorded_base_ref(&fixture, &b).await,
        fixture.task(&a).await.branch,
    );
    assert_eq!(
        recorded_base_ref(&fixture, &c).await,
        fixture.task(&b).await.branch,
    );

    queue.shutdown();
}

#[tokio::test]
async fn a_blocked_task_is_left_where_it_is_rather_than_failed() {
    // ADR-0008: a task with unsatisfied dependencies is "skipped by the queue
    // rather than failing". The queue drains, the blocked card is still `ready`
    // and `idle` — *not* `blocked`, which ADR-0008's amendment of 2026-09-02
    // leaves unwritten — and nothing was spawned for it.
    let fixture = Fixture::new().await;
    let runnable = fixture.add_task("Alpha").await;
    let blocked = fixture.add_task("Bravo").await;
    let never_ready = tasks::create_task(
        fixture.ctx(),
        NewTask {
            repository_id: fixture.repository_id.clone(),
            title: "Not ready at all".to_string(),
            plan: None,
            extra_instructions: None,
            column: Some(BoardColumn::NotReady),
            links: vec![],
        },
    )
    .await
    .expect("a dependency that will never satisfy on its own");
    tasks::set_task_dependencies(
        fixture.ctx(),
        &blocked,
        std::slice::from_ref(&never_ready.id),
    )
    .await
    .expect("Bravo depends on it");

    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue();
    queue.start().await.expect("start the queue");

    wait_until(
        &fixture,
        &mut changes,
        "Alpha to reach in_review",
        |board| {
            board
                .iter()
                .any(|task| task.task.id == runnable && task.task.column == BoardColumn::InReview)
        },
    )
    .await;
    wait_until_not_in_flight(&queue).await;

    let held = fixture.task(&blocked).await;
    assert_eq!(held.column, BoardColumn::Ready);
    assert_eq!(
        held.run_state,
        RunState::Idle,
        "blocking is derived at read time; `RunState::Blocked` stays unwritten",
    );
    assert_eq!(fixture.attempts(&blocked).await, 0);
    assert!(!fixture.cli.started().contains(&blocked));

    // And the card can say why, by name.
    let summary = fixture
        .board()
        .await
        .into_iter()
        .find(|summary| summary.task.id == blocked)
        .expect("the blocked card is on the board");
    assert!(summary.blocked_by_incomplete);
    assert_eq!(summary.blocking_title.as_deref(), Some("Not ready at all"));

    queue.shutdown();
}

#[tokio::test]
async fn a_failing_task_does_not_stop_the_queue() {
    // `max-turns.jsonl` is ADR-0011's `fatal` row: no retry, task -> failed,
    // and ADR-0007 keeps the card in `ready` so it interrupts the morning
    // review. What matters here is the card *after* it.
    let fixture = Fixture::new().await;
    let first = fixture.add_task("Alpha").await;
    let doomed = fixture.add_task("Bravo").await;
    let last = fixture.add_task("Charlie").await;
    fixture.cli.replays(&doomed, "max-turns", 1);

    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue();
    queue.start().await.expect("start the queue");

    let done = [first.clone(), doomed.clone(), last.clone()];
    wait_until(
        &fixture,
        &mut changes,
        "all three tasks to settle",
        move |board| {
            board
                .iter()
                .filter(|task| done.contains(&task.task.id))
                .all(|task| task.task.run_state != RunState::Running)
                && board
                    .iter()
                    .filter(|task| task.task.column == BoardColumn::InReview)
                    .count()
                    == 2
        },
    )
    .await;

    assert_eq!(
        fixture.cli.started(),
        vec![first.clone(), doomed.clone(), last.clone()]
    );
    assert_eq!(fixture.task(&doomed).await.run_state, RunState::Failed);
    assert_eq!(
        fixture.task(&doomed).await.column,
        BoardColumn::Ready,
        "a failure stays in `ready` so the morning review trips over it"
    );
    assert_eq!(fixture.task(&last).await.column, BoardColumn::InReview);

    queue.shutdown();
}

#[tokio::test]
async fn reordering_the_board_mid_queue_changes_what_runs_next() {
    // The reason selection is a fresh query on every pass rather than a
    // snapshot taken when the queue was started. Charlie is dragged above Bravo
    // while Alpha is still running, and runs second.
    let fixture = Fixture::new().await;
    let alpha = fixture.add_task("Alpha").await;
    let bravo = fixture.add_task("Bravo").await;
    let charlie = fixture.add_task("Charlie").await;
    let gate = fixture.cli.gates(&alpha, "success", HEAD_LINES);

    let tail = fixture.ctx().subscribe_tail();
    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue();
    queue.start().await.expect("start the queue");

    once_the_run_is_live(tail).await;
    tasks::move_task(
        fixture.ctx(),
        &charlie,
        BoardColumn::Ready,
        Some(&alpha),
        Some(&bravo),
    )
    .await
    .expect("drag Charlie above Bravo");
    open(&gate);

    wait_until(
        &fixture,
        &mut changes,
        "every task to reach in_review",
        |board| {
            board
                .iter()
                .all(|task| task.task.column == BoardColumn::InReview)
        },
    )
    .await;

    assert_eq!(fixture.cli.started(), vec![alpha, charlie, bravo]);

    queue.shutdown();
}

#[tokio::test]
async fn a_repository_without_the_opt_in_is_skipped_with_the_reason_rather_than_silently() {
    // ADR-0012 point 1 is the whole security posture, and a posture the user
    // cannot see is one they cannot fix at 09:00 when nothing ran overnight. So
    // the un-opted task stays *in* the plan, carrying its reason, while the
    // queue works around it.
    let mut fixture = Fixture::new().await;
    let locked_repository = fixture.register_repository(false).await;
    let locked = fixture.add_task_in(&locked_repository, "Locked").await;
    let runnable = fixture.add_task("Runnable").await;

    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue();
    queue.start().await.expect("start the queue");

    wait_until(
        &fixture,
        &mut changes,
        "the runnable task to finish",
        |board| {
            board
                .iter()
                .any(|task| task.task.column == BoardColumn::InReview)
        },
    )
    .await;

    assert_eq!(fixture.cli.started(), vec![runnable]);
    assert_eq!(fixture.task(&locked).await.run_state, RunState::Idle);

    let plan = scheduler::plan(fixture.ctx()).await.expect("read the plan");
    let entry = plan
        .iter()
        .find(|entry| entry.task_id == locked)
        .expect("a skipped task is still in the plan");
    assert_eq!(entry.skip, Some(SkipReason::UnattendedRunsNotAllowed));
    assert_eq!(
        entry.queue_position, None,
        "a skipped task has no place in the queue"
    );
    assert_eq!(
        entry.skip.expect("a reason").explanation(),
        "this repository has not enabled unattended agent runs"
    );

    queue.shutdown();
}

#[tokio::test]
async fn the_plan_numbers_what_the_queue_will_actually_start() {
    // Task 009's "board cards show `queued` position". Counted over what the
    // queue will start, not over the column: a card it will pass over is not
    // third in a queue it is not in.
    let mut fixture = Fixture::new().await;
    let locked_repository = fixture.register_repository(false).await;
    let first = fixture.add_task("Alpha").await;
    let locked = fixture.add_task_in(&locked_repository, "Locked").await;
    let second = fixture.add_task("Bravo").await;

    let plan = scheduler::plan(fixture.ctx()).await.expect("read the plan");
    let positions: Vec<(&str, Option<i64>)> = plan
        .iter()
        .map(|entry| (entry.task_id.as_str(), entry.queue_position))
        .collect();

    assert_eq!(positions.len(), 3);
    assert_eq!(position_of(&positions, &first), Some(1));
    assert_eq!(position_of(&positions, &second), Some(2));
    assert_eq!(position_of(&positions, &locked), None);
    assert_eq!(
        scheduler::next_to_start(&plan).map(|entry| entry.task_id.clone()),
        Some(first)
    );
}

// ---------------------------------------------------------------------------
// Control
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pause_lets_the_current_run_finish_and_starts_nothing_new() {
    let fixture = Fixture::new().await;
    let running = fixture.add_task("Alpha").await;
    let next = fixture.add_task("Bravo").await;
    let gate = fixture.cli.gates(&running, "success", HEAD_LINES);

    let tail = fixture.ctx().subscribe_tail();
    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue();
    queue.start().await.expect("start the queue");

    once_the_run_is_live(tail).await;
    queue.pause().await.expect("pause the queue");
    open(&gate);

    let finished = running.clone();
    wait_until(
        &fixture,
        &mut changes,
        "the current run to finish",
        move |board| {
            board
                .iter()
                .any(|task| task.task.id == finished && task.task.column == BoardColumn::InReview)
        },
    )
    .await;

    assert_eq!(
        fixture.task(&running).await.column,
        BoardColumn::InReview,
        "pause lets the run in flight finish"
    );
    assert_eq!(fixture.task(&next).await.run_state, RunState::Idle);
    assert_eq!(
        fixture.cli.started(),
        vec![running],
        "a paused queue starts nothing new"
    );
    assert_eq!(
        queue.status().await.expect("read the status").state,
        QueueState::Paused
    );

    queue.shutdown();
}

#[tokio::test]
async fn stop_cancels_the_run_in_flight_and_starts_nothing_new() {
    // The other half of ADR-0010's Control section: Stop is Pause plus
    // cancel-one, and cancel-one on a running task "goes to `failed` with
    // `cancelled` reason" — the reason lives on the run's `exit_class`, never
    // on `run_state` (seam-contract D9's two dimensions, again).
    let fixture = Fixture::new().await;
    let running = fixture.add_task("Alpha").await;
    let next = fixture.add_task("Bravo").await;
    fixture
        .cli
        .hangs(&running, "interrupted-sigterm", HEAD_LINES);

    let tail = fixture.ctx().subscribe_tail();
    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue();
    queue.start().await.expect("start the queue");

    once_the_run_is_live(tail).await;
    queue.stop().await.expect("stop the queue");

    let cancelled = running.clone();
    wait_until(
        &fixture,
        &mut changes,
        "the cancelled run to be recorded",
        move |board| {
            board
                .iter()
                .any(|task| task.task.id == cancelled && task.task.run_state == RunState::Failed)
        },
    )
    .await;

    let detail = fixture.detail(&running).await;
    let run = detail
        .last_run
        .expect("a cancelled run still records itself");
    assert_eq!(run.status, RunStatus::Cancelled);
    assert_eq!(run.exit_class, Some(ExitClass::Cancelled));
    assert_eq!(detail.task.run_state, RunState::Failed);
    assert_eq!(detail.task.column, BoardColumn::Ready);
    assert_eq!(fixture.task(&next).await.run_state, RunState::Idle);
    assert_eq!(fixture.cli.started(), vec![running]);

    queue.shutdown();
}

// ---------------------------------------------------------------------------
// Pause, Stop and shutdown pressed mid-claim (task 009's own verification
// report, finding 4) — `try_step` used to leave a window between reading the
// switch and actually claiming a task where none of the three had anything to
// act on. `hold_version_probe` widens that window enough for a test to press
// each of them inside it.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_stop_that_lands_while_the_queue_is_mid_claim_is_not_lost() {
    let fixture = Fixture::new().await;
    let task_id = fixture.add_task("Alpha").await;
    fixture.cli.hold_version_probe();

    let queue = fixture.spawn_queue();
    queue.start().await.expect("start the queue");
    wait_until_in_flight(&queue, &task_id).await;

    // The loop is now blocked inside the held `--version` probe, with
    // `Shared`'s in-flight entry already registered for `task_id` — exactly
    // the window this test exists to close.
    queue.stop().await.expect("stop the queue");
    fixture.cli.release_version_probe();
    wait_until_not_in_flight(&queue).await;

    assert_eq!(
        fixture.cli.started(),
        Vec::<String>::new(),
        "a task the queue was told to stop before it ever claimed must not start",
    );
    assert_eq!(fixture.task(&task_id).await.run_state, RunState::Idle);
    assert_eq!(
        queue.status().await.expect("read the status").state,
        QueueState::Paused
    );

    queue.shutdown();
}

#[tokio::test]
async fn a_pause_that_lands_while_the_queue_is_mid_claim_starts_nothing() {
    // The same window, the other control verb: `pause` never touches a
    // `CancelSignal` at all (only `stop` does), so this is what proves
    // `try_step`'s re-check of the switch itself — not merely the signal —
    // is what closes it.
    let fixture = Fixture::new().await;
    let task_id = fixture.add_task("Alpha").await;
    fixture.cli.hold_version_probe();

    let queue = fixture.spawn_queue();
    queue.start().await.expect("start the queue");
    wait_until_in_flight(&queue, &task_id).await;

    queue.pause().await.expect("pause the queue");
    fixture.cli.release_version_probe();
    wait_until_not_in_flight(&queue).await;

    assert_eq!(fixture.cli.started(), Vec::<String>::new());
    assert_eq!(fixture.task(&task_id).await.run_state, RunState::Idle);

    queue.shutdown();
}

#[tokio::test]
async fn a_shutdown_that_lands_while_the_queue_is_mid_claim_starts_nothing() {
    // The exit path's own window: `QueueHandle::shutdown` used to be checked
    // only at the top of the loop, never inside `try_step`, so a shutdown
    // requested mid-claim did not stop a fresh process from starting.
    let fixture = Fixture::new().await;
    let task_id = fixture.add_task("Alpha").await;
    fixture.cli.hold_version_probe();

    let queue = fixture.spawn_queue();
    queue.start().await.expect("start the queue");
    wait_until_in_flight(&queue, &task_id).await;

    queue.shutdown();
    fixture.cli.release_version_probe();
    wait_until_not_in_flight(&queue).await;

    assert_eq!(fixture.cli.started(), Vec::<String>::new());
    assert_eq!(fixture.task(&task_id).await.run_state, RunState::Idle);
}

#[tokio::test]
async fn a_queue_state_written_before_a_crash_is_what_the_next_launch_reads() {
    // ADR-0010: "queue state survives an app restart by being derived from the
    // database". Two handles over one pool stand in for two launches — the
    // second was never told anything, and knows anyway.
    let fixture = Fixture::new().await;
    let before_the_crash = fixture.spawn_queue();
    before_the_crash.start().await.expect("start the queue");
    before_the_crash.shutdown();

    let after_the_crash = fixture.spawn_queue();

    assert_eq!(
        after_the_crash
            .status()
            .await
            .expect("read the status")
            .state,
        QueueState::Running
    );
    assert_eq!(
        after_the_crash.in_flight_task_id(),
        None,
        "a process does not survive the app that started it"
    );

    after_the_crash.shutdown();
}

// ---------------------------------------------------------------------------
// Claiming
// ---------------------------------------------------------------------------

#[tokio::test]
async fn two_concurrent_claims_of_one_task_leave_exactly_one_winner() {
    // The conditional write, at its narrowest. ADR-0010 requires selection and
    // the transition to `running` to happen in one transaction so that the UI,
    // the MCP server and the scheduler cannot double-claim; this is that
    // property, driven against one pool from two callers at once.
    let fixture = Fixture::new().await;
    let task_id = fixture.add_task("Alpha").await;

    let (first, second) = tokio::join!(
        scheduler::claim(fixture.ctx(), &task_id),
        scheduler::claim(fixture.ctx(), &task_id),
    );
    let outcomes = [
        first.expect("a lost race is not an error"),
        second.expect("a lost race is not an error"),
    ];

    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| **outcome == ClaimOutcome::Claimed)
            .count(),
        1,
        "exactly one claimer may own the task: {outcomes:?}"
    );
    assert!(outcomes.contains(&ClaimOutcome::Lost));
    assert_eq!(fixture.task(&task_id).await.run_state, RunState::Running);
}

#[tokio::test]
async fn a_claim_is_refused_for_every_state_that_means_somebody_already_has_the_task() {
    // The property the mutual exclusion rests on, asserted state by state
    // rather than inferred from one race. `queued`, `running` and
    // `waiting_retry` have no legal edge into `queued`, so the second claimer
    // is refused whichever of them the first one left behind.
    let fixture = Fixture::new().await;

    for taken in [RunState::Queued, RunState::Running, RunState::WaitingRetry] {
        let task_id = fixture.add_task("Taken").await;
        walk_to(&fixture, &task_id, taken).await;

        assert_eq!(
            scheduler::claim(fixture.ctx(), &task_id)
                .await
                .expect("a lost race is not an error"),
            ClaimOutcome::Lost,
            "a task in {taken:?} was claimed out from under whoever has it",
        );
        assert_eq!(fixture.task(&task_id).await.run_state, taken);
    }

    // The other half, and the reason the route is fixed rather than restricted
    // to `idle`: last night's failure is startable again, which is what makes
    // "Run now" on a failed card mean anything (ADR-0007's note on that edge —
    // trying again "re-enters at Queued like every other start").
    for startable in [RunState::Failed, RunState::Cancelled] {
        let task_id = fixture.add_task("Startable").await;
        walk_to(&fixture, &task_id, startable).await;

        assert_eq!(
            scheduler::claim(fixture.ctx(), &task_id)
                .await
                .expect("claim"),
            ClaimOutcome::Claimed,
            "{startable:?}",
        );
    }
}

/// Walks a fresh task through the ADR-0007 machine to `target`, using the one
/// writer of `run_state` rather than a hand-written `UPDATE`.
async fn walk_to(fixture: &Fixture, task_id: &str, target: RunState) {
    let route: &[RunState] = match target {
        RunState::Queued => &[RunState::Queued],
        RunState::Running => &[RunState::Queued, RunState::Running],
        RunState::WaitingRetry => &[RunState::Queued, RunState::Running, RunState::WaitingRetry],
        RunState::Failed => &[RunState::Queued, RunState::Running, RunState::Failed],
        RunState::Cancelled => &[RunState::Queued, RunState::Cancelled],
        other => panic!("no route to {other:?}"),
    };

    for state in route {
        tasks::set_run_state(fixture.ctx(), task_id, *state)
            .await
            .unwrap_or_else(|error| panic!("walking to {target:?} via {state:?}: {error}"));
    }
}

#[tokio::test]
async fn a_starter_that_claims_before_it_spawns_never_produces_a_second_process() {
    // Task 009's acceptance criterion: "concurrent start attempts (UI button
    // plus scheduler) never produce two processes for one task."
    //
    // The contract that makes it true is that **a starter claims before it
    // spawns** — `runner::process::run_task` trusts a task already in
    // `running` and no-ops its own claim, which is the arm task 008 wrote for
    // this task ("when the scheduler exists it claims the task itself and hands
    // this a task already claimed"). So the second starter here does what the
    // queue does: claim, and only spawn if it won — which is now what the
    // real shell's `commands::runs::start_task_run` does too, in exactly
    // these two calls (`scheduler::claim`, then `run_task`), fixed after this
    // suite's own adversarial review found the button did not yet do this and
    // could spawn a second process for one task. `src-tauri` has no test
    // harness of its own to drive that command directly (its dev-dependency
    // on `rimaia-core`'s `testing` feature does not exist), so this is the
    // closest thing to a regression test the button's own claim has — it
    // exercises the identical core call the command now makes.
    let fixture = Fixture::new().await;
    let task_id = fixture.add_task("Alpha").await;

    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue();

    let button = {
        let ctx = fixture.ctx().clone();
        let paths = fixture.paths.clone();
        let runner = fixture.runner();
        let task_id = task_id.clone();
        async move {
            if scheduler::claim(&ctx, &task_id).await.expect("claim") == ClaimOutcome::Lost {
                return None;
            }
            Some(
                run_task(
                    &ctx,
                    &paths,
                    &runner,
                    RunRequest {
                        task_id,
                        trigger: RunTrigger::Queued,
                        cancel: CancelSignal::new(),
                    },
                )
                .await,
            )
        }
    };

    let (started, pressed) = tokio::join!(queue.start(), button);
    started.expect("start the queue");
    if let Some(result) = pressed {
        result.expect("the starter that won the claim runs to completion");
    }

    wait_until(
        &fixture,
        &mut changes,
        "the task to leave `ready`",
        |board| {
            board
                .iter()
                .all(|task| task.task.run_state != RunState::Running)
        },
    )
    .await;

    assert_eq!(
        fixture.cli.started(),
        vec![task_id.clone()],
        "one task, one process, whichever starter won",
    );
    assert_eq!(fixture.attempts(&task_id).await, 1);

    queue.shutdown();
}

#[tokio::test]
async fn the_queue_starts_nothing_for_a_task_something_else_already_claimed() {
    // The losing side of the same rule, made deterministic: the first task is
    // claimed out from under the queue before it ever starts, so a queue that
    // ignored the claim would spawn a process for it. The second task is what
    // proves the queue kept going rather than merely being asleep.
    let fixture = Fixture::new().await;
    let taken = fixture.add_task("Alpha").await;
    let free = fixture.add_task("Bravo").await;

    assert_eq!(
        scheduler::claim(fixture.ctx(), &taken)
            .await
            .expect("claim the first task"),
        ClaimOutcome::Claimed
    );

    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue();
    queue.start().await.expect("start the queue");

    let finished = free.clone();
    wait_until(
        &fixture,
        &mut changes,
        "the unclaimed task to finish",
        move |board| {
            board
                .iter()
                .any(|task| task.task.id == finished && task.task.column == BoardColumn::InReview)
        },
    )
    .await;

    assert_eq!(fixture.cli.started(), vec![free]);
    assert_eq!(
        fixture.attempts(&taken).await,
        0,
        "the queue must not open a run for a task it does not own"
    );

    queue.shutdown();
}

// ---------------------------------------------------------------------------
// What a crash left behind
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reopening_after_a_crash_shows_one_interrupted_task_and_leaves_the_rest_untouched() {
    // Task 009's acceptance criterion, and seam-contract D9's answer to what
    // the word means: the *run* carries `interrupted`, the *task* lands
    // `failed` and stays in `ready`, and the card reads the word off its last
    // run. `run_state` keeps ADR-0007's seven values and gains no eighth —
    // SQLite cannot widen a CHECK, so that is permanent.
    let fixture = Fixture::new().await;
    let untouched_before = fixture.add_task("Alpha").await;
    let crashed = fixture.add_task("Bravo").await;
    let untouched_after = fixture.add_task("Charlie").await;

    // The state a force-quit leaves: a claimed task with an open `runs` row,
    // written through the same services a real run writes them through.
    scheduler::claim(fixture.ctx(), &crashed)
        .await
        .expect("claim the task the crash caught");
    let run = start_run(
        fixture.ctx(),
        &fixture.paths,
        NewRun {
            task_id: crashed.clone(),
            session_id: "0b6d3e2e-0000-4000-8000-00000000c0de".to_string(),
            prompt: "implement the plan".to_string(),
            base_ref: None,
        },
    )
    .await
    .expect("open the run the crash interrupted");

    let report = startup::survey(&fixture.ctx().pool)
        .await
        .expect("survey the database");
    assert_eq!(
        report.tasks_left_running,
        vec![crashed.clone()],
        "the survey is what decides what counts as left running",
    );

    let reconciled = scheduler::reconcile_interrupted(fixture.ctx(), &report)
        .await
        .expect("reconcile");

    assert_eq!(reconciled, vec![crashed.clone()]);
    let detail = fixture.detail(&crashed).await;
    let closed = detail.last_run.expect("the interrupted run");
    assert_eq!(closed.id, run.id);
    assert_eq!(closed.status, RunStatus::Interrupted);
    assert_eq!(closed.exit_class, Some(ExitClass::Interrupted));
    assert!(closed.ended_at.is_some(), "an interrupted run is over");
    assert_eq!(detail.task.run_state, RunState::Failed);
    assert_eq!(detail.task.column, BoardColumn::Ready);

    // What the card actually reads (seam-contract D12's summary projection).
    let board = fixture.board().await;
    let card = board
        .iter()
        .find(|task| task.task.id == crashed)
        .expect("the crashed task is still on the board");
    assert_eq!(
        card.last_run.as_ref().and_then(|run| run.exit_class),
        Some(ExitClass::Interrupted),
        "the word `interrupted` is read off the last run, not off `run_state`",
    );

    for id in [untouched_before, untouched_after] {
        let task = fixture.task(&id).await;
        assert_eq!(task.run_state, RunState::Idle, "{id} was disturbed");
        assert_eq!(task.column, BoardColumn::Ready);
        assert_eq!(fixture.attempts(&id).await, 0);
    }
}

#[tokio::test]
async fn a_task_claimed_before_its_run_row_existed_still_lands_failed() {
    // The narrower crash: killed between the claim and `start_run`. There is no
    // run to mark, and the task must still not come back reading "running" with
    // a disabled Run now button and no way out.
    let fixture = Fixture::new().await;
    let crashed = fixture.add_task("Alpha").await;
    scheduler::claim(fixture.ctx(), &crashed)
        .await
        .expect("claim the task");

    let report = startup::survey(&fixture.ctx().pool)
        .await
        .expect("survey the database");
    let reconciled = scheduler::reconcile_interrupted(fixture.ctx(), &report)
        .await
        .expect("reconcile");

    assert_eq!(reconciled, vec![crashed.clone()]);
    let detail = fixture.detail(&crashed).await;
    assert_eq!(detail.task.run_state, RunState::Failed);
    assert_eq!(detail.last_run, None);
}

#[tokio::test]
async fn a_task_a_crash_caught_still_queued_is_not_stranded() {
    // Finding 3 of task 009's own verification report: `scheduler::claim`
    // walks `idle -> queued -> running` as two separately committed
    // transitions, so a crash between them leaves a task at `queued` with no
    // open run and no legal edge back to `idle` — invisible to
    // `selection::skip_reason` (which only ever claims from `idle`) and to a
    // "Run now" button disabled by the same badge. Only a database edit
    // could clear it before this repair existed.
    let fixture = Fixture::new().await;
    let crashed = fixture.add_task("Alpha").await;
    walk_to(&fixture, &crashed, RunState::Queued).await;

    let report = startup::survey(&fixture.ctx().pool)
        .await
        .expect("survey the database");
    assert_eq!(
        report.tasks_left_running,
        vec![crashed.clone()],
        "`running` alone would miss this task entirely",
    );

    let reconciled = scheduler::reconcile_interrupted(fixture.ctx(), &report)
        .await
        .expect("reconcile");

    assert_eq!(reconciled, vec![crashed.clone()]);
    let detail = fixture.detail(&crashed).await;
    assert_eq!(
        detail.task.run_state,
        RunState::Cancelled,
        "queued has no `-> failed` edge; cancelled is the one a task with no \
         live process to kill already has",
    );
    assert_eq!(detail.last_run, None, "no run was ever opened for it");

    // The queue must not spend the rest of the night trying to claim it
    // again — it now reads exactly like any other task the user has to act
    // on before it runs.
    let plan = scheduler::plan(fixture.ctx()).await.expect("read the plan");
    assert_eq!(
        plan.iter()
            .find(|entry| entry.task_id == crashed)
            .and_then(|entry| entry.skip),
        Some(SkipReason::NeedsAttention),
    );
}

#[tokio::test]
async fn reconciling_a_task_another_repair_already_settled_still_closes_its_run() {
    // Task 007's `worktree::reconcile` lands a `running` task on `failed` too,
    // when its directory vanished — so a crash that took both leaves two
    // repairs looking at one task in whichever order the startup hook wires
    // them. This is the order where that one went first: the task is already
    // settled, and the open `runs` row still has to be closed or the Runs view
    // shows an attempt that never ends.
    let fixture = Fixture::new().await;
    let crashed = fixture.add_task("Alpha").await;
    scheduler::claim(fixture.ctx(), &crashed)
        .await
        .expect("claim the task");
    let report = startup::survey(&fixture.ctx().pool)
        .await
        .expect("survey the database");
    start_run(
        fixture.ctx(),
        &fixture.paths,
        NewRun {
            task_id: crashed.clone(),
            session_id: "0b6d3e2e-0000-4000-8000-00000000feed".to_string(),
            prompt: "implement the plan".to_string(),
            base_ref: None,
        },
    )
    .await
    .expect("open the run the crash interrupted");
    tasks::set_run_state(fixture.ctx(), &crashed, RunState::Failed)
        .await
        .expect("the other repair got there first");

    scheduler::reconcile_interrupted(fixture.ctx(), &report)
        .await
        .expect("reconcile");

    let detail = fixture.detail(&crashed).await;
    let run = detail.last_run.expect("the interrupted run");
    assert_eq!(run.status, RunStatus::Interrupted);
    assert_eq!(run.exit_class, Some(ExitClass::Interrupted));
    assert!(run.ended_at.is_some());
    assert_eq!(
        detail.task.run_state,
        RunState::Failed,
        "the state the other repair produced is not walked backwards",
    );
}

#[tokio::test]
async fn a_clean_previous_exit_leaves_the_reconciliation_nothing_to_do() {
    let fixture = Fixture::new().await;
    fixture.add_task("Alpha").await;

    let report = startup::survey(&fixture.ctx().pool)
        .await
        .expect("survey the database");

    assert!(report.is_empty());
    assert_eq!(
        scheduler::reconcile_interrupted(fixture.ctx(), &report)
            .await
            .expect("reconcile"),
        Vec::<String>::new()
    );
}

#[tokio::test]
async fn a_reconciled_task_is_not_picked_up_again_by_the_queue() {
    // The consequence that makes the repair worth doing at all: `failed` is not
    // a state the queue re-selects, so an interrupted task waits for the user
    // instead of being restarted into the same wall (ADR-0007's "failed tasks
    // accumulate in `ready` unless the user acts").
    let fixture = Fixture::new().await;
    let crashed = fixture.add_task("Alpha").await;
    scheduler::claim(fixture.ctx(), &crashed)
        .await
        .expect("claim the task");
    let report = startup::survey(&fixture.ctx().pool)
        .await
        .expect("survey the database");
    scheduler::reconcile_interrupted(fixture.ctx(), &report)
        .await
        .expect("reconcile");

    let plan = scheduler::plan(fixture.ctx()).await.expect("read the plan");

    assert_eq!(
        plan.iter()
            .find(|entry| entry.task_id == crashed)
            .and_then(|entry| entry.skip),
        Some(SkipReason::NeedsAttention)
    );
    assert_eq!(scheduler::next_to_start(&plan), None);
}

// ---------------------------------------------------------------------------
// Waiting on the queue without waiting on a clock
// ---------------------------------------------------------------------------

/// Blocks until `predicate` holds against a freshly read board.
///
/// Every wake is a real `ChangeEvent` publication (ADR-0018), so this polls
/// nothing and sleeps for nothing; the outer timeout only exists so a queue
/// that has stopped making progress fails with a sentence instead of hanging
/// the job.
async fn wait_until(
    fixture: &Fixture,
    changes: &mut Receiver<ChangeEvent>,
    what: &str,
    predicate: impl Fn(&[TaskSummary]) -> bool,
) {
    let waiting = async {
        loop {
            if predicate(&fixture.board().await) {
                return;
            }
            // `Lagged` is a wake like any other: the reaction to both is to
            // re-read, which is what the next pass does.
            if changes.recv().await == Err(tokio::sync::broadcast::error::RecvError::Closed) {
                panic!("nothing can change any more while waiting for {what}");
            }
        }
    };

    tokio::time::timeout(TEST_TIMEOUT, waiting)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

/// Resolves once a run has published its first live-tail snapshot.
///
/// The run reporting its own progress (seam-contract D14), which is the only
/// honest answer to "is it in flight yet" that costs no sleep.
async fn once_the_run_is_live(mut tail: Receiver<RunTail>) {
    tokio::time::timeout(TEST_TIMEOUT, tail.recv())
        .await
        .expect("a run must report itself in flight")
        .expect("the tail sender outlives the run");
}

/// Resolves once `queue` has registered `task_id` as its in-flight task.
///
/// Cooperative polling rather than a channel: `Shared::begin` — unlike
/// everything else this file waits on — publishes nothing, because it is an
/// in-process bookkeeping write, not a database mutation. `yield_now` costs
/// no real time and never guesses a duration; it converges the moment the
/// loop actually reaches `begin`, which on the held `--version` probe the
/// tests using this are built around, is almost immediately.
async fn wait_until_in_flight(queue: &QueueHandle, task_id: &str) {
    tokio::time::timeout(TEST_TIMEOUT, async {
        while queue.in_flight_task_id().as_deref() != Some(task_id) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the queue never registered {task_id} as in flight"));
}

/// The other side of [`wait_until_in_flight`]: resolves once the queue has
/// released whatever it held, win or lose.
async fn wait_until_not_in_flight(queue: &QueueHandle) {
    tokio::time::timeout(TEST_TIMEOUT, async {
        while queue.in_flight_task_id().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the queue never released its in-flight task");
}

/// What the task's most recent attempt recorded as its base — ADR-0008's
/// amendment, read back off the `runs` row rather than re-resolved, which is
/// the whole point of it being written there.
async fn recorded_base_ref(fixture: &Fixture, task_id: &str) -> Option<String> {
    sqlx::query_scalar!(
        "SELECT base_ref FROM runs WHERE task_id = ?1 ORDER BY attempt DESC LIMIT 1",
        task_id,
    )
    .fetch_optional(&fixture.ctx().pool)
    .await
    .expect("read the run's base ref")
    .flatten()
}

fn position_of(positions: &[(&str, Option<i64>)], task_id: &str) -> Option<i64> {
    positions
        .iter()
        .find(|(id, _)| *id == task_id)
        .and_then(|(_, position)| *position)
}

/// Releases a gated stand-in.
fn open(gate: &Path) {
    std::fs::write(gate, "go\n").expect("open the gate");
}

// ---------------------------------------------------------------------------
// A stand-in for the CLI that can behave differently per task
// ---------------------------------------------------------------------------

/// One script, dispatching on the task whose worktree it was started in.
///
/// A queue's interesting scenarios are the ones where the second task does not
/// behave like the first, and `RunnerConfig` carries one program for the whole
/// queue — so the difference has to live inside the script. The worktree is
/// `<worktree-root>/<task-id>` (ADR-0005), which makes the child's own working
/// directory the dispatch key and needs nothing threaded through the runner.
///
/// A deliberate copy of the technique in `tests/runner_process.rs` rather than
/// a shared helper: each integration test is its own binary, and a `mod common`
/// shared between them would make either file's stand-in awkward to change for
/// the other's sake.
struct FakeCli {
    /// Held for its `Drop`; every path below points inside it.
    dir: TempDir,
}

impl FakeCli {
    /// A stand-in that replays `success.jsonl` for every task nothing else has
    /// been said about.
    fn new() -> Self {
        let cli = Self {
            dir: tempfile::Builder::new()
                .prefix("rimaia-fake-cli-")
                .tempdir()
                .expect("temp dir for the stand-in CLI"),
        };
        cli.write_plan(
            "default",
            &[
                "replay",
                &fixture_path("success").display().to_string(),
                "0",
                "",
            ],
        );
        cli.write_script();
        cli
    }

    fn program(&self) -> PathBuf {
        self.path("claude")
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Replays `fixture` for `task_id` and exits with `code`.
    fn replays(&self, task_id: &str, fixture: &str, code: i32) {
        self.write_plan(
            task_id,
            &[
                "replay",
                &fixture_path(fixture).display().to_string(),
                &code.to_string(),
                "",
            ],
        );
    }

    /// Replays the first `head` lines of `fixture` for `task_id`, then waits
    /// for the returned gate file to appear before replaying the rest.
    ///
    /// Not a new fixture: the same recorded bytes handed over in two pieces so
    /// a test can act between them, exactly as `tests/runner_process.rs` splits
    /// a recording around a signal.
    fn gates(&self, task_id: &str, fixture: &str, head: usize) -> PathBuf {
        let (head_file, rest_file) = self.split(task_id, fixture, head);
        let gate = self.path(&format!("gate-{task_id}"));
        self.write_plan(
            task_id,
            &[
                "gate",
                &head_file.display().to_string(),
                &rest_file.display().to_string(),
                &gate.display().to_string(),
            ],
        );
        gate
    }

    /// Replays the first `head` lines of `fixture` for `task_id` and then waits
    /// to be stopped — a run that is going nowhere until somebody cancels it.
    fn hangs(&self, task_id: &str, fixture: &str, head: usize) {
        let (head_file, _) = self.split(task_id, fixture, head);
        self.write_plan(task_id, &["hang", &head_file.display().to_string(), "", ""]);
    }

    /// The task ids the stand-in was started in, in the order it was started.
    fn started(&self) -> Vec<String> {
        self.spawns()
            .into_iter()
            .filter_map(|line| line.strip_prefix("start ").map(str::to_string))
            .collect()
    }

    /// Asserts no two runs overlapped — the sequential half of ADR-0010's
    /// sequential mode, read off the processes themselves rather than off the
    /// rows they wrote.
    fn assert_never_two_at_once(&self) {
        let mut open: Option<String> = None;
        for line in self.spawns() {
            match line.split_once(' ') {
                Some(("start", task_id)) => {
                    assert_eq!(open, None, "{task_id} started while {open:?} was running");
                    open = Some(task_id.to_string());
                }
                Some(("end", task_id)) => {
                    assert_eq!(
                        open.as_deref(),
                        Some(task_id),
                        "{task_id} ended out of turn"
                    );
                    open = None;
                }
                _ => panic!("unreadable spawn log line: {line}"),
            }
        }
    }

    fn spawns(&self) -> Vec<String> {
        match std::fs::read_to_string(self.path("spawns")) {
            Ok(log) => log.lines().map(str::to_string).collect(),
            // Nothing has been spawned yet, which is a result and not a
            // failure — several tests assert exactly that.
            Err(_) => Vec::new(),
        }
    }

    fn split(&self, task_id: &str, fixture: &str, head: usize) -> (PathBuf, PathBuf) {
        let lines: Vec<String> = fixture_lines(fixture).collect();
        assert!(head < lines.len(), "{fixture} is shorter than {head} lines");

        let head_file = self.path(&format!("head-{task_id}.jsonl"));
        let rest_file = self.path(&format!("rest-{task_id}.jsonl"));
        std::fs::write(&head_file, lines[..head].join("\n") + "\n").expect("write the head");
        std::fs::write(&rest_file, lines[head..].join("\n") + "\n").expect("write the rest");
        (head_file, rest_file)
    }

    /// One directive, one line per field, so a path containing a space survives
    /// `read` — the same reason the production code builds argument vectors and
    /// never `sh -c`.
    fn write_plan(&self, key: &str, fields: &[&str; 4]) {
        std::fs::write(self.path(&format!("plan-{key}")), fields.join("\n") + "\n")
            .expect("write a stand-in directive");
    }

    /// Makes every future `--version` probe block until
    /// [`release_version_probe`](Self::release_version_probe) is called —
    /// widening, for a test, the window `try_step` leaves open between
    /// registering its `CancelSignal` and actually claiming a task (finding 4
    /// of task 009's own verification report). The probe otherwise answers
    /// instantly, which is correct for every other test in this file and
    /// exactly why this is opt-in rather than the default.
    fn hold_version_probe(&self) {
        std::fs::write(self.path("version-hold"), "").expect("arm the version-probe gate");
    }

    /// Releases a probe blocked by [`hold_version_probe`](Self::hold_version_probe).
    fn release_version_probe(&self) {
        std::fs::write(self.path("version-go"), "").expect("release the version-probe gate");
    }

    /// A shebang script, executed directly rather than through `sh -c`.
    ///
    /// `--version` short-circuits before anything else: the runner probes for
    /// the prerequisite before it starts a run, and the probe runs in Rimaia's
    /// own working directory, where the dispatch below would find no task.
    /// It waits on `version-hold`/`version-go` first, so a test can hold it
    /// open — see [`hold_version_probe`](Self::hold_version_probe).
    ///
    /// `pwd -P` rather than the `pwd` builtin's default, because `PWD` is
    /// inherited from the parent and `Command::current_dir` does not update it.
    fn write_script(&self) {
        let script = format!(
            "#!/bin/sh\n\
             if [ \"$1\" = '--version' ]; then\n\
             if [ -f '{dir}/version-hold' ]; then\n\
             while [ ! -f '{dir}/version-go' ]; do sleep 0.02; done\n\
             fi\n\
             echo '2.1.234 (Claude Code)'; exit 0\n\
             fi\n\
             dir='{dir}'\n\
             task=\"$(basename \"$(pwd -P)\")\"\n\
             cat > \"$dir/stdin-$task\"\n\
             printf 'start %s\\n' \"$task\" >> \"$dir/spawns\"\n\
             plan=\"$dir/plan-$task\"\n\
             if [ ! -f \"$plan\" ]; then plan=\"$dir/plan-default\"; fi\n\
             {{ read -r mode; read -r one; read -r two; read -r three; }} < \"$plan\"\n\
             case \"$mode\" in\n\
             replay)\n\
               cat \"$one\"\n\
               printf 'end %s\\n' \"$task\" >> \"$dir/spawns\"\n\
               exit \"$two\"\n\
               ;;\n\
             gate)\n\
               cat \"$one\"\n\
               while [ ! -f \"$three\" ]; do sleep 0.02; done\n\
               cat \"$two\"\n\
               printf 'end %s\\n' \"$task\" >> \"$dir/spawns\"\n\
               exit 0\n\
               ;;\n\
             hang)\n\
               cat \"$one\"\n\
               sleep 300\n\
               ;;\n\
             esac\n",
            dir = self.dir.path().display(),
        );

        let program = self.program();
        std::fs::write(&program, script).expect("write the stand-in CLI");
        make_executable(&program);
    }
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .expect("make the stand-in executable");
}

// ---------------------------------------------------------------------------
// A board with runnable tasks on it
// ---------------------------------------------------------------------------

struct Fixture {
    harness: TestContext,
    /// Held for their `Drop`; the paths below point inside them.
    _repositories: Vec<TempRepo>,
    _data: TempDir,
    paths: AppPaths,
    repository_id: String,
    cli: FakeCli,
}

impl Fixture {
    /// One real git repository, registered and opted in to unattended runs, and
    /// a stand-in CLI that succeeds for every task.
    async fn new() -> Self {
        let harness = TestContext::new().await;
        let data = tempfile::Builder::new()
            .prefix("rimaia-data-")
            .tempdir()
            .expect("temp dir for the app data directory");
        let paths = AppPaths::new(data.path());
        paths.create_all().expect("the app data directories");

        let mut fixture = Self {
            harness,
            _repositories: Vec::new(),
            _data: data,
            paths,
            repository_id: String::new(),
            cli: FakeCli::new(),
        };
        fixture.repository_id = fixture.register_repository(true).await;
        fixture
    }

    fn ctx(&self) -> &ServiceContext {
        &self.harness.context
    }

    fn runner(&self) -> RunnerConfig {
        RunnerConfig {
            program: self.cli.program(),
            ..RunnerConfig::default()
        }
    }

    /// Wires a queue over this fixture's context and spawns its one long-lived
    /// task. Several tests build two, standing in for two launches.
    fn spawn_queue(&self) -> QueueHandle {
        let (handle, task) = scheduler::build(
            self.harness.context.clone(),
            self.paths.clone(),
            self.runner(),
        );
        tokio::spawn(task.run());
        handle
    }

    /// Registers another real repository, with ADR-0012's opt-in on or off.
    async fn register_repository(&mut self, opt_in: bool) -> String {
        let repository = TempRepo::init();
        let registered = repo::register(
            self.ctx(),
            &self.paths.worktrees_dir(),
            NewRepository {
                path: repository.path().to_string_lossy().into_owned(),
                name: None,
                worktree_root: None,
            },
        )
        .await
        .expect("register a test repository");
        self._repositories.push(repository);

        if opt_in {
            repo::set_allow_unattended_runs(self.ctx(), &registered.id, true)
                .await
                .expect("ADR-0012's per-repository opt-in");
        }
        registered.id
    }

    /// Appends a `ready` task with a plan to the opted-in repository, so
    /// creation order is board order.
    async fn add_task(&self, title: &str) -> String {
        self.add_task_in(&self.repository_id.clone(), title).await
    }

    async fn add_task_in(&self, repository_id: &str, title: &str) -> String {
        tasks::create_task(
            self.ctx(),
            NewTask {
                repository_id: repository_id.to_string(),
                title: title.to_string(),
                plan: Some(format!("1. Implement {title}\n2. Test it")),
                extra_instructions: None,
                column: Some(BoardColumn::Ready),
                links: vec![],
            },
        )
        .await
        .expect("create a ready task")
        .id
    }

    async fn board(&self) -> Vec<TaskSummary> {
        tasks::list_tasks(self.ctx(), TaskFilter::default())
            .await
            .expect("read the board")
    }

    async fn detail(&self, task_id: &str) -> tasks::TaskDetail {
        tasks::get_task(self.ctx(), task_id)
            .await
            .expect("read the task")
    }

    async fn task(&self, task_id: &str) -> Task {
        self.detail(task_id).await.task
    }

    /// How many `runs` rows a task has — the row-level answer to "how many
    /// times was this started", beside the process-level one the stand-in's own
    /// log gives.
    async fn attempts(&self, task_id: &str) -> i64 {
        sqlx::query_scalar!(
            r#"SELECT count(*) AS "count!: i64" FROM runs WHERE task_id = ?1"#,
            task_id
        )
        .fetch_one(&self.ctx().pool)
        .await
        .expect("count the attempts")
    }
}
