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

use std::collections::HashSet;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use pretty_assertions::assert_eq;
use rimaia_core::db::{BoardColumn, ExitClass, RunState, RunStatus, ScheduleMode, Task};
use rimaia_core::repo::{self, NewRepository};
use rimaia_core::runner::events::RunTail;
use rimaia_core::runner::outcome::{start_run, NewRun};
use rimaia_core::runner::prompt::compose_resume_prompt;
use rimaia_core::runner::{run_task, CancelSignal, RunRequest, RunTrigger, RunnerConfig};
use rimaia_core::schedule::window::RunWindow;
use rimaia_core::schedule::{self as scheduler_schedule, ScheduleInput};
use rimaia_core::scheduler::{
    self, capacity, ClaimOutcome, InFlight, LeaseOwner, QueueHandle, QueueState, SkipReason,
};
use rimaia_core::startup;
use rimaia_core::tasks::{self, NewTask, TaskFilter, TaskSummary};
use rimaia_core::testing::{open_gate as open, FakeCli, TempRepo, TestContext};
use rimaia_core::{AppPaths, ChangeEvent, Clock, ServiceContext};
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
    // The stand-in implements `--version` and a run, and refuses everything
    // else with its own name in the message. Asserted on the fullest run of
    // the loop there is, because the failure it guards against is *silent*: a
    // subcommand that fell through to the run path would write a phantom
    // `start <task>` into the log every ordering assertion in this file reads.
    fixture.cli.assert_nothing_fell_through();
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
// Several at once (task 012, ADR-0010)
//
// Every test here reads overlap off the stand-in's own start/end log rather
// than off the rows the runs wrote. Rows cannot answer it: two `runs` rows are
// both `running` for a while whether or not the two processes ever coexisted,
// and `started_at` has second-ish resolution against a fake clock. The log is
// written by the processes themselves, which is the only witness that two of
// them were alive at the same instant.
//
// Overlap is *forced* rather than hoped for, by gating every stand-in and
// opening the gates only once each has written its `start` line. A replaying
// stand-in exits in microseconds, so three of them started from a JoinSet would
// very likely never coexist and a test that asserted they did would fail for
// the wrong reason.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn three_tasks_in_three_repositories_run_at_once_when_max_concurrency_is_three() {
    // Task 012's first acceptance criterion. Three repositories, because
    // ADR-0010 caps each one at a single run: parallelism *across* repositories
    // is the safe default, and this is that default doing its job.
    let mut fixture = Fixture::new().await;
    let mut tasks_and_gates = Vec::new();
    for index in 0..3 {
        let repository = fixture.register_repository(true).await;
        let task_id = fixture
            .add_task_in(&repository, &format!("Task {index}"))
            .await;
        let gate = fixture.cli.gates(&task_id, "success", HEAD_LINES);
        tasks_and_gates.push((task_id, gate));
    }
    fixture.set_parallel(3).await;

    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue();
    queue.start().await.expect("start the queue");

    wait_until_started(&fixture, 3).await;
    fixture.cli.assert_overlapped(3);

    for (_, gate) in &tasks_and_gates {
        open(gate);
    }
    let expected: Vec<String> = tasks_and_gates.iter().map(|(id, _)| id.clone()).collect();
    let settled = expected.clone();
    wait_until(
        &fixture,
        &mut changes,
        "all three tasks to reach in_review",
        move |board| {
            board
                .iter()
                .filter(|task| settled.contains(&task.task.id))
                .all(|task| task.task.column == BoardColumn::InReview)
        },
    )
    .await;

    for task_id in &expected {
        assert_eq!(fixture.task(task_id).await.run_state, RunState::Idle);
    }

    queue.shutdown();
}

#[tokio::test]
async fn two_tasks_in_one_repository_run_one_after_the_other_by_default() {
    // The other half of the same rule, and the one that costs something: the
    // global limit says three, the repository says one, and the repository
    // wins. "Two agents in two worktrees of the same repo is safe for git, but
    // they will fight over ports, test databases, and lockfiles."
    let fixture = Fixture::new().await;
    let first = fixture.add_task("Alpha").await;
    let second = fixture.add_task("Bravo").await;
    fixture.set_parallel(3).await;

    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue();
    queue.start().await.expect("start the queue");

    wait_until(
        &fixture,
        &mut changes,
        "both tasks to reach in_review",
        |board| {
            board
                .iter()
                .all(|task| task.task.column == BoardColumn::InReview)
        },
    )
    .await;

    assert_eq!(fixture.cli.started(), vec![first, second]);
    fixture.cli.assert_never_two_at_once();

    queue.shutdown();
}

#[tokio::test]
async fn two_tasks_in_one_repository_run_at_once_once_that_repository_opts_out_of_the_cap() {
    // ADR-0010's opt-out, which is the whole reason the cap is a column and not
    // a constant.
    let fixture = Fixture::new().await;
    let first = fixture.add_task("Alpha").await;
    let second = fixture.add_task("Bravo").await;
    let gates = [
        fixture.cli.gates(&first, "success", HEAD_LINES),
        fixture.cli.gates(&second, "success", HEAD_LINES),
    ];
    fixture.set_parallel(3).await;
    fixture.opt_repository_out_of_the_cap(2).await;

    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue();
    queue.start().await.expect("start the queue");

    wait_until_started(&fixture, 2).await;
    fixture.cli.assert_overlapped(2);

    for gate in &gates {
        open(gate);
    }
    wait_until(
        &fixture,
        &mut changes,
        "both tasks to reach in_review",
        |board| {
            board
                .iter()
                .all(|task| task.task.column == BoardColumn::InReview)
        },
    )
    .await;

    queue.shutdown();
}

#[tokio::test]
async fn two_worktrees_in_one_repository_are_prepared_without_fighting_over_the_git_lock() {
    // `worktree::prepare` runs `git fetch --prune`, `git worktree prune` and
    // `git worktree add` against the **shared** repository, and two of those
    // take `.git`-level locks. Worktree isolation (ADR-0005) is about the
    // working trees; it says nothing about the administrative directory they
    // are all registered in. With a per-repository cap of one this can never
    // happen — which is why it is invisible until the opt-out above is used,
    // and why it would first appear as a raw `index.lock` error at 2am.
    //
    // Held from the outside, so this asserts that `run_task` *takes* the lock
    // rather than that a `tokio::Mutex` works: the run cannot spawn a process
    // without a worktree, so "nothing started while it was held" is the
    // observation. `converge` yields rather than sleeping — see its own note.
    let fixture = Fixture::new().await;
    let blocked = fixture.add_task("Alpha").await;
    let registry = InFlight::new();
    let held = registry
        .preparation_lock(&fixture.repository_id)
        .lock_owned()
        .await;

    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue_with(registry.clone());
    queue.start().await.expect("start the queue");

    wait_until_in_flight(&queue, &blocked).await;
    converge().await;
    assert_eq!(
        fixture.cli.started(),
        Vec::<String>::new(),
        "a run whose repository is mid-preparation must wait for the lock, not race it",
    );
    assert_eq!(
        fixture.task(&blocked).await.worktree_path,
        None,
        "and it must not have created a worktree behind the lock either",
    );

    drop(held);

    let finished = blocked.clone();
    wait_until(
        &fixture,
        &mut changes,
        "the released task to finish",
        move |board| {
            board
                .iter()
                .any(|task| task.task.id == finished && task.task.column == BoardColumn::InReview)
        },
    )
    .await;

    assert!(fixture.task(&blocked).await.worktree_path.is_some());

    queue.shutdown();
}

#[tokio::test]
async fn cancelling_one_run_leaves_the_others_running() {
    // Task 012's third acceptance criterion. The cancel is per lease, and the
    // lease is per task — the registry's own `cancel(task_id)`, which is the
    // same door the Cancel button on a card uses.
    let mut fixture = Fixture::new().await;
    let other_repository = fixture.register_repository(true).await;
    let doomed = fixture.add_task("Alpha").await;
    let survivor = fixture.add_task_in(&other_repository, "Bravo").await;
    fixture
        .cli
        .hangs(&doomed, "interrupted-sigterm", HEAD_LINES);
    let survivor_gate = fixture.cli.gates(&survivor, "success", HEAD_LINES);
    fixture.set_parallel(2).await;

    let registry = InFlight::new();
    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue_with(registry.clone());
    queue.start().await.expect("start the queue");

    wait_until_started(&fixture, 2).await;
    assert!(registry.cancel(&doomed), "the doomed run is in flight");

    let cancelled = doomed.clone();
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

    // The survivor was never asked to stop and is still there to be released,
    // which is the assertion: a cancel that reached both would have killed it
    // before this gate ever opened.
    assert!(
        registry.holds(&survivor),
        "cancelling one run must leave the other's lease alone"
    );
    open(&survivor_gate);

    let finished = survivor.clone();
    wait_until(
        &fixture,
        &mut changes,
        "the surviving run to finish",
        move |board| {
            board
                .iter()
                .any(|task| task.task.id == finished && task.task.column == BoardColumn::InReview)
        },
    )
    .await;

    let detail = fixture.detail(&doomed).await;
    assert_eq!(
        detail
            .last_run
            .expect("a cancelled run records itself")
            .status,
        RunStatus::Cancelled,
    );
    assert_eq!(fixture.task(&survivor).await.run_state, RunState::Idle);

    queue.shutdown();
}

