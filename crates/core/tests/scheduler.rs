//! **Unix only**, as this file was when the queue loop's tests shared it with
//! these. Those moved to `crates/runner/tests/queue.rs` with the loop (task
//! 042), and widening what is left to Windows is a change of its own, not one
//! to make on the way past.
#![cfg(unix)]

//! The scheduler's board half, without a loop: the plan the queue's claim is
//! chosen from, the conditional claim that decides who owns a task, the retry
//! budget's run kinds, and what a crash leaves behind (tasks 009 and 014;
//! ADR-0007, ADR-0010, ADR-0011, ADR-0012; seam-contract D9, D29).
//!
//! Everything here runs against a real database and real git repositories,
//! and nothing sleeps. The tests that build the runner loop live in the runner
//! crate, because `rimaia-core`'s tests cannot name `rimaia-runner` (task
//! 040's `rimaia_core_does_not_depend_on_rimaia_runner`).

use chrono::{DateTime, TimeDelta, Utc};
use pretty_assertions::assert_eq;
use rimaia_core::db::{BoardColumn, ExitClass, RunKind, RunState, RunStatus, Task};
use rimaia_core::repo::{self, NewRepository};
use rimaia_core::runner::events::TokenUsage;
use rimaia_core::runner::outcome::{finish_run, start_run, NewRun, RunOutcome, SpawnedAs};
use rimaia_core::runs::bundle::RunCapture;
use rimaia_core::scheduler::{self, ClaimOutcome, SkipReason};
use rimaia_core::startup;
use rimaia_core::tasks::{self, NewTask, TaskFilter, TaskSummary};
use rimaia_core::testing::{self, TempRepo, TestContext};
use rimaia_core::{AppPaths, Clock, ServiceContext};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Working the board
// ---------------------------------------------------------------------------

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

    let plan = scheduler::plan(fixture.ctx(), &fixture.consented().await)
        .await
        .expect("read the plan");
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

