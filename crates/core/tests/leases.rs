//! Runner leases: the one-transaction claim, the lease's life after it, the
//! pin a retry keeps, the record every starter makes, and the per-runner
//! reconcile (task 043; ADR-0031, ADR-0010, ADR-0011, ADR-0016; seam-contract
//! D31's 043 amendment).
//!
//! The board port's own fencing cases are in `testing::board_contract`, run
//! from `board_port_in_process.rs`, so task 052 runs them over HTTP too. What
//! is here needs more than the port: a trigger that refuses a write, the
//! runner's own store, a real `run_task`, or a reconcile after a crash.
//!
//! Git runs against real repositories, the CLI is `testing::FakeCli` replaying
//! recorded streams, and the clock is the harness's. Nothing sleeps: every
//! `timeout` is a failure bound.

#![cfg(unix)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use pretty_assertions::assert_eq;
use rimaia_core::board::OwnerPresence;
use rimaia_core::board::lease::{self, Lease, LeaseState};
use rimaia_core::board::{
    BoardFuture, BoardPort, Claim, ClaimTarget, FinishReceipt, FinishRun, Heartbeat,
    InProcessBoard, LeasePurpose, LeaseRef, LeaseTerm, NextStep, RunContext, StartRun,
    TranscriptAck, TranscriptChunk, TranscriptEnd,
};
use rimaia_core::db::{
    new_id, BoardColumn, ExitClass, Run, RunKind, RunState, RunStatus, Schedule, StrategyMode,
};
use rimaia_core::identity::SOLO_RUNNER_LABEL;
use rimaia_core::machine::{
    leases, Checkout, CheckoutPatch, HeldLease, MachineContext, MachineFuture, MachineStore,
    WorktreeRecord,
};
use rimaia_core::mcp::requests::{PlanSelectionRequest, TaskStrategyRequest};
use rimaia_core::mcp::server::{LocalTools, RimaiaServer};
use rimaia_core::repo::{self, NewRepository};
use rimaia_core::review::findings::{FindingSeverity, NewReviewFinding};
use rimaia_core::review_loop::config as review_config;
use rimaia_core::runner::events::{RunTail, TokenUsage};
use rimaia_core::runner::outcome::{start_run, NewRun, RunOutcome, SpawnedAs};
use rimaia_core::runner::provider::{ClaudeProvider, ProviderId};
use rimaia_core::runner::strategy::{claim_for_planning, plan_claimed, PlannerAccess};
use rimaia_core::runner::{
    claim_manual_start, run_task, ManualStart, RunRequest, RunTrigger, RunnerConfig,
};
use rimaia_core::scheduler::{self, InFlight, RunnerView, SlotOwner};
use rimaia_core::tasks::strategy::StrategyPlan;
use rimaia_core::tasks::{self, NewTask, TaskPatch};
use rimaia_core::testing::board::{claim_and_record, claim_run, start_and_note};
use rimaia_core::testing::db::insert_runner;
use rimaia_core::testing::machine::MemoryMachine;
use rimaia_core::testing::{self, FakeCli, TempRepo, TestContext};
use rimaia_core::{worktree, AppPaths, Clock, ErrorCode, ServiceContext};
use rmcp::handler::server::wrapper::Parameters;
use tempfile::TempDir;
use tokio::sync::broadcast::error::TryRecvError;

/// A failure bound for anything that waits on a child.
const TEST_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// The claim
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_claim_takes_both_edges_and_the_lease_in_one_transaction() {
    let f = Fixture::new().await;
    let task = f.task("Alpha").await;

    let claim = f.claim(Which::A, &task, false).await;

    assert_eq!(claim.lease.generation, 1);
    assert_eq!(claim.purpose, LeasePurpose::Implementation);
    assert_eq!(f.run_state(&task).await, RunState::Running);
    assert_eq!(
        f.lease_state(&task).await,
        LeaseState {
            lease: Some(Lease {
                task_id: task.clone(),
                purpose: LeasePurpose::Implementation,
                run_id: None,
                runner_id: f.runner_id(Which::A),
                generation: 1,
                acquired_at: f.harness.clock.now(),
                expires_at: None,
            }),
            generation: 1,
            pinned_runner_id: None,
        }
    );
    assert_eq!(f.lease_rows().await, 1, "exactly one lease row");
}

#[tokio::test]
async fn a_claim_that_cannot_write_its_lease_leaves_the_task_where_it_was() {
    // The regression test for the crash window the two-edge claim documented:
    // a failure after the edges and before the lease must take the edges back
    // with it. A trigger makes the lease's insert fail on a file-backed board,
    // the production pool's shape.
    let mut f = Fixture::over_file().await;
    let task = f.task("Alpha").await;
    sqlx::query(
        "CREATE TRIGGER refuse_leases BEFORE INSERT ON runner_leases
         BEGIN SELECT RAISE(ABORT, 'no lease can be written'); END",
    )
    .execute(&f.ctx().pool)
    .await
    .expect("create the trigger");
    let before = f.detail(&task).await;
    drain(&mut f.harness);

    let error = f
        .board(Which::A)
        .claim(run_now(&task))
        .await
        .expect_err("the lease could not be written");

    assert!(
        error.to_string().contains("no lease can be written"),
        "{error}"
    );
    assert_eq!(f.detail(&task).await, before, "the task is where it was");
    assert_eq!(f.run_state(&task).await, RunState::Idle);
    assert_eq!(
        f.lease_state(&task).await,
        LeaseState {
            lease: None,
            generation: 0,
            pinned_runner_id: None,
        }
    );
    assert_eq!(
        f.harness.changes.try_recv(),
        Err(TryRecvError::Empty),
        "nothing was published for a claim that wrote nothing",
    );
}

#[tokio::test]
async fn two_concurrent_claims_of_one_task_leave_exactly_one_winner() {
    // The conditional write, at its narrowest. ADR-0010 requires selection and
    // the transition to `running` to happen in one transaction so that the UI,
    // the MCP server and the scheduler cannot double-claim; this is that
    // property, driven against a multi-connection pool from two callers at
    // once. Moved here from `tests/scheduler.rs` with the claim (task 043).
    let f = Fixture::over_file().await;
    let task = f.task("Alpha").await;
    let a = f.board(Which::A);
    let b = f.board(Which::B);

    let (first, second) = tokio::join!(a.claim(run_now(&task)), b.claim(run_now(&task)));
    let outcomes = [
        first.expect("a lost race is not an error"),
        second.expect("a lost race is not an error"),
    ];

    assert_eq!(
        outcomes.iter().filter(|outcome| outcome.is_some()).count(),
        1,
        "exactly one claimer may own the task: {outcomes:?}"
    );
    assert!(outcomes.iter().any(Option::is_none));
    assert_eq!(f.run_state(&task).await, RunState::Running);
    assert_eq!(f.lease_rows().await, 1);
}

#[tokio::test]
async fn a_claim_is_refused_for_every_state_that_means_somebody_already_has_the_task() {
    // The property the mutual exclusion rests on, asserted state by state.
    // A `running` task holds a lease, and `waiting_retry` has no fresh-start
    // edge, so a second claimer is refused whichever of them the first left
    // behind. (`queued` with no lease is claimable since task 043: see
    // `a_queued_task_with_no_lease_is_claimed_with_one_edge`.)
    let f = Fixture::new().await;

    let running = f.task("Running").await;
    f.claim(Which::A, &running, false).await;
    let waiting = f.task("Waiting").await;
    walk(
        &f,
        &waiting,
        &[RunState::Queued, RunState::Running, RunState::WaitingRetry],
    )
    .await;

    for (task, taken) in [
        (&running, RunState::Running),
        (&waiting, RunState::WaitingRetry),
    ] {
        let lost = f
            .board(Which::B)
            .claim(run_now(task))
            .await
            .expect("a lost race is not an error");
        assert!(lost.is_none(), "a task in {taken:?} was claimed: {lost:?}");
        assert_eq!(f.run_state(task).await, taken);
    }

    // The other half: last night's failure is startable again, which is what
    // makes "Run now" on a failed card mean anything (ADR-0007's note on that
    // edge — trying again "re-enters at Queued like every other start").
    for (title, route) in [
        (
            "Failed",
            &[RunState::Queued, RunState::Running, RunState::Failed][..],
        ),
        ("Cancelled", &[RunState::Queued, RunState::Cancelled][..]),
    ] {
        let task = f.task(title).await;
        walk(&f, &task, route).await;
        f.claim(Which::A, &task, false).await;
        assert_eq!(f.run_state(&task).await, RunState::Running, "{title}");
    }
}

