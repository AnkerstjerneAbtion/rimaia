//! Board-port helpers for tests: a port that refuses everything, and the
//! claim a fixture takes before it hands a task to `run_task`.

use crate::board::{
    BoardFuture, BoardPort, Claim, ClaimTarget, FinishReceipt, FinishRun, Heartbeat, LeaseRef,
    RunContext, StartRun, TranscriptAck, TranscriptChunk,
};
use crate::error::{Error, Result};
use crate::review::findings::NewReviewFinding;
use crate::runner::events::RunTail;
use crate::runner::RunTrigger;
use crate::tasks::strategy::StrategyPlan;

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
        })
        .await?;
    Ok(claim.unwrap_or_else(|| panic!("the fixture's task {task_id} was already claimed")))
}
