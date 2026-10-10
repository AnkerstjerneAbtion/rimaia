//! Board-port helpers for tests: a port that refuses everything, the claim a
//! fixture takes before it hands a task to `run_task`, the claim and run a
//! crash test leaves behind for the lease reconcile (task 043), and a whole
//! run made without a child, for a test whose subject is what a later run
//! builds on (task 044).

use std::path::Path;

use crate::board::{
    BoardFuture, BoardPort, Claim, ClaimTarget, FinishReceipt, FinishRun, Heartbeat, LeaseRef,
    RunContext, StartRun, TranscriptAck, TranscriptChunk, TranscriptEnd,
};
use crate::db::{new_id, RunKind};
use crate::error::{Error, Result};
use crate::machine::MachineContext;
use crate::review::findings::NewReviewFinding;
use crate::runner::events::RunTail;
use crate::runner::outcome::RunOutcome;
use crate::runner::RunTrigger;
use crate::tasks::strategy::StrategyPlan;
use crate::testing::repo::git;
use crate::testing::TestContext;
use crate::worktree::Worktree;

/// Every fallible method answers `Invalid` with the same sentence, and the
/// tail goes nowhere.
///
/// What [`planner_access`](super::doctor::planner_access) fills its board
/// with: an MCP test that constructs a server and never plans is the helper's
/// documented contract, and a test that breaks it is told so by name rather
/// than planning against a board that does not exist.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unwired;

const REFUSAL: &str = "this test does not plan";

fn refused<'a, T: Send + 'a>() -> BoardFuture<'a, T> {
    Box::pin(async { Err(Error::invalid(REFUSAL)) })
}

impl BoardPort for Unwired {
    fn preview<'a>(&'a self, _task_id: &'a str) -> BoardFuture<'a, RunContext> {
        refused()
    }

    fn claim<'a>(&'a self, _target: ClaimTarget) -> BoardFuture<'a, Option<Claim>> {
        refused()
    }

    fn heartbeat<'a>(&'a self, _held: &'a [LeaseRef]) -> BoardFuture<'a, Heartbeat> {
        refused()
    }

    fn run_context<'a>(&'a self, _lease: &'a LeaseRef) -> BoardFuture<'a, RunContext> {
        refused()
    }

    fn record_branch<'a>(&'a self, _lease: &'a LeaseRef, _branch: &'a str) -> BoardFuture<'a, ()> {
        refused()
    }

    fn start_run<'a>(&'a self, _lease: &'a LeaseRef, _run: StartRun) -> BoardFuture<'a, ()> {
        refused()
    }

    fn append_transcript<'a>(
        &'a self,
        _lease: &'a LeaseRef,
        _chunk: TranscriptChunk,
    ) -> BoardFuture<'a, TranscriptAck> {
        refused()
    }

    fn publish_tail(&self, _lease: &LeaseRef, _tail: RunTail) {}

    fn finish_run<'a>(
        &'a self,
        _lease: &'a LeaseRef,
        _run_id: &'a str,
        _finish: FinishRun,
    ) -> BoardFuture<'a, FinishReceipt> {
        refused()
    }

    fn release<'a>(&'a self, _lease: &'a LeaseRef) -> BoardFuture<'a, ()> {
        refused()
    }

    fn record_strategy<'a>(
        &'a self,
        _lease: &'a LeaseRef,
        _plan: StrategyPlan,
    ) -> BoardFuture<'a, ()> {
        refused()
    }

    fn record_review_findings<'a>(
        &'a self,
        _lease: &'a LeaseRef,
        _run_id: &'a str,
        _findings: Vec<NewReviewFinding>,
    ) -> BoardFuture<'a, ()> {
        refused()
    }
}