#[tokio::test]
async fn a_lost_race_in_next_moves_on_to_the_next_entry() {
    // The plan reads the top task as startable: it is `idle`. But B holds a
    // planner's lease on it, so A's claim transaction finds the lease and
    // loses, and the same `Next` goes on to the task below.
    let f = Fixture::new().await;
    let top = f.task("Top").await;
    let below = f.task("Below").await;
    f.board(Which::B)
        .claim(ClaimTarget::Plan {
            task_id: top.clone(),
        })
        .await
        .expect("plan")
        .expect("B's planner holds the top task");

    let claim = f
        .board(Which::A)
        .claim(f.next())
        .await
        .expect("claim")
        .expect("the next entry");

    assert_eq!(claim.lease.task_id, below);
    assert_eq!(f.run_state(&top).await, RunState::Idle, "B's lease is B's");
    assert_eq!(
        f.lease_state(&top)
            .await
            .lease
            .map(|lease| (lease.runner_id, lease.purpose)),
        Some((f.runner_id(Which::B), LeasePurpose::Strategy))
    );
}

#[tokio::test]
async fn a_queued_task_with_no_lease_is_claimed_with_one_edge() {
    // What a build older than 043 left, and what task 057's `release_pin`
    // re-queues: `queued` takes its one remaining edge, `queued -> running`.
    let f = Fixture::new().await;
    let task = f.task("Queued").await;
    walk(&f, &task, &[RunState::Queued]).await;

    let claim = f.claim(Which::A, &task, false).await;

    assert_eq!(claim.lease.generation, 1);
    assert_eq!(f.run_state(&task).await, RunState::Running);
}

#[tokio::test]
async fn a_strategy_claim_writes_a_lease_and_no_run_state_edge() {
    let f = Fixture::new().await;
    let task = f.task("Plan me").await;

    let claim = f
        .board(Which::A)
        .claim(ClaimTarget::Plan {
            task_id: task.clone(),
        })
        .await
        .expect("plan")
        .expect("a lease");

    assert_eq!(claim.purpose, LeasePurpose::Strategy);
    let held = f.lease_state(&task).await.lease.expect("a lease");
    assert_eq!(held.purpose, LeasePurpose::Strategy);
    assert_eq!(held.run_id, None);
    assert_eq!(f.run_state(&task).await, RunState::Idle);

    f.board(Which::A)
        .release(&claim.lease)
        .await
        .expect("release");

    assert_eq!(f.lease_state(&task).await.lease, None);
    assert_eq!(f.run_state(&task).await, RunState::Idle);
}

#[tokio::test]
async fn a_task_that_needs_planning_is_leased_as_strategy_until_start_run() {
    // ADR-0016's inline planner is leased as `strategy` (ADR-0031 point 1),
    // with the task `running`; `start_run` moves the lease to the
    // implementation under the same generation.
    let f = Fixture::new().await;
    let board = f.board(Which::A);
    let planned = f.task("Planned").await;
    f.plan_mode(&planned).await;

    let claim = f.claim(Which::A, &planned, false).await;

    assert_eq!(claim.purpose, LeasePurpose::Strategy);
    assert_eq!(f.run_state(&planned).await, RunState::Running);
    let held = f.lease_state(&planned).await.lease.expect("a lease");
    assert_eq!((held.purpose, held.run_id), (LeasePurpose::Strategy, None));
    board
        .record_strategy(
            &claim.lease,
            StrategyPlan::proposed(Some("claude-sonnet-5".to_string()), None),
        )
        .await
        .expect("a planner writes under a strategy lease");
    let run_id = new_id();
    board
        .start_run(&claim.lease, starting(&run_id, RunKind::Implementation))
        .await
        .expect("start the implementation");
    let held = f.lease_state(&planned).await.lease.expect("a lease");
    assert_eq!(
        (held.purpose, held.run_id, held.generation),
        (
            LeasePurpose::Implementation,
            Some(run_id),
            claim.lease.generation
        )
    );

    // Released before `start_run`, the inline planner lands `failed`, as the
    // implementation it would have become does.
    let abandoned = f.task("Planned, then abandoned").await;
    f.plan_mode(&abandoned).await;
    let claim = f.claim(Which::A, &abandoned, false).await;
    board.release(&claim.lease).await.expect("release");
    assert_eq!(f.run_state(&abandoned).await, RunState::Failed);
    assert_eq!(f.lease_state(&abandoned).await.lease, None);

    // A task that needs no planning is leased as the implementation, and
    // `run_task` spawns exactly one process for it: no planner in front.
    let plain = f.task("Plain").await;
    let claim = f.claim(Which::A, &plain, false).await;
    assert_eq!(claim.purpose, LeasePurpose::Implementation);
    let run = f.run(Which::A, claim).await.expect("the run completes");
    assert_eq!(run.status, RunStatus::Succeeded);
    assert_eq!(f.cli.attempts(&plain), 1, "no planner was spawned");
}

#[tokio::test]
async fn start_run_moves_the_lease_to_the_runs_kind_under_one_generation() {
    let f = Fixture::new().await;
    f.turn_the_loop_on().await;
    let task = f.task("Looped").await;
    let board = f.board(Which::A);
    let claim = f.claim(Which::A, &task, false).await;
    let generation = claim.lease.generation;

    let implementation = new_id();
    board
        .start_run(
            &claim.lease,
            starting(&implementation, RunKind::Implementation),
        )
        .await
        .expect("start the implementation");
    f.assert_lease_on(
        &task,
        LeasePurpose::Implementation,
        &implementation,
        generation,
    )
    .await;
    let next = board
        .finish_run(
            &claim.lease,
            &implementation,
            finishing_at(succeeded(), "a1"),
        )
        .await
        .expect("finish the implementation")
        .next;
    assert_eq!(
        next,
        NextStep::Continue {
            kind: RunKind::Review
        }
    );

    let review = new_id();
    board
        .start_run(&claim.lease, starting(&review, RunKind::Review))
        .await
        .expect("start the review");
    f.assert_lease_on(&task, LeasePurpose::Review, &review, generation)
        .await;
    board
        .record_review_findings(&claim.lease, &review, vec![blocking_finding()])
        .await
        .expect("record a blocking finding");
    let next = board
        .finish_run(&claim.lease, &review, finishing_at(succeeded(), "a1"))
        .await
        .expect("finish the review")
        .next;
    assert_eq!(next, NextStep::Continue { kind: RunKind::Fix });

    let fix = new_id();
    board
        .start_run(&claim.lease, starting(&fix, RunKind::Fix))
        .await
        .expect("start the fix");
    f.assert_lease_on(&task, LeasePurpose::Fix, &fix, generation)
        .await;
    assert_eq!(f.lease_state(&task).await.generation, generation);
}

#[tokio::test]
async fn a_waiting_review_is_claimed_back_as_a_review() {
    let f = Fixture::new().await;
    f.turn_the_loop_on().await;
    let task = f.task("Reviewed, then a wall").await;
    let board = f.board(Which::A);
    let claim = f.claim(Which::A, &task, false).await;
    let implementation = new_id();
    board
        .start_run(
            &claim.lease,
            starting(&implementation, RunKind::Implementation),
        )
        .await
        .expect("start the implementation");
    board
        .finish_run(
            &claim.lease,
            &implementation,
            finishing_at(succeeded(), "a1"),
        )
        .await
        .expect("finish the implementation");
    let review = new_id();
    board
        .start_run(&claim.lease, starting(&review, RunKind::Review))
        .await
        .expect("start the review");
    let receipt = board
        .finish_run(&claim.lease, &review, finishing(transient()))
        .await
        .expect("the review hit a transient wall");
    let NextStep::Released {
        resume_after: Some(due),
    } = receipt.next
    else {
        panic!("a transient review is retried: {:?}", receipt.next);
    };
    assert_eq!(f.run_state(&task).await, RunState::WaitingRetry);
    f.harness.clock.set(due);

    let resumed = f.claim(Which::A, &task, true).await;

    assert_eq!(resumed.purpose, LeasePurpose::Review);
    assert_eq!(
        resumed.resume.map(|point| point.kind),
        Some(RunKind::Review)
    );
    assert_eq!(
        f.lease_state(&task).await.lease.map(|lease| lease.purpose),
        Some(LeasePurpose::Review)
    );
}