// ---------------------------------------------------------------------------
// Control
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Pause, Stop and shutdown pressed mid-claim (task 009's own verification
// report, finding 4) — `try_step` used to leave a window between reading the
// switch and actually claiming a task where none of the three had anything to
// act on. `hold_version_probe` widens that window enough for a test to press
// each of them inside it.
// ---------------------------------------------------------------------------

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
            kind: RunKind::Implementation,
            session_id: "0b6d3e2e-0000-4000-8000-00000000c0de".to_string(),
            prompt: "implement the plan".to_string(),
            base_ref: None,
            base_sha: None,
        },
    )
    .await
    .expect("open the run the crash interrupted");

    let report = startup::survey(fixture.ctx(), fixture.machine(), &fixture.paths)
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

    let report = startup::survey(fixture.ctx(), fixture.machine(), &fixture.paths)
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

    let report = startup::survey(fixture.ctx(), fixture.machine(), &fixture.paths)
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
    let plan = scheduler::plan(fixture.ctx(), &fixture.consented().await)
        .await
        .expect("read the plan");
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
    let report = startup::survey(fixture.ctx(), fixture.machine(), &fixture.paths)
        .await
        .expect("survey the database");
    start_run(
        fixture.ctx(),
        &fixture.paths,
        NewRun {
            task_id: crashed.clone(),
            kind: RunKind::Implementation,
            session_id: "0b6d3e2e-0000-4000-8000-00000000feed".to_string(),
            prompt: "implement the plan".to_string(),
            base_ref: None,
            base_sha: None,
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

    let report = startup::survey(fixture.ctx(), fixture.machine(), &fixture.paths)
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
    let report = startup::survey(fixture.ctx(), fixture.machine(), &fixture.paths)
        .await
        .expect("survey the database");
    scheduler::reconcile_interrupted(fixture.ctx(), &report)
        .await
        .expect("reconcile");

    let plan = scheduler::plan(fixture.ctx(), &fixture.consented().await)
        .await
        .expect("read the plan");

    assert_eq!(
        plan.iter()
            .find(|entry| entry.task_id == crashed)
            .and_then(|entry| entry.skip),
        Some(SkipReason::NeedsAttention)
    );
    assert_eq!(scheduler::next_to_start(&plan), None);
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

// ---------------------------------------------------------------------------
// Run kinds and the retry budget (seam-contract D29 point 3)
// ---------------------------------------------------------------------------

const IMPLEMENTATION_SESSION: &str = "0b6d3e2e-0000-4000-8000-0000000001a1";

#[tokio::test]
async fn a_review_between_two_fix_phases_ends_the_first_fix_phases_budget() {
    // Each fix phase gets its own budget, even though every fix resumes the
    // implementation's session: the review row between two phases ends the
    // first one's count.
    let fixture = Fixture::new().await;
    let task_id = fixture.add_task("Looped").await;
    walk_to(&fixture, &task_id, RunState::Running).await;
    let implementation = open_row(
        &fixture,
        &task_id,
        RunKind::Implementation,
        IMPLEMENTATION_SESSION,
    )
    .await;
    finish_implementation(&fixture, &implementation, ExitClass::Success, None).await;

    let review = open_row(&fixture, &task_id, RunKind::Review, "review-session-1").await;
    close_loop_row(&fixture, &review, RunStatus::Succeeded, ExitClass::Success).await;
    for _ in 0..2 {
        let fix = open_row(&fixture, &task_id, RunKind::Fix, IMPLEMENTATION_SESSION).await;
        close_loop_row(&fixture, &fix, RunStatus::Failed, ExitClass::Transient).await;
    }
    let review = open_row(&fixture, &task_id, RunKind::Review, "review-session-2").await;
    close_loop_row(&fixture, &review, RunStatus::Succeeded, ExitClass::Success).await;
    let fix = open_row(&fixture, &task_id, RunKind::Fix, IMPLEMENTATION_SESSION).await;
    close_loop_row(&fixture, &fix, RunStatus::Failed, ExitClass::Transient).await;
    open_row(&fixture, &task_id, RunKind::Fix, IMPLEMENTATION_SESSION).await;

    let history = scheduler::attempt_history(
        fixture.ctx(),
        &task_id,
        scheduler::Ending {
            exit_class: ExitClass::Transient,
            usage_limit_resets_at: None,
        },
    )
    .await
    .expect("read the history")
    .expect("a task with runs");

    assert_eq!(history.session_id, IMPLEMENTATION_SESSION);
    assert_eq!(
        history.attempts_in_session, 2,
        "the second fix phase's two rows, and not the first phase's two"
    );
    assert_eq!(history.transient_attempts, 2);
}

#[tokio::test]
async fn a_fix_that_resumes_the_implementation_session_gets_its_own_budget() {
    // The implementation spent one transient retry before it succeeded. A fix
    // resuming that same session, after a review, starts with none spent.
    let fixture = Fixture::new().await;
    let task_id = fixture.add_task("Fixed").await;
    walk_to(&fixture, &task_id, RunState::Running).await;
    let first = open_row(
        &fixture,
        &task_id,
        RunKind::Implementation,
        IMPLEMENTATION_SESSION,
    )
    .await;
    let resume_after = fixture.harness.clock.now() + TimeDelta::minutes(1);
    finish_implementation(&fixture, &first, ExitClass::Transient, Some(resume_after)).await;
    tasks::set_run_state(fixture.ctx(), &task_id, RunState::Running)
        .await
        .expect("the retry's own edge");
    let second = open_row(
        &fixture,
        &task_id,
        RunKind::Implementation,
        IMPLEMENTATION_SESSION,
    )
    .await;
    finish_implementation(&fixture, &second, ExitClass::Success, None).await;
    let review = open_row(&fixture, &task_id, RunKind::Review, "review-session").await;
    close_loop_row(&fixture, &review, RunStatus::Succeeded, ExitClass::Success).await;
    open_row(&fixture, &task_id, RunKind::Fix, IMPLEMENTATION_SESSION).await;

    let history = scheduler::attempt_history(
        fixture.ctx(),
        &task_id,
        scheduler::Ending {
            exit_class: ExitClass::Transient,
            usage_limit_resets_at: None,
        },
    )
    .await
    .expect("read the history")
    .expect("a task with runs");

    assert_eq!(history.session_id, IMPLEMENTATION_SESSION);
    assert_eq!(history.attempts_in_session, 1);
    assert_eq!(
        history.transient_attempts, 1,
        "only the fix's own ending; the implementation's retry is its phase's"
    );
}

/// Opens a row of `kind` on `task_id` through `start_run`, the one writer.
async fn open_row(fixture: &Fixture, task_id: &str, kind: RunKind, session_id: &str) -> String {
    start_run(
        fixture.ctx(),
        &fixture.paths,
        NewRun {
            task_id: task_id.to_string(),
            kind,
            session_id: session_id.to_string(),
            prompt: "a prompt".to_string(),
            base_ref: None,
            base_sha: None,
        },
    )
    .await
    .expect("open a run row")
    .id
}

/// Closes an implementation row through `finish_run`, which lands the task.
async fn finish_implementation(
    fixture: &Fixture,
    run_id: &str,
    exit_class: ExitClass,
    resume_after: Option<DateTime<Utc>>,
) {
    let status = match exit_class {
        ExitClass::Success => RunStatus::Succeeded,
        ExitClass::Interrupted => RunStatus::Interrupted,
        ExitClass::Cancelled => RunStatus::Cancelled,
        _ => RunStatus::Failed,
    };
    finish_run(
        fixture.ctx(),
        run_id,
        &RunOutcome {
            exit_class,
            status,
            error_message: None,
            num_turns: Some(1),
            cost_usd: Some(0.1),
            duration_ms: None,
            pr_url: None,
            usage_limit_resets_at: None,
            resume_after,
            spawned_as: SpawnedAs::default(),
            usage: TokenUsage::default(),
        },
        &RunCapture::default(),
    )
    .await
    .expect("close an implementation row");
}

/// Closes a review or fix row without landing its task, for a test that
/// arranges history rather than exercising the loop.
async fn close_loop_row(fixture: &Fixture, run_id: &str, status: RunStatus, class: ExitClass) {
    testing::runs::close_run(fixture.ctx(), run_id, status, class, None).await;
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

// ---------------------------------------------------------------------------
// Waiting on the queue without waiting on a clock
// ---------------------------------------------------------------------------

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
        };
        fixture.repository_id = fixture.register_repository(true).await;
        fixture
    }

    fn ctx(&self) -> &ServiceContext {
        &self.harness.context
    }

    /// This machine's own state, over the harness's machine store (task 041).
    fn machine(&self) -> &rimaia_core::machine::MachineContext {
        self.harness.machine()
    }

    /// Registers another real repository, with ADR-0012's opt-in on or off.
    async fn register_repository(&mut self, opt_in: bool) -> String {
        let repository = TempRepo::init();
        let registered = repo::register(
            self.ctx(),
            self.machine(),
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
            repo::set_allow_unattended_runs(self.ctx(), self.machine(), &registered.id, true)
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

    /// The task row.
    async fn task(&self, task_id: &str) -> Task {
        self.detail(task_id).await.task
    }

    /// Every repository this runner consented to, read once per pass as the
    /// queue reads it (task 066).
    async fn consented(&self) -> std::collections::BTreeSet<String> {
        rimaia_core::machine::consented_repositories(self.machine())
            .await
            .expect("read the consent")
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