/// Claims `task_id` for a run, the way every starter does before it calls
/// `run_task` (seam-contract D31 point 5).
///
/// For fixtures, which own the task they claim: a lost race panics, because
/// in a fixture it can only be a broken arrangement, while a refusal the board
/// raises is returned for the test to assert on.
pub async fn claim_run(
    board: &dyn BoardPort,
    task_id: &str,
    trigger: RunTrigger,
    continue_session: bool,
) -> Result<Claim> {
    let claim = board
        .claim(ClaimTarget::Run {
            task_id: task_id.to_string(),
            trigger,
            continue_session,
            ceiling: Default::default(),
        })
        .await?;
    Ok(claim.unwrap_or_else(|| panic!("the fixture's task {task_id} was already claimed")))
}

/// Claims `task_id` as Run now does and records the lease in `machine`, the
/// way every starter does before it spawns anything (task 043): the state a
/// crash leaves when it lands before the first `start_run`.
pub async fn claim_and_record(
    board: &dyn BoardPort,
    machine: &MachineContext,
    task_id: &str,
) -> Claim {
    let claim = claim_run(board, task_id, RunTrigger::Queued, false)
        .await
        .expect("claim the task");
    crate::machine::leases::record(machine, &claim)
        .await
        .expect("record the claim on the runner");
    claim
}

/// Opens `run` under `claim` and notes it on the runner, as `run_task` does
/// (task 043): the state a crash leaves mid-run.
pub async fn start_and_note(
    board: &dyn BoardPort,
    machine: &MachineContext,
    claim: &Claim,
    run: StartRun,
) {
    let run_id = run.run_id.clone();
    let kind = run.kind;
    board
        .start_run(&claim.lease, run)
        .await
        .expect("open the run");
    crate::machine::leases::note_run(machine, &claim.lease.task_id, Some(&run_id), kind.into())
        .await;
}

/// One run [`run_without_a_child`] made: the worktree it ran in, its row, and
/// the commit it ended on as `git rev-parse HEAD` read it.
#[derive(Debug, Clone)]
pub struct FinishedRun {
    pub worktree: Worktree,
    pub run_id: String,
    pub head_sha: String,
}

/// A run of `task_id` as the runner makes one, with no child, through the
/// board services production uses: the claim, the worktree prepared from the
/// claim's context, each of `files` committed in it, the row opened with the
/// worktree's base, and the finish at `HEAD` with `outcome`.
///
/// For a test whose subject is what a later run builds on (task 044), which
/// needs a run that really ended on a commit. A test about the run itself
/// goes through `run_task` and `FakeCli` instead.
pub async fn run_without_a_child(
    harness: &TestContext,
    board: &dyn BoardPort,
    task_id: &str,
    kind: RunKind,
    continue_session: bool,
    files: &[&str],
    outcome: RunOutcome,
) -> FinishedRun {
    let claim = claim_run(board, task_id, RunTrigger::Manual, continue_session)
        .await
        .expect("claim the task");
    let worktree = harness
        .prepare_worktree_from(&claim.context)
        .await
        .expect("prepare the worktree from the claim");
    let checkout = Path::new(&worktree.path);
    for file in files {
        std::fs::write(checkout.join(file), format!("// {file}\n")).expect("write a file");
        git(checkout, &["add", "--", file]);
        git(checkout, &["commit", "-m", &format!("Add {file}")]);
    }
    let head_sha = git(checkout, &["rev-parse", "HEAD"]);

    let run_id = new_id();
    board
        .start_run(
            &claim.lease,
            StartRun {
                run_id: run_id.clone(),
                kind,
                session_id: format!("session-{task_id}"),
                prompt: "do the work".to_string(),
                base_ref: Some(worktree.base.base_ref.clone()),
                base_sha: worktree.base_sha.clone(),
            },
        )
        .await
        .expect("open the run");
    board
        .finish_run(
            &claim.lease,
            &run_id,
            FinishRun {
                outcome,
                head_sha: Some(head_sha.clone()),
                bundle: None,
                window_closes_at: None,
                transcript: TranscriptEnd::Complete { length: 0 },
                ceiling: Default::default(),
            },
        )
        .await
        .expect("finish the run");

    FinishedRun {
        worktree,
        run_id,
        head_sha,
    }
}