#[tokio::test]
async fn a_released_finish_deletes_the_lease_in_the_transaction_that_lands_the_task() {
    let f = Fixture::new().await;
    let board = f.board(Which::A);

    let task = f.task("Landed").await;
    let claim = f.claim(Which::A, &task, false).await;
    let run_id = new_id();
    board
        .start_run(&claim.lease, starting(&run_id, RunKind::Implementation))
        .await
        .expect("start");
    let receipt = board
        .finish_run(&claim.lease, &run_id, finishing(succeeded()))
        .await
        .expect("finish");
    assert_eq!(receipt.next, NextStep::Released { resume_after: None });
    assert_eq!(f.lease_state(&task).await.lease, None);
    assert_eq!(f.run_state(&task).await, RunState::Idle);

    // One transaction: a landing that fails keeps the lease with it.
    let refused = f.task("Refused its landing").await;
    let claim = f.claim(Which::A, &refused, false).await;
    let run_id = new_id();
    board
        .start_run(&claim.lease, starting(&run_id, RunKind::Implementation))
        .await
        .expect("start");
    sqlx::query(
        "CREATE TRIGGER refuse_landing BEFORE UPDATE OF run_state ON tasks
         WHEN NEW.run_state = 'idle'
         BEGIN SELECT RAISE(ABORT, 'the landing was refused'); END",
    )
    .execute(&f.ctx().pool)
    .await
    .expect("create the trigger");

    board
        .finish_run(&claim.lease, &run_id, finishing(succeeded()))
        .await
        .expect_err("the landing was refused");

    assert_eq!(f.run_state(&refused).await, RunState::Running);
    assert_eq!(
        f.lease_state(&refused)
            .await
            .lease
            .map(|lease| lease.generation),
        Some(claim.lease.generation),
        "the lease was not deleted by a landing that did not happen",
    );
    assert_eq!(f.detail(&refused).await.task.column, BoardColumn::Ready);
}

#[tokio::test]
async fn continue_keeps_the_lease() {
    let f = Fixture::new().await;
    f.turn_the_loop_on().await;
    let task = f.task("Continued").await;
    let board = f.board(Which::A);
    let claim = f.claim(Which::A, &task, false).await;
    let run_id = new_id();
    board
        .start_run(&claim.lease, starting(&run_id, RunKind::Implementation))
        .await
        .expect("start");

    let receipt = board
        .finish_run(&claim.lease, &run_id, finishing_at(succeeded(), "a1"))
        .await
        .expect("finish");

    assert_eq!(
        receipt.next,
        NextStep::Continue {
            kind: RunKind::Review
        }
    );
    let held = f.lease_state(&task).await.lease.expect("still leased");
    assert_eq!(held.generation, claim.lease.generation);
    assert_eq!(held.runner_id, f.runner_id(Which::A));
    assert_eq!(f.run_state(&task).await, RunState::Running);
}

#[tokio::test]
async fn a_continue_that_eligibility_refuses_is_released() {
    // While A holds the lease mid-loop, the task is pinned to B. A's finish
    // would continue to a review, but the next phase is a claim, and this
    // runner is no longer eligible for it (D31 point 4).
    let f = Fixture::new().await;
    f.turn_the_loop_on().await;
    let task = f.task("Moved mid-loop").await;
    let board = f.board(Which::A);
    let claim = f.claim(Which::A, &task, false).await;
    let run_id = new_id();
    board
        .start_run(&claim.lease, starting(&run_id, RunKind::Implementation))
        .await
        .expect("start");
    lease::pin_for_test(f.ctx(), &task, Some(&f.runner_id(Which::B)))
        .await
        .expect("pin the task to B");

    let receipt = board
        .finish_run(&claim.lease, &run_id, finishing_at(succeeded(), "a1"))
        .await
        .expect("finish");

    assert_eq!(receipt.next, NextStep::Released { resume_after: None });
    assert_eq!(f.lease_state(&task).await.lease, None);
    assert_eq!(f.run_count(&task).await, 1, "no next phase was started");
    let detail = f.detail(&task).await;
    assert_eq!(detail.task.column, BoardColumn::InReview);
    assert_eq!(detail.task.run_state, RunState::Idle);
}

// ---------------------------------------------------------------------------
// Pinning
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_run_that_lands_waiting_retry_pins_the_task_to_its_runner() {
    let f = Fixture::new().await;
    let task = f.task("Hit the wall").await;
    f.cli.replays(&task, "usage-limit", 143);
    let claim = f.claim(Which::A, &task, false).await;

    let run = f
        .run(Which::A, claim)
        .await
        .expect("the attempt is recorded");

    assert_eq!(run.exit_class, Some(ExitClass::UsageLimit));
    assert_eq!(f.run_state(&task).await, RunState::WaitingRetry);
    assert_eq!(
        f.lease_state(&task).await,
        LeaseState {
            lease: None,
            generation: 1,
            pinned_runner_id: Some(f.runner_id(Which::A)),
        }
    );
}

#[tokio::test]
async fn an_interrupted_close_pins_the_task_even_when_the_budget_is_spent() {
    // Interrupted once, and again, in one session, until ADR-0011's budget is
    // spent and the board stops offering a resume: the task lands `failed`,
    // and it is still pinned, because its worktree and session are still A's.
    let f = Fixture::new().await;
    let task = f.task("Crashes every night").await;
    let board = f.board(Which::A);
    let mut claim = f.claim(Which::A, &task, false).await;

    for attempt in 1..=20 {
        let run_id = new_id();
        board
            .start_run(&claim.lease, starting(&run_id, RunKind::Implementation))
            .await
            .expect("start");
        let receipt = board
            .finish_run(&claim.lease, &run_id, finishing(interrupted()))
            .await
            .expect("finish as interrupted");
        assert_eq!(
            f.lease_state(&task).await.pinned_runner_id,
            Some(f.runner_id(Which::A)),
            "attempt {attempt} pinned the task",
        );
        match receipt.next {
            NextStep::Released {
                resume_after: Some(due),
            } => {
                f.harness.clock.set(due.max(f.harness.clock.now()));
                claim = f.claim(Which::A, &task, true).await;
            }
            NextStep::Released { resume_after: None } => {
                assert_eq!(f.run_state(&task).await, RunState::Failed);
                return;
            }
            other => panic!("an interrupted implementation does not continue: {other:?}"),
        }
    }
    panic!("the budget was never spent");
}

#[tokio::test]
async fn a_pinned_task_is_passed_over_by_another_runners_next_claim_and_claimed_by_its_own() {
    let f = Fixture::new().await;
    let task = f.task("Pinned to A").await;
    let due = f.wait_on_a_transient_wall(&task).await;
    f.harness.clock.set(due);

    let b = f
        .board(Which::B)
        .claim(f.next())
        .await
        .expect("B asks with free capacity");
    assert!(b.is_none(), "B was handed A's task: {b:?}");
    let b_plan = scheduler::plan(f.ctx(), &f.view(Which::B))
        .await
        .expect("B's plan");
    assert!(
        scheduler::next_to_start(&b_plan).is_none(),
        "B's plan lists the task as next: {b_plan:?}"
    );

    let a = f
        .board(Which::A)
        .claim(f.next())
        .await
        .expect("A asks")
        .expect("A's own task");
    assert_eq!(a.lease.task_id, task);
    assert!(a.resume.is_some(), "A resumes the session it pinned");
}

#[tokio::test]
async fn run_now_and_plan_now_on_another_runner_refuse_a_pinned_task_and_name_the_holder() {
    let f = Fixture::new().await;
    let task = f.task("Pinned to A").await;
    f.wait_on_a_transient_wall(&task).await;
    let before = (f.detail(&task).await, f.lease_state(&task).await);
    let expected = format!(
        "this task is pinned to {SOLO_RUNNER_LABEL}, which has its worktree and the agent's \
         conversation. Only that runner can run it until someone chooses to run it elsewhere."
    );

    for target in [
        run_now(&task),
        ClaimTarget::Plan {
            task_id: task.clone(),
        },
    ] {
        let error = f
            .board(Which::B)
            .claim(target.clone())
            .await
            .expect_err("refused");
        assert_eq!(error.code(), ErrorCode::Invalid, "{target:?}");
        assert_eq!(error.to_string(), expected, "{target:?}");
    }

    assert_eq!(
        (f.detail(&task).await, f.lease_state(&task).await),
        before,
        "nothing was written",
    );
}

