//! The board port: everything a runner says to the board (ADR-0027 point 5,
//! seam-contract D31).

use std::future::Future;
use std::pin::Pin;

use crate::error::Result;
use crate::review::findings::NewReviewFinding;
use crate::runner::events::RunTail;
use crate::tasks::strategy::StrategyPlan;

use super::types::{
    Claim, ClaimTarget, FinishReceipt, FinishRun, Heartbeat, LeaseRef, PreviewOf, RunContext,
    StartRun, TranscriptAck, TranscriptChunk,
};

/// What every fallible port method returns.
///
/// A boxed future rather than an `async fn`, for the reason
/// [`Clock::sleep_until`](crate::Clock::sleep_until) gives: the trait stays
/// object-safe without an `async-trait` dependency, which D6 forbids and D34
/// does not approve.
pub type BoardFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// The runner's only way to reach the board. Held as `Arc<dyn BoardPort>`.
///
/// There is no general `set_run_state` (D31 point 4): the claim takes both
/// edges, `finish_run` lands the task and `release` abandons it.
pub trait BoardPort: Send + Sync + 'static {
    // Scoped by the runner the adapter was built for, never by a request field.

    /// The context a claim would return, refused as the claim `of` names
    /// would refuse it. Writes nothing, and is advisory: a run is composed
    /// from its claim, never from a preview.
    fn preview<'a>(&'a self, task_id: &'a str, of: PreviewOf) -> BoardFuture<'a, RunContext>;

    /// The single path for every process a runner starts. `None` is a claim
    /// lost to another starter, and never an error.
    fn claim<'a>(&'a self, target: ClaimTarget) -> BoardFuture<'a, Option<Claim>>;

    fn heartbeat<'a>(&'a self, held: &'a [LeaseRef]) -> BoardFuture<'a, Heartbeat>;

    // Scoped by `lease`.

    /// The context re-read under a lease.
    fn run_context<'a>(&'a self, lease: &'a LeaseRef) -> BoardFuture<'a, RunContext>;

    /// Writes `tasks.branch`. The worktree path never crosses the port.
    fn record_branch<'a>(&'a self, lease: &'a LeaseRef, branch: &'a str) -> BoardFuture<'a, ()>;

    fn start_run<'a>(&'a self, lease: &'a LeaseRef, run: StartRun) -> BoardFuture<'a, ()>;

    fn append_transcript<'a>(
        &'a self,
        lease: &'a LeaseRef,
        chunk: TranscriptChunk,
    ) -> BoardFuture<'a, TranscriptAck>;

    /// Synchronous and infallible: a dropped tail costs nothing (D14).
    fn publish_tail(&self, lease: &LeaseRef, tail: RunTail);

    fn finish_run<'a>(
        &'a self,
        lease: &'a LeaseRef,
        run_id: &'a str,
        finish: FinishRun,
    ) -> BoardFuture<'a, FinishReceipt>;

    /// Ends a claim no `finish_run` ended. Callers treat it as best effort.
    fn release<'a>(&'a self, lease: &'a LeaseRef) -> BoardFuture<'a, ()>;

    /// Always sourced as the planner: a runner never writes a user's strategy.
    fn record_strategy<'a>(
        &'a self,
        lease: &'a LeaseRef,
        plan: StrategyPlan,
    ) -> BoardFuture<'a, ()>;

    fn record_review_findings<'a>(
        &'a self,
        lease: &'a LeaseRef,
        run_id: &'a str,
        findings: Vec<NewReviewFinding>,
    ) -> BoardFuture<'a, ()>;
}
