//! Solo's board adapter: the port, answered in this process (seam-contract D31
//! point 9).
//!
//! Every method is one call into [`service`](super::service). Nothing here
//! decides anything, so task 052's HTTP adapter and this one cannot come to
//! disagree about a rule: they reach the same function.

use std::sync::Arc;

use crate::context::ServiceContext;
use crate::db::MutationSource;
use crate::events::RunnerId;
use crate::paths::AppPaths;
use crate::review::findings::NewReviewFinding;
use crate::runner::events::RunTail;
use crate::runner::provider::AgentProvider;
use crate::tasks::strategy::StrategyPlan;

use super::lease::LeaseTerm;
use super::port::{BoardFuture, BoardPort};
use super::service::{self, Runner};
use super::types::{
    Claim, ClaimTarget, FinishReceipt, FinishRun, Heartbeat, LeaseRef, RunContext, StartRun,
    TranscriptAck, TranscriptChunk,
};

/// The board, in process.
///
/// `provider` is read only to build [`RunContext::catalogue`], and it must be
/// the provider of the [`RunnerConfig`](crate::runner::RunnerConfig) this board
/// serves: a board built for another one would hand the planner the wrong
/// catalogue, and nothing would fail loudly. `src-tauri` builds both from one
/// value, and `TestContext::board` takes it from the config.
pub struct InProcessBoard {
    ctx: ServiceContext,
    paths: AppPaths,
    provider: Arc<dyn AgentProvider>,
    /// The runner this board serves: solo's own, from its `SoloIdentity`
    /// (D31 point 9). It is the adapter's scope, never a request field (D31
    /// point 3), which is why `StartRun` does not carry it.
    runner_id: RunnerId,
    /// Whether this board's leases expire: [`LeaseTerm::Never`] in solo,
    /// where the board and its one runner are one process (ADR-0031 point 5,
    /// D31 point 9). The contract harness passes a renewable term to prove
    /// the heartbeat.
    term: LeaseTerm,
}

impl InProcessBoard {
    /// Re-sources `ctx` to [`MutationSource::System`]: a report comes from the
    /// runner, whoever pressed the button, and the claim's trigger records
    /// which button it was (D31 point 9).
    pub fn new(
        ctx: ServiceContext,
        paths: AppPaths,
        provider: Arc<dyn AgentProvider>,
        runner_id: RunnerId,
        term: LeaseTerm,
    ) -> Self {
        Self {
            ctx: ctx.with_source(MutationSource::System),
            paths,
            provider,
            runner_id,
            term,
        }
    }

    fn runner(&self) -> Runner<'_> {
        Runner {
            id: &self.runner_id,
            provider: self.provider.as_ref(),
            term: self.term,
        }
    }
}

impl BoardPort for InProcessBoard {
    fn preview<'a>(&'a self, task_id: &'a str) -> BoardFuture<'a, RunContext> {
        Box::pin(service::preview(&self.ctx, self.provider.as_ref(), task_id))
    }

    fn claim<'a>(&'a self, target: ClaimTarget) -> BoardFuture<'a, Option<Claim>> {
        Box::pin(service::claim(&self.ctx, self.runner(), target))
    }

    fn heartbeat<'a>(&'a self, held: &'a [LeaseRef]) -> BoardFuture<'a, Heartbeat> {
        Box::pin(service::heartbeat(&self.ctx, self.runner(), held))
    }

    fn run_context<'a>(&'a self, lease: &'a LeaseRef) -> BoardFuture<'a, RunContext> {
        Box::pin(service::run_context(&self.ctx, self.runner(), lease))
    }

    fn record_branch<'a>(&'a self, lease: &'a LeaseRef, branch: &'a str) -> BoardFuture<'a, ()> {
        Box::pin(service::record_branch(
            &self.ctx,
            &self.runner_id,
            lease,
            branch,
        ))
    }

    fn start_run<'a>(&'a self, lease: &'a LeaseRef, run: StartRun) -> BoardFuture<'a, ()> {
        Box::pin(service::start_run(
            &self.ctx,
            &self.paths,
            &self.runner_id,
            lease,
            run,
        ))
    }

    fn append_transcript<'a>(
        &'a self,
        lease: &'a LeaseRef,
        chunk: TranscriptChunk,
    ) -> BoardFuture<'a, TranscriptAck> {
        Box::pin(service::append_transcript(
            &self.ctx,
            &self.runner_id,
            lease,
            chunk,
        ))
    }

    fn publish_tail(&self, lease: &LeaseRef, tail: RunTail) {
        service::publish_tail(&self.ctx, lease, tail);
    }

    fn finish_run<'a>(
        &'a self,
        lease: &'a LeaseRef,
        run_id: &'a str,
        finish: FinishRun,
    ) -> BoardFuture<'a, FinishReceipt> {
        Box::pin(service::finish_run(
            &self.ctx,
            &self.runner_id,
            lease,
            run_id,
            finish,
        ))
    }

    fn release<'a>(&'a self, lease: &'a LeaseRef) -> BoardFuture<'a, ()> {
        Box::pin(service::release(&self.ctx, &self.runner_id, lease))
    }

    fn record_strategy<'a>(
        &'a self,
        lease: &'a LeaseRef,
        plan: StrategyPlan,
    ) -> BoardFuture<'a, ()> {
        Box::pin(service::record_strategy(
            &self.ctx,
            &self.runner_id,
            lease,
            plan,
        ))
    }

    fn record_review_findings<'a>(
        &'a self,
        lease: &'a LeaseRef,
        run_id: &'a str,
        findings: Vec<NewReviewFinding>,
    ) -> BoardFuture<'a, ()> {
        Box::pin(service::record_review_findings(
            &self.ctx,
            &self.runner_id,
            lease,
            run_id,
            findings,
        ))
    }
}