#[tokio::test]
async fn a_finish_by_the_pinned_runner_outside_waiting_retry_clears_the_pin() {
    let f = Fixture::new().await;
    let task = f.task("Back on its feet").await;
    let due = f.wait_on_a_transient_wall(&task).await;
    f.harness.clock.set(due);
    let board = f.board(Which::A);
    let claim = f.claim(Which::A, &task, true).await;
    let run_id = new_id();
    board
        .start_run(&claim.lease, starting(&run_id, RunKind::Implementation))
        .await
        .expect("start");

    board
        .finish_run(&claim.lease, &run_id, finishing(succeeded()))
        .await
        .expect("finish");

    assert_eq!(f.lease_state(&task).await.pinned_runner_id, None);
    assert_eq!(f.run_state(&task).await, RunState::Idle);
}

#[tokio::test]
async fn continue_leaves_the_pin() {
    let f = Fixture::new().await;
    f.turn_the_loop_on().await;
    let task = f.task("Pinned and looping").await;
    lease::pin_for_test(f.ctx(), &task, Some(&f.runner_id(Which::A)))
        .await
        .expect("pin the task to A");
    let board = f.board(Which::A);
    let claim = f.claim(Which::A, &task, false).await;
    let run_id = new_id();
    board
        .start_run(&claim.lease, starting(&run_id, RunKind::Implementation))
        .await
        .expect("start");

    let receipt = board
        .finish_run(&claim.lease, &run_id, finishing_at(succeeded(), "a1"))
        .await
        .expect("finish");

    assert_eq!(
        receipt.next,
        NextStep::Continue {
            kind: RunKind::Review
        }
    );
    assert_eq!(
        f.lease_state(&task).await.pinned_runner_id,
        Some(f.runner_id(Which::A))
    );
}

#[tokio::test]
async fn giving_up_leaves_the_pin() {
    let f = Fixture::new().await;
    let task = f.task("Given up on").await;
    f.wait_on_a_transient_wall(&task).await;

    scheduler::give_up(f.ctx(), &task).await.expect("give up");

    assert_eq!(f.run_state(&task).await, RunState::Failed);
    assert_eq!(
        f.lease_state(&task).await.pinned_runner_id,
        Some(f.runner_id(Which::A)),
        "the board never moves a pinned task on its own",
    );
}

// ---------------------------------------------------------------------------
// Recording on the runner
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_starter_records_its_claim_before_it_spawns() {
    // Run now, Retry now and Plan now — the command's path and both MCP
    // tools. The runner loop is the runner crate's, and its half of this
    // test is `crates/runner/tests/queue.rs`'s of the same name.
    let f = Fixture::new().await;
    let config = f.config();

    // Run now, and then Retry now on the wall it hits.
    let task = f.task("Started by hand").await;
    f.cli.replays_on_attempt(&task, 1, "usage-limit", 143);
    let spy = Witness::new(f.board(Which::A), f.machine().clone());
    let in_flight = InFlight::new();
    for continue_session in [false, true] {
        let started = claim_manual_start(
            // Asked from away, for the trigger every recording was captured
            // under.
            f.harness.starter(OwnerPresence::Remote),
            &spy,
            f.machine(),
            &f.paths,
            &config,
            &in_flight,
            ManualStart {
                task_id: task.clone(),
                continue_session,
            },
        )
        .await
        .expect("the start is granted");
        let generation = started.claim.lease.generation;
        let run = tokio::time::timeout(
            TEST_TIMEOUT,
            run_task(
                &spy,
                f.machine(),
                &f.paths,
                &config,
                started.claim,
                RunRequest::default(),
            ),
        )
        .await
        .expect("the run finishes")
        .expect("the run is recorded");
        drop(started.slot);

        let (before_spawn, at_finish) = spy.take();
        assert_eq!(
            before_spawn,
            vec![held(
                &f,
                &task,
                LeasePurpose::Implementation,
                None,
                generation
            )],
            "recorded before anything was spawned (continue: {continue_session})",
        );
        assert_eq!(
            at_finish,
            vec![held(
                &f,
                &task,
                LeasePurpose::Implementation,
                Some(&run.id),
                generation
            )],
            "the run is noted once start_run reported it",
        );
        assert_eq!(f.held().await, Vec::<HeldLease>::new(), "forgotten");
        if !continue_session {
            let due = run.resume_after.expect("a usage limit is retried");
            f.harness.clock.set(due);
        }
    }

    // Plan now, the command's path.
    let planned = f.task("Planned by hand").await;
    f.plan_mode(&planned).await;
    let claim = claim_for_planning(
        f.harness.starter(OwnerPresence::AtRunner),
        &spy,
        f.machine(),
        &in_flight,
        &planned,
        SlotOwner::Manual,
    )
    .await
        .expect("the claim")
        .expect("nothing refused it");
    let generation = claim.lease().generation;
    plan_claimed(&spy, f.machine(), &f.paths, &config, claim)
        .await
        .expect("the planner ran");
    let (before_spawn, _) = spy.take();
    assert_eq!(
        before_spawn,
        vec![held(&f, &planned, LeasePurpose::Strategy, None, generation)],
    );
    assert_eq!(f.held().await, Vec::<HeldLease>::new());

    // Plan now through both MCP tools, over the same witness.
    let server = RimaiaServer::new(
        f.ctx().clone(),
        Arc::new(ClaudeProvider),
        Some(LocalTools {
            machine: f.machine().clone(),
            doctor: testing::doctor::environment(),
            planner: PlannerAccess {
                paths: f.paths.clone(),
                runner: config.clone(),
                in_flight: in_flight.clone(),
                board: spy.shared(),
                runner_id: f.runner_id(Which::A),
            },
        }),
    );
    let one = f.task("Planned over MCP").await;
    f.plan_mode(&one).await;
    server
        .plan_task_strategy(Parameters(TaskStrategyRequest {
            task_id: one.clone(),
        }))
        .await
        .expect("the tool answers");
    let (before_spawn, _) = spy.take();
    assert_eq!(
        before_spawn
            .iter()
            .map(|lease| (lease.task_id.clone(), lease.purpose, lease.run_id.clone()))
            .collect::<Vec<_>>(),
        vec![(one.clone(), LeasePurpose::Strategy, None)],
    );
    assert_eq!(f.held().await, Vec::<HeldLease>::new());

    let pass = f.task("Planned in a pass").await;
    f.plan_mode(&pass).await;
    server
        .plan_tasks_strategy(Parameters(PlanSelectionRequest {
            column: None,
            repository_id: None,
            task_ids: vec![pass.clone()],
        }))
        .await
        .expect("the tool answers");
    let (before_spawn, _) = spy.take();
    assert_eq!(
        before_spawn
            .iter()
            .map(|lease| (lease.task_id.clone(), lease.purpose, lease.run_id.clone()))
            .collect::<Vec<_>>(),
        vec![(pass.clone(), LeasePurpose::Strategy, None)],
    );
    assert_eq!(f.held().await, Vec::<HeldLease>::new());
}

#[tokio::test]
async fn a_claim_the_runner_cannot_record_is_released_and_not_run() {
    // Run now and Plan now, the two starters core owns; the loop's is the
    // runner crate's. Either way the starter answers the store's error, the
    // board's lease is gone, and run_state is where D31's release rule puts it.
    let f = Fixture::new().await;
    let board = f.board(Which::A);
    let machine = MachineContext {
        store: Arc::new(RefusesToRecord(Arc::clone(&f.machine().store))),
        ..f.machine().clone()
    };
    let in_flight = InFlight::new();

    let task = f.task("Started by hand").await;
    let error = claim_manual_start(
        f.harness.starter(OwnerPresence::AtRunner),
        board.as_ref(),
        &machine,
        &f.paths,
        &f.config(),
        &in_flight,
        ManualStart {
            task_id: task.clone(),
            continue_session: false,
        },
    )
    .await
    .err()
    .expect("a claim this runner could not record is refused");

    assert_eq!(error.to_string(), REFUSED_RECORD);
    assert_eq!(
        f.lease_state(&task).await,
        LeaseState {
            lease: None,
            generation: 1,
            pinned_runner_id: None,
        },
        "the claim was released, and a release does not pin",
    );
    // The claim took the task to `running`; released with nothing open, it
    // lands `failed`.
    assert_eq!(f.run_state(&task).await, RunState::Failed);
    assert_eq!(f.run_count(&task).await, 0, "nothing was started");
    assert!(in_flight.is_empty(), "the slot went with the claim");

    let planned = f.task("Planned by hand").await;
    f.plan_mode(&planned).await;
    let error = claim_for_planning(
        f.harness.starter(OwnerPresence::AtRunner),
        board.as_ref(),
        &machine,
        &in_flight,
        &planned,
        SlotOwner::Manual,
    )
    .await
    .err()
    .expect("a claim this runner could not record is refused");

    assert_eq!(error.to_string(), REFUSED_RECORD);
    assert_eq!(f.lease_state(&planned).await.lease, None, "released");
    assert_eq!(
        f.run_state(&planned).await,
        RunState::Idle,
        "a Plan claim took no edge, so its release moves none",
    );
    assert!(in_flight.is_empty(), "the slot went with the claim");
    assert_eq!(f.held().await, Vec::<HeldLease>::new());
}