#[tokio::test]
async fn two_concurrent_runs_tail_under_their_own_run_ids() {
    // Task 012's fourth acceptance criterion: "live logs stay attributed to the
    // correct run under concurrency". Asserted on the channel (seam-contract
    // D14) rather than in the UI, because the UI's filter is only correct if
    // what it filters is: every snapshot must carry the run id of the run that
    // produced it, and no snapshot may ever carry the other's.
    let mut fixture = Fixture::new().await;
    let other_repository = fixture.register_repository(true).await;
    let first = fixture.add_task("Alpha").await;
    let second = fixture.add_task_in(&other_repository, "Bravo").await;
    let gates = [
        fixture.cli.gates(&first, "success", HEAD_LINES),
        fixture.cli.gates(&second, "success", HEAD_LINES),
    ];
    fixture.set_parallel(2).await;

    let mut tail = fixture.ctx().subscribe_tail();
    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue();
    queue.start().await.expect("start the queue");

    wait_until_started(&fixture, 2).await;

    // Collect snapshots until both runs have spoken, so the assertion is about
    // two live tails and not about one run that happened to publish twice.
    let mut seen: HashSet<String> = HashSet::new();
    tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            match tail.recv().await {
                Ok(snapshot) => {
                    seen.insert(snapshot.run_id);
                    if seen.len() == 2 {
                        return;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(error) => panic!("the tail channel closed: {error}"),
            }
        }
    })
    .await
    .expect("both runs must report themselves");

    // The invariant `ActiveRunCard` rests on, stated as an equality: the run
    // ids on the channel are exactly the run ids the two cards resolve from
    // `get_task(..).last_run`. A snapshot carrying the other run's id would
    // land in the wrong card, and one carrying an id belonging to neither would
    // land in no card at all.
    let mut expected: HashSet<String> = HashSet::new();
    for task_id in [&first, &second] {
        expected.insert(fixture.last_run_id(task_id).await);
    }
    assert_eq!(seen, expected);

    // And each of those ids belongs to one task, not to both — the set
    // equality above would still hold if one run had somehow been recorded
    // against the other's card.
    let mut owners: HashSet<String> = HashSet::new();
    for run_id in &seen {
        owners.insert(fixture.task_of_run(run_id).await);
    }
    assert_eq!(owners, HashSet::from([first.clone(), second.clone()]));

    for gate in &gates {
        open(gate);
    }
    wait_until(
        &fixture,
        &mut changes,
        "both tasks to reach in_review",
        |board| {
            board
                .iter()
                .all(|task| task.task.column == BoardColumn::InReview)
        },
    )
    .await;

    queue.shutdown();
}

#[tokio::test]
async fn a_dependent_task_never_starts_before_its_dependency_succeeds_even_with_free_slots() {
    // Task 012's fifth acceptance criterion. Task 011 is landing in parallel
    // with this one and has not merged here — `list_tasks` still returns
    // `blocked_by_incomplete` as a constant `0` (seam-contract D12), so there
    // is no way to make that predicate fire from a test yet. `run_state =
    // blocked` is ADR-0010's own spelling of the same condition and the arm
    // `skip_reason` already routes to `DependencyNotSatisfied`, so this asserts
    // the property through the spelling that is live today. When 011 lands,
    // the other spelling is covered by the same predicate and this test needs
    // no edit.
    let mut fixture = Fixture::new().await;
    let other_repository = fixture.register_repository(true).await;
    let dependent = fixture.add_task("Blocked").await;
    let free = fixture.add_task_in(&other_repository, "Free").await;
    walk_to(&fixture, &dependent, RunState::Blocked).await;
    // Four slots for two tasks: whatever stops the blocked one, it is not room.
    fixture.set_parallel(4).await;

    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue();
    queue.start().await.expect("start the queue");

    let finished = free.clone();
    wait_until(
        &fixture,
        &mut changes,
        "the unblocked task to finish",
        move |board| {
            board
                .iter()
                .any(|task| task.task.id == finished && task.task.column == BoardColumn::InReview)
        },
    )
    .await;
    converge().await;

    assert_eq!(
        fixture.cli.started(),
        vec![free],
        "a blocked task must not start however many slots are free",
    );
    assert_eq!(fixture.task(&dependent).await.run_state, RunState::Blocked);
    assert_eq!(
        scheduler::plan(fixture.ctx())
            .await
            .expect("read the plan")
            .iter()
            .find(|entry| entry.task_id == dependent)
            .and_then(|entry| entry.skip),
        Some(SkipReason::DependencyNotSatisfied),
    );

    queue.shutdown();
}

#[tokio::test]
async fn sequential_mode_still_starts_exactly_one_at_a_time() {
    // The regression that matters most, because the whole design of
    // `capacity::resolve` is "sequential is not a special case, it is
    // `global = 1` on the same path". Three tasks in three repositories — every
    // per-repository cap is satisfied, so the only thing keeping them apart is
    // the mode.
    let mut fixture = Fixture::new().await;
    let mut expected = Vec::new();
    for index in 0..3 {
        let repository = fixture.register_repository(true).await;
        expected.push(
            fixture
                .add_task_in(&repository, &format!("Task {index}"))
                .await,
        );
    }
    // Deliberately stored, and deliberately not in force.
    fixture.set_max_concurrency(3).await;

    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue();
    queue.start().await.expect("start the queue");

    let settled = expected.clone();
    wait_until(
        &fixture,
        &mut changes,
        "every task to reach in_review",
        move |board| {
            board
                .iter()
                .filter(|task| settled.contains(&task.task.id))
                .all(|task| task.task.column == BoardColumn::InReview)
        },
    )
    .await;

    // A set, not a sequence: with the board filter on "all repositories" the
    // plan groups by repository before position, and a fixture repository's
    // name is its temp directory's basename — so *which* of three independent
    // repositories goes first is not a property this test has any business
    // pinning. What it pins is that all three ran and that no two ever
    // overlapped.
    assert_eq!(
        fixture.cli.started().into_iter().collect::<HashSet<_>>(),
        expected.iter().cloned().collect::<HashSet<_>>(),
    );
    fixture.cli.assert_never_two_at_once();

    queue.shutdown();
}

#[tokio::test]
async fn a_manual_run_occupies_a_slot_the_queue_then_does_not_use() {
    // D19 point 5: the caps bound the scheduler, not a human. A "Run now" takes
    // `acquire_unbounded` and is not refused by them — but it is still *in* the
    // registry, so the queue counts it and starts one fewer. The alternative
    // would be a global limit of two meaning three processes whenever somebody
    // pressed a button.
    let mut fixture = Fixture::new().await;
    let manual_repository = fixture.register_repository(true).await;
    let manual = fixture.add_task_in(&manual_repository, "By hand").await;
    let queued = fixture.add_task("Queued").await;
    fixture.set_parallel(1).await;

    // Leased and claimed, as the button does both.
    walk_to(&fixture, &manual, RunState::Running).await;
    let registry = InFlight::new();
    let manual_lease = registry
        .acquire_unbounded(&manual, &manual_repository, LeaseOwner::Manual)
        .expect("a person clicking Run now is not refused by the caps");

    let queue = fixture.spawn_queue_with(registry.clone());
    queue.start().await.expect("start the queue");
    converge().await;

    assert_eq!(
        fixture.cli.started(),
        Vec::<String>::new(),
        "the one slot is taken by a run the queue did not start",
    );
    assert_eq!(fixture.task(&queued).await.run_state, RunState::Idle);
    assert_eq!(queue.in_flight_task_ids(), vec![manual.clone()]);

    // And a Stop leaves it alone (D19 point 4), which is what makes the slot
    // genuinely the human's rather than merely first in line.
    queue.stop().await.expect("stop the queue");
    assert!(!manual_lease.cancel_signal().is_cancelled());

    drop(manual_lease);
    queue.shutdown();
}

#[tokio::test]
async fn the_queue_starts_the_next_task_when_a_slot_is_freed_and_nothing_else_changes() {
    // The stall this codebase would otherwise ship. `finish_run` publishes its
    // `ChangeEvent`s from inside `run_task`, while the lease is still held, so
    // a loop woken only by that channel counts the finishing run, finds no
    // capacity and goes back to sleep — with nothing left to wake it when the
    // lease actually drops. A queue asleep at 2am with a free slot.
    //
    // Driven through a *manual* lease rather than a queued run, because that is
    // the case only the `releases` watch can cover: the `JoinSet` arm never
    // joins a run this queue did not spawn. Dropping the lease below publishes
    // no `ChangeEvent`, sends no control signal and completes no task — it is
    // literally the only thing that happens.
    let mut fixture = Fixture::new().await;
    let occupied_repository = fixture.register_repository(true).await;
    let occupant = fixture.add_task_in(&occupied_repository, "Occupant").await;
    let waiting = fixture.add_task("Waiting").await;
    fixture.set_parallel(1).await;

    // Claimed as well as leased, which is what a real "Run now" does: the
    // button takes the lease and then walks the row to `running`. Without the
    // claim the occupant is still an ordinary `ready` card, and the queue would
    // be free to pick *it* out of the plan the moment the slot opened — which
    // is a different (and correct) thing happening, and not the one under test.
    walk_to(&fixture, &occupant, RunState::Running).await;
    let registry = InFlight::new();
    let occupying = registry
        .acquire_unbounded(&occupant, &occupied_repository, LeaseOwner::Manual)
        .expect("the only slot");

    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue_with(registry.clone());
    queue.start().await.expect("start the queue");

    // The queue has now looked, found no room, and parked on its `select!`.
    converge().await;
    assert_eq!(fixture.cli.started(), Vec::<String>::new());

    drop(occupying);

    let finished = waiting.clone();
    wait_until(
        &fixture,
        &mut changes,
        "the waiting task to run once the slot opened",
        move |board| {
            board
                .iter()
                .any(|task| task.task.id == finished && task.task.column == BoardColumn::InReview)
        },
    )
    .await;

    assert_eq!(fixture.cli.started(), vec![waiting]);

    queue.shutdown();
}

