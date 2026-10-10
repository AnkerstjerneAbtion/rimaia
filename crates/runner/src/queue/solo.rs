//! The four board reads the solo loop still makes without the port, and the
//! one board context it still hands a run (task 042, Scope point 5).
//!
//! Everything the loop *writes* goes through the [`BoardPort`] or through the
//! machine's own store. What is left is four reads that have no port method
//! yet, and the context `run_task` keeps for `worktree::prepare` until task
//! 044 removes it. They live here, and only here: this is the one file under
//! `queue/` that holds a board `ServiceContext`, so it is the one place tasks
//! 058's headless runner and 059's connected mode replace, and a reviewer can
//! check that with one `grep`.
//!
//! [`BoardPort`]: rimaia_core::board::BoardPort

use chrono::{DateTime, Utc};
use rimaia_core::db::MutationSource;
use rimaia_core::doctor::{self, DoctorReport};
use rimaia_core::machine::MachineContext;
use rimaia_core::schedule::preflight::{self, PreflightSummary};
use rimaia_core::scheduler::selection::{self, QueueEntry, SkipReason};
use rimaia_core::{Result, ServiceContext};

/// The solo board, read in process.
///
/// Cheap to clone, like the context it wraps. Built once, in the shell's
/// `setup()`, over the same context the board port was built on.
#[derive(Clone)]
pub struct SoloBoard {
    ctx: ServiceContext,
}

impl SoloBoard {
    /// Re-sources `ctx` to [`MutationSource::System`], as the loop's own
    /// `build` did before task 042: the only write it can reach through here
    /// is `worktree::prepare`'s, made for a run the queue started, and the
    /// shell hands one `Ui` context to every subsystem (ADR-0019).
    pub fn new(ctx: ServiceContext) -> Self {
        Self {
            ctx: ctx.with_source(MutationSource::System),
        }
    }

    /// **Read 1: the deadline arm's source** (Scope point 4).
    ///
    /// The earliest instant after `after` at which a `waiting_retry` task among
    /// `repositories` becomes due, or `None`. Nothing publishes an event when
    /// that instant passes (seam-contract D23 point 1), and a `Next` claim that
    /// found nothing carries no deadline, so the loop asks for it here.
    ///
    /// `selection::next_deadline` over the plan, judged at `after` rather than
    /// at this read's own instant: the loop passes the moment it last asked the
    /// board, so a retry that came due between that claim and this read is
    /// still reported, and wakes the loop at once, rather than being read as
    /// no longer waiting and slept through. `QueueEntry::resume_after` is set
    /// only for a task in `waiting_retry`, so this is the same filter.
    pub async fn next_deadline(
        &self,
        repositories: &[String],
        after: DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>> {
        let plan = self.plan(repositories).await?;
        let still_waiting: Vec<QueueEntry> = plan
            .into_iter()
            .filter(|entry| entry.resume_after.is_some_and(|at| at > after))
            .map(|entry| QueueEntry {
                skip: Some(SkipReason::WaitingForRetry),
                ..entry
            })
            .collect();
        Ok(selection::next_deadline(&still_waiting))
    }

    /// **Read 2: the plan half of `status_with_plan`**: every `ready` task in
    /// board order with the reason a claim would pass over it, planned over
    /// the same repositories the loop sends with `Next`.
    pub async fn plan(&self, repositories: &[String]) -> Result<Vec<QueueEntry>> {
        selection::plan(&self.ctx, &repositories.iter().cloned().collect()).await
    }

    /// **Read 3: the doctor**, which lists the board's repositories by name.
    /// `QueueHandle::start`, `resume` and a schedule's fire run it, and
    /// seam-contract D22 point 1's gate stays where it is.
    pub async fn doctor(
        &self,
        machine: &MachineContext,
        environment: &doctor::Environment,
    ) -> Result<DoctorReport> {
        doctor::run(machine, &self.ctx, environment).await
    }

    /// **Read 4: the fire-time preflight log**: what a schedule that just
    /// fired will do, over the repositories the loop lists.
    pub async fn preflight(
        &self,
        machine: &MachineContext,
        schedule_id: &str,
        repositories: &[String],
    ) -> Result<PreflightSummary> {
        preflight::preview(machine, &self.ctx, schedule_id, repositories).await
    }

    /// **Not a read: task 044's temporary context.** `run_task` still takes a
    /// board context for `worktree::prepare` alone, and the loop's supervisor
    /// passes it this one. Task 044 removes it once `RunContext::base` carries
    /// what that call needs.
    pub fn prepare_context(&self) -> &ServiceContext {
        &self.ctx
    }
}