#[tokio::test]
async fn a_release_that_failed_keeps_the_record_and_one_that_found_no_lease_forgets_it() {
    // `Conflict` and `NotFound` say the board holds no such lease, so there is
    // nothing left to reconcile. Any other failure may have left the lease
    // standing, and the record is what lets the next launch settle it.
    let f = Fixture::new().await;
    let cases: [(&str, rimaia_core::Result<()>, bool); 5] = [
        ("released", Ok(()), false),
        ("fenced", Err(rimaia_core::Error::conflict("fenced")), false),
        (
            "no lease",
            Err(rimaia_core::Error::not_found("gone")),
            false,
        ),
        (
            "board unreachable",
            Err(rimaia_core::Error::Io(std::io::Error::other("unreachable"))),
            true,
        ),
        (
            "board failed",
            Err(rimaia_core::Error::internal("the board failed")),
            true,
        ),
    ];

    for (case, released, kept) in cases {
        let machine = memory_machine(&f.harness);
        let record = held(&f, "task-1", LeasePurpose::Implementation, None, 1);
        machine
            .store
            .record_held_lease(&record)
            .await
            .expect("record");

        leases::forget_released(&machine, "task-1", &released).await;

        let expected = if kept { vec![record] } else { Vec::new() };
        assert_eq!(
            leases::held(&machine).await.expect("the record"),
            expected,
            "{case}",
        );
    }
}

// ---------------------------------------------------------------------------
// Reconcile
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_runner_reconciles_only_the_leases_it_held() {
    let f = Fixture::new().await;
    let mine = f.task("A's").await;
    let theirs = f.task("B's").await;
    let a = f.board(Which::A);
    let b = f.board(Which::B);
    let b_machine = memory_machine(&f.harness);
    let a_run = f.crash_mid_run(a.as_ref(), f.machine(), &mine).await;
    f.crash_mid_run(b.as_ref(), &b_machine, &theirs).await;
    let theirs_before = (
        f.detail(&theirs).await,
        f.lease_state(&theirs).await,
        f.runs(&theirs).await,
        leases::held(&b_machine).await.expect("B's record"),
    );

    let reconciled = scheduler::reconcile_held(a.as_ref(), f.machine())
        .await
        .expect("reconcile");

    assert_eq!(reconciled, vec![mine.clone()]);
    let closed = f.runs(&mine).await.pop().expect("A's run");
    assert_eq!(closed.id, a_run);
    assert_eq!(closed.status, RunStatus::Interrupted);
    // ADR-0011 resumes an interruption once, immediately.
    assert_eq!(f.run_state(&mine).await, RunState::WaitingRetry);
    assert_eq!(
        f.lease_state(&mine).await,
        LeaseState {
            lease: None,
            generation: 1,
            pinned_runner_id: Some(f.runner_id(Which::A)),
        }
    );
    assert_eq!(f.held().await, Vec::<HeldLease>::new());
    assert_eq!(
        (
            f.detail(&theirs).await,
            f.lease_state(&theirs).await,
            f.runs(&theirs).await,
            leases::held(&b_machine).await.expect("B's record"),
        ),
        theirs_before,
        "B's task, run and lease are B's to reconcile",
    );
}

#[tokio::test]
async fn a_held_lease_whose_budget_is_spent_lands_failed_and_pinned() {
    let f = Fixture::new().await;
    let task = f.task("Out of budget").await;
    let board = f.board(Which::A);
    // A session whose interruptions have already spent the allowance.
    let session = "0b6d3e2e-0000-4000-8000-0000000b0d9e";
    walk(&f, &task, &[RunState::Queued, RunState::Running]).await;
    for _ in 0..=scheduler::MAX_TRANSIENT_ATTEMPTS {
        let run = start_run(
            f.ctx(),
            &f.paths,
            NewRun {
                task_id: task.clone(),
                kind: RunKind::Implementation,
                session_id: session.to_string(),
                prompt: "the plan".to_string(),
                base_ref: None,
                base_sha: None,
            },
        )
        .await
        .expect("an earlier attempt");
        testing::runs::close_run(
            f.ctx(),
            &run.id,
            RunStatus::Interrupted,
            ExitClass::Interrupted,
            None,
        )
        .await;
    }
    walk(&f, &task, &[RunState::Failed]).await;
    let claim = claim_and_record(board.as_ref(), f.machine(), &task).await;
    start_and_note(
        board.as_ref(),
        f.machine(),
        &claim,
        StartRun {
            session_id: session.to_string(),
            ..starting(&new_id(), RunKind::Implementation)
        },
    )
    .await;

    scheduler::reconcile_held(board.as_ref(), f.machine())
        .await
        .expect("reconcile");

    assert_eq!(f.run_state(&task).await, RunState::Failed);
    assert_eq!(
        f.lease_state(&task).await.pinned_runner_id,
        Some(f.runner_id(Which::A))
    );
    assert_eq!(f.lease_state(&task).await.lease, None);
}

#[tokio::test]
async fn a_held_lease_the_board_already_closed_is_dropped_without_touching_the_board() {
    let f = Fixture::new().await;
    let task = f.task("Already closed").await;
    let board = f.board(Which::A);
    let claim = claim_and_record(board.as_ref(), f.machine(), &task).await;
    // The board released it, and the forget never happened.
    board.release(&claim.lease).await.expect("release");
    let before = (f.detail(&task).await, f.lease_state(&task).await);

    let reconciled = scheduler::reconcile_held(board.as_ref(), f.machine())
        .await
        .expect("reconcile");

    assert_eq!(reconciled, Vec::<String>::new());
    assert_eq!(
        f.held().await,
        Vec::<HeldLease>::new(),
        "the record is gone"
    );
    assert_eq!(
        (f.detail(&task).await, f.lease_state(&task).await),
        before,
        "the board was not touched",
    );
}

#[tokio::test]
async fn a_held_lease_whose_run_the_board_already_closed_is_released() {
    // The crash landed between a `Continue` and the runner's note of it: the
    // board closed the implementation and kept the lease for the review, and
    // the record still names the closed run. Finishing it again is refused as
    // already finalized, so the lease is released, and a task still `running`
    // with nothing open lands `failed` (D31's release rule).
    let f = Fixture::new().await;
    f.turn_the_loop_on().await;
    let task = f.task("Crashed after a Continue").await;
    let board = f.board(Which::A);
    let claim = claim_and_record(board.as_ref(), f.machine(), &task).await;
    let implementation = new_id();
    start_and_note(
        board.as_ref(),
        f.machine(),
        &claim,
        starting(&implementation, RunKind::Implementation),
    )
    .await;
    let next = board
        .finish_run(
            &claim.lease,
            &implementation,
            finishing_at(succeeded(), "a1"),
        )
        .await
        .expect("finish the implementation")
        .next;
    assert_eq!(
        next,
        NextStep::Continue {
            kind: RunKind::Review
        }
    );
    assert_eq!(
        f.held().await,
        vec![held(
            &f,
            &task,
            LeasePurpose::Implementation,
            Some(&implementation),
            claim.lease.generation,
        )],
        "the record still names the run the board closed",
    );
    let closed_before = f.runs(&task).await;
    // Later, so a second close would show in the row's timestamps.
    f.harness.clock.advance(chrono::Duration::seconds(30));

    let reconciled = scheduler::reconcile_held(board.as_ref(), f.machine())
        .await
        .expect("reconcile");

    assert_eq!(reconciled, vec![task.clone()]);
    assert_eq!(f.held().await, Vec::<HeldLease>::new(), "forgotten");
    assert_eq!(
        f.lease_state(&task).await,
        LeaseState {
            lease: None,
            generation: claim.lease.generation,
            pinned_runner_id: None,
        },
        "released, and a release does not pin",
    );
    assert_eq!(f.run_state(&task).await, RunState::Failed);
    assert_eq!(
        f.runs(&task).await,
        closed_before,
        "the closed run is not closed again"
    );
}