#[tokio::test]
async fn shutdown_waits_for_every_run_it_started() {
    // The reason `run` drains its `JoinSet` instead of dropping it. Dropping a
    // `JoinSet` aborts its tasks, and an aborted supervisor never reaches
    // `finish_run` — so the attempt has an open `runs` row and comes back
    // `interrupted` on the next launch, for no reason at all. With N runs it is
    // N of them.
    let mut fixture = Fixture::new().await;
    let other_repository = fixture.register_repository(true).await;
    let first = fixture.add_task("Alpha").await;
    let second = fixture.add_task_in(&other_repository, "Bravo").await;
    let gates = [
        fixture.cli.gates(&first, "success", HEAD_LINES),
        fixture.cli.gates(&second, "success", HEAD_LINES),
    ];
    fixture.set_parallel(2).await;

    let (queue, task) = fixture.build_queue(InFlight::new());
    let loop_handle = tokio::spawn(task.run());
    queue.start().await.expect("start the queue");
    wait_until_started(&fixture, 2).await;

    // Shutdown while both are mid-run. It must not cancel them — that is the
    // exit path's job, not this one's — so both gates still have to be opened
    // for the loop to end.
    queue.shutdown();
    for gate in &gates {
        open(gate);
    }

    tokio::time::timeout(TEST_TIMEOUT, loop_handle)
        .await
        .expect("the loop must end once its runs do")
        .expect("the loop task itself does not panic");

    for task_id in [&first, &second] {
        let detail = fixture.detail(task_id).await;
        let run = detail
            .last_run
            .expect("a run this queue started and saw out");
        assert_eq!(run.status, RunStatus::Succeeded, "{task_id}");
        assert_ne!(
            run.exit_class,
            Some(ExitClass::Interrupted),
            "a run the queue waited for is not a run a crash caught: {task_id}",
        );
        assert!(run.ended_at.is_some(), "{task_id} has an open runs row");
        assert_eq!(detail.task.run_state, RunState::Idle, "{task_id}");
    }
    assert!(
        queue.in_flight_task_ids().is_empty(),
        "every lease is released by the time the loop returns",
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
        after_the_crash.in_flight_task_ids(),
        Vec::<String>::new(),
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
        // `Idle -> Blocked` is deliberately illegal: a task becomes blocked
        // when the scheduler re-evaluates a candidate it has already queued,
        // never by skipping the queue (`tasks::run_state`'s own header).
        RunState::Blocked => &[RunState::Queued, RunState::Blocked],
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
                        resume: None,
                        cancel: CancelSignal::new(),
                        in_flight: None,
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
    // Seam-contract D9's 2026-09-03 amendment. Task 009 landed this on
    // `failed`, because nothing resumed a `waiting_retry` task and a card
    // sitting there would have been invisible to the morning review. Task 014
    // resumes them, so ADR-0010:57-59's "offered for resume" is now what
    // happens — and the *word* is still read off the run's `exit_class`, which
    // is the half of D9 that did not change.
    assert_eq!(detail.task.run_state, RunState::WaitingRetry);
    assert_eq!(
        closed.resume_after,
        Some(fixture.harness.clock.now()),
        "ADR-0011 resumes an interruption once, immediately",
    );
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

#[tokio::test]
async fn a_launch_offers_a_crashed_run_for_resume_and_starts_nothing_until_the_queue_is_started() {
    // ADR-0010:57-59 and ADR-0011's startup reconciliation both say a run left
    // `running` by a crash is **offered** for resume. Offered, not performed —
    // and three independent things make the second half true, which is why all
    // three are asserted rather than only the outcome:
    //
    //   1. `QueueState`'s `Default` is `Paused`.
    //   2. `from_stored` falls back to `Paused` for anything unrecognised.
    //   3. Seam-contract D15 has the exit path write `paused` on the way out.
    //
    // Any one of them alone would be an accident. Together they are why a
    // laptop that force-quit at midnight does not start spending money at 03:00
    // when the app is reopened.
    let fixture = Fixture::new().await;
    let crashed = fixture.add_task("Alpha").await;
    scheduler::claim(fixture.ctx(), &crashed)
        .await
        .expect("claim the task the crash caught");
    start_run(
        fixture.ctx(),
        &fixture.paths,
        NewRun {
            task_id: crashed.clone(),
            session_id: SESSION.to_string(),
            prompt: "implement the plan".to_string(),
            // Task 011's column. These runs stand in for attempts a crash
            // caught, so the base they were built on is not what this test
            // is about.
            base_ref: None,
        },
    )
    .await
    .expect("open the run the crash interrupted");

    // (3) The exit path's write, replayed: this is the state a previous launch
    // left behind, whatever it had been doing.
    scheduler::set_queue_state(fixture.ctx(), QueueState::Paused)
        .await
        .expect("quitting always stops the queue");

    let report = startup::survey(&fixture.ctx().pool)
        .await
        .expect("survey the database");
    scheduler::reconcile_interrupted(fixture.ctx(), &report)
        .await
        .expect("reconcile");

    // Offered: the task is waiting, with a deadline that has already arrived.
    let detail = fixture.detail(&crashed).await;
    assert_eq!(detail.task.run_state, RunState::WaitingRetry);
    let due = detail
        .last_run
        .expect("the interrupted run")
        .resume_after
        .expect("an interruption is resumed once, immediately");
    assert!(due <= fixture.harness.clock.now(), "{due} is not yet due");
    assert_eq!(
        scheduler::plan(fixture.ctx())
            .await
            .expect("read the plan")
            .iter()
            .find(|entry| entry.task_id == crashed)
            .and_then(|entry| entry.skip),
        None,
        "a due task is claimable, which is what `offered` means",
    );

    // Not performed. The loop is running and looking at a claimable task, and
    // it starts nothing, because (1) and (2) put the switch at `paused`.
    let queue = fixture.spawn_queue();
    converge().await;
    assert_eq!(
        fixture.cli.started(),
        Vec::<String>::new(),
        "a launch must not resume anything before a human asks",
    );
    assert_eq!(
        queue.status().await.expect("read the status").state,
        QueueState::Paused,
    );
    assert_eq!(QueueState::default(), QueueState::Paused);

    // And then it is, the moment they do.
    let mut changes = fixture.ctx().subscribe();
    queue.start().await.expect("the human presses Start");
    let resumed = crashed.clone();
    wait_until(
        &fixture,
        &mut changes,
        "the offered task to be picked up",
        move |board| {
            board
                .iter()
                .any(|task| task.task.id == resumed && task.task.column == BoardColumn::InReview)
        },
    )
    .await;

    assert_eq!(fixture.cli.started(), vec![crashed.clone()]);
    assert!(
        fixture.argv(&crashed, 1).contains(&"--resume".to_string()),
        "the offer was a resume, not a restart",
    );

    queue.shutdown();
}

// ---------------------------------------------------------------------------
// Hitting the wall, and coming back (task 014; ADR-0011)
//
// Every test here drives the retry loop with the injected `TestClock`. Nothing
// sleeps: `Clock::sleep_until` resolves when the test moves the clock, so a
// five-hour usage window and a fifteen-minute backoff both cost microseconds.
// The reset time in `usage-limit.jsonl` is pinned at 2026-08-20T07:00:00Z, five
// hours after `test_epoch` — deliberately, because the five-hour window is the
// wall this whole task exists for.
// ---------------------------------------------------------------------------

/// What `usage-limit.jsonl`'s `resetsAt` decodes to.
const REPORTED_RESET: &str = "2026-08-20T07:00:00Z";

/// A session id for a `runs` row a test writes by hand.
const SESSION: &str = "0b6d3e2e-0000-4000-8000-00000000c0de";

#[tokio::test]
async fn a_usage_limit_schedules_a_resume_at_the_reported_reset_and_completes_when_it_fires() {
    // Task 014's first acceptance criterion, and the product's whole premise:
    // the night does not end at the first wall.
    let fixture = Fixture::new().await;
    let task_id = fixture.add_task("Alpha").await;
    fixture
        .cli
        .replays_on_attempt(&task_id, 1, "usage-limit", 143);

    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue();
    queue.start().await.expect("start the queue");

    let waiting = task_id.clone();
    wait_until(
        &fixture,
        &mut changes,
        "the wall to be recorded",
        move |board| {
            board.iter().any(|task| {
                task.task.id == waiting && task.task.run_state == RunState::WaitingRetry
            })
        },
    )
    .await;

    // The deadline is the reported reset plus at most a minute of jitter — two
    // tasks hitting one wall must not stampede the reset together.
    let reset: DateTime<Utc> = REPORTED_RESET.parse().expect("a literal timestamp");
    let due = fixture
        .detail(&task_id)
        .await
        .last_run
        .expect("the attempt that hit the wall")
        .resume_after
        .expect("a usage limit is always retried");
    assert!(
        due >= reset && due <= reset + TimeDelta::minutes(1),
        "{due} is not the reported reset plus a jitter",
    );

    // And nothing has happened in the meantime. The queue is holding, not
    // spinning: it woke on the cap, re-read, and went back to waiting.
    converge().await;
    assert_eq!(fixture.cli.attempts(&task_id), 1);

    // Five hours pass in one statement.
    fixture.harness.clock.set(reset + TimeDelta::minutes(5));

    let finished = task_id.clone();
    wait_until(
        &fixture,
        &mut changes,
        "the resumed run to finish",
        move |board| {
            board
                .iter()
                .any(|task| task.task.id == finished && task.task.column == BoardColumn::InReview)
        },
    )
    .await;

    assert_eq!(fixture.cli.attempts(&task_id), 2);
    assert_eq!(fixture.task(&task_id).await.run_state, RunState::Idle);

    queue.shutdown();
}

#[tokio::test]
async fn a_usage_limit_with_no_reported_reset_retries_on_the_fixed_poll() {
    // ADR-0011's fallback, and `spike/FINDINGS.md` §4's named gap: when the CLI
    // says a window is closed but not when it reopens, the answer is a fixed
    // fifteen-minute poll rather than giving up or guessing.
    let fixture = Fixture::new().await;
    let task_id = fixture.add_task("Alpha").await;
    fixture
        .cli
        .replays_on_attempt(&task_id, 1, "usage-limit-no-reset", 143);

    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue();
    queue.start().await.expect("start the queue");

    let waiting = task_id.clone();
    wait_until(
        &fixture,
        &mut changes,
        "the wall to be recorded",
        move |board| {
            board.iter().any(|task| {
                task.task.id == waiting && task.task.run_state == RunState::WaitingRetry
            })
        },
    )
    .await;

    let started_at = rimaia_core::testing::test_epoch();
    let due = fixture
        .detail(&task_id)
        .await
        .last_run
        .expect("the attempt that hit the wall")
        .resume_after
        .expect("a usage limit is always retried");
    assert!(
        due >= started_at + TimeDelta::minutes(15) && due <= started_at + TimeDelta::minutes(16),
        "{due} is not fifteen minutes after the run, plus a jitter",
    );

    fixture.harness.clock.advance(TimeDelta::minutes(20));

    let finished = task_id.clone();
    wait_until(
        &fixture,
        &mut changes,
        "the polled retry to finish",
        move |board| {
            board
                .iter()
                .any(|task| task.task.id == finished && task.task.column == BoardColumn::InReview)
        },
    )
    .await;

    assert_eq!(fixture.cli.attempts(&task_id), 2);

    queue.shutdown();
}

#[tokio::test]
async fn every_attempt_is_its_own_runs_row_sharing_one_session_id() {
    // ADR-0011: "each attempt is a row in `runs`, sharing the task's session
    // id, so the history of an overnight task reads as the sequence of walls it
    // hit". It is also the boundary `scheduler::attempts` counts a retry budget
    // against, which is why the sharing is asserted and not assumed.
    let fixture = Fixture::new().await;
    let task_id = fixture.add_task("Alpha").await;
    fixture
        .cli
        .replays_on_attempt(&task_id, 1, "usage-limit", 143);

    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue();
    queue.start().await.expect("start the queue");
    wait_until_waiting(&fixture, &mut changes, &task_id).await;
    fixture
        .harness
        .clock
        .set(REPORTED_RESET.parse::<DateTime<Utc>>().unwrap() + TimeDelta::minutes(5));
    wait_until_in_review(&fixture, &mut changes, &task_id).await;

    let runs = fixture.runs(&task_id).await;
    assert_eq!(runs.len(), 2, "one row per attempt, not one per task");
    assert_eq!(
        runs.iter().map(|run| run.attempt).collect::<Vec<_>>(),
        vec![2, 1],
        "newest first, and monotonic",
    );
    assert_eq!(
        runs[0].session_id, runs[1].session_id,
        "both attempts continue one session",
    );
    assert_eq!(runs[1].exit_class, Some(ExitClass::UsageLimit));
    assert_eq!(runs[0].exit_class, Some(ExitClass::Success));
    assert!(
        runs[1].resume_after.is_some() && runs[0].resume_after.is_none(),
        "the wall scheduled a retry; the success that followed did not",
    );

    queue.shutdown();
}

#[tokio::test]
async fn a_resumed_attempt_is_spawned_with_resume_and_the_session_of_the_attempt_it_continues() {
    // `spike/FINDINGS.md` §6: "`--session-id` on the first run and `--resume` on
    // the retry is the right shape". They are alternatives, not companions, and
    // getting this wrong opens a *new* session under an old name — which looks
    // fine and throws away every token the first attempt spent.
    let fixture = Fixture::new().await;
    let task_id = fixture.add_task("Alpha").await;
    fixture
        .cli
        .replays_on_attempt(&task_id, 1, "usage-limit", 143);

    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue();
    queue.start().await.expect("start the queue");
    wait_until_waiting(&fixture, &mut changes, &task_id).await;
    fixture
        .harness
        .clock
        .set(REPORTED_RESET.parse::<DateTime<Utc>>().unwrap() + TimeDelta::minutes(5));
    wait_until_in_review(&fixture, &mut changes, &task_id).await;

    let session = fixture.runs(&task_id).await[0].session_id.clone();
    let first = fixture.argv(&task_id, 1);
    let second = fixture.argv(&task_id, 2);

    assert_eq!(value_after(&first, "--session-id"), Some(session.clone()));
    assert!(
        !first.iter().any(|arg| arg == "--resume"),
        "the first attempt opens the session; it does not continue one",
    );
    assert_eq!(value_after(&second, "--resume"), Some(session));
    assert!(
        !second.iter().any(|arg| arg == "--session-id"),
        "the two flags are alternatives, never companions",
    );

    queue.shutdown();
}

#[tokio::test]
async fn a_resumed_attempt_is_sent_the_continuation_prompt_and_not_the_composed_one() {
    // ADR-0009 stores what was *sent*, verbatim, and ADR-0011 says a retry
    // sends "a short continuation prompt". So a morning reviewer reading four
    // rows sees one long prompt and three one-liners: the sequence of walls the
    // task hit. Asserted as an exact string, per CLAUDE.md's rule for prompt
    // composition — a substring check would pass for a prompt with the composed
    // one glued onto it, which is the mistake that costs the tokens.
    let fixture = Fixture::new().await;
    let task_id = fixture.add_task("Alpha").await;
    fixture
        .cli
        .replays_on_attempt(&task_id, 1, "usage-limit", 143);

    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue();
    queue.start().await.expect("start the queue");
    wait_until_waiting(&fixture, &mut changes, &task_id).await;
    fixture
        .harness
        .clock
        .set(REPORTED_RESET.parse::<DateTime<Utc>>().unwrap() + TimeDelta::minutes(5));
    wait_until_in_review(&fixture, &mut changes, &task_id).await;

    let expected = compose_resume_prompt(&fixture.detail(&task_id).await);
    assert_eq!(fixture.cli.stdin(&task_id, 2), expected);
    // And the row records the same string, because that is what ADR-0009's
    // stored copy is *for*.
    let runs = fixture.runs(&task_id).await;
    assert_eq!(runs[0].prompt, expected);
    assert_ne!(
        runs[1].prompt, expected,
        "the first attempt was sent the whole composed prompt",
    );
    assert!(runs[1].prompt.contains("# Plan"));

    queue.shutdown();
}

#[tokio::test]
async fn a_resumed_run_continues_in_the_same_worktree_with_its_earlier_commits_intact() {
    // ADR-0011: "work already committed stays". Verified against real git in a
    // real worktree, because a mocked git only proves the mock works — the
    // stand-in makes an actual commit on each attempt and this reads the
    // history back out afterwards.
    let fixture = Fixture::new().await;
    let task_id = fixture.add_task("Alpha").await;
    fixture
        .cli
        .commits_on_attempt(&task_id, 1, "step one", "usage-limit", 143);
    fixture
        .cli
        .commits_on_attempt(&task_id, 2, "step two", "success", 0);

    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue();
    queue.start().await.expect("start the queue");
    wait_until_waiting(&fixture, &mut changes, &task_id).await;

    let after_the_wall = fixture
        .task(&task_id)
        .await
        .worktree_path
        .expect("a run has a worktree");
    assert_eq!(
        git_log(&after_the_wall),
        vec!["step one".to_string()],
        "the first attempt's commit is in the tree",
    );

    fixture
        .harness
        .clock
        .set(REPORTED_RESET.parse::<DateTime<Utc>>().unwrap() + TimeDelta::minutes(5));
    wait_until_in_review(&fixture, &mut changes, &task_id).await;

    let after_the_resume = fixture
        .task(&task_id)
        .await
        .worktree_path
        .expect("a resumed run has the same worktree");
    assert_eq!(
        after_the_resume, after_the_wall,
        "a retry resumes; it does not get a fresh tree",
    );
    assert_eq!(
        git_log(&after_the_resume),
        vec!["step two".to_string(), "step one".to_string()],
        "the second attempt built on the first rather than replacing it",
    );

    queue.shutdown();
}

#[tokio::test]
async fn a_fatal_run_is_not_retried() {
    // ADR-0011's fatal row. `max-turns.jsonl` is the one recorded fatal
    // scenario, and the property is that nothing is scheduled at all — not a
    // long wait, not a wait the queue then declines to act on.
    let fixture = Fixture::new().await;
    let task_id = fixture.add_task("Alpha").await;
    fixture.cli.replays(&task_id, "max-turns", 1);

    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue();
    queue.start().await.expect("start the queue");

    let failing = task_id.clone();
    wait_until(
        &fixture,
        &mut changes,
        "the fatal run to settle",
        move |board| {
            board
                .iter()
                .any(|task| task.task.id == failing && task.task.run_state == RunState::Failed)
        },
    )
    .await;

    // Whatever the clock does, nothing comes back for it.
    fixture.harness.clock.advance(TimeDelta::hours(6));
    converge().await;

    assert_eq!(fixture.cli.attempts(&task_id), 1);
    let run = fixture
        .detail(&task_id)
        .await
        .last_run
        .expect("the fatal run");
    assert_eq!(run.exit_class, Some(ExitClass::Fatal));
    assert_eq!(run.resume_after, None);
    assert_eq!(fixture.task(&task_id).await.column, BoardColumn::Ready);

    queue.shutdown();
}

#[tokio::test]
async fn a_waiting_retry_task_releases_its_slot_so_another_task_runs_while_it_waits() {
    // ADR-0011: "the scheduler does not block on a waiting task. A task in
    // `waiting_retry` releases its concurrency slot; the queue continues with
    // other tasks and comes back to it."
    //
    // Two repositories and parallel mode, because the *sequential* answer here
    // is the opposite one and equally correct: a usage-limit wall pauses
    // everything, since the next task would hit the same wall. So this uses a
    // `transient` failure, which schedules a wait without raising the global
    // hold — which is precisely the case the sentence above is about.
    let mut fixture = Fixture::new().await;
    let other_repository = fixture.register_repository(true).await;
    let waiting = fixture.add_task("Waits").await;
    let other = fixture.add_task_in(&other_repository, "Carries on").await;
    // A stream that never reaches a `result` is ADR-0011's "empty stream", and
    // its class is `transient`.
    fixture
        .cli
        .replays_on_attempt(&waiting, 1, "truncated-stream", 1);
    fixture.set_parallel(1).await;

    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue();
    queue.start().await.expect("start the queue");

    // The wall first, so what follows is genuinely "while it waits" and not
    // "before it started" — with one slot, board order alone decides which of
    // the two runs first, and the assertion has to be about the slot.
    wait_until_waiting(&fixture, &mut changes, &waiting).await;

    // The second task then finishes while the first is waiting out its minute,
    // on a queue with exactly one slot. If the slot were still held by the
    // waiting task, this would hang.
    wait_until_in_review(&fixture, &mut changes, &other).await;
    assert_eq!(
        fixture.task(&waiting).await.run_state,
        RunState::WaitingRetry,
    );
    assert_eq!(
        scheduler::plan(fixture.ctx())
            .await
            .expect("read the plan")
            .iter()
            .find(|entry| entry.task_id == waiting)
            .and_then(|entry| entry.skip),
        Some(SkipReason::WaitingForRetry),
        "and the card says it is coming back, not that it is stuck",
    );

    // ADR-0011's first transient step is one minute.
    fixture.harness.clock.advance(TimeDelta::minutes(2));
    wait_until_in_review(&fixture, &mut changes, &waiting).await;
    assert_eq!(fixture.cli.attempts(&waiting), 2);

    queue.shutdown();
}

#[tokio::test]
async fn a_usage_limit_pauses_new_starts_globally_in_sequential_mode() {
    let fixture = Fixture::new().await;
    assert_a_wall_holds_every_other_start(fixture).await;
}

#[tokio::test]
async fn a_usage_limit_pauses_new_starts_globally_in_parallel_mode() {
    // "In both modes" is ADR-0011's own phrasing, and the reason the check sits
    // in `try_step` *before* the plan is read: neither mode has a branch for it.
    let fixture = Fixture::new().await;
    fixture.set_parallel(4).await;
    assert_a_wall_holds_every_other_start(fixture).await;
}

/// The shared body of the two tests above: a task in a *different* repository,
/// with slots to spare, must not be started into a window that is closed.
///
/// A second repository matters — with both tasks in one, the per-repository cap
/// of one would hold the second back anyway and the test would pass without the
/// global hold existing at all.
async fn assert_a_wall_holds_every_other_start(mut fixture: Fixture) {
    let other_repository = fixture.register_repository(true).await;
    let limited = fixture.add_task("Hits the wall").await;
    fixture
        .cli
        .replays_on_attempt(&limited, 1, "usage-limit", 143);

    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue();
    queue.start().await.expect("start the queue");
    wait_until_waiting(&fixture, &mut changes, &limited).await;

    // Added *after* the wall, deliberately. A task already on the board when
    // the queue started would be picked up in the same batch as the limited one
    // — correctly, since nothing knew about a wall yet — and the test would be
    // asserting a race rather than the hold. What ADR-0011 forbids is starting
    // into a window already known to be closed, which is exactly this.
    let held = fixture
        .add_task_in(&other_repository, "Would burn a start")
        .await;
    converge().await;

    assert_eq!(
        fixture.cli.started(),
        vec![limited.clone()],
        "starting a fresh task into a limited window just burns a start",
    );
    assert_eq!(fixture.task(&held).await.run_state, RunState::Idle);
    let reset: DateTime<Utc> = REPORTED_RESET.parse().expect("a literal timestamp");
    let until = queue
        .status()
        .await
        .expect("read the status")
        .usage_limit_pause_until
        .expect("the hold is on the status the Runs view reads");
    assert!(until >= reset, "the hold lasts until the window reopens");

    // And it lifts on its own — nothing has to remember to clear it.
    fixture.harness.clock.set(reset + TimeDelta::minutes(5));
    wait_until_in_review(&fixture, &mut changes, &held).await;
    assert_eq!(
        queue
            .status()
            .await
            .expect("read the status")
            .usage_limit_pause_until,
        None,
    );

    queue.shutdown();
}

#[tokio::test]
async fn retry_now_starts_a_waiting_task_before_its_deadline() {
    // The operator's override. This drives the two core calls the
    // `retry_task_now` command makes — `claim_retry` and `resumable_session` —
    // for the same reason `a_starter_that_claims_before_it_spawns_never_produces_a_second_process`
    // does: `src-tauri` has no test harness of its own, so the closest thing to
    // a regression test for the button is the core pair it is thin over.
    let fixture = Fixture::new().await;
    let task_id = fixture.add_task("Alpha").await;
    fixture
        .cli
        .replays_on_attempt(&task_id, 1, "usage-limit", 143);

    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue();
    queue.start().await.expect("start the queue");
    wait_until_waiting(&fixture, &mut changes, &task_id).await;
    queue
        .pause()
        .await
        .expect("the queue is not the starter here");

    // Five hours before the deadline, and deliberately so.
    let due = fixture
        .detail(&task_id)
        .await
        .last_run
        .expect("the attempt that hit the wall")
        .resume_after
        .expect("a scheduled resume");
    assert!(due > fixture.harness.clock.now());

    assert_eq!(
        scheduler::claim_retry(fixture.ctx(), &task_id)
            .await
            .expect("claim the waiting task"),
        ClaimOutcome::Claimed,
    );
    let session = scheduler::resumable_session(fixture.ctx(), &task_id)
        .await
        .expect("read the session")
        .expect("a task with attempts has one");
    run_task(
        fixture.ctx(),
        &fixture.paths,
        &fixture.runner(),
        RunRequest {
            // `Queued`, not `RunRequest::resuming`'s `Manual`: every recording
            // in the corpus was captured under `bypassPermissions`, and the
            // runner verifies the mode `init` echoes back against the one it
            // asked for. See this file's header.
            trigger: RunTrigger::Queued,
            ..RunRequest::resuming(&task_id, &session)
        },
    )
    .await
    .expect("the resumed run completes");

    assert_eq!(fixture.cli.attempts(&task_id), 2);
    assert_eq!(fixture.task(&task_id).await.column, BoardColumn::InReview);
    assert_eq!(
        value_after(&fixture.argv(&task_id, 2), "--resume"),
        Some(session),
        "retry now resumes; it does not restart",
    );

    queue.shutdown();
}

#[tokio::test]
async fn giving_up_lands_a_waiting_task_in_failed() {
    // The other half of the manual pair, and the one that ends the loop. The
    // refusals matter as much as the transition: "give up" on a task that is
    // not waiting is a sentence about the card, not a state-machine error.
    let fixture = Fixture::new().await;
    let task_id = fixture.add_task("Alpha").await;
    fixture
        .cli
        .replays_on_attempt(&task_id, 1, "usage-limit", 143);

    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue();
    queue.start().await.expect("start the queue");
    wait_until_waiting(&fixture, &mut changes, &task_id).await;
    queue.pause().await.expect("pause the queue");

    scheduler::give_up(fixture.ctx(), &task_id)
        .await
        .expect("the operator has read the error");

    assert_eq!(fixture.task(&task_id).await.run_state, RunState::Failed);
    assert_eq!(
        fixture.task(&task_id).await.column,
        BoardColumn::Ready,
        "a failure stays where the morning review trips over it",
    );

    // The deadline is still on the row — history, not a promise — and the queue
    // does not act on it, because the task is no longer waiting.
    assert!(fixture
        .detail(&task_id)
        .await
        .last_run
        .expect("the attempt that hit the wall")
        .resume_after
        .is_some());
    queue.start().await.expect("resume the queue");
    fixture.harness.clock.advance(TimeDelta::hours(6));
    converge().await;
    assert_eq!(fixture.cli.attempts(&task_id), 1);

    // And giving up twice is a sentence, not a panic.
    let error = scheduler::give_up(fixture.ctx(), &task_id)
        .await
        .expect_err("there is nothing left to give up on");
    assert!(
        error.to_string().contains("failed"),
        "the refusal names the state it found: {error}"
    );

    queue.shutdown();
}

/// Resolves once `task_id` is waiting out a retry.
async fn wait_until_waiting(fixture: &Fixture, changes: &mut Receiver<ChangeEvent>, task_id: &str) {
    let waiting = task_id.to_string();
    wait_until(fixture, changes, "the task to be waiting", move |board| {
        board
            .iter()
            .any(|task| task.task.id == waiting && task.task.run_state == RunState::WaitingRetry)
    })
    .await;
}

/// Resolves once `task_id` has reached the morning review.
async fn wait_until_in_review(
    fixture: &Fixture,
    changes: &mut Receiver<ChangeEvent>,
    task_id: &str,
) {
    let finished = task_id.to_string();
    wait_until(
        fixture,
        changes,
        "the task to reach in_review",
        move |board| {
            board
                .iter()
                .any(|task| task.task.id == finished && task.task.column == BoardColumn::InReview)
        },
    )
    .await;
}

/// The value of the argument after `flag`, or `None` when the flag is absent.
fn value_after(argv: &[String], flag: &str) -> Option<String> {
    argv.iter()
        .position(|arg| arg == flag)
        .and_then(|index| argv.get(index + 1))
        .cloned()
}

/// Every commit subject in `worktree`, newest first — real `git` against the
/// real tree the run was given (ADR-0005), never a stand-in.
fn git_log(worktree: &str) -> Vec<String> {
    let output = std::process::Command::new("git")
        .args(["log", "--format=%s"])
        .current_dir(worktree)
        .output()
        .expect("git must be installed");
    assert!(
        output.status.success(),
        "git log failed in {worktree}: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        // `TempRepo::init` seeds the repository with a first commit, which every
        // worktree inherits and which is not this task's work.
        .filter(|subject| subject != "Initial commit")
        .collect()
}

// ---------------------------------------------------------------------------
// Starting by itself, and stopping by itself (task 013; ADR-0010)
//
// Every test here drives the schedule timer with the injected `TestClock`.
// Nothing sleeps: the loop waits on `Clock::sleep_until`, so a nightly schedule
// two minutes out and a window that closes eight hours later both cost
// microseconds.
//
// The clock starts at `test_epoch` — 2026-08-20T02:00:00Z, which is 04:00 on a
// Thursday morning in Europe/Copenhagen, summer time. Every local time below is
// stated in that zone.
// ---------------------------------------------------------------------------

/// The zone every schedule in this section is read in.
const ZONE: &str = "Europe/Copenhagen";

/// 22:00 every night, which is the schedule this whole task is named for.
const NIGHTLY: &str = "0 22 * * *";

#[tokio::test]
async fn a_schedule_due_two_minutes_from_now_starts_the_queue_at_that_time() {
    // Task 013's first acceptance criterion, end to end: nobody presses
    // anything, and a card is in `in_review` in the morning.
    let fixture = Fixture::new().await;
    let task_id = fixture.add_task("Alpha").await;
    let due = fixture.harness.clock.now() + TimeDelta::minutes(2);
    let schedule = fixture.add_schedule(once_at("Tonight", due)).await;

    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue();

    // Nothing has happened yet, and that is half the assertion: the queue does
    // not start early just because a schedule exists.
    converge().await;
    assert_eq!(fixture.queue_state().await, QueueState::Paused);
    assert_eq!(fixture.cli.started(), Vec::<String>::new());

    fixture.harness.clock.advance(TimeDelta::minutes(2));

    let started = task_id.clone();
    wait_until(
        &fixture,
        &mut changes,
        "the scheduled task to finish",
        move |board| {
            board
                .iter()
                .any(|task| task.task.id == started && task.task.column == BoardColumn::InReview)
        },
    )
    .await;

    assert_eq!(fixture.cli.started(), vec![task_id]);
    assert_eq!(fixture.queue_state().await, QueueState::Running);
    let window = fixture.window().await.expect("a schedule opened a window");
    assert_eq!(window.schedule_id, schedule);
    assert_eq!(window.schedule_name, "Tonight");

    queue.shutdown();
}

#[tokio::test]
async fn a_schedule_whose_time_passed_while_the_app_was_closed_fires_once_on_next_launch() {
    // ADR-0010: "A wall-clock time in the past fires immediately rather than
    // being skipped; the machine having been asleep is the common case." The
    // launch is `spawn_queue`, which is what a launch is in this file.
    let fixture = Fixture::new().await;
    let task_id = fixture.add_task("Alpha").await;
    let missed = fixture.harness.clock.now() - TimeDelta::hours(3);
    let schedule = fixture.add_schedule(once_at("Last night", missed)).await;

    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue();

    let started = task_id.clone();
    wait_until(
        &fixture,
        &mut changes,
        "the missed schedule to fire",
        move |board| {
            board
                .iter()
                .any(|task| task.task.id == started && task.task.column == BoardColumn::InReview)
        },
    )
    .await;

    assert_eq!(fixture.cli.started(), vec![task_id]);
    assert_eq!(
        fixture.schedule(&schedule).await.last_fired_at,
        Some(fixture.harness.clock.now()),
        "`last_fired_at` is when it actually fired, not when it was due",
    );

    // Once, not repeatedly: the column that records the fire is what stops the
    // same overdue occurrence being honoured again on the very next pass.
    let opened_at = fixture.window().await.expect("a window").opened_at;
    fixture.harness.clock.advance(TimeDelta::minutes(5));
    converge().await;
    assert_eq!(
        fixture
            .window()
            .await
            .expect("still the same window")
            .opened_at,
        opened_at
    );

    queue.shutdown();
}

#[tokio::test]
async fn a_schedule_that_missed_five_occurrences_fires_once_not_five_times() {
    // Coalescing, and the half of it that matters: the window that opens is the
    // **newest** missed occurrence's. Honouring the oldest would open a window
    // whose stop time was five mornings ago, which is a night that never runs.
    let fixture = Fixture::new().await;
    fixture.add_task("Alpha").await;
    // Armed six days back, so five nights at 22:00 have come and gone. The
    // clock is 04:00 on the sixth morning, before this morning's 06:00 stop, so
    // last night's window is still open.
    let schedule = fixture
        .add_schedule(nightly_at("Nightly", Some("06:00")))
        .await;
    fixture
        .arm_schedule(&schedule, fixture.harness.clock.now() - TimeDelta::days(6))
        .await;

    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue();

    wait_until(
        &fixture,
        &mut changes,
        "the queue to be started by the schedule",
        |_| true,
    )
    .await;
    wait_until_running(&fixture).await;

    let window = fixture.window().await.expect("one window");
    assert_eq!(
        window.closes_at,
        Some(at("2026-08-20T04:00:00Z")),
        "06:00 Copenhagen *this* morning — the newest missed night, not the oldest",
    );
    assert_eq!(
        fixture.schedule(&schedule).await.last_fired_at,
        Some(fixture.harness.clock.now()),
    );

    // And the other four do not follow it in.
    converge().await;
    assert_eq!(
        fixture.window().await.expect("still one window").opened_at,
        window.opened_at
    );

    queue.shutdown();
}

#[tokio::test]
async fn a_window_whose_stop_time_already_passed_does_not_open() {
    // The bound on late firing. A laptop opened at 11:00 has genuinely missed
    // the 22:00-to-06:00 night; starting a full night's work in the middle of a
    // working morning is not what "fires late rather than skipping" means.
    let fixture = Fixture::new().await;
    fixture.add_task("Alpha").await;
    let schedule = fixture
        .add_schedule(nightly_at("Nightly", Some("06:00")))
        .await;
    fixture
        .arm_schedule(&schedule, fixture.harness.clock.now() - TimeDelta::days(2))
        .await;
    // 11:00 Copenhagen, five hours after the window this occurrence would have
    // opened was due to close.
    fixture.harness.clock.set(at("2026-08-20T09:00:00Z"));

    let queue = fixture.spawn_queue();
    converge().await;

    assert_eq!(fixture.queue_state().await, QueueState::Paused);
    assert_eq!(fixture.window().await, None);
    assert_eq!(fixture.cli.started(), Vec::<String>::new());
    assert_eq!(
        fixture.schedule(&schedule).await.last_fired_at,
        None,
        "nothing fired, so nothing may claim it did — `last_fired_at` is not a lie \
         told to stop a recomputation",
    );

    queue.shutdown();
}

#[tokio::test]
async fn a_stop_time_starts_nothing_new_and_lets_the_in_flight_run_finish() {
    // Task 013's third acceptance criterion, and ADR-0010's own sentence:
    // "Reaching it stops *starting* new tasks; in-flight runs are allowed to
    // finish rather than being killed mid-edit."
    let fixture = Fixture::new().await;
    let first = fixture.add_task("Alpha").await;
    let second = fixture.add_task("Bravo").await;
    let gate = fixture.cli.gates(&first, "success", HEAD_LINES);
    // Opens now (04:00 Copenhagen) and stops at 05:00, an hour out.
    let now = fixture.harness.clock.now();
    let schedule = ScheduleInput {
        stop_at: Some("05:00".to_string()),
        ..once_at("Tonight", now)
    };
    fixture.add_schedule(schedule).await;

    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue();

    wait_until_started(&fixture, 1).await;
    assert_eq!(fixture.cli.started(), vec![first.clone()]);

    // The stop time arrives while Alpha is still mid-run.
    fixture.harness.clock.advance(TimeDelta::hours(1));
    // Waiting on the *window*, not on the switch: `close_window` writes
    // `paused` first and clears the window second — deliberately, so a crash
    // between the two leaves a paused queue with a stale window rather than a
    // running queue with none — so the switch alone is reachable one write
    // early.
    wait_until_window_closed(&fixture).await;
    assert_eq!(fixture.queue_state().await, QueueState::Paused);

    // Alpha was not killed: it finishes, and lands in review.
    open(&gate);
    let finished = first.clone();
    wait_until(
        &fixture,
        &mut changes,
        "the in-flight run to finish",
        move |board| {
            board
                .iter()
                .any(|task| task.task.id == finished && task.task.column == BoardColumn::InReview)
        },
    )
    .await;

    // And Bravo never started.
    converge().await;
    assert_eq!(fixture.cli.started(), vec![first]);
    assert_eq!(fixture.task(&second).await.run_state, RunState::Idle);
    assert_eq!(fixture.task(&second).await.column, BoardColumn::Ready);

    queue.shutdown();
}

#[tokio::test]
async fn disabling_a_schedule_stops_it_firing_without_deleting_it() {
    // Task 013's fifth acceptance criterion, both halves: it does not fire, and
    // its configuration is still there to be switched back on.
    let fixture = Fixture::new().await;
    let task_id = fixture.add_task("Alpha").await;
    let due = fixture.harness.clock.now() - TimeDelta::minutes(5);
    let schedule = fixture
        .add_schedule(ScheduleInput {
            enabled: false,
            ..once_at("Tonight", due)
        })
        .await;

    let queue = fixture.spawn_queue();
    converge().await;

    assert_eq!(fixture.queue_state().await, QueueState::Paused);
    assert_eq!(fixture.cli.started(), Vec::<String>::new());
    let off = fixture.schedule(&schedule).await;
    assert_eq!(
        off.start_at,
        Some(due),
        "disabling keeps the configuration — that is the whole difference from deleting",
    );

    // And switching it back on is all it takes.
    let mut changes = fixture.ctx().subscribe();
    scheduler_schedule::set_enabled(fixture.ctx(), &schedule, true)
        .await
        .expect("turn it back on");

    let started = task_id.clone();
    wait_until(
        &fixture,
        &mut changes,
        "the re-enabled schedule to fire",
        move |board| {
            board
                .iter()
                .any(|task| task.task.id == started && task.task.column == BoardColumn::InReview)
        },
    )
    .await;
    assert_eq!(fixture.cli.started(), vec![task_id]);

    queue.shutdown();
}

#[tokio::test]
async fn a_blocking_doctor_report_stops_a_scheduled_start_and_says_why() {
    // Task 018's own sentence, and the reason its doctor is on this path: "a
    // broken environment is reported in the evening rather than discovered in
    // the morning". A scheduled start is the one nobody is watching.
    let fixture = Fixture::new().await;
    fixture.add_task("Alpha").await;
    let due = fixture.harness.clock.now();
    let schedule = fixture.add_schedule(once_at("Tonight", due)).await;

    // A `claude` that is not there, which is the doctor's one unambiguous
    // `fail`: every run would die at the same place.
    let queue = fixture.spawn_queue_with_runner(RunnerConfig {
        program: fixture.paths.data_dir().join("no-such-claude"),
        ..RunnerConfig::default()
    });
    wait_until_fired(&fixture, &schedule).await;
    converge().await;

    assert_eq!(
        fixture.queue_state().await,
        QueueState::Paused,
        "a refused start writes no queue state — the switch is untouched",
    );
    assert_eq!(fixture.window().await, None);
    assert_eq!(fixture.cli.started(), Vec::<String>::new());

    let reported = queue
        .status()
        .await
        .expect("read the status")
        .last_step_error
        .expect("the refusal has to be visible, not only logged");
    assert!(
        reported.contains("preflight") && reported.contains("Claude Code"),
        "the refusal names the check and the fix: {reported}",
    );

    // And it still recorded the fire. Without that, the occurrence stays due,
    // the next wake finds it again, and a missing binary becomes eight
    // subprocess spawns a minute until morning.
    assert!(fixture.schedule(&schedule).await.last_fired_at.is_some());

    queue.shutdown();
}

#[tokio::test]
async fn quitting_mid_window_closes_the_window_and_the_next_occurrence_still_fires() {
    // Seam-contract D15's amendment, in one test. Quitting still always stops
    // the queue, and now also closes the window — so relaunching at 03:00 does
    // not silently resume a night the user quit out of. The *schedule* is
    // untouched, because it is a standing instruction rather than queue state.
    let fixture = Fixture::new().await;
    fixture.add_task("Alpha").await;
    let schedule = fixture
        .add_schedule(nightly_at("Nightly", Some("06:00")))
        .await;
    // 21:59 Copenhagen, a minute before tonight's occurrence.
    fixture.harness.clock.set(at("2026-08-20T19:59:00Z"));

    let first_launch = fixture.spawn_queue();
    fixture.harness.clock.advance(TimeDelta::minutes(1));
    wait_until_running(&fixture).await;
    assert!(fixture.window().await.is_some());

    // Quitting: `AppState::cancel_everything` calls this, unconditionally, on
    // every exit — which is what makes D15 hold between runs as well as during
    // one.
    first_launch.stop().await.expect("quit");
    first_launch.shutdown();

    assert_eq!(fixture.queue_state().await, QueueState::Paused);
    assert_eq!(
        fixture.window().await,
        None,
        "a window the user quit out of is not a window a relaunch resumes",
    );
    let after_quitting = fixture.schedule(&schedule).await;
    assert!(
        after_quitting.enabled && after_quitting.cron.is_some(),
        "the standing instruction survives; only the night does not",
    );

    // A second launch at 03:00 starts nothing, and then tomorrow's 22:00 does.
    fixture.harness.clock.set(at("2026-08-21T01:00:00Z"));
    let second_launch = fixture.spawn_queue();
    converge().await;
    assert_eq!(
        fixture.queue_state().await,
        QueueState::Paused,
        "relaunching inside the hours the window covered resumes nothing",
    );

    fixture.harness.clock.set(at("2026-08-21T20:00:00Z"));
    wait_until_running(&fixture).await;
    assert_eq!(
        fixture
            .window()
            .await
            .expect("the next occurrence fires")
            .schedule_id,
        schedule,
    );

    second_launch.shutdown();
}

#[tokio::test]
async fn a_schedule_firing_tonight_does_not_resume_a_run_last_night_crashed_on() {
    // ADR-0010:57-59 says runs left `running` at a crash are "eligible for
    // resume", and task 014 made that real: `reconcile` lands such a run in
    // `waiting_retry` with a deadline that is already due. **Eligible is not
    // automatic**, and this is what that means concretely: a schedule's fire is
    // not itself a resume. Nothing about the timer reaching 22:00 moves a task.
    //
    // What actually resumes the run is ADR-0011's per-run decision, taken by
    // `selection` inside an **open window with the queue running** — so a fire
    // that opens no window resumes nothing, however due the deadline is. That
    // is the half asserted here, because it is the half that would otherwise
    // let a schedule quietly override a per-run policy it knows nothing about.
    let fixture = Fixture::new().await;
    let crashed = fixture.add_task("Alpha").await;
    scheduler::claim(fixture.ctx(), &crashed)
        .await
        .expect("claim the task the crash caught");
    start_run(
        fixture.ctx(),
        &fixture.paths,
        NewRun {
            task_id: crashed.clone(),
            session_id: SESSION.to_string(),
            prompt: "implement the plan".to_string(),
            // Task 011's column. These runs stand in for attempts a crash
            // caught, so the base they were built on is not what this test
            // is about.
            base_ref: None,
        },
    )
    .await
    .expect("open the run the crash interrupted");

    // The launch after the crash: D15's exit-path write, then the repair.
    scheduler::set_queue_state(fixture.ctx(), QueueState::Paused)
        .await
        .expect("quitting always stops the queue");
    let report = startup::survey(&fixture.ctx().pool)
        .await
        .expect("survey the database");
    scheduler::reconcile_interrupted(fixture.ctx(), &report)
        .await
        .expect("reconcile");

    // Offered: the deadline is due, so the *only* thing standing between this
    // task and a resume is the go signal.
    assert_eq!(
        fixture.task(&crashed).await.run_state,
        RunState::WaitingRetry
    );
    assert_eq!(
        scheduler::plan(fixture.ctx())
            .await
            .expect("read the plan")
            .iter()
            .find(|entry| entry.task_id == crashed)
            .and_then(|entry| entry.skip),
        None,
        "due, and therefore claimable the moment something starts the queue",
    );

    // A schedule that comes due into a window that has already closed. It
    // fires in the sense that its occurrence is honoured; it opens nothing.
    let schedule = fixture
        .add_schedule(nightly_at("Nightly", Some("06:00")))
        .await;
    fixture
        .arm_schedule(&schedule, fixture.harness.clock.now() - TimeDelta::days(2))
        .await;
    fixture.harness.clock.set(at("2026-08-20T09:00:00Z"));

    let queue = fixture.spawn_queue();
    converge().await;

    assert_eq!(fixture.window().await, None);
    assert_eq!(fixture.queue_state().await, QueueState::Paused);
    assert_eq!(
        fixture.cli.started(),
        Vec::<String>::new(),
        "the timer reaching a schedule's hour is not a go signal, and a due \
         `resume_after` is not one either",
    );
    assert_eq!(
        fixture.task(&crashed).await.run_state,
        RunState::WaitingRetry,
        "the fire moved no task at all — resume is ADR-0011's per-run policy, \
         and a schedule has no opinion about it",
    );

    queue.shutdown();
}

#[tokio::test]
async fn a_schedule_that_does_open_a_window_resumes_exactly_what_start_would() {
    // The converse of the test above, and the reason it is a pair. A schedule
    // is a standing instruction the user gave in advance (seam-contract D15's
    // amendment), so it must be neither weaker nor stronger than the button:
    // once the window is open and the switch is on, the same per-run policy
    // runs, and the crashed run is resumed exactly as pressing Start resumes
    // it.
    let fixture = Fixture::new().await;
    let crashed = fixture.add_task("Alpha").await;
    scheduler::claim(fixture.ctx(), &crashed)
        .await
        .expect("claim the task the crash caught");
    start_run(
        fixture.ctx(),
        &fixture.paths,
        NewRun {
            task_id: crashed.clone(),
            session_id: SESSION.to_string(),
            prompt: "implement the plan".to_string(),
            // Task 011's column. These runs stand in for attempts a crash
            // caught, so the base they were built on is not what this test
            // is about.
            base_ref: None,
        },
    )
    .await
    .expect("open the run the crash interrupted");
    let report = startup::survey(&fixture.ctx().pool)
        .await
        .expect("survey the database");
    scheduler::reconcile_interrupted(fixture.ctx(), &report)
        .await
        .expect("reconcile");

    let due = fixture.harness.clock.now() + TimeDelta::minutes(2);
    fixture.add_schedule(once_at("Tonight", due)).await;

    let mut changes = fixture.ctx().subscribe();
    let queue = fixture.spawn_queue();
    fixture.harness.clock.advance(TimeDelta::minutes(2));

    let resumed = crashed.clone();
    wait_until(
        &fixture,
        &mut changes,
        "the offered task to be picked up",
        move |board| {
            board
                .iter()
                .any(|task| task.task.id == resumed && task.task.column == BoardColumn::InReview)
        },
    )
    .await;

    assert_eq!(fixture.cli.started(), vec![crashed.clone()]);
    assert!(
        fixture.argv(&crashed, 1).contains(&"--resume".to_string()),
        "a schedule's window resumes, exactly as Start does — not a fresh restart",
    );

    queue.shutdown();
}

/// A one-off schedule at `at`, in Copenhagen, with no stop time.
fn once_at(name: &str, at: DateTime<Utc>) -> ScheduleInput {
    ScheduleInput {
        name: name.to_string(),
        mode: ScheduleMode::Sequential,
        max_concurrency: 2,
        timezone: ZONE.to_string(),
        cron: None,
        start_at: Some(at),
        stop_at: None,
        enabled: true,
    }
}

/// A nightly 22:00 Copenhagen schedule, optionally stopping at `stop_at`.
fn nightly_at(name: &str, stop_at: Option<&str>) -> ScheduleInput {
    ScheduleInput {
        cron: Some(NIGHTLY.to_string()),
        start_at: None,
        stop_at: stop_at.map(str::to_string),
        ..once_at(name, Utc::now())
    }
}

fn at(rfc3339: &str) -> DateTime<Utc> {
    rfc3339.parse().expect("a literal timestamp must parse")
}

/// Resolves once the schedule timer has started the queue.
///
/// Cooperative polling, for the same reason [`wait_until_in_flight`] uses it:
/// `yield_now` costs no real time and guesses no duration.
async fn wait_until_running(fixture: &Fixture) {
    tokio::time::timeout(TEST_TIMEOUT, async {
        while fixture.queue_state().await != QueueState::Running {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the schedule never started the queue");
}

/// Resolves once the open run window has been cleared.
///
/// The window rather than the switch, because `close_window` writes `paused`
/// first and clears the window second — so a test that waited on the switch
/// would see the intermediate state that ordering deliberately produces.
async fn wait_until_window_closed(fixture: &Fixture) {
    tokio::time::timeout(TEST_TIMEOUT, async {
        while fixture.window().await.is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the window never closed");
}

/// Resolves once `schedule_id` has recorded a fire — which a refused start does
/// too, so this is the one wake a doctor-refusal test can wait on.
async fn wait_until_fired(fixture: &Fixture, schedule_id: &str) {
    tokio::time::timeout(TEST_TIMEOUT, async {
        while fixture.schedule(schedule_id).await.last_fired_at.is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the schedule never fired");
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
        while !queue.holds(task_id) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the queue never registered {task_id} as in flight"));
}

/// Resolves once `count` stand-ins have written their own `start` line.
///
/// The processes' own witness, not the rows they wrote: two `runs` rows are
/// both `running` for a while whether or not the two children ever coexisted.
/// Cooperative polling for the same reason [`wait_until_in_flight`] uses it —
/// a stand-in writing a file publishes nothing, so there is no channel to wait
/// on, and `yield_now` costs no real time and guesses no duration.
async fn wait_until_started(fixture: &Fixture, count: usize) {
    tokio::time::timeout(TEST_TIMEOUT, async {
        while fixture.cli.started().len() < count {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "only {} of {count} stand-ins ever started",
            fixture.cli.started().len()
        )
    });
}

/// Yields long enough for a queue that was going to do something to have done
/// it.
///
/// The one shape of waiting this file cannot express as a channel: several
/// tests assert that the queue *did not* start anything, and a negative has no
/// event to wait for. `yield_now` in a bounded loop is not a sleep — it costs
/// no wall-clock time, guesses no duration, and hands the executor every other
/// task including the queue's own loop and the I/O driver. The count is
/// generous rather than tuned, because being wrong in that direction only
/// wastes microseconds while being wrong the other way is a test that passes
/// for the wrong reason.
async fn converge() {
    for _ in 0..2_000 {
        tokio::task::yield_now().await;
    }
}

/// The other side of [`wait_until_in_flight`]: resolves once the queue has
/// released whatever it held, win or lose.
async fn wait_until_not_in_flight(queue: &QueueHandle) {
    tokio::time::timeout(TEST_TIMEOUT, async {
        while !queue.in_flight_task_ids().is_empty() {
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
        // Each fixture-built queue gets its own registry, which is what makes
        // "two launches" two processes rather than one with a shared map.
        self.spawn_queue_with(InFlight::new())
    }

    /// The same, over a registry the test also holds — which is what a test
    /// needs to stand in for the *other* door: a "Run now" the shell started, a
    /// preparation lock held from outside, a Cancel pressed on one card.
    fn spawn_queue_with(&self, in_flight: InFlight) -> QueueHandle {
        let (handle, task) = self.build_queue(in_flight);
        tokio::spawn(task.run());
        handle
    }

    /// Both halves, unspawned. Only `shutdown_waits_for_every_run_it_started`
    /// needs this: it has to await the loop's own `JoinHandle`, which is
    /// exactly the thing `spawn_queue` throws away.
    fn build_queue(&self, in_flight: InFlight) -> (QueueHandle, scheduler::QueueTask) {
        scheduler::build(
            self.harness.context.clone(),
            self.paths.clone(),
            self.runner(),
            in_flight,
        )
    }

    /// Parallel mode with `limit` slots — the two settings keys, written
    /// through the same accessors the Settings panel and the MCP tool use.
    async fn set_parallel(&self, limit: usize) {
        capacity::set_schedule_mode(self.ctx(), ScheduleMode::Parallel)
            .await
            .expect("turn parallelism on");
        self.set_max_concurrency(limit).await;
    }

    /// The stored limit, without touching the mode — so a test can prove
    /// sequential ignores it.
    async fn set_max_concurrency(&self, limit: usize) {
        capacity::set_max_concurrency(self.ctx(), limit)
            .await
            .expect("store the global limit");
    }

    /// ADR-0010's per-repository opt-out, for this fixture's own repository.
    async fn opt_repository_out_of_the_cap(&self, limit: i64) {
        repo::set_max_concurrency(self.ctx(), &self.repository_id, limit)
            .await
            .expect("raise this repository's own cap");
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

    /// Wires a queue over a runner this test chose — for the one case that
    /// needs the preflight doctor to *fail*.
    fn spawn_queue_with_runner(&self, runner: RunnerConfig) -> QueueHandle {
        let (handle, task) = scheduler::build(
            self.harness.context.clone(),
            self.paths.clone(),
            runner,
            InFlight::new(),
        );
        tokio::spawn(task.run());
        handle
    }

    async fn add_schedule(&self, input: ScheduleInput) -> String {
        scheduler_schedule::create(self.ctx(), input)
            .await
            .expect("create a schedule")
            .id
    }

    /// Back-dates `armed_at`, standing in for a schedule that has existed since
    /// before the occurrences a test wants it to have missed.
    ///
    /// Written past the service on purpose: `create` arms from the clock, which
    /// is the behaviour every other test relies on, and there is no reason to
    /// add a service function whose only caller would be this line.
    async fn arm_schedule(&self, id: &str, armed_at: DateTime<Utc>) {
        sqlx::query!(
            "UPDATE schedules SET armed_at = ?2 WHERE id = ?1",
            id,
            armed_at
        )
        .execute(&self.ctx().pool)
        .await
        .expect("back-date a schedule");
    }

    async fn schedule(&self, id: &str) -> rimaia_core::db::Schedule {
        scheduler_schedule::get(self.ctx(), id)
            .await
            .expect("read a schedule")
    }

    async fn queue_state(&self) -> QueueState {
        scheduler::queue_state(&self.ctx().pool)
            .await
            .expect("read the queue state")
    }

    async fn window(&self) -> Option<RunWindow> {
        rimaia_core::schedule::window::active(&self.ctx().pool)
            .await
            .expect("read the run window")
    }

    /// Every `runs` row of a task, newest attempt first — the retry loop's own
    /// history, read through the same service the panel reads it through.
    async fn runs(&self, task_id: &str) -> Vec<rimaia_core::db::Run> {
        rimaia_core::runs::list_runs_for_task(self.ctx(), task_id)
            .await
            .expect("read a task's attempts")
    }

    /// The argument vector one attempt was spawned with.
    fn argv(&self, task_id: &str, attempt: usize) -> Vec<String> {
        self.cli.argv(task_id, attempt)
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

    /// The run id a card would resolve for this task — `get_task(..).last_run`,
    /// which is exactly what `ActiveRunCard` filters its tail snapshots on.
    async fn last_run_id(&self, task_id: &str) -> String {
        self.detail(task_id)
            .await
            .last_run
            .expect("a run in flight has a row")
            .id
    }

    /// The other direction: which task a run id belongs to. Read from `runs`
    /// rather than inferred, because the point is whether the recorded
    /// attribution is right.
    async fn task_of_run(&self, run_id: &str) -> String {
        sqlx::query_scalar!(
            r#"SELECT task_id AS "task_id!" FROM runs WHERE id = ?1"#,
            run_id
        )
        .fetch_one(&self.ctx().pool)
        .await
        .expect("read a run's task")
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