#[tokio::test]
async fn a_strategy_lease_held_at_a_crash_is_released_and_run_state_is_untouched() {
    let f = Fixture::new().await;
    let task = f.task("Planning when it crashed").await;
    let board = f.board(Which::A);
    let claim = board
        .claim(ClaimTarget::Plan {
            task_id: task.clone(),
        })
        .await
        .expect("plan")
        .expect("a lease");
    leases::record(f.machine(), &claim).await.expect("record");

    scheduler::reconcile_held(board.as_ref(), f.machine())
        .await
        .expect("reconcile");

    assert_eq!(f.run_state(&task).await, RunState::Idle);
    assert_eq!(f.lease_state(&task).await.lease, None);
    assert_eq!(f.held().await, Vec::<HeldLease>::new());
}

#[tokio::test]
async fn a_solo_lease_the_runner_store_never_recorded_is_reconciled_at_startup() {
    // The claim committed and its run opened, and the process died before the
    // runner's store heard of either: two stores share no transaction. A solo
    // lease never expires, so only the solo arm can find it.
    let f = Fixture::new().await;
    let task = f.task("Never recorded").await;
    let board = f.board(Which::A);
    let claim = f.claim(Which::A, &task, false).await;
    board
        .start_run(&claim.lease, starting(&new_id(), RunKind::Implementation))
        .await
        .expect("start");

    assert_eq!(
        scheduler::reconcile_held(board.as_ref(), f.machine())
            .await
            .expect("reconcile what was held"),
        Vec::<String>::new(),
        "nothing was recorded",
    );
    let reconciled = f.reconcile_unrecorded().await;

    assert_eq!(reconciled, vec![task.clone()]);
    assert_eq!(f.run_state(&task).await, RunState::WaitingRetry);
    let last = f.detail(&task).await.last_run.expect("the run");
    assert_eq!(last.status, RunStatus::Interrupted);
    assert!(
        last.resume_after
            .is_some_and(|due| due <= f.harness.clock.now()),
        "offered for resume, now",
    );
    assert_eq!(f.lease_state(&task).await.lease, None);
}

#[tokio::test]
async fn a_task_left_running_by_a_build_without_leases_is_still_offered_for_resume() {
    let f = Fixture::new().await;
    let task = f.task("Left by an older build").await;
    walk(&f, &task, &[RunState::Queued, RunState::Running]).await;
    start_run(
        f.ctx(),
        &f.paths,
        NewRun {
            task_id: task.clone(),
            kind: RunKind::Implementation,
            session_id: "0b6d3e2e-0000-4000-8000-0000000001d5".to_string(),
            prompt: "the plan".to_string(),
            base_ref: None,
            base_sha: None,
        },
    )
    .await
    .expect("the run an older build opened");

    let reconciled = f.reconcile_unrecorded().await;

    assert_eq!(reconciled, vec![task.clone()]);
    assert_eq!(f.run_state(&task).await, RunState::WaitingRetry);
    let last = f.detail(&task).await.last_run.expect("the run");
    assert_eq!(last.status, RunStatus::Interrupted);
    assert_eq!(last.resume_after, Some(f.harness.clock.now()));
}

#[tokio::test]
async fn a_vanished_worktree_is_repaired_after_the_lease_steps() {
    // The startup order: the two lease steps, then the worktree repair. Run
    // in that order, the interrupted run is closed and pinned by the lease
    // step, and the repair finds no lease to strand.
    let f = Fixture::new().await;
    let task = f.task("Lost its worktree").await;
    let board = f.board(Which::A);
    let claim = claim_and_record(board.as_ref(), f.machine(), &task).await;
    let prepared = f
        .harness
        .prepare_worktree(&task)
        .await
        .expect("prepare the worktree");
    start_and_note(
        board.as_ref(),
        f.machine(),
        &claim,
        starting(&new_id(), RunKind::Implementation),
    )
    .await;
    std::fs::remove_dir_all(&prepared.path).expect("the worktree vanishes");
    let report = rimaia_core::startup::survey(f.ctx(), f.machine(), &f.paths)
        .await
        .expect("survey");
    assert_eq!(report.missing_worktrees, vec![task.clone()]);

    scheduler::reconcile_held(board.as_ref(), f.machine())
        .await
        .expect("reconcile held");
    f.reconcile_unrecorded().await;
    let repaired = worktree::reconcile(f.ctx(), f.machine(), &report.missing_worktrees).await;

    assert_eq!(repaired.len(), 1);
    assert_eq!(
        repaired[0].retained_branch.as_deref(),
        Some(prepared.branch.as_str()),
        "the branch outlived its directory, so it is retained",
    );
    let last = f.detail(&task).await.last_run.expect("the run");
    assert_eq!(last.status, RunStatus::Interrupted);
    assert_eq!(
        f.lease_state(&task).await,
        LeaseState {
            lease: None,
            generation: 1,
            pinned_runner_id: Some(f.runner_id(Which::A)),
        }
    );
    assert_eq!(f.harness.worktree_path(&task).await, None, "forgotten");
    // The interrupted run was offered for resume, and its worktree is gone:
    // the repair lands it `failed`, as it did at 042's tip.
    assert_eq!(f.run_state(&task).await, RunState::Failed);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Which {
    A,
    B,
}

fn run_now(task_id: &str) -> ClaimTarget {
    ClaimTarget::Run {
        task_id: task_id.to_string(),
        trigger: RunTrigger::Queued,
        continue_session: false,
    }
}

fn starting(run_id: &str, kind: RunKind) -> StartRun {
    StartRun {
        run_id: run_id.to_string(),
        kind,
        session_id: "0b6d3e2e-0000-4000-8000-00000000043a".to_string(),
        prompt: "do the work".to_string(),
        base_ref: Some("main".to_string()),
        base_sha: None,
    }
}

fn succeeded() -> RunOutcome {
    RunOutcome {
        exit_class: ExitClass::Success,
        status: RunStatus::Succeeded,
        error_message: None,
        num_turns: Some(4),
        cost_usd: Some(0.25),
        duration_ms: Some(1_000),
        pr_url: None,
        usage_limit_resets_at: None,
        resume_after: None,
        spawned_as: SpawnedAs::default(),
        usage: TokenUsage::default(),
    }
}

fn transient() -> RunOutcome {
    RunOutcome {
        exit_class: ExitClass::Transient,
        status: RunStatus::Failed,
        error_message: Some("the API was overloaded".to_string()),
        ..succeeded()
    }
}

fn interrupted() -> RunOutcome {
    RunOutcome {
        exit_class: ExitClass::Interrupted,
        status: RunStatus::Interrupted,
        error_message: Some("the process died".to_string()),
        num_turns: None,
        cost_usd: None,
        duration_ms: None,
        ..succeeded()
    }
}

fn finishing(outcome: RunOutcome) -> FinishRun {
    FinishRun {
        outcome,
        head_sha: None,
        bundle: None,
        window_closes_at: None,
        transcript: TranscriptEnd::Complete { length: 0 },
    }
}

fn finishing_at(outcome: RunOutcome, head: &str) -> FinishRun {
    FinishRun {
        head_sha: Some(head.to_string()),
        ..finishing(outcome)
    }
}

fn blocking_finding() -> NewReviewFinding {
    NewReviewFinding {
        severity: FindingSeverity::Critical,
        title: "The retry never stops".to_string(),
        body: "The loop has no budget.".to_string(),
        file: Some("src/retry.rs".to_string()),
        line: Some(12),
    }
}

/// The record a starter leaves for `task_id`, stamped at the harness's now.
fn held(
    f: &Fixture,
    task_id: &str,
    purpose: LeasePurpose,
    run_id: Option<&str>,
    generation: i64,
) -> HeldLease {
    HeldLease {
        task_id: task_id.to_string(),
        team_id: f.harness.solo.team_id.clone(),
        purpose,
        run_id: run_id.map(str::to_string),
        generation,
        acquired_at: f.harness.clock.now(),
    }
}

/// Walks a task through ADR-0007's machine with the one writer of
/// `run_state`.
async fn walk(f: &Fixture, task_id: &str, route: &[RunState]) {
    for state in route {
        tasks::set_run_state(f.ctx(), task_id, *state)
            .await
            .unwrap_or_else(|error| panic!("walk {task_id} to {state:?}: {error}"));
    }
}

fn drain(harness: &mut TestContext) {
    while harness.changes.try_recv().is_ok() {}
}

/// A second machine's store over the harness's board: runner B's.
fn memory_machine(harness: &TestContext) -> MachineContext {
    MachineContext {
        store: Arc::new(MemoryMachine::new()),
        clock: Arc::new(harness.clock.clone()),
        changes: harness.context.changes.clone(),
        event_team: harness.solo.team_id.clone(),
    }
}

struct Fixture {
    harness: TestContext,
    paths: AppPaths,
    cli: FakeCli,
    second_runner: String,
    repository_id: String,
    /// Held for their `Drop`; the paths above point inside them.
    _data: TempDir,
    _repository: TempRepo,
}

impl Fixture {
    async fn new() -> Self {
        Self::over(TestContext::new().await, None).await
    }

    /// The same, over a board in a database file: the production pool's
    /// shape, more than one connection.
    async fn over_file() -> Self {
        let data = tempfile::Builder::new()
            .prefix("rimaia-leases-")
            .tempdir()
            .expect("a data directory");
        let harness = TestContext::over_file(&data.path().join("rimaia.db")).await;
        Self::over(harness, Some(data)).await
    }

    async fn over(harness: TestContext, data: Option<TempDir>) -> Self {
        let data = match data {
            Some(data) => data,
            None => tempfile::Builder::new()
                .prefix("rimaia-leases-")
                .tempdir()
                .expect("a data directory"),
        };
        let paths = AppPaths::new(data.path().join("data"));
        paths.create_all().expect("the app data directories");
        let repository = TempRepo::init();
        let registered = repo::register(
            &harness.context,
            harness.machine(),
            &paths.worktrees_dir(),
            NewRepository {
                path: repository.path().to_string_lossy().into_owned(),
                name: None,
                worktree_root: None,
            },
        )
        .await
        .expect("register a repository");
        repo::set_allow_unattended_runs(&harness.context, harness.machine(), &registered.id, true)
            .await
            .expect("ADR-0012's opt-in");
        let second_runner = {
            let mut conn = harness.context.pool.acquire().await.expect("a connection");
            insert_runner(&mut conn, &harness.clock, &harness.solo.user_id, "Runner B").await
        };

        Self {
            harness,
            paths,
            cli: FakeCli::new(),
            second_runner,
            repository_id: registered.id,
            _data: data,
            _repository: repository,
        }
    }

    fn ctx(&self) -> &ServiceContext {
        &self.harness.context
    }

    fn machine(&self) -> &MachineContext {
        self.harness.machine()
    }

    fn config(&self) -> RunnerConfig {
        RunnerConfig {
            program: self.cli.program(),
            ..RunnerConfig::default()
        }
    }

    fn runner_id(&self, which: Which) -> String {
        match which {
            Which::A => self.harness.solo.runner_id.clone(),
            Which::B => self.second_runner.clone(),
        }
    }

    /// Runner `which`'s port onto this board, whose leases never expire.
    fn board(&self, which: Which) -> Arc<dyn BoardPort> {
        Arc::new(InProcessBoard::new(
            self.harness.context.clone(),
            self.paths.clone(),
            self.config().provider,
            self.runner_id(which),
            LeaseTerm::Never,
        ))
    }

    /// Runner `which`'s view of the board, over the one repository.
    fn view(&self, which: Which) -> RunnerView {
        RunnerView::new(
            self.runner_id(which),
            ProviderId::ClaudeCode,
            [self.repository_id.clone()],
        )
    }

    /// A `Next` claim with a free slot in the one repository.
    fn next(&self) -> ClaimTarget {
        ClaimTarget::Next {
            capacity: rimaia_core::board::FreeCapacity {
                total: 1,
                per_repository: [(self.repository_id.clone(), 1)].into(),
            },
            repositories: vec![self.repository_id.clone()],
            wait: Duration::ZERO,
        }
    }

    async fn task(&self, title: &str) -> String {
        tasks::create_task(
            self.ctx(),
            NewTask {
                repository_id: self.repository_id.clone(),
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

    /// ADR-0016's planned mode, with no proposal yet.
    async fn plan_mode(&self, task_id: &str) {
        tasks::update_task(
            self.ctx(),
            task_id,
            TaskPatch {
                strategy_mode: Some(StrategyMode::Planned),
                ..TaskPatch::default()
            },
        )
        .await
        .expect("planned mode");
    }

    async fn turn_the_loop_on(&self) {
        review_config::set_review_settings(
            self.ctx(),
            &ClaudeProvider,
            "",
            serde_json::json!({ "enabled": "on_cost_acknowledged" }),
        )
        .await
        .expect("turn the loop on");
    }

    async fn claim(&self, which: Which, task_id: &str, continue_session: bool) -> Claim {
        claim_run(
            self.board(which).as_ref(),
            task_id,
            RunTrigger::Queued,
            continue_session,
        )
        .await
        .expect("the claim is granted")
    }

    /// `run_task` for `claim`, through runner `which`'s port.
    async fn run(&self, which: Which, claim: Claim) -> rimaia_core::Result<Run> {
        tokio::time::timeout(
            TEST_TIMEOUT,
            run_task(
                self.board(which).as_ref(),
                self.machine(),
                &self.paths,
                &self.config(),
                claim,
                RunRequest::default(),
            ),
        )
        .await
        .expect("a run must finish inside the test timeout")
    }

    /// Runner A claims `task_id`, runs into a transient wall, and is pinned
    /// to it; answers when the retry is due.
    async fn wait_on_a_transient_wall(&self, task_id: &str) -> chrono::DateTime<chrono::Utc> {
        let board = self.board(Which::A);
        let claim = self.claim(Which::A, task_id, false).await;
        let run_id = new_id();
        board
            .start_run(&claim.lease, starting(&run_id, RunKind::Implementation))
            .await
            .expect("start");
        let receipt = board
            .finish_run(&claim.lease, &run_id, finishing(transient()))
            .await
            .expect("a transient wall");
        assert_eq!(self.run_state(task_id).await, RunState::WaitingRetry);
        assert_eq!(
            self.lease_state(task_id).await.pinned_runner_id,
            Some(self.runner_id(Which::A)),
        );
        match receipt.next {
            NextStep::Released {
                resume_after: Some(due),
            } => due,
            other => panic!("a transient failure is retried: {other:?}"),
        }
    }

    /// The state a force-quit leaves mid-run, through `board`, recorded on
    /// `machine`. Answers the run's id.
    async fn crash_mid_run(
        &self,
        board: &dyn BoardPort,
        machine: &MachineContext,
        task_id: &str,
    ) -> String {
        let claim = claim_and_record(board, machine, task_id).await;
        let run_id = new_id();
        start_and_note(
            board,
            machine,
            &claim,
            starting(&run_id, RunKind::Implementation),
        )
        .await;
        run_id
    }

    async fn reconcile_unrecorded(&self) -> Vec<String> {
        let held: Vec<String> = self
            .held()
            .await
            .into_iter()
            .map(|lease| lease.task_id)
            .collect();
        scheduler::reconcile_unrecorded(
            self.ctx(),
            &self.harness.solo.runner_id,
            &ClaudeProvider,
            &held,
        )
            .await
            .expect("reconcile")
    }

    async fn assert_lease_on(
        &self,
        task_id: &str,
        purpose: LeasePurpose,
        run_id: &str,
        generation: i64,
    ) {
        let held = self.lease_state(task_id).await.lease.expect("a lease");
        assert_eq!(
            (held.purpose, held.run_id.as_deref(), held.generation),
            (purpose, Some(run_id), generation)
        );
    }

    async fn detail(&self, task_id: &str) -> tasks::TaskDetail {
        tasks::get_task(self.ctx(), task_id)
            .await
            .expect("read the task")
    }

    async fn run_state(&self, task_id: &str) -> RunState {
        self.detail(task_id).await.task.run_state
    }

    async fn lease_state(&self, task_id: &str) -> LeaseState {
        lease::state_of(self.ctx(), task_id)
            .await
            .expect("read the lease")
    }

    async fn lease_rows(&self) -> i64 {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM runner_leases")
            .fetch_one(&self.ctx().pool)
            .await
            .expect("count the leases")
    }

    async fn runs(&self, task_id: &str) -> Vec<Run> {
        let mut runs = rimaia_core::runs::list_runs_for_task(self.ctx(), task_id)
            .await
            .expect("read the runs");
        runs.sort_by_key(|run| run.attempt);
        runs
    }

    async fn run_count(&self, task_id: &str) -> usize {
        self.runs(task_id).await.len()
    }

    async fn held(&self) -> Vec<HeldLease> {
        leases::held(self.machine())
            .await
            .expect("the runner's record")
    }
}

/// A board port that notes what the runner's store holds at the first report
/// a starter makes after its claim (which is before anything is spawned) and
/// when a run finishes (which is after `start_run`). Every call goes through.
struct Witness {
    inner: Arc<dyn BoardPort>,
    machine: MachineContext,
    state: Arc<Mutex<WitnessState>>,
}

#[derive(Default)]
struct WitnessState {
    before_spawn: Option<Vec<HeldLease>>,
    at_finish: Option<Vec<HeldLease>>,
}

impl Witness {
    fn new(inner: Arc<dyn BoardPort>, machine: MachineContext) -> Self {
        Self {
            inner,
            machine,
            state: Arc::new(Mutex::new(WitnessState::default())),
        }
    }

    /// The same witness, shareable as a port.
    fn shared(&self) -> Arc<dyn BoardPort> {
        Arc::new(Self {
            inner: Arc::clone(&self.inner),
            machine: self.machine.clone(),
            state: Arc::clone(&self.state),
        })
    }

    /// What was seen, and a fresh start for the next starter.
    fn take(&self) -> (Vec<HeldLease>, Vec<HeldLease>) {
        let mut state = self.state.lock().expect("the witness");
        let seen = std::mem::take(&mut *state);
        (
            seen.before_spawn.expect("a report before the spawn"),
            seen.at_finish.unwrap_or_default(),
        )
    }

    async fn note_before_spawn(&self) {
        let held = leases::held(&self.machine).await.expect("the record");
        let mut state = self.state.lock().expect("the witness");
        if state.before_spawn.is_none() {
            state.before_spawn = Some(held);
        }
    }
}

impl BoardPort for Witness {
    fn preview<'a>(&'a self, task_id: &'a str) -> BoardFuture<'a, RunContext> {
        self.inner.preview(task_id)
    }

    fn claim<'a>(&'a self, target: ClaimTarget) -> BoardFuture<'a, Option<Claim>> {
        self.inner.claim(target)
    }

    fn heartbeat<'a>(&'a self, held: &'a [LeaseRef]) -> BoardFuture<'a, Heartbeat> {
        self.inner.heartbeat(held)
    }

    fn run_context<'a>(&'a self, lease: &'a LeaseRef) -> BoardFuture<'a, RunContext> {
        Box::pin(async move {
            self.note_before_spawn().await;
            self.inner.run_context(lease).await
        })
    }

    fn record_branch<'a>(&'a self, lease: &'a LeaseRef, branch: &'a str) -> BoardFuture<'a, ()> {
        Box::pin(async move {
            self.note_before_spawn().await;
            self.inner.record_branch(lease, branch).await
        })
    }

    fn start_run<'a>(&'a self, lease: &'a LeaseRef, run: StartRun) -> BoardFuture<'a, ()> {
        self.inner.start_run(lease, run)
    }

    fn append_transcript<'a>(
        &'a self,
        lease: &'a LeaseRef,
        chunk: TranscriptChunk,
    ) -> BoardFuture<'a, TranscriptAck> {
        self.inner.append_transcript(lease, chunk)
    }

    fn publish_tail(&self, lease: &LeaseRef, tail: RunTail) {
        self.inner.publish_tail(lease, tail);
    }

    fn finish_run<'a>(
        &'a self,
        lease: &'a LeaseRef,
        run_id: &'a str,
        finish: FinishRun,
    ) -> BoardFuture<'a, FinishReceipt> {
        Box::pin(async move {
            let held = leases::held(&self.machine).await.expect("the record");
            self.state.lock().expect("the witness").at_finish = Some(held);
            self.inner.finish_run(lease, run_id, finish).await
        })
    }

    fn release<'a>(&'a self, lease: &'a LeaseRef) -> BoardFuture<'a, ()> {
        self.inner.release(lease)
    }

    fn record_strategy<'a>(
        &'a self,
        lease: &'a LeaseRef,
        plan: StrategyPlan,
    ) -> BoardFuture<'a, ()> {
        self.inner.record_strategy(lease, plan)
    }

    fn record_review_findings<'a>(
        &'a self,
        lease: &'a LeaseRef,
        run_id: &'a str,
        findings: Vec<NewReviewFinding>,
    ) -> BoardFuture<'a, ()> {
        self.inner.record_review_findings(lease, run_id, findings)
    }
}

/// What [`RefusesToRecord`] answers every `record_held_lease` with.
const REFUSED_RECORD: &str = "the runner's store refused the record";

/// A runner store that cannot note a lease: `record_held_lease` fails, and
/// every other call goes through to the store it wraps, so a starter's
/// preflight reads the checkout and settings it always does.
struct RefusesToRecord(Arc<dyn MachineStore>);

impl MachineStore for RefusesToRecord {
    fn get_setting<'a>(&'a self, key: &'a str) -> MachineFuture<'a, Option<String>> {
        self.0.get_setting(key)
    }

    fn set_setting<'a>(&'a self, key: &'a str, value: &'a str) -> MachineFuture<'a, ()> {
        self.0.set_setting(key, value)
    }

    fn clear_setting<'a>(&'a self, key: &'a str) -> MachineFuture<'a, ()> {
        self.0.clear_setting(key)
    }

    fn list_checkouts(&self) -> MachineFuture<'_, Vec<Checkout>> {
        self.0.list_checkouts()
    }

    fn get_checkout<'a>(&'a self, repository_id: &'a str) -> MachineFuture<'a, Option<Checkout>> {
        self.0.get_checkout(repository_id)
    }

    fn insert_checkout<'a>(&'a self, checkout: &'a Checkout) -> MachineFuture<'a, ()> {
        self.0.insert_checkout(checkout)
    }

    fn patch_checkout<'a>(
        &'a self,
        repository_id: &'a str,
        patch: &'a CheckoutPatch,
    ) -> MachineFuture<'a, bool> {
        self.0.patch_checkout(repository_id, patch)
    }

    fn remove_checkout<'a>(&'a self, repository_id: &'a str) -> MachineFuture<'a, bool> {
        self.0.remove_checkout(repository_id)
    }

    fn get_worktree<'a>(&'a self, task_id: &'a str) -> MachineFuture<'a, Option<WorktreeRecord>> {
        self.0.get_worktree(task_id)
    }

    fn list_worktrees(&self) -> MachineFuture<'_, Vec<WorktreeRecord>> {
        self.0.list_worktrees()
    }

    fn record_worktree<'a>(&'a self, record: &'a WorktreeRecord) -> MachineFuture<'a, ()> {
        self.0.record_worktree(record)
    }

    fn forget_worktree<'a>(&'a self, task_id: &'a str) -> MachineFuture<'a, bool> {
        self.0.forget_worktree(task_id)
    }

    fn record_held_lease<'a>(&'a self, _lease: &'a HeldLease) -> MachineFuture<'a, ()> {
        Box::pin(std::future::ready(Err(rimaia_core::Error::internal(
            REFUSED_RECORD,
        ))))
    }

    fn set_held_lease_run<'a>(
        &'a self,
        task_id: &'a str,
        run_id: Option<&'a str>,
        purpose: LeasePurpose,
    ) -> MachineFuture<'a, bool> {
        self.0.set_held_lease_run(task_id, run_id, purpose)
    }

    fn forget_held_lease<'a>(&'a self, task_id: &'a str) -> MachineFuture<'a, bool> {
        self.0.forget_held_lease(task_id)
    }

    fn list_held_leases(&self) -> MachineFuture<'_, Vec<HeldLease>> {
        self.0.list_held_leases()
    }

    fn list_schedules(&self) -> MachineFuture<'_, Vec<Schedule>> {
        self.0.list_schedules()
    }

    fn get_schedule<'a>(&'a self, id: &'a str) -> MachineFuture<'a, Option<Schedule>> {
        self.0.get_schedule(id)
    }

    fn insert_schedule<'a>(&'a self, schedule: &'a Schedule) -> MachineFuture<'a, ()> {
        self.0.insert_schedule(schedule)
    }

    fn update_schedule<'a>(&'a self, schedule: &'a Schedule) -> MachineFuture<'a, bool> {
        self.0.update_schedule(schedule)
    }

    fn set_schedule_enabled<'a>(
        &'a self,
        id: &'a str,
        enabled: bool,
        armed_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> MachineFuture<'a, bool> {
        self.0.set_schedule_enabled(id, enabled, armed_at)
    }

    fn record_schedule_fire<'a>(
        &'a self,
        id: &'a str,
        fired_at: chrono::DateTime<chrono::Utc>,
    ) -> MachineFuture<'a, bool> {
        self.0.record_schedule_fire(id, fired_at)
    }

    fn delete_schedule<'a>(&'a self, id: &'a str) -> MachineFuture<'a, bool> {
        self.0.delete_schedule(id)
    }
}
